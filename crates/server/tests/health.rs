use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    routing::get,
};
use cannery_server::health::{HealthState, health};
use serde_json::{Value, json};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::{error::Error, time::Duration};
use tower::ServiceExt;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

async fn response(state: HealthState) -> Result<(StatusCode, Value)> {
    let app = Router::new()
        .route("/api/health", get(health))
        .with_state(state);
    let response = app
        .oneshot(Request::builder().uri("/api/health").body(Body::empty())?)
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024).await?;
    Ok((status, serde_json::from_slice(&bytes)?))
}

fn fixture_options() -> Result<PgConnectOptions> {
    // The launcher supplies the controlled PostgreSQL fixture URL.
    std::env::var("CANNERY_TEST_DATABASE_URL")?
        .parse()
        .map_err(|_| "could not parse health fixture connection".into())
}

#[tokio::test]
async fn unavailable_database_is_degraded_without_version() -> Result<()> {
    let options = PgConnectOptions::new_without_pgpass()
        .host("127.0.0.1")
        .port(1)
        .username("unavailable")
        .database("unavailable");
    let pool = PgPoolOptions::new()
        .min_connections(0)
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(2))
        .connect_lazy_with(options.clone());
    let (status, body) = response(HealthState::new(pool.clone(), options)).await?;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, json!({"status":"degraded","database":"unavailable"}));
    pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires the isolated PostgreSQL fixture"]
async fn reachable_database_is_healthy() -> Result<()> {
    let options = fixture_options()?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone())
        .await?;
    let (status, body) = response(HealthState::new(pool.clone(), options)).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"status":"ok","database":"ok","version":env!("CARGO_PKG_VERSION")})
    );
    pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires the isolated PostgreSQL fixture"]
async fn held_pool_connection_is_saturated() -> Result<()> {
    let options = fixture_options()?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone())
        .await?;
    let held = pool.acquire().await?;
    let (status, body) = response(HealthState::new(pool.clone(), options)).await?;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, json!({"status":"degraded","database":"saturated"}));
    drop(held);
    let (status, _) = response(HealthState::new(pool.clone(), fixture_options()?)).await?;
    assert_eq!(status, StatusCode::OK);
    pool.close().await;
    Ok(())
}
