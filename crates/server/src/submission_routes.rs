//! A claimant submits its run document: the attempt freezes and its verify job is
//! queued in one transaction.
use crate::{
    AppState,
    api_models::RunSubmission,
    attempt_lease_routes::{self, Failure, domain, failure, internal},
    authentication::authenticate,
    body::{self, DecodedBody},
    job_lifecycle::{self, Context},
    manifest_routes::{canonical_bytes, canonical_sha256},
    request_context::first_header,
    requests::RequestContext,
    validation::{BodyInput, validate_run_submission},
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
    contracts::phases::{Phase, PhaseSchemas},
    errors::{DomainError, ErrorCode},
    front_matter::{self, FrontMatterError, Limits},
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
    /// Where an agent's transcript is sealed at its submission; none leaves
    /// transcripts unsealed.
    pub transcripts: Option<Arc<cannery_storage::ObjectStore>>,
    pub lifecycle: Arc<Context>,
    pub phases: PhaseSchemas,
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
            "/api/projects/{slug}/units/{number}/attempts/{sequence}/submission",
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
/// A checked run document: its front matter, its run notes and the verified
/// manifest it links.
struct Run {
    front_matter: Document,
    body: String,
    manifest: Manifest,
}
#[allow(
    clippy::too_many_lines,
    reason = "Schema, pinned provenance, manifest roles and the notes bound keep their validation order"
)]
async fn check_run(
    c: &mut PgConnection,
    attempt: &Attempt,
    parsed: Result<front_matter::Document, FrontMatterError>,
    profile: &SubmissionContext,
    request: &RequestContext,
) -> Result<Result<Run, DomainError>, Failure> {
    let parsed = match parsed {
        Ok(parsed) => parsed,
        Err(error) => return Ok(Err(invalid("body/document", &error.to_string()))),
    };
    let front_matter = Value::Object(parsed.front_matter);
    let violations = profile.phases.violations(Phase::Run, &front_matter);
    if let Some(first) = violations.first() {
        // The message names the first problem, so the failure a reviewer
        // reads says what to fix; the details list them all.
        let message = format!(
            "invalid run document front matter: {} {}{}",
            if first.path.is_empty() {
                "/"
            } else {
                first.path.as_str()
            },
            first.message,
            match violations.len() {
                1 => String::new(),
                count => format!(" (and {} more)", count - 1),
            }
        );
        let details: Vec<_> = violations
            .into_iter()
            .map(|value| json!({"path":value.path,"message":value.message}))
            .collect();
        return Ok(Err(
            DomainError::new(ErrorCode::ValidationFailed, message).with_details(json!(details))
        ));
    }
    let sheet = job_lifecycle::document(&front_matter, &profile.lifecycle, request)?;
    let provenance = sheet
        .field(sheet.root(), "provenance")
        .ok_or_else(|| internal(request, "submission provenance"))?;
    if text(&sheet, provenance, "science_revision") != Some(attempt.science_revision.to_string()) {
        return Ok(Err(invalid(
            "/provenance/science_revision",
            "the run must use the pinned science revision",
        )));
    }
    let reference = sheet
        .field(sheet.root(), "manifest")
        .ok_or_else(|| internal(request, "submission manifest reference"))?;
    let id = text(&sheet, reference, "ref").and_then(|value| uuid::Uuid::parse_str(&value).ok());
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
            && Some(value.sha256.clone()) == text(&sheet, reference, "sha256")
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
    let required = registration["required_artifact_roles"]["attempt"]
        .as_array()
        .ok_or_else(|| internal(request, "submission required roles"))?;
    let missing: Vec<&str> = required
        .iter()
        .filter_map(Value::as_str)
        .filter(|role| !roles.contains(*role))
        .collect();
    if !missing.is_empty() {
        let message = format!(
            "the manifest lacks the required artifact role{} {}: a run must cite a manifest with an object of each role the science revision requires ({})",
            if missing.len() == 1 { "" } else { "s" },
            missing.join(", "),
            required
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        );
        return Ok(Err(DomainError::new(
            ErrorCode::ValidationFailed,
            message.clone(),
        )
        .with_details(json!([{
            "path": "/manifest",
            "message": message,
            "missing_roles": missing,
            "required_roles": required,
        }]))));
    }
    let limit = registration["limits"]["report_max_bytes"]
        .as_u64()
        .ok_or_else(|| internal(request, "submission report limit"))?;
    if u64::try_from(parsed.body.len()).map_or(true, |bytes| bytes > limit) {
        return Ok(Err(invalid(
            "body/document",
            "the run notes exceed the configured byte limit",
        )));
    }
    Ok(Ok(Run {
        front_matter: sheet,
        body: parsed.body,
        manifest,
    }))
}
async fn failed(
    c: &mut PgConnection,
    principal: &Principal,
    attempt: &Attempt,
    error: &DomainError,
    profile: &SubmissionContext,
    request: &RequestContext,
) -> Result<(), Failure> {
    let details = job_lifecycle::document(
        &json!({"errors":error.details}),
        &profile.lifecycle,
        request,
    )?;
    crate::attempt_failure::agent(
        c,
        profile.lifecycle.attempts,
        principal,
        attempt,
        &crate::attempt_failure::Report {
            code: "invalid_submission",
            reason: &format!("invalid submission: {}", error.message),
            details: &details,
            log_refs: None,
            step: None,
            idempotency_key: None,
            manifest_sha256: None,
        },
        request,
    )
    .await
}
#[allow(
    clippy::too_many_lines,
    reason = "The lease, the run document, the verify queue and the audit commit as one submission"
)]
#[utoipa::path(
    post,
    path = "/api/projects/{slug}/units/{number}/attempts/{sequence}/submission",
    operation_id = "submit_api_projects__slug__units__number__attempts__sequence__submission_post",
    summary = "Submit",
    description = "Submit the run document of a completed run; the attempt is frozen on acceptance.\n\nThe document is Markdown with YAML front matter, checked against\n`GET /api/schemas/run`: the claims, provenance, artifact roles and verified\nmanifest, and optional run notes as the body. A run that failed releases\nthe attempt instead; front matter with a `status` is refused and leaves the\nattempt as it was. Any other invalid document fails the attempt for human\nreview, once the caller has shown it holds the lease.",
    params(("slug" = String, Path),
        ("number" = i64, Path),
        ("sequence" = i64, Path),
        ("X-Lease-Token" = Option<String>, Header),
        ("X-Lease-Generation" = Option<i64>, Header),
        ("idempotency-key" = Option<String>, Header)),
    request_body(content = crate::api_models::RunSubmission, content_type = "application/json"),
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
    let body = crate::api_contract::typed_body::<RunSubmission>(body).map_err(failure)?;
    let input = match &body {
        DecodedBody::Missing => BodyInput::Missing,
        DecodedBody::RawBytes => BodyInput::RawBytes,
        DecodedBody::Json(document) => BodyInput::Json(document),
    };
    let document = validate_run_submission(input).map_err(|error| {
        error.domain_error().map_or_else(
            |_| internal(&request_context, "submission request error encoding"),
            |error| failure(crate::errors::ApiError::from(error)),
        )
    })?;
    let DecodedBody::Json(submission) = body else {
        return Err(internal(&request_context, "submission body"));
    };
    let parsed = front_matter::parse(&document, Limits::default());
    if parsed
        .as_ref()
        .is_ok_and(|parsed| parsed.front_matter.contains_key("status"))
    {
        return Err(failure(crate::errors::ApiError::from(invalid(
            "body/document",
            "a run document has no status: release the attempt with a failure report when the run failed",
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
    let submission_root = envelope
        .import(&submission, submission.root())
        .map_err(|_| internal(&request_context, "submission hash document"))?;
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
            (String::from("submission"), submission_root),
        ]))
        .map_err(|_| internal(&request_context, "submission hash envelope"))?;
    let envelope = envelope
        .finish(root)
        .map_err(|_| internal(&request_context, "submission hash envelope"))?;
    let hash = Sha256::digest(
        canonical_bytes(&envelope, state.profile.nesting_budget)
            .map_err(|_| internal(&request_context, "submission hash"))?,
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
    let run = match check_run(&mut tx, &attempt, parsed, &state.profile, &request_context).await? {
        Ok(run) => run,
        Err(error) => {
            failed(
                &mut tx,
                &auth.principal,
                &attempt,
                &error,
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
    record_run(
        &mut tx,
        &auth.principal,
        &project,
        &attempt,
        &run,
        &format!("{:x}", Sha256::digest(document.as_bytes())),
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
    if let Some(store) = &state.profile.transcripts {
        crate::transcript_routes::seal(&mut tx, store, project.id, &attempt, &request_context)
            .await
            .map_err(failure)?;
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
    reason = "One transaction binds the run, its mentions, the queue and the submission audit"
)]
#[allow(
    clippy::too_many_lines,
    reason = "The run, mentions, freeze, verify queue and audit share one caller transaction"
)]
async fn record_run(
    c: &mut PgConnection,
    principal: &Principal,
    project: &cannery_projects::repo::Project,
    attempt: &Attempt,
    run: &Run,
    document_sha: &str,
    key: Option<&str>,
    state: &RouteState,
    request: &RequestContext,
) -> Result<(), Failure> {
    let profile = &state.profile;
    let manifest = &run.manifest;
    // The verifier reads the front matter alone; its job pins that digest.
    let claimed_sha = canonical_sha256(&run.front_matter, profile.nesting_budget)
        .map_err(|_| internal(request, "submission claims hash"))?;
    let evidence = Repository::new(c, profile.lifecycle.attempts)
        .add_evidence(AddEvidence {
            project_id: project.id,
            attempt_id: attempt.id,
            stage: "agent",
            status: "completed",
            content: &run.front_matter,
            body: &run.body,
            sha256: document_sha,
            manifest_id: Some(manifest.id),
            principal,
        })
        .await
        .map_err(|_| internal(request, "submission run insert"))?;
    let notes = job_lifecycle::document(
        &json!({"body_markdown": run.body}),
        &profile.lifecycle,
        request,
    )?;
    let mentions = crate::unit_mutations::resolve_mentions(
        c,
        principal,
        project,
        &notes,
        Some(attempt.unit_id),
        profile.nesting_budget,
        request,
    )
    .await
    .map_err(failure)?;
    cannery_units::repo::replace_mentions(
        c,
        cannery_units::repo::MentionSource::Report(cannery_units::repo::EvidenceId(evidence.0)),
        mentions,
    )
    .await
    .map_err(|_| internal(request, "submission mentions"))?;
    Repository::new(c, profile.lifecycle.attempts)
        .end_lease(attempt.id, "verifying")
        .await
        .map_err(|_| internal(request, "submission freeze"))?;
    let job = job_lifecycle::create_verify_job(
        c,
        attempt,
        &job_lifecycle::RunInputs {
            run: evidence.0,
            run_sha256: claimed_sha,
            manifest: manifest.id.0,
            manifest_sha256: manifest.sha256.clone(),
        },
        state.app.settings.leases.job_overhead_seconds.as_bigint(),
        cannery_jobs::repo::Origin::Submission,
        None,
        &profile.lifecycle,
        request,
    )
    .await?;
    let prior = json!({"state":attempt.state.as_str()});
    let new = json!({"state":"verifying","manifest_sha256":manifest.sha256,"run_sha256":document_sha,"job_id":job.id.0.to_string()});
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
    job_lifecycle::record_job_created(c, principal, attempt, &job, &profile.lifecycle, request)
        .await?;
    Ok(())
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
