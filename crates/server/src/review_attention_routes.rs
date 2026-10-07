//! Three read routes, installed only with explicit compatibility contexts.
use crate::{
    AppState,
    authentication::authenticate,
    requests::RequestContext,
    review_attention_wire::{self, AttentionDetail, CaseDetail, ResponseContext},
};
use axum::{
    Router,
    extract::{FromRequestParts, Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use cannery_attempts::{
    model::{EvidenceId, StoredJson},
    repo::Repository,
};
use cannery_core::errors::{DomainError, ErrorCode};
use cannery_hypotheses::repo as hypotheses;
use cannery_reviews::repo;
use sqlx::PgConnection;
use std::{collections::BTreeMap, sync::Arc};
/// Profiles and bounded preparation factories have no inferred production defaults.
pub struct ReviewAttentionContext {
    pub reviews: cannery_reviews::JsonContext,
    pub hypotheses: hypotheses::JsonContext,
    pub attempts: cannery_attempts::model::JsonContext,
    pub response: ResponseContext,
}
#[derive(Clone)]
pub(crate) struct RouteState {
    app: AppState,
    context: Arc<ReviewAttentionContext>,
}
pub(crate) struct Failure(Box<Response>);
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        *self.0
    }
}
impl Failure {
    fn new(v: impl IntoResponse) -> Self {
        Self(Box::new(v.into_response()))
    }
}
fn internal(r: &RequestContext, operation: &'static str) -> Failure {
    Failure::new(r.internal(operation))
}
fn output(v: Vec<u8>) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], v).into_response()
}
/// Install list, detail and attention reads without the dependent review decision route.
pub fn routes(app: AppState, context: Arc<ReviewAttentionContext>) -> Router {
    Router::new()
        .route(
            "/api/projects/{slug}/review-cases",
            get(list).head(head_get),
        )
        .route(
            "/api/projects/{slug}/review-cases/{case_id}",
            get(read).head(head_get),
        )
        .route(
            "/api/projects/{slug}/attention",
            get(attention).head(head_get),
        )
        .with_state(RouteState { app, context })
}
#[allow(
    clippy::many_single_char_names,
    reason = "Keep independent connection/domain context handles in source model order"
)]
pub(crate) async fn case_detail(
    c: &mut PgConnection,
    case: repo::Case,
    s: &ReviewAttentionContext,
    r: &RequestContext,
) -> Result<CaseDetail, Failure> {
    let failure = if let Some(id) = case.failure_id {
        let f = repo::get_failure(c, id, s.reviews)
            .await
            .map_err(|_| internal(r, "review failure"))?
            .ok_or_else(|| internal(r, "review failure invariant"))?;
        // Source constructs FailureOut before querying evidence and decisions.
        CaseDetail::validate_failure(&f).map_err(|_| internal(r, "review failure model"))?;
        Some(f)
    } else {
        None
    };
    let evaluation = if let Some(id) = case.evidence_id {
        let attempt = case
            .attempt_id
            .ok_or_else(|| internal(r, "review attempt invariant"))?;
        let (record, _) = Repository::new(c, s.attempts)
            .get_evidence_by_id(attempt, EvidenceId(id.0))
            .await
            .map_err(|_| internal(r, "review evidence"))?
            .ok_or_else(|| internal(r, "review evidence invariant"))?;
        match record {
            StoredJson::SqlNull => None,
            StoredJson::Value(v) => Some(v),
        }
    } else {
        None
    };
    let decisions = hypotheses::list_decisions(c, &[case.id], s.hypotheses)
        .await
        .map_err(|_| internal(r, "review decisions"))?;
    let d = CaseDetail {
        case,
        failure,
        evaluation,
        decisions,
    };
    d.validate().map_err(|_| internal(r, "review model"))?;
    Ok(d)
}
#[allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "Retain dependency, query and model construction order"
)]
async fn reading(
    State(s): State<RouteState>,
    r: RequestContext,
    request: Request,
    kind: u8,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&s.app, &r, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let paths = Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &s)
        .await
        .map_err(Failure::new)?
        .0;
    let query =
        crate::request_context::QueryParams::parse(parts.uri.query().unwrap_or("").as_bytes());
    let pairs = query
        .pairs()
        .iter()
        .map(|(k, v)| (String::from(k), String::from(v)))
        .collect::<Vec<_>>();
    let params = crate::validation::review_attention_parameters(
        paths.get("case_id").map(String::as_str),
        &pairs,
        kind == 0,
        kind == 2,
    )
    .map_err(|e| {
        e.domain_error().map_or_else(
            |_| internal(&r, "review validation"),
            |e| Failure::new(crate::errors::ApiError::from(e)),
        )
    })?;
    let slug = paths
        .get("slug")
        .ok_or_else(|| internal(&r, "review path"))?;
    let project =
        cannery_projects::authz::project_read(&mut auth.connection, &auth.principal, slug)
            .await
            .map_err(|e| Failure::new(r.project_error(e)))?;
    if kind == 2 {
        let limit = num_bigint::BigInt::from(params.limit);
        let reviews =
            cannery_attention::pending_reviews(&mut auth.connection, project.id, Some(&limit))
                .await
                .map_err(|_| internal(&r, "attention pending reviews"))?;
        let (running_count, running) =
            cannery_attention::running_attempts(&mut auth.connection, project.id, Some(&limit))
                .await
                .map_err(|_| internal(&r, "attention running"))?;
        let outcomes =
            cannery_attention::recent_outcomes(&mut auth.connection, project.id, Some(&limit))
                .await
                .map_err(|_| internal(&r, "attention outcomes"))?;
        let failures =
            cannery_attention::recent_failures(&mut auth.connection, project.id, Some(&limit))
                .await
                .map_err(|_| internal(&r, "attention failures"))?;
        let (stalled_count, stalled) = cannery_attention::stalled_evaluations(
            &mut auth.connection,
            project.id,
            Some(&limit),
            Some(s.app.settings.leases.stalled_evaluation_seconds.as_bigint()),
        )
        .await
        .map_err(|_| internal(&r, "attention stalled"))?;
        let counts = cannery_attention::pending_counts(&mut auth.connection, project.id)
            .await
            .map_err(|_| internal(&r, "attention counts"))?
            .into_iter()
            .map(|(k, v)| (k.as_str().to_owned(), v))
            .collect();
        let d = AttentionDetail {
            counts,
            reviews,
            running_count,
            running,
            outcomes,
            failures,
            stalled_count,
            stalled,
        };
        return review_attention_wire::attention(&d)
            .map(output)
            .map_err(|_| internal(&r, "attention model"));
    }
    if kind == 0 {
        let limit = num_bigint::BigInt::from(params.limit + 1);
        let mut rows = repo::list_cases(
            &mut auth.connection,
            project.id,
            repo::ListCases {
                kind: params.kind.as_ref(),
                state: params.state.as_ref(),
                before: params.before,
                limit: Some(&limit),
            },
        )
        .await
        .map_err(|_| internal(&r, "review list"))?;
        let more = rows.len() > params.limit;
        rows.truncate(params.limit);
        let next = if more {
            rows.last().map(|v| v.id)
        } else {
            None
        };
        let mut items = Vec::with_capacity(rows.len());
        for case in rows {
            items.push(case_detail(&mut auth.connection, case, &s.context, &r).await?);
        }
        review_attention_wire::page(&items, next, s.context.response)
            .map(output)
            .map_err(|_| internal(&r, "review page serialization"))
    } else {
        let case = repo::get_case(
            &mut auth.connection,
            project.id,
            params
                .case_id
                .ok_or_else(|| internal(&r, "review case path"))?,
            false,
        )
        .await
        .map_err(|_| internal(&r, "review detail"))?
        .ok_or_else(|| {
            Failure::new(crate::errors::ApiError::from(DomainError::new(
                ErrorCode::NotFound,
                "review case not found",
            )))
        })?;
        let d = case_detail(&mut auth.connection, case, &s.context, &r).await?;
        review_attention_wire::case(&d, s.context.response)
            .map(output)
            .map_err(|_| internal(&r, "review serialization"))
    }
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/review-cases",
    operation_id = "list_review_cases_api_projects__slug__review_cases_get",
    summary = "List Review Cases",
    description = "Review cases, newest first: ``state=pending`` is the review queue.",
    params(("slug" = String, Path),
        ("kind" = Option<crate::api_models::CaseKind>, Query),
        ("state" = Option<crate::api_models::CaseState>, Query),
        ("before" = Option<String>, Query, format = "uuid"),
        ("limit" = Option<i64>, Query, minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::ReviewCasePage, content_type = "application/json"),
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
    s: State<RouteState>,
    axum::Extension(r): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    reading(s, r, request, 0).await
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/review-cases/{case_id}",
    operation_id = "get_review_case_api_projects__slug__review_cases__case_id__get",
    summary = "Get Review Case",
    params(("slug" = String, Path),
        ("case_id" = String, Path, format = "uuid")),
    responses((status = 200, description = "Successful Response", body = crate::api_models::cannery_row__reviews__routes__ReviewCaseOut, content_type = "application/json"),
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
    s: State<RouteState>,
    axum::Extension(r): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    reading(s, r, request, 1).await
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/attention",
    operation_id = "attention_api_projects__slug__attention_get",
    summary = "Attention",
    description = "Pending reviews, running work, recent outcomes and failures of the project,\nand evaluation jobs no evaluator has claimed for too long.",
    params(("slug" = String, Path),
        ("limit" = Option<i64>, Query, description = "Rows per list.", minimum = 1, maximum = 50)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::AttentionOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn attention(
    s: State<RouteState>,
    axum::Extension(r): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    reading(s, r, request, 2).await
}
async fn head_get() -> impl IntoResponse {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "GET")])
}
