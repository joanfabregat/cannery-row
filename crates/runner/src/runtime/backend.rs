//! Backend boundary shared by local, Docker and Kubernetes adapters.
use super::{FutureResult, RuntimeError};
use crate::{
    cancellation::CancellationEvent,
    launcher::{Outcome, StepSpec},
};
use std::{path::PathBuf, sync::Arc};

/// Preparation allocates backend resources. The returned object owns cleanup.
/// Cancellation must be settled before a caller starts another step or removes inputs.
pub trait Backend: Send + Sync {
    fn start(&self) -> FutureResult<'_, ()>;
    fn prepare(&self, spec: StepSpec, root: PathBuf) -> FutureResult<'_, Arc<dyn PreparedStep>>;
    fn release_job<'a>(&'a self, job: &'a str) -> FutureResult<'a, ()>;
    fn close(&self) -> FutureResult<'_, ()>;
}
pub trait PreparedStep: Send + Sync {
    fn run(
        &self,
        log: PathBuf,
        seconds: f64,
        cancel: CancellationEvent,
    ) -> FutureResult<'_, Outcome>;
    fn cleanup(&self) -> FutureResult<'_, ()>;
}

/// Filesystem preparation runs on an owned blocking task and settles before
/// returning. This is the documented native runtime policy, not thread killing.
struct LocalPreparationExecutor;
impl crate::local_preparation::PreparationExecutor for LocalPreparationExecutor {
    fn execute(&self, operation: crate::local_preparation::LocalPreparation) -> PinPreparation<'_> {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || operation.prepare())
                .await
                .map_err(|_| crate::local_preparation::PreparationError::Value)?
        })
    }
}
type PinPreparation<'a> = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<(), crate::local_preparation::PreparationError>>
            + Send
            + 'a,
    >,
>;

pub struct LocalBackend {
    launcher: crate::local_preparation::LocalLauncher,
    step_root: PathBuf,
}
impl LocalBackend {
    /// # Errors
    /// Local execution requires the explicit acknowledgement at the CLI/config boundary.
    pub fn new(
        binary: PathBuf,
        interpreter: PathBuf,
        step_root: PathBuf,
        acknowledged: bool,
    ) -> Result<Self, RuntimeError> {
        let backend =
            crate::local_process::LocalProcessBackend::new(binary, interpreter, acknowledged)
                .map_err(|_| RuntimeError::Configuration)?;
        Ok(Self {
            launcher: crate::local_preparation::LocalLauncher::new(backend, step_root.clone()),
            step_root,
        })
    }
}
impl Backend for LocalBackend {
    fn start(&self) -> FutureResult<'_, ()> {
        Box::pin(async {
            tokio::fs::create_dir_all(&self.step_root).await?;
            Ok(())
        })
    }
    fn prepare(&self, spec: StepSpec, root: PathBuf) -> FutureResult<'_, Arc<dyn PreparedStep>> {
        Box::pin(async move {
            let pending = self.launcher.prepare(spec, root);
            let ready = pending
                .prepare_with(
                    &LocalPreparationExecutor,
                    crate::local_preparation::CopyLimits::default(),
                    crate::local_preparation::MetadataCapabilities {
                        symlink_chmod: false,
                        symlink_chflags: false,
                    },
                )
                .await
                .map_err(|_| RuntimeError::Launch)?;
            Ok(Arc::new(LocalPrepared(ready)) as Arc<dyn PreparedStep>)
        })
    }
    fn release_job<'a>(&'a self, _job: &'a str) -> FutureResult<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn close(&self) -> FutureResult<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
struct LocalPrepared(crate::local_preparation::ReadyLocalStep);
impl PreparedStep for LocalPrepared {
    fn run(
        &self,
        log: PathBuf,
        seconds: f64,
        cancel: CancellationEvent,
    ) -> FutureResult<'_, Outcome> {
        Box::pin(async move {
            let handle = self.0.run(log, seconds.into(), cancel);
            handle.result().await.map_err(|_| RuntimeError::Launch)
        })
    }
    fn cleanup(&self) -> FutureResult<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
