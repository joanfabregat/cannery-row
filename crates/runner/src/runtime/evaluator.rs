//! Stock evaluator: leased HTTP reads, pinned evidence and native exact gates.
use super::{
    FutureResult, RuntimeError,
    http::{self, ApiClient, LeaseHeaders},
    process::RuntimeWorker,
    worker::{JobResult, heartbeat_loop, instant, outcome_state, string},
};
use crate::{
    cancellation::CancellationEvent,
    evaluator_inputs::{self, EvaluationError},
    policy::StockPolicy,
};
use cannery_core::{
    json::{self, Document},
    principal::Secret,
};
use reqwest::Method;
use serde_json::{Value, json};
use std::sync::Arc;

const DEPTH: usize = 256;
pub struct Evaluator {
    pub client: ApiClient,
    pub token: Secret,
    pub project: String,
    pub policy: Arc<StockPolicy>,
}
impl RuntimeWorker for Evaluator {
    fn run_once(&self, cancel: CancellationEvent) -> FutureResult<'_, Option<JobResult>> {
        Box::pin(self.run_once(cancel))
    }
    fn close(&self) -> FutureResult<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
struct Renewal {
    done: CancellationEvent,
    handle: tokio::task::JoinHandle<()>,
}
impl Drop for Renewal {
    fn drop(&mut self) {
        self.done.set();
        self.handle.abort();
    }
}
impl Evaluator {
    /// Claim one job for this exact policy revision and settle its lease.
    /// # Errors
    /// Returns sanitized configuration and transport failures; idle is `None`.
    #[allow(
        clippy::too_many_lines,
        reason = "One claim owns renewal and reporting"
    )]
    pub async fn run_once(
        &self,
        cancel: CancellationEvent,
    ) -> Result<Option<JobResult>, RuntimeError> {
        if cancel.is_set() {
            return Err(RuntimeError::Cancelled);
        }
        let api = format!("/api/projects/{}", self.project);
        let revision = self
            .policy
            .revision
            .as_utf8()
            .ok_or(RuntimeError::Configuration)?;
        let claimed = http::cancelled(&cancel, RuntimeError::Cancelled, async {
            let claim = self
                .client
                .request(
                    Method::POST,
                    &format!("{api}/jobs/claims"),
                    &self.token,
                    None,
                    Some(&json!({"stage":"evaluator","revision":revision})),
                )
                .await?;
            if claim.status().as_u16() == 409 {
                return Ok(None);
            }
            Ok(Some(http::json_response(claim).await?))
        })
        .await?;
        let Some(claim) = claimed else {
            return Ok(None);
        };
        let job = claim.get("job").ok_or(RuntimeError::Contract)?;
        let id = string(job, "job_id")?.to_owned();
        uuid::Uuid::parse_str(&id).map_err(|_| RuntimeError::Contract)?;
        let generation = job["lease"]["generation"]
            .as_u64()
            .filter(|v| *v > 0)
            .ok_or(RuntimeError::Contract)?
            .to_string();
        let lease = LeaseHeaders {
            token: Secret::new(string(&job["lease"], "token")?.to_owned()),
            generation,
            idempotency: None,
        };
        let interval = claim["heartbeat_seconds"]
            .as_f64()
            .filter(|v| v.is_finite() && *v > 0.0 && *v <= 86_400.0)
            .ok_or(RuntimeError::Contract)?;
        let expiry = instant(string(&job["lease"], "expires_at")?)?;
        let base = format!("{api}/jobs/{id}");
        let lost = CancellationEvent::new();
        let done = CancellationEvent::new();
        let client = self.client.clone();
        let token = Secret::new(self.token.expose().to_owned());
        let renew_lease = LeaseHeaders {
            token: Secret::new(lease.token.expose().to_owned()),
            generation: lease.generation.clone(),
            idempotency: None,
        };
        let renewal = Renewal {
            done: done.clone(),
            handle: tokio::spawn(heartbeat_loop(
                client,
                token,
                renew_lease,
                base.clone(),
                interval,
                expiry,
                lost.clone(),
                done.clone(),
            )),
        };
        let record = tokio::select! {
            biased;
            () = cancel.wait() => return Err(RuntimeError::Cancelled),
            () = lost.wait() => return Ok(Some(JobResult { job_id: id, state: "abandoned".to_owned() })),
            record = self.record(&api, &base, job, &lease) => record,
        };
        // Completion/failure consumes the lease; its response decides the result.
        done.set();
        let delivery = async {
            Ok::<_, RuntimeError>(match record {
                Ok(record) => {
                    let completion = LeaseHeaders {
                        token: Secret::new(lease.token.expose().to_owned()),
                        generation: lease.generation.clone(),
                        idempotency: Some(format!("evaluate-{id}-{}", lease.generation)),
                    };
                    let response = self
                        .client
                        .request(
                            Method::POST,
                            &format!("{base}/completion"),
                            &self.token,
                            Some(&completion),
                            Some(&json!({"schema_version":"0.2","job_id":id,"evidence":record})),
                        )
                        .await;
                    match response {
                        Ok(response) => match outcome_state(response, true).await {
                            Ok(state) => state,
                            Err(error) => {
                                self.fail(&base, &id, &lease, "evaluator_error", error)
                                    .await?
                            }
                        },
                        Err(error) => {
                            self.fail(&base, &id, &lease, "evaluator_error", error)
                                .await?
                        }
                    }
                }
                Err((code, error)) => self.fail(&base, &id, &lease, code, error).await?,
            })
        };
        let state = tokio::select! {
            biased;
            () = cancel.wait() => Err(RuntimeError::Cancelled),
            () = lost.wait() => Ok("abandoned".to_owned()),
            state = delivery => state,
        };
        drop(renewal);
        Ok(Some(JobResult {
            job_id: id,
            state: state?,
        }))
    }
    async fn record(
        &self,
        api: &str,
        base: &str,
        job: &Value,
        lease: &LeaseHeaders,
    ) -> Result<Value, (&'static str, RuntimeError)> {
        let expected_id = self
            .policy
            .evaluator_id
            .as_utf8()
            .ok_or(("evaluator_error", RuntimeError::Configuration))?;
        let expected_revision = self
            .policy
            .revision
            .as_utf8()
            .ok_or(("evaluator_error", RuntimeError::Configuration))?;
        if job["evaluator"]["id"] != expected_id
            || job["evaluator"]["revision"] != expected_revision
        {
            return Err(("policy_mismatch", RuntimeError::Contract));
        }
        let started = chrono::Utc::now().to_rfc3339();
        let inputs = async {
            let revision = super::worker::science_revision(&job["science_revision"])?;
            let science = http::json_response(
                self.client
                    .request(
                        Method::GET,
                        &format!("{api}/config/science/{revision}"),
                        &self.token,
                        None,
                        None,
                    )
                    .await?,
            )
            .await?;
            let records = http::json_response(
                self.client
                    .request(
                        Method::GET,
                        &format!("{base}/inputs/evidence"),
                        &self.token,
                        Some(lease),
                        None,
                    )
                    .await?,
            )
            .await?;
            Ok::<_, RuntimeError>((science, records))
        }
        .await
        .map_err(|error| ("evaluator_error", error))?;
        let policy = self.policy.clone();
        let job = document(job)?;
        let science = document(
            inputs
                .0
                .get("content")
                .ok_or(("evaluator_error", RuntimeError::Contract))?,
        )?;
        let records = document(&inputs.1)?;
        tokio::task::spawn_blocking(move || {
            let finished = chrono::Utc::now().to_rfc3339();
            let record = evaluator_inputs::stock_record(
                &policy, &job, &science, &records, &started, &finished, DEPTH,
            )
            .map_err(|error| match error {
                EvaluationError::PolicyMismatch => ("policy_mismatch", RuntimeError::Contract),
                EvaluationError::EvidenceMismatch => {
                    ("input_verification_failed", RuntimeError::Integrity)
                }
                _ => ("evaluator_error", RuntimeError::Contract),
            })?;
            let bytes = json::canonical::bytes(&record, DEPTH)
                .map_err(|_| ("evaluator_error", RuntimeError::Contract))?;
            serde_json::from_slice(&bytes).map_err(|_| ("evaluator_error", RuntimeError::Contract))
        })
        .await
        .map_err(|_| ("evaluator_error", RuntimeError::Contract))?
    }
    async fn fail(
        &self,
        base: &str,
        id: &str,
        lease: &LeaseHeaders,
        code: &str,
        error: RuntimeError,
    ) -> Result<String, RuntimeError> {
        if error == RuntimeError::LostLease {
            return Ok("abandoned".to_owned());
        }
        let response = self
            .client
            .request(
                Method::POST,
                &format!("{base}/failure"),
                &self.token,
                Some(lease),
                Some(
                    &json!({"schema_version":"0.2","job_id":id,"error_code":code,
                "reason":error.to_string(),"logs":[]}),
                ),
            )
            .await?;
        outcome_state(response, false).await
    }
}
fn document(value: &Value) -> Result<Document, (&'static str, RuntimeError)> {
    let bytes =
        serde_json::to_vec(value).map_err(|_| ("evaluator_error", RuntimeError::Contract))?;
    json::decode(&bytes, DEPTH).map_err(|_| ("evaluator_error", RuntimeError::Contract))
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
