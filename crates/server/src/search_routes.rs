//! Search reads and facets preserve the source transaction and permission order.
use crate::{
    AppState, authentication::authenticate, errors::ApiError, requests::RequestContext,
    search_request,
};
use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use cannery_core::{
    errors::{DomainError, ErrorCode},
    principal::Principal,
};
use cannery_search::repo::{self, Hit};
use num_bigint::BigInt;
use serde_json::json;
use sqlx::Acquire;
use std::sync::Arc;

/// Caller-selected source profiles; installing the physical pool remains separate.
pub struct SearchContext {
    pub cursor_decode_budget: usize,
}
#[derive(Clone)]
pub(crate) struct RouteState {
    app: AppState,
    context: Arc<SearchContext>,
}
pub(crate) struct Failure(Box<Response>);
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        *self.0
    }
}
fn internal(context: &RequestContext, operation: &'static str) -> Failure {
    Failure(Box::new(context.internal(operation).into_response()))
}
fn domain(error: DomainError) -> Failure {
    Failure(Box::new(ApiError::from(error).into_response()))
}
/// Install only the independently compared search operation.
pub fn routes(app: AppState, context: Arc<SearchContext>) -> Router {
    Router::new()
        .route("/api/search", get(search).head(head))
        .with_state(RouteState { app, context })
}
async fn head() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "GET")],
        axum::Json(json!({"detail":"Method Not Allowed"})),
    )
}
fn hit(
    row: &Hit,
) -> Result<crate::api_models::SearchHit, cannery_core::json::model::ModelEncodeError> {
    use crate::api_contract::convert;
    let reference = row
        .unit_number
        .map(|number| format!("{}#{number}", row.project));
    let attempt = reference
        .as_ref()
        .zip(row.attempt_sequence)
        .map(|(reference, sequence)| format!("{reference}.{sequence}"));
    let actor = row
        .actor_user
        .map(|user| crate::api_models::Producer {
            kind: "user".to_owned(),
            id: Some(user.to_string()),
        })
        .or_else(|| {
            row.actor_service
                .map(|service| crate::api_models::Producer {
                    kind: "service".to_owned(),
                    id: Some(service.to_string()),
                })
        });
    Ok(crate::api_models::SearchHit {
        kind: row.kind.as_str().to_owned(),
        source_id: row.source_id.0.to_string(),
        project: row.project.clone(),
        r#ref: attempt.as_ref().or(reference.as_ref()).cloned(),
        unit: row.unit_number.map(i64::from),
        attempt_ref: attempt,
        title: row.unit_title.as_ref().unwrap_or(&row.doc_title).clone(),
        snippet: row.snippet.clone(),
        track: row.track.clone(),
        unit_state: row.unit_state.map(|v| v.as_str().to_owned()),
        attempt_state: row.attempt_state.map(|v| v.as_str().to_owned()),
        verdict: row.verdict.clone(),
        decision: row.decision.map(|v| v.as_str().to_owned()),
        actor,
        occurred_at: crate::timestamps::public_timestamp(row.occurred_at),
        origin: convert(row.origin.as_str())?,
        score: row.score,
    })
}
#[allow(
    clippy::too_many_lines,
    reason = "Preserve source authentication, validation, permission, cursor and transaction order"
)]
#[utoipa::path(
    get,
    path = "/api/search",
    operation_id = "search_api_search_get",
    summary = "Search",
    description = "Search the projects you can read, best match first, with facet counts.",
    params(("q" = Option<String>, Query, description = "Words (web-search syntax), or #12 / slug#12.3.", max_length = 500),
        ("project" = Option<Vec<String>>, Query, description = "Project slugs."),
        ("kind" = Option<Vec<crate::api_models::cannery_row__search__routes__Kind>>, Query),
        ("track" = Option<Vec<String>>, Query, description = "Track slugs."),
        ("unit_state" = Option<Vec<crate::api_models::UnitState>>, Query),
        ("attempt_state" = Option<Vec<crate::api_models::AttemptState>>, Query),
        ("verdict" = Option<Vec<crate::api_models::Verdict>>, Query),
        ("decision" = Option<Vec<crate::api_models::ResultDecision>>, Query),
        ("actor" = Option<Vec<String>>, Query, description = "User or service account ids."),
        ("since" = Option<String>, Query, description = "Records from this instant.", format = "date-time"),
        ("until" = Option<String>, Query, description = "Records before this instant.", format = "date-time"),
        ("before" = Option<String>, Query, max_length = 200),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::SearchPage, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn search(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (parts, _) = request.into_parts();
    let auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(|error| Failure(Box::new(error.into_response())))?;
    let mut connection = auth.connection;
    let principal = auth.principal;
    let query =
        crate::request_context::QueryParams::parse(parts.uri.query().unwrap_or("").as_bytes());
    let mut parameters = search_request::parse(&query).map_err(|error| {
        error
            .domain_error()
            .map_or_else(|_| internal(&context, "search validation encoding"), domain)
    })?;
    parameters.criteria.readable = match &principal {
        Principal::Service(service) => Some(vec![service.project_id]),
        Principal::User(user) if user.is_admin => None,
        Principal::User(user) => Some(
            cannery_projects::repo::list_projects_for_user(
                &mut connection,
                user.user_id,
                false,
                None,
                None,
            )
            .await
            .map_err(|error| Failure(Box::new(context.project_error(error).into_response())))?
            .into_iter()
            .map(|project| project.id)
            .collect(),
        ),
    };
    let after = parameters
        .before
        .as_ref()
        .map(|before| {
            search_request::decode_cursor(before, state.context.cursor_decode_budget).ok_or_else(
                || {
                    let mut error = DomainError::new(ErrorCode::ValidationFailed, "invalid cursor");
                    error.details = json!([{"path":"before","message":"not a search cursor"}]);
                    domain(error)
                },
            )
        })
        .transpose()?;
    let mut transaction = connection
        .begin()
        .await
        .map_err(|_| internal(&context, "search begin"))?;
    repo::set_fuzziness(&mut transaction)
        .await
        .map_err(|_| internal(&context, "search fuzziness"))?;
    let rows = repo::search(
        &mut transaction,
        &parameters.criteria,
        after.as_ref().map(|(score, id)| (*score, id)),
        Some(&BigInt::from(parameters.limit + 1)),
    )
    .await
    .map_err(|_| internal(&context, "search query"))?;
    let (total, facets) = repo::facets(&mut transaction, &parameters.criteria)
        .await
        .map_err(|_| internal(&context, "search facets"))?;
    transaction
        .commit()
        .await
        .map_err(|_| internal(&context, "search commit"))?;
    let page = &rows[..parameters.limit.min(rows.len())];
    let next = if rows.len() > parameters.limit {
        page.last()
            .map(|row| search_request::encode_cursor(row.score, row.id.0))
    } else {
        None
    };
    let items = page
        .iter()
        .map(hit)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| internal(&context, "search hit contract"))?;
    let facets = repo::FACETS
        .into_iter()
        .map(|name| {
            (
                name.to_owned(),
                facets.get(name).cloned().unwrap_or_default(),
            )
        })
        .collect();
    let bytes = serde_json::to_vec(&crate::api_models::SearchPage {
        items,
        next_before: next,
        total,
        facets,
    })
    .map_err(|_| internal(&context, "search response"))?;
    Ok(([(header::CONTENT_TYPE, "application/json")], bytes).into_response())
}
