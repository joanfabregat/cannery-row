//! Metric reads preserve source query, projection and database evaluation order.
use crate::{
    AppState,
    authentication::authenticate,
    errors::ApiError,
    metric_request::{Operation, Parameters},
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
    errors::{DomainError, ErrorCode},
    ids::ProjectId,
    json::{Document, Node},
};
use cannery_metrics::{projection, repo, series};
use cannery_projects::authz;
use cannery_research::{
    config_repo,
    science::{RenderingContext, Science},
};
use num_bigint::BigInt;
use std::{collections::BTreeMap, sync::Arc};
/// Entry-specific limits and science identities are selected explicitly by callers.
pub struct MetricContext {
    pub repository: repo::JsonContext,
    pub config_repository: config_repo::JsonContext,
    pub science: RenderingContext,
    pub rendering_budget: usize,
    pub response: projection::Context,
}
#[derive(Clone)]
pub(crate) struct RouteState {
    app: AppState,
    context: Arc<MetricContext>,
}
pub(crate) struct Failure(Box<Response>);
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        *self.0
    }
}
fn internal(c: &RequestContext, op: &'static str) -> Failure {
    Failure(Box::new(c.internal(op).into_response()))
}
fn domain(code: ErrorCode, message: impl Into<String>) -> Failure {
    Failure(Box::new(
        ApiError::from(DomainError::new(code, message)).into_response(),
    ))
}
fn json(bytes: Vec<u8>) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], bytes).into_response()
}
async fn head() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "GET")],
        axum::Json(serde_json::json!({"detail":"Method Not Allowed"})),
    )
}
/// Install all four metric operations with explicit compatibility profiles.
pub fn routes(app: AppState, context: Arc<MetricContext>) -> Router {
    Router::new()
        .route("/api/projects/{slug}/metrics", get(catalog).head(head))
        .route("/api/projects/{slug}/metrics/query", get(query).head(head))
        .route("/api/projects/{slug}/dashboard", get(dashboard).head(head))
        .route(
            "/api/projects/{slug}/dashboard/views/{view_id}",
            get(view).head(head),
        )
        .with_state(RouteState { app, context })
}
async fn science(
    conn: &mut sqlx::PgConnection,
    project: ProjectId,
    revision: Option<&BigInt>,
    ctx: &MetricContext,
    c: &RequestContext,
) -> Result<config_repo::ConfigRevision, Failure> {
    config_repo::get_revision(
        conn,
        project,
        config_repo::Kind::Science,
        revision,
        ctx.config_repository,
    )
    .await
    .map_err(|_| internal(c, "metric science read"))?
    .ok_or_else(|| {
        domain(
            ErrorCode::NotFound,
            revision.map_or_else(
                || "this project has no science revision yet".into(),
                |r| format!("science revision {r} not found"),
            ),
        )
    })
}
fn constructed<'a>(
    row: &'a config_repo::ConfigRevision,
    ctx: &MetricContext,
    c: &RequestContext,
) -> Result<Science<'a>, Failure> {
    Science::new(row.revision.into(), &row.content, ctx.science)
        .map_err(|_| internal(c, "metric science construction"))
}
struct Dashboard {
    revision: Option<i32>,
    science: config_repo::ConfigRevision,
    views: Document,
}
async fn resolved_dashboard(
    conn: &mut sqlx::PgConnection,
    project: ProjectId,
    revision: Option<&BigInt>,
    ctx: &MetricContext,
    c: &RequestContext,
) -> Result<Dashboard, Failure> {
    let row = config_repo::get_revision(
        conn,
        project,
        config_repo::Kind::Dashboard,
        revision,
        ctx.config_repository,
    )
    .await
    .map_err(|_| internal(c, "metric dashboard read"))?;
    if let Some(row) = row {
        let pin = row
            .science_revision
            .ok_or_else(|| internal(c, "metric dashboard pin"))?;
        let science = science(conn, project, Some(&BigInt::from(pin)), ctx, c).await?;
        constructed(&science, ctx, c)?;
        let views = if let Some(id) = row.content.field(row.content.root(), "views") {
            document_at(&row.content, id).map_err(|_| internal(c, "metric dashboard views"))?
        } else {
            cannery_core::json::decode(b"[]", 1)
                .map_err(|_| internal(c, "metric dashboard empty views"))?
        };
        Ok(Dashboard {
            revision: Some(row.revision),
            science,
            views,
        })
    } else {
        if let Some(revision) = revision {
            return Err(domain(
                ErrorCode::NotFound,
                format!("dashboard revision {revision} not found"),
            ));
        }
        let science = science(conn, project, None, ctx, c).await?;
        let constructed = constructed(&science, ctx, c)?;
        let views =
            cannery_metrics::views::derived_views(constructed.content, ctx.rendering_budget)
                .map_err(|_| internal(c, "metric derived views"))?;
        Ok(Dashboard {
            revision: None,
            science,
            views,
        })
    }
}
async fn evidence(
    conn: &mut sqlx::PgConnection,
    q: &repo::Query,
    ctx: &MetricContext,
    c: &RequestContext,
) -> Result<(repo::Summary, i64), Failure> {
    let summary = repo::summary(conn, q, ctx.repository)
        .await
        .map_err(|_| internal(c, "metric summary"))?;
    let failed = repo::failed_attempts(conn, q)
        .await
        .map_err(|_| internal(c, "metric failed attempts"))?;
    Ok((summary, failed))
}
async fn reading(
    State(s): State<RouteState>,
    c: RequestContext,
    r: Request,
    op: Operation,
) -> Result<Response, Failure> {
    let (mut parts, _) = r.into_parts();
    let mut auth = authenticate(&s.app, &c, &parts.headers, &parts.method)
        .await
        .map_err(|e| Failure(Box::new(e.into_response())))?;
    let Path(paths) = Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &s)
        .await
        .map_err(|e| Failure(Box::new(e.into_response())))?;
    let params =
        crate::request_context::QueryParams::parse(parts.uri.query().unwrap_or("").as_bytes());
    let p = crate::metric_request::parse(&params, op).map_err(|e| {
        e.domain_error().map_or_else(
            |_| internal(&c, "metric validation encoding"),
            |e| Failure(Box::new(ApiError::from(e).into_response())),
        )
    })?;
    let slug = paths
        .get("slug")
        .ok_or_else(|| internal(&c, "metric slug"))?;
    let project = authz::project_read(&mut auth.connection, &auth.principal, slug)
        .await
        .map_err(|e| Failure(Box::new(c.project_error(e).into_response())))?;
    let bytes = match op {
        Operation::Catalog => {
            let row = science(
                &mut auth.connection,
                project.id,
                p.science_revision.as_ref(),
                &s.context,
                &c,
            )
            .await?;
            let science = constructed(&row, &s.context, &c)?;
            crate::metric_wire::catalog(&science, s.context.rendering_budget, s.context.response)
                .map_err(|_| internal(&c, "metric catalog projection"))?
        }
        Operation::Query => query_page(&mut auth.connection, project.id, p, &s.context, &c).await?,
        Operation::Dashboard => {
            let found = resolved_dashboard(
                &mut auth.connection,
                project.id,
                p.dashboard_revision.as_ref(),
                &s.context,
                &c,
            )
            .await?;
            crate::metric_wire::dashboard(
                found.revision,
                &BigInt::from(found.science.revision),
                &found.views,
                found.views.root(),
                s.context.response,
            )
            .map_err(|_| internal(&c, "metric dashboard projection"))?
        }
        Operation::View => {
            resolved_view(
                &mut auth.connection,
                project.id,
                p,
                paths
                    .get("view_id")
                    .ok_or_else(|| internal(&c, "metric view id"))?,
                &s.context,
                &c,
            )
            .await?
        }
    };
    Ok(json(bytes))
}
async fn query_page(
    conn: &mut sqlx::PgConnection,
    project: ProjectId,
    p: Parameters,
    ctx: &MetricContext,
    c: &RequestContext,
) -> Result<Vec<u8>, Failure> {
    let parsed=cannery_metrics::filters::parse_filters(&p.filters).map_err(|e|Failure(Box::new(ApiError::from(DomainError::new(ErrorCode::ValidationFailed,"a filter is dimension:value").with_details(serde_json::json!([{"path":format!("filter/{}",e.index),"message":"expected dimension:value"}]))).into_response())))?;
    let mut dimensions = p.dimensions.unwrap_or_default();
    dimensions.sort();
    dimensions.dedup();
    for (name, _) in &parsed {
        if !dimensions.contains(name) {
            dimensions.push(name.clone());
        }
    }
    let filters = parsed
        .into_iter()
        .map(|(name, values)| {
            values
                .into_iter()
                .map(|v| {
                    v.as_utf8()
                        .ok_or_else(|| internal(c, "metric filter encoding"))
                })
                .collect::<Result<Vec<_>, _>>()
                .map(|values| (name, values))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let q = repo::Query {
        project_id: project,
        metric: p.metric.ok_or_else(|| internal(c, "metric key"))?,
        authority: p.authority,
        split: p.split,
        dimensions: (!p.all_slices).then_some(dimensions),
        filters: Some(filters),
        tracks: p.tracks.filter(|v| !v.is_empty()),
        attempt_states: p.attempt_states.filter(|v| !v.is_empty()),
        science_revision: p.science_revision,
        since: p.since,
        until: p.until,
    };
    let mut rows = repo::points(
        conn,
        &q,
        p.before.as_ref(),
        Some(&BigInt::from(p.limit + 1)),
        ctx.repository,
    )
    .await
    .map_err(|_| internal(c, "metric points"))?;
    let next = if rows.len() > p.limit {
        rows.truncate(p.limit);
        rows.last().map(|r| r.id)
    } else {
        None
    };
    let points = rows
        .iter()
        .map(projection::point_out)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| internal(c, "metric point models"))?;
    let (summary, failed) = evidence(conn, &q, ctx, c).await?;
    let evidence = crate::metric_wire::EvidenceContext::new(&summary, &q, failed)
        .map_err(|_| internal(c, "metric evidence model"))?;
    crate::metric_wire::page(&points, next, &evidence, ctx.response)
        .map_err(|_| internal(c, "metric page bytes"))
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/metrics",
    operation_id = "metric_catalog_api_projects__slug__metrics_get",
    summary = "Metric Catalog",
    description = "The metric registry of the latest (or the given) science revision.",
    params(("slug" = String, Path),
        ("science_revision" = Option<i64>, Query, minimum = 1, maximum = 2_147_483_647)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::CatalogOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn catalog(
    s: State<RouteState>,
    axum::Extension(c): axum::Extension<RequestContext>,
    r: Request,
) -> Result<Response, Failure> {
    reading(s, c, r, Operation::Catalog).await
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/metrics/query",
    operation_id = "query_metrics_api_projects__slug__metrics_query_get",
    summary = "Query Metrics",
    description = "Measurements of one metric, newest first, with the context a chart must show.",
    params(("slug" = String, Path),
        ("metric" = String, Query, pattern = "^[a-z][a-z0-9_]{0,63}$"),
        ("split" = Option<String>, Query),
        ("authority" = Option<String>, Query),
        ("dimensions" = Option<Vec<String>>, Query, description = "The exact slice: rows with exactly these dimensions."),
        ("filter" = Option<Vec<String>>, Query, description = "dimension:value; repeat for several values."),
        ("all_slices" = Option<bool>, Query, description = "Rows of every slice."),
        ("track" = Option<Vec<String>>, Query),
        ("attempt_state" = Option<Vec<crate::api_models::AttemptState>>, Query),
        ("science_revision" = Option<i64>, Query, minimum = 1, maximum = 2_147_483_647),
        ("since" = Option<String>, Query, description = "Recorded from this instant.", format = "date-time"),
        ("until" = Option<String>, Query, description = "Recorded before this instant.", format = "date-time"),
        ("before" = Option<i64>, Query, description = "Continue after this measurement id.", minimum = 1),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::MetricsPage, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn query(
    s: State<RouteState>,
    axum::Extension(c): axum::Extension<RequestContext>,
    r: Request,
) -> Result<Response, Failure> {
    reading(s, c, r, Operation::Query).await
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/dashboard",
    operation_id = "get_dashboard_api_projects__slug__dashboard_get",
    summary = "Get Dashboard",
    description = "The views of the latest (or given) dashboard revision, or derived defaults.",
    params(("slug" = String, Path),
        ("dashboard_revision" = Option<i64>, Query, minimum = 1, maximum = 2_147_483_647)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::DashboardOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn dashboard(
    s: State<RouteState>,
    axum::Extension(c): axum::Extension<RequestContext>,
    r: Request,
) -> Result<Response, Failure> {
    reading(s, c, r, Operation::Dashboard).await
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/dashboard/views/{view_id}",
    operation_id = "resolve_view_api_projects__slug__dashboard_views__view_id__get",
    summary = "Resolve View",
    description = "One view resolved against tester-verified measurements, grouped into series.\n\nLine and scatter charts plot one point per measurement; bar and table\ncharts combine the measurements of one group and x with the metric's\nregistered aggregation. Series are split by science revision. A filter\nor grouping on a dimension the metric does not register (which a\ndashboard revision cannot declare) is ignored and reported in\n``warnings``.",
    params(("slug" = String, Path),
        ("view_id" = String, Path),
        ("dashboard_revision" = Option<i64>, Query, minimum = 1, maximum = 2_147_483_647),
        ("authority" = Option<String>, Query, description = "`imported`: the imported history's measurements instead (docs/import.md).")),
    responses((status = 200, description = "Successful Response", body = crate::api_models::ViewOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn view(
    s: State<RouteState>,
    axum::Extension(c): axum::Extension<RequestContext>,
    r: Request,
) -> Result<Response, Failure> {
    reading(s, c, r, Operation::View).await
}
fn document_at(
    document: &Document,
    id: cannery_core::json::NodeId,
) -> Result<Document, cannery_core::json::BuildError> {
    let mut builder = cannery_core::json::DocumentBuilder::new();
    let root = builder.import(document, id)?;
    builder.finish(root)
}
fn text(
    document: &Document,
    id: cannery_core::json::NodeId,
    budget: usize,
) -> Result<String, projection::Error> {
    cannery_core::text::str_value(document, id, budget).map_err(|_| projection::Error::Value)
}
fn iterable(
    document: &Document,
    id: cannery_core::json::NodeId,
) -> Result<Vec<String>, projection::Error> {
    match document.node(id) {
        Some(Node::Array(ids)) => ids
            .iter()
            .map(|id| match document.node(*id) {
                Some(Node::String(v)) => Ok(v.clone()),
                _ => Err(projection::Error::Type),
            })
            .collect(),
        Some(Node::Object(entries)) => Ok(entries.iter().map(|(key, _)| key.clone()).collect()),
        Some(Node::String(v)) => v
            .codepoints()
            .iter()
            .map(|cp| {
                cannery_core::text::from_codepoints(vec![*cp]).ok_or(projection::Error::Value)
            })
            .collect(),
        _ => Err(projection::Error::Type),
    }
}
fn repr(value: &str, _budget: usize) -> Result<String, projection::Error> {
    cannery_core::text::repr_string(value)
        .map_err(|_| projection::Error::Value)?
        .as_utf8()
        .ok_or(projection::Error::Value)
}
// Python membership first requires hashability, but non-string scalar names
// reach row grouping only when there are rows. Preserve that lazy failure.
fn group_names(
    document: &Document,
    id: cannery_core::json::NodeId,
    budget: usize,
) -> Result<Vec<(Option<String>, String)>, projection::Error> {
    if let Some(Node::Array(ids)) = document.node(id) {
        ids.iter()
            .map(|id| match document.node(*id) {
                Some(Node::String(v)) => Ok((Some(v.clone()), repr(v, budget)?)),
                Some(Node::Null | Node::Bool(_) | Node::Integer(_) | Node::Float(_)) => Ok((
                    None,
                    text(document, *id, budget)?
                        .as_utf8()
                        .ok_or(projection::Error::Value)?,
                )),
                _ => Err(projection::Error::Type),
            })
            .collect()
    } else {
        iterable(document, id)?
            .into_iter()
            .map(|v| {
                let rendered = repr(&v, budget)?;
                Ok((Some(v), rendered))
            })
            .collect()
    }
}
// The source sorts a Python set before JSONB adaptation. Non-string JSON
// array elements cannot satisfy PostgreSQL's `? text` predicate; keep its
// string members while retaining source sorting/type failures.
fn dimension_values(
    document: &Document,
    id: cannery_core::json::NodeId,
) -> Result<Vec<String>, projection::Error> {
    let Some(Node::Array(ids)) = document.node(id) else {
        return iterable(document, id);
    };
    let mut category = None;
    let mut strings = Vec::new();
    for id in ids {
        let kind = match document.node(*id) {
            Some(Node::String(v)) => {
                strings.push(v.clone());
                0
            }
            Some(Node::Bool(_) | Node::Integer(_)) => 1,
            Some(Node::Float(v)) if v.is_finite() => 1,
            Some(Node::Null) => 2,
            _ => return Err(projection::Error::Type),
        };
        if category.is_some_and(|previous| previous != kind) {
            return Err(projection::Error::Type);
        }
        category = Some(kind);
    }
    strings.sort_by_key(cannery_core::text::TextExt::codepoints);
    strings.dedup();
    Ok(strings)
}
#[allow(
    clippy::too_many_lines,
    reason = "Preserve source view resolution, warnings and database evaluation order"
)]
async fn resolved_view(
    conn: &mut sqlx::PgConnection,
    project: ProjectId,
    p: Parameters,
    view_id: &str,
    ctx: &MetricContext,
    c: &RequestContext,
) -> Result<Vec<u8>, Failure> {
    let found = resolved_dashboard(conn, project, p.dashboard_revision.as_ref(), ctx, c).await?;
    let science = constructed(&found.science, ctx, c)?;
    let Some(Node::Array(views)) = found.views.node(found.views.root()) else {
        return Err(internal(c, "metric view sequence"));
    };
    let mut selected = None;
    for id in views {
        if !matches!(found.views.node(*id), Some(Node::Object(_))) {
            return Err(internal(c, "metric view mapping"));
        }
        if found.views.field(*id, "id").is_some_and(
            |id| matches!(found.views.node(id),Some(Node::String(v)) if v.equals_utf8(view_id)),
        ) {
            selected = Some(*id);
            break;
        }
    }
    let selected = selected.ok_or_else(|| {
        repr(&String::from(view_id), ctx.rendering_budget).map_or_else(
            |_| internal(c, "metric view repr"),
            |id| domain(ErrorCode::NotFound, format!("view {id} not found")),
        )
    })?;
    let view =
        document_at(&found.views, selected).map_err(|_| internal(c, "metric selected view"))?;
    let metric = view
        .field(view.root(), "metric")
        .ok_or_else(|| internal(c, "metric view key"))?;
    let metric = text(&view, metric, ctx.rendering_budget)
        .map_err(|_| internal(c, "metric key rendering"))?;
    let metric_key = metric
        .as_utf8()
        .ok_or_else(|| internal(c, "metric key encoding"))?;
    let registered = science
        .metrics
        .iter()
        .find(|(key, _)| key == &metric)
        .map(|(_, metric)| metric);
    let mut registry_id = None;
    if let Some(id) = science.content.field(science.content.root(), "metrics") {
        let Some(Node::Array(items)) = science.content.node(id) else {
            return Err(internal(c, "metric registry sequence"));
        };
        for id in items {
            if !matches!(science.content.node(*id), Some(Node::Object(_))) {
                return Err(internal(c, "metric registry mapping"));
            }
            if science.content.field(*id, "key").is_some_and(
                |id| matches!(science.content.node(id),Some(Node::String(v)) if v==&metric),
            ) {
                registry_id = Some(*id);
                break;
            }
        }
    }
    let registry = registry_id.map_or_else(
        || cannery_core::json::decode(b"{}", 1).map_err(|_| internal(c, "metric empty registry")),
        |id| document_at(science.content, id).map_err(|_| internal(c, "metric registry view")),
    )?;
    let dimensions = registered.map_or_else(Vec::new, |m| m.dimensions.clone());
    let mut warnings = Vec::new();
    if registered.is_none() {
        warnings.push(format!(
            "metric {} is not registered",
            repr(&metric, ctx.rendering_budget).map_err(|_| internal(c, "metric repr"))?
        ));
    }
    let groups = view
        .field(view.root(), "group_by")
        .map(|id| group_names(&view, id, ctx.rendering_budget))
        .transpose()
        .map_err(|_| internal(c, "metric grouping"))?
        .unwrap_or_default();
    let mut groupable = dimensions.clone();
    groupable.push(String::from("track"));
    groupable.extend(
        science
            .unit_facets()
            .map_err(|_| internal(c, "metric facets"))?,
    );
    for (name, rendered) in &groups {
        if !name.as_ref().is_some_and(|name| groupable.contains(name)) {
            warnings.push(format!(
                "group_by dimension {rendered} is not registered; it is null"
            ));
        }
    }
    let raw = view.field(view.root(), "filters");
    let entries = match raw.map(|id| view.node(id)) {
        None => &[][..],
        Some(Some(Node::Object(entries))) => entries.as_slice(),
        _ => return Err(internal(c, "metric filter mapping")),
    };
    for (name, _) in entries {
        if !name.equals_utf8("track") && !dimensions.contains(name) {
            warnings.push(format!(
                "filter on {} ignored: not a registered dimension",
                repr(name, ctx.rendering_budget).map_err(|_| internal(c, "metric filter repr"))?
            ));
        }
    }
    let mut filters = Vec::new();
    for (name, id) in entries {
        if dimensions.contains(name) {
            let values =
                dimension_values(&view, *id).map_err(|_| internal(c, "metric filter values"))?;
            let values = values
                .iter()
                .map(|v| {
                    v.as_utf8()
                        .ok_or_else(|| internal(c, "metric filter encoding"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            filters.push((
                name.as_utf8()
                    .ok_or_else(|| internal(c, "metric filter name"))?,
                values,
            ));
        }
    }
    let tracks = entries
        .iter()
        .find(|(name, _)| name.equals_utf8("track"))
        .map(|(_, id)| match view.node(*id) {
            Some(Node::Array(values)) => values
                .iter()
                .map(|id| match view.node(*id) {
                    Some(Node::String(value)) => value
                        .as_utf8()
                        .ok_or_else(|| internal(c, "metric track encoding")),
                    _ => Err(internal(c, "metric track value")),
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some),
            Some(Node::Null) => Ok(None),
            _ => Err(internal(c, "metric track value")),
        })
        .transpose()?
        .flatten();
    let mut exact = groups
        .iter()
        .filter_map(|(name, _)| name.as_ref())
        .filter(|v| dimensions.contains(v))
        .map(|v| {
            v.as_utf8()
                .ok_or_else(|| internal(c, "metric dimension encoding"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    exact.sort();
    exact.dedup();
    for (name, _) in &filters {
        if !exact.contains(name) {
            exact.push(name.clone());
        }
    }
    let split = view
        .field(view.root(), "split")
        .map(|id| match view.node(id) {
            Some(Node::Null) => Ok(None),
            Some(Node::String(v)) => v
                .as_utf8()
                .map(Some)
                .ok_or_else(|| internal(c, "metric split encoding")),
            _ => Err(internal(c, "metric split value")),
        })
        .transpose()?
        .flatten();
    let q = repo::Query {
        project_id: project,
        metric: metric_key,
        authority: p.authority,
        split,
        dimensions: Some(exact),
        filters: Some(filters),
        tracks,
        attempt_states: None,
        science_revision: None,
        since: None,
        until: None,
    };
    let mut rows = repo::points(conn, &q, None, Some(&BigInt::from(5001)), ctx.repository)
        .await
        .map_err(|_| internal(c, "metric view rows"))?;
    let truncated = rows.len() > 5000;
    rows.truncate(5000);
    let aggregation=if view.field(view.root(),"chart").is_some_and(|id|matches!(view.node(id),Some(Node::String(v)) if v.equals_utf8("bar")||v.equals_utf8("table"))){Some(registry.field(registry.root(),"aggregation").map_or_else(||Ok(String::from("mean")),|id|text(&registry,id,ctx.rendering_budget)).map_err(|_|internal(c,"metric aggregation"))?)}else{None};
    let (series, series_warnings) = series::series(&view, &rows, aggregation.as_ref())
        .map_err(|_| internal(c, "metric series"))?;
    let (summary, failed) = evidence(conn, &q, ctx, c).await?;
    let evidence = crate::metric_wire::EvidenceContext::new(&summary, &q, failed)
        .map_err(|_| internal(c, "metric view evidence"))?;
    warnings.extend(series_warnings);
    crate::metric_wire::view(
        &view,
        found.revision,
        &registry,
        aggregation.as_ref(),
        &series,
        &evidence,
        truncated,
        &warnings,
        ctx.response,
    )
    .map_err(|_| internal(c, "metric view bytes"))
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
