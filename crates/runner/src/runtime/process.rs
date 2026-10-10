//! Concrete scheduler adapters with owned cancellation and cleanup settlement.
use super::{
    FutureResult, RuntimeError,
    backend::Backend,
    worker::{JobResult, Provisioner},
};
use crate::{
    cancellation::CancellationEvent,
    worker_process::{self, ProcessError, ProcessFuture},
};

use std::{
    future::Future,
    io::{self, Write},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

fn process_error(error: RuntimeError) -> ProcessError {
    if error == RuntimeError::Cancelled {
        ProcessError::cancelled()
    } else {
        let text = String::from(&error.to_string());
        ProcessError::expected(text.clone(), text)
    }
}
type OwnedFuture<T> = Pin<Box<dyn Future<Output = Result<T, ProcessError>> + Send>>;
struct Operation<T: Send + 'static> {
    cancel: CancellationEvent,
    pending: Option<OwnedFuture<T>>,
    task: Option<tokio::task::JoinHandle<Result<T, ProcessError>>>,
}
impl<T: Send + 'static> worker_process::Operation<T> for Operation<T> {
    fn cancel(&self) {
        self.cancel.set();
    }
    fn settle(&mut self) -> ProcessFuture<'_, T> {
        Box::pin(async move {
            if let Some(pending) = self.pending.take() {
                self.task = Some(tokio::spawn(pending));
            }
            let task = self
                .task
                .as_mut()
                .ok_or_else(|| process_error(RuntimeError::Contract))?;
            let result = task
                .await
                .map_err(|_| process_error(RuntimeError::Launch))?;
            self.task.take();
            result
        })
    }
}
impl<T: Send + 'static> Drop for Operation<T> {
    fn drop(&mut self) {
        self.cancel.set();
    }
}
fn operation<T, F, Fut>(build: F) -> Box<dyn worker_process::Operation<T>>
where
    T: Send + 'static,
    F: FnOnce(CancellationEvent) -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, RuntimeError>> + Send + 'static,
{
    let cancel = CancellationEvent::new();
    let signal = cancel.clone();
    let pending = Box::pin(async move {
        if signal.is_set() {
            return Err(ProcessError::cancelled());
        }
        build(signal).await.map_err(process_error)
    });
    Box::new(Operation {
        cancel,
        pending: Some(pending),
        task: None,
    })
}
/// Concrete workers share the scheduler without supplying fake client/backend defaults.
pub trait RuntimeWorker: Send + Sync {
    fn run_once(&self, cancel: CancellationEvent) -> FutureResult<'_, Option<JobResult>>;
    fn close(&self) -> FutureResult<'_, ()>;
}
struct WorkerAdapter(Arc<dyn RuntimeWorker>);
impl worker_process::Worker for WorkerAdapter {
    fn run_once(
        &self,
    ) -> Result<Box<dyn worker_process::Operation<Option<worker_process::JobResult>>>, ProcessError>
    {
        let worker = self.0.clone();
        Ok(operation(move |cancel| async move {
            worker.run_once(cancel).await.map(|result| {
                result.map(|result| worker_process::JobResult {
                    job_id: String::from(&result.job_id),
                    state: String::from(&result.state),
                })
            })
        }))
    }
    fn aclose(&self) -> Box<dyn worker_process::Operation<()>> {
        let worker = self.0.clone();
        operation(move |_| async move { worker.close().await })
    }
}
struct Factory(Vec<Arc<dyn RuntimeWorker>>);
impl worker_process::WorkerFactory for Factory {
    fn create(
        &self,
        index: usize,
        _entry: &worker_process::Entry,
        _prefix: &str,
    ) -> Result<Arc<dyn worker_process::Worker>, ProcessError> {
        let worker = self
            .0
            .get(index)
            .ok_or_else(|| process_error(RuntimeError::Configuration))?;
        Ok(Arc::new(WorkerAdapter(worker.clone())))
    }
}
struct Lifecycle(Arc<dyn Backend>, Arc<dyn Provisioner>);
impl worker_process::SharedLifecycle for Lifecycle {
    fn launcher_start(&self) -> Box<dyn worker_process::Operation<()>> {
        let backend = self.0.clone();
        operation(move |_| async move { backend.start().await })
    }
    fn launcher_close(&self) -> Box<dyn worker_process::Operation<()>> {
        let backend = self.0.clone();
        operation(move |_| async move { backend.close().await })
    }
    fn client_open(&self) -> Box<dyn worker_process::Operation<()>> {
        operation(|_| async { Ok(()) })
    }
    fn client_close(&self) -> Box<dyn worker_process::Operation<()>> {
        operation(|_| async { Ok(()) })
    }
    fn code_close(&self) -> Box<dyn worker_process::Operation<()>> {
        let provisioner = self.1.clone();
        operation(move |_| async move { provisioner.close().await })
    }
}
struct Wait;
impl worker_process::Wait for Wait {
    fn sleep(&self, seconds: f64) -> Box<dyn worker_process::Operation<()>> {
        operation(move |cancel| async move {
            if !seconds.is_finite() {
                return Err(RuntimeError::Configuration);
            }
            let duration = Duration::try_from_secs_f64(seconds.max(0.0))
                .map_err(|_| RuntimeError::Configuration)?;
            tokio::select! {()=cancel.wait()=>Err(RuntimeError::Cancelled),()=tokio::time::sleep(duration)=>Ok(())}
        })
    }
}
struct Output;
impl worker_process::LogSink for Output {
    fn line(&self, stream: worker_process::Stream, text: &str) -> Result<(), ProcessError> {
        let mut output: Box<dyn Write> = match stream {
            worker_process::Stream::Stdout => Box::new(io::stdout()),
            worker_process::Stream::Stderr => Box::new(io::stderr()),
        };
        writeln!(output, "{text}")
            .and_then(|()| output.flush())
            .map_err(|_| process_error(RuntimeError::Filesystem))
    }
}
/// Start the concrete workers under the proven shared scheduler.
/// # Errors
/// Rejects entry/factory mismatches; startup and cleanup failures stay explicit.
pub async fn run(
    entries: Vec<worker_process::Entry>,
    workers: Vec<Arc<dyn RuntimeWorker>>,
    backend: Arc<dyn Backend>,
    provisioner: Arc<dyn Provisioner>,
    once: bool,
) -> Result<i32, RuntimeError> {
    if entries.len() != workers.len() || entries.is_empty() {
        return Err(RuntimeError::Configuration);
    }
    let mut terminated = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| RuntimeError::Configuration)?;
    let mut interrupted = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .map_err(|_| RuntimeError::Configuration)?;
    let task = worker_process::ProcessTask::start(worker_process::Request {
        entries,
        prog: String::from("cannery runner"),
        once,
        has_launcher: true,
        external_client: false,
        factory: Arc::new(Factory(workers)),
        lifecycle: Arc::new(Lifecycle(backend, provisioner)),
        wait: Arc::new(Wait),
        output: Arc::new(Output),
    });
    let cancel = task.cancellation();
    let settle = task.settle();
    tokio::pin!(settle);
    tokio::select! {
        result=&mut settle=>result.map_err(|_|RuntimeError::Launch),
        _=terminated.recv()=>{cancel.cancel();let _=settle.await;Ok(143)},
        _=interrupted.recv()=>{cancel.cancel();let _=settle.await;Ok(130)},
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
