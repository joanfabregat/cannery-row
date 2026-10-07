//! Compile-time excluded, loopback-configured observations for the system suite.
use crate::{AppState, authentication::authenticate, errors::ApiError, requests::RequestContext};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, Method},
    routing::{get, post},
};
use cannery_core::{
    errors::{DomainError, ErrorCode},
    principal::Scope,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[path = "conformance_storage.rs"]
mod storage;

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    after: i64,
    #[serde(default = "default_limit")]
    limit: i64,
}
const fn default_limit() -> i64 {
    1000
}

pub(crate) fn routes(state: AppState) -> Router {
    Router::new()
        .route("/__conformance/audit", get(audit))
        .route("/__conformance/sweep", post(sweep))
        .route(
            "/__conformance/storage/projects/{slug}",
            get(storage::project_storage).head(storage::head),
        )
        .with_state(state)
}

async fn sweep(
    State(state): State<AppState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    headers: HeaderMap,
) -> Result<Json<crate::sweeps::SweepReport>, ApiError> {
    let authenticated = authenticate(&state, &context, &headers, &Method::POST).await?;
    authenticated.principal.require_admin(true)?;
    authenticated.principal.require_scope(Scope::Write)?;
    drop(authenticated);
    crate::sweeps::run(&state)
        .await
        .map(Json)
        .map_err(|_| context.internal("conformance recovery run"))
}

async fn audit(
    State(state): State<AppState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    headers: HeaderMap,
    Query(page): Query<Page>,
) -> Result<Json<Value>, ApiError> {
    let mut authenticated = authenticate(&state, &context, &headers, &Method::GET).await?;
    authenticated.principal.require_admin(false)?;
    authenticated.principal.require_scope(Scope::Read)?;
    if page.after < 0 || !(1..=1000).contains(&page.limit) {
        return Err(
            DomainError::new(ErrorCode::ValidationFailed, "invalid audit pagination").into(),
        );
    }
    // Observation SQL is a fixed allowlist, compiled only into conformance builds.
    // Request data can choose only bound pagination, never identifiers or columns.
    let items: Value = sqlx::query_scalar(
        r"SELECT coalesce(jsonb_agg(to_jsonb(e) ORDER BY e.seq), '[]'::jsonb)
        FROM (SELECT seq, occurred_at::text, project_id, actor_kind, actor_user_id,
              actor_service_id, via_channel, via_client, action, subject_type,
              subject_id, prior_state, new_state, reason, idempotency_key
              FROM audit_events WHERE seq > $1 ORDER BY seq LIMIT $2) e",
    )
    .bind(page.after)
    .bind(page.limit)
    .fetch_one(&mut *authenticated.connection)
    .await
    .map_err(|_| context.internal("conformance audit observation"))?;
    let next = items
        .as_array()
        .and_then(|items| items.last())
        .and_then(|row| row["seq"].as_i64())
        .unwrap_or(page.after);
    Ok(Json(json!({"items":items,"next_after":next})))
}
