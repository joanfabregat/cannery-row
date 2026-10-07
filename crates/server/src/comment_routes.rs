//! Comment reads preserve source target lookup and projection ordering.
use crate::{AppState, authentication::authenticate, errors::ApiError, requests::RequestContext};
use axum::{
    Router,
    extract::{FromRequestParts, Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use cannery_comments_reports::comments::{self, Comment, CommentId};
use cannery_core::{
    errors::{DomainError, ErrorCode},
    ids::{AttemptId, HypothesisId},
    json::Node,
    pg_integer::Integer,
};
use cannery_hypotheses::repo as hypotheses;
use num_bigint::BigInt;
use sqlx::PgConnection;
use std::{collections::BTreeMap, sync::Arc};

/// Caller-selected repository profiles; physical pooled history is bound separately.
pub struct CommentContext {
    pub hypotheses: hypotheses::JsonContext,
    pub mutations: Option<Arc<crate::comment_mutations::CommentMutationContext>>,
}
#[derive(Clone)]
pub(crate) struct RouteState {
    pub(crate) app: AppState,
    pub(crate) context: Arc<CommentContext>,
}
pub(crate) struct Failure(pub(crate) Box<Response>);
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        *self.0
    }
}
pub(crate) fn internal(c: &RequestContext, op: &'static str) -> Failure {
    Failure(Box::new(c.internal(op).into_response()))
}
pub(crate) fn domain(code: ErrorCode, message: impl Into<String>) -> Failure {
    Failure(Box::new(
        ApiError::from(DomainError::new(code, message)).into_response(),
    ))
}
pub(crate) fn invalid(e: &crate::validation::ValidationErrors, c: &RequestContext) -> Failure {
    e.domain_error().map_or_else(
        |_| internal(c, "comment validation encoding"),
        |e| Failure(Box::new(ApiError::from(e).into_response())),
    )
}
/// Install the four independently compared comment read operations.
pub fn routes(app: AppState, context: Arc<CommentContext>) -> Router {
    let router = Router::new()
        .route(
            "/api/projects/{slug}/hypotheses/{number}/comments",
            get(rest_list_hypothesis_comments).head(head_post),
        )
        .route(
            "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/comments",
            get(rest_list_attempt_comments).head(head_post),
        )
        .route(
            "/api/projects/{slug}/comments/{comment_id}",
            get(read).head(head_get),
        )
        .route(
            "/api/projects/{slug}/comments/{comment_id}/revisions",
            get(revisions).head(head_get),
        );
    let router = if context.mutations.is_some() {
        router
            .route(
                "/api/projects/{slug}/hypotheses/{number}/comments",
                axum::routing::post(crate::comment_mutations::rest_comment_on_hypothesis),
            )
            .route(
                "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/comments",
                axum::routing::post(crate::comment_mutations::rest_comment_on_attempt),
            )
            .route(
                "/api/projects/{slug}/comments/{comment_id}",
                axum::routing::put(crate::comment_mutations::edit),
            )
    } else {
        router
    };
    router.with_state(RouteState { app, context })
}
async fn head_post() -> impl IntoResponse {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST")])
}
async fn head_get() -> impl IntoResponse {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "GET")])
}
pub(crate) fn output(row: &Comment) -> crate::api_models::CommentOut {
    crate::api_models::CommentOut {
        id: row.id.0.to_string(),
        hypothesis: i64::from(row.hypothesis_number),
        hypothesis_ref: format!("#{}", row.hypothesis_number),
        attempt_ref: row
            .attempt_sequence
            .map(|sequence| format!("#{}.{sequence}", row.hypothesis_number)),
        author_user_id: row.author_user.to_string(),
        body_markdown: row.body_markdown.clone(),
        revision: i64::from(row.revision),
        created_at: crate::timestamps::public_timestamp(row.created_at),
        edited_at: row.edited_at.map(crate::timestamps::public_timestamp),
    }
}
pub(crate) fn response<T: serde::Serialize>(
    value: &T,
    c: &RequestContext,
) -> Result<Response, Failure> {
    let bytes = serde_json::to_vec(value).map_err(|_| internal(c, "comment response"))?;
    Ok(([(header::CONTENT_TYPE, "application/json")], bytes).into_response())
}
pub(crate) fn path_uuid(
    raw: Option<&String>,
    errors: &mut Vec<crate::validation::Problem>,
    c: &RequestContext,
) -> Result<Option<CommentId>, Failure> {
    let raw = raw.ok_or_else(|| internal(c, "comment path invariant"))?;
    match crate::validation::validate_parameter_node(
        &Node::String(String::from(raw)),
        crate::validation::Parameter::TokenId,
    ) {
        Ok(crate::validation::ParameterValue::TokenId(id)) => Ok(Some(CommentId(id.0))),
        Ok(_) => Err(internal(c, "comment UUID invariant")),
        Err(e) => {
            let mut problems = e.problems().to_vec();
            for p in &mut problems {
                p.loc[1] = crate::validation::Location::Field(String::from("comment_id"));
            }
            errors.extend(problems);
            Ok(None)
        }
    }
}
pub(crate) async fn paths(
    parts: &mut axum::http::request::Parts,
    state: &RouteState,
) -> Result<BTreeMap<String, String>, Failure> {
    Path::<BTreeMap<String, String>>::from_request_parts(parts, state)
        .await
        .map(|p| p.0)
        .map_err(|e| Failure(Box::new(e.into_response())))
}
fn paging(
    q: &crate::request_context::QueryParams,
    revisions: bool,
    mut errors: Vec<crate::validation::Problem>,
    c: &RequestContext,
) -> Result<(Option<uuid::Uuid>, Option<BigInt>, usize), Failure> {
    let mut before_uuid = None;
    let mut before_integer = None;
    if let Some(raw) = q.get("before") {
        if revisions {
            match crate::validation::bounded_query_integer(
                raw,
                "before",
                0,
                Some(i64::from(i32::MAX)),
            ) {
                Ok(v) => before_integer = Some(v),
                Err(e) => errors.extend(e.problems().iter().cloned()),
            }
        } else {
            match crate::validation::validate_parameter_node(
                &Node::String(String::from(raw)),
                crate::validation::Parameter::BeforeUuid,
            ) {
                Ok(crate::validation::ParameterValue::BeforeUuid(v)) => before_uuid = v,
                Ok(_) => return Err(internal(c, "comment cursor invariant")),
                Err(e) => errors.extend(e.problems().iter().cloned()),
            }
        }
    }
    let mut limit = 50;
    if let Some(raw) = q.get("limit") {
        match crate::validation::validate_parameter_node(
            &Node::String(String::from(raw)),
            crate::validation::Parameter::Limit,
        ) {
            Ok(crate::validation::ParameterValue::Limit(v)) => limit = v,
            Ok(_) => return Err(internal(c, "comment limit invariant")),
            Err(e) => errors.extend(e.problems().iter().cloned()),
        }
    }
    if errors.is_empty() {
        Ok((before_uuid, before_integer, limit))
    } else {
        Err(invalid(
            &crate::validation::ValidationErrors::from_problems(errors),
            c,
        ))
    }
}
pub(crate) async fn attempt_id(
    conn: &mut PgConnection,
    hypothesis: HypothesisId,
    sequence: &BigInt,
    c: &RequestContext,
) -> Result<Option<AttemptId>, Failure> {
    let integer = Integer::new(sequence).map_err(|_| internal(c, "comment sequence encoding"))?;
    let row = sqlx::query!(
        "SELECT id AS \"id!: AttemptId\" FROM attempts WHERE hypothesis_id=$1 AND sequence=$2",
        hypothesis as _,
        integer as _
    )
    .fetch_optional(conn)
    .await
    .map_err(|_| internal(c, "comment target query"))?;
    Ok(row.map(|row| row.id))
}

async fn list(
    State(s): State<RouteState>,
    axum::Extension(c): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&s.app, &c, &parts.headers, &parts.method)
        .await
        .map_err(|e| Failure(Box::new(e.into_response())))?;
    let paths = paths(&mut parts, &s).await?;
    let parsed = crate::validation::hypothesis_parameters(
        paths.get("number").map(String::as_str),
        paths.get("sequence").map(String::as_str),
        &[],
        false,
        false,
    );
    let q = crate::request_context::QueryParams::parse(parts.uri.query().unwrap_or("").as_bytes());
    let mut errors = Vec::new();
    let parsed = match parsed {
        Ok(parsed) => Some(parsed),
        Err(e) => {
            let mut problems = e.problems().to_vec();
            for p in &mut problems {
                if p.loc[1] == crate::validation::Location::Field(String::from("revision")) {
                    p.loc[1] = crate::validation::Location::Field(String::from("sequence"));
                }
            }
            errors.extend(problems);
            None
        }
    };
    let (before, _, limit) = paging(&q, false, errors, &c)?;
    let parsed = parsed.ok_or_else(|| internal(&c, "comment validation invariant"))?;
    let slug = paths
        .get("slug")
        .ok_or_else(|| internal(&c, "comment slug"))?;
    let project =
        cannery_projects::authz::project_read(&mut auth.connection, &auth.principal, slug)
            .await
            .map_err(|e| Failure(Box::new(c.project_error(e).into_response())))?;
    let number = parsed
        .number
        .ok_or_else(|| internal(&c, "comment number"))?;
    let hyp = hypotheses::get_hypothesis(
        &mut auth.connection,
        project.id,
        &number,
        false,
        s.context.hypotheses,
    )
    .await
    .map_err(|_| internal(&c, "comment hypothesis"))?
    .ok_or_else(|| {
        domain(
            ErrorCode::NotFound,
            format!("hypothesis #{number} not found"),
        )
    })?;
    let attempt = if let Some(sequence) = parsed.revision {
        Some(
            attempt_id(&mut auth.connection, hyp.id, &sequence, &c)
                .await?
                .ok_or_else(|| {
                    domain(
                        ErrorCode::NotFound,
                        format!("attempt #{number}.{sequence} not found"),
                    )
                })?,
        )
    } else {
        None
    };
    let rows = comments::list_comments(
        &mut auth.connection,
        hyp.id,
        attempt,
        before.map(CommentId),
        Some(&BigInt::from(limit + 1)),
    )
    .await
    .map_err(|_| internal(&c, "comment list"))?;
    let page = &rows[..rows.len().min(limit)];
    response(
        &crate::api_models::Page_CommentOut_UUID_ {
            items: page.iter().map(output).collect(),
            next_before: if rows.len() > limit {
                page.last().map(|r| r.id.0.to_string())
            } else {
                None
            },
        },
        &c,
    )
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/comments/{comment_id}",
    operation_id = "get_comment_api_projects__slug__comments__comment_id__get",
    summary = "Get Comment",
    params(("slug" = String, Path),
        ("comment_id" = String, Path, format = "uuid")),
    responses((status = 200, description = "Successful Response", body = crate::api_models::CommentOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn read(
    State(s): State<RouteState>,
    axum::Extension(c): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&s.app, &c, &parts.headers, &parts.method)
        .await
        .map_err(|e| Failure(Box::new(e.into_response())))?;
    let paths = paths(&mut parts, &s).await?;
    let mut errors = Vec::new();
    let id = path_uuid(paths.get("comment_id"), &mut errors, &c)?;
    if !errors.is_empty() {
        return Err(invalid(
            &crate::validation::ValidationErrors::from_problems(errors),
            &c,
        ));
    }
    let id = id.ok_or_else(|| internal(&c, "comment validated UUID invariant"))?;
    let project = cannery_projects::authz::project_read(
        &mut auth.connection,
        &auth.principal,
        paths
            .get("slug")
            .ok_or_else(|| internal(&c, "comment slug"))?,
    )
    .await
    .map_err(|e| Failure(Box::new(c.project_error(e).into_response())))?;
    let row = comments::get_comment(&mut auth.connection, project.id, id, false)
        .await
        .map_err(|_| internal(&c, "comment read"))?
        .ok_or_else(|| domain(ErrorCode::NotFound, "comment not found"))?;
    response(&output(&row), &c)
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/comments/{comment_id}/revisions",
    operation_id = "list_comment_revisions_api_projects__slug__comments__comment_id__revisions_get",
    summary = "List Comment Revisions",
    description = "Every body the comment had, oldest first.",
    params(("slug" = String, Path),
        ("comment_id" = String, Path, format = "uuid"),
        ("before" = Option<i64>, Query, description = "Continue after this revision.", minimum = 0, maximum = 2_147_483_647),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_CommentRevisionOut_int_, content_type = "application/json"),
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
    State(s): State<RouteState>,
    axum::Extension(c): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&s.app, &c, &parts.headers, &parts.method)
        .await
        .map_err(|e| Failure(Box::new(e.into_response())))?;
    let paths = paths(&mut parts, &s).await?;
    let mut errors = Vec::new();
    let id = path_uuid(paths.get("comment_id"), &mut errors, &c)?;
    let q = crate::request_context::QueryParams::parse(parts.uri.query().unwrap_or("").as_bytes());
    let (_, before, limit) = paging(&q, true, errors, &c)?;
    let id = id.ok_or_else(|| internal(&c, "comment validated UUID invariant"))?;
    let project = cannery_projects::authz::project_read(
        &mut auth.connection,
        &auth.principal,
        paths
            .get("slug")
            .ok_or_else(|| internal(&c, "comment slug"))?,
    )
    .await
    .map_err(|e| Failure(Box::new(c.project_error(e).into_response())))?;
    let row = comments::get_comment(&mut auth.connection, project.id, id, false)
        .await
        .map_err(|_| internal(&c, "comment read"))?
        .ok_or_else(|| domain(ErrorCode::NotFound, "comment not found"))?;
    let rows = comments::list_revisions(
        &mut auth.connection,
        row.id,
        before.as_ref(),
        Some(&BigInt::from(limit + 1)),
    )
    .await
    .map_err(|_| internal(&c, "comment revisions"))?;
    let page = &rows[..rows.len().min(limit)];
    let items = page
        .iter()
        .map(|r| crate::api_models::CommentRevisionOut {
            revision: i64::from(r.revision),
            body_markdown: r.body_markdown.clone(),
            via_channel: r.via_channel.clone(),
            via_client: r.via_client.clone(),
            created_at: crate::timestamps::public_timestamp(r.created_at),
        })
        .collect();
    response(
        &crate::api_models::Page_CommentRevisionOut_int_ {
            items,
            next_before: if rows.len() > limit {
                page.last().map(|r| i64::from(r.revision))
            } else {
                None
            },
        },
        &c,
    )
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/hypotheses/{number}/comments",
    operation_id = "list_hypothesis_comments_api_projects__slug__hypotheses__number__comments_get",
    summary = "List Hypothesis Comments",
    description = "Comments on the hypothesis and on its attempts, newest first.",
    params(("slug" = String, Path),
        ("number" = i64, Path),
        ("before" = Option<String>, Query, description = "Continue after this comment id.", format = "uuid"),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_CommentOut_UUID_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_list_hypothesis_comments(
    arg0: State<RouteState>,
    arg1: axum::Extension<RequestContext>,
    arg2: Request,
) -> Result<Response, Failure> {
    list(arg0, arg1, arg2).await
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/comments",
    operation_id = "list_attempt_comments_api_projects__slug__hypotheses__number__attempts__sequence__comments_get",
    summary = "List Attempt Comments",
    description = "Comments on the attempt, newest first.",
    params(("slug" = String, Path),
        ("number" = i64, Path),
        ("sequence" = i64, Path),
        ("before" = Option<String>, Query, description = "Continue after this comment id.", format = "uuid"),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_CommentOut_UUID_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_list_attempt_comments(
    arg0: State<RouteState>,
    arg1: axum::Extension<RequestContext>,
    arg2: Request,
) -> Result<Response, Failure> {
    list(arg0, arg1, arg2).await
}
