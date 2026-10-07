//! Worker scheduling with required owned operation and lifecycle boundaries.

use num_bigint::BigInt;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::Notify;

pub type ProcessFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ProcessError>> + Send + 'a>>;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    /// `RunnerError`, `LaunchError`, or `HTTPError`: `_once` prints raw `str(exc)`.
    Expected,
    /// Any other ordinary Exception.
    Unexpected,
    /// A non-Exception `BaseException`; `TaskGroup` wraps these in `BaseGroup`.
    Fatal,
    /// `KeyboardInterrupt`/`SystemExit`: `TaskGroup` rethrows the first directly.
    Interrupt,
    Cancelled,
    Group,
    BaseGroup,
    Task,
}
#[derive(Clone)]
pub struct ProcessError {
    pub kind: ErrorKind,
    /// Source `str(exc)`, used by expected failures in once mode only.
    pub display: String,
    /// Source `describe(exc)`, supplied losslessly by the required adapter.
    pub description: String,
    pub causes: Vec<Self>,
}
impl std::fmt::Debug for ProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessError")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}
impl ProcessError {
    #[must_use]
    pub fn new(kind: ErrorKind, description: String) -> Self {
        Self {
            kind,
            display: description.clone(),
            description,
            causes: Vec::new(),
        }
    }
    /// Preserve both source renderings; whitespace is significant in `display`.
    #[must_use]
    pub fn expected(display: String, description: String) -> Self {
        Self {
            kind: ErrorKind::Expected,
            display,
            description,
            causes: Vec::new(),
        }
    }
    #[must_use]
    pub fn cancelled() -> Self {
        Self::new(ErrorKind::Cancelled, String::from("CancelledError"))
    }
    fn task() -> Self {
        Self::new(ErrorKind::Task, String::from("worker task failed"))
    }
}
#[derive(Clone)]
pub struct JobResult {
    pub job_id: String,
    pub state: String,
}
impl std::fmt::Debug for JobResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JobResult([redacted])")
    }
}
/// `settle` borrows an owned operation: dropping its waiter must not drop the work.
/// `cancel` requests actual callback cancellation, including its finally cleanup.
/// Implementations must settle that cleanup; this selects no filesystem-thread policy.
pub trait Operation<T>: Send {
    fn cancel(&self);
    fn settle(&mut self) -> ProcessFuture<'_, T>;
}
pub trait Worker: Send + Sync {
    /// Construct one operation; concurrent loops call this same worker instance.
    /// Construction must be lazy, as calling a Python async method constructs
    /// its coroutine without running its body.
    /// # Errors
    /// Returns an operation-construction failure before starting that operation.
    fn run_once(&self) -> Result<Box<dyn Operation<Option<JobResult>>>, ProcessError>;
    fn aclose(&self) -> Box<dyn Operation<()>>;
}
#[derive(Clone)]
pub struct Entry {
    pub name: String,
    pub poll_seconds: f64,
    pub concurrency: BigInt,
}
impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Entry([redacted])")
    }
}
pub trait WorkerFactory: Send + Sync {
    /// Called after launcher/client startup, in entry order. The factory binds
    /// entries to their configuration/credentials; no token reaches this scheduler.
    /// # Errors
    /// Returns the source-positioned factory failure; earlier workers are not closed.
    fn create(
        &self,
        index: usize,
        entry: &Entry,
        prefix: &str,
    ) -> Result<Arc<dyn Worker>, ProcessError>;
}
pub trait SharedLifecycle: Send + Sync {
    fn launcher_start(&self) -> Box<dyn Operation<()>>;
    fn launcher_close(&self) -> Box<dyn Operation<()>>;
    fn client_open(&self) -> Box<dyn Operation<()>>;
    fn client_close(&self) -> Box<dyn Operation<()>>;
    fn code_close(&self) -> Box<dyn Operation<()>>;
}
pub trait Wait: Send + Sync {
    /// Preserve source f64 sleep semantics, including zero yielding. No implicit
    /// adapter, conversion bound, polling cap, or clock is installed here.
    fn sleep(&self, seconds: f64) -> Box<dyn Operation<()>>;
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stream {
    Stdout,
    Stderr,
}
pub trait LogSink: Send + Sync {
    /// Deliberate output only. Failure propagates at the source print position.
    /// Write text then newline; stderr requires a flush, stdout does not force
    /// one. Partial writes and encoding/flush failure remain adapter effects.
    /// # Errors
    /// Returns the source-compatible output failure, including encoding failures.
    fn line(&self, stream: Stream, text: &str) -> Result<(), ProcessError>;
}
pub struct Request {
    pub entries: Vec<Entry>,
    pub prog: String,
    pub once: bool,
    pub has_launcher: bool,
    pub external_client: bool,
    pub factory: Arc<dyn WorkerFactory>,
    pub lifecycle: Arc<dyn SharedLifecycle>,
    pub wait: Arc<dyn Wait>,
    pub output: Arc<dyn LogSink>,
}
struct StopState {
    count: AtomicUsize,
    changed: Notify,
}
#[derive(Clone)]
struct Stop(Arc<StopState>);
impl Stop {
    fn new() -> Self {
        Self(Arc::new(StopState {
            count: AtomicUsize::new(0),
            changed: Notify::new(),
        }))
    }
    fn request(&self) {
        self.0.count.fetch_add(1, Ordering::AcqRel);
        self.0.changed.notify_waiters();
    }
    fn epoch(&self) -> usize {
        self.0.count.load(Ordering::Acquire)
    }
    async fn changed(&self, epoch: usize) {
        let notified = self.0.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.epoch() == epoch {
            notified.await;
        }
    }
}
async fn run<T: Send>(
    mut op: Box<dyn Operation<T>>,
    stop: &Stop,
    delivered: &mut usize,
) -> Result<T, ProcessError> {
    loop {
        let current = stop.epoch();
        if current != *delivered {
            *delivered = current;
            op.cancel();
        }
        tokio::select! {
            result=op.settle()=>return result,
            ()=stop.changed(*delivered)=>{}
        }
    }
}
async fn finish(mut op: Box<dyn Operation<()>>, stop: &Stop) -> Result<(), ProcessError> {
    let mut epoch = stop.epoch();
    let mut interrupted = false;
    loop {
        tokio::select! {
            result=op.settle()=>return if interrupted || stop.epoch()!=epoch {Err(ProcessError::cancelled())} else {result},
            ()=stop.changed(epoch)=>{ interrupted=true; epoch=stop.epoch(); }
        }
    }
}
async fn client_finish(op: Box<dyn Operation<()>>, stop: &Stop) -> Result<(), ProcessError> {
    let mut delivered = stop.epoch();
    run(op, stop, &mut delivered).await
}
fn joined(parts: &[&str]) -> Result<String, ProcessError> {
    cannery_core::text::from_codepoints(
        parts
            .iter()
            .flat_map(|p| p.chars().map(u32::from))
            .collect(),
    )
    .ok_or_else(ProcessError::task)
}
fn t(s: &str) -> String {
    String::from(s)
}
fn stderr(request: &Request, prefix: &str, message: &str) -> Result<(), ProcessError> {
    request
        .output
        .line(Stream::Stderr, &joined(&[prefix, &t(": "), message])?)
}
struct BoundWorker {
    worker: Arc<dyn Worker>,
    prefix: String,
}
async fn once(
    request: &Request,
    workers: &[BoundWorker],
    stop: &Stop,
    delivered: &mut usize,
) -> Result<i32, ProcessError> {
    let parent_epoch = *delivered;
    let mut tasks = Vec::new();
    let operations = workers
        .iter()
        .map(|bound| bound.worker.run_once())
        .collect::<Result<Vec<_>, _>>()?;
    for operation in operations {
        let stop = stop.clone();
        tasks.push(tokio::spawn(async move {
            let mut epoch = parent_epoch;
            run(operation, &stop, &mut epoch).await
        }));
    }
    let mut results = Vec::new();
    for task in tasks {
        results.push(task.await.map_err(|_| ProcessError::task())?);
    }
    if stop.epoch() != parent_epoch {
        *delivered = stop.epoch();
        return Err(ProcessError::cancelled());
    }
    let mut status = 0;
    let mut fatal = None;
    for ((entry, bound), result) in request.entries.iter().zip(workers).zip(results) {
        match result {
            Err(e)
                if matches!(
                    e.kind,
                    ErrorKind::Fatal
                        | ErrorKind::Interrupt
                        | ErrorKind::Cancelled
                        | ErrorKind::BaseGroup
                ) =>
            {
                if fatal.is_none() {
                    fatal = Some(e);
                }
            }
            Err(e) => {
                let message = if e.kind == ErrorKind::Expected {
                    e.display
                } else {
                    joined(&[&t("unexpected error: "), &e.description])?
                };
                stderr(request, &bound.prefix, &message)?;
                status = 1;
            }
            Ok(result) => {
                let label = if workers.len() == 1 {
                    t("")
                } else {
                    joined(&[&entry.name, &t(": ")])?
                };
                let message = result.map_or_else(
                    || Ok(t("no job waiting")),
                    |r| joined(&[&t("job "), &r.job_id, &t(" "), &r.state]),
                )?;
                request
                    .output
                    .line(Stream::Stdout, &joined(&[&label, &message])?)?;
            }
        }
    }
    fatal.map_or(Ok(status), Err)
}
async fn worker_loop(
    request: Arc<Request>,
    index: usize,
    bound: Arc<BoundWorker>,
    stop: Stop,
    mut delivered: usize,
) -> Result<(), ProcessError> {
    loop {
        let result = match bound.worker.run_once() {
            Ok(operation) => run(operation, &stop, &mut delivered).await,
            Err(e) => Err(e),
        };
        let result = match result {
            Err(e)
                if matches!(
                    e.kind,
                    ErrorKind::Fatal
                        | ErrorKind::Interrupt
                        | ErrorKind::Cancelled
                        | ErrorKind::BaseGroup
                ) =>
            {
                return Err(e);
            }
            Err(e) => {
                stderr(
                    &request,
                    &bound.prefix,
                    &joined(&[&t("claim or job error: "), &e.description])?,
                )?;
                None
            }
            Ok(result) => result,
        };
        let seconds = if let Some(result) = result {
            stderr(
                &request,
                &bound.prefix,
                &joined(&[&t("job "), &result.job_id, &t(" "), &result.state])?,
            )?;
            0.0
        } else {
            request.entries[index].poll_seconds
        };
        run(request.wait.sleep(seconds), &stop, &mut delivered).await?;
    }
}
async fn continuous(
    request: Arc<Request>,
    workers: &[Arc<BoundWorker>],
    stop: &Stop,
    delivered: &mut usize,
) -> Result<i32, ProcessError> {
    let mut parent_epoch = *delivered;
    let children = Stop::new();
    let mut aborting = false;
    let mut parent_cancelled = false;
    let mut tasks = tokio::task::JoinSet::new();
    for (index, worker) in workers.iter().enumerate() {
        let mut count = BigInt::from(0);
        while count < request.entries[index].concurrency {
            tasks.spawn(worker_loop(
                request.clone(),
                index,
                worker.clone(),
                children.clone(),
                0,
            ));
            count += 1;
        }
    }
    let mut failed = Vec::new();
    while !tasks.is_empty() {
        let result = tokio::select! {
            result=tasks.join_next()=>result,
            ()=stop.changed(parent_epoch)=>{
                parent_epoch=stop.epoch();*delivered=parent_epoch;parent_cancelled=true;
                if !aborting {aborting=true;children.request();}
                continue;
            }
        };
        let Some(result) = result else {
            break;
        };
        let result = result.map_err(|_| ProcessError::task());
        match result {
            Ok(Err(e)) if e.kind == ErrorKind::Cancelled => {}
            Ok(Err(e)) | Err(e) => {
                failed.push(e);
                if !aborting {
                    aborting = true;
                    children.request();
                }
            }
            Ok(Ok(())) => {}
        }
    }
    if !failed.is_empty() {
        if let Some(index) = failed
            .iter()
            .position(|e| matches!(e.kind, ErrorKind::Interrupt | ErrorKind::Task))
        {
            return Err(failed.remove(index));
        }
        let kind = if failed
            .iter()
            .any(|e| matches!(e.kind, ErrorKind::Fatal | ErrorKind::BaseGroup))
        {
            ErrorKind::BaseGroup
        } else {
            ErrorKind::Group
        };
        return Err(ProcessError {
            kind,
            display: String::from("unhandled errors in a TaskGroup"),
            description: String::from("unhandled errors in a TaskGroup"),
            causes: failed,
        });
    }
    if parent_cancelled || stop.epoch() != parent_epoch {
        *delivered = stop.epoch();
        Err(ProcessError::cancelled())
    } else {
        Ok(0)
    }
}
async fn workers(
    request: Arc<Request>,
    stop: &Stop,
    delivered: &mut usize,
) -> Result<i32, ProcessError> {
    let mut workers = Vec::new();
    for (index, entry) in request.entries.iter().enumerate() {
        let prefix = if request.entries.len() > 1 {
            joined(&[&request.prog, &t(": "), &entry.name])?
        } else {
            request.prog.clone()
        };
        workers.push(Arc::new(BoundWorker {
            worker: request.factory.create(index, entry, &prefix)?,
            prefix,
        }));
    }
    let outcome = if request.once {
        let borrowed = workers
            .iter()
            .map(|w| BoundWorker {
                worker: w.worker.clone(),
                prefix: w.prefix.clone(),
            })
            .collect::<Vec<_>>();
        once(&request, &borrowed, stop, delivered).await
    } else {
        continuous(request.clone(), &workers, stop, delivered).await
    };
    for bound in workers {
        finish(bound.worker.aclose(), stop).await?;
    }
    finish(request.lifecycle.code_close(), stop).await?;
    outcome
}
async fn client(
    request: Arc<Request>,
    stop: &Stop,
    delivered: &mut usize,
) -> Result<i32, ProcessError> {
    if !request.external_client {
        run(request.lifecycle.client_open(), stop, delivered).await?;
    }
    let outcome = workers(request.clone(), stop, delivered).await;
    if !request.external_client {
        client_finish(request.lifecycle.client_close(), stop).await?;
    }
    outcome
}
async fn serve(request: Arc<Request>, stop: Stop) -> Result<i32, ProcessError> {
    if stop.epoch() != 0 {
        return Err(ProcessError::cancelled());
    }
    let mut delivered = 0;
    let outcome = if request.has_launcher {
        match run(request.lifecycle.launcher_start(), &stop, &mut delivered).await {
            Ok(()) => client(request.clone(), &stop, &mut delivered).await,
            Err(e) => Err(e),
        }
    } else {
        client(request.clone(), &stop, &mut delivered).await
    };
    if request.has_launcher {
        finish(request.lifecycle.launcher_close(), &stop).await?;
    }
    outcome
}
/// Owns the supervisor; dropping a waiter requests stop while cleanup still settles.
/// Real factories/operations/waits are deliberately required, never installed here.
pub struct ProcessTask {
    stop: Stop,
    task: Option<tokio::task::JoinHandle<Result<i32, ProcessError>>>,
}
/// Signal bridge that preserves the owned task's settlement barrier.
#[derive(Clone)]
pub struct ProcessCancellation(Stop);
impl ProcessCancellation {
    pub fn cancel(&self) {
        self.0.request();
    }
}
impl ProcessTask {
    #[must_use]
    pub fn cancellation(&self) -> ProcessCancellation {
        ProcessCancellation(self.stop.clone())
    }
    #[must_use]
    pub fn start(request: Request) -> Self {
        let stop = Stop::new();
        let owned = stop.clone();
        Self {
            stop,
            task: Some(tokio::spawn(serve(Arc::new(request), owned))),
        }
    }
    pub fn cancel(&self) {
        self.stop.request();
    }
    /// # Errors
    /// Returns the settled source-ordered lifecycle/worker failure.
    pub async fn settle(mut self) -> Result<i32, ProcessError> {
        self.task
            .take()
            .ok_or_else(ProcessError::task)?
            .await
            .map_err(|_| ProcessError::task())?
    }
}
impl Drop for ProcessTask {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
