//! Useful comparison-history filtering, pagination and privacy on actual PostgreSQL.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use cannery_core::settings::load_settings;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
async fn call(app: &Router, query: &str, actor: Option<&str>) -> Result<(u16, Value)> {
    let mut request = Request::builder().uri(format!("/api/projects/matrix/comparisons{query}"));
    if let Some(actor) = actor {
        request = request.header("authorization", format!("Bearer cr_pat_track_http_{actor}"));
    }
    let response = app.clone().oneshot(request.body(Body::empty())?).await?;
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 1 << 20).await?;
    Ok((status, serde_json::from_slice(&bytes)?))
}
#[tokio::test]
#[ignore = "requires a positively selected artifact and owned migrated PostgreSQL child"]
#[allow(
    clippy::too_many_lines,
    reason = "One owned database verifies complete history, pagination, filtering and authorization together"
)]
async fn comparisons_filter_and_page_verified_history() -> Result<()> {
    let url = std::env::var("COMPARISON_DATABASE_URL")?;
    let settings = load_settings(
        None,
        &BTreeMap::from([("CANNERY_DATABASE_URL".into(), url)]),
    )?;
    let (app, state) = cannery_server::application(settings)?;
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    let suffix = database
        .strip_prefix("preparation_owner_")
        .ok_or("owned database required")?;
    if suffix.len() != 24 || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("owned database required".into());
    }
    sqlx::raw_sql(include_str!("fixtures/metrics_http/seed.sql"))
        .execute(&state.pool)
        .await?;
    let (status, base) = call(&app, "", Some("researcher")).await?;
    assert_eq!(status, 200);
    let items = base["items"]
        .as_array()
        .ok_or("comparison items required")?;
    assert_eq!(
        items.len(),
        5,
        "Only completed, live evaluator records publish comparisons"
    );
    assert!(base["next_before"].is_null());
    for item in items {
        assert_eq!(item["metric"], "score");
        assert_eq!(item["source"], "tester");
        assert_eq!(item["verdict"], "pass");
        assert_eq!(item["reference"]["value"], 0.75);
        assert!(
            item["recorded_at"]
                .as_str()
                .is_some_and(|value| value.ends_with('Z'))
        );
    }
    let mut collected = Vec::new();
    let mut query = "?limit=2".to_owned();
    loop {
        let (status, page) = call(&app, &query, Some("viewer")).await?;
        assert_eq!(status, 200);
        collected.extend(
            page["items"]
                .as_array()
                .ok_or("page items required")?
                .iter()
                .cloned(),
        );
        let Some(cursor) = page["next_before"].as_i64() else {
            break;
        };
        query = format!("?limit=2&before={cursor}");
    }
    assert_eq!(&collected, items);
    let identities: BTreeSet<_> = collected
        .iter()
        .filter_map(|row| row["id"].as_i64())
        .collect();
    assert_eq!(identities.len(), collected.len());
    for query in [
        "?split=train",
        "?metric=empty_metric",
        "?dimensions=lang",
        "?filter=lang:en",
        "?verdict=fail",
        "?origin=imported",
        "?since=2025-01-01T00:00:00Z",
        "?until=2024-01-01T00:00:00Z",
    ] {
        let (status, result) = call(&app, query, Some("researcher")).await?;
        assert_eq!(status, 200);
        assert_eq!(result["items"], serde_json::json!([]), "{query}");
    }
    for query in [
        "?overall=true",
        "?origin=all",
        "?metric=score&split=test&verdict=pass",
    ] {
        let (status, result) = call(&app, query, Some("researcher")).await?;
        assert_eq!(status, 200);
        assert_eq!(result, base);
    }
    let (status, alpha) = call(&app, "?track=alpha", Some("researcher")).await?;
    assert_eq!(status, 200);
    assert_eq!(
        alpha["items"]
            .as_array()
            .ok_or("track items required")?
            .len(),
        3
    );
    assert!(
        alpha["items"]
            .as_array()
            .ok_or("track items required")?
            .iter()
            .all(|row| row["track"] == "alpha")
    );
    for query in [
        "?limit=0",
        "?limit=201",
        "?before=-1",
        "?metric=INVALID",
        "?verdict=unsupported",
        "?attempt_state=unsupported",
        "?origin=unsupported",
        "?overall=unsupported",
        "?since=unsupported",
        "?filter=malformed",
    ] {
        assert_eq!(
            call(&app, query, Some("researcher")).await?.0,
            422,
            "{query}"
        );
    }
    assert_eq!(call(&app, "", None).await?.0, 401);
    assert_eq!(call(&app, "", Some("outsider")).await?.0, 404);
    state.pool.close().await;
    Ok(())
}
