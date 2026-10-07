//! Docker Engine API backend, with an explicit Unix socket and no registry credentials.
use super::{
    Active, CancelOnDrop, Limits, Runs, Tasks, cpu, deadline, env, image, memory, nonce, paths,
    text,
};
use crate::{
    cancellation::CancellationEvent,
    launcher::{Outcome, StepSpec},
    runtime::{
        FutureResult, RuntimeError,
        backend::{Backend, PreparedStep},
    },
};
use bollard::{
    Docker,
    container::LogOutput,
    errors::Error,
    models::ContainerCreateBody,
    query_parameters::{
        CreateContainerOptionsBuilder, CreateImageOptionsBuilder, KillContainerOptionsBuilder,
        ListContainersOptionsBuilder, LogsOptionsBuilder, RemoveContainerOptionsBuilder,
    },
};
use futures_util::StreamExt;
use num_traits::ToPrimitive;
use serde_json::json;
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::io::AsyncWriteExt;

/// Explicit daemon-host GPU mode. The adapter never installs drivers or discovers host devices.
#[derive(Clone, Debug)]
pub enum GpuMode {
    Nvidia,
    Cos { driver_root: PathBuf },
}

struct GpuPool {
    free: std::sync::Mutex<BTreeSet<u32>>,
    changed: tokio::sync::Notify,
}
impl GpuPool {
    async fn acquire(
        self: &Arc<Self>,
        count: usize,
        stop: &CancellationEvent,
    ) -> Result<Arc<GpuHold>, RuntimeError> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if stop.is_set() {
                return Err(RuntimeError::Cancelled);
            }
            {
                let mut free = super::lock(&self.free)?;
                if free.len() >= count {
                    let indices = free.iter().copied().take(count).collect::<Vec<_>>();
                    for index in &indices {
                        free.remove(index);
                    }
                    return Ok(Arc::new(GpuHold {
                        pool: self.clone(),
                        indices,
                        released: AtomicBool::new(false),
                    }));
                }
            }
            tokio::select! {()=changed=>{},()=stop.wait()=>return Err(RuntimeError::Cancelled)}
        }
    }
}
/// Deliberately no Drop release: a failed daemon removal must never make a live GPU available.
struct GpuHold {
    pool: Arc<GpuPool>,
    indices: Vec<u32>,
    released: AtomicBool,
}
impl GpuHold {
    fn release(&self) -> Result<(), RuntimeError> {
        let mut free = super::lock(&self.pool.free)?;
        if !self.released.swap(true, Ordering::AcqRel) {
            free.extend(&self.indices);
            self.pool.changed.notify_waiters();
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct DockerOptions {
    pub runner_id: String,
    pub socket: PathBuf,
    pub uid: u32,
    pub gid: u32,
    pub job_root: PathBuf,
    pub job_root_host: Option<PathBuf>,
    pub pids: i64,
    pub tmp_bytes: i64,
    pub shm_bytes: i64,
    pub default_cpu_millis: u32,
    pub default_memory_bytes: i64,
    pub gpu_devices: Vec<u32>,
    pub gpu_mode: GpuMode,
    /// Bridge egress is unrestricted. Enabling it acknowledges that the host
    /// must enforce metadata blocking and any required destination allowlist.
    pub allow_unrestricted_egress: bool,
    pub limits: Limits,
}
impl DockerOptions {
    pub(crate) fn validate(&self) -> Result<(), RuntimeError> {
        self.limits.validate()?;
        if self.runner_id.is_empty()
            || self.runner_id.len() > 63
            || !self
                .runner_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
            || !self.socket.is_absolute()
            || self.uid == 0
            || self.gid == 0
            || !self.job_root.is_absolute()
            || self
                .job_root_host
                .as_ref()
                .is_some_and(|p| !p.is_absolute())
            || self.pids <= 0
            || self.tmp_bytes <= 0
            || self.shm_bytes <= 0
            || self.default_cpu_millis == 0
            || self.default_memory_bytes <= 0
            || self
                .gpu_devices
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len()
                != self.gpu_devices.len()
            || matches!(&self.gpu_mode,GpuMode::Cos{driver_root} if !driver_root.is_absolute() || driver_root.components().any(|part|matches!(part,std::path::Component::ParentDir)))
        {
            return Err(RuntimeError::Configuration);
        }
        Ok(())
    }
    fn host(&self, path: &Path) -> Result<String, RuntimeError> {
        if self.job_root_host.is_none() {
            if !path.is_absolute()
                || path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(RuntimeError::Integrity);
            }
            return path
                .to_str()
                .map(str::to_owned)
                .ok_or(RuntimeError::Contract);
        }
        let relative = path
            .strip_prefix(&self.job_root)
            .map_err(|_| RuntimeError::Integrity)?;
        if relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(RuntimeError::Integrity);
        }
        let mapped = self
            .job_root_host
            .as_ref()
            .map_or_else(|| path.to_path_buf(), |root| root.join(relative));
        mapped
            .to_str()
            .map(str::to_owned)
            .ok_or(RuntimeError::Contract)
    }
    /// Validate a manifest before a worker claims work when such a manifest is available.
    /// # Errors
    /// Rejects unsupported manifests and invalid resource or environment values.
    pub fn validate_step(&self, spec: &StepSpec) -> Result<(), RuntimeError> {
        image(spec)?;
        env(spec)?;
        memory(spec)?;
        cpu(spec, 1_000_000_000)?;
        if spec
            .resources
            .gpus
            .to_usize()
            .is_none_or(|count| count > self.gpu_devices.len())
            || (spec.networked() && !self.allow_unrestricted_egress)
        {
            return Err(RuntimeError::Configuration);
        }
        for mount in &spec.mounts {
            self.host(mount.source())?;
        }
        Ok(())
    }
    fn config(
        &self,
        spec: &StepSpec,
        root: &Path,
        gpus: &[u32],
    ) -> Result<ContainerCreateBody, RuntimeError> {
        self.validate_step(spec)?;
        let source = self.host(root)?;
        let mut mounts = vec![
            json!({"Type":"bind","Source":source,"Target":"/cr","ReadOnly":true}),
            json!({"Type":"bind","Source":self.host(&root.join("outputs"))?,"Target":"/cr/outputs","ReadOnly":false}),
        ];
        for mount in &spec.mounts {
            mounts.push(json!({"Type":"bind","Source":self.host(mount.source())?,"Target":text(&mount.target().text())?,"ReadOnly":mount.read_only()}));
        }
        let mut host = json!({"ReadonlyRootfs":true,"CapDrop":["ALL"],"SecurityOpt":["no-new-privileges"],"Privileged":false,"NetworkMode":if spec.networked(){"bridge"}else{"none"},"PidsLimit":self.pids,"Tmpfs":{"/tmp":format!("rw,nosuid,nodev,exec,size={},mode=1777",self.tmp_bytes)},"ShmSize":self.shm_bytes,"IpcMode":"private","OomScoreAdj":500,"LogConfig":{"Type":"json-file","Config":{"max-size":"100m","max-file":"2"}},"RestartPolicy":{"Name":"no"},"Mounts":mounts});
        if let Some(bytes) = Some(memory(spec)?.unwrap_or(self.default_memory_bytes)) {
            host["Memory"] = json!(bytes);
            host["MemorySwap"] = json!(bytes);
        }
        if let Some(nanos) = Some(
            cpu(spec, 1_000_000_000)?.unwrap_or(i64::from(self.default_cpu_millis) * 1_000_000),
        ) {
            host["NanoCpus"] = json!(nanos);
        }
        if !gpus.is_empty() {
            match &self.gpu_mode {
                GpuMode::Nvidia => {
                    host["DeviceRequests"] = json!([{"Driver":"nvidia","DeviceIDs":gpus.iter().map(u32::to_string).collect::<Vec<_>>(),"Capabilities":[["gpu"]]}]);
                }
                GpuMode::Cos { driver_root } => {
                    let mut paths = gpus
                        .iter()
                        .map(|index| format!("/dev/nvidia{index}"))
                        .collect::<Vec<_>>();
                    paths.extend(["/dev/nvidiactl".into(), "/dev/nvidia-uvm".into()]);
                    host["Devices"]=json!(paths.iter().map(|path|json!({"PathOnHost":path,"PathInContainer":path,"CgroupPermissions":"rw"})).collect::<Vec<_>>());
                    let mounts = host["Mounts"]
                        .as_array_mut()
                        .ok_or(RuntimeError::Contract)?;
                    for part in ["lib64", "bin"] {
                        mounts.push(json!({"Type":"bind","Source":driver_root.join(part).to_str().ok_or(RuntimeError::Contract)?,"Target":format!("/usr/local/nvidia/{part}"),"ReadOnly":true}));
                    }
                }
            }
        }
        let config = json!({"Image":image(spec)?,"User":format!("{}:{}",self.uid,self.gid),"Entrypoint":spec.command.iter().map(text).collect::<Result<Vec<_>,_>>()?,"Cmd":spec.args.iter().map(text).collect::<Result<Vec<_>,_>>()?,"Env":env(spec)?.iter().map(|(k,v)|format!("{k}={v}")).collect::<Vec<_>>(),"WorkingDir":spec.workdir.as_ref().map(|p|text(&p.text())).transpose()?,"Labels":{"cannery.runner":self.runner_id,"cannery.job":text(&spec.job_id)?,"cannery.step":text(&spec.label)?},"AttachStdin":false,"AttachStdout":false,"AttachStderr":false,"OpenStdin":false,"Tty":false,"NetworkDisabled":!spec.networked(),"HostConfig":host});
        serde_json::from_value(config).map_err(|_| RuntimeError::Contract)
    }
}
struct Inner {
    docker: Docker,
    options: DockerOptions,
    tasks: Tasks,
    closed: AtomicBool,
    active: Active,
    gpus: Arc<GpuPool>,
}
#[derive(Clone)]
pub struct DockerBackend(Arc<Inner>);
fn error(value: Error) -> RuntimeError {
    match value {
        Error::DockerResponseServerError {
            status_code,
            message,
        } => {
            drop(message);
            RuntimeError::Http(status_code)
        }
        _ => RuntimeError::Transport,
    }
}
fn missing(value: &Error) -> bool {
    matches!(
        value,
        Error::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}
impl DockerBackend {
    /// Constructs only an explicit Unix-socket client; no environment discovery or CLI.
    /// # Errors
    /// Rejects invalid options or an unavailable client transport.
    pub fn new(options: DockerOptions) -> Result<Self, RuntimeError> {
        options.validate()?;
        let socket = options.socket.to_str().ok_or(RuntimeError::Configuration)?;
        let version = bollard::ClientVersion {
            major_version: 1,
            minor_version: 41,
        };
        let docker = Docker::connect_with_unix(socket, 60, &version).map_err(error)?;
        Self::with_client(options, docker)
    }
    /// Explicit client injection supports isolated protocol peers without a daemon.
    /// # Errors
    /// Rejects invalid backend options before any API operation.
    pub fn with_client(options: DockerOptions, docker: Docker) -> Result<Self, RuntimeError> {
        options.validate()?;
        let gpus = Arc::new(GpuPool {
            free: std::sync::Mutex::new(options.gpu_devices.iter().copied().collect()),
            changed: tokio::sync::Notify::new(),
        });
        Ok(Self(Arc::new(Inner {
            docker,
            options,
            tasks: Tasks::default(),
            closed: AtomicBool::new(false),
            gpus,
            active: Active::default(),
        })))
    }
}
impl Inner {
    async fn remove(&self, name: &str) -> Result<(), RuntimeError> {
        let options = RemoveContainerOptionsBuilder::default()
            .force(true)
            .v(true)
            .build();
        match tokio::time::timeout(
            self.options.limits.cleanup,
            self.docker.remove_container(name, Some(options)),
        )
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) if missing(&e) => Ok(()),
            Ok(Err(e)) => Err(error(e)),
            Err(_) => Err(RuntimeError::Launch),
        }
    }
    async fn labelled(&self, job: Option<&str>) -> Result<(), RuntimeError> {
        let mut labels = vec![format!("cannery.runner={}", self.options.runner_id)];
        if let Some(job) = job {
            labels.push(format!("cannery.job={job}"));
        }
        let filters: HashMap<String, Vec<String>> = HashMap::from([("label".into(), labels)]);
        let options = ListContainersOptionsBuilder::default()
            .all(true)
            .filters(&filters)
            .build();
        for container in self
            .docker
            .list_containers(Some(options))
            .await
            .map_err(error)?
        {
            let id = container.id.ok_or(RuntimeError::Contract)?;
            self.remove(&id).await?;
        }
        Ok(())
    }
    async fn prepare(
        self: Arc<Self>,
        spec: StepSpec,
        root: PathBuf,
        stop: CancellationEvent,
    ) -> Result<Arc<dyn PreparedStep>, RuntimeError> {
        self.options.validate_step(&spec)?;
        let mounts = spec.mounts.clone();
        let local = root.clone();
        tokio::task::spawn_blocking(move || {
            paths(&local)?;
            for mount in mounts {
                let target = if mount.target().text().equals_utf8("/cr/code") {
                    "code"
                } else {
                    "cache"
                };
                let path = local.join(target);
                if !path.exists() {
                    std::fs::create_dir(&path)?;
                }
                let metadata = std::fs::symlink_metadata(path)?;
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(RuntimeError::Integrity);
                }
            }
            Ok::<_, RuntimeError>(())
        })
        .await
        .map_err(|_| RuntimeError::Filesystem)??;
        let reference = image(&spec)?;
        match self.docker.inspect_image(&reference).await {
            Ok(_) => {}
            Err(e) if missing(&e) => {
                let options = CreateImageOptionsBuilder::default()
                    .from_image(&reference)
                    .build();
                let mut pull = self.docker.create_image(Some(options), None, None);
                while let Some(progress) = pull.next().await {
                    progress.map_err(error)?;
                    if stop.is_set() {
                        return Err(RuntimeError::Cancelled);
                    }
                }
                self.docker.inspect_image(&reference).await.map_err(error)?;
            }
            Err(e) => return Err(error(e)),
        }
        let name = format!("cr-{}-{}", self.options.runner_id, nonce()?);
        if stop.is_set() {
            return Err(RuntimeError::Cancelled);
        }
        let gpus = self
            .gpus
            .acquire(
                spec.resources
                    .gpus
                    .to_usize()
                    .ok_or(RuntimeError::Contract)?,
                &stop,
            )
            .await?;
        let body = match self.options.config(&spec, &root, &gpus.indices) {
            Ok(body) => body,
            Err(error) => {
                gpus.release()?;
                return Err(error);
            }
        };
        let options = CreateContainerOptionsBuilder::default().name(&name).build();
        if let Err(e) = self.docker.create_container(Some(options), body).await {
            self.remove(&name).await?;
            gpus.release()?;
            return Err(error(e));
        }
        if stop.is_set() {
            self.remove(&name).await?;
            gpus.release()?;
            return Err(RuntimeError::Cancelled);
        }
        let runs = Arc::new(Runs::default());
        if let Err(error) = self.active.register(text(&spec.job_id)?, &runs) {
            self.remove(&name).await?;
            gpus.release()?;
            return Err(error);
        }
        Ok(Arc::new(DockerStep {
            inner: self,
            name,
            gpus,
            runs,
            removed: Arc::new(AtomicBool::new(false)),
        }))
    }
}
impl Backend for DockerBackend {
    fn start(&self) -> FutureResult<'_, ()> {
        Box::pin(async move {
            let version = self.0.docker.version().await.map_err(error)?;
            let version = version.api_version.ok_or(RuntimeError::Contract)?;
            let parts = version
                .split('.')
                .map(str::parse::<u32>)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| RuntimeError::Contract)?;
            if parts.len() != 2 || (parts[0], parts[1]) < (1, 41) {
                return Err(RuntimeError::Configuration);
            }
            self.0.labelled(None).await
        })
    }
    fn prepare(&self, spec: StepSpec, root: PathBuf) -> FutureResult<'_, Arc<dyn PreparedStep>> {
        Box::pin(async move {
            if self.0.closed.load(Ordering::Acquire) {
                return Err(RuntimeError::Configuration);
            }
            self.0.options.validate_step(&spec)?;
            let (tx, rx) = tokio::sync::oneshot::channel();
            let inner = self.0.clone();
            let stop = CancellationEvent::new();
            let _stop = CancelOnDrop(stop.clone());
            self.0.tasks.spawn(async move {
                let work=inner.clone().prepare(spec,root,stop.clone());tokio::pin!(work);
                let result=tokio::select!{
                    answer=&mut work=>answer,
                    ()=tokio::time::sleep(inner.options.limits.scheduling)=>{
                        stop.set();
                        match work.await{Ok(prepared)=>{match prepared.cleanup().await{Ok(())=>Err(RuntimeError::Launch),Err(error)=>Err(error)}},Err(error)=>Err(error)}
                    }
                };
                if let Err(Ok(step)) = tx.send(result) {
                    let _ = step.cleanup().await;
                }
            })?;
            rx.await.map_err(|_| RuntimeError::Launch)?
        })
    }
    fn release_job<'a>(&'a self, job: &'a str) -> FutureResult<'a, ()> {
        Box::pin(async move {
            self.0.tasks.settle().await?;
            self.0.active.settle(Some(job)).await?;
            self.0.labelled(Some(job)).await
        })
    }
    fn close(&self) -> FutureResult<'_, ()> {
        Box::pin(async move {
            self.0.closed.store(true, Ordering::Release);
            self.0.tasks.settle().await?;
            self.0.active.settle(None).await?;
            self.0.labelled(None).await
        })
    }
}
struct DockerStep {
    inner: Arc<Inner>,
    name: String,
    runs: Arc<Runs>,
    removed: Arc<AtomicBool>,
    gpus: Arc<GpuHold>,
}
impl DockerStep {
    #[allow(
        clippy::too_many_arguments,
        reason = "Supervisor owns the container, reserved GPUs and cancellation through awaited removal"
    )]
    async fn execute(
        inner: Arc<Inner>,
        name: String,
        runs: Arc<Runs>,
        removed: Arc<AtomicBool>,
        gpus: Arc<GpuHold>,
        log: PathBuf,
        seconds: Duration,
        cancel: CancellationEvent,
    ) -> Result<Outcome, RuntimeError> {
        let outcome = Self::work(&inner, &name, &runs, log, seconds, cancel).await;
        // Remove is awaited even when work fails; cleanup failure cannot become success.
        inner.remove(&name).await?;
        removed.store(true, Ordering::Release);
        gpus.release()?;
        outcome
    }
    async fn work(
        inner: &Inner,
        name: &str,
        runs: &Runs,
        log: PathBuf,
        seconds: Duration,
        cancel: CancellationEvent,
    ) -> Result<Outcome, RuntimeError> {
        if cancel.is_set() || runs.cancel.is_set() {
            let mut out = Outcome::new(None);
            out.cancelled = true;
            return Ok(out);
        }
        if let Err(value) = inner.docker.start_container(name, None).await {
            if let Error::DockerResponseServerError { message, .. } = &value {
                let lowered = message.to_lowercase();
                let code = if lowered.contains("device") || lowered.contains("nvidia") {
                    None
                } else if lowered.contains("executable file not found")
                    || lowered.contains("no such file or directory")
                {
                    Some(127)
                } else if lowered.contains("permission denied")
                    || lowered.contains("is a directory")
                {
                    Some(126)
                } else {
                    None
                };
                if let Some(code) = code {
                    tokio::fs::write(log, b"container command cannot be executed\n").await?;
                    return Ok(Outcome::new(Some(code.into())));
                }
            }
            return Err(error(value));
        }
        let wait = inner.docker.wait_container(name, None);
        tokio::pin!(wait);
        let mut out = Outcome::new(None);
        tokio::select! {
            answer=wait.next()=>{match answer {Some(Ok(v))=>out.exit_code=Some(v.status_code.into()),Some(Err(Error::DockerContainerWaitError{code,..}))=>out.exit_code=Some(code.into()),Some(Err(e))=>return Err(error(e)),None=>return Err(RuntimeError::Contract)}},
            ()=tokio::time::sleep(seconds)=>out.timed_out=true,
            ()=cancel.wait()=>out.cancelled=true,
            ()=runs.cancel.wait()=>out.cancelled=true,
        }
        if out.timed_out || out.cancelled {
            let options = KillContainerOptionsBuilder::default()
                .signal("SIGKILL")
                .build();
            inner
                .docker
                .kill_container(name, Some(options))
                .await
                .map_err(error)?;
            let mut waiting = Box::pin(inner.docker.wait_container(name, None));
            let answer = tokio::time::timeout(inner.options.limits.cleanup, waiting.next())
                .await
                .map_err(|_| RuntimeError::Launch)?;
            match answer {
                Some(Ok(v)) => out.exit_code = Some(v.status_code.into()),
                Some(Err(Error::DockerContainerWaitError { code, .. })) => {
                    out.exit_code = Some(code.into());
                }
                Some(Err(e)) => return Err(error(e)),
                None => return Err(RuntimeError::Contract),
            }
        }
        let state = inner
            .docker
            .inspect_container(name, None)
            .await
            .map_err(error)?
            .state
            .ok_or(RuntimeError::Contract)?;
        out.oom_killed = state.oom_killed.unwrap_or(false);
        let options = LogsOptionsBuilder::default()
            .stdout(true)
            .stderr(true)
            .build();
        let mut logs = inner.docker.logs(name, Some(options));
        let mut file = tokio::fs::File::create(log).await?;
        let mut remaining = inner.options.limits.log_bytes;
        while let Some(chunk) =
            tokio::time::timeout(inner.options.limits.transfer_idle, logs.next())
                .await
                .map_err(|_| RuntimeError::Launch)?
        {
            let chunk = chunk.map_err(error)?;
            let bytes = match chunk {
                LogOutput::StdOut { message }
                | LogOutput::StdErr { message }
                | LogOutput::Console { message }
                | LogOutput::StdIn { message } => message,
            };
            let size = bytes
                .len()
                .min(usize::try_from(remaining).unwrap_or(usize::MAX));
            file.write_all(&bytes[..size]).await?;
            remaining -= u64::try_from(size).map_err(|_| RuntimeError::Contract)?;
            if remaining == 0 {
                break;
            }
        }
        file.flush().await?;
        Ok(out)
    }
}
impl PreparedStep for DockerStep {
    fn run(
        &self,
        log: PathBuf,
        seconds: f64,
        cancel: CancellationEvent,
    ) -> FutureResult<'_, Outcome> {
        Box::pin(async move {
            let seconds = deadline(seconds)?;
            if self.inner.closed.load(Ordering::Acquire) {
                return Err(RuntimeError::Configuration);
            }
            let run = self.runs.install(Self::execute(
                self.inner.clone(),
                self.name.clone(),
                self.runs.clone(),
                self.removed.clone(),
                self.gpus.clone(),
                log,
                seconds,
                cancel,
            ))?;
            let _cancel = CancelOnDrop(self.runs.cancel.clone());
            run.await
        })
    }
    fn cleanup(&self) -> FutureResult<'_, ()> {
        Box::pin(async move {
            self.runs.settle().await?;
            if !self.removed.load(Ordering::Acquire) {
                self.inner.remove(&self.name).await?;
                self.removed.store(true, Ordering::Release);
                self.gpus.release()?;
            }
            Ok(())
        })
    }
}
impl Drop for DockerStep {
    fn drop(&mut self) {
        self.runs.cancel.set();
        if !self.removed.load(Ordering::Acquire) {
            let inner = self.inner.clone();
            let name = self.name.clone();
            let runs = self.runs.clone();
            let gpus = self.gpus.clone();
            let _ = self.inner.tasks.spawn(async move {
                let _ = runs.settle().await;
                if inner.remove(&name).await.is_ok() {
                    let _ = gpus.release();
                }
            });
        }
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
