//! A claimant submission freezes evidence and queues testing in one transaction.
use crate::{
    AppState,
    attempt_lease_routes::{self, Failure, domain, failure, internal},
    authentication::authenticate,
    body::{self, DecodedBody},
    job_lifecycle::{self, Context},
    manifest_routes::{canonical_bytes, canonical_sha256},
    request_context::first_header,
    requests::RequestContext,
};
use axum::{
    Router,
    extract::{FromRequestParts, Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use cannery_attempts::{
    model::{Attempt, Manifest, ManifestId, Stage, StoredJson},
    repo::{AddEvidence, Repository},
};
use cannery_core::{
    audit::{self, Attribution, Record},
    contracts::{ContractKind, ContractValidator},
    errors::{DomainError, ErrorCode},
    ids::AttemptId,
    json::{Document, DocumentBuilder, Node, NodeId},
    principal::Principal,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Acquire, PgConnection};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub struct SubmissionContext {
    pub lifecycle: Arc<Context>,
    pub contracts: ContractValidator,
    pub nesting_budget: usize,
    pub response: crate::attempt_read_wire::ResponseContext,
}
#[derive(Clone)]
pub(crate) struct RouteState {
    app: AppState,
    profile: Arc<SubmissionContext>,
}
pub fn routes(app: AppState, profile: Arc<SubmissionContext>) -> Router {
    Router::new()
        .route(
            "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/submission",
            post(submit),
        )
        .with_state(RouteState { app, profile })
}
fn invalid(path: &str, message: &str) -> DomainError {
    DomainError::new(ErrorCode::ValidationFailed, message)
        .with_details(json!([{"path":path,"message":message}]))
}
fn text(document: &Document, root: NodeId, key: &str) -> Option<String> {
    document
        .field(root, key)
        .and_then(|id| document.node(id))
        .and_then(|node| {
            if let Node::String(value) = node {
                value.as_utf8()
            } else {
                None
            }
        })
}
fn subtree(document: &Document, key: &str, request: &RequestContext) -> Result<Document, Failure> {
    let root = document
        .field(document.root(), key)
        .ok_or_else(|| internal(request, "submission field"))?;
    let mut builder = DocumentBuilder::new();
    let root = builder
        .import(document, root)
        .map_err(|_| internal(request, "submission subtree"))?;
    builder
        .finish(root)
        .map_err(|_| internal(request, "submission subtree"))
}
fn actor(principal: &Principal) -> String {
    match principal {
        Principal::User(user) => format!("user:{}", user.user_id.0),
        Principal::Service(service) => format!("service:{}", service.service_account_id.0),
    }
}
async fn lookup(
    c: &mut PgConnection,
    principal: &Principal,
    key: &str,
    hash: &[u8],
    request: &RequestContext,
) -> Result<Option<String>, Failure> {
    let actor = actor(principal);
    let lock = format!("attempt.submit\n{actor}\n{key}");
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        lock
    )
    .execute(&mut *c)
    .await
    .map_err(|_| internal(request, "submission replay lock"))?;
    let row = sqlx::query!("SELECT request_hash,result_id FROM idempotency_keys WHERE scope=$1 AND actor=$2 AND key=$3", "attempt.submit", actor, key).fetch_optional(c).await.map_err(|_| internal(request, "submission replay lookup"))?;
    if let Some(row) = row {
        if row.request_hash != hash {
            return Err(domain(
                ErrorCode::Conflict,
                "this Idempotency-Key was already used with a different request",
            ));
        }
        return Ok(Some(row.result_id));
    }
    Ok(None)
}
async fn remember(
    c: &mut PgConnection,
    principal: &Principal,
    key: &str,
    hash: &[u8],
    id: &str,
    request: &RequestContext,
) -> Result<(), Failure> {
    let actor = actor(principal);
    sqlx::query!("INSERT INTO idempotency_keys(scope,actor,key,request_hash,result_id) VALUES($1,$2,$3,$4,$5)", "attempt.submit", actor, key, hash, id)
        .execute(c).await.map_err(|_| internal(request, "submission replay storage"))?;
    Ok(())
}
#[allow(
    clippy::too_many_arguments,
    reason = "Validation uses the caller's attempt lock, stored manifest and pinned science"
)]
#[allow(
    clippy::too_many_lines,
    reason = "Schema, pinned provenance, manifest roles and report bounds preserve their validation order"
)]
async fn check_sheet(
    c: &mut PgConnection,
    attempt: &Attempt,
    sheet: &Document,
    profile: &SubmissionContext,
    request: &RequestContext,
) -> Result<Result<Manifest, DomainError>, Failure> {
    let violations = profile
        .contracts
        .violations(ContractKind::EvidenceEnvelope, sheet)
        .map_err(|_| internal(request, "submission contract"))?;
    if !violations.is_empty() {
        let details: Vec<_> = violations
            .into_iter()
            .map(|value| json!({"path":value.path.as_utf8(),"message":value.message}))
            .collect();
        return Ok(Err(DomainError::new(
            ErrorCode::ValidationFailed,
            "invalid evidence envelope",
        )
        .with_details(json!(details))));
    }
    if text(sheet, sheet.root(), "stage").as_deref() != Some("agent") {
        return Ok(Err(invalid(
            "/stage",
            "an attempt submits agent-stage evidence",
        )));
    }
    if text(sheet, sheet.root(), "attempt_id") != Some(attempt.id.0.to_string()) {
        return Ok(Err(invalid(
            "/attempt_id",
            "sheet belongs to a different attempt",
        )));
    }
    let provenance = sheet
        .field(sheet.root(), "provenance")
        .ok_or_else(|| internal(request, "submission provenance"))?;
    if text(sheet, provenance, "science_revision") != Some(attempt.science_revision.to_string()) {
        return Ok(Err(invalid(
            "/provenance/science_revision",
            "sheet must use the pinned science revision",
        )));
    }
    let reference = sheet
        .field(sheet.root(), "manifest")
        .ok_or_else(|| internal(request, "submission manifest reference"))?;
    let id = text(sheet, reference, "ref").and_then(|value| uuid::Uuid::parse_str(&value).ok());
    let manifest = if let Some(id) = id {
        Repository::new(c, profile.lifecycle.attempts)
            .get_manifest(attempt.id, ManifestId(id))
            .await
            .map_err(|_| internal(request, "submission manifest lookup"))?
    } else {
        None
    };
    let Some(manifest) = manifest.filter(|value| {
        value.stage == Stage::Agent
            && Some(value.sha256.clone()) == text(sheet, reference, "sha256")
    }) else {
        return Ok(Err(invalid(
            "/manifest",
            "not a verified manifest of this attempt",
        )));
    };
    let StoredJson::Value(content) = &manifest.content else {
        return Ok(Err(invalid(
            "/manifest",
            "verified manifest content is missing",
        )));
    };
    let Some(Node::Array(objects)) = content
        .field(content.root(), "objects")
        .and_then(|id| content.node(id))
    else {
        return Ok(Err(invalid(
            "/manifest",
            "verified manifest has no objects",
        )));
    };
    let roles: BTreeSet<_> = objects
        .iter()
        .filter_map(|object| text(content, *object, "role"))
        .collect();
    if let Some(Node::Array(referenced)) = sheet
        .field(sheet.root(), "artifact_roles")
        .and_then(|id| sheet.node(id))
    {
        for (index, role) in referenced.iter().enumerate() {
            if !matches!(sheet.node(*role), Some(Node::String(value)) if value.as_utf8().is_some_and(|value| roles.contains(&value)))
            {
                return Ok(Err(invalid(
                    &format!("/artifact_roles/{index}"),
                    "the manifest has no object with this role",
                )));
            }
        }
    }
    let raw = job_lifecycle::science(c, attempt, &profile.lifecycle, request).await?;
    let registration = job_lifecycle::value(&raw.content, &profile.lifecycle, request)?;
    if text(sheet, sheet.root(), "status").as_deref() == Some("completed") {
        let required = registration["required_artifact_roles"]["attempt"]
            .as_array()
            .ok_or_else(|| internal(request, "submission required roles"))?;
        if required
            .iter()
            .any(|role| role.as_str().is_none_or(|role| !roles.contains(role)))
        {
            return Ok(Err(invalid(
                "/manifest",
                "the manifest lacks required artifact roles",
            )));
        }
    }
    let report = sheet
        .field(sheet.root(), "report")
        .ok_or_else(|| internal(request, "submission report"))?;
    let markdown = text(sheet, report, "body_markdown")
        .ok_or_else(|| internal(request, "submission report body"))?;
    let limit = registration["limits"]["report_max_bytes"]
        .as_u64()
        .ok_or_else(|| internal(request, "submission report limit"))?;
    if u64::try_from(markdown.len()).map_or(true, |bytes| bytes > limit) {
        return Ok(Err(invalid(
            "/report/body_markdown",
            "the report exceeds the configured byte limit",
        )));
    }
    Ok(Ok(manifest))
}
#[allow(
    clippy::too_many_arguments,
    reason = "Failure and its audit share the caller's lease transaction"
)]
async fn failed(
    c: &mut PgConnection,
    principal: &Principal,
    attempt: &Attempt,
    code: &str,
    reason: &str,
    details: &Value,
    key: Option<&str>,
    manifest: Option<&str>,
    profile: &SubmissionContext,
    request: &RequestContext,
) -> Result<(), Failure> {
    let details = job_lifecycle::document(details, &profile.lifecycle, request)?;
    crate::attempt_failure::agent(
        c,
        profile.lifecycle.attempts,
        principal,
        attempt,
        &crate::attempt_failure::Report {
            code,
            reason,
            details: &details,
            log_refs: None,
            step: None,
            idempotency_key: key,
            manifest_sha256: manifest,
        },
        request,
    )
    .await
}
#[allow(
    clippy::too_many_lines,
    reason = "The lease, evidence, testing queue and audit commit as one submission"
)]
#[utoipa::path(
    post,
    path = "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/submission",
    operation_id = "submit_api_projects__slug__hypotheses__number__attempts__sequence__submission_post",
    summary = "Submit",
    description = "Submit the claimed result sheet; the attempt is frozen on acceptance.\n\nA sheet with ``status: failed`` records the agent's own failure report and\nfails the attempt for human review. So does an invalid sheet, once the\ncaller has shown it holds the lease.",
    params(("slug" = String, Path),
        ("number" = i64, Path),
        ("sequence" = i64, Path),
        ("X-Lease-Token" = Option<String>, Header),
        ("X-Lease-Generation" = Option<i64>, Header),
        ("idempotency-key" = Option<String>, Header)),
    request_body(content = crate::api_models::EvidenceEnvelopeRequest, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::AttemptOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn submit(
    State(state): State<RouteState>,
    axum::Extension(request_context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let mut auth = authenticate(
        &state.app,
        &request_context,
        request.headers(),
        request.method(),
    )
    .await
    .map_err(failure)?;
    let (mut parts, body) = body::read_body(request).await.map_err(failure)?;
    let paths = Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &state)
        .await
        .map_err(failure)?
        .0;
    let parameters = attempt_lease_routes::parameters(&paths, &parts.headers, &request_context)?;
    let slug = paths
        .get("slug")
        .ok_or_else(|| internal(&request_context, "submission project path"))?;
    let project = attempt_lease_routes::worker_access(
        &mut auth.connection,
        &auth.principal,
        slug,
        &request_context,
    )
    .await?;
    let DecodedBody::Json(sheet) = body else {
        return Err(failure(crate::errors::ApiError::from(invalid(
            "body",
            "submission must be a JSON object",
        ))));
    };
    if !matches!(sheet.node(sheet.root()), Some(Node::Object(_))) {
        return Err(failure(crate::errors::ApiError::from(invalid(
            "body",
            "submission must be a JSON object",
        ))));
    }
    let key = first_header(&parts.headers, "idempotency-key");
    if key
        .as_ref()
        .is_some_and(|key| !(1..=200).contains(&key.chars().count()))
    {
        return Err(failure(crate::errors::ApiError::from(invalid(
            "header/Idempotency-Key",
            "Idempotency-Key must be 1 to 200 characters",
        ))));
    }
    let mut envelope = DocumentBuilder::new();
    let sheet_root = envelope
        .import(&sheet, sheet.root())
        .map_err(|_| internal(&request_context, "submission hash sheet"))?;
    let project_slug = envelope
        .push(Node::String(String::from(&project.slug)))
        .map_err(|_| internal(&request_context, "submission hash project"))?;
    let reference = parameters.reference();
    let attempt_ref = envelope
        .push(Node::String(String::from(&reference)))
        .map_err(|_| internal(&request_context, "submission hash attempt"))?;
    let root = envelope
        .push(Node::Object(vec![
            (String::from("project"), project_slug),
            (String::from("attempt"), attempt_ref),
            (String::from("sheet"), sheet_root),
        ]))
        .map_err(|_| internal(&request_context, "submission hash envelope"))?;
    let envelope = envelope
        .finish(root)
        .map_err(|_| internal(&request_context, "submission hash envelope"))?;
    let hash = Sha256::digest(
        canonical_bytes(&envelope, state.profile.nesting_budget).map_err(|_| {
            failure(crate::errors::ApiError::from(invalid(
                "body",
                "submission must contain finite JSON values within the nesting limit",
            )))
        })?,
    )
    .to_vec();
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&request_context, "submission transaction"))?;
    if let Some(key) = &key
        && let Some(id) = lookup(&mut tx, &auth.principal, key, &hash, &request_context).await?
    {
        let id = AttemptId(
            uuid::Uuid::parse_str(&id)
                .map_err(|_| internal(&request_context, "submission replay ID"))?,
        );
        let attempt = Repository::new(&mut tx, state.profile.lifecycle.attempts)
            .get_attempt_by_id(id, false)
            .await
            .map_err(|_| internal(&request_context, "submission replay result"))?
            .ok_or_else(|| internal(&request_context, "submission replay result missing"))?;
        tx.commit()
            .await
            .map_err(|_| internal(&request_context, "submission replay commit"))?;
        return response(StatusCode::OK, &attempt, &state.profile, &request_context);
    }
    let attempt = attempt_lease_routes::leased(
        &mut tx,
        &auth.principal,
        &project,
        &parameters,
        state.profile.lifecycle.attempts,
        &request_context,
    )
    .await?;
    let manifest =
        match check_sheet(&mut tx, &attempt, &sheet, &state.profile, &request_context).await? {
            Ok(manifest) => manifest,
            Err(error) => {
                failed(
                    &mut tx,
                    &auth.principal,
                    &attempt,
                    "invalid_submission",
                    &format!("invalid submission: {}", error.message),
                    &json!({"errors":error.details}),
                    None,
                    None,
                    &state.profile,
                    &request_context,
                )
                .await?;
                tx.commit()
                    .await
                    .map_err(|_| internal(&request_context, "invalid submission commit"))?;
                return Err(failure(crate::errors::ApiError::from(
                    DomainError::new(
                        ErrorCode::ValidationFailed,
                        format!("{}; the attempt failed and awaits review", error.message),
                    )
                    .with_details(error.details),
                )));
            }
        };
    let sheet =
        crate::api_models::request_document::<crate::api_models::EvidenceEnvelopeRequest>(&sheet)
            .map_err(failure)?;
    record_sheet(
        &mut tx,
        &auth.principal,
        &project,
        &attempt,
        &sheet,
        &manifest,
        key.as_deref(),
        &state,
        &request_context,
    )
    .await?;
    if let Some(key) = &key {
        remember(
            &mut tx,
            &auth.principal,
            key,
            &hash,
            &attempt.id.0.to_string(),
            &request_context,
        )
        .await?;
    }
    let frozen = Repository::new(&mut tx, state.profile.lifecycle.attempts)
        .get_attempt_by_id(attempt.id, false)
        .await
        .map_err(|_| internal(&request_context, "frozen submission"))?
        .ok_or_else(|| internal(&request_context, "frozen submission missing"))?;
    tx.commit()
        .await
        .map_err(|_| internal(&request_context, "submission commit"))?;
    response(
        StatusCode::CREATED,
        &frozen,
        &state.profile,
        &request_context,
    )
}
fn response(
    status: StatusCode,
    attempt: &Attempt,
    profile: &SubmissionContext,
    request: &RequestContext,
) -> Result<Response, Failure> {
    let bytes = crate::attempt_read_wire::attempt(attempt, profile.response)
        .map_err(|_| internal(request, "submission response"))?;
    Ok((status, [(header::CONTENT_TYPE, "application/json")], bytes).into_response())
}
#[allow(
    clippy::too_many_arguments,
    reason = "One transaction binds evidence, report mentions, queue and submission audit"
)]
#[allow(
    clippy::too_many_lines,
    reason = "Evidence, mentions, freeze, testing queue and audit share one caller transaction"
)]
async fn record_sheet(
    c: &mut PgConnection,
    principal: &Principal,
    project: &cannery_projects::repo::Project,
    attempt: &Attempt,
    sheet: &Document,
    manifest: &Manifest,
    key: Option<&str>,
    state: &RouteState,
    request: &RequestContext,
) -> Result<(), Failure> {
    let profile = &state.profile;
    let sha = canonical_sha256(sheet, profile.nesting_budget)
        .map_err(|_| internal(request, "submission evidence hash"))?;
    let status = text(sheet, sheet.root(), "status")
        .ok_or_else(|| internal(request, "submission status"))?;
    let evidence = Repository::new(c, profile.lifecycle.attempts)
        .add_evidence(AddEvidence {
            project_id: project.id,
            attempt_id: attempt.id,
            stage: "agent",
            status: &status,
            content: sheet,
            sha256: &sha,
            manifest_id: Some(manifest.id),
            principal,
        })
        .await
        .map_err(|_| internal(request, "submission evidence insert"))?;
    let report = subtree(sheet, "report", request)?;
    let mentions = crate::hypothesis_mutations::resolve_mentions(
        c,
        principal,
        project,
        &report,
        Some(attempt.hypothesis_id),
        profile.nesting_budget,
        request,
    )
    .await
    .map_err(failure)?;
    cannery_hypotheses::repo::replace_mentions(
        c,
        cannery_hypotheses::repo::MentionSource::Report(cannery_hypotheses::repo::EvidenceId(
            evidence.0,
        )),
        mentions,
    )
    .await
    .map_err(|_| internal(request, "submission mentions"))?;
    if status != "completed" {
        let typed = job_lifecycle::value(sheet, &profile.lifecycle, request)?;
        return failed(
            c,
            principal,
            attempt,
            "agent_reported_failure",
            "the agent reported that the attempt failed",
            &json!({"observations":typed.get("observations")}),
            key,
            Some(&manifest.sha256),
            profile,
            request,
        )
        .await;
    }
    Repository::new(c, profile.lifecycle.attempts)
        .end_lease(attempt.id, "submitted")
        .await
        .map_err(|_| internal(request, "submission freeze"))?;
    let blocked =
        job_lifecycle::no_evaluator_reason(c, attempt, &profile.lifecycle, request).await?;
    let job = if blocked.is_none() {
        let job = job_lifecycle::create_test_job(
            c,
            attempt,
            evidence.0,
            &sha,
            manifest,
            state.app.settings.leases.job_overhead_seconds.as_bigint(),
            &profile.lifecycle,
            request,
        )
        .await?;
        Repository::new(c, profile.lifecycle.attempts)
            .move_attempt(attempt.id, "submitted", "testing")
            .await
            .map_err(|_| internal(request, "submission testing transition"))?;
        Some(job)
    } else {
        None
    };
    let prior = json!({"state":attempt.state.as_str()});
    let new = json!({"state":if job.is_some() { "testing" } else { "submitted" },"manifest_sha256":manifest.sha256,"claimed_sheet_sha256":sha,"job_id":job.as_ref().map(|job|job.id.0.to_string())});
    audit::record(
        c,
        Attribution::Principal(principal),
        Record {
            action: "attempt.submitted",
            subject_type: "attempt",
            subject_id: &attempt.id.0.to_string(),
            project_id: Some(project.id),
            prior_state: Some(&prior),
            new_state: Some(&new),
            reason: None,
            idempotency_key: key,
        },
    )
    .await
    .map_err(|_| internal(request, "submission audit"))?;
    if let Some(job) = job {
        job_lifecycle::record_job_created(c, principal, attempt, &job, &profile.lifecycle, request)
            .await?;
    }
    if let Some(reason) = blocked {
        let submitted = Repository::new(c, profile.lifecycle.attempts)
            .get_attempt_by_id(attempt.id, false)
            .await
            .map_err(|_| internal(request, "blocked submitted attempt"))?
            .ok_or_else(|| internal(request, "blocked submitted attempt missing"))?;
        job_lifecycle::fail_without_evaluator(
            c,
            principal,
            &submitted,
            cannery_jobs::repo::Stage::Tester,
            &reason,
            &profile.lifecycle,
            request,
        )
        .await?;
    }
    Ok(())
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
