//! Two ordinary job reads, installed only with explicit caller profiles.
use crate::{
    AppState,
    authentication::authenticate,
    job_read_wire::{self, ResponseContext},
    requests::RequestContext,
    validation,
};
use axum::{
    Router,
    extract::{FromRequestParts, Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use cannery_core::ids::JobId;
use cannery_jobs::repo;
use num_bigint::BigInt;
use std::{collections::BTreeMap, sync::Arc};
pub struct JobReadContext {
    pub jobs: repo::JsonContext,
    pub attempts: cannery_attempts::model::JsonContext,
    pub response: ResponseContext,
}
#[derive(Clone)]
pub(crate) struct RouteState {
    app: AppState,
    profile: Arc<JobReadContext>,
}
pub(crate) struct Failure(Box<Response>);
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        *self.0
    }
}
fn failure(v: impl IntoResponse) -> Failure {
    Failure(Box::new(v.into_response()))
}
fn missing(message: String) -> Failure {
    failure(crate::errors::ApiError::from(
        cannery_core::errors::DomainError::new(cannery_core::errors::ErrorCode::NotFound, message),
    ))
}
fn internal(c: &RequestContext) -> Failure {
    failure(c.internal("job read"))
}
pub fn routes(app: AppState, profile: Arc<JobReadContext>) -> Router {
    Router::new()
        .route(
            "/api/projects/{slug}/jobs/{job_id}",
            get(detail).head(head).fallback(method),
        )
        .route(
            "/api/projects/{slug}/units/{number}/attempts/{sequence}/jobs",
            get(list).head(head).fallback(method),
        )
        .with_state(RouteState { app, profile })
}
async fn head() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [
            (header::ALLOW, "GET"),
            (header::CONTENT_TYPE, "application/json"),
            (header::CONTENT_LENGTH, "31"),
        ],
    )
}
async fn method() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "GET")],
        axum::Json(serde_json::json!({"detail":"Method Not Allowed"})),
    )
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/jobs/{job_id}",
    operation_id = "get_job_api_projects__slug__jobs__job_id__get",
    summary = "Get Job",
    params(("slug" = String, Path),
        ("job_id" = String, Path, format = "uuid")),
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
pub(crate) async fn detail(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    read(state, context, request, false).await
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/units/{number}/attempts/{sequence}/jobs",
    operation_id = "list_attempt_jobs_api_projects__slug__units__number__attempts__sequence__jobs_get",
    summary = "List Attempt Jobs",
    description = "The attempt's verify jobs, by run number.",
    params(("slug" = String, Path),
        ("number" = i64, Path),
        ("sequence" = i64, Path),
        ("before" = Option<String>, Query, description = "Continue after this job id.", format = "uuid"),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_JobOut_UUID_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn list(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    read(state, context, request, true).await
}
#[allow(
    clippy::too_many_lines,
    reason = "Retain source validation, lookup and model construction order"
)]
async fn read(
    state: RouteState,
    context: RequestContext,
    request: Request,
    collection: bool,
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
    let mut errors = vec![];
    let mut number = None;
    let mut sequence = None;
    let mut id = None;
    let mut before = None;
    let mut limit = 50;
    if collection {
        for (name, destination) in [("number", &mut number), ("sequence", &mut sequence)] {
            if let Some(raw) = paths.get(name) {
                match validation::attempt_path_integer(name, raw) {
                    Ok(value) => *destination = Some(value),
                    Err(e) => errors.extend(e.problems().iter().cloned()),
                }
            }
        }
        let pairs = query
            .pairs()
            .iter()
            .filter(|(k, _)| matches!(k.as_str(), "before" | "limit"))
            .map(|(k, v)| (String::from(k), String::from(v)))
            .collect::<Vec<_>>();
        match validation::review_attention_parameters(None, &pairs, true, false) {
            Ok(v) => {
                before = v.before.map(|v| JobId(v.0));
                limit = v.limit;
            }
            Err(e) => errors.extend(e.problems().iter().cloned()),
        }
    } else if let Some(raw) = paths.get("job_id") {
        match validation::job_uuid(raw) {
            Ok(value) => id = Some(JobId(value)),
            Err(e) => errors.extend(e.problems().iter().cloned()),
        }
    }
    if !errors.is_empty() {
        return Err(validation::ValidationErrors::from_problems(errors)
            .domain_error()
            .map_or_else(
                |_| internal(&context),
                |e| failure(crate::errors::ApiError::from(e)),
            ));
    }
    let slug = paths.get("slug").ok_or_else(|| internal(&context))?;
    let project =
        cannery_projects::authz::project_read(&mut auth.connection, &auth.principal, slug)
            .await
            .map_err(|e| failure(context.project_error(e)))?;
    let rows = if collection {
        let n = number.as_ref().ok_or_else(|| internal(&context))?;
        let s = sequence.as_ref().ok_or_else(|| internal(&context))?;
        let attempt =
            cannery_attempts::repo::Repository::new(&mut auth.connection, state.profile.attempts)
                .get_attempt(project.id, n, s, false)
                .await
                .map_err(|_| internal(&context))?
                .ok_or_else(|| missing(format!("attempt #{n}.{s} not found")))?;
        repo::list_jobs(
            &mut auth.connection,
            attempt.id,
            before,
            Some(&BigInt::from(limit + 1)),
            state.profile.jobs,
        )
        .await
        .map_err(|_| internal(&context))?
    } else {
        let row = repo::get_job(
            &mut auth.connection,
            id.ok_or_else(|| internal(&context))?,
            false,
            state.profile.jobs,
        )
        .await
        .map_err(|_| internal(&context))?
        .filter(|j| j.project_id == project.id)
        .ok_or_else(|| missing("job not found".into()))?;
        vec![row]
    };
    let next = (collection && rows.len() > limit).then(|| rows[limit - 1].id);
    let mut output = vec![];
    for row in rows.iter().take(if collection { limit } else { 1 }) {
        let mut attempts =
            cannery_attempts::repo::Repository::new(&mut auth.connection, state.profile.attempts);
        let verification = if let Some(id) = row.evidence_id {
            attempts
                .get_output_by_id(row.attempt_id, cannery_attempts::model::EvidenceId(id.0))
                .await
                .map_err(|_| internal(&context))?
                .map(|v| (v.0, v.1))
        } else {
            None
        };
        let projected =
            job_read_wire::prepare(row, state.profile.response).map_err(|_| internal(&context))?;
        let mut artifacts = attempts
            .list_job_artifacts(row.id)
            .await
            .map_err(|_| internal(&context))?;
        // A completed verify job's outputs are what its completion manifest
        // lists, not every upload made under its output prefix.
        if row.phase == repo::Phase::Verify
            && row.state == repo::State::Completed
            && let Some(id) = row.manifest_id
            && let Some(manifest) = attempts
                .get_manifest(row.attempt_id, cannery_attempts::model::ManifestId(id.0))
                .await
                .map_err(|_| internal(&context))?
        {
            crate::job_outputs::keep_listed(&manifest.content, &mut artifacts);
        }
        output.push(
            job_read_wire::job(
                row,
                projected,
                verification
                    .as_ref()
                    .map(|(front, body)| (front, body.as_str())),
                &artifacts,
                state.profile.response,
            )
            .map_err(|_| internal(&context))?,
        );
    }
    let bytes = if collection {
        job_read_wire::page(output, next).map_err(|_| internal(&context))?
    } else {
        output.pop().ok_or_else(|| internal(&context))?
    };
    Ok(([(header::CONTENT_TYPE, "application/json")], bytes).into_response())
}
