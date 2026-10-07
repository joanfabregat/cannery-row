//! Lease-fenced completion and failure; mutations and audit share one transaction.
#![allow(
    clippy::many_single_char_names,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "Transaction phases retain explicit caller context and ordered mutations"
)]
use crate::{
    AppState,
    attempt_lease_routes::{Failure, domain, failure, internal},
    authentication::authenticate,
    errors::ApiError,
    job_lifecycle as flow, job_read_wire,
    request_context::first_header,
    requests::RequestContext,
    validation,
};
use axum::{
    Router,
    extract::{FromRequestParts, Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use cannery_attempts::{
    model::{Attempt, EvidenceId},
    repo::Repository,
};
use cannery_core::{
    contracts::{ContractKind, ContractValidator},
    errors::{DomainError, ErrorCode},
    ids::JobId,
    json::Document,
    principal::{Principal, ServiceKind, ServicePrincipal},
    timestamps::Timestamp,
};
use cannery_jobs::repo::{self as jobs, Job, Stage};
use num_bigint::BigInt;
use serde_json::{Value, json};
use sqlx::{Acquire, PgConnection};
use std::{collections::BTreeMap, sync::Arc};

/// Explicit installation, schema and serialization profiles; defaults remain unbound.
pub struct JobLifecycleContext {
    pub flow: flow::Context,
    pub contracts: ContractValidator,

    pub repr_budget: usize,
    pub response: job_read_wire::ResponseContext,
}
#[derive(Clone, Copy, Debug)]
#[doc(hidden)]
pub struct CompletionReplay(pub bool);
#[derive(Clone)]
pub(crate) struct RouteState {
    app: AppState,
    context: Arc<JobLifecycleContext>,
}
pub fn routes(app: AppState, context: Arc<JobLifecycleContext>) -> Router {
    Router::new()
        .route(
            "/api/projects/{slug}/jobs/{job_id}/completion",
            post(complete),
        )
        .route("/api/projects/{slug}/jobs/{job_id}/failure", post(fail))
        .with_state(RouteState { app, context })
}
pub(crate) fn invalid(path: &str, message: impl Into<String>) -> Failure {
    let message = message.into();
    failure(ApiError::from(
        DomainError::new(ErrorCode::ValidationFailed, &message)
            .with_details(json!([{"path":path,"message":message}])),
    ))
}
fn validation_error(e: &validation::ValidationErrors, r: &RequestContext) -> Failure {
    e.domain_error().map_or_else(
        |_| internal(r, "job request validation"),
        |v| failure(ApiError::from(v)),
    )
}
pub(crate) async fn worker(
    c: &mut PgConnection,
    p: &Principal,
    slug: &str,
    r: &RequestContext,
) -> Result<cannery_projects::repo::Project, Failure> {
    cannery_projects::authz::project_access(
        c,
        p,
        slug,
        None,
        &[ServiceKind::Tester, ServiceKind::Evaluator],
        true,
    )
    .await
    .map(|v| v.project)
    .map_err(|e| failure(r.project_error(e)))
}
pub(crate) fn stage(p: &ServicePrincipal) -> Stage {
    if p.kind == ServiceKind::Tester {
        Stage::Tester
    } else {
        Stage::Evaluator
    }
}
pub(crate) async fn locked_attempt(
    c: &mut PgConnection,
    id: JobId,
    project: cannery_core::ids::ProjectId,
    s: &flow::Context,
    r: &RequestContext,
) -> Result<Attempt, Failure> {
    let job = jobs::get_job(c, id, false, s.jobs)
        .await
        .map_err(|_| internal(r, "job unlocked lookup"))?
        .filter(|j| j.project_id == project)
        .ok_or_else(|| domain(ErrorCode::NotFound, "job not found"))?;
    Repository::new(c, s.attempts)
        .get_attempt_by_id(job.attempt_id, true)
        .await
        .map_err(|_| internal(r, "job attempt lock"))?
        .ok_or_else(|| internal(r, "job attempt missing"))
}
pub(crate) async fn leased(
    c: &mut PgConnection,
    p: &ServicePrincipal,
    id: JobId,
    project: cannery_core::ids::ProjectId,
    token: Option<&str>,
    generation: Option<&BigInt>,
    s: &flow::Context,
    r: &RequestContext,
) -> Result<Job, Failure> {
    let job = jobs::get_job(c, id, true, s.jobs)
        .await
        .map_err(|_| internal(r, "job lease lock"))?
        .filter(|j| j.project_id == project)
        .ok_or_else(|| domain(ErrorCode::NotFound, "job not found"))?;
    if job.stage != stage(p) {
        return Err(domain(
            ErrorCode::Forbidden,
            format!(
                "a {} service account cannot work on {} jobs",
                stage(p).as_str(),
                job.stage.as_str()
            ),
        ));
    }
    if job.claimed_by_service != Some(p.service_account_id) {
        return Err(domain(
            ErrorCode::Forbidden,
            format!(
                "only the {} that claimed this job can work on it",
                job.stage.as_str()
            ),
        ));
    }
    let (Some(token), Some(generation)) = (token, generation) else {
        return Err(domain(
            ErrorCode::StaleLease,
            "send X-Lease-Token and X-Lease-Generation",
        ));
    };
    if job.state != jobs::State::Claimed
        || generation != &BigInt::from(job.lease_generation)
        || !job
            .lease_token_hash
            .as_ref()
            .is_some_and(|hash| cannery_identity::secrets::matches_digest(hash, token))
    {
        return Err(domain(
            ErrorCode::StaleLease,
            "this lease is no longer valid for the job",
        ));
    }
    let now: Timestamp = sqlx::query_scalar("SELECT now()")
        .fetch_one(c)
        .await
        .map_err(|_| internal(r, "job lease clock"))?;
    if job.deadline.is_none_or(|v| v.0 <= now.0) {
        return Err(domain(ErrorCode::StaleLease, "the job's deadline passed"));
    }
    if job.lease_expires_at.is_none_or(|v| v.0 <= now.0) {
        return Err(domain(ErrorCode::StaleLease, "the lease expired"));
    }
    Ok(job)
}
pub(crate) fn require_stage(a: &Attempt, j: &Job) -> Result<(), Failure> {
    let expected = if j.stage == Stage::Tester {
        "testing"
    } else {
        "evaluating"
    };
    if a.state.as_str() != expected {
        return Err(domain(
            ErrorCode::StaleLease,
            format!(
                "the attempt is no longer in the {} stage ({})",
                j.stage.as_str(),
                a.state.as_str()
            ),
        ));
    }
    Ok(())
}
async fn output(
    c: &mut PgConnection,
    j: &Job,
    s: &JobLifecycleContext,
    r: &RequestContext,
) -> Result<Response, Failure> {
    let evidence = if let Some(id) = j.evidence_id {
        Repository::new(c, s.flow.attempts)
            .get_evidence_by_id(j.attempt_id, EvidenceId(id.0))
            .await
            .map_err(|_| internal(r, "completed job evidence"))?
            .map(|v| v.0)
    } else {
        None
    };
    let projection = job_read_wire::prepare(j, s.response)
        .map_err(|_| internal(r, "completed job projection"))?;
    let artifacts = Repository::new(c, s.flow.attempts)
        .list_job_artifacts(j.id)
        .await
        .map_err(|_| internal(r, "completed job artifacts"))?;
    let bytes = job_read_wire::job(j, projection, evidence.as_ref(), &artifacts, s.response)
        .map_err(|_| internal(r, "completed job response"))?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response())
}
fn contract(
    d: &Document,
    kind: ContractKind,
    s: &JobLifecycleContext,
    r: &RequestContext,
) -> Result<(), Failure> {
    let errors = s
        .contracts
        .violations(kind, d)
        .map_err(|_| internal(r, "job contract execution"))?;
    if errors.is_empty() {
        return Ok(());
    }
    let details=errors.into_iter().map(|e|Ok(json!({"path":e.path.as_utf8().ok_or_else(||internal(r,"job validation path"))?,"message":e.message}))).collect::<Result<Vec<_>,Failure>>()?;
    Err(failure(ApiError::from(
        DomainError::new(ErrorCode::ValidationFailed, "invalid job document")
            .with_details(json!(details)),
    )))
}
#[utoipa::path(
    post,
    path = "/api/projects/{slug}/jobs/{job_id}/completion",
    operation_id = "complete_job_api_projects__slug__jobs__job_id__completion_post",
    summary = "Complete Job",
    description = "Publish the job's evidence envelope with the manifest of its outputs.\n\nA test job completes with tester evidence and a manifest; the attempt\nmoves on to ``evaluating`` and waits for its evaluation job\n(:mod:`cannery_row.evaluation.flow`). An evaluation job completes with the\nevaluator's record (the\nmanifest is optional); the attempt then awaits human review in a\n``result`` case. Repeating a completion returns the completed job\nwithout publishing again. An invalid completion from the lease holder is\nan infrastructure failure of the job's stage: it reruns or fails the\nattempt for human review.",
    params(("slug" = String, Path),
        ("job_id" = String, Path, format = "uuid"),
        ("X-Lease-Token" = Option<String>, Header),
        ("X-Lease-Generation" = Option<i64>, Header),
        ("idempotency-key" = Option<String>, Header)),
    request_body(content = crate::api_models::JobCompletionRequest, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = crate::api_models::JobOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn complete(
    State(s): State<RouteState>,
    axum::Extension(r): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    mutate(s, r, request, true).await
}
#[utoipa::path(
    post,
    path = "/api/projects/{slug}/jobs/{job_id}/failure",
    operation_id = "fail_job_api_projects__slug__jobs__job_id__failure_post",
    summary = "Fail Job",
    description = "Report that the job failed: never with metrics, always with a reason.\n\nThe stage runs again automatically while reruns remain; then the attempt\nfails with a failure of the job's stage and a review case opens. An\nevaluator crash is such a failure, never a `fail` or `inconclusive` verdict.\nAn `invalid_step_output` naming a producer step is the agent's failure\n(the attempt fails at once for review, with no rerun) only when the API\nitself refused one of that step's outputs in this job, against the\ninterface the step declares for it (see `PUT /api/job-uploads/{id}`).\nOtherwise, whatever the report says, it is the tester's failure.",
    params(("slug" = String, Path),
        ("job_id" = String, Path, format = "uuid"),
        ("X-Lease-Token" = Option<String>, Header),
        ("X-Lease-Generation" = Option<i64>, Header)),
    request_body(content = crate::api_models::JobFailureRequest, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = crate::api_models::JobOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn fail(
    State(s): State<RouteState>,
    axum::Extension(r): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    mutate(s, r, request, false).await
}

fn sha(d: &Document, s: &JobLifecycleContext, r: &RequestContext) -> Result<String, Failure> {
    let bytes = crate::hypothesis_idempotency::canonical_hash(d, s.flow.jobs.encode_nesting_budget)
        .map_err(|_| internal(r, "job canonical digest"))?;
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        result.push(char::from(b"0123456789abcdef"[usize::from(byte & 15)]));
    }
    Ok(result)
}
async fn replay_lookup(
    c: &mut PgConnection,
    actor: &str,
    key: &str,
    hash: &[u8],
    r: &RequestContext,
) -> Result<Option<JobId>, Failure> {
    let lock = format!("job.complete\n{actor}\n{key}");
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        lock
    )
    .execute(&mut *c)
    .await
    .map_err(|_| internal(r, "completion replay lock"))?;
    let scope = "job.complete";
    let row=sqlx::query!("SELECT request_hash,result_id FROM idempotency_keys WHERE scope=$1 AND actor=$2 AND key=$3",scope,actor,key).fetch_optional(c).await.map_err(|_|internal(r,"completion replay lookup"))?;
    row.map(|row| {
        if row.request_hash != hash {
            return Err(domain(
                ErrorCode::Conflict,
                "this Idempotency-Key was already used with a different request",
            ));
        }
        uuid::Uuid::parse_str(&row.result_id)
            .map(JobId)
            .map_err(|_| internal(r, "completion replay identity"))
    })
    .transpose()
}
async fn remember(
    c: &mut PgConnection,
    actor: &str,
    key: &str,
    hash: &[u8],
    id: JobId,
    r: &RequestContext,
) -> Result<(), Failure> {
    let scope = "job.complete";
    let id = id.to_string();
    sqlx::query!("INSERT INTO idempotency_keys(scope,actor,key,request_hash,result_id) VALUES($1,$2,$3,$4,$5)",scope,actor,key,hash,id).execute(c).await.map(|_|()).map_err(|_|internal(r,"completion replay insertion"))
}
#[allow(
    clippy::too_many_arguments,
    reason = "Caller owns publication transaction"
)]
async fn publish(
    c: &mut PgConnection,
    p: &Principal,
    a: &Attempt,
    j: &Job,
    evidence: &Document,
    manifest: Option<&Document>,
    key: Option<&str>,
    s: &JobLifecycleContext,
    r: &RequestContext,
) -> Result<(), Failure> {
    let evidence_sha = sha(evidence, s, r)?;
    let stored = if let Some(manifest) = manifest {
        Some(
            Repository::new(c, s.flow.attempts)
                .add_manifest(a.id, j.stage.as_str(), manifest, &sha(manifest, s, r)?)
                .await
                .map_err(|_| internal(r, "job manifest publication"))?,
        )
    } else {
        None
    };
    let id = Repository::new(c, s.flow.attempts)
        .add_evidence(cannery_attempts::repo::AddEvidence {
            project_id: a.project_id,
            attempt_id: a.id,
            stage: j.stage.as_str(),
            status: "completed",
            content: evidence,
            sha256: &evidence_sha,
            manifest_id: stored.as_ref().map(|v| v.id),
            principal: p,
        })
        .await
        .map_err(|_| internal(r, "job evidence publication"))?;
    if j.stage == Stage::Tester {
        Repository::new(c, s.flow.attempts)
            .move_attempt(a.id, "testing", "evaluating")
            .await
            .map_err(|_| internal(r, "tested attempt transition"))?;
    } else {
        let revision: i32 = sqlx::query_scalar!(
            "SELECT revision FROM evidence_records WHERE id=$1",
            id.0 as _
        )
        .fetch_one(&mut *c)
        .await
        .map_err(|_| internal(r, "evaluator evidence revision"))?;
        Repository::new(c, s.flow.attempts)
            .move_attempt(a.id, "evaluating", "awaiting_human_review")
            .await
            .map_err(|_| internal(r, "evaluated attempt transition"))?;
        cannery_hypotheses::repo::set_state(
            c,
            a.hypothesis_id,
            cannery_hypotheses::repo::HypothesisState::AwaitingHumanReview,
            None,
        )
        .await
        .map_err(|_| internal(r, "evaluated hypothesis transition"))?;
        let case = cannery_reviews::repo::open_result_case(
            c,
            cannery_reviews::repo::OpenResultCase {
                project_id: a.project_id,
                hypothesis_id: a.hypothesis_id,
                attempt_id: a.id,
                evidence_id: cannery_reviews::EvidenceId(id.0),
                evidence_revision: Some(&BigInt::from(revision)),
            },
        )
        .await
        .map_err(|_| internal(r, "evaluated result review"))?;
        let record = flow::value(evidence, &s.flow, r)?;
        flow::event(c,p,a,"attempt.evaluated","attempt",&a.id.to_string(),Some(&json!({"state":"evaluating","hypothesis_state":"active"})),&json!({"state":"awaiting_human_review","hypothesis_state":"awaiting_human_review","verdict":record["assessment"]["verdict"],"evaluator":record["producer"],"evidence_id":id.0.to_string(),"evidence_sha256":evidence_sha,"review_case_id":case.to_string(),"job_id":j.id.to_string()}),record["assessment"]["reason"].as_str(),r).await?;
    }
    jobs::complete_job(
        c,
        j.id,
        jobs::EvidenceId(id.0),
        stored.as_ref().map(|v| jobs::ManifestId(v.id.0)),
        s.flow.jobs,
    )
    .await
    .map_err(|_| internal(r, "job completion transition"))?;
    cannery_core::audit::record(c,cannery_core::audit::Attribution::Principal(p),cannery_core::audit::Record{
        action:"job.completed",subject_type:"job",subject_id:&j.id.to_string(),project_id:Some(a.project_id),
        prior_state:Some(&json!({"state":"claimed"})),new_state:Some(&json!({"state":"completed","evidence_sha256":evidence_sha,"manifest_sha256":stored.as_ref().map(|v|&v.sha256)})),reason:None,idempotency_key:key,
    }).await.map_err(|_|internal(r,"job completion audit"))?;
    if j.stage == Stage::Tester {
        flow::event(
            c,
            p,
            a,
            "attempt.tested",
            "attempt",
            &a.id.to_string(),
            Some(&json!({"state":"testing"})),
            &json!({"state":"evaluating","job_id":j.id.to_string()}),
            None,
            r,
        )
        .await?;
        let evaluating = Repository::new(c, s.flow.attempts)
            .get_attempt_by_id(a.id, false)
            .await
            .map_err(|_| internal(r, "tested attempt reload"))?
            .ok_or_else(|| internal(r, "tested attempt missing"))?;
        flow::start_evaluation(
            c,
            p,
            &evaluating,
            j,
            id,
            &evidence_sha,
            stored
                .as_ref()
                .ok_or_else(|| internal(r, "tester manifest missing"))?,
            &s.flow,
            r,
        )
        .await?;
    }
    Ok(())
}
#[allow(
    clippy::too_many_lines,
    reason = "Authorization, fencing, replay, commit and durable failure order remain visible"
)]
async fn mutate(
    s: RouteState,
    r: RequestContext,
    request: Request,
    completion: bool,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request).await.map_err(failure)?;
    let mut auth = authenticate(&s.app, &r, &parts.headers, &parts.method)
        .await
        .map_err(failure)?;
    let paths = Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &s)
        .await
        .map_err(failure)?
        .0;
    let mut errors = vec![];
    let id = validation::job_uuid(&paths["job_id"])
        .map_err(|e| errors.extend(e.problems().iter().cloned()))
        .ok();
    let generation = first_header(&parts.headers, "x-lease-generation")
        .map(|v| validation::attempt_header_integer("X-Lease-Generation", &v))
        .transpose()
        .map_err(|e| errors.extend(e.problems().iter().cloned()))
        .ok()
        .flatten();
    let input = match &body {
        crate::body::DecodedBody::Missing => validation::BodyInput::Missing,
        crate::body::DecodedBody::RawBytes => validation::BodyInput::RawBytes,
        crate::body::DecodedBody::Json(d) => validation::BodyInput::Json(d),
    };
    let document = validation::validate_document_body(input)
        .map_err(|e| errors.extend(e.problems().iter().cloned()))
        .ok();
    if !errors.is_empty() {
        return Err(validation_error(
            &validation::ValidationErrors::from_problems(errors),
            &r,
        ));
    }
    let document = document.ok_or_else(|| internal(&r, "job request document"))?;
    let id = JobId(id.ok_or_else(|| internal(&r, "job request identity"))?);
    let project = worker(&mut auth.connection, &auth.principal, &paths["slug"], &r).await?;
    let Principal::Service(service) = &auth.principal else {
        return Err(internal(&r, "job worker principal"));
    };
    let token = first_header(&parts.headers, "x-lease-token");
    let key = completion
        .then(|| first_header(&parts.headers, "idempotency-key"))
        .flatten();
    if key
        .as_ref()
        .is_some_and(|v| v.chars().count() == 0 || v.chars().count() > 200)
    {
        return Err(domain(
            ErrorCode::ValidationFailed,
            "Idempotency-Key must be 1 to 200 characters",
        ));
    }
    let hash = if completion {
        let envelope = flow::document(
            &json!({"project":project.slug,"job":id.to_string(),"completion":flow::value(document,&s.context.flow,&r)?}),
            &s.context.flow,
            &r,
        )?;
        crate::hypothesis_idempotency::canonical_hash(
            &envelope,
            s.context.flow.jobs.encode_nesting_budget,
        )
        .map_err(|_| internal(&r, "completion request hash"))?
    } else {
        Vec::new()
    };
    let typed_failure = if completion {
        None
    } else {
        contract(document, ContractKind::JobFailure, &s.context, &r)?;
        Some(
            crate::api_models::request_document::<crate::api_models::JobFailureRequest>(document)
                .map_err(failure)?,
        )
    };
    let document = typed_failure.as_ref().unwrap_or(document);
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&r, "job mutation transaction"))?;
    let result = async {
        let actor = format!("service:{}", service.service_account_id);
        if let Some(key) = &key
            && let Some(previous) = replay_lookup(&mut tx, &actor, key, &hash, &r).await?
        {
            let job = jobs::get_job(&mut tx, previous, false, s.context.flow.jobs)
                .await
                .map_err(|_| internal(&r, "completion replay job"))?
                .ok_or_else(|| internal(&r, "completion replay missing"))?;
            return Ok((job, true, None));
        }
        let attempt =
            locked_attempt(&mut tx, id, project.id, &s.context.flow, &r).await?;
        let current = jobs::get_job(&mut tx, id, true, s.context.flow.jobs)
            .await
            .map_err(|_| internal(&r, "completion current job"))?
            .ok_or_else(|| internal(&r, "completion current job missing"))?;
        if completion
            && current.state == jobs::State::Completed
            && current.claimed_by_service == Some(service.service_account_id)
        {
            let evidence = flow::value(document, &s.context.flow, &r)?;
            let evidence = flow::document(
                evidence.get("evidence").unwrap_or(&Value::Null),
                &s.context.flow,
                &r,
            )?;
            if let Some(evidence_id) = current.evidence_id
                && Repository::new(&mut tx, s.context.flow.attempts)
                    .get_evidence_by_id(current.attempt_id, EvidenceId(evidence_id.0))
                    .await
                    .map_err(|_| internal(&r, "completion prior evidence"))?
                    .is_some_and(|v| sha(&evidence, &s.context, &r).ok().as_ref() == Some(&v.1))
            {
                return Ok((current, true, None));
            }
        }
        let job = leased(
            &mut tx,
            service,
            id,
            project.id,
            token.as_deref(),
            generation.as_ref(),
            &s.context.flow,
            &r,
        )
        .await?;
        require_stage(&attempt, &job)?;
        if completion {
            let checked = match contract(document, ContractKind::JobCompletion, &s.context, &r) {
                Ok(()) => match crate::api_models::request_document::<crate::api_models::JobCompletionRequest>(document) {
                    Ok(typed_document) => crate::job_completion_checks::completion(
                        &mut tx,
                        &attempt,
                        &job,
                        &typed_document,
                        &s.context,
                        &r,
                    )
                    .await,
                    Err(error) => Err(failure(error)),
                }
                Err(e) => Err(e),
            };
            match checked {
                Ok((evidence, manifest)) => {
                    publish(
                        &mut tx,
                        &auth.principal,
                        &attempt,
                        &job,
                        &evidence,
                        manifest.as_ref(),
                        key.as_deref(),
                        &s.context,
                        &r,
                    )
                    .await?;
                    if let Some(key) = &key {
                        remember(&mut tx, &actor, key, &hash, id, &r).await?;
                    }
                }
                Err(error) => {
                    let response = error.into_response();
                    if response.status() != StatusCode::UNPROCESSABLE_ENTITY {
                        return Err(failure(response));
                    }
                    let bytes = axum::body::to_bytes(response.into_body(), 65536).await.map_err(|_| internal(&r, "completion validation response"))?;
                    let diagnosis: Value = serde_json::from_slice(&bytes).map_err(|_| internal(&r, "completion validation diagnosis"))?;
                    let message = diagnosis["error"]["message"].as_str().unwrap_or("invalid job completion");
                    let reason = format!("invalid job completion: {message}");
                    let logs = flow::document(&json!([]), &s.context.flow, &r)?;
                    let details = flow::document(
                        &json!({"errors":diagnosis["error"]["details"]}),
                        &s.context.flow,
                        &r,
                    )?;
                    let (_, rerun) = flow::fail_job_run(
                        &mut tx,
                        &auth.principal,
                        &attempt,
                        &job,
                        jobs::Failure {
                            step: None,
                            code: "invalid_output",
                            reason: &reason,
                            logs: &logs,
                        },
                        &details,
                        false,
                        &s.context.flow,
                        &r,
                    )
                    .await?;
                    let outcome = if rerun.is_some() { "another run was queued" } else { "the attempt failed and awaits review" };
                    let error = failure(ApiError::from(DomainError::new(ErrorCode::ValidationFailed, format!("{message}; {outcome}")).with_details(diagnosis["error"]["details"].clone())));
                    return Ok((job, false, Some(error)));
                }
            }
        } else {
            let report = flow::value(document, &s.context.flow, &r)?;
            if report["job_id"] != job.id.to_string() {
                return Err(invalid("/job_id", format!("this is job {}", job.id)));
            }
            let step = report.get("step").and_then(Value::as_str);
            let spec = flow::value(&job.spec, &s.context.flow, &r)?;
            if let Some(step) = step
                && !spec["steps"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|v| v["name"] == step)
            {
                return Err(invalid("/step", "not a step of this job"));
            }
            let artifacts = Repository::new(&mut tx, s.context.flow.attempts)
                .list_job_artifacts(job.id)
                .await
                .map_err(|_| internal(&r, "job failure logs"))?;
            for (i, log) in report["logs"].as_array().into_iter().flatten().enumerate() {
                if !artifacts.iter().any(|a| {
                    log["key"] == a.key
                        && log["size_bytes"] == a.size_bytes
                        && log["sha256"] == a.sha256
                }) {
                    return Err(invalid(
                        &format!("/logs/{i}"),
                        "not a verified output of this job",
                    ));
                }
            }
            let logs = flow::document(&report["logs"], &s.context.flow, &r)?;
            let mut refused = vec![];
            if report["error_code"] == "invalid_step_output" && let Some(step) = step {
                let producer = spec["steps"].as_array().into_iter().flatten().find(|v|v["name"] == step && v["manifest"]["spec"]["role"] == "producer");
                if let Some(producer) = producer {
                    let uploads = Repository::new(&mut tx, s.context.flow.attempts).list_refused_uploads(job.id).await.map_err(|_|internal(&r,"producer refusal evidence"))?;
                    for upload in uploads {
                        if producer["manifest"]["spec"]["outputs"]["artifacts"].as_array().into_iter().flatten().any(|v|v["name"] == upload.role && v.get("interface").and_then(Value::as_str) == upload.interface.as_deref()) && spec["output_prefix"].as_str().is_some_and(|prefix|upload.key.starts_with(&format!("{prefix}{step}/{}/",upload.role))) {
                            refused.push(json!({"upload_id":upload.id.0.to_string(),"role":upload.role,"interface":upload.interface}));
                        }
                    }
                }
            }
            let agent_side = !refused.is_empty();
            let details = flow::document(&if agent_side {json!({"refused_uploads":refused})} else {json!({})}, &s.context.flow, &r)?;
            flow::fail_job_run(
                &mut tx,
                &auth.principal,
                &attempt,
                &job,
                jobs::Failure {
                    step,
                    code: report["error_code"]
                        .as_str()
                        .ok_or_else(|| internal(&r, "failure code"))?,
                    reason: report["reason"]
                        .as_str()
                        .ok_or_else(|| internal(&r, "failure reason"))?,
                    logs: &logs,
                },
                &details,
                agent_side,
                &s.context.flow,
                &r,
            )
            .await?;
        }
        let final_job = jobs::get_job(&mut tx, id, false, s.context.flow.jobs)
            .await
            .map_err(|_| internal(&r, "job outcome lookup"))?
            .ok_or_else(|| internal(&r, "job outcome missing"))?;
        Ok((final_job, false, None))
    }
    .await;
    let (job, replayed, invalid) = match result {
        Ok(v) => {
            tx.commit()
                .await
                .map_err(|_| internal(&r, "job mutation commit"))?;
            v
        }
        Err(e) => {
            if tx.rollback().await.is_err() {
                auth.connection.close_on_drop();
            }
            return Err(e);
        }
    };
    if let Some(error) = invalid {
        return Err(error);
    }
    let mut response = output(&mut auth.connection, &job, &s.context, &r).await?;
    response.extensions_mut().insert(CompletionReplay(replayed));
    response
        .extensions_mut()
        .insert(crate::mcp::ReplayOutcome(replayed));
    Ok(response)
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
