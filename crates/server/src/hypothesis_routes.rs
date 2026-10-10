//! Hypothesis HTTP reads with an explicitly injected mutation context for plan units.
use crate::{
    AppState,
    authentication::authenticate,
    errors::ApiError,
    hypothesis_wire::{self, Detail, ResponseContext},
    requests::RequestContext,
};
use axum::{
    Router,
    extract::{FromRequestParts, Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use cannery_core::{
    contracts::ContractValidator,
    errors::{DomainError, ErrorCode},
    principal::Principal,
};
use cannery_hypotheses::repo::{self, Hypothesis, HypothesisState, JsonContext};
use cannery_projects::{authz, repo as projects};
use num_bigint::BigInt;
use sqlx::PgConnection;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
/// Explicit fixture/frontend profiles; physical-connection history binding is caller-owned.
pub struct HypothesisContext {
    pub mutations: Option<Arc<crate::hypothesis_mutations::MutationContext>>,
    pub contracts: ContractValidator,
    pub validation_walk_budget: usize,
    pub repr_budget: usize,

    pub repository: JsonContext,
    pub response: ResponseContext,
    pub request_hash_budget: usize,
}
#[derive(Clone)]
pub(crate) struct RouteState {
    pub(crate) app: AppState,
    pub(crate) context: Arc<HypothesisContext>,
}
pub(crate) struct Failure(pub(crate) Box<Response>);
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        *self.0
    }
}
impl Failure {
    pub(crate) fn new(v: impl IntoResponse) -> Self {
        Self(Box::new(v.into_response()))
    }
}
pub(crate) fn internal(c: &RequestContext, op: &'static str) -> Failure {
    Failure::new(c.internal(op))
}
pub(crate) fn domain(code: ErrorCode, message: impl Into<String>) -> Failure {
    Failure::new(ApiError::from(DomainError::new(code, message)))
}
pub(crate) fn body_error(e: &crate::validation::ValidationErrors, c: &RequestContext) -> Failure {
    e.domain_error().map_or_else(
        |_| internal(c, "hypothesis validation error"),
        |e| Failure::new(ApiError::from(e)),
    )
}
pub(crate) fn response(bytes: Vec<u8>) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], bytes).into_response()
}
/// Hypotheses are read here; plan approval is the only way one is created.
pub fn routes(app: AppState, context: Arc<HypothesisContext>) -> Router {
    Router::new()
        .route(
            "/api/projects/{slug}/hypotheses",
            get(list).head(head_get).fallback(method_get),
        )
        .route(
            "/api/projects/{slug}/hypotheses/{number}",
            get(read).head(head_get).fallback(method_get),
        )
        .route(
            "/api/projects/{slug}/hypotheses/{number}/revisions",
            get(revisions).head(head_get),
        )
        .route(
            "/api/projects/{slug}/hypotheses/{number}/revisions/{revision}",
            get(revision).head(head_get),
        )
        .with_state(RouteState { app, context })
}
async fn method_get() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [
            (header::ALLOW, "GET"),
            (header::CONTENT_TYPE, "application/json"),
        ],
        "{\"detail\":\"Method Not Allowed\"}",
    )
}
pub(crate) async fn hypothesis(
    c: &mut PgConnection,
    project: cannery_core::ids::ProjectId,
    number: &BigInt,
    lock: bool,
    s: &HypothesisContext,
    r: &RequestContext,
) -> Result<Hypothesis, Failure> {
    repo::get_hypothesis(c, project, number, lock, s.repository)
        .await
        .map_err(|_| internal(r, "hypothesis load"))?
        .ok_or_else(|| {
            domain(
                ErrorCode::NotFound,
                format!("hypothesis #{number} not found"),
            )
        })
}
#[allow(
    clippy::many_single_char_names,
    reason = "Short domain handles retain source detail construction order"
)]
pub(crate) async fn detail(
    c: &mut PgConnection,
    principal: &Principal,
    project: &projects::Project,
    h: Hypothesis,
    s: &HypothesisContext,
    r: &RequestContext,
) -> Result<Detail, Failure> {
    let revision = repo::get_revision(c, h.id, &h.revision.into(), s.repository)
        .await
        .map_err(|_| internal(r, "hypothesis revision"))?
        .ok_or_else(|| internal(r, "hypothesis current revision invariant"))?;
    let readable = match principal {
        Principal::Service(v) => Some(BTreeSet::from([v.project_id])),
        Principal::User(v) if v.is_admin => None,
        Principal::User(v) => Some(
            projects::list_projects_for_user(c, v.user_id, false, None, None)
                .await
                .map_err(|e| Failure::new(r.project_error(e)))?
                .into_iter()
                .map(|p| p.id)
                .collect(),
        ),
    };
    let cases = repo::list_cases(c, h.id, s.repository)
        .await
        .map_err(|_| internal(r, "hypothesis cases"))?;
    let decisions = repo::list_decisions(
        c,
        &cases.iter().map(|c| c.id).collect::<Vec<_>>(),
        s.repository,
    )
    .await
    .map_err(|_| internal(r, "hypothesis decisions"))?;
    // Source constructs _summary before evaluating outgoing/backlinks arguments.
    hypothesis_wire::validate_summary(&h)
        .map_err(|_| internal(r, "hypothesis summary construction"))?;
    let relations = repo::outgoing_relations(c, h.id, s.repository)
        .await
        .map_err(|_| internal(r, "hypothesis relations"))?;
    let backlinks = repo::backlinks(c, h.id, s.repository)
        .await
        .map_err(|_| internal(r, "hypothesis backlinks"))?;
    let d = Detail {
        hypothesis: h,
        revision,
        project: project.slug.clone(),
        relations,
        backlinks,
        cases,
        decisions,
        readable,
    };
    d.validate()
        .map_err(|_| internal(r, "hypothesis detail construction"))?;
    Ok(d)
}
pub(crate) async fn path(
    parts: &mut axum::http::request::Parts,
    state: &RouteState,
) -> Result<BTreeMap<String, String>, Failure> {
    Path::<BTreeMap<String, String>>::from_request_parts(parts, state)
        .await
        .map(|p| p.0)
        .map_err(Failure::new)
}
fn archived(state: HypothesisState) -> bool {
    matches!(
        state,
        HypothesisState::Rejected
            | HypothesisState::Inconclusive
            | HypothesisState::Failed
            | HypothesisState::Cancelled
    )
}
#[allow(
    clippy::many_single_char_names,
    clippy::too_many_lines,
    reason = "Keep signature validation and source authorization order together"
)]
async fn reading(
    State(s): State<RouteState>,
    c: RequestContext,
    r: Request,
    kind: u8,
) -> Result<Response, Failure> {
    let (mut parts, _) = r.into_parts();
    let mut auth = authenticate(&s.app, &c, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let paths = path(&mut parts, &s).await?;
    let q = query(parts.uri.query().unwrap_or(""));
    let params = crate::validation::hypothesis_parameters(
        paths.get("number").map(String::as_str),
        paths.get("revision").map(String::as_str),
        &q,
        kind == 0,
        kind == 2,
    )
    .map_err(|e| body_error(&e, &c))?;
    let slug = paths
        .get("slug")
        .ok_or_else(|| internal(&c, "hypothesis path"))?;
    let project = authz::project_read(&mut auth.connection, &auth.principal, slug)
        .await
        .map_err(|e| Failure::new(c.project_error(e)))?;
    if kind == 0 {
        let states = params.states.or_else(|| {
            params.archived.map(|wanted| {
                [
                    HypothesisState::Queued,
                    HypothesisState::Active,
                    HypothesisState::Documenting,
                    HypothesisState::Deciding,
                    HypothesisState::Promoted,
                    HypothesisState::Rejected,
                    HypothesisState::Inconclusive,
                    HypothesisState::Failed,
                    HypothesisState::Cancelled,
                ]
                .into_iter()
                .filter(|s| archived(*s) == wanted)
                .collect()
            })
        });
        let mut rows = repo::list_hypotheses(
            &mut auth.connection,
            project.id,
            repo::ListHypotheses {
                states: states.as_deref(),
                track_slug: params.track.as_ref(),
                before: params.before.as_ref(),
                limit: &BigInt::from(params.limit + 1),
            },
            s.context.repository,
        )
        .await
        .map_err(|_| internal(&c, "hypothesis list"))?;
        let next = if rows.len() > params.limit {
            rows.truncate(params.limit);
            rows.last().map(|h| h.number)
        } else {
            None
        };
        return Ok(response(
            hypothesis_wire::summaries(&rows, next, s.context.response)
                .map_err(|_| internal(&c, "hypothesis page serialization"))?,
        ));
    }
    let number = params
        .number
        .ok_or_else(|| internal(&c, "hypothesis number"))?;
    let h = hypothesis(
        &mut auth.connection,
        project.id,
        &number,
        false,
        &s.context,
        &c,
    )
    .await?;
    let bytes = match kind {
        1 => {
            let d = detail(
                &mut auth.connection,
                &auth.principal,
                &project,
                h,
                &s.context,
                &c,
            )
            .await?;
            hypothesis_wire::detail(&d, s.context.response)
        }
        2 => {
            let mut rows = repo::list_revisions(
                &mut auth.connection,
                h.id,
                params.before.as_ref(),
                Some(&BigInt::from(params.limit + 1)),
                s.context.repository,
            )
            .await
            .map_err(|_| internal(&c, "hypothesis revisions"))?;
            let next = if rows.len() > params.limit {
                rows.truncate(params.limit);
                rows.last().map(|r| r.revision)
            } else {
                None
            };
            hypothesis_wire::revisions(&rows, next, s.context.response)
        }
        _ => {
            let revision = params
                .revision
                .ok_or_else(|| internal(&c, "hypothesis revision path"))?;
            let row =
                repo::get_revision(&mut auth.connection, h.id, &revision, s.context.repository)
                    .await
                    .map_err(|_| internal(&c, "hypothesis exact revision"))?
                    .ok_or_else(|| {
                        domain(
                            ErrorCode::NotFound,
                            format!("#{number} has no revision {revision}"),
                        )
                    })?;
            hypothesis_wire::revision(&row, s.context.response)
        }
    }
    .map_err(|_| internal(&c, "hypothesis response serialization"))?;
    Ok(response(bytes))
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/hypotheses",
    operation_id = "list_hypotheses_api_projects__slug__hypotheses_get",
    summary = "List Hypotheses",
    params(("slug" = String, Path),
        ("state" = Option<Vec<crate::api_models::HypothesisState>>, Query),
        ("archived" = Option<bool>, Query, description = "true: only archived states; false: hide them. Ignored with `state`."),
        ("track" = Option<String>, Query),
        ("before" = Option<i64>, Query, minimum = 1, maximum = 2_147_483_647),
        ("limit" = Option<i64>, Query, minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::HypothesisPage, content_type = "application/json"),
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
    axum::Extension(c): axum::Extension<RequestContext>,
    r: Request,
) -> Result<Response, Failure> {
    reading(s, c, r, 0).await
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/hypotheses/{number}",
    operation_id = "get_hypothesis_api_projects__slug__hypotheses__number__get",
    summary = "Get Hypothesis",
    params(("slug" = String, Path),
        ("number" = i64, Path)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::HypothesisOut, content_type = "application/json"),
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
    axum::Extension(c): axum::Extension<RequestContext>,
    r: Request,
) -> Result<Response, Failure> {
    reading(s, c, r, 1).await
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/hypotheses/{number}/revisions",
    operation_id = "list_revisions_api_projects__slug__hypotheses__number__revisions_get",
    summary = "List Revisions",
    description = "Revisions, oldest first.",
    params(("slug" = String, Path),
        ("number" = i64, Path),
        ("before" = Option<i64>, Query, description = "Continue after this revision.", minimum = 0, maximum = 2_147_483_647),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_RevisionOut_int_, content_type = "application/json"),
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
    s: State<RouteState>,
    axum::Extension(c): axum::Extension<RequestContext>,
    r: Request,
) -> Result<Response, Failure> {
    reading(s, c, r, 2).await
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/hypotheses/{number}/revisions/{revision}",
    operation_id = "get_revision_api_projects__slug__hypotheses__number__revisions__revision__get",
    summary = "Get Revision",
    params(("slug" = String, Path),
        ("number" = i64, Path),
        ("revision" = i64, Path)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::RevisionOut, content_type = "application/json"),
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
    s: State<RouteState>,
    axum::Extension(c): axum::Extension<RequestContext>,
    r: Request,
) -> Result<Response, Failure> {
    reading(s, c, r, 3).await
}
fn query(raw: &str) -> Vec<(String, String)> {
    crate::request_context::QueryParams::parse(raw.as_bytes())
        .pairs()
        .iter()
        .map(|(k, v)| (String::from(k), String::from(v)))
        .collect()
}
async fn head_get() -> impl IntoResponse {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "GET")])
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
