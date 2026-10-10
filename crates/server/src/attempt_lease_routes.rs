//! Attempt leases use the enclosing PostgreSQL transaction's real clock.
use crate::{
    AppState, authentication::authenticate, errors::ApiError, request_context::first_header,
    requests::RequestContext, validation,
};
use axum::{
    Router,
    extract::{FromRequestParts, Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use cannery_attempts::{
    model::{Attempt, JsonContext, State as AttemptState, is_claimant},
    repo::Repository,
};
use cannery_core::{
    errors::{DomainError, ErrorCode},
    principal::{Principal, Role, ServiceKind},
    timestamps::Timestamp,
};
use cannery_projects::{authz, repo::Project};
use num_bigint::BigInt;
use sqlx::{Acquire, PgConnection};
use std::{collections::BTreeMap, sync::Arc};

/// Entry-selected JSON and physical preparation profiles; no global defaults.
pub struct AttemptLeaseContext {
    pub repository: JsonContext,
    pub release: Option<Arc<crate::attempt_release_routes::AttemptReleaseContext>>,
}
#[derive(Clone)]
pub(crate) struct RouteState {
    pub app: AppState,
    pub context: Arc<AttemptLeaseContext>,
}
use crate::api_models::LeaseOut;
pub(crate) struct Failure(Box<Response>);
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        *self.0
    }
}
pub(crate) fn failure(value: impl IntoResponse) -> Failure {
    Failure(Box::new(value.into_response()))
}
pub(crate) fn internal(context: &RequestContext, operation: &'static str) -> Failure {
    failure(context.internal(operation))
}
pub(crate) fn domain(code: ErrorCode, message: impl Into<String>) -> Failure {
    failure(ApiError::from(DomainError::new(code, message)))
}

pub fn routes(app: AppState, context: Arc<AttemptLeaseContext>) -> Router {
    let router = Router::new().route(
        "/api/projects/{slug}/units/{number}/attempts/{sequence}/heartbeat",
        post(heartbeat),
    );
    let router = if context.release.is_some() {
        router.route(
            "/api/projects/{slug}/units/{number}/attempts/{sequence}/release",
            post(crate::attempt_release_routes::release),
        )
    } else {
        router
    };
    router.with_state(RouteState { app, context })
}

pub(crate) async fn worker_access(
    conn: &mut PgConnection,
    principal: &Principal,
    slug: &str,
    context: &RequestContext,
) -> Result<Project, Failure> {
    authz::project_access(
        conn,
        principal,
        slug,
        Some(Role::Researcher),
        &[ServiceKind::Agent, ServiceKind::Experimenter],
        true,
    )
    .await
    .map(|access| access.project)
    .map_err(|error| failure(context.project_error(error)))
}

pub(crate) struct LeaseParameters {
    number: BigInt,
    sequence: BigInt,
    token: Option<String>,
    generation: Option<BigInt>,
}
impl LeaseParameters {
    pub(crate) fn reference(&self) -> String {
        format!("{}.{}", self.number, self.sequence)
    }
}
pub(crate) fn parameters(
    paths: &BTreeMap<String, String>,
    headers: &axum::http::HeaderMap,
    context: &RequestContext,
) -> Result<LeaseParameters, Failure> {
    parameters_with_body(paths, headers, context, &[])
}
pub(crate) fn parameters_with_body(
    paths: &BTreeMap<String, String>,
    headers: &axum::http::HeaderMap,
    context: &RequestContext,
    body_errors: &[validation::Problem],
) -> Result<LeaseParameters, Failure> {
    parameters_with_path_errors(paths, headers, context, &[], body_errors)
}
pub(crate) fn parameters_with_path_errors(
    paths: &BTreeMap<String, String>,
    headers: &axum::http::HeaderMap,
    context: &RequestContext,
    path_errors: &[validation::Problem],
    body_errors: &[validation::Problem],
) -> Result<LeaseParameters, Failure> {
    let mut errors = Vec::new();
    let mut parse = |name, raw: Option<&str>, header: bool| {
        raw.and_then(|raw| {
            let result = if header {
                validation::attempt_header_integer(name, raw)
            } else {
                validation::attempt_path_integer(name, raw)
            };
            match result {
                Ok(value) => Some(value),
                Err(error) => {
                    errors.extend(error.problems().iter().cloned());
                    None
                }
            }
        })
    };
    let number = parse("number", paths.get("number").map(String::as_str), false);
    let sequence = parse("sequence", paths.get("sequence").map(String::as_str), false);
    drop(parse);
    errors.extend_from_slice(path_errors);
    let generation_header = first_header(headers, "x-lease-generation");
    let generation = generation_header.as_deref().and_then(|raw| {
        match validation::attempt_header_integer("X-Lease-Generation", raw) {
            Ok(value) => Some(value),
            Err(error) => {
                errors.extend(error.problems().iter().cloned());
                None
            }
        }
    });
    // FastAPI aggregates non-body parameter errors before body-model errors.
    errors.extend_from_slice(body_errors);
    if !errors.is_empty() {
        return Err(validation::ValidationErrors::from_problems(errors)
            .domain_error()
            .map_or_else(
                |_| internal(context, "lease validation"),
                |error| failure(ApiError::from(error)),
            ));
    }
    Ok(LeaseParameters {
        number: number.ok_or_else(|| internal(context, "lease number path"))?,
        sequence: sequence.ok_or_else(|| internal(context, "lease sequence path"))?,
        token: first_header(headers, "x-lease-token"),
        generation,
    })
}

/// Called only inside the caller's transaction, after worker authorization.
pub(crate) async fn leased(
    conn: &mut PgConnection,
    principal: &Principal,
    project: &Project,
    parameters: &LeaseParameters,
    profile: JsonContext,
    context: &RequestContext,
) -> Result<Attempt, Failure> {
    let attempt = Repository::new(conn, profile)
        .get_attempt(project.id, &parameters.number, &parameters.sequence, true)
        .await
        .map_err(|_| internal(context, "lease attempt lookup"))?
        .ok_or_else(|| {
            domain(
                ErrorCode::NotFound,
                format!(
                    "attempt #{}.{} not found",
                    parameters.number, parameters.sequence
                ),
            )
        })?;
    if !is_claimant(principal, &attempt) {
        return Err(domain(
            ErrorCode::Forbidden,
            "only the identity that claimed this attempt can work on it",
        ));
    }
    let (Some(token), Some(generation)) = (&parameters.token, &parameters.generation) else {
        return Err(domain(
            ErrorCode::StaleLease,
            "send X-Lease-Token and X-Lease-Generation",
        ));
    };
    if !matches!(attempt.state, AttemptState::Claimed | AttemptState::Running)
        || !attempt
            .lease_token_hash
            .as_ref()
            .is_some_and(|held| cannery_identity::secrets::matches_digest(held, token))
        || generation != &BigInt::from(attempt.lease_generation)
    {
        return Err(domain(
            ErrorCode::StaleLease,
            "this lease is no longer valid for the attempt",
        ));
    }
    let now: Timestamp = sqlx::query_scalar("SELECT now()")
        .fetch_one(&mut *conn)
        .await
        .map_err(|_| internal(context, "lease database clock"))?;
    if attempt
        .lease_expires_at
        .is_none_or(|expires| expires.0 <= now.0)
    {
        return Err(domain(ErrorCode::StaleLease, "the lease expired"));
    }
    if attempt.deadline.is_some_and(|deadline| deadline.0 <= now.0) {
        return Err(domain(
            ErrorCode::StaleLease,
            "the attempt's deadline passed",
        ));
    }
    Ok(attempt)
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/units/{number}/attempts/{sequence}/heartbeat",
    operation_id = "heartbeat_api_projects__slug__units__number__attempts__sequence__heartbeat_post",
    summary = "Heartbeat",
    params(("slug" = String, Path),
        ("number" = i64, Path),
        ("sequence" = i64, Path),
        ("X-Lease-Token" = Option<String>, Header),
        ("X-Lease-Generation" = Option<i64>, Header)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::LeaseOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn heartbeat(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(failure)?;
    let paths = Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &state)
        .await
        .map_err(failure)?
        .0;
    let parameters = parameters(&paths, &parts.headers, &context)?;
    let slug = paths
        .get("slug")
        .ok_or_else(|| internal(&context, "lease slug path"))?;
    let project = worker_access(&mut auth.connection, &auth.principal, slug, &context).await?;
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "lease transaction"))?;
    let result = async {
        let attempt = leased(
            &mut tx,
            &auth.principal,
            &project,
            &parameters,
            state.context.repository,
            &context,
        )
        .await?;
        let mut repository = Repository::new(&mut tx, state.context.repository);
        repository
            .extend_lease(
                attempt.id,
                state.app.settings.leases.ttl_seconds.as_bigint(),
            )
            .await
            .map_err(|_| internal(&context, "lease extension"))?;
        repository
            .get_attempt_by_id(attempt.id, false)
            .await
            .map_err(|_| internal(&context, "renewed lease lookup"))
    }
    .await;
    let renewed = match result {
        Ok(renewed) => {
            tx.commit()
                .await
                .map_err(|_| internal(&context, "lease commit"))?;
            renewed
        }
        Err(error) => {
            if tx.rollback().await.is_err() {
                auth.connection.close_on_drop();
            }
            return Err(error);
        }
    }
    .ok_or_else(|| internal(&context, "renewed lease invariant"))?;
    let expires = renewed
        .lease_expires_at
        .ok_or_else(|| internal(&context, "renewed lease expiry invariant"))?;
    let bytes = serde_json::to_vec(&LeaseOut {
        lease_generation: i64::from(renewed.lease_generation),
        lease_expires_at: crate::timestamps::public_timestamp(expires),
    })
    .map_err(|_| internal(&context, "lease serialization"))?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response())
}
