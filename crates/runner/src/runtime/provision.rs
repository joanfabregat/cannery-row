//! Real shared code/cache/setup composition with owned filesystem settlement.
use super::{
    FutureResult, RuntimeError,
    backend::Backend,
    github::GitHub,
    worker::{Provisioner, SetupReport},
};
use crate::{
    cache::{CacheError, CacheEvent, CacheLog},
    cancellation::CancellationEvent,
    code_store::{self, CodeCancellation, CodeError, CodeFuture, FsOperation, FsReceipt},
    launcher::{Outcome, StepSpec},
    setup,
};
use cannery_core::json;
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};
use tokio::sync::{mpsc, oneshot};

type FsReply = oneshot::Sender<Result<FsReceipt, CodeError>>;
type HoldRegistry = BTreeMap<(String, String), (Arc<crate::cache::CacheRoot>, Vec<PathBuf>)>;
struct FsQueue(mpsc::UnboundedSender<(FsOperation, Option<FsReply>)>);
impl FsQueue {
    fn new() -> Arc<Self> {
        let (sender, mut receiver) = mpsc::unbounded_channel::<(FsOperation, Option<FsReply>)>();
        tokio::spawn(async move {
            while let Some((operation, reply)) = receiver.recv().await {
                let result = tokio::task::spawn_blocking(move || operation.run())
                    .await
                    .map_err(|_| CodeError::Callback)
                    .and_then(|v| v);
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
                // An abandoned recipient drops the receipt: its armed cleanup
                // is appended to this same FIFO, after the completed operation.
            }
        });
        Arc::new(Self(sender))
    }
}
impl code_store::FsExecutor for FsQueue {
    fn execute(
        &self,
        operation: FsOperation,
        cancel: CodeCancellation,
    ) -> CodeFuture<'_, FsReceipt> {
        Box::pin(async move {
            if cancel.requested() {
                return Err(CodeError::Cancelled);
            }
            let (sender, receiver) = oneshot::channel();
            self.0
                .send((operation, Some(sender)))
                .map_err(|_| CodeError::Callback)?;
            receiver.await.map_err(|_| CodeError::Callback)?
        })
    }
    fn detach(&self, operations: Vec<FsOperation>) {
        for operation in operations {
            let _ = self.0.send((operation, None));
        }
    }
}
struct Log;
impl CacheLog for Log {
    fn event(&self, event: CacheEvent) -> Result<(), CacheError> {
        let mut output = std::io::stderr().lock();
        match event {
            CacheEvent::Evicted(count) => {
                writeln!(output, "cannery runner: evicted {count} cache entries")
            }
            CacheEvent::OverCap { .. } => {
                writeln!(
                    output,
                    "cannery runner: held cache exceeds configured capacity"
                )
            }
        }
        .map_err(|error| CacheError::Io {
            errno: error.raw_os_error(),
        })
    }
}
struct SetupRunner(Arc<dyn Backend>, SystemTime);
impl setup::RunSetup for SetupRunner {
    fn run(
        &self,
        step: StepSpec,
        directory: PathBuf,
        seconds: BigInt,
        mut cancel: setup::Cancellation,
    ) -> setup::SetupFuture<'_, Outcome> {
        Box::pin(async move {
            let seconds = seconds
                .to_f64()
                .filter(|v| v.is_finite() && *v > 0.0)
                .ok_or(setup::SetupError::Value)?;
            let seconds = seconds.min(
                self.1
                    .duration_since(SystemTime::now())
                    .map_or(0.0, |v| v.as_secs_f64()),
            );
            if seconds <= 0.0 {
                return Err(setup::SetupError::Cancelled);
            }
            let prepared = self
                .0
                .prepare(step, directory.clone())
                .await
                .map_err(|_| setup::SetupError::Callback)?;
            let signal = CancellationEvent::new();
            if cancel.requested() {
                signal.set();
            }
            let run = prepared.run(directory.join("setup.log"), seconds, signal.clone());
            tokio::pin!(run);
            let outcome = tokio::select! {
                result=&mut run=>result,
                ()=cancel.cancelled()=>{signal.set();run.await},
            };
            prepared
                .cleanup()
                .await
                .map_err(|_| setup::SetupError::Callback)?;
            outcome.map_err(|_| setup::SetupError::Callback)
        })
    }
}
/// One shared cache; each provisioned step owns its exact hold list until release.
pub struct CodeProvisioner {
    slot: code_store::CodeSlot,
    backend: Arc<dyn Backend>,
    holds: Mutex<HoldRegistry>,
    reports: Mutex<BTreeMap<(String, String), SetupReport>>,
}
struct ArmedHolds {
    cache: Arc<crate::cache::CacheRoot>,
    held: Option<Vec<PathBuf>>,
}
impl Drop for ArmedHolds {
    fn drop(&mut self) {
        if let Some(held) = self.held.take() {
            let cache = self.cache.clone();
            tokio::task::spawn_blocking(move || {
                let _ = cache.release(&held);
            });
        }
    }
}
impl CodeProvisioner {
    pub(crate) fn check_config(config: &crate::config::ProcessConfig) -> Result<(), RuntimeError> {
        GitHub::check_settings(&config.github)?;
        Self::max_bytes(config)?;
        Ok(())
    }
    fn max_bytes(config: &crate::config::ProcessConfig) -> Result<BigInt, RuntimeError> {
        config
            .cache_max_bytes
            .as_ref()
            .map(|v| v.as_utf8().ok_or(RuntimeError::Configuration))
            .transpose()?
            .map(|v| v.parse::<BigInt>().map_err(|_| RuntimeError::Configuration))
            .transpose()
            .map(|value| value.unwrap_or_else(|| crate::cache::DEFAULT_MAX_BYTES.into()))
    }
    /// # Errors
    /// Configured credentials must be usable before accepting work.
    pub fn new(
        config: &crate::config::ProcessConfig,
        backend: Arc<dyn Backend>,
    ) -> Result<Self, RuntimeError> {
        let github = Arc::new(GitHub::from_settings(&config.github)?);
        let max_bytes = Self::max_bytes(config)?;
        let slot = code_store::CodeSlot::new(
            code_store::SlotConfig {
                root: config.cache_root.clone(),
                max_bytes,
                allowed_repos: config.github.allowed_repos.clone(),
            },
            github,
            FsQueue::new(),
            Arc::new(Log),
        );
        Ok(Self {
            slot,
            backend,
            holds: Mutex::new(BTreeMap::new()),
            reports: Mutex::new(BTreeMap::new()),
        })
    }
}
impl Provisioner for CodeProvisioner {
    fn prepare<'a>(
        &'a self,
        manifest: &'a Value,
        step: StepSpec,
        root: &'a Path,
        cancel: CancellationEvent,
        deadline: SystemTime,
    ) -> FutureResult<'a, StepSpec> {
        Box::pin(async move {
            if manifest["spec"].get("code").is_none_or(Value::is_null) {
                return Ok(step);
            }
            let document = Arc::new(
                json::decode(&serde_json::to_vec(manifest)?, 9994)
                    .map_err(|_| RuntimeError::Contract)?,
            );
            let store = self
                .slot
                .get(CodeCancellation::from_event(cancel.clone()))
                .await
                .map_err(cache_acquisition_error)?;
            let key = step_key(&step)?;
            let task = setup::ProvisionTask::start(setup::ProvisionRequest {
                manifest: document,
                step,
                cache: store.cache.clone(),
                source: store.clone(),
                directory: root.join(".setup"),
                run: Arc::new(SetupRunner(self.backend.clone(), deadline)),
                rendering_budget: 9992,
                executor: Arc::new(setup::SpawnBlocking),
            });
            let cancellation = task.cancellation_handle();
            let settle = task.settle();
            tokio::pin!(settle);
            let provisioned = tokio::select! {
                result=&mut settle=>result,
                ()=cancel.wait()=>{cancellation.cancel();settle.await},
            }
            .map_err(setup_error)?;
            let ready = provisioned.ready();
            let mut ownership = ArmedHolds {
                cache: store.cache.clone(),
                held: Some(provisioned.held),
            };
            let failure = provisioned
                .setup
                .as_ref()
                .map_or(RuntimeError::SetupFailed, |run| {
                    if run.outcome.cancelled {
                        RuntimeError::LostLease
                    } else if run.outcome.timed_out {
                        RuntimeError::DeadlineExceeded
                    } else {
                        RuntimeError::SetupFailed
                    }
                });
            if let Some(run) = &provisioned.setup {
                self.reports
                    .lock()
                    .map_err(|_| RuntimeError::Contract)?
                    .insert(
                        key.clone(),
                        SetupReport {
                            log: root.join(".setup/setup.log"),
                            label: key.1.clone(),
                            outcome: run.outcome.clone(),
                        },
                    );
            }
            if !ready || cancel.is_set() {
                let cache = ownership.cache.clone();
                let held = ownership.held.take().ok_or(RuntimeError::Contract)?;
                tokio::task::spawn_blocking(move || cache.release(&held))
                    .await
                    .map_err(|_| RuntimeError::Filesystem)?
                    .map_err(|_| RuntimeError::Filesystem)?;
                return Err(if cancel.is_set() {
                    RuntimeError::LostLease
                } else {
                    failure
                });
            }
            let mut holds = self.holds.lock().map_err(|_| RuntimeError::Contract)?;
            if holds.contains_key(&key) {
                return Err(RuntimeError::Contract);
            }
            holds.insert(
                key,
                (
                    store.cache.clone(),
                    ownership.held.take().ok_or(RuntimeError::Contract)?,
                ),
            );
            Ok(provisioned.step)
        })
    }
    fn take_setup<'a>(&'a self, step: &'a StepSpec) -> FutureResult<'a, Option<SetupReport>> {
        Box::pin(async move {
            Ok(self
                .reports
                .lock()
                .map_err(|_| RuntimeError::Contract)?
                .remove(&step_key(step)?))
        })
    }
    fn release<'a>(&'a self, step: &'a StepSpec) -> FutureResult<'a, ()> {
        Box::pin(async move {
            let entry = {
                self.holds
                    .lock()
                    .map_err(|_| RuntimeError::Contract)?
                    .remove(&step_key(step)?)
            };
            if let Some((cache, held)) = entry {
                tokio::task::spawn_blocking(move || cache.release(&held))
                    .await
                    .map_err(|_| RuntimeError::Filesystem)?
                    .map_err(|_| RuntimeError::Filesystem)?;
            }
            Ok(())
        })
    }
    fn close(&self) -> FutureResult<'_, ()> {
        Box::pin(async move {
            let holds =
                { std::mem::take(&mut *self.holds.lock().map_err(|_| RuntimeError::Contract)?) };
            tokio::task::spawn_blocking(move || {
                for (_, (cache, held)) in holds {
                    cache.release(&held)?;
                }
                Ok::<(), CacheError>(())
            })
            .await
            .map_err(|_| RuntimeError::Filesystem)?
            .map_err(|_| RuntimeError::Filesystem)?;
            self.slot
                .close()
                .await
                .map_err(|_| RuntimeError::Filesystem)
        })
    }
}
fn step_key(step: &StepSpec) -> Result<(String, String), RuntimeError> {
    Ok((
        step.job_id.as_utf8().ok_or(RuntimeError::Contract)?,
        step.label.as_utf8().ok_or(RuntimeError::Contract)?,
    ))
}

fn setup_error(error: setup::SetupError) -> RuntimeError {
    match error {
        setup::SetupError::CodeNotAllowed => RuntimeError::CodeNotAllowed,
        setup::SetupError::InvalidCode => RuntimeError::InvalidCode,
        setup::SetupError::Cancelled => RuntimeError::LostLease,
        _ => RuntimeError::Launch,
    }
}

fn cache_acquisition_error(error: CodeError) -> RuntimeError {
    match error {
        CodeError::Cache(CacheError::Busy) => RuntimeError::CacheRootInUse,
        _ => RuntimeError::Configuration,
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_cache_contention_has_the_actionable_safe_category() {
        assert_eq!(
            cache_acquisition_error(CodeError::Cache(CacheError::Busy)),
            RuntimeError::CacheRootInUse
        );
        assert_eq!(
            RuntimeError::CacheRootInUse.to_string(),
            "runner cache root is already in use"
        );
        for error in [
            CodeError::RunnerError,
            CodeError::GitHub,
            CodeError::Cancelled,
            CodeError::State,
            CodeError::Cache(CacheError::Io { errno: Some(13) }),
        ] {
            assert_eq!(cache_acquisition_error(error), RuntimeError::Configuration);
        }
    }
}
