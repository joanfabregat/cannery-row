//! Independently replay production lease observations with actual database clocks.
#![forbid(unsafe_code)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use cannery_core::{settings::load_settings, timestamps::Timestamp};
use cannery_server::{
    application_with_attempt_lease_context, attempt_lease_routes::AttemptLeaseContext,
};
use chrono::TimeDelta;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::fmt::Write;
use std::{collections::BTreeMap, error::Error, sync::Arc};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
const ATTEMPT: &str = "00000000-0000-0000-0000-000000002001";
const TTL: i64 = 90;
fn fixture() -> Result<Value> {
    Ok(serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/attempt_leases_http/reference.json"
    ))?)
}
fn hex(bytes: &[u8]) -> Result<String> {
    let mut out = String::new();
    for byte in bytes {
        write!(&mut out, "{byte:02x}")?;
    }
    Ok(out)
}
fn output(value: Value) -> Value {
    if let Some(error) = value
        .get("error")
        .filter(|error| error["code"] == "validation_failed")
    {
        return json!({"error":{"code":error["code"],"details":error["details"].as_array().map(|values| values.iter().map(|value|json!({"path":value["path"]})).collect::<Vec<_>>())}});
    }
    value
}
async fn storage(pool: &PgPool, project: bool) -> Result<Value> {
    let mut out = json!({});
    for (table, order) in [
        ("tracks", "id"),
        ("hypotheses", "id"),
        ("hypothesis_revisions", "hypothesis_id,revision"),
        ("attempts", "id"),
        ("artifacts", "id"),
        ("attempt_failures", "id"),
        ("evidence_records", "id"),
        ("manifests", "id"),
        ("uploads", "id"),
        ("jobs", "id"),
        ("review_cases", "id"),
        ("decisions", "id"),
        ("audit_events", "seq"),
        ("idempotency_keys", "scope,actor,key"),
        ("search_documents", "id"),
    ] {
        let expression = if project && table == "attempts" {
            format!(
                "CASE WHEN id='{ATTEMPT}' THEN jsonb_set(jsonb_set(to_jsonb(t),'{{lease_expires_at}}',CASE WHEN lease_expires_at BETWEEN '2002-01-01Z' AND '2090-01-01Z' THEN '\"@checked-clock\"'::jsonb ELSE to_jsonb(t)->'lease_expires_at' END),'{{started_at}}',CASE WHEN started_at BETWEEN '2002-01-01Z' AND '2090-01-01Z' THEN '\"@checked-clock\"'::jsonb ELSE to_jsonb(t)->'started_at' END) ELSE to_jsonb(t) END"
            )
        } else {
            "to_jsonb(t)".to_owned()
        };
        out[table] = json!(
            sqlx::query_scalar::<_, String>(&format!(
                "SELECT ({expression})::text FROM {table} t ORDER BY {order}"
            ))
            .fetch_all(pool)
            .await?
        );
    }
    Ok(out)
}
async fn clock(pool: &PgPool) -> Result<Timestamp> {
    Ok(sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(pool)
        .await?)
}
async fn call(app: &Router, recipe: &Value) -> Result<(u16, Option<String>, Value, Vec<u8>)> {
    let mut request = Request::builder()
        .method(recipe["method"].as_str().ok_or("method")?)
        .uri(recipe["path"].as_str().ok_or("path")?);
    let role = recipe["role"].as_str().ok_or("role")?;
    if role != "none" {
        let prefix = if [
            "agent",
            "foreign-agent",
            "experimenter",
            "tester",
            "evaluator",
        ]
        .contains(&role)
        {
            "cr_svc_"
        } else {
            "cr_pat_"
        };
        request = request.header("authorization", format!("Bearer {prefix}track_http_{role}"));
    }
    for header in recipe["headers"].as_array().ok_or("headers")? {
        request = request.header(
            header[0].as_str().ok_or("header name")?,
            header[1].as_str().ok_or("header value")?,
        );
    }
    let response = app.clone().oneshot(request.body(Body::empty())?).await?;
    let status = response.status().as_u16();
    let allow = response
        .headers()
        .get("allow")
        .map(|value| value.to_str().map(str::to_owned))
        .transpose()?;
    let bytes = to_bytes(response.into_body(), usize::MAX).await?.to_vec();
    let value = if status == 500 {
        assert_eq!(bytes, b"Internal Server Error");
        Value::Null
    } else if bytes.is_empty() {
        Value::Null
    } else {
        output(serde_json::from_slice(&bytes)?)
    };
    Ok((status, allow, value, bytes))
}
#[tokio::test]
#[ignore = "requires positive exact selection and a guarded fresh migrated child"]
#[allow(
    clippy::too_many_lines,
    reason = "Keep full lease response, raw storage and clock checks together"
)]
async fn attempt_leases_match_production() -> Result<()> {
    let url = std::env::var("CANNERY_ATTEMPT_LEASES_HTTP_DATABASE_URL")?;
    let settings = load_settings(
        None,
        &BTreeMap::from([
            ("CANNERY_DATABASE_URL".into(), url),
            ("CANNERY_LEASES_TTL_SECONDS".into(), TTL.to_string()),
        ]),
    )?;
    assert_eq!(
        settings.leases.ttl_seconds.as_bigint().to_string(),
        TTL.to_string()
    );
    let profile = AttemptLeaseContext {
        release: None,
        repository: cannery_attempts::model::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
    };
    let (app, state) = application_with_attempt_lease_context(settings, Arc::new(profile))?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("owned nonce child required")?;
    if suffix.len() != 24 || !suffix.bytes().all(|value| value.is_ascii_hexdigit()) {
        return Err("owned nonce child required".into());
    }
    for seed in [
        include_str!("fixtures/attempt_reads_http/seed.sql"),
        include_str!("fixtures/attempt_leases_http/seed.sql"),
    ] {
        sqlx::raw_sql(seed).execute(&state.pool).await?;
    }
    let f = fixture()?;
    for recipe in f["cases"].as_array().ok_or("cases")? {
        let setup = recipe["setup"].as_str().ok_or("setup")?;
        if !setup.is_empty() {
            sqlx::raw_sql(setup).execute(&state.pool).await?;
        }
        let initial = storage(&state.pool, false).await?;
        let before = clock(&state.pool).await?;
        let (status, allow, mut response, bytes) = call(&app, recipe).await?;
        let after = clock(&state.pool).await?;
        if let Some(path) = recipe["native_integer_path"].as_str() {
            assert!(matches!(
                recipe["name"].as_str(),
                Some("generation" | "lease-headers" | "number" | "sequence")
            ));
            let raw = if path == "header/X-Lease-Generation" {
                recipe["headers"]
                    .as_array()
                    .ok_or("headers")?
                    .iter()
                    .find(|v| v[0] == "X-Lease-Generation")
                    .and_then(|v| v[1].as_str())
                    .ok_or("generation")?
            } else {
                assert!(matches!(path, "path/number" | "path/sequence"));
                let index = if path == "path/number" { 5 } else { 7 };
                recipe["path"]
                    .as_str()
                    .ok_or("path")?
                    .split('/')
                    .nth(index)
                    .ok_or("integer segment")?
            };
            assert!(
                raw.parse::<i64>().is_err(),
                "declared native unsupported integer"
            );
            assert_eq!(status, 422);
            assert_eq!(allow, None);
            assert_eq!(
                serde_json::from_slice::<Value>(&bytes)?,
                json!({"error":{
                "code":"validation_failed", "message":"request validation failed",
                "details":[{"path":path,"message":"Input should be a signed 64-bit integer"}]}})
            );
            assert_eq!(
                storage(&state.pool, false).await?,
                initial,
                "all raw rows unchanged"
            );
            assert_eq!(
                initial, recipe["native_initial_storage"],
                "independent pre-request storage"
            );
            continue;
        }
        assert!(recipe["native_integer_path"].is_null());
        assert_eq!(
            json!(status),
            recipe["status"],
            "status {}; {response}",
            recipe["name"]
        );
        assert_eq!(json!(allow), recipe["allow"], "allow {}", recipe["name"]);
        if status == 200 {
            let (expires,started,indexed): (Timestamp,Timestamp,Timestamp) = sqlx::query_as(
                "SELECT a.lease_expires_at,a.started_at,s.updated_at FROM attempts a JOIN search_documents s ON s.kind='attempt' AND s.source_id=a.id WHERE a.id=$1::text::uuid"
            ).bind(ATTEMPT).fetch_one(&state.pool).await?;
            assert!(
                before.0 + TimeDelta::seconds(TTL) <= expires.0
                    && expires.0 <= after.0 + TimeDelta::seconds(TTL)
            );
            assert_eq!(indexed.to_string(), "2001-01-01T00:00:00+00:00");
            let renewed_at = expires.0 - TimeDelta::seconds(TTL);
            assert!(started.0 <= renewed_at);
            if setup.contains("state='claimed'") && !setup.contains("started_at='2001-01-01Z'") {
                assert_eq!(started.0, renewed_at);
            }
            let expected_time = cannery_server::timestamps::public_timestamp(expires);
            assert_eq!(response["lease_expires_at"], json!(expected_time));
            let expected = format!(
                "{{\"lease_generation\":{},\"lease_expires_at\":\"{expected_time}\"}}",
                response["lease_generation"]
            );
            assert_eq!(bytes, expected.as_bytes());
            response["lease_expires_at"] = json!("@checked-clock");
        } else {
            assert_eq!(
                storage(&state.pool, false).await?,
                initial,
                "failed heartbeat mutated domain storage"
            );
        }
        assert_eq!(response, recipe["response"], "response {}", recipe["name"]);
        if let Some(wire) = recipe["wire_hex"].as_str() {
            assert_eq!(hex(&bytes)?, wire, "wire {}", recipe["name"]);
        }
        let stored = storage(&state.pool, true).await?;
        let key = recipe["storage"].as_str().ok_or("storage key")?;
        assert_eq!(stored, f["snapshots"][key], "storage {}", recipe["name"]);
    }
    assert_eq!(
        f["cases"]
            .as_array()
            .ok_or("cases")?
            .iter()
            .filter(|r| r["native_integer_path"].is_string())
            .count(),
        23
    );
    race(&app, &state.pool, &f["race"]).await?;
    state.pool.close().await;
    Ok(())
}

async fn race(app: &Router, pool: &PgPool, expected: &Value) -> Result<()> {
    sqlx::raw_sql(expected["setup"].as_str().ok_or("race setup")?)
        .execute(pool)
        .await?;
    let before = clock(pool).await?;
    let mut lock = pool.begin().await?;
    sqlx::query("SELECT id FROM attempts WHERE id=$1::text::uuid FOR UPDATE")
        .bind(ATTEMPT)
        .execute(&mut *lock)
        .await?;
    let recipe = json!({"method":"POST","path":expected["path"],"role":"researcher",
        "headers":[["X-Lease-Token","cr_lease_fixture"],["X-Lease-Generation","1"]]});
    let pending = (0..2)
        .map(|_| {
            let app = app.clone();
            let recipe = recipe.clone();
            tokio::spawn(async move { call(&app, &recipe).await })
        })
        .collect::<Vec<_>>();
    let waited = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if pending.iter().any(tokio::task::JoinHandle::is_finished) {
                return Err("heartbeat finished before both PostgreSQL lock waiters".into());
            }
            sqlx::query("SELECT pg_stat_clear_snapshot()").execute(&mut *lock).await?;
            let waiters: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND state='active' AND wait_event_type='Lock' AND pid<>pg_backend_pid()")
                .fetch_one(&mut *lock).await?;
            if waiters==2 { return Ok::<_,Box<dyn Error+Send+Sync>>(waiters); }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await;
    let waiters = match waited {
        Ok(Ok(waiters)) => waiters,
        other => {
            for task in &pending {
                task.abort();
            }
            for task in pending {
                let _ = task.await;
            }
            return match other {
                Err(error) => Err(error.into()),
                Ok(Err(error)) => Err(error),
                Ok(Ok(_)) => unreachable!(),
            };
        }
    };
    assert_eq!(json!(waiters), expected["observed_lock_waiters"]);
    println!("observed {waiters} actual heartbeat PostgreSQL lock waiters");
    lock.commit().await?;
    let mut outputs = Vec::new();
    let mut statuses = Vec::new();
    let mut renewals = Vec::new();
    for task in pending {
        let (status, _, mut output, bytes) = task.await??;
        assert_eq!(status, 200);
        let expires: Timestamp = output["lease_expires_at"]
            .as_str()
            .ok_or("race expiry")?
            .parse()?;
        let text = cannery_server::timestamps::public_timestamp(expires);
        assert_eq!(
            bytes,
            format!("{{\"lease_generation\":1,\"lease_expires_at\":\"{text}\"}}").as_bytes()
        );
        statuses.push(status);
        renewals.push(expires);
        output["lease_expires_at"] = json!("@checked-clock");
        outputs.push(output);
    }
    let after = clock(pool).await?;
    assert!(
        renewals
            .iter()
            .all(|expiry| before.0 + TimeDelta::seconds(TTL) <= expiry.0
                && expiry.0 <= after.0 + TimeDelta::seconds(TTL))
    );
    let (stored_expiry, started): (Timestamp, Timestamp) =
        sqlx::query_as("SELECT lease_expires_at,started_at FROM attempts WHERE id=$1::text::uuid")
            .bind(ATTEMPT)
            .fetch_one(pool)
            .await?;
    let expiry_matches = renewals.contains(&stored_expiry);
    let start_matches = renewals
        .iter()
        .any(|expires| started.0 == expires.0 - TimeDelta::seconds(TTL));
    assert!(expiry_matches && start_matches);
    assert_eq!(
        json!(expiry_matches),
        expected["stored_expiry_matches_response"]
    );
    assert_eq!(
        json!(start_matches),
        expected["stored_start_matches_transaction"]
    );
    assert_eq!(json!(statuses), expected["statuses"]);
    assert_eq!(json!(outputs), expected["outputs"]);
    assert_eq!(storage(pool, true).await?, expected["storage"]);
    Ok(())
}
