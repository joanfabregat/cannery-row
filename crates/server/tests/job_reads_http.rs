//! Independent frozen production report reads, raw storage and fixed wire bytes.
#![forbid(unsafe_code)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use cannery_core::settings::load_settings;
use cannery_server::{
    api_models::{JobOut, Page_JobOut_UUID_},
    application_with_job_read_and_claim_context,
    job_read_routes::JobReadContext,
    job_read_wire::ResponseContext,
};
use serde_json::{Value, json};
use std::fmt::Write;
use std::{collections::BTreeMap, error::Error, sync::Arc};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
fn fixture() -> Result<Value> {
    Ok(serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/job_reads_http/reference.json"
    ))?)
}
fn profile() -> JobReadContext {
    JobReadContext {
        jobs: cannery_jobs::repo::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
        attempts: cannery_attempts::model::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
        response: ResponseContext {
            inferred_nesting_budget: 80,
            representation_budget: 80,
        },
    }
}
fn projection(v: Value) -> Value {
    match v {
        Value::Object(v)
            if v.get("error")
                .is_some_and(|e| e["code"] == "validation_failed") =>
        {
            let e = &v["error"];
            json!({"error":{"code":e["code"],"details":e["details"].as_array().map(|a|a.iter().map(|d|json!({"path":d["path"]})).collect::<Vec<_>>())}})
        }
        Value::Number(n) if n.to_string().contains(['.', 'e', 'E']) => n
            .as_f64()
            .and_then(serde_json::Number::from_f64)
            .map_or(Value::Number(n), Value::Number),
        Value::Object(v) => Value::Object(v.into_iter().map(|(k, v)| (k, projection(v))).collect()),
        Value::Array(v) => Value::Array(v.into_iter().map(projection).collect()),
        v => v,
    }
}
async fn call(app: &Router, r: &Value) -> Result<(u16, Option<String>, Value, String)> {
    let mut req = Request::builder()
        .method(r["method"].as_str().ok_or("method")?)
        .uri(r["path"].as_str().ok_or("path")?);
    let role = r["role"].as_str().ok_or("role")?;
    if role != "none" && role != "malformed" {
        req = req.header(
            "authorization",
            format!(
                "Bearer {}track_http_{role}",
                if matches!(
                    role,
                    "agent" | "foreign-agent" | "tester" | "evaluator" | "experimenter"
                ) {
                    "cr_svc_"
                } else {
                    "cr_pat_"
                }
            ),
        );
    }
    if role == "malformed" {
        req = req.header("authorization", "Basic x");
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(
            r["body"].as_str().unwrap_or_default().to_owned(),
        ))?)
        .await?;
    let status = response.status().as_u16();
    if r["method"] == "HEAD" && r["path"] != "/api/projects/matrix/jobs/claims" {
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(
            response
                .headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok()),
            Some("31")
        );
    }
    let allow = response
        .headers()
        .get("allow")
        .map(|v| v.to_str().map(str::to_owned))
        .transpose()?;
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let mut hex = String::new();
    for b in &bytes {
        write!(hex, "{b:02x}")?;
    }
    let value = if status == 500 {
        assert_eq!(&bytes[..], b"Internal Server Error");
        Value::Null
    } else if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)?
    };
    if r["native"]["kind"] == "checked-integer" {
        assert_eq!(status, 422);
        assert_eq!(
            value,
            json!({"error":{"code":"validation_failed","message":"request validation failed",
                "details":[{"path":r["native"]["path"],"message":"Input should be a signed 64-bit integer"}]}}),
            "complete native integer refusal"
        );
    }
    Ok((status, allow, projection(value), hex))
}
// Native differences are finite recipe annotations, never response normalization.
fn expected(recipe: &Value) -> Result<(Value, Value)> {
    let mut response = projection(recipe["response"].clone());
    let native = &recipe["native"];
    if native.is_null() {
        return Ok((recipe["status"].clone(), response));
    }
    assert_eq!(recipe["method"], "GET");
    assert_eq!(recipe["role"], "researcher");
    match native["kind"].as_str().ok_or("native kind")? {
        "checked-integer" => {
            let input = native["input"].as_str().ok_or("integer input")?;
            assert!(input.parse::<i64>().is_err());
            let path = native["path"].as_str().ok_or("integer path")?;
            assert!(matches!(
                path,
                "path/number" | "path/sequence" | "query/limit"
            ));
            assert!(
                matches!(input, "8.0" | "1.0" | "٨" | "١" | "9223372036854775808")
                    || (matches!(input.len(), 4300 | 4301) && input.bytes().all(|v| v == b'9'))
            );
            Ok((
                json!(422),
                json!({"error":{"code":"validation_failed","details":[{"path":path}]}}),
            ))
        }
        "json-text" => {
            assert_eq!(recipe["status"], 200);
            let field = native["field"].as_str().ok_or("text field")?;
            assert!(matches!(field, "track" | "output_prefix"));
            assert_eq!(recipe["name"], format!("recovered-{field}"));
            assert_eq!(response[field], native["source"]);
            let rendered = native["value"].as_str().ok_or("JSON text")?;
            let input: Value = serde_json::from_str(rendered)?;
            assert!(!input.is_string());
            assert_eq!(serde_json::to_string(&input)?, rendered);
            response[field] = native["value"].clone();
            Ok((json!(200), response))
        }
        "typed-refusal" => {
            assert_eq!(recipe["status"], 200);
            let field = native["field"].as_str().ok_or("typed field")?;
            assert_eq!(recipe["name"], format!("recovered-{field}"));
            match field {
                "steps" => assert!(
                    native["input"] == json!([{"name":null,"revision":true}])
                        || native["input"] == json!([{"name":[],"revision":1.0}])
                ),
                "logs" => {
                    assert!(
                        native["input"] == json!([{}])
                            || native["input"] == json!([{"key":"x","extra":null}])
                    );
                    assert_eq!(response["logs"], native["input"]);
                }
                "evidence" => {
                    assert!(
                        native["input"] == json!({})
                            || native["input"]
                                == serde_json::from_str::<Value>(
                                    r#"{"wide":123456789012345678901234567890,"x":[1.25]}"#
                                )?
                    );
                    assert_eq!(response["evidence"], native["input"]);
                }
                _ => return Err("unknown typed refusal".into()),
            }
            Ok((json!(500), Value::Null))
        }
        _ => Err("unknown native recipe".into()),
    }
}
fn typed_wire(recipe: &Value, hex: &str) -> Result<()> {
    let bytes = hex
        .as_bytes()
        .chunks(2)
        .map(|part| Ok(u8::from_str_radix(std::str::from_utf8(part)?, 16)?))
        .collect::<Result<Vec<_>>>()?;
    let encoded = if recipe["path"]
        .as_str()
        .ok_or("path")?
        .contains("/hypotheses/")
    {
        serde_json::to_vec(&serde_json::from_slice::<Page_JobOut_UUID_>(&bytes)?)?
    } else {
        serde_json::to_vec(&serde_json::from_slice::<JobOut>(&bytes)?)?
    };
    assert_eq!(encoded, bytes, "actual handler DTO serialization");
    Ok(())
}
async fn storage(pool: &sqlx::PgPool) -> Result<Value> {
    let mut output = json!({});
    for (table, order) in [
        ("tracks", "id"),
        ("hypotheses", "id"),
        ("hypothesis_revisions", "hypothesis_id,revision"),
        ("attempts", "id"),
        ("artifacts", "id"),
        ("measurements", "id"),
        ("comparisons", "id"),
        ("phase_outputs", "id"),
        ("review_cases", "id"),
        ("decisions", "id"),
        ("audit_events", "seq"),
        ("idempotency_keys", "scope,actor,key"),
        ("search_documents", "id"),
        ("jobs", "id"),
        ("manifests", "id"),
        ("uploads", "id"),
        ("attempt_failures", "id"),
    ] {
        let rows = sqlx::query_scalar::<_, String>(&format!(
            "SELECT row_to_json(t)::text FROM {table} t ORDER BY {order}"
        ))
        .fetch_all(pool)
        .await?;
        output[table] = json!(rows);
    }
    Ok(output)
}
#[tokio::test]
#[ignore = "requires guarded uniquely owned migrated PostgreSQL"]
async fn job_reads_match_production() -> Result<()> {
    let url = std::env::var("CANNERY_JOB_READS_HTTP_DATABASE_URL")?;
    let settings = load_settings(
        None,
        &BTreeMap::from([("CANNERY_DATABASE_URL".into(), url)]),
    )?;
    let claims = cannery_server::job_claim_routes::JobClaimContext {
        jobs: cannery_jobs::repo::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
        attempts: cannery_attempts::model::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
        rendering: cannery_research::science::RenderingContext { nesting_budget: 80 },
        hash_budget: 80,
        response_budget: 80,
        mint: || {
            Ok(cannery_identity::secrets::new_secret("cr_job_")?
                .plaintext()
                .clone())
        },
    };
    let (app, state) = application_with_job_read_and_claim_context(
        settings,
        Arc::new(profile()),
        Arc::new(claims),
    )?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("owned child required")?;
    if suffix.len() != 24 || !suffix.bytes().all(|v| v.is_ascii_hexdigit()) {
        return Err("owned child required".into());
    }
    sqlx::raw_sql(include_str!("fixtures/attempt_reads_http/seed.sql"))
        .execute(&state.pool)
        .await?;
    sqlx::raw_sql(include_str!("fixtures/job_reads_http/seed.sql"))
        .execute(&state.pool)
        .await?;
    let f = fixture()?;
    for (i, r) in f["cases"].as_array().ok_or("cases")?.iter().enumerate() {
        if let Some(setup) = r["setup"].as_str() {
            sqlx::raw_sql(setup).execute(&state.pool).await?;
        }
        let before = storage(&state.pool).await?;
        let (expected_status, expected_value) = expected(r)?;
        let (status, allow, value, wire) = call(&app, r).await?;
        assert_eq!(
            json!(status),
            expected_status,
            "case{i} {} response{value}",
            r["name"]
        );
        assert_eq!(json!(allow), r["allow"], "case{i}");
        assert_eq!(value, expected_value, "case{i} {}", r["name"]);
        if status == 200 && !r["wire_hex"].is_null() {
            typed_wire(r, &wire)?;
        } else if status == 500 {
            assert_eq!(
                wire, "496e7465726e616c20536572766572204572726f72",
                "case{i} refusal wire"
            );
        } else if !r["wire_hex"].is_null() {
            assert_eq!(json!(wire), r["wire_hex"], "case{i} wire");
        }
        assert_eq!(
            storage(&state.pool).await?,
            before,
            "case{i} request changed raw storage"
        );
        let snapshot = r["storage"].as_str().ok_or("storage")?;
        assert_eq!(
            storage(&state.pool).await?,
            f["snapshots"][snapshot],
            "case{i} storage"
        );
    }
    state.pool.close().await;
    Ok(())
}
