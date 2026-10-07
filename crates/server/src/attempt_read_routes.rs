//! Attempt read adapters retain explicit profiles and caller connection histories.
use crate::{
    AppState,
    attempt_read_request::{self, Operation},
    attempt_read_wire::{self, ResponseContext},
    authentication::authenticate,
    errors::ApiError,
    requests::RequestContext,
};
use axum::{
    Router,
    extract::{FromRequestParts, Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use cannery_attempts::{
    model::{JsonContext, StoredJson},
    repo::Repository,
};
use cannery_core::errors::{DomainError, ErrorCode};
use num_bigint::BigInt;
use std::{collections::BTreeMap, sync::Arc};
/// Caller-selected profiles; production pool installation is a separate gate.
pub struct AttemptReadContext {
    pub repository: JsonContext,
    pub response: ResponseContext,
    pub hypothesis_lookup: crate::attempt_read_lookup::LookupContext,
}
#[derive(Clone)]
pub(crate) struct RouteState {
    app: AppState,
    context: Arc<AttemptReadContext>,
}
pub(crate) struct Failure(Box<Response>);
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        *self.0
    }
}
fn failure(value: impl IntoResponse) -> Failure {
    Failure(Box::new(value.into_response()))
}
fn internal(context: &RequestContext, operation: &'static str) -> Failure {
    failure(context.internal(operation))
}
fn missing(message: String) -> Failure {
    failure(ApiError::from(DomainError::new(
        ErrorCode::NotFound,
        message,
    )))
}
/// Install only the three read endpoints.
pub fn routes(app: AppState, context: Arc<AttemptReadContext>) -> Router {
    Router::new()
        .route(
            "/api/projects/{slug}/hypotheses/{number}/attempts",
            get(hypothesis).head(head),
        )
        .route("/api/projects/{slug}/attempts", get(project).head(head))
        .route(
            "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}",
            get(detail).head(head),
        )
        .with_state(RouteState { app, context })
}
async fn head() -> impl IntoResponse {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "GET")])
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/hypotheses/{number}/attempts",
    operation_id = "list_attempts_api_projects__slug__hypotheses__number__attempts_get",
    summary = "List Attempts",
    description = "The hypothesis's attempts, by sequence.",
    params(("slug" = String, Path),
        ("number" = i64, Path),
        ("before" = Option<i64>, Query, description = "Continue after this sequence.", minimum = 0, maximum = 2_147_483_647),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_AttemptOut_int_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn hypothesis(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    read(state, context, request, Operation::Hypothesis).await
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/attempts",
    operation_id = "list_project_attempts_api_projects__slug__attempts_get",
    summary = "List Project Attempts",
    description = "Every attempt of the project, most recently claimed first.",
    params(("slug" = String, Path),
        ("state" = Option<Vec<crate::api_models::AttemptState>>, Query),
        ("track" = Option<String>, Query),
        ("before" = Option<String>, Query, description = "Continue after this attempt id.", format = "uuid"),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_AttemptOut_UUID_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn project(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    read(state, context, request, Operation::Project).await
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}",
    operation_id = "get_attempt_api_projects__slug__hypotheses__number__attempts__sequence__get",
    summary = "Get Attempt",
    params(("slug" = String, Path),
        ("number" = i64, Path),
        ("sequence" = i64, Path)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::AttemptDetail, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn detail(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    read(state, context, request, Operation::Detail).await
}
#[allow(
    clippy::too_many_lines,
    reason = "Source dependency, lookup and detail model construction order"
)]
async fn read(
    state: RouteState,
    context: RequestContext,
    request: Request,
    operation: Operation,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(failure)?;
    let paths = Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &state)
        .await
        .map_err(failure)?
        .0;
    let query =
        crate::request_context::QueryParams::parse(parts.uri.query().unwrap_or("").as_bytes());
    let params = attempt_read_request::parse(
        paths.get("number").map(String::as_str),
        paths.get("sequence").map(String::as_str),
        &query,
        operation,
    )
    .map_err(|error| {
        error.domain_error().map_or_else(
            |_| internal(&context, "attempt validation"),
            |error| failure(ApiError::from(error)),
        )
    })?;
    let slug = paths
        .get("slug")
        .ok_or_else(|| internal(&context, "attempt slug"))?;
    let project =
        cannery_projects::authz::project_read(&mut auth.connection, &auth.principal, slug)
            .await
            .map_err(|error| failure(context.project_error(error)))?;
    let bytes = match operation {
        Operation::Hypothesis => {
            let number = params
                .number
                .as_ref()
                .ok_or_else(|| internal(&context, "attempt hypothesis path"))?;
            let hypothesis = crate::attempt_read_lookup::hypothesis(
                &mut auth.connection,
                project.id,
                number,
                state.context.hypothesis_lookup,
            )
            .await
            .map_err(|_| internal(&context, "attempt hypothesis"))?
            .ok_or_else(|| missing(format!("hypothesis #{number} not found")))?;
            let rows = Repository::new(&mut auth.connection, state.context.repository)
                .list_attempts(
                    hypothesis,
                    params.after.as_ref(),
                    Some(&BigInt::from(params.limit + 1)),
                )
                .await
                .map_err(|_| internal(&context, "attempt list"))?;
            attempt_read_wire::page(&rows, params.limit, true, state.context.response)
        }
        Operation::Project => {
            let rows = Repository::new(&mut auth.connection, state.context.repository)
                .list_project_attempts(
                    project.id,
                    params.states.as_deref(),
                    params.track.as_deref(),
                    params.before,
                    &BigInt::from(params.limit + 1),
                )
                .await
                .map_err(|_| internal(&context, "project attempts"))?;
            attempt_read_wire::page(&rows, params.limit, false, state.context.response)
        }
        Operation::Detail => {
            let number = params
                .number
                .as_ref()
                .ok_or_else(|| internal(&context, "attempt number path"))?;
            let sequence = params
                .sequence
                .as_ref()
                .ok_or_else(|| internal(&context, "attempt sequence path"))?;
            let mut repository = Repository::new(&mut auth.connection, state.context.repository);
            let attempt = repository
                .get_attempt(project.id, number, sequence, false)
                .await
                .map_err(|_| internal(&context, "attempt lookup"))?
                .ok_or_else(|| missing(format!("attempt #{number}.{sequence} not found")))?;
            let failures = repository
                .list_failures(&[attempt.id])
                .await
                .map_err(|_| internal(&context, "attempt failures"))?
                .into_iter()
                .find(|(id, _)| *id == attempt.id)
                .map_or_else(Vec::new, |(_, v)| v);
            let sheet = repository
                .get_evidence(attempt.id, "agent")
                .await
                .map_err(|_| internal(&context, "attempt sheet"))?
                .map_or(StoredJson::SqlNull, |(_, v, _)| v);
            attempt_read_wire::validate_attempt(&attempt)
                .map_err(|_| internal(&context, "attempt base model"))?;
            let artifacts = repository
                .list_artifacts(attempt.id)
                .await
                .map_err(|_| internal(&context, "attempt artifacts"))?;
            attempt_read_wire::detail(
                &attempt,
                &artifacts,
                &failures,
                &sheet,
                state.context.response,
            )
        }
    }
    .map_err(|_| internal(&context, "attempt response model"))?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response())
}
