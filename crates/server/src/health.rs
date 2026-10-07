//! Database health distinguishes unavailable service from a busy pool.

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Serialize;
use sqlx::{Connection, PgConnection, PgPool, postgres::PgConnectOptions};
use std::time::Duration;

#[derive(Clone)]
pub struct HealthState {
    pool: PgPool,
    options: PgConnectOptions,
}

impl HealthState {
    #[must_use]
    pub fn new(pool: PgPool, options: PgConnectOptions) -> Self {
        Self { pool, options }
    }

    async fn reachable(&self) -> bool {
        let connection = tokio::time::timeout(
            Duration::from_secs(2),
            PgConnection::connect_with(&self.options),
        )
        .await;
        let Ok(Ok(mut connection)) = connection else {
            return false;
        };
        let reachable = select_one(&mut connection).await.is_ok();
        let _ = connection.close().await;
        reachable
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct HealthResponse {
    status: &'static str,
    database: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<&'static str>,
}

async fn select_one(connection: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query!("SELECT 1 AS \"healthy!\"")
        .fetch_one(connection)
        .await?;
    Ok(())
}

#[utoipa::path(
    get,
    path = "/api/health",
    operation_id = "health_api_health_get",
    summary = "Health",
    responses((status = 200, description = "Healthy database", body = HealthResponse, content_type = "application/json"),
        (status = 503, description = "Database unavailable or saturated", body = HealthResponse, content_type = "application/json"))
)]
pub async fn health(State(state): State<HealthState>) -> impl IntoResponse {
    let outcome = match tokio::time::timeout(Duration::from_secs(2), state.pool.acquire()).await {
        Ok(Ok(mut connection)) => select_one(&mut connection).await,
        Ok(Err(error)) => Err(error),
        Err(_) => Err(sqlx::Error::PoolTimedOut),
    };
    let database = match outcome {
        Ok(()) => "ok",
        Err(sqlx::Error::PoolTimedOut) if state.reachable().await => "saturated",
        Err(_) => "unavailable",
    };
    let healthy = database == "ok";
    (
        if healthy {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(HealthResponse {
            status: if healthy { "ok" } else { "degraded" },
            database,
            version: healthy.then_some(env!("CARGO_PKG_VERSION")),
        }),
    )
}
