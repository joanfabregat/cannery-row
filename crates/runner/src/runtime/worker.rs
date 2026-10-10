//! Step sessions: pinned inputs, sequential steps, integrity checks and uploads.
use super::{
    FutureResult, RuntimeError,
    backend::Backend,
    http::{self, ApiClient, LeaseHeaders},
};
use crate::{
    cancellation::CancellationEvent,
    launcher::{self, Manifest, Network, StepSpec},
};
use cannery_core::{
    front_matter,
    json::{self},
    principal::Secret,
};
use reqwest::Method;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::io::AsyncReadExt;

/// The experiment output the last step writes: the run document `run.md`.
const RUN_OUTPUT: &str = "run";
/// The scorer output a verification report is composed from.
const EVIDENCE_OUTPUT: &str = "evidence";
/// The policy step's only output.
const VERDICT_OUTPUT: &str = "verdict";
/// The decider step's only output: the decision document `decision.md`.
const DECISION_OUTPUT: &str = "decision";
/// The built-in interface of the run document.
pub(crate) const RUN_INTERFACE: &str = "cr-run/v0.2";

pub trait OutputValidator: Send + Sync {
    /// Validation runs before uploading any output of this step. A checker must
    /// not quote private file contents in diagnostics or fail open on errors.
    fn check<'a>(
        &'a self,
        interface: Option<&'a str>,
        science: &'a Value,
        file: &'a Path,
    ) -> FutureResult<'a, ()>;
}
pub trait Provisioner: Send + Sync {
    fn prepare<'a>(
        &'a self,
        manifest: &'a Value,
        step: StepSpec,
        root: &'a Path,
        cancel: CancellationEvent,
        deadline: SystemTime,
    ) -> FutureResult<'a, StepSpec>;
    fn release<'a>(&'a self, step: &'a StepSpec) -> FutureResult<'a, ()>;
    fn close(&self) -> FutureResult<'_, ()>;
    fn take_setup<'a>(&'a self, step: &'a StepSpec) -> FutureResult<'a, Option<SetupReport>>;
}
pub struct SetupReport {
    pub log: PathBuf,
    pub label: String,
    pub outcome: launcher::Outcome,
}
/// Reject unsupported code-bearing manifests rather than silently executing
/// without the declared code/setup. Replaced by the real shared code store.
pub struct NoCode;
impl Provisioner for NoCode {
    fn prepare<'a>(
        &'a self,
        manifest: &'a Value,
        step: StepSpec,
        _root: &'a Path,
        _cancel: CancellationEvent,
        _deadline: SystemTime,
    ) -> FutureResult<'a, StepSpec> {
        Box::pin(async move {
            if manifest["spec"]
                .get("code")
                .is_some_and(|code| !code.is_null())
            {
                Err(RuntimeError::Configuration)
            } else {
                Ok(step)
            }
        })
    }
    fn release<'a>(&'a self, _step: &'a StepSpec) -> FutureResult<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn close(&self) -> FutureResult<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn take_setup<'a>(&'a self, _step: &'a StepSpec) -> FutureResult<'a, Option<SetupReport>> {
        Box::pin(async { Ok(None) })
    }
}
/// The resources a step session borrows: API client and credential, roots,
/// launch backend, code provisioner and output validator.
pub struct Resources {
    pub client: ApiClient,
    pub token: Secret,
    pub project: String,
    pub data_root: PathBuf,
    pub work_root: PathBuf,
    pub backend: Arc<dyn Backend>,
    pub provisioner: Arc<dyn Provisioner>,
    pub validator: Arc<dyn OutputValidator>,
}
#[derive(Debug)]
pub struct JobResult {
    pub job_id: String,
    pub state: String,
}
pub(crate) struct DoneGuard(pub(crate) CancellationEvent);
impl Drop for DoneGuard {
    fn drop(&mut self) {
        self.0.set();
    }
}
pub(crate) async fn outcome_state(
    response: reqwest::Response,
    durable: bool,
) -> Result<String, RuntimeError> {
    let status = response.status().as_u16();
    let body = http::read_json(response).await?;
    if status == 409 && body["error"]["code"] == "stale_lease" {
        return Ok("abandoned".to_owned());
    }
    if status == 422 && durable {
        return Ok("failed".to_owned());
    }
    if !(200..300).contains(&status) {
        return Err(RuntimeError::Http(status));
    }
    Ok(string(&body, "state")?.to_owned())
}
#[derive(Clone, Copy, Eq, PartialEq)]
enum SessionMode {
    /// A verify job's producer and scorer.
    Verify,
    /// A verify job's policy step, after its producer and scorer.
    Policy,
    Experiment,
    /// A decide job's decider step.
    Decide,
}
pub(crate) struct Session<'a> {
    mode: SessionMode,
    worker: &'a Resources,
    claim: &'a Value,
    job: &'a Value,
    api: &'a str,
    base: &'a str,
    lease: &'a LeaseHeaders,
    work: &'a Path,
    cancel: CancellationEvent,
    science: Option<Value>,
    metrics: Option<Value>,
    objects: Vec<Value>,
    resumed: Vec<Value>,
    produced: BTreeMap<String, Produced>,
    evidence_step: Option<String>,
    failure_step: Option<String>,
    failure_logs: Vec<Value>,
}
struct Produced {
    path: PathBuf,
    output_root: PathBuf,
    digests: BTreeMap<String, (u64, String)>,
}
impl Session<'_> {
    #[allow(clippy::too_many_arguments)] // Borrow the single session's explicit resources and lease.
    fn new<'a>(
        mode: SessionMode,
        worker: &'a Resources,
        claim: &'a Value,
        job: &'a Value,
        api: &'a str,
        base: &'a str,
        lease: &'a LeaseHeaders,
        work: &'a Path,
        cancel: CancellationEvent,
    ) -> Session<'a> {
        Session {
            mode,
            worker,
            claim,
            job,
            api,
            base,
            lease,
            work,
            cancel,
            science: None,
            metrics: None,
            objects: vec![],
            resumed: vec![],
            produced: BTreeMap::new(),
            evidence_step: None,
            failure_step: None,
            failure_logs: vec![],
        }
    }
    #[allow(clippy::too_many_arguments)] // Borrow the single session's explicit resources and lease.
    pub(crate) fn experiment<'a>(
        worker: &'a Resources,
        claim: &'a Value,
        job: &'a Value,
        api: &'a str,
        base: &'a str,
        lease: &'a LeaseHeaders,
        work: &'a Path,
        cancel: CancellationEvent,
    ) -> Session<'a> {
        Session::new(
            SessionMode::Experiment,
            worker,
            claim,
            job,
            api,
            base,
            lease,
            work,
            cancel,
        )
    }
    #[allow(clippy::too_many_arguments)] // Borrow the single session's explicit resources and lease.
    pub(crate) fn verify<'a>(
        worker: &'a Resources,
        claim: &'a Value,
        job: &'a Value,
        api: &'a str,
        base: &'a str,
        lease: &'a LeaseHeaders,
        work: &'a Path,
        cancel: CancellationEvent,
    ) -> Session<'a> {
        Session::new(
            SessionMode::Verify,
            worker,
            claim,
            job,
            api,
            base,
            lease,
            work,
            cancel,
        )
    }
    #[allow(clippy::too_many_arguments)] // Borrow the single session's explicit resources and lease.
    pub(crate) fn decide<'a>(
        worker: &'a Resources,
        claim: &'a Value,
        job: &'a Value,
        api: &'a str,
        base: &'a str,
        lease: &'a LeaseHeaders,
        work: &'a Path,
        cancel: CancellationEvent,
    ) -> Session<'a> {
        Session::new(
            SessionMode::Decide,
            worker,
            claim,
            job,
            api,
            base,
            lease,
            work,
            cancel,
        )
    }
    pub(crate) fn objects(&self) -> &[Value] {
        &self.objects
    }
    /// The verify job's output manifest objects: what this job uploaded, then
    /// the verified outputs it resumed from an earlier run of the job, stored
    /// where this job's own uploads are.
    pub(crate) fn manifest_objects(&self) -> Result<Vec<Value>, RuntimeError> {
        let mut objects = self.objects.clone();
        if self.resumed.is_empty() {
            return Ok(objects);
        }
        let mut storage = objects
            .first()
            .map(|object| object["storage"].clone())
            .filter(Value::is_object)
            .ok_or(RuntimeError::Contract)?;
        if let Some(fields) = storage.as_object_mut() {
            fields.remove("generation");
        }
        for output in &self.resumed {
            let mut stored = storage.clone();
            stored["key"] = output["key"].clone();
            objects.push(json!({"role":output["name"],"storage":stored,"size_bytes":output["size_bytes"],"sha256":output["sha256"],"media_type":output["media_type"]}));
        }
        Ok(objects)
    }
    /// The step whose output a report or run document failed on.
    pub(crate) fn blame(&mut self, step: Option<String>) {
        self.failure_step = step;
    }
    /// The scorer, whose `evidence` output the report is composed from.
    pub(crate) fn evidence_step(&self) -> Option<String> {
        self.evidence_step.clone()
    }
    pub(crate) async fn upload_policy(
        &mut self,
        manifest: &Value,
        name: &str,
    ) -> Result<(), RuntimeError> {
        let file = self.work.join("policy-step.json");
        tokio::fs::write(&file, serde_json::to_vec_pretty(manifest)?).await?;
        self.upload(
            &file,
            "policy_step",
            &format!("{name}/policy_step/{name}.json"),
            None,
        )
        .await?;
        Ok(())
    }
    fn metrics(&self) -> Result<Option<json::Document>, RuntimeError> {
        self.metrics.as_ref().map(document).transpose()
    }
    pub(crate) fn failure(&self) -> (Option<&str>, &[Value]) {
        (self.failure_step.as_deref(), &self.failure_logs)
    }
    fn job_kind(&self) -> crate::job::JobKind {
        match self.mode {
            SessionMode::Verify | SessionMode::Policy => crate::job::JobKind::Verify,
            SessionMode::Experiment => crate::job::JobKind::Experiment,
            SessionMode::Decide => crate::job::JobKind::Decide,
        }
    }
    async fn get_json(
        &self,
        path: &str,
        lease: Option<&LeaseHeaders>,
    ) -> Result<Value, RuntimeError> {
        let operation = async {
            http::json_response(
                self.worker
                    .client
                    .request(Method::GET, path, &self.worker.token, lease, None)
                    .await?,
            )
            .await
        };
        tokio::select! {
            ()=self.cancel.wait()=>Err(RuntimeError::LostLease),
            result=operation=>result,
        }
    }
    async fn provision(
        &mut self,
        manifest: &Value,
        launch: StepSpec,
        root: &Path,
        owner: &str,
    ) -> Result<StepSpec, RuntimeError> {
        let deadline = instant(string(self.job, "deadline")?)?
            .checked_sub(Duration::from_secs(30))
            .ok_or(RuntimeError::DeadlineExceeded)?;
        let result = self
            .worker
            .provisioner
            .prepare(
                manifest,
                launch.clone(),
                root,
                self.cancel.clone(),
                deadline,
            )
            .await;
        let checked = async {
            if let Some(report) = self.worker.provisioner.take_setup(&launch).await? {
                if report.outcome.cancelled {
                    return Err(RuntimeError::LostLease);
                }
                let artifact = self
                    .upload(
                        &report.log,
                        "setup_log",
                        &format!("{owner}/setup_log/{}.log", report.label),
                        None,
                    )
                    .await?;
                self.failure_logs.push(log_reference(&artifact)?);
            }
            result
        }
        .await;
        if checked.is_err() {
            self.worker.provisioner.release(&launch).await?;
        }
        checked
    }
    #[allow(clippy::too_many_lines)] // One isolated validator process and log lifecycle.
    async fn run_validator(
        &mut self,
        index: usize,
        owner: &Value,
        interface: &str,
        science: &Value,
        local: &Path,
    ) -> Result<(), RuntimeError> {
        // The built-in evidence envelope and run document are checked by the
        // API on completion or submission, rather than resolved as
        // project-registered content interfaces.
        if interface == "cr-evidence/v0.2" || interface == RUN_INTERFACE {
            return Ok(());
        }
        let (name, version) = interface.rsplit_once("/v").ok_or(RuntimeError::Contract)?;
        let definition = array(science, "interfaces")?
            .iter()
            .find(|value| {
                value["name"] == name && value["version"].as_u64() == version.parse().ok()
            })
            .ok_or(RuntimeError::Contract)?;
        let Some(validator) = definition.get("validator").and_then(Value::as_str) else {
            return Ok(());
        };
        if !slug(validator) {
            return Err(RuntimeError::Contract);
        }
        let manifest = array(science, "validators")?
            .iter()
            .find(|value| value["metadata"]["name"] == validator)
            .ok_or(RuntimeError::Contract)?;
        let inputs = array(&manifest["spec"]["inputs"], "artifacts")?;
        if inputs.len() != 1 {
            return Err(RuntimeError::Contract);
        }
        let label = format!("{index}-{}.{}.{}", string(owner, "name")?, name, validator);
        let root = self.work.join(&label);
        let target = confined(&root, string(&inputs[0], "path")?)?;
        tokio::fs::create_dir_all(&target).await?;
        copy_input(local.to_owned(), target.clone()).await?;
        make_read_only(target).await?;
        let step = json!({"name":validator,"revision":0,"manifest":manifest});
        let claim_doc = document(self.claim)?;
        let step_doc = document(&step)?;
        let metrics = self.metrics()?;
        crate::job::write_contract(
            &crate::job::JobContext {
                kind: self.job_kind(),
                claim: &claim_doc,
                step: &step_doc,
                metrics: metrics.as_ref(),
                nesting_budget: 256,
            },
            &root,
            &[],
        )
        .map_err(|_| RuntimeError::Contract)?;
        let launch = manifest_step(manifest, string(self.job, "job_id")?, &label)?;
        let launch = self
            .provision(manifest, launch, &root, string(owner, "name")?)
            .await?;
        let seconds = positive(&manifest["spec"]["activeDeadlineSeconds"])?
            .min(time_left(instant(string(self.job, "deadline")?)?) - 30.0);
        let prepared = match self
            .worker
            .backend
            .prepare(launch.clone(), root.clone())
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                self.worker.provisioner.release(&launch).await?;
                return Err(error);
            }
        };
        let log = self.work.join(format!("{label}.log"));
        let result = if seconds <= 0.0 {
            Err(RuntimeError::DeadlineExceeded)
        } else {
            prepared
                .run(log.clone(), seconds, self.cancel.clone())
                .await
        };
        let cleanup = prepared.cleanup().await;
        let release = self.worker.provisioner.release(&launch).await;
        cleanup?;
        release?;
        let outcome = result?;
        if outcome.cancelled {
            return Err(RuntimeError::LostLease);
        }
        let artifact = self
            .upload(
                &log,
                "validator_log",
                &format!(
                    "{}/validator_log/{name}.{validator}.log",
                    string(owner, "name")?
                ),
                None,
            )
            .await?;
        self.failure_logs.push(log_reference(&artifact)?);
        if outcome.timed_out {
            return Err(RuntimeError::DeadlineExceeded);
        }
        if outcome.exit_code == Some(1.into()) && !outcome.oom_killed {
            return Err(RuntimeError::InvalidStepOutput);
        }
        if outcome.exit_code != Some(0.into()) || outcome.oom_killed {
            return Err(RuntimeError::StepFailed);
        }
        Ok(())
    }
    /// The pinned science revision's content, read once per session.
    pub(crate) async fn science(&mut self) -> Result<Value, RuntimeError> {
        if let Some(science) = &self.science {
            return Ok(science.clone());
        }
        let revision = science_revision(&self.job["science_revision"])?;
        let science = self
            .get_json(&format!("{}/config/science/{revision}", self.api), None)
            .await?;
        if science["revision"]
            .as_u64()
            .is_some_and(|served| served != revision)
        {
            return Err(RuntimeError::Integrity);
        }
        let content = science
            .get("content")
            .cloned()
            .ok_or(RuntimeError::Contract)?;
        self.science = Some(content.clone());
        Ok(content)
    }
    /// Run the job's steps in order. A verify job resumed after an
    /// infrastructure failure stages the verified outputs of the steps before
    /// `resume.from_step` instead of running them again.
    /// Returns the scorer's evidence (verify) or the run document (experiment).
    pub(crate) async fn run_steps(&mut self) -> Result<Value, RuntimeError> {
        let steps = array(self.job, "steps")?.clone();
        self.run_list(&steps, 0).await
    }
    /// Run the policy step after the producer and the scorer, with their
    /// outputs and the science revision's metric registry. Returns its verdict.
    pub(crate) async fn run_policy(&mut self, step: &Value) -> Result<Value, RuntimeError> {
        let science = self.science().await?;
        self.metrics = Some(science.get("metrics").cloned().unwrap_or_else(|| json!([])));
        self.mode = SessionMode::Policy;
        let offset = array(self.job, "steps")?.len();
        self.run_list(std::slice::from_ref(step), offset).await
    }
    /// Run a decide job's decider step on the decider's context bundle.
    /// Returns its decision document's front matter and body.
    pub(crate) async fn run_decider(&mut self, step: &Value) -> Result<Value, RuntimeError> {
        self.run_list(std::slice::from_ref(step), 0).await
    }
    /// Stage the decider's context bundle the claim names at
    /// `/cr/context/context.md`.
    async fn stage_context(&self, root: &Path) -> Result<(), RuntimeError> {
        let reference = string(&self.claim["context"], "ref")?;
        let operation = async {
            http::read_text(
                self.worker
                    .client
                    .request(Method::GET, reference, &self.worker.token, None, None)
                    .await?,
            )
            .await
        };
        let bundle = tokio::select! {
            ()=self.cancel.wait()=>return Err(RuntimeError::LostLease),
            result=operation=>result?,
        };
        let directory = root.join("context");
        tokio::fs::create_dir(&directory).await?;
        tokio::fs::write(directory.join("context.md"), bundle).await?;
        Ok(())
    }
    fn resume_position(&self, steps: &[Value]) -> Result<usize, RuntimeError> {
        let resume = &self.job["resume"];
        if self.mode != SessionMode::Verify || resume.is_null() {
            return Ok(0);
        }
        let from = string(resume, "from_step")?;
        Ok(steps
            .iter()
            .position(|step| step["name"] == from)
            .unwrap_or(steps.len()))
    }
    #[allow(clippy::too_many_lines)] // Preserve sequential validate-all-before-upload order.
    async fn run_list(&mut self, steps: &[Value], offset: usize) -> Result<Value, RuntimeError> {
        let science = self.science().await?;
        let science = &science;
        let deadline = instant(string(self.job, "deadline")?)?;
        let resume_from = self.resume_position(steps)?;
        let mut evidence = None;
        for (position, step) in steps.iter().enumerate() {
            let index = offset + position;
            if self.cancel.is_set() {
                return Err(RuntimeError::LostLease);
            }
            let name = string(step, "name")?;
            self.failure_step = Some(name.to_owned());
            self.failure_logs.clear();
            if !slug(name) {
                return Err(RuntimeError::Contract);
            }
            let manifest = &step["manifest"];
            let spec = &manifest["spec"];
            let role = string(spec, "role")?;
            if match self.mode {
                SessionMode::Verify => !["producer", "scorer"].contains(&role),
                SessionMode::Policy => role != "policy",
                SessionMode::Experiment => role != "experiment",
                SessionMode::Decide => role != "decider",
            } {
                return Err(RuntimeError::Contract);
            }
            if position < resume_from {
                if let Some(path) = self.resume_step(index, step).await? {
                    evidence = Some(path);
                }
                continue;
            }
            let root = self.work.join(format!("{index}-{name}"));
            tokio::fs::create_dir(&root).await?;
            self.stage_inputs(&root, spec, science).await?;
            if self.mode == SessionMode::Decide {
                self.stage_context(&root).await?;
            }
            let outputs = array(&spec["outputs"], "artifacts")?;
            let directories = outputs
                .iter()
                .map(|output| {
                    let path = confined(&root, string(output, "path")?)?;
                    Ok(
                        if Path::new(string(output, "path")?).components().count() == 4 {
                            path
                        } else {
                            path.parent().ok_or(RuntimeError::Contract)?.to_owned()
                        },
                    )
                })
                .collect::<Result<Vec<_>, RuntimeError>>()?;
            let claim_doc = document(self.claim)?;
            let step_doc = document(step)?;
            let metrics = self.metrics()?;
            let contract = crate::job::JobContext {
                kind: self.job_kind(),
                claim: &claim_doc,
                step: &step_doc,
                metrics: metrics.as_ref(),
                nesting_budget: 256,
            };
            crate::job::write_contract(&contract, &root, &directories)
                .map_err(|_| RuntimeError::Contract)?;
            let launch = manifest_step(
                manifest,
                string(self.job, "job_id")?,
                &format!("{index}-{name}"),
            )?;
            let step_limit = positive(&spec["activeDeadlineSeconds"])?;
            let launch = self.provision(manifest, launch, &root, name).await?;
            let prepared = match self
                .worker
                .backend
                .prepare(launch.clone(), root.clone())
                .await
            {
                Ok(prepared) => prepared,
                Err(error) => {
                    self.worker.provisioner.release(&launch).await?;
                    return Err(error);
                }
            };
            let log = self.work.join(format!("{index}-{name}.log"));
            let seconds = step_limit.min(time_left(deadline) - 30.0);
            let run = if seconds <= 0.0 {
                Err(RuntimeError::DeadlineExceeded)
            } else {
                prepared
                    .run(log.clone(), seconds, self.cancel.clone())
                    .await
            };
            let cleanup = prepared.cleanup().await;
            let release = self.worker.provisioner.release(&launch).await;
            cleanup?;
            release?;
            let outcome = run?;
            if outcome.cancelled {
                return Err(RuntimeError::LostLease);
            }
            let log_artifact = self
                .upload(
                    &log,
                    "step_log",
                    &format!("{name}/step_log/{name}.log"),
                    None,
                )
                .await?;
            self.failure_logs.push(log_reference(&log_artifact)?);
            self.raise_concern(&root).await;
            if outcome.timed_out {
                return Err(RuntimeError::DeadlineExceeded);
            }
            if outcome.oom_killed
                || outcome
                    .exit_code
                    .as_ref()
                    .is_none_or(|code| code != &0.into())
            {
                return Err(RuntimeError::StepFailed);
            }
            // Validate the entire output set before uploading its files.
            let mut checked = vec![];
            if outcome.output_error.is_some() {
                return Err(RuntimeError::InvalidOutput);
            }
            for output in outputs {
                let local = confined(&root, string(output, "path")?)?;
                ensure_unlinked(&root.join("outputs"), &local)
                    .await
                    .map_err(|_| RuntimeError::InvalidOutput)?;
                let files = inspect_output(&local).await?;
                if files.is_empty() {
                    return Err(RuntimeError::MissingOutput);
                }
                let interface = output.get("interface").and_then(Value::as_str);
                for (path, relative) in &files {
                    let checked = self.worker.validator.check(interface, science, path).await;
                    if let Err(error) = checked {
                        // A refused producer upload is the API's authoritative
                        // evidence for blaming the agent rather than the verifier.
                        if role == "producer" && error == RuntimeError::InvalidStepOutput {
                            let output_name = string(output, "name")?;
                            let _ = self
                                .upload(
                                    path,
                                    output_name,
                                    &format!("{name}/{output_name}/{relative}"),
                                    interface,
                                )
                                .await;
                        }
                        return Err(error);
                    }
                }
                if let Some(interface) = interface {
                    self.run_validator(index, step, interface, science, &local)
                        .await?;
                }
                if self.mode == SessionMode::Policy
                    && output["name"] == "verdict"
                    && (files.len() != 1
                        || !files[0]
                            .0
                            .extension()
                            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
                        || tokio::fs::metadata(&files[0].0).await?.len() > 1024 * 1024)
                {
                    return Err(RuntimeError::InvalidStepOutput);
                }
                checked.push((output, local, files));
            }
            for (output, local, files) in checked {
                let output_name = string(output, "name")?;
                let mut digests = BTreeMap::new();
                for (path, relative) in files {
                    // The run and the decision are submitted as documents, not uploaded.
                    if (self.mode == SessionMode::Experiment && output_name == RUN_OUTPUT)
                        || (self.mode == SessionMode::Decide && output_name == DECISION_OUTPUT)
                    {
                        digests.insert(relative, http::digest(&path).await?);
                        continue;
                    }
                    let artifact = self
                        .upload(
                            &path,
                            output_name,
                            &format!("{name}/{output_name}/{relative}"),
                            output.get("interface").and_then(Value::as_str),
                        )
                        .await?;
                    digests.insert(
                        relative,
                        (
                            artifact["size_bytes"]
                                .as_u64()
                                .ok_or(RuntimeError::Contract)?,
                            string(&artifact, "sha256")?.to_owned(),
                        ),
                    );
                }
                if (self.mode == SessionMode::Verify
                    && role == "scorer"
                    && output_name == EVIDENCE_OUTPUT)
                    || (self.mode == SessionMode::Experiment && output_name == RUN_OUTPUT)
                    || (self.mode == SessionMode::Policy && output_name == VERDICT_OUTPUT)
                    || (self.mode == SessionMode::Decide && output_name == DECISION_OUTPUT)
                {
                    if self.mode == SessionMode::Verify {
                        self.evidence_step = Some(name.to_owned());
                    }
                    evidence = Some(local.clone());
                }
                self.produced.insert(
                    output_name.to_owned(),
                    Produced {
                        path: local,
                        output_root: root.join("outputs"),
                        digests,
                    },
                );
            }
        }
        self.finish(evidence).await
    }
    /// The track, unit number and attempt sequence of the session's attempt.
    fn concern_origin(&self) -> Option<(String, u64, u64)> {
        let track = self.job["track"].as_str()?.to_owned();
        if self.mode == SessionMode::Experiment {
            let attempt = &self.claim["attempt"];
            return Some((
                track,
                attempt["number"].as_u64()?,
                attempt["sequence"].as_u64()?,
            ));
        }
        // A job claim names its attempt as `#<number>.<sequence>`.
        let (number, sequence) = self.claim["attempt_ref"]
            .as_str()?
            .strip_prefix('#')?
            .split_once('.')?;
        Some((track, number.parse().ok()?, sequence.parse().ok()?))
    }
    /// Submit the concern about the track's plan a step wrote to
    /// `/cr/outputs/concern/concern.md`, naming the session's unit and
    /// attempt when it names none. A concern is advice to the researchers, so
    /// it never changes the step's result: one that cannot be read or is
    /// refused is dropped.
    async fn raise_concern(&self, root: &Path) {
        let outputs = root.join("outputs");
        let path = outputs.join("concern").join("concern.md");
        let Some((track, number, sequence)) = self.concern_origin() else {
            return;
        };
        if ensure_unlinked(&outputs, &path).await.is_err() {
            return;
        }
        let Ok(metadata) = tokio::fs::symlink_metadata(&path).await else {
            return;
        };
        if !metadata.is_file() || metadata.len() > 1024 * 1024 {
            return;
        }
        let Ok(text) = tokio::fs::read_to_string(&path).await else {
            return;
        };
        let document = match front_matter::parse(&text, front_matter::Limits::default()) {
            Ok(parsed) => {
                let mut fields = parsed.front_matter;
                if !fields.contains_key("unit") {
                    fields.insert("unit".into(), json!(number));
                    fields.entry("attempt").or_insert_with(|| json!(sequence));
                }
                format!("---\n{}\n---\n{}", Value::Object(fields), parsed.body)
            }
            // The API refuses it with the reason; the step's log is where to look.
            Err(_) => text,
        };
        let _ = self
            .worker
            .client
            .request(
                Method::POST,
                &format!("{}/tracks/{track}/concerns", self.api),
                &self.worker.token,
                None,
                Some(&json!({"document": document})),
            )
            .await;
    }
    /// Read the single file of the session's result output, checked against
    /// the bytes the API accepted.
    async fn finish(&mut self, evidence: Option<PathBuf>) -> Result<Value, RuntimeError> {
        // A run document or a decision document: Markdown with front matter.
        let experiment = matches!(self.mode, SessionMode::Experiment | SessionMode::Decide);
        if self.mode == SessionMode::Verify {
            self.failure_step.clone_from(&self.evidence_step);
        }
        let evidence = evidence.ok_or(if experiment {
            RuntimeError::MissingOutput
        } else {
            RuntimeError::Contract
        })?;
        ensure_unlinked(self.work, &evidence).await?;
        let files = inspect_output(&evidence).await?;
        if files.len() != 1
            || !files[0].0.extension().is_some_and(|extension| {
                extension.eq_ignore_ascii_case(if experiment { "md" } else { "json" })
            })
        {
            return Err(RuntimeError::InvalidStepOutput);
        }
        let produced = self
            .produced
            .get(match self.mode {
                SessionMode::Verify => EVIDENCE_OUTPUT,
                SessionMode::Policy => VERDICT_OUTPUT,
                SessionMode::Experiment => RUN_OUTPUT,
                SessionMode::Decide => DECISION_OUTPUT,
            })
            .ok_or(RuntimeError::Contract)?;
        for (path, relative) in &files {
            if produced.digests.get(relative) != Some(&http::digest(path).await?) {
                return Err(RuntimeError::Integrity);
            }
        }
        let file = tokio::fs::File::open(&files[0].0).await?;
        let mut bytes = Vec::new();
        let limit = if self.mode == SessionMode::Verify {
            16 * 1024 * 1024
        } else {
            1024 * 1024
        };
        file.take(limit + 1).read_to_end(&mut bytes).await?;
        if u64::try_from(bytes.len()).map_err(|_| RuntimeError::InvalidStepOutput)? > limit {
            return Err(RuntimeError::InvalidStepOutput);
        }
        if experiment {
            // The run document: front matter and notes, submitted by the session.
            let text = String::from_utf8(bytes).map_err(|_| RuntimeError::InvalidStepOutput)?;
            let document = front_matter::parse(&text, front_matter::Limits::default())
                .map_err(|_| RuntimeError::InvalidStepOutput)?;
            return Ok(json!({
                "front_matter": Value::Object(document.front_matter),
                "body": document.body,
            }));
        }
        let invalid = RuntimeError::InvalidStepOutput;
        json::decode(&bytes, 128).map_err(|_| invalid)?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid)?;
        if !value.is_object() {
            return Err(invalid);
        }
        Ok(value)
    }
    /// Stage the outputs an earlier run of this verify job verified for a step
    /// before `resume.from_step`, downloaded under this job's lease and
    /// checked against their recorded size and SHA-256. Returns the scorer's
    /// evidence path.
    async fn resume_step(
        &mut self,
        index: usize,
        step: &Value,
    ) -> Result<Option<PathBuf>, RuntimeError> {
        let name = string(step, "name")?;
        let spec = &step["manifest"]["spec"];
        let role = string(spec, "role")?;
        let root = self.work.join(format!("{index}-{name}"));
        tokio::fs::create_dir(&root).await?;
        let resumed = array(&self.job["resume"], "outputs")?.clone();
        let mut evidence = None;
        for output in array(&spec["outputs"], "artifacts")? {
            let output_name = string(output, "name")?;
            let declared = string(output, "path")?;
            let local = confined(&root, declared)?;
            let directory = Path::new(declared).components().count() == 4;
            let objects: Vec<&Value> = resumed
                .iter()
                .filter(|object| object["step"] == name && object["name"] == output_name)
                .collect();
            if objects.is_empty() || (!directory && objects.len() != 1) {
                return Err(RuntimeError::Contract);
            }
            let marker = format!("/{name}/{output_name}/");
            let mut digests = BTreeMap::new();
            for object in objects {
                let key = string(object, "key")?;
                let relative = key
                    .find(&marker)
                    .map(|at| &key[at + marker.len()..])
                    .filter(|relative| {
                        !relative
                            .split('/')
                            .any(|part| part.is_empty() || part == "." || part == "..")
                    })
                    .ok_or(RuntimeError::Contract)?;
                let destination = if directory {
                    local.join(relative)
                } else if local.file_name().and_then(|file| file.to_str()) == Some(relative) {
                    local.clone()
                } else {
                    return Err(RuntimeError::Contract);
                };
                if let Some(parent) = destination.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                let size = object["size_bytes"]
                    .as_u64()
                    .ok_or(RuntimeError::Contract)?;
                let hash = string(object, "sha256")?;
                self.worker
                    .client
                    .download(
                        &format!("{}/inputs/object", self.base),
                        key,
                        &self.worker.token,
                        self.lease,
                        &destination,
                        size,
                        &self.cancel,
                    )
                    .await?;
                if http::digest(&destination).await? != (size, hash.to_owned()) {
                    return Err(RuntimeError::Integrity);
                }
                if self.cancel.is_set() {
                    return Err(RuntimeError::LostLease);
                }
                digests.insert(relative.to_owned(), (size, hash.to_owned()));
                self.resumed.push(object.clone());
            }
            if role == "scorer" && output_name == EVIDENCE_OUTPUT {
                self.evidence_step = Some(name.to_owned());
                evidence = Some(local.clone());
            }
            self.produced.insert(
                output_name.to_owned(),
                Produced {
                    path: local,
                    output_root: root.join("outputs"),
                    digests,
                },
            );
        }
        Ok(evidence)
    }
    #[allow(clippy::too_many_lines)] // The four input sources share one ordered contract.
    async fn stage_inputs(
        &self,
        root: &Path,
        spec: &Value,
        science: &Value,
    ) -> Result<(), RuntimeError> {
        for input in array(&spec["inputs"], "artifacts")? {
            let name = string(input, "name")?;
            let target = confined(root, string(input, "path")?)?;
            tokio::fs::create_dir_all(&target).await?;
            match string(input, "from")? {
                kind @ ("dataset" | "baseline") => {
                    let identifier = input.get("id").and_then(Value::as_str).unwrap_or(name);
                    if !slug(identifier) {
                        return Err(RuntimeError::Contract);
                    }
                    // Only the scorer and the policy may read held-out labels.
                    if self.mode != SessionMode::Policy
                        && string(spec, "role")? != "scorer"
                        && array(science, "datasets")?.iter().any(|dataset| {
                            dataset["id"] == identifier && dataset["held_out_labels"] == true
                        })
                    {
                        return Err(RuntimeError::Contract);
                    }
                    let collection = format!("{kind}s");
                    let reference = array(&self.job["inputs"], &collection)?
                        .iter()
                        .find(|item| item["id"] == identifier)
                        .ok_or(RuntimeError::Contract)?;
                    let revision = reference["revision"]
                        .as_str()
                        .map_or_else(|| reference["revision"].to_string(), str::to_owned);
                    if revision.is_empty()
                        || revision.contains(['/', '\\'])
                        || revision.contains("..")
                        || revision.starts_with('.')
                    {
                        return Err(RuntimeError::Contract);
                    }
                    let base = self.worker.data_root.join(&collection).join(identifier);
                    let source = tokio::fs::canonicalize(base.join(revision)).await?;
                    let base = tokio::fs::canonicalize(base).await?;
                    if source.parent() != Some(base.as_path()) {
                        return Err(RuntimeError::Integrity);
                    }
                    copy_input(source, target).await?;
                }
                "attempt" if matches!(self.mode, SessionMode::Policy | SessionMode::Decide) => {
                    return Err(RuntimeError::Contract);
                }
                "attempt" if self.mode == SessionMode::Experiment => {
                    if name == "claimed_sheet" {
                        return Err(RuntimeError::Contract);
                    }
                    let predecessor = &self.job["inputs"]["predecessor"];
                    let objects = if predecessor.is_null() {
                        &[][..]
                    } else {
                        array(predecessor, "artifacts")?.as_slice()
                    };
                    let mut filenames = std::collections::BTreeSet::new();
                    for object in objects.iter().filter(|object| object["role"] == name) {
                        let key = string(&object["storage"], "key")?;
                        let filename = Path::new(key).file_name().ok_or(RuntimeError::Contract)?;
                        if !filenames.insert(filename.to_owned()) {
                            return Err(RuntimeError::Contract);
                        }
                        let id = string(object, "id")?;
                        uuid::Uuid::parse_str(id).map_err(|_| RuntimeError::Contract)?;
                        let destination = target.join(filename);
                        // Settle the owned filesystem writer before mounted-root removal.
                        self.worker
                            .client
                            .download(
                                &format!("{}/inputs/predecessor/{id}", self.base),
                                "",
                                &self.worker.token,
                                self.lease,
                                &destination,
                                object["size_bytes"]
                                    .as_u64()
                                    .ok_or(RuntimeError::Contract)?,
                                &self.cancel,
                            )
                            .await?;
                        let (size, hash) = http::digest(&destination).await?;
                        if object["size_bytes"].as_u64() != Some(size) || object["sha256"] != hash {
                            return Err(RuntimeError::Integrity);
                        }
                        if self.cancel.is_set() {
                            return Err(RuntimeError::LostLease);
                        }
                    }
                }
                // The front matter of the frozen run document, never its notes.
                "attempt" if name == "claimed_sheet" => {
                    let run = self
                        .get_json(&format!("{}/inputs/run", self.base), Some(self.lease))
                        .await?;
                    let expected = string(&self.job["inputs"]["run"], "sha256")?;
                    if canonical_digest(&run)? != expected {
                        return Err(RuntimeError::Integrity);
                    }
                    tokio::fs::write(
                        target.join("claimed.json"),
                        serde_json::to_vec_pretty(&run)?,
                    )
                    .await?;
                }
                "attempt" => {
                    let manifest = self
                        .get_json(&format!("{}/inputs/manifest", self.base), Some(self.lease))
                        .await?;
                    if canonical_digest(&manifest)?
                        != string(&self.job["inputs"]["manifest"], "sha256")?
                    {
                        return Err(RuntimeError::Integrity);
                    }
                    let objects: Vec<_> = array(&manifest, "objects")?
                        .iter()
                        .filter(|object| object["role"] == name)
                        .collect();
                    if objects.is_empty() {
                        return Err(RuntimeError::Contract);
                    }
                    for object in objects {
                        let key = string(&object["storage"], "key")?;
                        let filename = Path::new(key).file_name().ok_or(RuntimeError::Contract)?;
                        let destination = target.join(filename);
                        let object_path = format!("{}/inputs/object", self.base);
                        let download = self.worker.client.download(
                            &object_path,
                            key,
                            &self.worker.token,
                            self.lease,
                            &destination,
                            object["size_bytes"]
                                .as_u64()
                                .ok_or(RuntimeError::Contract)?,
                            &self.cancel,
                        );
                        download.await?;
                        let (size, hash) = http::digest(&destination).await?;
                        if object["size_bytes"].as_u64() != Some(size) || object["sha256"] != hash {
                            return Err(RuntimeError::Integrity);
                        }
                    }
                }
                _ => {
                    let produced = self.produced.get(name).ok_or(RuntimeError::Contract)?;
                    // Refuse links before copying, then check the copied bytes against
                    // the bytes actually accepted by the server. Local producer mutation
                    // after PUT cannot become the scorer's input.
                    ensure_unlinked(&produced.output_root, &produced.path).await?;
                    inspect_output(&produced.path).await?;
                    copy_input(produced.path.clone(), target.clone()).await?;
                    let mut actual = BTreeMap::new();
                    for (path, relative) in inspect_output(&target).await? {
                        actual.insert(relative, http::digest(&path).await?);
                    }
                    if actual != produced.digests {
                        return Err(RuntimeError::Integrity);
                    }
                }
            }
        }
        Ok(())
    }
    async fn upload(
        &mut self,
        file: &Path,
        role: &str,
        path: &str,
        interface: Option<&str>,
    ) -> Result<Value, RuntimeError> {
        if self.cancel.is_set() {
            return Err(RuntimeError::LostLease);
        }
        let (size, hash) = http::digest(file).await?;
        let mut body = json!({"role":role,"path":path,"size_bytes":size,"sha256":hash,"media_type":media_type(file)});
        if self.mode == SessionMode::Experiment {
            let relative = path.splitn(3, '/').nth(2).ok_or(RuntimeError::Contract)?;
            if relative.contains('/') {
                return Err(RuntimeError::Contract);
            }
            body = json!({"role":role,"name":relative,"size_bytes":size,"sha256":hash,"media_type":media_type(file)});
        }
        if let Some(interface) = interface.filter(|_| self.mode != SessionMode::Experiment) {
            body["interface"] = json!(interface);
        }
        let transfer = async {
            let response = self
                .worker
                .client
                .request(
                    Method::POST,
                    &format!("{}/uploads", self.base),
                    &self.worker.token,
                    Some(self.lease),
                    Some(&body),
                )
                .await?;
            if response.status().as_u16() == 422 {
                return Err(RuntimeError::InvalidOutput);
            }
            let grant = http::json_response(response).await?;
            let response = if let Some(plan) = grant.get("direct").filter(|v| !v.is_null()) {
                self.send_direct(file, size, plan, &grant["headers"])
                    .await?
            } else {
                self.worker
                    .client
                    .upload_api(string(&grant, "upload_url")?, &grant["headers"], file, size)
                    .await?
            };
            let status = response.status().as_u16();
            let artifact = http::read_json(response).await?;
            Ok::<_, RuntimeError>((status, artifact))
        };
        // This future reads local files and performs HTTP only. Receiver-side
        // partial-object cleanup/expiry owns an interrupted remote transfer.
        let (status, artifact) =
            http::cancelled(&self.cancel, RuntimeError::LostLease, transfer).await?;
        match (status, artifact["error"]["code"].as_str()) {
            (409, Some("stale_lease")) => return Err(RuntimeError::LostLease),
            (409, Some("upload_expired")) => return Err(RuntimeError::UploadExpired),
            (422, Some("invalid_content")) => return Err(RuntimeError::InvalidStepOutput),
            (422, _) => return Err(RuntimeError::DurablyFailed),
            (200..=299, _) => {}
            _ => return Err(RuntimeError::Http(status)),
        }
        if artifact["size_bytes"].as_u64() != Some(size) || artifact["sha256"] != hash {
            return Err(RuntimeError::Integrity);
        }
        if artifact["role"] != role {
            return Err(RuntimeError::Integrity);
        }
        self.objects.push(json!({"role":artifact["role"],"storage":artifact["storage"],"size_bytes":artifact["size_bytes"],"sha256":artifact["sha256"],"media_type":artifact["media_type"]}));
        Ok(artifact)
    }
    #[allow(clippy::too_many_lines)] // One bounded multipart transfer state machine.
    async fn send_direct(
        &self,
        file: &Path,
        size: u64,
        plan: &Value,
        headers: &Value,
    ) -> Result<reqwest::Response, RuntimeError> {
        let mut parts = BTreeMap::new();
        let single = string(plan, "transfer")? == "single";
        let count = if single {
            1
        } else {
            plan["part_count"].as_u64().ok_or(RuntimeError::Contract)?
        };
        let part_size = if single {
            size
        } else {
            plan["part_size"]
                .as_u64()
                .filter(|size| *size > 0)
                .ok_or(RuntimeError::Contract)?
        };
        // The API's S3 contract caps multipart transfers at 10,000 parts.
        // Check its arithmetic before allocating or iterating server counts.
        if !single && (count == 0 || count > 10_000 || count != size.div_ceil(part_size)) {
            return Err(RuntimeError::Contract);
        }
        if single {
            parts.insert(1, plan["request"].clone());
        } else {
            for part in array(plan, "parts")? {
                parts.insert(
                    part["part_number"].as_u64().ok_or(RuntimeError::Contract)?,
                    part.clone(),
                );
            }
        }
        let capability = Capability { headers };
        let mut pending: Vec<_> = (1..=count).collect();
        for finish_attempt in 0..4 {
            for number in &pending {
                let offset = (number - 1)
                    .checked_mul(part_size)
                    .ok_or(RuntimeError::Contract)?;
                let length = size
                    .checked_sub(offset)
                    .ok_or(RuntimeError::Contract)?
                    .min(part_size);
                for attempt in 0..4 {
                    if !parts.contains_key(number) {
                        let body = (!single).then(|| json!({"part_numbers":[number]}));
                        let fresh = http::json_response(
                            self.capability_post(
                                string(plan, "presign_url")?,
                                &capability,
                                body.as_ref(),
                            )
                            .await?,
                        )
                        .await?;
                        if single {
                            parts.insert(*number, fresh["request"].clone());
                        } else {
                            for part in array(&fresh, "parts")? {
                                parts.insert(
                                    part["part_number"].as_u64().ok_or(RuntimeError::Contract)?,
                                    part.clone(),
                                );
                            }
                        }
                    }
                    let request = parts.get(number).ok_or(RuntimeError::Contract)?;
                    let response = self
                        .worker
                        .client
                        .signed_put(request, file, offset, length)
                        .await;
                    match response {
                        Ok(response) if response.status().is_success() => break,
                        Ok(response)
                            if attempt < 3
                                && matches!(
                                    response.status().as_u16(),
                                    401 | 403 | 408 | 429 | 500 | 502 | 503 | 504
                                ) =>
                        {
                            parts.remove(number);
                        }
                        Ok(response) => return Err(RuntimeError::Http(response.status().as_u16())),
                        Err(_) if attempt < 3 => {}
                        Err(error) => return Err(error),
                    }
                    tokio::time::sleep(Duration::from_millis(500 << attempt)).await;
                }
            }
            let response = self
                .capability_post(string(plan, "finish_url")?, &capability, None)
                .await?;
            if response.status().as_u16() != 409 || finish_attempt == 3 {
                return Ok(response);
            }
            let body = http::read_json(response).await?;
            let Some(missing) = body["error"]["details"]["missing_parts"].as_array() else {
                return Err(RuntimeError::Http(409));
            };
            pending = missing
                .iter()
                .map(|value| {
                    value
                        .as_u64()
                        .filter(|n| *n > 0 && *n <= count)
                        .ok_or(RuntimeError::Contract)
                })
                .collect::<Result<_, _>>()?;
            if pending.is_empty() {
                return Err(RuntimeError::Http(409));
            }
            for number in &pending {
                parts.remove(number);
            }
        }
        Err(RuntimeError::Transport)
    }
    async fn capability_post(
        &self,
        path: &str,
        capability: &Capability<'_>,
        body: Option<&Value>,
    ) -> Result<reqwest::Response, RuntimeError> {
        self.worker
            .client
            .capability_post(path, capability.headers, body)
            .await
    }
}
struct Capability<'a> {
    headers: &'a Value,
}
#[allow(clippy::too_many_arguments)] // Owned credential, clock and cancellation state.
pub(crate) async fn heartbeat_loop(
    client: ApiClient,
    token: Secret,
    lease: LeaseHeaders,
    base: String,
    interval: f64,
    mut expiry: SystemTime,
    lost: CancellationEvent,
    done: CancellationEvent,
) {
    let mut reported_denial = false;
    loop {
        tokio::select! {()=done.wait()=>return, ()=tokio::time::sleep(Duration::from_secs_f64(interval))=>{}}
        let mut delay = 0.5f64;
        loop {
            let remaining = time_left(expiry);
            if remaining <= 0.0 {
                lost.set();
                return;
            }
            let renewed = tokio::select! {
                ()=done.wait()=>return,
                result=tokio::time::timeout(Duration::from_secs_f64(remaining),
                    renew_lease(&client,&token,&lease,&base))=>result,
            };
            match renewed {
                Ok(Ok(value)) => {
                    expiry = value;
                    break;
                }
                Ok(Err(RuntimeError::LostLease)) | Err(_) => {
                    lost.set();
                    return;
                }
                Ok(Err(RuntimeError::Http(status @ (401 | 403)))) => {
                    if !reported_denial {
                        eprintln!("runner lease renewal HTTP {status}");
                        reported_denial = true;
                    }
                }
                Ok(Err(_)) => {}
            }
            if time_left(expiry) <= delay {
                lost.set();
                return;
            }
            tokio::select! {()=done.wait()=>return, ()=tokio::time::sleep(Duration::from_secs_f64(delay))=>{}}
            delay = (delay * 2.0).min(interval.max(0.5)).min(30.0);
        }
    }
}
async fn renew_lease(
    client: &ApiClient,
    token: &Secret,
    lease: &LeaseHeaders,
    base: &str,
) -> Result<SystemTime, RuntimeError> {
    let response = client
        .request(
            Method::POST,
            &format!("{base}/heartbeat"),
            token,
            Some(lease),
            None,
        )
        .await?;
    let status = response.status().as_u16();
    let body = http::read_json(response).await?;
    if status == 409 && body["error"]["code"] == "stale_lease" {
        return Err(RuntimeError::LostLease);
    }
    if status != 200 {
        return Err(RuntimeError::Http(status));
    }
    instant(string(&body, "lease_expires_at")?)
}
pub(crate) fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, RuntimeError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or(RuntimeError::Contract)
}
fn array<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>, RuntimeError> {
    value
        .get(key)
        .and_then(Value::as_array)
        .ok_or(RuntimeError::Contract)
}
pub(crate) fn positive(value: &Value) -> Result<f64, RuntimeError> {
    value
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.0 && Duration::try_from_secs_f64(*v).is_ok())
        .ok_or(RuntimeError::Contract)
}
pub(crate) fn science_revision(value: &Value) -> Result<u64, RuntimeError> {
    value
        .as_u64()
        .or_else(|| {
            value
                .as_str()
                .filter(|text| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
                .and_then(|text| text.parse().ok())
        })
        .filter(|revision| *revision > 0)
        .ok_or(RuntimeError::Contract)
}
pub(crate) fn instant(text: &str) -> Result<SystemTime, RuntimeError> {
    let time = chrono::DateTime::parse_from_rfc3339(text).map_err(|_| RuntimeError::Contract)?;
    let micros = time.timestamp_micros();
    let duration = Duration::from_micros(micros.unsigned_abs());
    if micros < 0 {
        SystemTime::UNIX_EPOCH.checked_sub(duration)
    } else {
        SystemTime::UNIX_EPOCH.checked_add(duration)
    }
    .ok_or(RuntimeError::Contract)
}
pub(crate) fn time_left(deadline: SystemTime) -> f64 {
    deadline
        .duration_since(SystemTime::now())
        .map_or(0.0, |d| d.as_secs_f64())
}
pub(crate) fn random_name() -> Result<String, RuntimeError> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(|_| RuntimeError::Filesystem)?;
    Ok(uuid::Uuid::from_bytes(bytes).simple().to_string())
}
fn slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}
fn confined(root: &Path, value: &str) -> Result<PathBuf, RuntimeError> {
    let relative = value.strip_prefix("/cr/").ok_or(RuntimeError::Contract)?;
    if relative
        .split('/')
        .any(|part| part.is_empty() || part == ".." || part == ".")
    {
        return Err(RuntimeError::Contract);
    }
    Ok(root.join(relative))
}
fn output_files(local: &Path) -> Result<Vec<(PathBuf, String)>, RuntimeError> {
    let metadata = match std::fs::symlink_metadata(local) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        return Err(RuntimeError::InvalidOutput);
    }
    if metadata.is_file() {
        return Ok(vec![(
            local.to_owned(),
            local
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or(RuntimeError::Contract)?
                .to_owned(),
        )]);
    }
    if !metadata.is_dir() {
        return Err(RuntimeError::Integrity);
    }
    let mut result = vec![];
    let mut pending = vec![local.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path)?;
            if meta.file_type().is_symlink() {
                return Err(RuntimeError::InvalidOutput);
            }
            if meta.is_dir() {
                pending.push(path);
            } else if meta.is_file() {
                let relative = path
                    .strip_prefix(local)
                    .map_err(|_| RuntimeError::InvalidOutput)?
                    .to_str()
                    .ok_or(RuntimeError::Contract)?
                    .to_owned();
                result.push((path, relative));
            } else {
                return Err(RuntimeError::InvalidOutput);
            }
        }
    }
    if result.is_empty() {
        return Err(RuntimeError::MissingOutput);
    }
    Ok(result)
}
async fn copy_input(source: PathBuf, target: PathBuf) -> Result<(), RuntimeError> {
    tokio::task::spawn_blocking(move || {
        for (path, relative) in output_files(&source)? {
            let destination = target.join(relative);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(path, destination)?;
        }
        Ok(())
    })
    .await
    .map_err(|_| RuntimeError::Filesystem)?
}
async fn make_read_only(root: PathBuf) -> Result<(), RuntimeError> {
    tokio::task::spawn_blocking(move || {
        use std::os::unix::fs::PermissionsExt;
        let mut directories = vec![root];
        let mut cursor = 0;
        while cursor < directories.len() {
            for entry in std::fs::read_dir(&directories[cursor])? {
                let entry = entry?;
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    directories.push(entry.path());
                } else if kind.is_file() {
                    std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(0o400))?;
                } else {
                    return Err(RuntimeError::InvalidOutput);
                }
            }
            cursor += 1;
        }
        for directory in directories.into_iter().rev() {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o500))?;
        }
        Ok(())
    })
    .await
    .map_err(|_| RuntimeError::Filesystem)?
}
fn document(value: &Value) -> Result<json::Document, RuntimeError> {
    json::decode(&serde_json::to_vec(value)?, 256).map_err(|_| RuntimeError::Contract)
}
pub(crate) fn canonical_digest(value: &Value) -> Result<String, RuntimeError> {
    let doc = document(value)?;
    json::canonical::sha256(&doc, 256).map_err(|_| RuntimeError::Contract)
}
fn media_type(path: &Path) -> &'static str {
    match path.extension().and_then(|v| v.to_str()) {
        Some("json") => "application/json",
        Some("jsonl") => "application/jsonl",
        Some("tsv") => "text/tab-separated-values",
        Some("csv") => "text/csv",
        Some("txt" | "log") => "text/plain",
        _ => "application/octet-stream",
    }
}
fn texts(value: &Value) -> Result<Vec<String>, RuntimeError> {
    value
        .as_array()
        .ok_or(RuntimeError::Contract)?
        .iter()
        .map(|v| v.as_str().map(String::from).ok_or(RuntimeError::Contract))
        .collect()
}
fn manifest_step(manifest: &Value, job: &str, label: &str) -> Result<StepSpec, RuntimeError> {
    let spec = &manifest["spec"];
    let container = &spec["container"];
    let limits = &container["resources"]["limits"];
    let network = if spec["network"] == "none" {
        Network::None
    } else {
        Network::Egress(texts(&spec["network"]["egress"])?)
    };
    let env = container["env"]
        .as_array()
        .map_or(Ok::<_, RuntimeError>(vec![]), |env| {
            env.iter()
                .map(|value| {
                    Ok((
                        String::from(string(value, "name")?),
                        String::from(string(value, "value")?),
                    ))
                })
                .collect()
        })?;
    let quantity = |name: &str| limits.get(name).and_then(Value::as_str).map(String::from);
    let manifest = Manifest {
        image: String::from(string(container, "image")?),
        command: texts(&container["command"])?,
        args: container.get("args").map_or(Ok(vec![]), texts)?,
        env,
        network,
        cpu: quantity("cpu"),
        memory: quantity("memory"),
        gpus: quantity("nvidia.com/gpu"),
    };
    launcher::StepSpec::from_manifest(&manifest, String::from(job), String::from(label))
        .map_err(|_| RuntimeError::Contract)
}
fn log_reference(artifact: &Value) -> Result<Value, RuntimeError> {
    Ok(
        json!({"key":string(&artifact["storage"],"key")?,"size_bytes":artifact["size_bytes"],"sha256":artifact["sha256"]}),
    )
}

async fn ensure_unlinked(base: &Path, path: &Path) -> Result<(), RuntimeError> {
    let relative = path
        .strip_prefix(base)
        .map_err(|_| RuntimeError::Integrity)?;
    let mut current = base.to_owned();
    if existing_link(&current).await? {
        return Err(RuntimeError::Integrity);
    }
    for part in relative.components() {
        if !matches!(part, std::path::Component::Normal(_)) {
            return Err(RuntimeError::Integrity);
        }
        current.push(part.as_os_str());
        if existing_link(&current).await? {
            return Err(RuntimeError::Integrity);
        }
    }
    Ok(())
}
async fn existing_link(path: &Path) -> Result<bool, RuntimeError> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => Ok(metadata.file_type().is_symlink()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

async fn inspect_output(path: &Path) -> Result<Vec<(PathBuf, String)>, RuntimeError> {
    let owned = path.to_owned();
    tokio::task::spawn_blocking(move || output_files(&owned))
        .await
        .map_err(|_| RuntimeError::Filesystem)?
}
