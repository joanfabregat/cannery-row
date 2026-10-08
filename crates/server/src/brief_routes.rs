// SPDX-License-Identifier: AGPL-3.0-only
//! The project brief: one Markdown document with YAML front matter per
//! revision, written by researchers and read by everyone working on the
//! project. Claims and jobs hand out the revision they run under.
use crate::{
    AppState,
    api_models::{BriefOut, BriefRef, BriefRevise, BriefRevisionOut, Page_BriefRevisionOut_int_},
    attempt_lease_routes::{Failure, domain, failure, internal},
    authentication::authenticate,
    body::{self, DecodedBody},
    errors::ApiError,
    requests::RequestContext,
    validation::{BodyInput, validate_brief_revise},
};
use axum::{
    Json, Router,
    extract::{FromRequestParts, Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use cannery_core::{
    audit::{self, Attribution, Record},
    contracts::phases::{Phase, PhaseDocumentError, PhaseSchemas},
    errors::{DomainError, ErrorCode},
    front_matter::{FrontMatterError, Limits},
    ids::AttemptId,
    principal::{Channel, Principal, Role},
};
use cannery_projects::{ProjectError, authz, briefs, repo};
use num_traits::ToPrimitive;
use sha2::{Digest, Sha256};
use sqlx::{Acquire, PgConnection};
use std::{collections::BTreeMap, sync::Arc};

/// The largest brief document, in UTF-8 bytes.
pub const BRIEF_MAX_BYTES: usize = 262_144;

#[derive(Clone)]
pub(crate) struct RouteState {
    app: AppState,
    schemas: Arc<PhaseSchemas>,
}

pub(crate) fn routes(app: AppState) -> Result<Router, crate::ServerError> {
    let schemas = PhaseSchemas::new().map_err(|_| crate::ServerError::Contracts)?;
    Ok(Router::new()
        .route(
            "/api/projects/{slug}/brief",
            get(current).post(revise).head(head_get),
        )
        .route(
            "/api/projects/{slug}/brief/revisions",
            get(revisions).head(head_get),
        )
        .route(
            "/api/projects/{slug}/brief/revisions/{revision}",
            get(revision).head(head_get),
        )
        .with_state(RouteState {
            app,
            schemas: Arc::new(schemas),
        }))
}

async fn head_get() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "GET")],
        Json(serde_json::json!({"detail":"Method Not Allowed"})),
    )
}

/// Where a revision is read, for claim and job responses.
fn revision_path(slug: &str, revision: i32) -> String {
    format!("/api/projects/{slug}/brief/revisions/{revision}")
}

/// The brief revision an attempt pinned at its claim, as handed to its claimant
/// and to the jobs that verify it.
/// # Errors
/// Returns a sanitized persistence error.
pub(crate) async fn pinned(
    conn: &mut PgConnection,
    slug: &str,
    attempt: AttemptId,
) -> Result<Option<BriefRef>, ProjectError> {
    Ok(briefs::pinned_brief(conn, attempt)
        .await?
        .map(|pinned| BriefRef {
            revision: i64::from(pinned.revision),
            sha256: pinned.sha256,
            r#ref: revision_path(slug, pinned.revision),
        }))
}

fn channel_name(channel: Channel) -> &'static str {
    match channel {
        Channel::Ui => "ui",
        Channel::Api => "api",
        Channel::Mcp => "mcp",
        Channel::Cli => "cli",
        Channel::System => "system",
    }
}

fn text_field(front_matter: &serde_json::Value, key: &str) -> String {
    front_matter
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn brief_out(brief: briefs::Brief, context: &RequestContext) -> Result<BriefOut, Failure> {
    let front_matter: serde_json::Value = serde_json::from_str(&brief.front_matter)
        .map_err(|_| internal(context, "brief front matter"))?;
    let serde_json::Value::Object(fields) = &front_matter else {
        return Err(internal(context, "brief front matter"));
    };
    Ok(BriefOut {
        revision: i64::from(brief.revision),
        title: text_field(&front_matter, "title"),
        goal: text_field(&front_matter, "goal"),
        front_matter: fields
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        body: brief.body,
        document: brief.document,
        sha256: brief.sha256,
        created_by: brief.created_by.to_string(),
        created_by_name: brief.created_by_name,
        via_channel: brief.via_channel,
        via_client: brief.via_client,
        created_at: crate::timestamps::public_timestamp(brief.created_at),
    })
}

fn revision_out(
    brief: briefs::Brief,
    context: &RequestContext,
) -> Result<BriefRevisionOut, Failure> {
    let out = brief_out(brief, context)?;
    Ok(BriefRevisionOut {
        revision: out.revision,
        title: out.title,
        goal: out.goal,
        sha256: out.sha256,
        created_by: out.created_by,
        created_by_name: out.created_by_name,
        via_channel: out.via_channel,
        via_client: out.via_client,
        created_at: out.created_at,
    })
}

async fn path_parameters(
    parts: &mut axum::http::request::Parts,
    state: &RouteState,
) -> Result<BTreeMap<String, String>, Failure> {
    Path::<BTreeMap<String, String>>::from_request_parts(parts, state)
        .await
        .map(|Path(values)| values)
        .map_err(failure)
}

fn invalid(path: &str, message: impl Into<String>) -> Failure {
    failure(ApiError::from(
        DomainError::new(ErrorCode::ValidationFailed, "invalid brief")
            .with_details(serde_json::json!([{"path": path, "message": message.into()}])),
    ))
}

fn not_found() -> Failure {
    domain(ErrorCode::NotFound, "the project has no brief yet")
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/brief",
    operation_id = "get_brief_api_projects__slug__brief_get",
    summary = "Get Brief",
    description = "The project's current brief: its latest revision. `404 not_found` until a\nresearcher writes the first one.",
    params(("slug" = String, Path)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::BriefOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn current(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(failure)?;
    let paths = path_parameters(&mut parts, &state).await?;
    let project = authz::project_read(&mut auth.connection, &auth.principal, &paths["slug"])
        .await
        .map_err(|error| failure(context.project_error(error)))?;
    let brief = briefs::get_brief(&mut auth.connection, project.id, None)
        .await
        .map_err(|error| failure(context.project_error(error)))?
        .ok_or_else(not_found)?;
    Ok(Json(brief_out(brief, &context)?).into_response())
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/brief/revisions",
    operation_id = "list_brief_revisions_api_projects__slug__brief_revisions_get",
    summary = "List Brief Revisions",
    description = "Every revision of the project's brief, newest first, without their documents.",
    params(("slug" = String, Path),
        ("before" = Option<i64>, Query, description = "Continue before this revision.", minimum = 1, maximum = 2_147_483_647),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_BriefRevisionOut_int_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn revisions(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    use crate::validation::{Parameter, ParameterValue, validate_parameter_node};
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(failure)?;
    let paths = path_parameters(&mut parts, &state).await?;
    let query =
        crate::request_context::QueryParams::parse(parts.uri.query().unwrap_or("").as_bytes());
    let node = |value: Option<&str>| {
        value.map_or(cannery_core::json::Node::Null, |value| {
            cannery_core::json::Node::String(String::from(value))
        })
    };
    let mut errors: Option<crate::validation::ValidationErrors> = None;
    let mut collect = |result: Result<ParameterValue, crate::validation::ValidationErrors>| {
        result
            .map_err(|error| match &mut errors {
                Some(errors) => errors.append(error),
                None => errors = Some(error),
            })
            .ok()
    };
    let before = collect(validate_parameter_node(
        &node(query.get("before")),
        Parameter::ConfigBefore,
    ));
    let limit = collect(validate_parameter_node(
        &node(Some(query.get("limit").unwrap_or("50"))),
        Parameter::Limit,
    ));
    if let Some(errors) = errors {
        return Err(failure(ApiError::from(
            errors
                .domain_error()
                .map_err(|_| internal(&context, "brief query encoding"))?,
        )));
    }
    let (Some(ParameterValue::ConfigBefore(before)), Some(ParameterValue::Limit(limit))) =
        (before, limit)
    else {
        return Err(internal(&context, "brief query type"));
    };
    let before = before
        .map(|value| {
            value
                .to_i32()
                .ok_or_else(|| internal(&context, "brief cursor"))
        })
        .transpose()?;
    let project = authz::project_read(&mut auth.connection, &auth.principal, &paths["slug"])
        .await
        .map_err(|error| failure(context.project_error(error)))?;
    let fetch = i64::try_from(limit + 1).map_err(|_| internal(&context, "brief page limit"))?;
    let mut rows = briefs::list_briefs(&mut auth.connection, project.id, before, Some(fetch))
        .await
        .map_err(|error| failure(context.project_error(error)))?;
    let next_before = if rows.len() > limit {
        rows.truncate(limit);
        rows.last().map(|row| i64::from(row.revision))
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(|row| revision_out(row, &context))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(Page_BriefRevisionOut_int_ { items, next_before }).into_response())
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/brief/revisions/{revision}",
    operation_id = "get_brief_revision_api_projects__slug__brief_revisions__revision__get",
    summary = "Get Brief Revision",
    description = "One revision of the project's brief, as claims and jobs name it.",
    params(("slug" = String, Path),
        ("revision" = i64, Path)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::BriefOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn revision(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    use crate::validation::{Parameter, ParameterValue, validate_parameter_node};
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(failure)?;
    let paths = path_parameters(&mut parts, &state).await?;
    let value = validate_parameter_node(
        &cannery_core::json::Node::String(paths["revision"].clone()),
        Parameter::ConfigRevision,
    )
    .map_err(|error| {
        error.domain_error().map_or_else(
            |_| internal(&context, "brief revision encoding"),
            |error| failure(ApiError::from(error)),
        )
    })?;
    let ParameterValue::ConfigRevision(number) = value else {
        return Err(internal(&context, "brief revision type"));
    };
    let project = authz::project_read(&mut auth.connection, &auth.principal, &paths["slug"])
        .await
        .map_err(|error| failure(context.project_error(error)))?;
    let missing = || domain(ErrorCode::NotFound, "brief revision not found");
    let number = number
        .to_i32()
        .filter(|number| *number > 0)
        .ok_or_else(missing)?;
    let brief = briefs::get_brief(&mut auth.connection, project.id, Some(number))
        .await
        .map_err(|error| failure(context.project_error(error)))?
        .ok_or_else(missing)?;
    Ok(Json(brief_out(brief, &context)?).into_response())
}

fn document_error(error: &PhaseDocumentError) -> Failure {
    match error {
        PhaseDocumentError::FrontMatter(FrontMatterError::TooLarge) => invalid(
            "body/document",
            format!("the document exceeds {BRIEF_MAX_BYTES} bytes"),
        ),
        PhaseDocumentError::FrontMatter(error) => invalid("body/document", error.to_string()),
        PhaseDocumentError::Invalid(violations) => {
            let details = violations
                .iter()
                .map(|violation| {
                    let pointer = if violation.path.is_empty() {
                        "/"
                    } else {
                        violation.path.as_str()
                    };
                    serde_json::json!({
                        "path": "body/document",
                        "message": format!("front matter {pointer}: {}", violation.message),
                    })
                })
                .collect::<Vec<_>>();
            failure(ApiError::from(
                DomainError::new(ErrorCode::ValidationFailed, "invalid brief")
                    .with_details(serde_json::Value::Array(details)),
            ))
        }
    }
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/brief",
    operation_id = "revise_brief_api_projects__slug__brief_post",
    summary = "Revise Brief",
    description = "Write a new revision of the project's brief: one Markdown document with YAML\nfront matter, checked against `GET /api/schemas/brief`. Researchers only;\n`expected_revision` is the current revision, 0 for the first brief, and a\nstale one is `409 stale_revision`.",
    params(("slug" = String, Path)),
    request_body(content = crate::api_models::BriefRevise, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::BriefOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
#[allow(
    clippy::too_many_lines,
    reason = "Validation, the locked revision check, the insert and its audit stay in order"
)]
pub(crate) async fn revise(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = body::read_body(request).await.map_err(failure)?;
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(failure)?;
    let paths = path_parameters(&mut parts, &state).await?;
    let body = crate::api_contract::typed_body::<BriefRevise>(body).map_err(failure)?;
    let input = match &body {
        DecodedBody::Missing => BodyInput::Missing,
        DecodedBody::RawBytes => BodyInput::RawBytes,
        DecodedBody::Json(document) => BodyInput::Json(document),
    };
    let typed = validate_brief_revise(input).map_err(|error| {
        error.domain_error().map_or_else(
            |_| internal(&context, "brief request error encoding"),
            |error| failure(ApiError::from(error)),
        )
    })?;
    let expected = typed.expected_revision;
    let access = authz::project_access(
        &mut auth.connection,
        &auth.principal,
        &paths["slug"],
        Some(Role::Researcher),
        &[],
        true,
    )
    .await
    .map_err(|error| failure(context.project_error(error)))?;
    let Principal::User(user) = &auth.principal else {
        return Err(internal(&context, "brief author"));
    };
    let limits = Limits {
        max_bytes: BRIEF_MAX_BYTES,
        ..Limits::default()
    };
    let parsed = state
        .schemas
        .parse(Phase::Brief, &typed.document, limits)
        .map_err(|error| document_error(&error))?;
    let front_matter = serde_json::Value::Object(parsed.front_matter);
    let front_matter_text =
        serde_json::to_string(&front_matter).map_err(|_| internal(&context, "brief encoding"))?;
    let sha256 = format!("{:x}", Sha256::digest(typed.document.as_bytes()));
    let mut transaction = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "brief transaction"))?;
    if !repo::lock_project(&mut transaction, access.project.id)
        .await
        .map_err(|error| failure(context.project_error(error)))?
    {
        return Err(domain(ErrorCode::NotFound, "project not found"));
    }
    let current = briefs::current_revision(&mut transaction, access.project.id)
        .await
        .map_err(|error| failure(context.project_error(error)))?;
    if current != expected {
        return Err(domain(
            ErrorCode::StaleRevision,
            if current == 0 {
                String::from("the project has no brief yet; expected_revision is 0")
            } else {
                format!("the brief is at revision {current}")
            },
        ));
    }
    let revision = briefs::create_brief(
        &mut transaction,
        briefs::NewBrief {
            project_id: access.project.id,
            document: &typed.document,
            front_matter: &front_matter_text,
            body: &parsed.body,
            sha256: &sha256,
            created_by: user.user_id,
            via_channel: channel_name(user.via.channel),
            via_client: user.via.client.as_deref(),
        },
    )
    .await
    .map_err(|error| failure(context.project_error(error)))?;
    let subject = access.project.id.to_string();
    let prior = (current > 0).then(|| serde_json::json!({"revision": current}));
    let new_state = serde_json::json!({
        "revision": revision,
        "sha256": sha256,
        "title": front_matter["title"],
    });
    audit::record(
        &mut transaction,
        Attribution::Principal(&auth.principal),
        Record {
            action: "brief.revised",
            subject_type: "brief",
            subject_id: &subject,
            project_id: Some(access.project.id),
            prior_state: prior.as_ref(),
            new_state: Some(&new_state),
            reason: None,
            idempotency_key: None,
        },
    )
    .await
    .map_err(|_| internal(&context, "brief audit"))?;
    let stored = briefs::get_brief(&mut transaction, access.project.id, Some(revision))
        .await
        .map_err(|error| failure(context.project_error(error)))?
        .ok_or_else(|| internal(&context, "stored brief missing"))?;
    transaction
        .commit()
        .await
        .map_err(|_| internal(&context, "brief commit"))?;
    Ok((StatusCode::CREATED, Json(brief_out(stored, &context)?)).into_response())
}
