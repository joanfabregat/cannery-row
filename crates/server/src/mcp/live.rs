//! Guarded real PostgreSQL/HTTP proof, selected explicitly before child creation.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "Assertions confined to the gated live fixture"
)]
use super::*;
use crate::{
    authentication::authenticate, comment_mutations::CommentMutationContext,
    comment_routes::CommentContext,
};
use axum::http::Request as HttpRequest;
use cannery_core::settings::load_settings;
use chrono::{DateTime, Datelike, Utc};
use serde_json::Value;
use std::{collections::BTreeMap, error::Error, time::Duration};
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
struct MetadataOnlyStore;
impl crate::artifact_download::DownloadStore for MetadataOnlyStore {
    fn backend(&self) -> &'static str {
        "local"
    }
    fn bucket(&self) -> &'static str {
        "local"
    }
    fn head<'a>(
        &'a self,
        _: &'a str,
    ) -> crate::artifact_download::StoreFuture<'a, Option<cannery_storage::ObjectHead>> {
        panic!("MCP metadata must not HEAD store bytes")
    }
    fn read<'a>(
        &'a self,
        _: &'a str,
    ) -> crate::artifact_download::StoreFuture<'a, cannery_storage::ObjectReader> {
        panic!("MCP metadata must not read store bytes")
    }
    fn presigning(&self) -> Option<&cannery_storage::s3::S3Store> {
        None
    }
}
static FIXTURE: std::sync::LazyLock<&str> =
    std::sync::LazyLock::new(|| runtime_reference!("/tests/fixtures/mcp_reference.json"));
fn comments() -> Arc<CommentContext> {
    Arc::new(CommentContext {
        units: cannery_units::repo::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
        mutations: Some(Arc::new(CommentMutationContext {
            mention_walk_budget: 80,
        })),
    })
}
fn projection(value: Value, registry: bool) -> Result<Value> {
    let Some(object) = value.as_object() else {
        return Ok(value);
    };
    if let Some(error) = object.get("error") {
        return Ok(json!({"error":{"code":error["code"]}}));
    }
    if registry {
        return Ok(
            json!({"jsonrpc":value["jsonrpc"],"id":value["id"],"tools":value["result"]["tools"].as_array().ok_or("tools")?.iter().map(|t|&t["name"]).collect::<Vec<_>>()}),
        );
    }
    if object.contains_key("content") {
        assert_eq!(
            serde_json::from_str::<Value>(value["content"][0]["text"].as_str().ok_or("text")?)?,
            value["structuredContent"]
        );
        let mut result = json!({"isError":value["isError"],"structuredContent":projection(value["structuredContent"].clone(),false)?});
        if let Some(meta) = value.get("_meta") {
            result["_meta"] = meta.clone();
        }
        return Ok(result);
    }
    let mut result = Map::new();
    for (key, value) in object {
        let mut value = value.clone();
        if matches!(
            key.as_str(),
            "created_at" | "edited_at" | "updated_at" | "occurred_at"
        ) && let Some(text) = value.as_str()
            && let Ok(instant) = DateTime::parse_from_rfc3339(text)
            && instant.year() >= 2026
        {
            assert!(instant <= Utc::now());
            value = json!("@actual-clock");
        }
        result.insert(
            key.clone(),
            if let Some(items) = value.as_array() {
                Value::Array(
                    items
                        .iter()
                        .cloned()
                        .map(|v| projection(v, false))
                        .collect::<Result<_>>()?,
                )
            } else {
                projection(value, false)?
            },
        );
    }
    Ok(Value::Object(result))
}
async fn request(
    app: &Router,
    recipe: &Value,
) -> Result<(StatusCode, Option<String>, Option<String>, Value)> {
    let mut builder = HttpRequest::builder()
        .method(recipe["method"].as_str().ok_or("method")?)
        .uri("/mcp")
        .header("content-type", "application/json")
        .header(
            "user-agent",
            recipe["headers"]["user-agent"]
                .as_str()
                .unwrap_or("mcp-fixture"),
        );
    if let Some(authorization) = recipe["authorization"].as_str() {
        builder = builder.header("authorization", authorization);
    }
    for (name, value) in recipe["headers"].as_object().ok_or("headers")? {
        if name == "user-agent" {
            continue;
        }
        builder = builder.header(name, value.as_str().ok_or("header")?);
    }
    let body = recipe["raw"]
        .as_str()
        .map(str::as_bytes)
        .map(<[u8]>::to_vec)
        .unwrap_or(serde_json::to_vec(&recipe["message"])?);
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        app.clone().oneshot(builder.body(Body::from(body))?),
    )
    .await??;
    let status = response.status();
    let allow = response
        .headers()
        .get("allow")
        .map(|v| v.to_str().map(str::to_owned))
        .transpose()?;
    let challenge = response
        .headers()
        .get("www-authenticate")
        .map(|v| v.to_str().map(str::to_owned))
        .transpose()?;
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024).await?;
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)?
    };
    Ok((status, allow, challenge, value))
}
async fn touches(state: &AppState) -> Result<i32> {
    Ok(
        sqlx::query_scalar("SELECT coalesce(sum(n),0)::int FROM fixture_mcp_touches")
            .fetch_one(&state.pool)
            .await?,
    )
}
async fn reuse(State(state): State<AppState>, request: Request) -> Response {
    let context = request
        .extensions()
        .get::<RequestContext>()
        .expect("context");
    let first = authenticate(&state, context, request.headers(), request.method())
        .await
        .expect("first handoff");
    drop(first);
    match authenticate(&state, context, request.headers(), request.method()).await {
        Ok(_) => panic!("handoff reused"),
        Err(error) => error.into_response(),
    }
}
#[tokio::test]
#[ignore = "requires a positively selected owned migrated PostgreSQL fixture"]
#[allow(
    clippy::too_many_lines,
    reason = "Ordered source observations and ownership recovery are one guarded fixture"
)]
async fn mcp_stateless_http() -> Result<()> {
    let url = std::env::var("CANNERY_MCP_DATABASE_URL")?;
    let mut settings = load_settings(
        None,
        &BTreeMap::from([("CANNERY_DATABASE_URL".into(), url)]),
    )?;
    settings.database.pool_min_size = 1_i64.into();
    settings.database.pool_max_size = 1_i64.into();
    settings.server.public_base_url = "https://cannery.test".into();
    let (factory, state) = crate::application_with_comment_context(settings.clone(), comments())?;
    let download = Arc::new(DownloadContext {
        store: Arc::new(MetadataOnlyStore),
        signing_clock: std::time::SystemTime::now,
        repository: cannery_attempts::model::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
    });
    let domain = Router::new()
        .merge(crate::project_routes::routes(state.clone()))
        .merge(crate::brief_routes::routes(state.clone())?)
        .merge(crate::comment_routes::routes(state.clone(), comments()));
    let mcp = routes(state.clone(), domain.clone(), Some(download))?;
    let app = domain.merge(mcp).layer(middleware::from_fn_with_state(
        crate::requests::RequestState::new("127.0.0.1"),
        crate::requests::contextualize,
    ));
    let version: String = sqlx::query_scalar("SHOW server_version_num")
        .fetch_one(&state.pool)
        .await?;
    assert_eq!(version, "170011");
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    let nonce = name
        .strip_prefix("conformance_")
        .ok_or("owned child required")?;
    if nonce.len() != 24
        || !nonce
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err("owned child required".into());
    }
    for seed in [
        include_str!("../../tests/fixtures/comment_reads/seed.sql"),
        include_str!("../../tests/fixtures/comment_mutations/seed.sql"),
        include_str!("../../tests/fixtures/mcp_seed.sql"),
    ] {
        sqlx::raw_sql(seed).execute(&state.pool).await?;
    }
    let fixture: Value = serde_json::from_str(*FIXTURE)?;
    let cases = fixture["cases"].as_array().ok_or("cases")?;
    assert_eq!(cases.len(), 86);
    let mut unauthenticated = cases[0].clone();
    unauthenticated["authorization"] = Value::Null;
    assert_eq!(
        request(&factory, &unauthenticated).await?.0,
        StatusCode::UNAUTHORIZED
    );
    for recipe in cases {
        let (status, allow, challenge, value) = request(&app, recipe).await?;
        assert_eq!(
            u64::from(status.as_u16()),
            recipe["status"].as_u64().ok_or("status")?,
            "{}",
            recipe["name"]
        );
        assert_eq!(json!(allow), recipe["allow"], "{}", recipe["name"]);
        assert_eq!(json!(challenge), recipe["challenge"], "{}", recipe["name"]);
        let mut projected = projection(value, recipe["name"] == "registry")?;
        // The instructions are the opening of docs/agents.md; the
        // reference names it rather than freezing its text.
        if let Some(instructions) = projected.pointer_mut("/result/instructions") {
            assert_eq!(
                instructions.as_str(),
                Some(super::instructions(&settings.server.public_base_url).as_str())
            );
            *instructions = json!("@protocol");
        }
        assert_eq!(projected, recipe["output"], "{}", recipe["name"]);
        assert_eq!(
            i64::from(touches(&state).await?),
            recipe["touches"].as_i64().ok_or("touches")?,
            "{}",
            recipe["name"]
        );
    }
    let audit: Vec<(String, Option<String>, String)> =
        sqlx::query_as("SELECT via_channel,via_client,action FROM audit_events ORDER BY seq")
            .fetch_all(&state.pool)
            .await?;
    assert_eq!(json!(audit), fixture["audit"]);
    let revisions:Vec<(String,Option<String>,String)>=sqlx::query_as("SELECT via_channel,via_client,body_markdown FROM comment_revisions WHERE comment_id::text LIKE '60000000-%' ORDER BY comment_id,revision").fetch_all(&state.pool).await?;
    assert_eq!(json!(revisions), fixture["revisions"]);
    // Even if a domain handler calls authentication twice, the second consumption
    // fails promptly instead of acquiring/authenticating another connection.
    let domain = Router::new()
        .route("/api/projects", axum::routing::get(reuse))
        .with_state(state.clone());
    let probe = routes(state.clone(), domain, None)?;
    let before = touches(&state).await?;
    let mut original = cases
        .iter()
        .find(|r| r["name"] == "list_projects")
        .ok_or("project recipe")?
        .clone();
    // Supply the normal outer contextualizer, retaining the private handoff only
    // inside the MCP dispatcher (no externally constructible trust path).
    let layered = probe.layer(middleware::from_fn_with_state(
        crate::requests::RequestState::new("127.0.0.1"),
        crate::requests::contextualize,
    ));
    let (_, _, _, value) = request(&layered, &original).await?;
    assert_eq!(value["error"]["code"], -32603);
    assert_eq!(touches(&state).await?, before + 1);
    // A manifest of the right shape: the tool's input schema embeds the
    // artifact manifest contract, so an empty one is refused before dispatch.
    let manifest = json!({"schema_version":"0.2","attempt_id":"00000000-0000-0000-0000-000000000001","objects":[{"role":"data","storage":{"backend":"local","bucket":"local","key":"data"},"size_bytes":1,"sha256":"0".repeat(64),"media_type":"application/json"}]});
    original["message"] = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"post_manifest","arguments":{"project":"matrix","number":1,"sequence":1,"lease_token":"opaque","lease_generation":1,"document":manifest}}});
    let (_, _, _, value) = request(&app, &original).await?;
    assert_eq!(
        value["result"]["structuredContent"]["error"]["code"],
        "tool_unavailable"
    );
    assert_eq!(state.pool.size(), 1);
    let connection = tokio::time::timeout(Duration::from_secs(1), state.pool.acquire()).await??;
    drop(connection);
    // Limits are enforced at the MCP boundary, including before authentication.
    settings.mcp.max_request_bytes = 16_i64.into();
    let (small, small_state) =
        crate::application_with_comment_context(settings.clone(), comments())?;
    let before = touches(&state).await?;
    let (status, _, _, value) = request(&small, &original).await?;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(value["error"]["code"], -32600);
    assert_eq!(touches(&state).await?, before);
    small_state.pool.close().await;
    settings.mcp.max_request_bytes = 1024_i64.into();
    settings.mcp.max_result_bytes = 8_i64.into();
    let (small, small_state) = crate::application_with_comment_context(settings, comments())?;
    original["message"] = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_projects","arguments":{}}});
    let (_, _, _, value) = request(&small, &original).await?;
    assert_eq!(value["result"]["isError"], true);
    assert_eq!(
        value["result"]["structuredContent"]["error"]["code"],
        "result_too_large"
    );
    small_state.pool.close().await;
    state.pool.close().await;
    println!(
        "MCP89 unchanged-source observations, max1 ownership/reuse, metadata/no-store-IO, limits and persisted attribution passed"
    );
    Ok(())
}
