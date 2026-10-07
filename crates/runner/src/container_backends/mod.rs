//! Native Engine API and Kubernetes adapters. Credentials stay in clients.
pub mod docker;
pub mod kubernetes;
mod transfer;

use crate::{
    cancellation::CancellationEvent,
    launcher::{Outcome, StepSpec},
    runtime::RuntimeError,
};

use futures_util::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use num_traits::ToPrimitive;
use std::{fmt::Write as _, sync::Mutex, time::Duration};

/// Explicit finite storage and transport bounds, independent of the step deadline.
#[derive(Clone, Debug)]
pub struct Limits {
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub files: usize,
    pub log_bytes: u64,
    pub scheduling: Duration,
    pub transfer_idle: Duration,
    pub cleanup: Duration,
}
impl Limits {
    pub(crate) fn validate(&self) -> Result<(), RuntimeError> {
        if self.input_bytes == 0
            || self.output_bytes == 0
            || self.files == 0
            || self.log_bytes == 0
            || self.scheduling.is_zero()
            || self.transfer_idle.is_zero()
            || self.cleanup.is_zero()
        {
            return Err(RuntimeError::Configuration);
        }
        Ok(())
    }
}
pub(crate) fn text(value: &String) -> Result<String, RuntimeError> {
    let value = value.as_utf8().ok_or(RuntimeError::Contract)?;
    if value.contains('\0') {
        return Err(RuntimeError::Contract);
    }
    Ok(value)
}
pub(crate) fn image(spec: &StepSpec) -> Result<String, RuntimeError> {
    let (repo, digest) =
        crate::launcher::pinned_image(&spec.image).map_err(|_| RuntimeError::Contract)?;
    Ok(format!("{}@{}", text(&repo)?, text(&digest)?))
}
pub(crate) fn start_failure_code(message: &str) -> Option<i32> {
    let message = message.to_lowercase();
    if message.contains("device") || message.contains("nvidia") {
        None
    } else if message.contains("executable file not found") || message.contains("no such file") {
        Some(127)
    } else if message.contains("permission denied") || message.contains("is a directory") {
        Some(126)
    } else {
        None
    }
}
pub(crate) fn env(spec: &StepSpec) -> Result<Vec<(String, String)>, RuntimeError> {
    spec.validate().map_err(|_| RuntimeError::Contract)?;
    let mut values = Vec::new();
    for (key, value) in &spec.env {
        let key = text(key)?;
        if key.is_empty()
            || key.contains('=')
            || key.starts_with("NVIDIA_")
            || key.starts_with("CANNERY_")
        {
            return Err(RuntimeError::Contract);
        }
        if !matches!(key.as_str(), "CR_ROOT" | "HOME" | "TMPDIR") {
            values.push((key, text(value)?));
        }
    }
    values.extend([
        ("CR_ROOT".into(), "/cr".into()),
        ("HOME".into(), "/tmp".into()),
        ("TMPDIR".into(), "/tmp".into()),
    ]);
    Ok(values)
}
pub(crate) fn memory(spec: &StepSpec) -> Result<Option<i64>, RuntimeError> {
    spec.resources
        .memory_bytes
        .as_ref()
        .map(|v| v.to_i64().filter(|n| *n > 0).ok_or(RuntimeError::Contract))
        .transpose()
}
pub(crate) fn cpu(spec: &StepSpec, scale: u64) -> Result<Option<i64>, RuntimeError> {
    spec.resources
        .cpu
        .as_ref()
        .map(|v| {
            (v * num_bigint::BigInt::from(scale))
                .ceil()
                .to_integer()
                .to_i64()
                .filter(|n| *n > 0)
                .ok_or(RuntimeError::Contract)
        })
        .transpose()
}
pub(crate) fn deadline(seconds: f64) -> Result<Duration, RuntimeError> {
    Duration::try_from_secs_f64(seconds)
        .ok()
        .filter(|v| !v.is_zero())
        .ok_or(RuntimeError::Configuration)
}
pub(crate) fn lock<T>(value: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, RuntimeError> {
    value.lock().map_err(|_| RuntimeError::Launch)
}
pub(crate) type Run = Shared<BoxFuture<'static, Result<Outcome, RuntimeError>>>;
#[derive(Default)]
pub(crate) struct Runs {
    run: Mutex<Option<Run>>,
    pub cancel: CancellationEvent,
}
impl Runs {
    pub fn install(
        &self,
        work: impl std::future::Future<Output = Result<Outcome, RuntimeError>> + Send + 'static,
    ) -> Result<Run, RuntimeError> {
        let mut state = lock(&self.run)?;
        if state.is_some() {
            return Err(RuntimeError::Configuration);
        }
        let task = tokio::spawn(work);
        let run = async move { task.await.map_err(|_| RuntimeError::Launch)? }
            .boxed()
            .shared();
        *state = Some(run.clone());
        Ok(run)
    }
    pub async fn settle(&self) -> Result<(), RuntimeError> {
        self.cancel.set();
        let run = lock(&self.run)?.clone();
        if let Some(run) = run {
            let _ = run.await;
        }
        Ok(())
    }
}
pub(crate) struct CancelOnDrop(pub CancellationEvent);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.set();
    }
}
/// Backend-owned tasks remain attached to the client lifetime and are drained by close.
#[derive(Default)]
pub(crate) struct Tasks(Mutex<Vec<tokio::task::JoinHandle<()>>>);
impl Tasks {
    pub fn spawn(
        &self,
        work: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<(), RuntimeError> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| RuntimeError::Launch)?;
        let mut tasks = lock(&self.0)?;
        tasks.retain(|task| !task.is_finished());
        tasks.push(runtime.spawn(work));
        Ok(())
    }
    pub async fn settle(&self) -> Result<(), RuntimeError> {
        loop {
            let tasks = std::mem::take(&mut *lock(&self.0)?);
            if tasks.is_empty() {
                return Ok(());
            }
            for task in tasks {
                task.await.map_err(|_| RuntimeError::Launch)?;
            }
        }
    }
}
pub(crate) fn nonce() -> Result<String, RuntimeError> {
    let mut data = [0u8; 12];
    getrandom::fill(&mut data).map_err(|_| RuntimeError::Launch)?;
    let mut value = String::with_capacity(24);
    for byte in data {
        let _ = write!(value, "{byte:02x}");
    }
    Ok(value)
}
pub(crate) fn paths(root: &std::path::Path) -> Result<(), RuntimeError> {
    if !root.is_absolute() {
        return Err(RuntimeError::Configuration);
    }
    for name in ["inputs", "outputs"] {
        let path = root.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(info) if !info.is_dir() || info.file_type().is_symlink() => {
                return Err(RuntimeError::Integrity);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&path)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Explicit prepared-step ownership; no daemon PID or global lookup is involved.
#[derive(Default)]
pub(crate) struct Active(Mutex<Vec<(String, std::sync::Weak<Runs>)>>);
impl Active {
    pub fn register(&self, job: String, runs: &std::sync::Arc<Runs>) -> Result<(), RuntimeError> {
        let mut active = lock(&self.0)?;
        active.retain(|(_, step)| step.strong_count() > 0);
        active.push((job, std::sync::Arc::downgrade(runs)));
        Ok(())
    }
    pub async fn settle(&self, job: Option<&str>) -> Result<(), RuntimeError> {
        let steps = lock(&self.0)?
            .iter()
            .filter(|(name, _)| job.is_none_or(|job| job == name))
            .filter_map(|(_, step)| step.upgrade())
            .collect::<Vec<_>>();
        for step in &steps {
            step.cancel.set();
        }
        for step in steps {
            step.settle().await?;
        }
        Ok(())
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
