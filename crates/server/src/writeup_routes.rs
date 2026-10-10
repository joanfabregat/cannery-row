// SPDX-License-Identifier: AGPL-3.0-only
//! Write-ups: the queue of hypotheses waiting for one, a hypothesis's
//! write-up, and a researcher's write-up or skip. A researcher's write-up
//! claims the document job and completes it in one action; an agent claims
//! it with `POST /jobs/claims` and completes it like any job.
use crate::{
    AppState,
    api_models::{
        ClaimedDocumentInputs, RequestCommonContentRef, WriteupOut, WriteupQueueOut,
        WriteupRecordOut, WriteupRequest, WriteupSkipRequest,
    },
    attempt_lease_routes::{Failure, domain, failure, internal},
    authentication::authenticate,
    body,
    job_completion_routes::{JobLifecycleContext, invalid},
    requests::RequestContext,
};
use axum::{
    Json, Router,
    extract::{FromRequestParts, Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use cannery_attempts::repo::Repository;
use cannery_core::{
    audit::{self, Attribution, Record},
    errors::ErrorCode,
    ids::{HypothesisId, ProjectId},
    principal::{Principal, Role},
};
use cannery_jobs::repo::{self as jobs, Claimant, Job};
use cannery_projects::{authz, repo::Project};
use num_bigint::BigInt;
use sqlx::{Acquire, PgConnection};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone)]
pub(crate) struct RouteState {
    app: AppState,
    context: Arc<JobLifecycleContext>,
}

pub(crate) fn routes(app: AppState, context: Arc<JobLifecycleContext>) -> Router {
    Router::new()
        .route("/api/projects/{slug}/writeups", get(queue))
        .route(
            "/api/projects/{slug}/hypotheses/{number}/writeup",
            get(read).post(write),
        )
        .route(
            "/api/projects/{slug}/hypotheses/{number}/writeup/skip",
            post(skip),
        )
        .with_state(RouteState { app, context })
}

async fn paths(
    parts: &mut axum::http::request::Parts,
    state: &RouteState,
) -> Result<BTreeMap<String, String>, Failure> {
    Path::<BTreeMap<String, String>>::from_request_parts(parts, state)
        .await
        .map(|Path(values)| values)
        .map_err(failure)
}

fn number(paths: &BTreeMap<String, String>) -> Result<i32, Failure> {
    paths
        .get("number")
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid("path/number", "Input should be a positive integer"))
}

/// The hypothesis numbered `number`, locked when `lock`.
async fn hypothesis(
    c: &mut PgConnection,
    project: ProjectId,
    number: i32,
    lock: bool,
    r: &RequestContext,
) -> Result<(HypothesisId, String), Failure> {
    let row = if lock {
        sqlx::query!(
            "SELECT id AS \"id: uuid::Uuid\", state FROM hypotheses WHERE project_id=$1 AND number=$2 FOR UPDATE",
            project.0 as _,
            number
        )
        .fetch_optional(&mut *c)
        .await
        .map(|row| row.map(|row| (row.id, row.state)))
    } else {
        sqlx::query!(
            "SELECT id AS \"id: uuid::Uuid\", state FROM hypotheses WHERE project_id=$1 AND number=$2",
            project.0 as _,
            number
        )
        .fetch_optional(&mut *c)
        .await
        .map(|row| row.map(|row| (row.id, row.state)))
    }
    .map_err(|_| internal(r, "write-up hypothesis"))?;
    row.map(|(id, state)| (HypothesisId(id), state))
        .ok_or_else(|| {
            domain(
                ErrorCode::NotFound,
                format!("hypothesis #{number} not found"),
            )
        })
}

/// A hypothesis's write-up as the API shows it.
async fn model(
    c: &mut PgConnection,
    project: &Project,
    number: i32,
    s: &RouteState,
    r: &RequestContext,
) -> Result<WriteupOut, Failure> {
    let (id, state) = hypothesis(c, project.id, number, false, r).await?;
    let job = jobs::document_job(c, id, s.context.flow.jobs)
        .await
        .map_err(|_| internal(r, "write-up job"))?;
    let written = sqlx::query!(
        r#"SELECT p.id AS "id: uuid::Uuid", p.sha256 AS "sha256!", p.front_matter::text AS "front_matter!",
                  p.body AS "body!", p.producer_user AS "producer_user?: uuid::Uuid",
                  p.producer_service AS "producer_service?: uuid::Uuid",
                  p.created_at AS "created_at: cannery_core::timestamps::Timestamp", a.sequence
           FROM phase_outputs p JOIN attempts a ON a.id=p.attempt_id
           WHERE a.hypothesis_id=$1 AND p.stage='writeup' AND p.status='completed'
           ORDER BY p.created_at DESC, p.id DESC LIMIT 1"#,
        id.0 as _
    )
    .fetch_optional(&mut *c)
    .await
    .map_err(|_| internal(r, "write-up record"))?;
    let writeup = written
        .as_ref()
        .map(|row| {
            Ok::<_, Failure>(WriteupRecordOut {
                id: row.id.to_string(),
                sha256: row.sha256.clone(),
                front_matter: serde_json::from_str(&row.front_matter)
                    .map_err(|_| internal(r, "write-up front matter"))?,
                body_markdown: row.body.clone(),
                written_by_user: row.producer_user.map(|id| id.to_string()),
                written_by_service: row.producer_service.map(|id| id.to_string()),
                created_at: crate::timestamps::public_timestamp(row.created_at),
            })
        })
        .transpose()?;
    let Some(job) = job else {
        let row = written.ok_or_else(|| {
            domain(
                ErrorCode::NotFound,
                format!("hypothesis #{number} has no write-up"),
            )
        })?;
        return Ok(WriteupOut {
            hypothesis: i64::from(number),
            hypothesis_ref: format!("#{number}"),
            hypothesis_state: state,
            status: String::from("written"),
            job_id: None,
            attempt_ref: format!("#{number}.{}", row.sequence),
            claimed_by: None,
            claimed_by_user: None,
            inputs: None,
            context: None,
            writeup,
            skip_reason: None,
        });
    };
    let attempt = Repository::new(c, s.context.flow.attempts)
        .get_attempt_by_id(job.attempt_id, false)
        .await
        .map_err(|_| internal(r, "write-up attempt"))?
        .ok_or_else(|| internal(r, "write-up attempt invariant"))?;
    let inputs = crate::document_jobs::inputs(c, &attempt, r).await?;
    let (_, bundle) = crate::context_bundle::refs(c, project, attempt.id, r)
        .await
        .map_err(failure)?;
    let status = match job.state {
        jobs::State::Pending | jobs::State::Failed => "pending",
        jobs::State::Claimed => "claimed",
        jobs::State::Completed => "written",
        jobs::State::Skipped => "skipped",
    };
    Ok(WriteupOut {
        hypothesis: i64::from(number),
        hypothesis_ref: format!("#{number}"),
        hypothesis_state: state,
        status: String::from(status),
        job_id: Some(job.id.to_string()),
        attempt_ref: format!("#{number}.{}", attempt.sequence),
        claimed_by: job.claimed_by_service.map(|id| id.to_string()),
        claimed_by_user: job.claimed_by_user.map(|id| id.to_string()),
        inputs: Some(ClaimedDocumentInputs {
            attempts: inputs.attempts.clone(),
            verification: inputs
                .verification
                .as_ref()
                .map(|cited| RequestCommonContentRef {
                    r#ref: cited.id.to_string(),
                    sha256: cited.sha256.clone(),
                }),
        }),
        context: bundle.map(|bundle| format!("{}?phase=document", bundle.r#ref)),
        writeup: if job.state == jobs::State::Completed {
            writeup
        } else {
            None
        },
        skip_reason: (job.state == jobs::State::Skipped)
            .then(|| job.error_reason.clone())
            .flatten(),
    })
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/writeups",
    operation_id = "list_writeups_api_projects__slug__writeups_get",
    summary = "List Write-ups To Do",
    description = "The hypotheses waiting for their write-up, oldest first, with their\ndocument job: `pending` until an agent or a researcher claims it.",
    params(("slug" = String, Path),
        ("limit" = Option<i64>, Query, description = "Items to return.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = WriteupQueueOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn queue(
    State(s): State<RouteState>,
    axum::Extension(r): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&s.app, &r, &parts.headers, &parts.method)
        .await
        .map_err(failure)?;
    let paths = paths(&mut parts, &s).await?;
    let limit = match parts.uri.query().unwrap_or_default() {
        "" => 50,
        query => query
            .strip_prefix("limit=")
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| (1..=200).contains(value))
            .ok_or_else(|| invalid("query/limit", "Input should be an integer from 1 to 200"))?,
    };
    let project = authz::project_read(&mut auth.connection, &auth.principal, &paths["slug"])
        .await
        .map_err(|error| failure(r.project_error(error)))?;
    let (total, items) = cannery_attention::pending_writeups(
        &mut auth.connection,
        project.id,
        Some(&BigInt::from(limit)),
    )
    .await
    .map_err(|_| internal(&r, "write-up queue"))?;
    let items = items
        .iter()
        .map(crate::review_attention_wire::writeup_model)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| internal(&r, "write-up queue model"))?;
    Ok(Json(WriteupQueueOut { total, items }).into_response())
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/hypotheses/{number}/writeup",
    operation_id = "get_writeup_api_projects__slug__hypotheses__number__writeup_get",
    summary = "Get Write-up",
    description = "The hypothesis's write-up: `pending` or `claimed` while its document job\nwaits or is being written, `written` with the write-up, or `skipped` with\nthe researcher's reason. `inputs` is what the write-up covers and cites,\nand `context` the documenter's context bundle.",
    params(("slug" = String, Path), ("number" = i64, Path)),
    responses((status = 200, description = "Successful Response", body = WriteupOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn read(
    State(s): State<RouteState>,
    axum::Extension(r): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&s.app, &r, &parts.headers, &parts.method)
        .await
        .map_err(failure)?;
    let paths = paths(&mut parts, &s).await?;
    let number = number(&paths)?;
    let project = authz::project_read(&mut auth.connection, &auth.principal, &paths["slug"])
        .await
        .map_err(|error| failure(r.project_error(error)))?;
    let out = model(&mut auth.connection, &project, number, &s, &r).await?;
    Ok(Json(out).into_response())
}

/// The researcher, the project and the locked document job a mutation acts on.
struct Target {
    project: Project,
    attempt: cannery_attempts::model::Attempt,
    job: Job,
}

async fn target(
    c: &mut PgConnection,
    principal: &Principal,
    slug: &str,
    number: i32,
    s: &RouteState,
    r: &RequestContext,
) -> Result<Target, Failure> {
    let project = authz::project_access(c, principal, slug, Some(Role::Researcher), &[], true)
        .await
        .map_err(|error| failure(r.project_error(error)))?
        .project;
    let (id, state) = hypothesis(c, project.id, number, false, r).await?;
    let job = jobs::document_job(c, id, s.context.flow.jobs)
        .await
        .map_err(|_| internal(r, "write-up job"))?
        .filter(|job| matches!(job.state, jobs::State::Pending | jobs::State::Claimed))
        .ok_or_else(|| {
            domain(
                ErrorCode::Conflict,
                format!("hypothesis #{number} is {state}; it does not wait for a write-up"),
            )
        })?;
    let attempt = Repository::new(c, s.context.flow.attempts)
        .get_attempt_by_id(job.attempt_id, true)
        .await
        .map_err(|_| internal(r, "write-up attempt lock"))?
        .ok_or_else(|| internal(r, "write-up attempt invariant"))?;
    let job = jobs::get_job(c, job.id, true, s.context.flow.jobs)
        .await
        .map_err(|_| internal(r, "write-up job lock"))?
        .ok_or_else(|| internal(r, "write-up job invariant"))?;
    Ok(Target {
        project,
        attempt,
        job,
    })
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/hypotheses/{number}/writeup",
    operation_id = "write_up_api_projects__slug__hypotheses__number__writeup_post",
    summary = "Write Up",
    description = "A researcher writes the hypothesis up: the document job is claimed and\ncompleted in one action. The write-up is Markdown with YAML front matter\n(``writeup.schema.json``): a one-sentence `summary`, the `attempts` it\ncovers (every attempt of the hypothesis) and the `verification` report it\ncites (null for a hypothesis stopped after a failure), as the write-up's\n`inputs` name them, and a body. The hypothesis then awaits its decision.\nA job another documenter holds is `409 conflict`.",
    params(("slug" = String, Path), ("number" = i64, Path)),
    request_body(content = WriteupRequest, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = WriteupOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn write(
    State(s): State<RouteState>,
    axum::Extension(r): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, decoded) = body::read_body(request).await.map_err(failure)?;
    let mut auth = authenticate(&s.app, &r, &parts.headers, &parts.method)
        .await
        .map_err(failure)?;
    let paths = paths(&mut parts, &s).await?;
    let number = number(&paths)?;
    let typed: WriteupRequest = body::typed(&decoded).map_err(failure)?;
    if typed.document.trim().is_empty() {
        return Err(invalid("/document", "the write-up is empty"));
    }
    let Principal::User(_) = &auth.principal else {
        return Err(domain(
            ErrorCode::Forbidden,
            "a researcher writes a hypothesis up here; an agent claims the document job",
        ));
    };
    let principal = auth.principal.clone();
    let ttl = s.app.settings.leases.job_ttl_seconds.as_bigint().clone();
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&r, "write-up transaction"))?;
    let result = async {
        let Target {
            project,
            attempt,
            job,
        } = target(&mut tx, &principal, &paths["slug"], number, &s, &r).await?;
        let job = match job.state {
            jobs::State::Pending => {
                let token = uuid::Uuid::new_v4().to_string();
                let claimed = jobs::claim_job(
                    &mut tx,
                    job.id,
                    &principal,
                    &cannery_identity::secrets::digest(&token),
                    &ttl,
                    s.context.flow.jobs,
                )
                .await
                .map_err(|_| internal(&r, "write-up claim"))?;
                audit::record(
                    &mut tx,
                    Attribution::Principal(&principal),
                    Record {
                        action: "job.claimed",
                        subject_type: "job",
                        subject_id: &claimed.id.to_string(),
                        project_id: Some(project.id),
                        prior_state: Some(&serde_json::json!({"state":"pending"})),
                        new_state: Some(&serde_json::json!({"state":"claimed",
                            "lease_generation":claimed.lease_generation,
                            "attempt_id":claimed.attempt_id.to_string(),
                            "run_number":claimed.run_number})),
                        reason: None,
                        idempotency_key: None,
                    },
                )
                .await
                .map_err(|_| internal(&r, "write-up claim audit"))?;
                claimed
            }
            _ if job.claimant() == Some(Claimant::of(&principal)) => job,
            _ => {
                return Err(domain(
                    ErrorCode::Conflict,
                    format!("another documenter is writing hypothesis #{number} up"),
                ));
            }
        };
        let inputs = crate::document_jobs::inputs(&mut tx, &attempt, &r).await?;
        let writeup =
            crate::document_jobs::check(&typed.document, &inputs, &s.context.phases, "/document")?;
        crate::document_jobs::publish(
            &mut tx,
            &principal,
            &attempt,
            &job,
            &inputs,
            &writeup,
            None,
            &s.context.flow,
            &r,
        )
        .await?;
        model(&mut tx, &project, number, &s, &r).await
    }
    .await;
    let (response, rollback_failed) = finish(tx, result, StatusCode::CREATED, &r).await;
    if rollback_failed {
        auth.connection.close_on_drop();
    }
    response
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/hypotheses/{number}/writeup/skip",
    operation_id = "skip_writeup_api_projects__slug__hypotheses__number__writeup_skip_post",
    summary = "Skip Write-up",
    description = "A researcher skips the hypothesis's write-up, waiting or being written,\nwith a reason: the hypothesis then awaits its decision without one, and\nits decision shows \"No write-up: <reason>\". Nothing skips a write-up\nautomatically.",
    params(("slug" = String, Path), ("number" = i64, Path)),
    request_body(content = WriteupSkipRequest, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = WriteupOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn skip(
    State(s): State<RouteState>,
    axum::Extension(r): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, decoded) = body::read_body(request).await.map_err(failure)?;
    let mut auth = authenticate(&s.app, &r, &parts.headers, &parts.method)
        .await
        .map_err(failure)?;
    let paths = paths(&mut parts, &s).await?;
    let number = number(&paths)?;
    let typed: WriteupSkipRequest = body::typed(&decoded).map_err(failure)?;
    let reason = typed.reason.trim().to_owned();
    if reason.is_empty() || reason.chars().count() > 4000 {
        return Err(invalid("/reason", "say why, in 1 to 4000 characters"));
    }
    let Principal::User(_) = &auth.principal else {
        return Err(domain(
            ErrorCode::Forbidden,
            "only a researcher skips a write-up",
        ));
    };
    let principal = auth.principal.clone();
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&r, "write-up skip transaction"))?;
    let result = async {
        let Target {
            project,
            attempt,
            job,
        } = target(&mut tx, &principal, &paths["slug"], number, &s, &r).await?;
        let inputs = crate::document_jobs::inputs(&mut tx, &attempt, &r).await?;
        crate::document_jobs::skip(
            &mut tx,
            &principal,
            &attempt,
            &job,
            &inputs,
            &reason,
            None,
            &s.context.flow,
            &r,
        )
        .await?;
        model(&mut tx, &project, number, &s, &r).await
    }
    .await;
    let (response, rollback_failed) = finish(tx, result, StatusCode::OK, &r).await;
    if rollback_failed {
        auth.connection.close_on_drop();
    }
    response
}

async fn finish(
    tx: sqlx::Transaction<'_, sqlx::Postgres>,
    result: Result<WriteupOut, Failure>,
    status: StatusCode,
    r: &RequestContext,
) -> (Result<Response, Failure>, bool) {
    match result {
        Ok(out) => (
            tx.commit()
                .await
                .map(|()| (status, Json(out)).into_response())
                .map_err(|_| internal(r, "write-up commit")),
            false,
        ),
        // A connection whose rollback failed never returns to the pool.
        Err(error) => (Err(error), tx.rollback().await.is_err()),
    }
}
