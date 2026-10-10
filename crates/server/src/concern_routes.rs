// SPDX-License-Identifier: AGPL-3.0-only
//! Plan concerns. Anyone working on a project (a member, a researcher or a
//! service account) raises a concern about a track's plan: a wrong
//! assumption, a better idea, a blocker. While one is open no new unit
//! of the track is claimed; work already claimed continues. A plan revision
//! answers it, or a researcher dismisses it with a reason.
use crate::{
    AppState,
    api_models::{ConcernDismissal, ConcernOut, ConcernRaise, Page_ConcernOut_UUID_},
    authentication::authenticate,
    body::DecodedBody,
    errors::ApiError,
    requests::RequestContext,
    unit_routes::{Failure, domain, internal},
};
use axum::{
    Json, Router,
    extract::{FromRequestParts, Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use cannery_core::{
    audit::{self, Attribution, Record},
    contracts::phases::{Phase, PhaseDocumentError, PhaseSchemas},
    errors::{DomainError, ErrorCode},
    front_matter::{FrontMatterError, Limits},
    ids::ConcernId,
    principal::{Channel, Principal, Role},
};
use cannery_projects::{authz, repo as projects};
use cannery_tracks::{
    concerns::{self, Concern, Filter},
    plans,
    repo::{self as tracks, RowLock, Track, TrackState},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Acquire, PgConnection};
use std::{collections::BTreeMap, sync::Arc};

/// The largest concern body, in UTF-8 bytes.
pub const CONCERN_BODY_MAX_BYTES: usize = 16_384;
/// Items per page when a list names no limit.
const PAGE: i64 = 50;
const STATES: [&str; 3] = ["open", "answered", "dismissed"];

#[derive(Clone)]
pub(crate) struct RouteState {
    app: AppState,
    schemas: Arc<PhaseSchemas>,
}

pub(crate) fn routes(app: AppState) -> Result<Router, crate::ServerError> {
    let schemas = PhaseSchemas::new().map_err(|_| crate::ServerError::Contracts)?;
    Ok(Router::new()
        .route(
            "/api/projects/{slug}/tracks/{track_slug}/concerns",
            get(track_list).post(raise),
        )
        .route("/api/projects/{slug}/concerns", get(list))
        .route("/api/projects/{slug}/concerns/{concern_id}", get(read))
        .route(
            "/api/projects/{slug}/concerns/{concern_id}/dismissal",
            post(dismiss),
        )
        .with_state(RouteState {
            app,
            schemas: Arc::new(schemas),
        }))
}

// ---------------------------------------------------------------- helpers

async fn paths(
    parts: &mut axum::http::request::Parts,
    state: &RouteState,
) -> Result<BTreeMap<String, String>, Failure> {
    Path::<BTreeMap<String, String>>::from_request_parts(parts, state)
        .await
        .map(|Path(values)| values)
        .map_err(Failure::new)
}

fn invalid(path: &str, message: impl Into<String>) -> Failure {
    let message = message.into();
    Failure::new(ApiError::from(
        DomainError::new(ErrorCode::ValidationFailed, message.clone())
            .with_details(json!([{"path": path, "message": message}])),
    ))
}

fn decode<T: serde::de::DeserializeOwned>(body: &DecodedBody) -> Result<T, Failure> {
    let value = match body {
        DecodedBody::Json(document) => cannery_core::json::to_value(document)
            .map_err(|_| invalid("body", "the body is not valid JSON"))?,
        DecodedBody::Missing => return Err(invalid("body", "Field required")),
        DecodedBody::RawBytes => return Err(invalid("body", "the body must be JSON")),
    };
    serde_json::from_value(value).map_err(|error| invalid("body", error.to_string()))
}

fn query_value<'a>(parts: &'a axum::http::request::Parts, name: &str) -> Option<&'a str> {
    parts.uri.query().and_then(|query| {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (key == name).then_some(value)
        })
    })
}

fn channel_name(channel: Channel) -> &'static str {
    match channel {
        Channel::Ui => "ui",
        Channel::Mcp => "mcp",
        Channel::Cli => "cli",
        Channel::Api | Channel::System => "api",
    }
}

fn track_error(context: &RequestContext) -> impl Fn(tracks::TrackError) -> Failure {
    move |_| internal(context, "concern persistence")
}

fn project_failure(context: &RequestContext) -> impl Fn(cannery_projects::ProjectError) -> Failure {
    move |error| Failure::new(context.project_error(error))
}

async fn load_track(
    conn: &mut PgConnection,
    project: &projects::Project,
    slug: &str,
    lock: Option<RowLock>,
    context: &RequestContext,
) -> Result<Track, Failure> {
    tracks::get_track(
        conn,
        project.id,
        &slug.to_owned(),
        lock,
        tracks::JsonContext {
            encode_nesting_budget: crate::body::REST_JSON_NESTING_BUDGET,
            decode_nesting_budget: crate::body::REST_JSON_NESTING_BUDGET,
        },
    )
    .await
    .map_err(|_| internal(context, "concern track load"))?
    .ok_or_else(|| domain(ErrorCode::NotFound, format!("track '{slug}' not found")))
}

fn concern_id(paths: &BTreeMap<String, String>) -> Result<ConcernId, Failure> {
    paths["concern_id"]
        .parse::<ConcernId>()
        .map_err(|_| invalid("path/concern_id", "Input should be a UUID"))
}

/// A concern as the API shows it.
pub(crate) fn concern_out(
    concern: Concern,
    context: &RequestContext,
) -> Result<ConcernOut, Failure> {
    let Value::Object(front_matter) = serde_json::from_str(&concern.front_matter)
        .map_err(|_| internal(context, "concern front matter"))?
    else {
        return Err(internal(context, "concern front matter"));
    };
    let (raised_by_kind, raised_by, raised_by_name) =
        match (concern.raised_by_user, concern.raised_by_service) {
            (Some(user), _) => ("user", user.to_string(), concern.raised_by_user_name),
            (None, Some(service)) => (
                "service",
                service.to_string(),
                concern.raised_by_service_name,
            ),
            (None, None) => return Err(internal(context, "concern raiser")),
        };
    Ok(ConcernOut {
        id: concern.id.to_string(),
        track: concern.track_slug,
        kind: concern.kind,
        unit: concern.unit_number.map(i64::from),
        attempt: concern.attempt_sequence.map(i64::from),
        front_matter: front_matter.into_iter().collect(),
        body: concern.body,
        sha256: concern.sha256,
        raised_by_kind: raised_by_kind.to_owned(),
        raised_by,
        raised_by_name,
        via_channel: concern.via_channel,
        via_client: concern.via_client,
        raised_at: crate::timestamps::public_timestamp(concern.raised_at),
        state: concern.state,
        answered_by_revision: concern.answered_by_revision.map(i64::from),
        dismissed_by_name: concern.dismissed_by_name,
        dismissal_reason: concern.dismissal_reason,
        closed_at: concern.closed_at.map(crate::timestamps::public_timestamp),
    })
}

fn document_error(error: &PhaseDocumentError) -> Failure {
    match error {
        PhaseDocumentError::FrontMatter(FrontMatterError::TooLarge) => {
            invalid("body/document", "the concern exceeds 1 MiB")
        }
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
                    json!({
                        "path": "body/document",
                        "message": format!("front matter {pointer}: {}", violation.message),
                    })
                })
                .collect::<Vec<_>>();
            Failure::new(ApiError::from(
                DomainError::new(ErrorCode::ValidationFailed, "invalid concern")
                    .with_details(Value::Array(details)),
            ))
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "One audit row: the concern, its action, its new state and the reason, under the caller's connection"
)]
async fn record(
    conn: &mut PgConnection,
    principal: &Principal,
    project: &projects::Project,
    concern: ConcernId,
    action: &'static str,
    new_state: &Value,
    reason: Option<&str>,
    context: &RequestContext,
) -> Result<(), Failure> {
    let subject = concern.to_string();
    audit::record(
        conn,
        Attribution::Principal(principal),
        Record {
            action,
            subject_type: "concern",
            subject_id: &subject,
            project_id: Some(project.id),
            prior_state: None,
            new_state: Some(new_state),
            reason,
            idempotency_key: None,
        },
    )
    .await
    .map(|_| ())
    .map_err(|_| internal(context, "concern audit"))
}

/// Close the open concerns an approved plan revision answers, with an audit
/// row each; returns their ids.
pub(crate) async fn answer_approved(
    conn: &mut PgConnection,
    principal: &Principal,
    project: &projects::Project,
    plan: &plans::PlanRevision,
    context: &RequestContext,
) -> Result<Vec<String>, Failure> {
    let answered = concerns::answer_open(conn, plan.id)
        .await
        .map_err(track_error(context))?;
    let new_state = json!({"state": "answered", "plan_revision": plan.revision});
    for concern in &answered {
        record(
            conn,
            principal,
            project,
            *concern,
            "concern.answered",
            &new_state,
            None,
            context,
        )
        .await?;
    }
    Ok(answered.iter().map(ToString::to_string).collect())
}

/// `state`, `before` and `limit` of a concern list.
fn list_query(
    parts: &axum::http::request::Parts,
) -> Result<(Option<&'static str>, Option<ConcernId>, i64), Failure> {
    let state = query_value(parts, "state")
        .map(|value| {
            STATES
                .into_iter()
                .find(|state| *state == value)
                .ok_or_else(|| {
                    invalid(
                        "query/state",
                        "Input should be 'open', 'answered' or 'dismissed'",
                    )
                })
        })
        .transpose()?;
    let before = query_value(parts, "before")
        .map(|value| {
            value
                .parse::<ConcernId>()
                .map_err(|_| invalid("query/before", "Input should be a UUID"))
        })
        .transpose()?;
    let limit = query_value(parts, "limit")
        .map(|value| {
            value
                .parse::<i64>()
                .ok()
                .filter(|value| (1..=200).contains(value))
                .ok_or_else(|| invalid("query/limit", "Input should be between 1 and 200"))
        })
        .transpose()?
        .unwrap_or(PAGE);
    Ok((state, before, limit))
}

async fn page(
    conn: &mut PgConnection,
    project: &projects::Project,
    filter: Filter<'_>,
    limit: i64,
    context: &RequestContext,
) -> Result<Page_ConcernOut_UUID_, Failure> {
    let mut rows = concerns::select(conn, project.id, filter, limit + 1)
        .await
        .map_err(track_error(context))?;
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let next_before = if rows.len() > limit {
        rows.truncate(limit);
        rows.last().map(|row| row.id.to_string())
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(|row| concern_out(row, context))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Page_ConcernOut_UUID_ { items, next_before })
}

// ------------------------------------------------------------------ routes

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/tracks/{track_slug}/concerns",
    operation_id = "raise_concern_api_projects__slug__tracks__track_slug__concerns_post",
    summary = "Raise Concern",
    description = "Raise a concern about the track's plan: a wrong assumption, a better idea,\na blocker. The document is Markdown with YAML front matter\n(`concern.schema.json`): its `kind` and, when it comes from one, the\n`unit` (number) and `attempt` (sequence) of the track, with the\nargument as its body (at most 16 KiB). Members, researchers and the\nproject's service accounts raise concerns. While one is open, no new\nunit of the track is claimed (`409 concern_open`); work already\nclaimed continues. A plan revision answers it, or a researcher dismisses it.",
    params(("slug" = String, Path), ("track_slug" = String, Path)),
    request_body(content = crate::api_models::ConcernRaise, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::ConcernOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
#[allow(
    clippy::too_many_lines,
    reason = "The document, the unit and attempt it names, the write and its audit row, in one transaction"
)]
pub(crate) async fn raise(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let paths = paths(&mut parts, &state).await?;
    let input: ConcernRaise = decode(&body)?;
    let project = authz::project_access(
        &mut auth.connection,
        &auth.principal,
        &paths["slug"],
        Some(Role::Member),
        &authz::ALL_SERVICES,
        true,
    )
    .await
    .map_err(project_failure(&context))?
    .project;
    let parsed = state
        .schemas
        .parse(Phase::Concern, &input.document, Limits::default())
        .map_err(|error| document_error(&error))?;
    if parsed.body.trim().is_empty() {
        return Err(invalid(
            "body/document",
            "the concern's body is empty: write the argument",
        ));
    }
    if parsed.body.len() > CONCERN_BODY_MAX_BYTES {
        return Err(invalid(
            "body/document",
            format!(
                "the concern's body is {} bytes; at most {CONCERN_BODY_MAX_BYTES}",
                parsed.body.len()
            ),
        ));
    }
    let front_matter = Value::Object(parsed.front_matter);
    let kind = front_matter["kind"]
        .as_str()
        .ok_or_else(|| internal(&context, "concern kind"))?
        .to_owned();
    let number = |key: &str| {
        front_matter
            .get(key)
            .and_then(Value::as_i64)
            .map(|value| i32::try_from(value).unwrap_or(i32::MAX))
    };
    let (number, sequence) = (number("unit"), number("attempt"));
    let (user, service, via) = match &auth.principal {
        Principal::User(user) => (Some(user.user_id), None, user.via.clone()),
        Principal::Service(service) => {
            (None, Some(service.service_account_id), service.via.clone())
        }
    };
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "concern transaction"))?;
    let track = load_track(
        &mut tx,
        &project,
        &paths["track_slug"],
        Some(RowLock::Update),
        &context,
    )
    .await?;
    if track.state == TrackState::Archived {
        return Err(domain(
            ErrorCode::Conflict,
            format!("track '{}' is archived and takes no concern", track.slug),
        ));
    }
    let mut unit_id = None;
    let mut attempt_id = None;
    if let Some(number) = number {
        let (_, unit) = plans::project_unit(&mut tx, project.id, number)
            .await
            .map_err(track_error(&context))?
            .filter(|(unit_track, _)| *unit_track == track.id)
            .ok_or_else(|| {
                invalid(
                    "body/document",
                    format!(
                        "front matter /unit: #{number} is not a unit of track '{}'",
                        track.slug
                    ),
                )
            })?;
        unit_id = Some(unit.unit_id);
        if let Some(sequence) = sequence {
            attempt_id = Some(
                plans::attempt_id(&mut tx, project.id, number, sequence)
                    .await
                    .map_err(track_error(&context))?
                    .ok_or_else(|| {
                        invalid(
                            "body/document",
                            format!(
                                "front matter /attempt: attempt #{number}.{sequence} not found"
                            ),
                        )
                    })?,
            );
        }
    }
    let front_matter_text =
        serde_json::to_string(&front_matter).map_err(|_| internal(&context, "concern encoding"))?;
    let sha256 = format!("{:x}", Sha256::digest(input.document.as_bytes()));
    let id = concerns::insert(
        &mut tx,
        concerns::NewConcern {
            project_id: project.id,
            track_id: track.id,
            kind: &kind,
            unit_id,
            attempt_id,
            front_matter: &front_matter_text,
            body: &parsed.body,
            sha256: &sha256,
            raised_by_user: user,
            raised_by_service: service,
            via_channel: channel_name(via.channel),
            via_client: via.client.as_deref(),
        },
    )
    .await
    .map_err(track_error(&context))?;
    let new_state = json!({
        "track": track.slug, "kind": kind, "unit": number, "attempt": sequence,
        "state": "open", "sha256": sha256,
    });
    record(
        &mut tx,
        &auth.principal,
        &project,
        id,
        "concern.raised",
        &new_state,
        None,
        &context,
    )
    .await?;
    let concern = concerns::get(&mut tx, project.id, id, false)
        .await
        .map_err(track_error(&context))?
        .ok_or_else(|| internal(&context, "raised concern"))?;
    let out = concern_out(concern, &context)?;
    tx.commit()
        .await
        .map_err(|_| internal(&context, "concern commit"))?;
    Ok((StatusCode::CREATED, Json(out)).into_response())
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/tracks/{track_slug}/concerns",
    operation_id = "list_track_concerns_api_projects__slug__tracks__track_slug__concerns_get",
    summary = "List Track Concerns",
    description = "The concerns raised about a track's plan, newest first.",
    params(("slug" = String, Path), ("track_slug" = String, Path),
        ("state" = Option<String>, Query, description = "Only concerns in this state: `open`, `answered` or `dismissed`."),
        ("before" = Option<String>, Query, description = "Continue before this concern.", format = "uuid"),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_ConcernOut_UUID_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn track_list(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let paths = paths(&mut parts, &state).await?;
    let (state_filter, before, limit) = list_query(&parts)?;
    let project = authz::project_read(&mut auth.connection, &auth.principal, &paths["slug"])
        .await
        .map_err(project_failure(&context))?;
    let track = load_track(
        &mut auth.connection,
        &project,
        &paths["track_slug"],
        None,
        &context,
    )
    .await?;
    let filter = Filter {
        track: Some(track.id),
        state: state_filter,
        before,
        ..Filter::default()
    };
    let out = page(&mut auth.connection, &project, filter, limit, &context).await?;
    Ok(Json(out).into_response())
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/concerns",
    operation_id = "list_concerns_api_projects__slug__concerns_get",
    summary = "List Concerns",
    description = "The concerns raised about the project's plans, newest first: of one track\nwhen `track` is given. `state=open` lists those waiting for a plan revision\nor a dismissal.",
    params(("slug" = String, Path),
        ("track" = Option<String>, Query, description = "Only concerns about this track's plan."),
        ("state" = Option<String>, Query, description = "Only concerns in this state: `open`, `answered` or `dismissed`."),
        ("before" = Option<String>, Query, description = "Continue before this concern.", format = "uuid"),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_ConcernOut_UUID_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn list(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let paths = paths(&mut parts, &state).await?;
    let (state_filter, before, limit) = list_query(&parts)?;
    let project = authz::project_read(&mut auth.connection, &auth.principal, &paths["slug"])
        .await
        .map_err(project_failure(&context))?;
    let track = match query_value(&parts, "track") {
        Some(slug) => Some(
            load_track(&mut auth.connection, &project, slug, None, &context)
                .await?
                .id,
        ),
        None => None,
    };
    let filter = Filter {
        track,
        state: state_filter,
        before,
        ..Filter::default()
    };
    let out = page(&mut auth.connection, &project, filter, limit, &context).await?;
    Ok(Json(out).into_response())
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/concerns/{concern_id}",
    operation_id = "get_concern_api_projects__slug__concerns__concern_id__get",
    summary = "Get Concern",
    description = "One concern: its document, who raised it, and how it was closed.",
    params(("slug" = String, Path), ("concern_id" = String, Path, format = "uuid")),
    responses((status = 200, description = "Successful Response", body = crate::api_models::ConcernOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn read(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let paths = paths(&mut parts, &state).await?;
    let id = concern_id(&paths)?;
    let project = authz::project_read(&mut auth.connection, &auth.principal, &paths["slug"])
        .await
        .map_err(project_failure(&context))?;
    let concern = concerns::get(&mut auth.connection, project.id, id, false)
        .await
        .map_err(track_error(&context))?
        .ok_or_else(|| domain(ErrorCode::NotFound, "concern not found"))?;
    Ok(Json(concern_out(concern, &context)?).into_response())
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/concerns/{concern_id}/dismissal",
    operation_id = "dismiss_concern_api_projects__slug__concerns__concern_id__dismissal_post",
    summary = "Dismiss Concern",
    description = "A researcher dismisses an open concern without revising the plan, with a\nreason the raiser and the track see. The track's units can be claimed\nagain once no concern about its plan is open.",
    params(("slug" = String, Path), ("concern_id" = String, Path, format = "uuid")),
    request_body(content = crate::api_models::ConcernDismissal, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = crate::api_models::ConcernOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn dismiss(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let paths = paths(&mut parts, &state).await?;
    let id = concern_id(&paths)?;
    let input: ConcernDismissal = decode(&body)?;
    let reason = input.reason.trim();
    if reason.is_empty() {
        return Err(invalid("body/reason", "a reason is required"));
    }
    if reason.len() > 4000 {
        return Err(invalid("body/reason", "a reason is at most 4000 bytes"));
    }
    let project = authz::project_access(
        &mut auth.connection,
        &auth.principal,
        &paths["slug"],
        Some(Role::Researcher),
        &[],
        true,
    )
    .await
    .map_err(project_failure(&context))?
    .project;
    let Principal::User(user) = &auth.principal else {
        return Err(domain(
            ErrorCode::Forbidden,
            "only researchers dismiss concerns",
        ));
    };
    let user_id = user.user_id;
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "concern transaction"))?;
    let concern = concerns::get(&mut tx, project.id, id, true)
        .await
        .map_err(track_error(&context))?
        .ok_or_else(|| domain(ErrorCode::NotFound, "concern not found"))?;
    if concern.state != "open" {
        return Err(domain(
            ErrorCode::Conflict,
            format!("the concern is already {}", concern.state),
        ));
    }
    concerns::dismiss(&mut tx, id, user_id, reason)
        .await
        .map_err(track_error(&context))?;
    let new_state = json!({"track": concern.track_slug, "state": "dismissed"});
    record(
        &mut tx,
        &auth.principal,
        &project,
        id,
        "concern.dismissed",
        &new_state,
        Some(reason),
        &context,
    )
    .await?;
    let concern = concerns::get(&mut tx, project.id, id, false)
        .await
        .map_err(track_error(&context))?
        .ok_or_else(|| internal(&context, "dismissed concern"))?;
    let out = concern_out(concern, &context)?;
    tx.commit()
        .await
        .map_err(|_| internal(&context, "concern commit"))?;
    Ok(Json(out).into_response())
}
