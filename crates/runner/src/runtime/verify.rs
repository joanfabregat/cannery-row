//! The verify kind: claim a runner verify job registered to this verifier and
//! policy revision, run its producer and scorer, apply the policy (stock gates
//! in process, or a policy step through the launcher), compose the
//! verification report and complete the job with it and the output manifest.
use super::{
    FutureResult, RuntimeError,
    http::{self, LeaseHeaders},
    worker::{self, JobResult, Resources, Session},
};
use crate::{
    cancellation::CancellationEvent,
    gates::Control,
    policy::{StepPolicy, StockPolicy},
    verification::{self, ReportError},
};
use cannery_core::{
    contracts::{ContractKind, ContractValidator, phases::PhaseSchemas},
    json::{self, Document},
    principal::Secret,
};
use cannery_research::{
    science::{RenderingContext, Science},
    steps::{self, Role},
};
use num_traits::ToPrimitive;
use reqwest::Method;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

const DEPTH: usize = 256;

/// The policy a verifier applies, under the revision the science revision registers.
pub enum AppliedPolicy {
    Stock(Arc<StockPolicy>),
    Step(Arc<StepPolicy>),
}
impl AppliedPolicy {
    fn verifier(&self) -> (&str, &str) {
        match self {
            Self::Stock(policy) => (policy.verifier_id.as_str(), policy.revision.as_str()),
            Self::Step(policy) => (policy.verifier_id.as_str(), policy.revision.as_str()),
        }
    }
}

/// Why a verify job failed: the error, a code other than the error's own,
/// and a reason other than its sanitized text.
struct Failure {
    error: RuntimeError,
    code: Option<&'static str>,
    reason: Option<String>,
}
impl From<RuntimeError> for Failure {
    fn from(error: RuntimeError) -> Self {
        Self {
            error,
            code: None,
            reason: None,
        }
    }
}
fn mismatch() -> Failure {
    Failure {
        error: RuntimeError::Contract,
        code: Some("policy_mismatch"),
        reason: None,
    }
}

pub struct Verifier {
    resources: Resources,
    policy: AppliedPolicy,
    schemas: PhaseSchemas,
}
impl Verifier {
    /// # Errors
    /// Validate the local policy and the report schema before any claim.
    pub fn new(resources: Resources, policy: AppliedPolicy) -> Result<Self, RuntimeError> {
        if let AppliedPolicy::Step(step) = &policy {
            let contracts = ContractValidator::new().map_err(|_| RuntimeError::Configuration)?;
            if !contracts.is_valid(ContractKind::StepManifest, &step.document) {
                return Err(RuntimeError::Configuration);
            }
        }
        Ok(Self {
            resources,
            policy,
            schemas: PhaseSchemas::new().map_err(|_| RuntimeError::Configuration)?,
        })
    }
    /// # Errors
    /// Claims exactly one job of this policy revision; 409 means idle. All
    /// other failures are explicit.
    #[allow(clippy::too_many_lines)] // One authoritative lease/heartbeat/cleanup scope.
    pub async fn run_once(
        &self,
        cancel: CancellationEvent,
    ) -> Result<Option<JobResult>, RuntimeError> {
        if cancel.is_set() {
            return Err(RuntimeError::Cancelled);
        }
        let worker = &self.resources;
        let (_, revision) = self.policy.verifier();
        let api = format!("/api/projects/{}", worker.project);
        let claimed = http::cancelled(&cancel, RuntimeError::Cancelled, async {
            let response = worker
                .client
                .request(
                    Method::POST,
                    &format!("{api}/jobs/claims"),
                    &worker.token,
                    None,
                    Some(&json!({"phase":"verify","revision":revision})),
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
        let id = worker::string(job, "job_id")?.to_owned();
        uuid::Uuid::parse_str(&id).map_err(|_| RuntimeError::Contract)?;
        let generation = job["lease"]["generation"]
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or(RuntimeError::Contract)?
            .to_string();
        let lease = LeaseHeaders {
            token: Secret::new(worker::string(&job["lease"], "token")?.to_owned()),
            generation,
            idempotency: None,
        };
        let base = format!("{api}/jobs/{id}");
        let heartbeat = worker::positive(&claim["heartbeat_seconds"])?;
        let expiry = worker::instant(worker::string(&job["lease"], "expires_at")?)?;
        let work = worker
            .work_root
            .join(format!("cr-verify-{}", worker::random_name()?));
        tokio::fs::create_dir_all(&worker.work_root).await?;
        tokio::fs::create_dir(&work).await?;
        let lost = CancellationEvent::new();
        let done = CancellationEvent::new();
        let _done_guard = worker::DoneGuard(done.clone());
        let renewal = tokio::spawn(worker::heartbeat_loop(
            worker.client.clone(),
            Secret::new(worker.token.expose().to_owned()),
            LeaseHeaders {
                token: Secret::new(lease.token.expose().to_owned()),
                generation: lease.generation.clone(),
                idempotency: None,
            },
            base.clone(),
            heartbeat,
            expiry,
            lost.clone(),
            done.clone(),
        ));
        let forwarding = {
            let cancel = cancel.clone();
            let lost = lost.clone();
            let done = done.clone();
            tokio::spawn(async move {
                tokio::select! { () = cancel.wait() => lost.set(), () = done.wait() => {} }
            })
        };
        let mut session = Session::verify(
            worker,
            &claim,
            job,
            &api,
            &base,
            &lease,
            &work,
            lost.clone(),
        );
        let verified = self.verify(&mut session, job).await;
        let delivery = async {
            if cancel.is_set() {
                Err(RuntimeError::Cancelled)
            } else if lost.is_set() {
                Ok("abandoned".to_owned())
            } else {
                let (step, logs) = session.failure();
                match verified {
                    Ok((document, objects)) => {
                        let completion = LeaseHeaders {
                            token: Secret::new(lease.token.expose().to_owned()),
                            generation: lease.generation.clone(),
                            idempotency: Some(format!("complete-{id}-{}", lease.generation)),
                        };
                        let body = http::completion(job, &document, &objects);
                        match self.complete(&base, &completion, &body, &lost).await {
                            Ok(state) => Ok(state),
                            Err(error) => {
                                self.report_failure(&base, &lease, &id, error.into(), None, &[])
                                    .await
                            }
                        }
                    }
                    Err(failure) => {
                        self.report_failure(&base, &lease, &id, failure, step, logs)
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
        worker.backend.release_job(&id).await?;
        let owned = work.clone();
        tokio::task::spawn_blocking(move || crate::removal::remove_tree(&owned))
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
    /// Run the job and compose its report: the document and the output
    /// manifest objects.
    async fn verify(
        &self,
        session: &mut Session<'_>,
        job: &Value,
    ) -> Result<(String, Vec<Value>), Failure> {
        let (verifier, revision) = self.policy.verifier();
        if job["performer"] != "runner"
            || job["verifier"] != json!({"id":verifier,"revision":revision})
        {
            return Err(mismatch());
        }
        if let Some(resume) = job.get("resume").filter(|resume| !resume.is_null()) {
            let from = worker::string(resume, "from_step")?;
            let known = job["steps"]
                .as_array()
                .is_some_and(|steps| steps.iter().any(|step| step["name"] == from))
                || matches!(&self.policy, AppliedPolicy::Step(step) if step.name == from);
            if !known {
                return Err(RuntimeError::Contract.into());
            }
        }
        let science = session.science().await?;
        if let AppliedPolicy::Step(policy) = &self.policy {
            Self::check_step(policy, job, &science)?;
        }
        let evidence = session.run_steps().await?;
        let verdict = match &self.policy {
            AppliedPolicy::Stock(policy) => {
                let control = job
                    .get("control")
                    .filter(|control| !control.is_null())
                    .map(|control| {
                        Ok::<_, RuntimeError>(Control {
                            id: worker::string(control, "id")?.to_owned(),
                            revision: worker::string(control, "revision")?.to_owned(),
                        })
                    })
                    .transpose()?;
                verification::stock_assessment(policy, &science, &evidence, control.as_ref(), DEPTH)
                    .map_err(|_| {
                        session.blame(session.evidence_step());
                        RuntimeError::InvalidStepOutput
                    })?
            }
            AppliedPolicy::Step(policy) => {
                let manifest = value(&policy.document)?;
                let step =
                    json!({"name":policy.name,"revision":policy.revision,"manifest":manifest});
                let verdict = session.run_policy(&step).await?;
                session.upload_policy(&manifest, &policy.name).await?;
                verdict
            }
        };
        let document =
            verification::compose(job, revision, &evidence, &verdict, &science, &self.schemas)
                .map_err(|error| match error {
                    ReportError::Evidence => {
                        session.blame(session.evidence_step());
                        RuntimeError::InvalidStepOutput
                    }
                    ReportError::Verdict => {
                        if let AppliedPolicy::Step(policy) = &self.policy {
                            session.blame(Some(policy.name.clone()));
                            RuntimeError::InvalidStepOutput
                        } else {
                            session.blame(None);
                            RuntimeError::InvalidOutput
                        }
                    }
                    ReportError::Report => {
                        session.blame(None);
                        RuntimeError::InvalidOutput
                    }
                })?;
        session.blame(None);
        Ok((document, session.manifest_objects()?))
    }
    /// A policy step must fit the science revision and its policy allowance
    /// (`limits.max_deadline_seconds`, with 30 seconds kept to report).
    fn check_step(policy: &StepPolicy, job: &Value, science: &Value) -> Result<(), Failure> {
        let revision = worker::science_revision(&job["science_revision"])?;
        let content = document(science)?;
        let rendering = RenderingContext {
            nesting_budget: DEPTH,
        };
        let view = Science::new(revision.into(), &content, rendering)
            .map_err(|_| RuntimeError::Contract)?;
        steps::check_step(
            &view,
            &policy.document,
            Role::Policy,
            &String::from("/step"),
            rendering,
        )
        .map_err(|_| RuntimeError::Contract)?;
        let manifest = value(&policy.document)?;
        let step = worker::positive(&manifest["spec"]["activeDeadlineSeconds"])?;
        let setup = steps::setup_deadline(&policy.document)
            .map_err(|_| RuntimeError::Contract)?
            .map_or(Some(0.0), |seconds| seconds.to_f64())
            .filter(|n| n.is_finite() && *n >= 0.0)
            .ok_or(RuntimeError::Contract)?;
        let budget = science["limits"]
            .get("max_deadline_seconds")
            .map_or(Ok(3600.0), worker::positive)?;
        if step + setup + 30.0 > budget {
            let parts = if setup == 0.0 {
                format!("{step:.0}s for the step")
            } else {
                format!("{step:.0}s for the step, {setup:.0}s for its setup")
            };
            return Err(Failure {
                error: RuntimeError::DeadlineExceeded,
                code: None,
                reason: Some(format!(
                    "policy step {} needs up to {:.0}s ({parts}, 30s kept by the runner) but a verify job of science revision {revision} allows its policy {budget:.0}s (max_deadline_seconds)",
                    policy.name,
                    step + setup + 30.0
                )),
            });
        }
        Ok(())
    }
    async fn complete(
        &self,
        base: &str,
        lease: &LeaseHeaders,
        body: &Value,
        lost: &CancellationEvent,
    ) -> Result<String, RuntimeError> {
        let worker = &self.resources;
        // Exact same body/key for an ambiguous response; the API consumes it once.
        for retry in 0..4 {
            if lost.is_set() {
                return Ok("abandoned".into());
            }
            match worker
                .client
                .request(
                    Method::POST,
                    &format!("{base}/completion"),
                    &worker.token,
                    Some(lease),
                    Some(body),
                )
                .await
            {
                Ok(response) => return worker::outcome_state(response, true).await,
                Err(RuntimeError::Transport) if retry < 3 => {
                    tokio::select! {()=lost.wait()=>return Ok("abandoned".into()),()=tokio::time::sleep(Duration::from_millis(500<<retry))=>{}}
                }
                Err(error) => return Err(error),
            }
        }
        Err(RuntimeError::Transport)
    }
    async fn report_failure(
        &self,
        base: &str,
        lease: &LeaseHeaders,
        id: &str,
        failure: Failure,
        step: Option<&str>,
        logs: &[Value],
    ) -> Result<String, RuntimeError> {
        let error = failure.error;
        let code = match (failure.code, error) {
            (Some(code), _) => code,
            (None, RuntimeError::DurablyFailed) => return Ok("failed".to_owned()),
            (None, RuntimeError::LostLease) => return Ok("abandoned".to_owned()),
            (None, error) => failure_code(error),
        };
        let reason = failure.reason.unwrap_or_else(|| error.to_string());
        let mut report = json!({"schema_version":"0.2","job_id":id,"error_code":code,"reason":reason,"logs":logs});
        if let Some(step) = step {
            report["step"] = json!(step);
        }
        let worker = &self.resources;
        let mut response = worker
            .client
            .request(
                Method::POST,
                &format!("{base}/failure"),
                &worker.token,
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
            response = worker
                .client
                .request(
                    Method::POST,
                    &format!("{base}/failure"),
                    &worker.token,
                    Some(lease),
                    Some(&report),
                )
                .await;
        }
        match response {
            Ok(response) => worker::outcome_state(response, false)
                .await
                .or(Ok("unreported".to_owned())),
            Err(_) => Ok("unreported".to_owned()),
        }
    }
}
/// The failure code a runner reports for a step session error.
#[must_use]
pub const fn failure_code(error: RuntimeError) -> &'static str {
    match error {
        RuntimeError::Integrity => "input_verification_failed",
        RuntimeError::StepFailed => "step_failed",
        RuntimeError::SetupFailed => "setup_failed",
        RuntimeError::CodeNotAllowed => "code_not_allowed",
        RuntimeError::InvalidCode => "invalid_code",
        RuntimeError::DeadlineExceeded => "deadline_exceeded",
        RuntimeError::InvalidStepOutput => "invalid_step_output",
        RuntimeError::InvalidOutput => "invalid_output",
        RuntimeError::UploadExpired => "upload_expired",
        RuntimeError::MissingOutput => "missing_output",
        _ => "runner_error",
    }
}
fn document(value: &Value) -> Result<Document, RuntimeError> {
    json::decode(&serde_json::to_vec(value)?, DEPTH).map_err(|_| RuntimeError::Contract)
}
fn value(document: &Document) -> Result<Value, RuntimeError> {
    serde_json::from_slice(&json::encode_http(document, DEPTH).map_err(|_| RuntimeError::Contract)?)
        .map_err(|_| RuntimeError::Contract)
}
impl super::process::RuntimeWorker for Verifier {
    fn run_once(&self, cancel: CancellationEvent) -> FutureResult<'_, Option<JobResult>> {
        Box::pin(Verifier::run_once(self, cancel))
    }
    fn close(&self) -> FutureResult<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
