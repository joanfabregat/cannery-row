//! Kubernetes API backend. Transfer Pods carry data; step Pods carry no credentials.
use super::{
    Active, CancelOnDrop, Limits, Runs, Tasks, cpu, deadline, env, image, memory, nonce, paths,
    text, transfer,
};
use crate::{
    cancellation::CancellationEvent,
    launcher::{Outcome, StepSpec},
    runtime::{
        FutureResult, RuntimeError,
        backend::{Backend, PreparedStep},
    },
};
use futures_util::io::AsyncReadExt as FuturesRead;
use k8s_openapi::api::core::v1::{PersistentVolumeClaim, Pod};
use kube::{
    Api, Client, Config,
    api::{AttachParams, DeleteParams, ListParams, LogParams, PostParams},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone, Debug)]
pub struct KubernetesOptions {
    pub runner_id: String,
    pub namespace: String,
    pub uid: i64,
    pub gid: i64,
    pub volume_bytes: u64,
    pub storage_class: Option<String>,
    pub gpu_runtime_class: Option<String>,
    pub transfer_image: String,
    pub tmp_bytes: u64,
    pub shm_bytes: u64,
    pub default_cpu_millis: u32,
    pub default_memory_bytes: i64,
    /// API Service address used by the no-network init-container gate.
    pub api_service_host: String,
    pub api_service_port: u16,
    /// The namespace must have operator-installed deny policies for transfer/none
    /// Pods and metadata/cluster blocking for egress Pods. No allowlist is inferred.
    pub namespace_policy_acknowledged: bool,
    pub allow_unrestricted_egress: bool,
    pub limits: Limits,
}
fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))[..16].into()
}
fn label(value: &str) -> String {
    if value.len() <= 63
        && !value.is_empty()
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
    {
        value.into()
    } else {
        hash(value)
    }
}
fn map_error(error: kube::Error) -> RuntimeError {
    match error {
        kube::Error::Api(value) => RuntimeError::Http(value.code),
        _ => RuntimeError::Transport,
    }
}
fn absent(error: &kube::Error) -> bool {
    matches!(error,kube::Error::Api(v) if v.code==404)
}
impl KubernetesOptions {
    pub(crate) fn validate(&self) -> Result<(), RuntimeError> {
        self.limits.validate()?;
        if self.runner_id.is_empty()
            || self.runner_id.len() > 63
            || label(&self.runner_id) != self.runner_id
            || self.namespace.is_empty()
            || self.namespace.len() > 63
            || !self
                .namespace
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !self
                .namespace
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !self
                .namespace
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || self.uid <= 0
            || self.gid <= 0
            || self.volume_bytes == 0
            || self.tmp_bytes == 0
            || self.shm_bytes == 0
            || self.default_cpu_millis == 0
            || self.default_memory_bytes <= 0
            || self.api_service_port == 0
            || self.api_service_host.is_empty()
            || !self
                .api_service_host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".:-".contains(&b))
            || !self.namespace_policy_acknowledged
        {
            return Err(RuntimeError::Configuration);
        }
        crate::launcher::pinned_image(&String::from(&self.transfer_image))
            .map_err(|_| RuntimeError::Configuration)?;
        Ok(())
    }
    /// # Errors
    /// Rejects unsupported manifests and invalid resource or environment values.
    pub fn validate_step(&self, spec: &StepSpec) -> Result<(), RuntimeError> {
        image(spec)?;
        env(spec)?;
        memory(spec)?;
        cpu(spec, 1000)?;
        if spec.networked() && !self.allow_unrestricted_egress {
            return Err(RuntimeError::Configuration);
        }
        if spec.resources.gpus < 0.into() {
            return Err(RuntimeError::Contract);
        }
        Ok(())
    }
    fn labels(&self, spec: &StepSpec, role: &str) -> Result<Value, RuntimeError> {
        Ok(
            json!({"cannery.runner":self.runner_id,"cannery.job":label(&text(&spec.job_id)?),"cannery.step":label(&text(&spec.label)?),"cannery.role":role,"cannery.network":if role=="step"&&spec.networked(){"egress"}else{"none"}}),
        )
    }
    fn security(&self) -> Value {
        json!({"runAsNonRoot":true,"runAsUser":self.uid,"runAsGroup":self.gid,"fsGroup":self.gid,"fsGroupChangePolicy":"OnRootMismatch","seccompProfile":{"type":"RuntimeDefault"}})
    }
    fn container_security() -> Value {
        json!({"runAsNonRoot":true,"privileged":false,"allowPrivilegeEscalation":false,"readOnlyRootFilesystem":true,"capabilities":{"drop":["ALL"]}})
    }
    fn pod(
        &self,
        name: &str,
        spec: &StepSpec,
        role: &str,
        mut body: Value,
    ) -> Result<Pod, RuntimeError> {
        body["restartPolicy"] = json!("Never");
        body["automountServiceAccountToken"] = json!(false);
        body["enableServiceLinks"] = json!(false);
        body["hostNetwork"] = json!(false);
        body["hostPID"] = json!(false);
        body["hostIPC"] = json!(false);
        body["terminationGracePeriodSeconds"] = json!(2);
        body["securityContext"] = self.security();
        serde_json::from_value(json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":name,"labels":self.labels(spec,role)?},"spec":body})).map_err(|_|RuntimeError::Contract)
    }
    fn transfer_pod(&self, name: &str, spec: &StepSpec, volume: &str) -> Result<Pod, RuntimeError> {
        self.pod(name,spec,"transfer",json!({"activeDeadlineSeconds":86400,"containers":[{"name":"transfer","image":self.transfer_image,"command":["sleep","86400"],"securityContext":Self::container_security(),"resources":{"requests":{"cpu":"100m","memory":"64Mi"},"limits":{"cpu":"500m","memory":"256Mi"}},"volumeMounts":[{"name":"work","mountPath":"/vol"}]}],"volumes":[{"name":"work","persistentVolumeClaim":{"claimName":volume}}]}))
    }
    fn step_pod(
        &self,
        name: &str,
        spec: &StepSpec,
        volume: &str,
        directory: &str,
        seconds: Duration,
    ) -> Result<Pod, RuntimeError> {
        self.validate_step(spec)?;
        let mut limits = json!({});
        if let Some(value) = Some(cpu(spec, 1000)?.unwrap_or(i64::from(self.default_cpu_millis))) {
            limits["cpu"] = json!(format!("{value}m"));
        }
        if let Some(value) = Some(memory(spec)?.unwrap_or(self.default_memory_bytes)) {
            limits["memory"] = json!(value.to_string());
        }
        let requests = limits.clone();
        if spec.resources.gpus > 0.into() {
            limits["nvidia.com/gpu"] = json!(spec.resources.gpus.to_string());
        }
        let mut mounts = vec![
            json!({"name":"work","mountPath":"/cr","subPath":directory,"readOnly":true}),
            json!({"name":"work","mountPath":"/cr/outputs","subPath":format!("{directory}/outputs")}),
            json!({"name":"tmp","mountPath":"/tmp"}),
            json!({"name":"shm","mountPath":"/dev/shm"}),
        ];
        for mount in &spec.mounts {
            let target = text(&mount.target().text())?;
            mounts.push(json!({"name":"work","mountPath":target,"subPath":format!("{directory}/{}",if target=="/cr/code"{"code"}else{"cache"}),"readOnly":mount.read_only()}));
        }
        let mut container = json!({"name":"step","image":image(spec)?,"imagePullPolicy":"IfNotPresent","command":spec.command.iter().map(text).collect::<Result<Vec<_>,_>>()?,"args":spec.args.iter().map(text).collect::<Result<Vec<_>,_>>()?,"env":env(spec)?.iter().map(|(k,v)|json!({"name":k,"value":v})).collect::<Vec<_>>(),"resources":{"requests":requests,"limits":limits},"securityContext":Self::container_security(),"volumeMounts":mounts});
        if let Some(workdir) = &spec.workdir {
            container["workingDir"] = json!(text(&workdir.text())?);
        }
        let backstop = seconds
            .as_secs()
            .checked_add(u64::from(seconds.subsec_nanos() > 0))
            .and_then(|n| n.checked_add(self.limits.scheduling.as_secs()))
            .and_then(|n| n.checked_add(60))
            .ok_or(RuntimeError::Configuration)?;
        let mut body = json!({"activeDeadlineSeconds":backstop,"containers":[container],"volumes":[{"name":"work","persistentVolumeClaim":{"claimName":volume}},{"name":"tmp","emptyDir":{"medium":"Memory","sizeLimit":self.tmp_bytes.to_string()}},{"name":"shm","emptyDir":{"medium":"Memory","sizeLimit":self.shm_bytes.to_string()}}]});
        if spec.resources.gpus > 0.into()
            && let Some(class) = &self.gpu_runtime_class
        {
            body["runtimeClassName"] = json!(class);
        }
        if !spec.networked() {
            let script = "command -v wget >/dev/null || exit 1; h=$1; p=$2; case $h in *:*) h=\"[$h]\";; esac; start=$(date +%s); while :; do out=$(wget -q -T 2 -O /dev/null \"http://$h:$p/\" 2>&1); case $out in *\"can't connect\"*|*\"timed out\"*) exit 0;; esac; [ $(($(date +%s)-start)) -lt 120 ] || exit 1; sleep 1; done";
            body["initContainers"] = json!([{"name":"network-gate","image":self.transfer_image,"command":["sh","-c",script,"sh",self.api_service_host,self.api_service_port.to_string()],"securityContext":Self::container_security(),"resources":{"requests":{"cpu":"10m","memory":"16Mi"},"limits":{"cpu":"100m","memory":"32Mi"}}}]);
        }
        self.pod(name, spec, "step", body)
    }
}
struct Inner {
    client: Client,
    options: KubernetesOptions,
    jobs: tokio::sync::Mutex<BTreeSet<String>>,
    tasks: Tasks,
    closed: AtomicBool,
    active: Active,
}
#[derive(Clone)]
pub struct KubernetesBackend(Arc<Inner>);
impl KubernetesBackend {
    /// Uses explicit configuration only; kubeconfig/exec plugin discovery is refused.
    /// # Errors
    /// Rejects invalid options, insecure verification or credential exec plugins.
    pub fn connect(options: KubernetesOptions, mut config: Config) -> Result<Self, RuntimeError> {
        options.validate()?;
        if config.auth_info.exec.is_some()
            || config.auth_info.auth_provider.is_some()
            || config.accept_invalid_certs
        {
            return Err(RuntimeError::Configuration);
        }
        config.default_namespace.clone_from(&options.namespace);
        config.connect_timeout = Some(options.limits.scheduling);
        config.read_timeout = Some(options.limits.scheduling);
        config.write_timeout = Some(options.limits.scheduling);
        let client = Client::try_from(config).map_err(map_error)?;
        Self::with_client(options, client)
    }
    /// # Errors
    /// Rejects invalid backend options before any API operation.
    pub fn with_client(options: KubernetesOptions, client: Client) -> Result<Self, RuntimeError> {
        options.validate()?;
        Ok(Self(Arc::new(Inner {
            client,
            options,
            jobs: tokio::sync::Mutex::new(BTreeSet::new()),
            tasks: Tasks::default(),
            closed: AtomicBool::new(false),
            active: Active::default(),
        })))
    }
}
impl Inner {
    fn pods(&self) -> Api<Pod> {
        Api::namespaced(self.client.clone(), &self.options.namespace)
    }
    fn volumes(&self) -> Api<PersistentVolumeClaim> {
        Api::namespaced(self.client.clone(), &self.options.namespace)
    }
    async fn delete_pod(&self, name: &str) -> Result<(), RuntimeError> {
        let pods = self.pods();
        let params = DeleteParams {
            grace_period_seconds: Some(2),
            ..DeleteParams::default()
        };
        match pods.delete(name, &params).await {
            Ok(_) => {}
            Err(e) if absent(&e) => return Ok(()),
            Err(e) => return Err(map_error(e)),
        }
        tokio::time::timeout(self.options.limits.cleanup, async {
            loop {
                match pods.get(name).await {
                    Err(e) if absent(&e) => return Ok(()),
                    Err(e) => return Err(map_error(e)),
                    Ok(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            }
        })
        .await
        .map_err(|_| RuntimeError::Launch)?
    }
    async fn delete_volume(&self, name: &str) -> Result<(), RuntimeError> {
        let api = self.volumes();
        match api.delete(name, &DeleteParams::default()).await {
            Ok(_) => {}
            Err(e) if absent(&e) => return Ok(()),
            Err(e) => return Err(map_error(e)),
        }
        tokio::time::timeout(self.options.limits.cleanup, async {
            loop {
                match api.get(name).await {
                    Err(e) if absent(&e) => return Ok(()),
                    Err(e) => return Err(map_error(e)),
                    Ok(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            }
        })
        .await
        .map_err(|_| RuntimeError::Launch)?
    }
    async fn labelled(&self, job: Option<&str>) -> Result<(), RuntimeError> {
        let mut selector = format!("cannery.runner={}", self.options.runner_id);
        if let Some(job) = job {
            let _ = write!(selector, ",cannery.job={}", label(job));
        }
        let params = ListParams::default().labels(&selector);
        for pod in self.pods().list(&params).await.map_err(map_error)?.items {
            self.delete_pod(pod.metadata.name.as_deref().ok_or(RuntimeError::Contract)?)
                .await?;
        }
        for volume in self.volumes().list(&params).await.map_err(map_error)?.items {
            self.delete_volume(
                volume
                    .metadata
                    .name
                    .as_deref()
                    .ok_or(RuntimeError::Contract)?,
            )
            .await?;
        }
        Ok(())
    }
    async fn volume(&self, spec: &StepSpec) -> Result<String, RuntimeError> {
        let job = text(&spec.job_id)?;
        let name = format!("cr-{}-{}", hash(&self.options.runner_id), hash(&job));
        let mut jobs = self.jobs.lock().await;
        if jobs.contains(&name) {
            return Ok(name);
        }
        // A same-runner crash remnant is never reused with potentially hostile data.
        match self.volumes().get(&name).await {
            Ok(_) => self.delete_volume(&name).await?,
            Err(e) if absent(&e) => {}
            Err(e) => return Err(map_error(e)),
        }
        let mut body = json!({"apiVersion":"v1","kind":"PersistentVolumeClaim","metadata":{"name":name,"labels":{"cannery.runner":self.options.runner_id,"cannery.job":label(&job)}},"spec":{"accessModes":["ReadWriteOnce"],"resources":{"requests":{"storage":self.options.volume_bytes.to_string()}}}});
        if let Some(class) = &self.options.storage_class {
            body["spec"]["storageClassName"] = json!(class);
        }
        let volume = serde_json::from_value(body).map_err(|_| RuntimeError::Contract)?;
        self.volumes()
            .create(&PostParams::default(), &volume)
            .await
            .map_err(map_error)?;
        jobs.insert(name.clone());
        Ok(name)
    }
    async fn running(&self, name: &str) -> Result<Pod, RuntimeError> {
        tokio::time::timeout(self.options.limits.scheduling, async {
            loop {
                let pod = self.pods().get(name).await.map_err(map_error)?;
                match phase(&pod)? {
                    State::Running | State::Ended(_, _) | State::StartFailure(_) => return Ok(pod),
                    State::Pending => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            }
        })
        .await
        .map_err(|_| RuntimeError::Launch)?
    }
    async fn transfer(
        &self,
        spec: &StepSpec,
        volume: &str,
        input: Option<&Path>,
        output: Option<&Path>,
        directory: &str,
    ) -> Result<(), RuntimeError> {
        let name = format!("cr-transfer-{}", nonce()?);
        let body = self.options.transfer_pod(&name, spec, volume)?;
        let result=async{
            self.pods().create(&PostParams::default(),&body).await.map_err(map_error)?;
            if !matches!(phase(&self.running(&name).await?)?,State::Running){return Err(RuntimeError::Launch);}
            let target=format!("/vol/{directory}");
            let mut command=if input.is_some(){vec!["sh".into(),"-c".into(),"mkdir -p \"$1\" && exec tar -x -f - -C \"$1\"".into(),"sh".into(),target]}else{vec!["sh".into(),"-c".into(),"d=$1; shift; cd \"$d\" || exit 1; chmod -R u+rwX \"$@\" 2>/dev/null; tar -c -f - \"$@\"; s=$?; cd / && rm -rf \"$d\"; exit \"$s\"".into(),"sh".into(),target,"outputs".into()]};
            if input.is_none()&&spec.mounts.iter().any(|m|m.target().text().equals_utf8("/cr/cache")&&!m.read_only()){command.push("cache".into());}
            self.exec(&name,command,input,output).await
        }.await;
        self.delete_pod(&name).await?;
        result
    }
    #[allow(
        clippy::too_many_lines,
        reason = "All three bounded exec channels must drain before status and teardown"
    )]
    async fn exec(
        &self,
        name: &str,
        command: Vec<String>,
        input: Option<&Path>,
        output: Option<&Path>,
    ) -> Result<(), RuntimeError> {
        let params = AttachParams::default()
            .stdin(input.is_some())
            .stdout(true)
            .stderr(true);
        let mut process = self
            .pods()
            .exec(name, command, &params)
            .await
            .map_err(map_error)?;
        let status = process.take_status().ok_or(RuntimeError::Contract)?;
        let stdout = process.stdout().ok_or(RuntimeError::Contract)?;
        let stderr = process.stderr().ok_or(RuntimeError::Contract)?;
        let stdin = process.stdin();
        let operation = async {
            let write = async {
                if let Some(path) = input {
                    let mut file = tokio::fs::File::open(path).await?;
                    let mut stdin = stdin.ok_or(RuntimeError::Contract)?;
                    let mut block = vec![0; 65536];
                    loop {
                        let size = file.read(&mut block).await?;
                        if size == 0 {
                            break;
                        }
                        tokio::time::timeout(
                            self.options.limits.transfer_idle,
                            stdin.write_all(&block[..size]),
                        )
                        .await
                        .map_err(|_| RuntimeError::Launch)??;
                    }
                    stdin.shutdown().await?;
                }
                Ok::<_, RuntimeError>(())
            };
            let read = async {
                let mut stdout = stdout;
                let mut file = match output {
                    Some(path) => Some(
                        tokio::fs::OpenOptions::new()
                            .write(true)
                            .truncate(true)
                            .open(path)
                            .await?,
                    ),
                    None => None,
                };
                let mut received = 0u64;
                let mut block = vec![0; 65536];
                loop {
                    let size = tokio::time::timeout(
                        self.options.limits.transfer_idle,
                        stdout.read(&mut block),
                    )
                    .await
                    .map_err(|_| RuntimeError::Launch)??;
                    if size == 0 {
                        break;
                    }
                    received = received
                        .checked_add(size as u64)
                        .ok_or(RuntimeError::InvalidOutput)?;
                    if received > self.options.limits.output_bytes {
                        return Err(RuntimeError::InvalidOutput);
                    }
                    if let Some(file) = &mut file {
                        file.write_all(&block[..size]).await?;
                    }
                }
                Ok::<_, RuntimeError>(())
            };
            let errors = async {
                let mut stderr = stderr;
                let mut block = [0; 4096];
                while tokio::time::timeout(
                    self.options.limits.transfer_idle,
                    stderr.read(&mut block),
                )
                .await
                .map_err(|_| RuntimeError::Launch)??
                    > 0
                {}
                Ok::<_, RuntimeError>(())
            };
            let (written, received, diagnostics) = tokio::join!(write, read, errors);
            written?;
            received?;
            diagnostics?;
            let status = tokio::time::timeout(self.options.limits.transfer_idle, status)
                .await
                .map_err(|_| RuntimeError::Launch)?
                .ok_or(RuntimeError::Contract)?;
            if status.status.as_deref() != Some("Success") {
                return Err(RuntimeError::InvalidOutput);
            }
            Ok(())
        }
        .await;
        if operation.is_err() {
            process.abort();
        }
        let settlement = process.join().await;
        match operation {
            Err(error) => {
                let _ = settlement;
                Err(error)
            }
            Ok(()) => settlement.map_err(|_| RuntimeError::Transport),
        }
    }
    async fn prepare(
        self: Arc<Self>,
        spec: StepSpec,
        root: PathBuf,
    ) -> Result<Arc<dyn PreparedStep>, RuntimeError> {
        self.options.validate_step(&spec)?;
        let local = root.clone();
        tokio::task::spawn_blocking(move || paths(&local))
            .await
            .map_err(|_| RuntimeError::Filesystem)??;
        let volume = self.volume(&spec).await?;
        let directory = nonce()?;
        let (archive, file) = transfer::spool(&root, "in")?;
        let packed = archive.clone();
        let local = root.clone();
        let mounts = spec.mounts.clone();
        let limits = self.options.limits.clone();
        let result = async {
            tokio::task::spawn_blocking(move || transfer::pack(&local, &mounts, file, &limits))
                .await
                .map_err(|_| RuntimeError::Filesystem)??;
            self.transfer(&spec, &volume, Some(&packed), None, &directory)
                .await
        }
        .await;
        tokio::fs::remove_file(archive).await?;
        result?;
        let runs = Arc::new(Runs::default());
        self.active.register(text(&spec.job_id)?, &runs)?;
        Ok(Arc::new(KubernetesStep {
            inner: self,
            spec,
            root,
            volume,
            directory,
            name: format!("cr-step-{}", nonce()?),
            runs,
            removed: Arc::new(AtomicBool::new(false)),
        }))
    }
}
enum State {
    Pending,
    Running,
    Ended(i32, bool),
    StartFailure(i32),
}
fn phase(pod: &Pod) -> Result<State, RuntimeError> {
    let Some(status) = &pod.status else {
        return Ok(State::Pending);
    };
    if status
        .init_container_statuses
        .as_ref()
        .is_some_and(|containers| {
            containers.iter().any(|container| {
                container.state.as_ref().is_some_and(|state| {
                    state
                        .terminated
                        .as_ref()
                        .is_some_and(|end| end.exit_code != 0)
                })
            })
        })
    {
        return Err(RuntimeError::Launch);
    }
    if status.reason.as_deref().is_some_and(|r| {
        matches!(
            r,
            "Evicted" | "Preempting" | "Shutdown" | "NodeLost" | "UnexpectedAdmissionError"
        )
    }) || status.conditions.as_ref().is_some_and(|c| {
        c.iter()
            .any(|v| v.type_ == "DisruptionTarget" && v.status == "True")
    }) {
        return Err(RuntimeError::Launch);
    }
    if let Some(containers) = &status.container_statuses {
        for container in containers {
            if container.name != "step" && container.name != "transfer" {
                continue;
            }
            if let Some(state) = &container.state {
                if let Some(ended) = &state.terminated {
                    if ended.reason.as_deref() == Some("ContainerStatusUnknown") {
                        return Err(RuntimeError::Launch);
                    }
                    return Ok(State::Ended(
                        ended.exit_code,
                        ended.reason.as_deref() == Some("OOMKilled"),
                    ));
                }
                if let Some(waiting) = &state.waiting
                    && waiting.reason.as_deref().is_some_and(|r| {
                        matches!(
                            r,
                            "CreateContainerError" | "RunContainerError" | "StartError"
                        )
                    })
                {
                    return super::start_failure_code(waiting.message.as_deref().unwrap_or(""))
                        .map(State::StartFailure)
                        .ok_or(RuntimeError::Launch);
                }
                if let Some(waiting) = &state.waiting
                    && waiting.reason.as_deref().is_some_and(|r| {
                        matches!(
                            r,
                            "ErrImagePull"
                                | "ErrImageNeverPull"
                                | "ImagePullBackOff"
                                | "InvalidImageName"
                                | "CreateContainerConfigError"
                        )
                    })
                {
                    return Err(RuntimeError::Launch);
                }
                if state.running.is_some() {
                    return Ok(State::Running);
                }
            }
        }
    }
    match status.phase.as_deref() {
        Some("Running") => Ok(State::Running),
        Some("Failed" | "Succeeded") => Err(RuntimeError::Launch),
        _ => Ok(State::Pending),
    }
}
impl Backend for KubernetesBackend {
    fn start(&self) -> FutureResult<'_, ()> {
        Box::pin(async move {
            let version = self.0.client.apiserver_version().await.map_err(map_error)?;
            let minor = version
                .minor
                .trim_end_matches('+')
                .parse::<u32>()
                .map_err(|_| RuntimeError::Contract)?;
            if version.major != "1" || minor < 30 {
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
            self.0.tasks.spawn(async move {
                let result = inner.prepare(spec, root).await;
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
            self.0.labelled(Some(job)).await?;
            self.0.jobs.lock().await.remove(&format!(
                "cr-{}-{}",
                hash(&self.0.options.runner_id),
                hash(job)
            ));
            Ok(())
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
struct KubernetesStep {
    inner: Arc<Inner>,
    spec: StepSpec,
    root: PathBuf,
    volume: String,
    directory: String,
    name: String,
    runs: Arc<Runs>,
    removed: Arc<AtomicBool>,
}
impl KubernetesStep {
    #[allow(
        clippy::too_many_arguments,
        reason = "Owned step fields cross the supervisor task boundary together"
    )]
    async fn execute(
        inner: Arc<Inner>,
        spec: StepSpec,
        root: PathBuf,
        volume: String,
        directory: String,
        name: String,
        runs: Arc<Runs>,
        removed: Arc<AtomicBool>,
        log: PathBuf,
        seconds: Duration,
        cancel: CancellationEvent,
    ) -> Result<Outcome, RuntimeError> {
        let result = Self::work(
            &inner, &spec, &name, &volume, &directory, &root, &runs, log, seconds, cancel,
        )
        .await;
        inner.delete_pod(&name).await?;
        removed.store(true, Ordering::Release);
        result
    }
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "Owned lifecycle coordinates cancellation, logs and output transfer before return"
    )]
    async fn work(
        inner: &Inner,
        spec: &StepSpec,
        name: &str,
        volume: &str,
        directory: &str,
        root: &Path,
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
        let body = inner
            .options
            .step_pod(name, spec, volume, directory, seconds)?;
        inner
            .pods()
            .create(&PostParams::default(), &body)
            .await
            .map_err(map_error)?;
        let running = inner.running(name);
        tokio::pin!(running);
        let pod = tokio::select! {result=&mut running=>result?,()=cancel.wait()=>{let mut out=Outcome::new(None);out.cancelled=true;return Ok(out);},()=runs.cancel.wait()=>{let mut out=Outcome::new(None);out.cancelled=true;return Ok(out);}};
        if let State::StartFailure(code) = phase(&pod)? {
            tokio::fs::write(log, b"container command cannot be executed\n").await?;
            return Ok(Outcome::new(Some(code.into())));
        }
        let pods = inner.pods();
        let log_name = name.to_owned();
        let log_limit = inner.options.limits.log_bytes;
        let idle = inner.options.limits.transfer_idle;
        let log_stop = CancellationEvent::new();
        let stop = log_stop.clone();
        let (ready, started) = tokio::sync::oneshot::channel();
        let mut logging = tokio::spawn(async move {
            let mut file = tokio::fs::File::create(log).await?;
            let params = LogParams {
                follow: true,
                container: Some("step".into()),
                ..LogParams::default()
            };
            let open = pods.log_stream(&log_name, &params);
            let mut stream = tokio::select! {result=open=>result.map_err(map_error)?,()=stop.wait()=>{file.flush().await?;return Ok(());}};
            let _ = ready.send(());
            let mut remaining = log_limit;
            let mut buffer = vec![0; 65536];
            loop {
                let size = tokio::select! {result=tokio::time::timeout(idle,stream.read(&mut buffer))=>result.map_err(|_|RuntimeError::Launch)??,()=stop.wait()=>break};
                if size == 0 {
                    break;
                }
                let used = size.min(usize::try_from(remaining).unwrap_or(usize::MAX));
                file.write_all(&buffer[..used]).await?;
                remaining -= used as u64;
                if remaining == 0 {
                    break;
                }
            }
            file.flush().await?;
            Ok::<_, RuntimeError>(())
        });
        let wait = async {
            let mut pod = pod;
            loop {
                if let State::Ended(code, oom) = phase(&pod)? {
                    started.await.map_err(|_| RuntimeError::Transport)?;
                    let mut out = Outcome::new(Some(code.into()));
                    out.oom_killed = oom;
                    return Ok::<_, RuntimeError>(out);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                pod = inner.pods().get(name).await.map_err(map_error)?;
            }
        };
        let mut outcome = tokio::select! {result=wait=>result,()=tokio::time::sleep(seconds)=>{let mut out=Outcome::new(None);out.timed_out=true;Ok(out)},()=cancel.wait()=>{let mut out=Outcome::new(None);out.cancelled=true;Ok(out)},()=runs.cancel.wait()=>{let mut out=Outcome::new(None);out.cancelled=true;Ok(out)}};
        let deletion = inner.delete_pod(name).await;
        let log_result = if let Ok(result) =
            tokio::time::timeout(inner.options.limits.cleanup, &mut logging).await
        {
            result
        } else {
            log_stop.set();
            logging.await
        };
        deletion?;
        match log_result {
            Ok(Ok(())) => {}
            Ok(Err(e))
                if outcome
                    .as_ref()
                    .is_ok_and(|out| out.cancelled || out.timed_out) =>
            {
                let _ = e;
            }
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(RuntimeError::Launch),
        }
        if let Ok(value) = &mut outcome
            && !value.cancelled
            && !value.timed_out
        {
            let (archive, file) = transfer::spool(root, "out")?;
            drop(file);
            let copied = inner
                .transfer(spec, volume, None, Some(&archive), directory)
                .await;
            let result = match copied {
                Ok(()) => {
                    let path = archive.clone();
                    let root = root.to_path_buf();
                    let mounts = spec.mounts.clone();
                    let limits = inner.options.limits.clone();
                    tokio::task::spawn_blocking(move || {
                        transfer::unpack(&path, &root, &mounts, &limits)
                    })
                    .await
                    .unwrap_or(Err(RuntimeError::Filesystem))
                }
                Err(e) => Err(e),
            };
            tokio::fs::remove_file(archive).await?;
            match result {
                Ok(()) => {}
                Err(RuntimeError::InvalidOutput) => {
                    value.output_error = Some(String::from(
                        "step outputs exceed transfer limits or contain invalid entries",
                    ));
                }
                Err(e) => return Err(e),
            }
        }
        outcome
    }
}
impl PreparedStep for KubernetesStep {
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
                self.spec.clone(),
                self.root.clone(),
                self.volume.clone(),
                self.directory.clone(),
                self.name.clone(),
                self.runs.clone(),
                self.removed.clone(),
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
                self.inner.delete_pod(&self.name).await?;
                self.removed.store(true, Ordering::Release);
            }
            Ok(())
        })
    }
}
impl Drop for KubernetesStep {
    fn drop(&mut self) {
        self.runs.cancel.set();
        if !self.removed.load(Ordering::Acquire) {
            let inner = self.inner.clone();
            let name = self.name.clone();
            let runs = self.runs.clone();
            let _ = self.inner.tasks.spawn(async move {
                let _ = runs.settle().await;
                let _ = inner.delete_pod(&name).await;
            });
        }
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;

#[cfg(test)]
#[allow(clippy::expect_used, reason = "Typed Pod phase fixtures")]
mod tests {
    use super::*;
    fn pod(state: Value) -> Pod {
        let mut value = json!({"metadata":{"name":"fixture"},"status":{"phase":"Pending","containerStatuses":[{"name":"step","ready":false,"restartCount":0,"image":"fixture","imageID":"fixture","state":{}}]}});
        value["status"]["containerStatuses"][0]["state"] = state;
        serde_json::from_value(value).expect("actual Kubernetes status shape")
    }
    #[test]
    fn unavailable_image_and_runtime_devices_are_runner_errors() {
        for reason in [
            "ErrImagePull",
            "ImagePullBackOff",
            "ErrImageNeverPull",
            "InvalidImageName",
            "CreateContainerConfigError",
        ] {
            assert!(matches!(
                phase(&pod(json!({"waiting":{"reason":reason}}))),
                Err(RuntimeError::Launch)
            ));
        }
        assert!(matches!(
            phase(&pod(
                json!({"waiting":{"reason":"StartError","message":"nvidia device missing"}})
            )),
            Err(RuntimeError::Launch)
        ));
    }
    #[test]
    fn missing_commands_are_step_failures_and_failed_gate_never_runs() {
        assert!(matches!(
            phase(&pod(
                json!({"waiting":{"reason":"CreateContainerError","message":"executable file not found"}})
            )),
            Ok(State::StartFailure(127))
        ));
        assert!(matches!(
            phase(&pod(
                json!({"waiting":{"reason":"RunContainerError","message":"permission denied"}})
            )),
            Ok(State::StartFailure(126))
        ));
        let mut value = pod(json!({"running":{}}));
        value.status.as_mut().expect("status").init_container_statuses=serde_json::from_value(json!([{"name":"network-gate","ready":false,"restartCount":0,"image":"fixture","imageID":"fixture","state":{"terminated":{"exitCode":1}}}])).expect("gate");
        assert!(matches!(phase(&value), Err(RuntimeError::Launch)));
    }
}
