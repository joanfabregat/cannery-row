//! Workflow experiment worker using the common owned step engine.
use super::{
    FutureResult, RuntimeError,
    http::{self, LeaseHeaders},
    worker::{self, JobResult, Resources},
};
use crate::cancellation::CancellationEvent;
use cannery_core::principal::Secret;
use reqwest::Method;
use serde_json::{Value, json};
use std::time::Duration;

/// The resource configuration is shared with the verify kind; claims and completion
/// are attempt-specific. The scheduler must cancel and settle an active call.
pub struct Experiment {
    resources: Resources,
}
impl Experiment {
    #[must_use]
    pub const fn new(resources: Resources) -> Self {
        Self { resources }
    }
    /// # Errors
    /// Returns protocol, filesystem or cancellation failures without private values.
    #[allow(clippy::too_many_lines)] // One owned heartbeat/step/cleanup scope.
    pub async fn run_once(
        &self,
        cancel: CancellationEvent,
    ) -> Result<Option<JobResult>, RuntimeError> {
        let worker = &self.resources;
        let api = format!("/api/projects/{}", worker.project);
        let (status, claim) = http::cancelled(&cancel, RuntimeError::Cancelled, async {
            let response = worker
                .client
                .request(
                    Method::POST,
                    &format!("{api}/claims"),
                    &worker.token,
                    None,
                    Some(&json!({"mode":"workflow"})),
                )
                .await?;
            let status = response.status().as_u16();
            let claim = http::read_json(response).await?;
            Ok((status, claim))
        })
        .await?;
        if status == 409
            && matches!(
                claim["error"]["code"].as_str(),
                Some("nothing_to_claim" | "workflow_unavailable" | "concern_open")
            )
        {
            return Ok(None);
        }
        if status != 201 {
            return Err(RuntimeError::Http(status));
        }
        let attempt = &claim["attempt"];
        let id = worker::string(attempt, "id")?.to_owned();
        uuid::Uuid::parse_str(&id).map_err(|_| RuntimeError::Contract)?;
        let number = attempt["number"]
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or(RuntimeError::Contract)?;
        let sequence = attempt["sequence"]
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or(RuntimeError::Contract)?;
        let generation = claim["lease_generation"]
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or(RuntimeError::Contract)?
            .to_string();
        let lease = LeaseHeaders {
            token: Secret::new(worker::string(&claim, "lease_token")?.to_owned()),
            generation,
            idempotency: None,
        };
        let base = format!("{api}/hypotheses/{number}/attempts/{sequence}");
        let heartbeat = worker::positive(&claim["heartbeat_seconds"])?;
        let expiry = worker::instant(worker::string(&claim, "lease_expires_at")?)?;
        let mut job = claim["workflow"].clone();
        if !job.is_object() {
            return Err(RuntimeError::Contract);
        }
        job["job_id"] = json!(id);
        job["lease"] = json!({"token":lease.token.expose(),"generation":claim["lease_generation"],"expires_at":claim["lease_expires_at"]});
        let work = worker
            .work_root
            .join(format!("cr-attempt-{}", worker::random_name()?));
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
        let mut session = worker::Session::experiment(
            worker,
            &claim,
            &job,
            &api,
            &base,
            &lease,
            &work,
            lost.clone(),
        );
        let result = session.run_steps().await;
        let delivery = async {
            if cancel.is_set() {
                Err(RuntimeError::Cancelled)
            } else if lost.is_set() {
                Ok("abandoned".to_owned())
            } else {
                match result {
                    Ok(run) => match self
                        .submit(&base, &lease, &id, &job, run, session.objects(), &lost)
                        .await
                    {
                        Ok(state) => Ok(state),
                        Err(error) => self.release(&base, &lease, error, session.failure()).await,
                    },
                    Err(error) => self.release(&base, &lease, error, session.failure()).await,
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
        // Never remove a directory while a backend may still mount it.
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
    #[allow(clippy::too_many_arguments)] // One claimed attempt's immutable submission provenance.
    async fn submit(
        &self,
        base: &str,
        lease: &LeaseHeaders,
        id: &str,
        job: &Value,
        mut run: Value,
        objects: &[Value],
        lost: &CancellationEvent,
    ) -> Result<String, RuntimeError> {
        let worker = &self.resources;
        if lost.is_set() {
            return Err(RuntimeError::LostLease);
        }
        let published = json!({"schema_version":"0.2","attempt_id":id,"objects":objects});
        let manifest = http::json_response(
            worker
                .client
                .request(
                    Method::POST,
                    &format!("{base}/manifest"),
                    &worker.token,
                    Some(lease),
                    Some(&published),
                )
                .await?,
        )
        .await?;
        if lost.is_set() {
            return Err(RuntimeError::LostLease);
        }
        uuid::Uuid::parse_str(worker::string(&manifest, "ref")?)
            .map_err(|_| RuntimeError::Contract)?;
        if manifest["sha256"] != worker::canonical_digest(&published)? {
            return Err(RuntimeError::Integrity);
        }
        let body = run["body"]
            .as_str()
            .ok_or(RuntimeError::InvalidStepOutput)?
            .to_owned();
        let record = run["front_matter"]
            .as_object_mut()
            .ok_or(RuntimeError::InvalidStepOutput)?;
        let mut provenance = record
            .get("provenance")
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        provenance["science_revision"] =
            json!(worker::science_revision(&job["science_revision"])?.to_string());
        if provenance.get("source_revision").is_none() {
            provenance["source_revision"] = json!(format!(
                "sha256:{}",
                worker::canonical_digest(&job["steps"])?
            ));
        }
        record.insert("provenance".into(), provenance);
        record.insert("manifest".into(), manifest);
        // The run document: its front matter, then its notes unchanged.
        let submission = json!({"document": crate::verification::markdown(record, &body)?});
        let submission_lease = LeaseHeaders {
            token: Secret::new(lease.token.expose().to_owned()),
            generation: lease.generation.clone(),
            idempotency: Some(format!("submit-{id}-{}", lease.generation)),
        };
        // Exact same body/key for an ambiguous response; server consumes once.
        for retry in 0..4 {
            if lost.is_set() {
                return Err(RuntimeError::LostLease);
            }
            match worker
                .client
                .request(
                    Method::POST,
                    &format!("{base}/submission"),
                    &worker.token,
                    Some(&submission_lease),
                    Some(&submission),
                )
                .await
            {
                Ok(response) => return worker::outcome_state(response, true).await,
                Err(RuntimeError::Transport) if retry < 3 => {
                    tokio::select! { ()=lost.wait()=>return Err(RuntimeError::LostLease), ()=tokio::time::sleep(Duration::from_millis(500 * (1 << retry)))=>{} }
                }
                Err(error) => return Err(error),
            }
        }
        Err(RuntimeError::Transport)
    }
    async fn release(
        &self,
        base: &str,
        lease: &LeaseHeaders,
        error: RuntimeError,
        failure: (Option<&str>, &[Value]),
    ) -> Result<String, RuntimeError> {
        let code = match error {
            RuntimeError::LostLease => return Ok("abandoned".into()),
            RuntimeError::DurablyFailed => return Ok("failed".into()),
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
        };
        let mut report = json!({"reason":error.to_string(),"code":code,"logs":failure.1});
        if let Some(step) = failure.0 {
            report["step"] = json!(step);
        }
        let worker = &self.resources;
        let mut response = worker
            .client
            .request(
                Method::POST,
                &format!("{base}/release"),
                &worker.token,
                Some(lease),
                Some(&report),
            )
            .await;
        if response.as_ref().is_ok_and(|r| r.status().as_u16() == 422) {
            report = json!({"reason":error.to_string(),"code":code});
            response = worker
                .client
                .request(
                    Method::POST,
                    &format!("{base}/release"),
                    &worker.token,
                    Some(lease),
                    Some(&report),
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
impl super::process::RuntimeWorker for Experiment {
    fn run_once(&self, cancel: CancellationEvent) -> FutureResult<'_, Option<JobResult>> {
        Box::pin(Experiment::run_once(self, cancel))
    }
    fn close(&self) -> FutureResult<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
