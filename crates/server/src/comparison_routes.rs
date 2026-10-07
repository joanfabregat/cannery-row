//! Authorized, filtered evaluator comparison history with keyset pagination.
use crate::{
    AppState,
    attempt_lease_routes::{Failure, failure, internal},
    authentication::authenticate,
    manifest_routes::violation,
    request_context::QueryParams,
    requests::RequestContext,
};
use axum::{
    Json, Router,
    extract::{FromRequestParts, Path, Request, State},
    routing::get,
};
use cannery_core::ids::ProjectId;
use cannery_projects::authz;
use std::collections::BTreeMap;

pub fn routes(app: AppState) -> Router {
    Router::new()
        .route("/api/projects/{slug}/comparisons", get(list))
        .with_state(app)
}
fn values(query: &QueryParams, name: &str) -> Option<Vec<String>> {
    let values: Vec<_> = query
        .pairs()
        .iter()
        .filter(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
        .collect();
    (!values.is_empty()).then_some(values)
}
fn bound(query: &QueryParams, name: &str) -> Result<Option<String>, Failure> {
    query
        .get(name)
        .map(|raw| {
            crate::datetime_query::parse(raw)
                .map(|value| match value {
                    crate::datetime_query::ParsedDateTime::Aware(value) => value.isoformat(),
                    crate::datetime_query::ParsedDateTime::Naive(value) => {
                        value.format("%Y-%m-%d %H:%M:%S%.f").to_string()
                    }
                })
                .map_err(|_| violation(&format!("query/{name}"), "invalid date-time"))
        })
        .transpose()
}
fn boolean(query: &QueryParams, name: &str) -> Result<bool, Failure> {
    match query
        .get(name)
        .unwrap_or("false")
        .to_ascii_lowercase()
        .as_str()
    {
        "1" | "true" | "t" | "on" | "yes" | "y" => Ok(true),
        "0" | "false" | "f" | "off" | "no" | "n" => Ok(false),
        _ => Err(violation(&format!("query/{name}"), "invalid boolean")),
    }
}
#[allow(
    clippy::too_many_lines,
    reason = "Keep comparison filter validation and the checked query together"
)]
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/comparisons",
    operation_id = "list_comparisons_api_projects__slug__comparisons_get",
    summary = "List Comparisons",
    description = "Evaluator comparisons of the project, ordered by id, newest first; every\nslice unless ``dimensions`` or ``overall`` asks for one.",
    params(("slug" = String, Path),
        ("metric" = Option<String>, Query, pattern = "^[a-z][a-z0-9_]{0,63}$"),
        ("split" = Option<String>, Query),
        ("dimensions" = Option<Vec<String>>, Query, description = "The exact slice: rows with exactly these dimensions."),
        ("filter" = Option<Vec<String>>, Query, description = "dimension:value; repeat for several values."),
        ("overall" = Option<bool>, Query, description = "Only the overall slice (no dimensions)."),
        ("track" = Option<Vec<String>>, Query),
        ("attempt_state" = Option<Vec<crate::api_models::AttemptState>>, Query),
        ("verdict" = Option<Vec<crate::api_models::Verdict>>, Query),
        ("since" = Option<String>, Query, description = "Recorded from this instant.", format = "date-time"),
        ("until" = Option<String>, Query, description = "Recorded before this instant.", format = "date-time"),
        ("origin" = Option<crate::api_models::OriginFilter>, Query, description = "`live` (the default) for live evaluations, `imported` for the verdicts of an imported history (docs/import.md), `all` for both."),
        ("before" = Option<i64>, Query, description = "Continue after this comparison id.", minimum = 1),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_ComparisonOut_int_, content_type = "application/json"),
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
    State(app): State<AppState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Json<crate::api_models::Page_ComparisonOut_int_>, Failure> {
    let mut auth = authenticate(&app, &context, request.headers(), request.method())
        .await
        .map_err(failure)?;
    let (mut parts, _) = request.into_parts();
    let paths = Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &app)
        .await
        .map_err(failure)?
        .0;
    let slug = paths
        .get("slug")
        .ok_or_else(|| internal(&context, "comparison slug"))?;
    let project = authz::project_read(&mut auth.connection, &auth.principal, slug)
        .await
        .map_err(|error| failure(context.project_error(error)))?;
    let query = QueryParams::parse(parts.uri.query().unwrap_or_default().as_bytes());
    let metric = query.get("metric");
    if metric.is_some_and(|value| {
        let bytes = value.as_bytes();
        !(1..=64).contains(&bytes.len())
            || !bytes[0].is_ascii_lowercase()
            || !bytes[1..]
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
    }) {
        return Err(violation("query/metric", "invalid metric name"));
    }
    let split = query.get("split");
    let dimensions = values(&query, "dimensions");
    let overall = boolean(&query, "overall")?;
    let any_slice = dimensions.is_none() && !overall;
    let mut keys = dimensions.unwrap_or_default();
    keys.sort();
    keys.dedup();
    let raw_filters: Vec<_> = values(&query, "filter")
        .unwrap_or_default()
        .iter()
        .map(String::from)
        .collect();
    let parsed = cannery_metrics::filters::parse_filters(&raw_filters)
        .map_err(|_| violation("query/filter", "invalid dimension filter"))?;
    let mut filters = BTreeMap::new();
    for (name, values) in parsed {
        let values = values
            .iter()
            .map(|value| Some(value.clone()))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| violation("query/filter", "invalid filter text"))?;
        filters.insert(name, values);
    }
    let filter_text = if filters.is_empty() {
        None
    } else {
        Some(
            serde_json::to_string(&filters)
                .map_err(|_| internal(&context, "comparison filters"))?,
        )
    };
    let tracks = values(&query, "track");
    let states = values(&query, "attempt_state");
    if states.as_ref().is_some_and(|states| {
        states.iter().any(|value| {
            ![
                "claimed",
                "running",
                "submitted",
                "validating",
                "testing",
                "evaluating",
                "awaiting_human_review",
                "promoted",
                "rejected",
                "inconclusive",
                "failed",
                "cancelled",
                "unreviewed",
            ]
            .contains(&value.as_str())
        })
    }) {
        return Err(violation("query/attempt_state", "invalid attempt state"));
    }
    let verdicts = values(&query, "verdict");
    if verdicts.as_ref().is_some_and(|values| {
        values
            .iter()
            .any(|value| !["pass", "fail", "inconclusive"].contains(&value.as_str()))
    }) {
        return Err(violation("query/verdict", "invalid verdict"));
    }
    let since = bound(&query, "since")?;
    let until = bound(&query, "until")?;
    let origin = match query.get("origin").unwrap_or("live") {
        "all" => None,
        value @ ("live" | "imported") => Some(value),
        _ => return Err(violation("query/origin", "invalid origin")),
    };
    let before = query
        .get("before")
        .map(|value| {
            value
                .parse::<i64>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| violation("query/before", "positive cursor required"))
        })
        .transpose()?;
    let limit = query
        .get("limit")
        .unwrap_or("50")
        .parse::<i64>()
        .ok()
        .filter(|value| (1..=200).contains(value))
        .ok_or_else(|| violation("query/limit", "limit must be between 1 and 200"))?;
    let take = limit + 1;
    let stored = sqlx::query_file_scalar!(
        "src/sql/comparisons.sql",
        project.id as ProjectId,
        metric,
        split,
        any_slice,
        &keys,
        filter_text.as_deref(),
        tracks.as_deref(),
        states.as_deref(),
        verdicts.as_deref(),
        since.as_deref(),
        until.as_deref(),
        origin,
        before,
        take
    )
    .fetch_all(&mut *auth.connection)
    .await
    .map_err(|_| internal(&context, "comparison history query"))?;
    let mut rows: Vec<crate::api_models::ComparisonOut> = stored
        .iter()
        .map(|text| {
            serde_json::from_str(text).map_err(|_| internal(&context, "comparison row decoding"))
        })
        .collect::<Result<_, _>>()?;
    for row in &mut rows {
        let timestamp = row
            .recorded_at
            .as_str()
            .parse::<cannery_core::timestamps::Timestamp>()
            .map_err(|_| internal(&context, "comparison timestamp"))?;
        row.recorded_at = crate::timestamps::public_timestamp(timestamp);
    }
    let limit = usize::try_from(limit).map_err(|_| internal(&context, "comparison limit"))?;
    let next = if rows.len() > limit {
        rows.get(limit - 1).map(|row| row.id)
    } else {
        None
    };
    rows.truncate(limit);
    Ok(Json(crate::api_models::Page_ComparisonOut_int_ {
        items: rows,
        next_before: next,
    }))
}
