//! Tester pipeline: pinned inputs, sequential steps, integrity checks and uploads.
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
pub struct Tester {
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
impl super::process::RuntimeWorker for Tester {
    fn run_once(&self, cancel: CancellationEvent) -> FutureResult<'_, Option<JobResult>> {
        Box::pin(Tester::run_once(self, cancel))
    }
    fn close(&self) -> FutureResult<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
impl Tester {
    /// # Errors
    /// Claims exactly one job; 409 means idle. All other failures are explicit.
    #[allow(clippy::too_many_lines)] // One authoritative lease/heartbeat/cleanup scope.
    pub async fn run_once(
        &self,
        cancel: CancellationEvent,
    ) -> Result<Option<JobResult>, RuntimeError> {
        let api = format!("/api/projects/{}", self.project);
        let claimed = http::cancelled(&cancel, RuntimeError::Cancelled, async {
            let response = self
                .client
                .request(
                    Method::POST,
                    &format!("{api}/jobs/claims"),
                    &self.token,
                    None,
                    Some(&json!({"stage":"tester"})),
                )
                .await?;
            if response.status().as_u16() == 409 {
                return Ok(None);
            }
            if response.status().as_u16() != 201 {
                return Err(RuntimeError::Http(response.status().as_u16()));
            }
            Ok(Some(http::read_json(response).await?))
        })
        .await?;
        let Some(claim) = claimed else {
            return Ok(None);
        };
        let job = claim.get("job").ok_or(RuntimeError::Contract)?;
        let id = string(job, "job_id")?.to_owned();
        uuid::Uuid::parse_str(&id).map_err(|_| RuntimeError::Contract)?;
        let lease = LeaseHeaders {
            token: Secret::new(string(&job["lease"], "token")?.to_owned()),
            generation: job["lease"]["generation"].to_string(),
            idempotency: None,
        };
        let base = format!("{api}/jobs/{id}");
        let heartbeat = positive(&claim["heartbeat_seconds"])?;
        let initial_expiry = instant(string(&job["lease"], "expires_at")?)?;
        let work = self.work_root.join(format!("cr-job-{}", random_name()?));
        tokio::fs::create_dir_all(&self.work_root).await?;
        tokio::fs::create_dir(&work).await?;
        let lost = CancellationEvent::new();
        let done = CancellationEvent::new();
        let _done_guard = DoneGuard(done.clone());
        let renew_client = self.client.clone();
        let renew_token = Secret::new(self.token.expose().to_owned());
        let renew_lease = LeaseHeaders {
            token: Secret::new(lease.token.expose().to_owned()),
            generation: lease.generation.clone(),
            idempotency: None,
        };
        let renew_base = base.clone();
        let renew_lost = lost.clone();
        let renew_done = done.clone();
        let renewal = tokio::spawn(async move {
            heartbeat_loop(
                renew_client,
                renew_token,
                renew_lease,
                renew_base,
                heartbeat,
                initial_expiry,
                renew_lost,
                renew_done,
            )
            .await;
        });
        let combined = lost.clone();
        let shutdown = cancel.clone();
        let forwarding_done = done.clone();
        let forwarding = tokio::spawn(async move {
            tokio::select! { () = shutdown.wait() => combined.set(), () = forwarding_done.wait() => {} }
        });
        let mut session = Session {
            mode: SessionMode::Tester,
            worker: self,
            claim: &claim,
            job,
            api: &api,
            base: &base,
            lease: &lease,
            work: &work,
            cancel: lost.clone(),
            objects: vec![],
            produced: BTreeMap::new(),
            failure_step: None,
            failure_logs: vec![],
        };
        let result = session.run_steps().await;
        let delivery = async {
            if cancel.is_set() {
                Err(RuntimeError::Cancelled)
            } else if lost.is_set() {
                Ok("abandoned".to_owned())
            } else {
                match result {
                    Ok(evidence) => {
                        let complete_lease = LeaseHeaders {
                            token: Secret::new(lease.token.expose().to_owned()),
                            generation: lease.generation.clone(),
                            idempotency: Some(format!("complete-{id}-{}", lease.generation)),
                        };
                        let completion = self
                            .client
                            .request(
                                Method::POST,
                                &format!("{base}/completion"),
                                &self.token,
                                Some(&complete_lease),
                                Some(&http::completion(job, &evidence, &session.objects)),
                            )
                            .await;
                        let completed = match completion {
                            Ok(response) => outcome_state(response, true).await,
                            Err(error) => Err(error),
                        };
                        match completed {
                            Ok(state) => Ok(state),
                            Err(error) => {
                                self.report_failure(
                                    &base,
                                    &lease,
                                    &id,
                                    error,
                                    session.failure_step.as_deref(),
                                    &session.failure_logs,
                                )
                                .await
                            }
                        }
                    }
                    Err(error) => {
                        self.report_failure(
                            &base,
                            &lease,
                            &id,
                            error,
                            session.failure_step.as_deref(),
                            &session.failure_logs,
                        )
                        .await
                    }
                }
            }
        };
        let state = tokio::select! {
            biased;
            () = lost.wait() => Ok("abandoned".to_owned()),
            state = delivery => state,
        };
        done.set();
        let _ = renewal.await;
        let _ = forwarding.await;
        // Backend teardown always precedes removing directories it may still mount.
        self.backend.release_job(&id).await?;
        let owned_work = work.clone();
        tokio::task::spawn_blocking(move || crate::removal::remove_tree(&owned_work))
            .await
            .map_err(|_| RuntimeError::Filesystem)?;
        if cancel.is_set() {
            return Err(RuntimeError::Cancelled);
        }
        Ok(Some(JobResult {
            job_id: id,
            state: state?,
        }))
    }
    async fn report_failure(
        &self,
        base: &str,
        lease: &LeaseHeaders,
        id: &str,
        error: RuntimeError,
        step: Option<&str>,
        logs: &[Value],
    ) -> Result<String, RuntimeError> {
        let code = match error {
            RuntimeError::Integrity => "input_verification_failed",
            RuntimeError::StepFailed => "step_failed",
            RuntimeError::SetupFailed => "setup_failed",
            RuntimeError::CodeNotAllowed => "code_not_allowed",
            RuntimeError::InvalidCode => "invalid_code",
            RuntimeError::DeadlineExceeded => "deadline_exceeded",
            RuntimeError::InvalidStepOutput => "invalid_step_output",
            RuntimeError::InvalidOutput => "invalid_output",
            RuntimeError::UploadExpired => "upload_expired",
            RuntimeError::DurablyFailed => return Ok("failed".to_owned()),
            RuntimeError::MissingOutput => "missing_output",
            RuntimeError::LostLease => return Ok("abandoned".to_owned()),
            _ => "runner_error",
        };
        let mut report = json!({"schema_version":"0.2", "job_id":id,"error_code":code,"reason":error.to_string(),"logs":logs});
        if let Some(step) = step {
            report["step"] = json!(step);
        }
        let mut response = self
            .client
            .request(
                Method::POST,
                &format!("{base}/failure"),
                &self.token,
                Some(lease),
                Some(&report),
            )
            .await;
        if (step.is_some() || !logs.is_empty())
            && response
                .as_ref()
                .is_ok_and(|reply| reply.status().as_u16() == 422)
        {
            // Invalid log/step references must not suppress the failure itself.
            report["logs"] = json!([]);
            report
                .as_object_mut()
                .ok_or(RuntimeError::Contract)?
                .remove("step");
            response = self
                .client
                .request(
                    Method::POST,
                    &format!("{base}/failure"),
                    &self.token,
                    Some(lease),
                    Some(&report),
                )
                .await;
        }
        match response {
            Ok(response) => outcome_state(response, false)
                .await
                .or(Ok("unreported".to_owned())),
            Err(_) => Ok("unreported".to_owned()),
        }
    }
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
pub(crate) struct EvaluatorInputs {
    pub science: Value,
    pub evidence: Value,
    pub manifest: Value,
    pub metrics: Value,
}
#[derive(Clone, Copy)]
enum SessionMode<'a> {
    Tester,
    Experiment,
    Evaluator(&'a EvaluatorInputs),
}
impl SessionMode<'_> {
    fn is_experiment(self) -> bool {
        matches!(self, Self::Experiment)
    }
    fn is_evaluator(self) -> bool {
        matches!(self, Self::Evaluator(_))
    }
    fn owns_transfers(self) -> bool {
        !matches!(self, Self::Tester)
    }
}
pub(crate) struct Session<'a> {
    mode: SessionMode<'a>,
    worker: &'a Tester,
    claim: &'a Value,
    job: &'a Value,
    api: &'a str,
    base: &'a str,
    lease: &'a LeaseHeaders,
    work: &'a Path,
    cancel: CancellationEvent,
    objects: Vec<Value>,
    produced: BTreeMap<String, Produced>,
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
    pub(crate) fn experiment<'a>(
        worker: &'a Tester,
        claim: &'a Value,
        job: &'a Value,
        api: &'a str,
        base: &'a str,
        lease: &'a LeaseHeaders,
        work: &'a Path,
        cancel: CancellationEvent,
    ) -> Session<'a> {
        Session {
            mode: SessionMode::Experiment,
            worker,
            claim,
            job,
            api,
            base,
            lease,
            work,
            cancel,
            objects: vec![],
            produced: BTreeMap::new(),
            failure_step: None,
            failure_logs: vec![],
        }
    }
    pub(crate) fn objects(&self) -> &[Value] {
        &self.objects
    }
    #[allow(clippy::too_many_arguments)] // Borrow one session, its lease and verified evaluator inputs.
    pub(crate) fn evaluator<'a>(
        worker: &'a Tester,
        claim: &'a Value,
        job: &'a Value,
        api: &'a str,
        base: &'a str,
        lease: &'a LeaseHeaders,
        work: &'a Path,
        cancel: CancellationEvent,
        inputs: &'a EvaluatorInputs,
    ) -> Session<'a> {
        let mut session = Self::experiment(worker, claim, job, api, base, lease, work, cancel);
        session.mode = SessionMode::Evaluator(inputs);
        session
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
        match self.mode {
            SessionMode::Evaluator(inputs) => Ok(Some(document(&inputs.metrics)?)),
            _ => Ok(None),
        }
    }
    pub(crate) fn failure(&self) -> (Option<&str>, &[Value]) {
        (self.failure_step.as_deref(), &self.failure_logs)
    }
    fn job_kind(&self) -> crate::job::JobKind {
        match self.mode {
            SessionMode::Tester => crate::job::JobKind::Tester,
            SessionMode::Experiment => crate::job::JobKind::Experiment,
            SessionMode::Evaluator(_) => crate::job::JobKind::Evaluator,
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
    #[allow(clippy::too_many_lines)] // Preserve sequential validate-all-before-upload order.
    pub(crate) async fn run_steps(&mut self) -> Result<Value, RuntimeError> {
        let revision = science_revision(&self.job["science_revision"])?;
        let science = if let SessionMode::Evaluator(inputs) = self.mode {
            inputs.science.clone()
        } else {
            self.get_json(&format!("{}/config/science/{}", self.api, revision), None)
                .await?
        };
        let science = &science["content"];
        let deadline = instant(string(self.job, "deadline")?)?;
        let mut evidence = None;
        for (index, step) in array(self.job, "steps")?.iter().enumerate() {
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
            if if self.mode.is_evaluator() {
                role != "evaluator"
            } else if self.mode.is_experiment() {
                role != "experiment"
            } else {
                !["producer", "scorer"].contains(&role)
            } {
                return Err(RuntimeError::Contract);
            }
            let root = self.work.join(format!("{index}-{name}"));
            tokio::fs::create_dir(&root).await?;
            self.stage_inputs(&root, spec, science).await?;
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
                        // evidence for blaming the agent rather than a tester.
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
                if self.mode.is_evaluator()
                    && output["name"] == "verdict"
                    && (files.len() != 1
                        || !files[0]
                            .0
                            .extension()
                            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
                        || tokio::fs::metadata(&files[0].0).await?.len() > 1024 * 1024)
                {
                    return Err(RuntimeError::InvalidOutput);
                }
                checked.push((output, local, files));
            }
            for (output, local, files) in checked {
                let output_name = string(output, "name")?;
                let mut digests = BTreeMap::new();
                for (path, relative) in files {
                    if self.mode.is_experiment() && output_name == RUN_OUTPUT {
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
                if (role == "scorer" && output_name == "evidence")
                    || (self.mode.is_experiment() && output_name == RUN_OUTPUT)
                    || (self.mode.is_evaluator() && output_name == "verdict")
                {
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
        let evidence = evidence.ok_or(if self.mode.is_experiment() {
            RuntimeError::MissingOutput
        } else {
            RuntimeError::Contract
        })?;
        if self.mode.owns_transfers() {
            ensure_unlinked(self.work, &evidence).await?;
        }
        let files = inspect_output(&evidence).await?;
        if files.len() != 1 {
            return Err(if self.mode.is_experiment() {
                RuntimeError::InvalidStepOutput
            } else {
                RuntimeError::Contract
            });
        }
        if self.mode.owns_transfers() {
            if !files[0].0.extension().is_some_and(|extension| {
                extension.eq_ignore_ascii_case(if self.mode.is_experiment() {
                    "md"
                } else {
                    "json"
                })
            }) {
                return Err(RuntimeError::InvalidStepOutput);
            }
            let produced = self
                .produced
                .get(if self.mode.is_evaluator() {
                    "verdict"
                } else {
                    RUN_OUTPUT
                })
                .ok_or(RuntimeError::Contract)?;
            for (path, relative) in &files {
                if produced.digests.get(relative) != Some(&http::digest(path).await?) {
                    return Err(RuntimeError::Integrity);
                }
            }
        }
        let file = tokio::fs::File::open(&files[0].0).await?;
        let mut bytes = Vec::new();
        let limit = if self.mode.is_evaluator() || self.mode.is_experiment() {
            1024 * 1024
        } else {
            16 * 1024 * 1024
        };
        file.take(limit + 1).read_to_end(&mut bytes).await?;
        if u64::try_from(bytes.len()).map_err(|_| RuntimeError::InvalidOutput)? > limit {
            return Err(if self.mode.is_experiment() {
                RuntimeError::InvalidStepOutput
            } else {
                RuntimeError::InvalidOutput
            });
        }
        if self.mode.is_experiment() {
            // The run document: front matter and notes, submitted by the session.
            let text = String::from_utf8(bytes).map_err(|_| RuntimeError::InvalidStepOutput)?;
            let document = front_matter::parse(&text, front_matter::Limits::default())
                .map_err(|_| RuntimeError::InvalidStepOutput)?;
            return Ok(json!({
                "front_matter": Value::Object(document.front_matter),
                "body": document.body,
            }));
        }
        let invalid = RuntimeError::InvalidOutput;
        json::decode(&bytes, 128).map_err(|_| invalid)?;
        let evidence: Value = serde_json::from_slice(&bytes).map_err(|_| invalid)?;
        if !evidence.is_object() {
            return Err(invalid);
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
                    if !self.mode.is_evaluator()
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
                "attempt"
                    if self.mode.is_evaluator() && ["evidence", "manifest"].contains(&name) =>
                {
                    let SessionMode::Evaluator(inputs) = self.mode else {
                        return Err(RuntimeError::Contract);
                    };
                    let (file, value) = if name == "evidence" {
                        ("evidence.json", &inputs.evidence["staged"])
                    } else {
                        ("manifest.json", &inputs.manifest)
                    };
                    tokio::fs::write(target.join(file), serde_json::to_vec_pretty(value)?).await?;
                }
                "attempt" if self.mode.is_experiment() => {
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
                "attempt" if name == "claimed_sheet" => {
                    let sheet = self
                        .get_json(
                            &format!("{}/inputs/claimed-sheet", self.base),
                            Some(self.lease),
                        )
                        .await?;
                    let expected = string(&self.job["inputs"]["claimed_sheet"], "sha256")?;
                    if canonical_digest(&sheet)? != expected {
                        return Err(RuntimeError::Integrity);
                    }
                    tokio::fs::write(
                        target.join("claimed.json"),
                        serde_json::to_vec_pretty(&sheet)?,
                    )
                    .await?;
                }
                "attempt" => {
                    let manifest = if let SessionMode::Evaluator(inputs) = self.mode {
                        inputs.manifest.clone()
                    } else {
                        self.get_json(&format!("{}/inputs/manifest", self.base), Some(self.lease))
                            .await?
                    };
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
        if self.mode.is_experiment() {
            let relative = path.splitn(3, '/').nth(2).ok_or(RuntimeError::Contract)?;
            if relative.contains('/') {
                return Err(RuntimeError::Contract);
            }
            body = json!({"role":role,"name":relative,"size_bytes":size,"sha256":hash,"media_type":media_type(file)});
        }
        if let Some(interface) = interface.filter(|_| !self.mode.is_experiment()) {
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
        if self.mode.owns_transfers() && artifact["role"] != role {
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
