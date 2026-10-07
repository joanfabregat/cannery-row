//! Leased policy-step evaluation through the common owned step engine.
use super::{
    FutureResult, RuntimeError,
    http::{self, LeaseHeaders},
    worker::{self, JobResult, Tester},
};
use crate::{
    cancellation::CancellationEvent,
    evaluator_inputs::{self, EvaluationError},
    policy::StepPolicy,
};
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
type Failure = (&'static str, RuntimeError, Option<String>);
const DEPTH: usize = 256;
pub struct PolicyEvaluator {
    resources: Tester,
    policy: Arc<StepPolicy>,
    contracts: ContractValidator,
}
impl PolicyEvaluator {
    /// # Errors
    /// Validate the local policy before any claim or backend work.
    pub fn new(resources: Tester, policy: Arc<StepPolicy>) -> Result<Self, RuntimeError> {
        let contracts = ContractValidator::new().map_err(|_| RuntimeError::Configuration)?;
        if !contracts.is_valid(ContractKind::StepManifest, &policy.document) {
            return Err(RuntimeError::Configuration);
        }
        Ok(Self {
            resources,
            policy,
            contracts,
        })
    }
    /// # Errors
    /// Claim one policy revision and settle its process, transfers and mounted roots.
    #[allow(clippy::too_many_lines)] // One authoritative claim, heartbeat, execution and cleanup owner.
    pub async fn run_once(
        &self,
        cancel: CancellationEvent,
    ) -> Result<Option<JobResult>, RuntimeError> {
        if cancel.is_set() {
            return Err(RuntimeError::Cancelled);
        }
        let worker = &self.resources;
        let revision = self
            .policy
            .revision
            .as_utf8()
            .ok_or(RuntimeError::Configuration)?;
        let api = format!("/api/projects/{}", worker.project);
        let claimed = http::cancelled(&cancel, RuntimeError::Cancelled, async {
            let response = worker
                .client
                .request(
                    Method::POST,
                    &format!("{api}/jobs/claims"),
                    &worker.token,
                    None,
                    Some(&json!({"stage":"evaluator","revision":revision})),
                )
                .await?;
            if response.status().as_u16() == 409 {
                return Ok(None);
            }
            Ok(Some(http::json_response(response).await?))
        })
        .await?;
        let Some(mut claim) = claimed else {
            return Ok(None);
        };
        let job = claim.get_mut("job").ok_or(RuntimeError::Contract)?;
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
        let interval = worker::positive(&claim["heartbeat_seconds"])?;
        let expiry = worker::instant(worker::string(&claim["job"]["lease"], "expires_at")?)?;
        let base = format!("{api}/jobs/{id}");
        let work = worker
            .work_root
            .join(format!("cr-policy-{}", worker::random_name()?));
        tokio::fs::create_dir_all(&worker.work_root).await?;
        tokio::fs::create_dir(&work).await?;
        let lost = CancellationEvent::new();
        let done = CancellationEvent::new();
        let _guard = worker::DoneGuard(done.clone());
        let renewal = tokio::spawn(worker::heartbeat_loop(
            worker.client.clone(),
            Secret::new(worker.token.expose().to_owned()),
            LeaseHeaders {
                token: Secret::new(lease.token.expose().to_owned()),
                generation: lease.generation.clone(),
                idempotency: None,
            },
            base.clone(),
            interval,
            expiry,
            lost.clone(),
            done.clone(),
        ));
        let forwarding = {
            let cancel = cancel.clone();
            let lost = lost.clone();
            let done = done.clone();
            tokio::spawn(async move {
                tokio::select! {()=cancel.wait()=>lost.set(),()=done.wait()=>{}}
            })
        };
        let started = chrono::Utc::now().to_rfc3339();
        let mut logs = vec![];
        let evaluated = async {
            let original = &claim["job"];
            let inputs = http::cancelled(
                &lost,
                ("evaluator_error", RuntimeError::LostLease, None),
                self.inputs(&api, &base, original, &lease),
            )
            .await?;
            let manifest = value(&self.policy.document).map_err(eval_error)?;
            let mut job = original.clone();
            let name = self.policy.name.as_utf8().ok_or((
                "evaluator_error",
                RuntimeError::Configuration,
                None,
            ))?;
            job["steps"] = json!([{"name":name,"revision":revision,"manifest":manifest}]);
            let mut session = worker::Session::evaluator(
                worker,
                &claim,
                &job,
                &api,
                &base,
                &lease,
                &work,
                lost.clone(),
                &inputs,
            );
            let assessment = session.run_steps().await;
            logs = session.failure().1.to_vec();
            let assessment = assessment.map_err(eval_error)?;
            let record = evaluator_inputs::policy_record(
                &self.policy,
                &document(original).map_err(eval_error)?,
                &document(&inputs.science["content"]).map_err(eval_error)?,
                &document(&inputs.evidence["served"]).map_err(eval_error)?,
                &document(&assessment).map_err(eval_error)?,
                &started,
                &chrono::Utc::now().to_rfc3339(),
                DEPTH,
                &self.contracts,
            )
            .map_err(|error| match error {
                EvaluationError::PolicyMismatch => {
                    ("policy_mismatch", RuntimeError::Contract, None)
                }
                EvaluationError::EvidenceMismatch => {
                    ("input_verification_failed", RuntimeError::Integrity, None)
                }
                _ => ("evaluator_error", RuntimeError::InvalidOutput, None),
            })?;
            session
                .upload_policy(&manifest, &name)
                .await
                .map_err(eval_error)?;
            Ok::<_, Failure>((
                value(&record).map_err(eval_error)?,
                session.objects().to_vec(),
            ))
        }
        .await;
        let delivery = async {
            if cancel.is_set() {
                Err(RuntimeError::Cancelled)
            } else if lost.is_set() {
                Ok("abandoned".into())
            } else {
                match evaluated {
                    Ok((record, objects)) => {
                        let completion = LeaseHeaders {
                            token: Secret::new(lease.token.expose().to_owned()),
                            generation: lease.generation.clone(),
                            idempotency: Some(format!("evaluate-{id}-{}", lease.generation)),
                        };
                        let body = http::completion(&claim["job"], &record, &objects);
                        match self.complete(&base, &completion, &body, &lost).await {
                            Ok(state) => Ok(state),
                            Err(error) => {
                                self.fail(&base, &id, &lease, eval_error(error), &logs)
                                    .await
                            }
                        }
                    }
                    Err(failure) => self.fail(&base, &id, &lease, failure, &logs).await,
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
    async fn inputs(
        &self,
        api: &str,
        base: &str,
        job: &Value,
        lease: &LeaseHeaders,
    ) -> Result<worker::EvaluatorInputs, Failure> {
        if job["evaluator"]["id"].as_str() != self.policy.evaluator_id.as_utf8().as_deref()
            || job["evaluator"]["revision"].as_str() != self.policy.revision.as_utf8().as_deref()
        {
            return Err(("policy_mismatch", RuntimeError::Contract, None));
        }
        let revision = worker::science_revision(&job["science_revision"]).map_err(eval_error)?;
        let science = self
            .get(&format!("{api}/config/science/{revision}"), None)
            .await
            .map_err(eval_error)?;
        if science["revision"].as_u64() != Some(revision) {
            return Err(("input_verification_failed", RuntimeError::Integrity, None));
        }
        let content = document(&science["content"]).map_err(eval_error)?;
        let rendering = RenderingContext {
            nesting_budget: DEPTH,
        };
        let view = Science::new(revision.into(), &content, rendering)
            .map_err(|_| ("evaluator_error", RuntimeError::Contract, None))?;
        steps::check_step(
            &view,
            &self.policy.document,
            Role::Evaluator,
            &String::from("/step"),
            rendering,
        )
        .map_err(|_| ("evaluator_error", RuntimeError::Contract, None))?;
        let manifest = value(&self.policy.document).map_err(eval_error)?;
        let step =
            worker::positive(&manifest["spec"]["activeDeadlineSeconds"]).map_err(eval_error)?;
        let setup = steps::setup_deadline(&self.policy.document)
            .map_err(|_| ("evaluator_error", RuntimeError::Contract, None))?
            .map_or(Some(0.0), |seconds| seconds.to_f64())
            .filter(|n| n.is_finite() && *n >= 0.0)
            .ok_or(("evaluator_error", RuntimeError::Contract, None))?;
        let budget = science["content"]["limits"]
            .get("max_deadline_seconds")
            .map_or(Ok(3600.0), worker::positive)
            .map_err(eval_error)?;
        if step + setup + 30.0 > budget {
            return Err((
                "evaluator_error",
                RuntimeError::DeadlineExceeded,
                Some(self.headroom_reason(revision, step, setup, budget)?),
            ));
        }
        if worker::time_left(
            worker::instant(worker::string(job, "deadline").map_err(eval_error)?)
                .map_err(eval_error)?,
        ) < step + setup + 30.0
        {
            return Err(("evaluator_error", RuntimeError::DeadlineExceeded, None));
        }
        let served = self
            .get(&format!("{base}/inputs/evidence"), Some(lease))
            .await
            .map_err(eval_error)?;
        let refs = &job["inputs"]["evidence"];
        let records = evaluator_inputs::verified_records(
            &document(refs).map_err(eval_error)?,
            &document(&served).map_err(eval_error)?,
            DEPTH,
        )
        .map_err(|_| ("input_verification_failed", RuntimeError::Integrity, None))?;
        let refs = refs
            .as_array()
            .ok_or(("evaluator_error", RuntimeError::Contract, None))?;
        let evidence=refs.iter().zip(records.iter()).map(|(reference,record)|Ok(json!({"ref":reference["ref"],"sha256":reference["sha256"],"record":value(record)?}))).collect::<Result<Vec<_>,RuntimeError>>().map_err(eval_error)?;
        let tester_manifest = self
            .get(&format!("{base}/inputs/manifest"), Some(lease))
            .await
            .map_err(eval_error)?;
        if worker::canonical_digest(&tester_manifest).map_err(eval_error)?
            != worker::string(&job["inputs"]["manifest"], "sha256").map_err(eval_error)?
        {
            return Err(("input_verification_failed", RuntimeError::Integrity, None));
        }
        let metrics = science["content"]
            .get("metrics")
            .cloned()
            .unwrap_or_else(|| json!([]));
        Ok(worker::EvaluatorInputs {
            science,
            evidence: json!({"staged":evidence,"served":served}),
            manifest: tester_manifest,
            metrics,
        })
    }
    fn headroom_reason(
        &self,
        revision: u64,
        step: f64,
        setup: f64,
        budget: f64,
    ) -> Result<String, Failure> {
        let name = self.policy.name.as_utf8().ok_or((
            "evaluator_error",
            RuntimeError::Configuration,
            None,
        ))?;
        let parts = if setup == 0.0 {
            format!("{step:.0}s for the step")
        } else {
            format!("{step:.0}s for the step, {setup:.0}s for its setup")
        };
        Ok(format!(
            "policy step {name} needs up to {:.0}s ({parts}, 30s kept by the runner) but an evaluation job of science revision {revision} has {budget:.0}s (max_deadline_seconds)",
            step + setup + 30.0
        ))
    }
    async fn get(&self, path: &str, lease: Option<&LeaseHeaders>) -> Result<Value, RuntimeError> {
        http::json_response(
            self.resources
                .client
                .request(Method::GET, path, &self.resources.token, lease, None)
                .await?,
        )
        .await
    }
    async fn complete(
        &self,
        base: &str,
        lease: &LeaseHeaders,
        body: &Value,
        lost: &CancellationEvent,
    ) -> Result<String, RuntimeError> {
        for retry in 0..4 {
            if lost.is_set() {
                return Ok("abandoned".into());
            }
            match self
                .resources
                .client
                .request(
                    Method::POST,
                    &format!("{base}/completion"),
                    &self.resources.token,
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
    async fn fail(
        &self,
        base: &str,
        id: &str,
        lease: &LeaseHeaders,
        (code, error, reason): Failure,
        logs: &[Value],
    ) -> Result<String, RuntimeError> {
        if error == RuntimeError::LostLease {
            return Ok("abandoned".into());
        }
        let reason = reason.unwrap_or_else(|| error.to_string());
        let mut body = json!({"schema_version":"0.2","job_id":id,"error_code":code,"reason":reason,"logs":logs});
        let worker = &self.resources;
        let mut response = worker
            .client
            .request(
                Method::POST,
                &format!("{base}/failure"),
                &worker.token,
                Some(lease),
                Some(&body),
            )
            .await;
        if !logs.is_empty() && response.as_ref().is_ok_and(|r| r.status().as_u16() == 422) {
            body["logs"] = json!([]);
            response = worker
                .client
                .request(
                    Method::POST,
                    &format!("{base}/failure"),
                    &worker.token,
                    Some(lease),
                    Some(&body),
                )
                .await;
        }
        match response {
            Ok(response) => worker::outcome_state(response, false)
                .await
                .or(Ok("unreported".into())),
            Err(_) => Ok("unreported".into()),
        }
    }
}
fn eval_error(error: RuntimeError) -> Failure {
    (
        if error == RuntimeError::Integrity {
            "input_verification_failed"
        } else {
            "evaluator_error"
        },
        error,
        None,
    )
}
fn document(value: &Value) -> Result<Document, RuntimeError> {
    json::decode(&serde_json::to_vec(value)?, DEPTH).map_err(|_| RuntimeError::Contract)
}
fn value(document: &Document) -> Result<Value, RuntimeError> {
    serde_json::from_slice(&json::encode_http(document, DEPTH).map_err(|_| RuntimeError::Contract)?)
        .map_err(|_| RuntimeError::Contract)
}
impl super::process::RuntimeWorker for PolicyEvaluator {
    fn run_once(&self, cancel: CancellationEvent) -> FutureResult<'_, Option<JobResult>> {
        Box::pin(PolicyEvaluator::run_once(self, cancel))
    }
    fn close(&self) -> FutureResult<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
