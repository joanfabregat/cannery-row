//! Two explicitly configured report JSON reads; artifact delivery is independent.
use crate::{
    AppState,
    authentication::authenticate,
    report_wire::{self, ResponseContext},
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
use cannery_comments_reports::reports;
use cannery_core::json::Node;
use num_bigint::BigInt;
use std::{collections::BTreeMap, sync::Arc};
pub struct ReportContext {
    pub reports: reports::JsonContext,
    pub attempts: cannery_attempts::model::JsonContext,
    pub hypotheses: cannery_hypotheses::repo::JsonContext,
    pub response: ResponseContext,
}
#[derive(Clone)]
pub(crate) struct RouteState {
    app: AppState,
    context: Arc<ReportContext>,
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
fn internal(context: &RequestContext) -> Failure {
    failure(context.internal("report read"))
}
fn missing(message: String) -> Failure {
    failure(crate::errors::ApiError::from(
        cannery_core::errors::DomainError::new(cannery_core::errors::ErrorCode::NotFound, message),
    ))
}
/// Install the JSON reports list and exact report detail only.
pub fn routes(app: AppState, context: Arc<ReportContext>) -> Router {
    Router::new()
        .route("/api/projects/{slug}/reports", get(list).head(head))
        .route(
            "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/report",
            get(detail).head(head),
        )
        .with_state(RouteState { app, context })
}
async fn head() -> impl IntoResponse {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "GET")])
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/reports",
    operation_id = "list_reports_api_projects__slug__reports_get",
    summary = "List Reports",
    description = "Reports of the project, newest first: run documents, and claimed result\nsheets submitted before them, so never an imported attempt, which has none.",
    params(("slug" = String, Path),
        ("hypothesis" = Option<i64>, Query, minimum = 1, maximum = 2_147_483_647),
        ("track" = Option<String>, Query),
        ("before" = Option<String>, Query, description = "Continue after this report id.", format = "uuid"),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_ReportSummary_UUID_, content_type = "application/json"),
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
    read(state, context, request, false).await
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/report",
    operation_id = "get_report_api_projects__slug__hypotheses__number__attempts__sequence__report_get",
    summary = "Get Report",
    description = "The attempt's full report with the evidence and decisions that followed it.",
    params(("slug" = String, Path),
        ("number" = i64, Path),
        ("sequence" = i64, Path)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::ReportOut, content_type = "application/json"),
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
    read(state, context, request, true).await
}
#[allow(
    clippy::too_many_lines,
    reason = "Retain source query and model construction order"
)]
async fn read(
    state: RouteState,
    context: RequestContext,
    request: Request,
    detail: bool,
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
    let mut errors = Vec::new();
    let mut parse_path = |name: &str| {
        paths
            .get(name)
            .and_then(|value| match validation::report_path_integer(name, value) {
                Ok(v) => Some(v),
                Err(e) => {
                    errors.extend(e.problems().iter().cloned());
                    None
                }
            })
    };
    let number = parse_path("number");
    let sequence = parse_path("sequence");
    let mut filter = None;
    let mut before = None;
    let mut limit = 50;
    if !detail {
        if let Some(raw) = query.get("hypothesis") {
            match validation::bounded_query_integer(raw, "hypothesis", 1, Some(i64::from(i32::MAX)))
            {
                Ok(v) => filter = Some(v),
                Err(e) => errors.extend(e.problems().iter().cloned()),
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
                before = v.before.map(|v| reports::EvidenceId(v.0));
                limit = v.limit;
            }
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
    let profile = &state.context;
    let bytes = if detail {
        let number = number.ok_or_else(|| internal(&context))?;
        let sequence = sequence.ok_or_else(|| internal(&context))?;
        let attempt =
            cannery_attempts::repo::Repository::new(&mut auth.connection, profile.attempts)
                .get_attempt(project.id, &number, &sequence, false)
                .await
                .map_err(|_| internal(&context))?
                .ok_or_else(|| missing(format!("attempt #{number}.{sequence} not found")))?;
        let records = reports::latest_evidence(&mut auth.connection, attempt.id, profile.reports)
            .await
            .map_err(|_| internal(&context))?;
        let sheet = records.get(&reports::Stage::Agent);
        if attempt.origin.as_str() != "imported" {
            let has_report = match sheet {
                None => false,
                Some(sheet) => match sheet.content.node(sheet.content.root()) {
                    Some(Node::Object(_)) => report_wire::is_report(&sheet.content),
                    Some(Node::Array(values)) => values.iter().any(|id| {
                        matches!(
                            sheet.content.node(*id),
                            Some(Node::String(value)) if value.equals_utf8("report")
                        )
                    }),
                    Some(Node::String(value)) => value
                        .as_utf8()
                        .is_some_and(|value| value.contains("report")),
                    _ => return Err(internal(&context)),
                },
            };
            if !has_report {
                return Err(missing(format!(
                    "attempt #{number}.{sequence} has no report yet"
                )));
            }
        }
        let hypothesis = cannery_hypotheses::repo::get_hypothesis_by_id(
            &mut auth.connection,
            attempt.hypothesis_id,
            profile.hypotheses,
        )
        .await
        .map_err(|_| internal(&context))?
        .ok_or_else(|| internal(&context))?;
        let verification = records.get(&reports::Stage::Verification);
        let ids = reports::result_case_ids(&mut auth.connection, attempt.id)
            .await
            .map_err(|_| internal(&context))?;
        let decisions = cannery_hypotheses::repo::list_decisions(
            &mut auth.connection,
            &ids,
            profile.hypotheses,
        )
        .await
        .map_err(|_| internal(&context))?;
        let imported = if sheet.is_none() {
            reports::imported_report(&mut auth.connection, attempt.id)
                .await
                .map_err(|_| internal(&context))?
        } else {
            None
        };
        if let Some(sheet) = sheet {
            report_wire::field(&sheet.content, "report").map_err(|_| internal(&context))?;
            if !report_wire::is_report(&sheet.content) {
                return Err(internal(&context));
            }
        }
        let verification = report_wire::verification(verification, profile.response)
            .map_err(|_| internal(&context))?;
        let assets =
            cannery_attempts::repo::Repository::new(&mut auth.connection, profile.attempts)
                .list_artifacts_by_role(attempt.id, "report_asset")
                .await
                .map_err(|_| internal(&context))?;
        let report = report_wire::Detail {
            attempt: &attempt,
            title: &hypothesis
                .title
                .as_utf8()
                .ok_or_else(|| internal(&context))?,
            sheet,
            imported: imported.as_ref(),
            verification,
            decisions: &decisions,
            assets: &assets,
        };
        report_wire::detail(&report, profile.response)
    } else {
        let rows = reports::list_reports(
            &mut auth.connection,
            project.id,
            filter.as_ref(),
            query.get("track"),
            before,
            Some(&BigInt::from(limit + 1)),
            profile.reports,
        )
        .await
        .map_err(|_| internal(&context))?;
        report_wire::page(&rows, limit, profile.response)
    }
    .map_err(|_| internal(&context))?;
    Ok(([(header::CONTENT_TYPE, "application/json")], bytes).into_response())
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
