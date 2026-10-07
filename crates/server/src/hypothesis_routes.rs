//! Hypothesis HTTP operations with an explicitly injected mutation context.
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
    routing::{get, post},
};
use cannery_core::{
    audit::{self, Attribution, Record},
    contracts::{ContractKind, ContractValidator},
    errors::{DomainError, ErrorCode},
    json::{Document, Node},
    principal::{Principal, Role},
};
use cannery_hypotheses::repo::{
    self, DecisionAction, Hypothesis, HypothesisState, JsonContext, RecordDecision,
};
use cannery_projects::{authz, repo as projects};
use num_bigint::BigInt;
use sqlx::{Acquire, PgConnection};
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
/// Install mutation operations only when their explicit context is supplied.
pub fn routes(app: AppState, context: Arc<HypothesisContext>) -> Router {
    let mutations = context.mutations.is_some();
    let collection = if mutations {
        get(list)
            .post(crate::hypothesis_mutations::create)
            .head(head_post)
            .fallback(method_post)
    } else {
        get(list).head(head_post)
    };
    let item = if mutations {
        get(read)
            .put(crate::hypothesis_mutations::revise)
            .head(head_get)
            .fallback(method_get)
    } else {
        get(read).head(head_get)
    };
    Router::new()
        .route("/api/projects/{slug}/hypotheses", collection)
        .route("/api/projects/{slug}/hypotheses/{number}", item)
        .route(
            "/api/projects/{slug}/hypotheses/{number}/revisions",
            get(revisions).head(head_get),
        )
        .route(
            "/api/projects/{slug}/hypotheses/{number}/revisions/{revision}",
            get(revision).head(head_get),
        )
        .route(
            "/api/projects/{slug}/hypotheses/{number}/draft-review",
            post(review).head(head_post),
        )
        .with_state(RouteState { app, context })
}
async fn head_post() -> impl IntoResponse {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST")])
}
async fn method_post() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [
            (header::ALLOW, "POST"),
            (header::CONTENT_TYPE, "application/json"),
        ],
        "{\"detail\":\"Method Not Allowed\"}",
    )
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
fn contract(d: &Document, s: &HypothesisContext, c: &RequestContext) -> Result<(), Failure> {
    let mut stack = vec![(d.root(), 0)];
    while let Some((id, depth)) = stack.pop() {
        if depth >= s.validation_walk_budget {
            return Err(internal(c, "hypothesis contract traversal"));
        }
        match d.node(id) {
            Some(Node::Array(v)) => stack.extend(v.iter().rev().map(|v| (*v, depth + 1))),
            Some(Node::Object(v)) => stack.extend(v.iter().rev().map(|(_, v)| (*v, depth + 1))),
            Some(_) => {}
            None => return Err(internal(c, "hypothesis contract node")),
        }
    }
    let errors = s
        .contracts
        .document_violations(ContractKind::DraftReview, d)
        .map_err(|_| internal(c, "hypothesis review contract"))?;
    if errors.is_empty() {
        return Ok(());
    }
    let details = errors
        .into_iter()
        .map(|e| {
            e.path
                .as_utf8()
                .map(|path| serde_json::json!({"path":path,"message":e.message}))
                .ok_or_else(|| internal(c, "hypothesis contract path"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Err(Failure::new(ApiError::from(
        DomainError::new(ErrorCode::ValidationFailed, "invalid draft review")
            .with_details(serde_json::json!(details)),
    )))
}
fn archived(state: HypothesisState) -> bool {
    matches!(
        state,
        HypothesisState::Declined
            | HypothesisState::Rejected
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
                    HypothesisState::Draft,
                    HypothesisState::Queued,
                    HypothesisState::Active,
                    HypothesisState::AwaitingHumanReview,
                    HypothesisState::Promoted,
                    HypothesisState::Rejected,
                    HypothesisState::Inconclusive,
                    HypothesisState::Declined,
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
    description = "Draft revisions, oldest first.",
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
#[allow(
    clippy::many_single_char_names,
    clippy::too_many_lines,
    clippy::float_cmp,
    reason = "Review transaction retains source ordering and exact integer-float equality"
)]
#[utoipa::path(
    post,
    path = "/api/projects/{slug}/hypotheses/{number}/draft-review",
    operation_id = "review_draft_api_projects__slug__hypotheses__number__draft_review_post",
    summary = "Review Draft",
    description = "Approve, send back or decline the current revision of a draft.\n\nReplaying an ``Idempotency-Key`` returns the hypothesis as it is now\nwithout deciding again; the same key with another request is a conflict.",
    params(("slug" = String, Path),
        ("number" = i64, Path),
        ("idempotency-key" = Option<String>, Header)),
    request_body(content = crate::api_models::DraftReviewRequest, content_type = "application/json"),
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
pub(crate) async fn review(
    State(s): State<RouteState>,
    axum::Extension(c): axum::Extension<RequestContext>,
    r: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(r).await.map_err(Failure::new)?;
    let mut auth = authenticate(&s.app, &c, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let paths = path(&mut parts, &s).await?;
    let q = query(parts.uri.query().unwrap_or(""));
    let params = crate::validation::hypothesis_parameters(
        paths.get("number").map(String::as_str),
        None,
        &q,
        false,
        false,
    );
    let input = match &body {
        crate::body::DecodedBody::Missing => crate::validation::BodyInput::Missing,
        crate::body::DecodedBody::RawBytes => crate::validation::BodyInput::RawBytes,
        crate::body::DecodedBody::Json(d) => crate::validation::BodyInput::Json(d),
    };
    let document = crate::validation::validate_document_body(input);
    let (params, d) = match (params, document) {
        (Ok(params), Ok(document)) => (params, document),
        (Err(mut parameters), Err(document)) => {
            parameters.append(document);
            return Err(body_error(&parameters, &c));
        }
        (Err(error), Ok(_)) | (Ok(_), Err(error)) => return Err(body_error(&error, &c)),
    };
    let user = auth
        .principal
        .require_user()
        .map_err(|e| Failure::new(ApiError::from(e)))?;
    let slug = paths
        .get("slug")
        .ok_or_else(|| internal(&c, "hypothesis project path"))?;
    let project = authz::project_access(
        &mut auth.connection,
        &auth.principal,
        slug,
        Some(Role::Researcher),
        &[],
        true,
    )
    .await
    .map_err(|e| Failure::new(c.project_error(e)))?
    .project;
    contract(d, &s.context, &c)?;
    let typed_document =
        crate::api_models::request_document::<crate::api_models::DraftReviewRequest>(d)
            .map_err(Failure::new)?;
    let d = &typed_document;
    let key = crate::request_context::first_header(&parts.headers, "idempotency-key");
    if key
        .as_ref()
        .is_some_and(|k| k.chars().count() == 0 || k.chars().count() > 200)
    {
        return Err(domain(
            ErrorCode::ValidationFailed,
            "Idempotency-Key must be 1 to 200 characters",
        ));
    }
    let number = params
        .number
        .ok_or_else(|| internal(&c, "hypothesis review number"))?;
    let hash = crate::hypothesis_idempotency::request_hash(
        slug,
        &number,
        d,
        s.context.request_hash_budget,
    )
    .map_err(|_| internal(&c, "hypothesis request hash"))?;
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&c, "hypothesis review transaction"))?;
    let result = async {
        if let Some(key) = &key
            && let Some((prior, _)) = crate::hypothesis_idempotency::lookup(
                &mut tx, &format!("user:{}", user.user_id), key,
            ).await.map_err(|_| internal(&c, "hypothesis replay lookup"))?
        {
            if prior != hash {
                return Err(domain(ErrorCode::Conflict,
                    "this Idempotency-Key was already used with a different request"));
            }
            let h = hypothesis(&mut tx, project.id, &number, false, &s.context, &c).await?;
            return detail(&mut tx, &auth.principal, &project, h, &s.context, &c).await;
        }
        let h = hypothesis(&mut tx, project.id, &number, true, &s.context, &c).await?;
        if h.state != HypothesisState::Draft {
            return Err(domain(ErrorCode::Conflict,
                format!("#{number} is {}; only drafts are reviewed", h.state.as_str())));
        }
        let revision = d.field(d.root(), "draft_revision").and_then(|id| d.node(id))
            .ok_or_else(|| internal(&c, "hypothesis review revision"))?;
        let equal = match revision {
            Node::Integer(v) => v == &BigInt::from(h.revision),
            Node::Float(v) => *v == f64::from(h.revision),
            _ => false,
        };
        if !equal {
            return Err(domain(ErrorCode::StaleRevision,
                format!("#{number} is at revision {}; review the current revision", h.revision)));
        }
        if repo::revision_requested(&mut tx, h.id, &h.revision.into()).await
            .map_err(|_| internal(&c, "hypothesis revision requested"))?
        {
            return Err(domain(ErrorCode::Conflict, format!(
                "a revision of #{number} r{} was requested; it must be revised before it is reviewed again",
                h.revision)));
        }
        let text = |key: &str| match d.field(d.root(), key).and_then(|id| d.node(id)) {
            Some(Node::String(v)) => Ok(v.clone()),
            _ => Err(internal(&c, "hypothesis review field")),
        };
        let action = text("action")?;
        let action = DecisionAction::try_from(action.as_utf8()
            .ok_or_else(|| internal(&c, "hypothesis action encoding"))?.as_str())
            .map_err(|_| internal(&c, "hypothesis action"))?;
        let reason = text("reason")?;
        let case = repo::open_draft_case(&mut tx, project.id, h.id,
            &h.revision.into(), s.context.repository).await
            .map_err(|_| internal(&c, "hypothesis draft case"))?;
        let decision = repo::record_decision(&mut tx, RecordDecision {
            case_id: case.id, action, subject_revision: &h.revision.into(), reason: &reason,
            principal: user, supersedes: None,
        }, s.context.repository).await.map_err(|_| internal(&c, "hypothesis decision"))?;
        let (state, audit_action) = match action {
            DecisionAction::Approve => (HypothesisState::Queued, "hypothesis.draft_approved"),
            DecisionAction::Decline => (HypothesisState::Declined, "hypothesis.draft_declined"),
            DecisionAction::RequestRevision => (HypothesisState::Draft, "hypothesis.revision_requested"),
            _ => return Err(internal(&c, "hypothesis review action invariant")),
        };
        if action != DecisionAction::RequestRevision {
            repo::set_state(&mut tx, h.id, state,
                (action == DecisionAction::Approve).then_some(&BigInt::from(h.revision)))
                .await.map_err(|_| internal(&c, "hypothesis state"))?;
        }
        let prior = serde_json::json!({"state":h.state.as_str(),"revision":h.revision});
        let new = serde_json::json!({"state":state.as_str(),"revision":h.revision});
        let reason = reason.as_utf8().ok_or_else(|| internal(&c, "hypothesis reason encoding"))?;
        audit::record(&mut tx, Attribution::Principal(&auth.principal), Record {
            action: audit_action, subject_type: "hypothesis", subject_id: &h.id.to_string(),
            project_id: Some(project.id), prior_state: Some(&prior), new_state: Some(&new),
            reason: Some(&reason), idempotency_key: key.as_deref(),
        }).await.map_err(|_| internal(&c, "hypothesis review audit"))?;
        if let Some(key) = &key {
            crate::hypothesis_idempotency::remember(&mut tx, &format!("user:{}", user.user_id),
                key, &hash, &decision.id.0.to_string()).await
                .map_err(|_| internal(&c, "hypothesis remember replay"))?;
        }
        let h = repo::get_hypothesis_by_id(&mut tx, h.id, s.context.repository).await
            .map_err(|_| internal(&c, "hypothesis reviewed"))?
            .ok_or_else(|| internal(&c, "hypothesis reviewed invariant"))?;
        detail(&mut tx, &auth.principal, &project, h, &s.context, &c).await
    }.await;
    match result {
        Ok(d) => {
            tx.commit()
                .await
                .map_err(|_| internal(&c, "hypothesis review commit"))?;
            Ok(response(
                hypothesis_wire::detail(&d, s.context.response)
                    .map_err(|_| internal(&c, "hypothesis response serialization"))?,
            ))
        }
        Err(e) => {
            tx.rollback()
                .await
                .map_err(|_| internal(&c, "hypothesis review rollback"))?;

            Err(e)
        }
    }
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
