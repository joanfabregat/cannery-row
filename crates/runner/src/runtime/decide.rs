//! The decide kind: claim a decide job registered to this decider and the
//! revision of its step, run the decider step on the decider's context
//! bundle (`/cr/context/context.md`), and complete the job with the decision
//! document the step wrote to `/cr/outputs/decision/decision.md`. The runner
//! sets the document's `verification` and `writeup` to what the job names,
//! so the step only decides: its outcome, and the reason as the body.
use super::{
    FutureResult, RuntimeError,
    http::{self, LeaseHeaders},
    verify::failure_code,
    worker::{self, JobResult, Resources, Session},
};
use crate::{cancellation::CancellationEvent, policy::DeciderStep};
use cannery_core::{
    contracts::{ContractKind, ContractValidator},
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

/// Why a decide job failed: the error, and a reason other than its sanitized text.
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

pub struct Decider {
    resources: Resources,
    step: Arc<DeciderStep>,
}
impl Decider {
    /// # Errors
    /// Validate the local decider step before any claim.
    pub fn new(resources: Resources, step: Arc<DeciderStep>) -> Result<Self, RuntimeError> {
        let contracts = ContractValidator::new().map_err(|_| RuntimeError::Configuration)?;
        if !contracts.is_valid(ContractKind::StepManifest, &step.document) {
            return Err(RuntimeError::Configuration);
        }
        Ok(Self { resources, step })
    }
    /// # Errors
    /// Claims exactly one job of this decider revision; 409 means idle. All
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
        let api = format!("/api/projects/{}", worker.project);
        let claimed = http::cancelled(&cancel, RuntimeError::Cancelled, async {
            let response = worker
                .client
                .request(
                    Method::POST,
                    &format!("{api}/jobs/claims"),
                    &worker.token,
                    None,
                    Some(&json!({"phase":"decide","revision":self.step.revision})),
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
            .join(format!("cr-decide-{}", worker::random_name()?));
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
        let mut session = Session::decide(
            worker,
            &claim,
            job,
            &api,
            &base,
            &lease,
            &work,
            lost.clone(),
        );
        let decided = self.decide(&mut session, job).await;
        let delivery = async {
            if cancel.is_set() {
                Err(RuntimeError::Cancelled)
            } else if lost.is_set() {
                Ok("abandoned".to_owned())
            } else {
                let (step, logs) = session.failure();
                match decided {
                    Ok(document) => {
                        let completion = LeaseHeaders {
                            token: Secret::new(lease.token.expose().to_owned()),
                            generation: lease.generation.clone(),
                            idempotency: Some(format!("complete-{id}-{}", lease.generation)),
                        };
                        let body = http::decision(job, &document);
                        match self.complete(&base, &completion, &body, &lost).await {
                            Ok(state) => Ok(state),
                            Err(failure) if failure.error == RuntimeError::InvalidStepOutput => {
                                let step = Some(self.step.name.as_str());
                                self.report_failure(&base, &lease, &id, failure, step, logs)
                                    .await
                            }
                            Err(failure) => {
                                self.report_failure(&base, &lease, &id, failure, None, &[])
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
    /// Run the decider step and compose the decision document: its front
    /// matter, citing what the job names, and its body.
    async fn decide(&self, session: &mut Session<'_>, job: &Value) -> Result<String, Failure> {
        if job["performer"] != "runner"
            || job["decider"] != json!({"id":self.step.decider_id,"revision":self.step.revision})
        {
            return Err(Failure {
                error: RuntimeError::Contract,
                code: Some("decider_mismatch"),
                reason: None,
            });
        }
        let science = session.science().await?;
        self.check_step(job, &science)?;
        let manifest = value(&self.step.document)?;
        let step = json!({"name":self.step.name,"revision":self.step.revision,"manifest":manifest});
        let decision = session.run_decider(&step).await?;
        let mut front_matter = decision["front_matter"].clone();
        let fields = front_matter
            .as_object_mut()
            .ok_or(RuntimeError::InvalidStepOutput)?;
        for name in ["verification", "writeup"] {
            fields.insert(
                name.to_owned(),
                job["inputs"].get(name).cloned().unwrap_or(Value::Null),
            );
        }
        let body = decision["body"].as_str().ok_or(RuntimeError::Contract)?;
        Ok(format!(
            "---\n{}\n---\n{body}",
            serde_json::to_string(&front_matter).map_err(|_| RuntimeError::Contract)?
        ))
    }
    /// The decider step must fit the science revision and the decide job's
    /// allowance (`limits.max_deadline_seconds`, with 30 seconds kept to report).
    fn check_step(&self, job: &Value, science: &Value) -> Result<(), Failure> {
        let step = &self.step;
        let revision = worker::science_revision(&job["science_revision"])?;
        let content = document(science)?;
        let rendering = RenderingContext {
            nesting_budget: DEPTH,
        };
        let view = Science::new(revision.into(), &content, rendering)
            .map_err(|_| RuntimeError::Contract)?;
        steps::check_step(
            &view,
            &step.document,
            Role::Decider,
            &String::from("/step"),
            rendering,
        )
        .map_err(|_| RuntimeError::Contract)?;
        let manifest = value(&step.document)?;
        let seconds = worker::positive(&manifest["spec"]["activeDeadlineSeconds"])?;
        let setup = steps::setup_deadline(&step.document)
            .map_err(|_| RuntimeError::Contract)?
            .map_or(Some(0.0), |seconds| seconds.to_f64())
            .filter(|n| n.is_finite() && *n >= 0.0)
            .ok_or(RuntimeError::Contract)?;
        let budget = science["limits"]
            .get("max_deadline_seconds")
            .map_or(Ok(3600.0), worker::positive)?;
        if seconds + setup + 30.0 > budget {
            return Err(Failure {
                error: RuntimeError::DeadlineExceeded,
                code: None,
                reason: Some(format!(
                    "decider step {} needs up to {:.0}s (30s kept by the runner) but a decide job of science revision {revision} allows it {budget:.0}s (max_deadline_seconds)",
                    step.name,
                    seconds + setup + 30.0
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
    ) -> Result<String, Failure> {
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
                // A refused decision document is the step's fault: the job fails with its reason.
                Ok(response) if response.status().as_u16() == 422 => {
                    let refusal = http::read_json(response).await?;
                    let message = refusal["error"]["message"]
                        .as_str()
                        .unwrap_or("invalid decision");
                    return Err(Failure {
                        error: RuntimeError::InvalidStepOutput,
                        code: None,
                        reason: Some(format!("the decision document was refused: {message}")),
                    });
                }
                Ok(response) => return Ok(worker::outcome_state(response, true).await?),
                Err(RuntimeError::Transport) if retry < 3 => {
                    tokio::select! {()=lost.wait()=>return Ok("abandoned".into()),()=tokio::time::sleep(Duration::from_millis(500<<retry))=>{}}
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(RuntimeError::Transport.into())
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
fn document(value: &Value) -> Result<Document, RuntimeError> {
    json::decode(&serde_json::to_vec(value)?, DEPTH).map_err(|_| RuntimeError::Contract)
}
fn value(document: &Document) -> Result<Value, RuntimeError> {
    serde_json::from_slice(&json::encode_http(document, DEPTH).map_err(|_| RuntimeError::Contract)?)
        .map_err(|_| RuntimeError::Contract)
}
impl super::process::RuntimeWorker for Decider {
    fn run_once(&self, cancel: CancellationEvent) -> FutureResult<'_, Option<JobResult>> {
        Box::pin(Decider::run_once(self, cancel))
    }
    fn close(&self) -> FutureResult<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
