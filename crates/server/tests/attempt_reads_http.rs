//! Independent native replay against actual frozen production attempt reads.
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
    application_with_attempt_read_context, attempt_read_lookup::LookupContext,
    attempt_read_routes::AttemptReadContext, attempt_read_wire::ResponseContext,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::fmt::Write;
use std::{collections::BTreeMap, error::Error, sync::Arc};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
fn fixture() -> Result<Value> {
    Ok(serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/attempt_reads_http/reference.json"
    ))?)
}
fn profile() -> AttemptReadContext {
    AttemptReadContext {
        repository: cannery_attempts::model::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
        response: ResponseContext {
            inferred_nesting_budget: 80,
        },
        hypothesis_lookup: LookupContext::BorrowedUnnamed,
    }
}
fn hex(bytes: &[u8]) -> Result<String> {
    let mut out = String::new();
    for byte in bytes {
        write!(&mut out, "{byte:02x}")?;
    }
    Ok(out)
}
fn json_wire(value: &str) -> Result<Value> {
    let bytes = value
        .as_bytes()
        .chunks(2)
        .map(|chunk| Ok(u8::from_str_radix(std::str::from_utf8(chunk)?, 16)?))
        .collect::<Result<Vec<_>>>()?;
    Ok(normalized(serde_json::from_slice(&bytes)?))
}
fn normalized(value: Value) -> Value {
    match value {
        Value::Array(v) => Value::Array(v.into_iter().map(normalized).collect()),
        Value::Object(v) => Value::Object(v.into_iter().map(|(k, v)| (k, normalized(v))).collect()),
        Value::Number(v) if v.to_string().contains(['.', 'e', 'E']) => v
            .as_f64()
            .and_then(serde_json::Number::from_f64)
            .map_or(Value::Number(v), Value::Number),
        v => v,
    }
}
fn output(value: Value) -> Value {
    if value
        .get("error")
        .is_some_and(|v| v["code"] == "validation_failed")
    {
        let e = &value["error"];
        return json!({"error":{"code":e["code"],"details":e["details"].as_array().map(|v|v.iter().map(|v|json!({"path":v["path"]})).collect::<Vec<_>>())}});
    }
    normalized(value)
}
async fn storage(pool: &PgPool) -> Result<Value> {
    let mut value = json!({});
    for (table, order) in [
        ("tracks", "id"),
        ("hypotheses", "id"),
        ("hypothesis_revisions", "hypothesis_id,revision"),
        ("attempts", "id"),
        ("artifacts", "id"),
        ("attempt_failures", "id"),
        ("phase_outputs", "id"),
        ("manifests", "id"),
        ("uploads", "id"),
        ("jobs", "id"),
        ("review_cases", "id"),
        ("decisions", "id"),
        ("audit_events", "seq"),
        ("idempotency_keys", "scope,actor,key"),
        ("search_documents", "id"),
    ] {
        value[table] = json!(
            sqlx::query_scalar::<_, String>(&format!(
                "SELECT row_to_json(t)::text FROM {table} t ORDER BY {order}"
            ))
            .fetch_all(pool)
            .await?
        );
    }
    Ok(value)
}
async fn call(app: &Router, recipe: &Value) -> Result<(u16, Option<String>, Value, String)> {
    let target = recipe["path"].as_str().ok_or("path")?;
    // Match the HTTP client's UTF-8 percent encoding for authored Unicode paths.
    let target = if target.is_ascii() {
        target.to_owned()
    } else {
        let url = url::Url::parse(&format!("http://fixture.invalid{target}"))?;
        url[url::Position::BeforePath..url::Position::AfterQuery].to_owned()
    };
    let mut request = Request::builder()
        .method(recipe["method"].as_str().ok_or("method")?)
        .uri(target);
    let role = recipe["role"].as_str().ok_or("role")?;
    if role != "none" {
        request = request.header(
            "authorization",
            format!(
                "Bearer {}track_http_{role}",
                if role.contains("agent") {
                    "cr_svc_"
                } else {
                    "cr_pat_"
                }
            ),
        );
    }
    let response = app.clone().oneshot(request.body(Body::empty())?).await?;
    let status = response.status().as_u16();
    let allow = response
        .headers()
        .get("allow")
        .map(|v| v.to_str().map(str::to_owned))
        .transpose()?;
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let body = if status == 500 {
        assert_eq!(&bytes[..], b"Internal Server Error");
        Value::Null
    } else if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)?
    };
    Ok((status, allow, body, hex(&bytes)?))
}
fn assert_native_read_refusal(r: &Value, status: u16, response: &Value, wire: &str) -> Result<()> {
    let content: Value =
        serde_json::from_str(r["native_content"].as_str().ok_or("read refusal content")?)?;
    match r["native_refusal"].as_str().ok_or("read refusal profile")? {
        "claimed_sheet" => {
            assert_eq!(r["name"], "recovered-sheet");
            assert!(!content.is_null());
            assert!(
                serde_json::from_value::<cannery_server::api_models::ReadEvidenceEnvelope>(
                    content.clone()
                )
                .is_err()
            );
            assert_eq!(r["status"], if content.is_object() { 200 } else { 500 });
        }
        "log_reference" => {
            assert_eq!(r["name"], "recovered-log-reference");
            assert!(
                serde_json::from_value::<Vec<cannery_server::api_models::LogRef>>(content.clone())
                    .is_err()
            );
            assert_eq!(
                r["status"],
                if content[0]["size_bytes"] == json!(1.5) {
                    500
                } else {
                    200
                }
            );
        }
        _ => return Err("unknown native attempt read refusal".into()),
    }
    assert_eq!(status, 500);
    assert_eq!(response, &Value::Null);
    assert_eq!(wire, hex(b"Internal Server Error")?);
    Ok(())
}
fn assert_native_integer_refusal(r: &Value, status: u16, response: &Value) -> Result<()> {
    let path = r["native_integer_path"]
        .as_str()
        .ok_or("integer refusal path")?;
    assert!(
        [
            "query/limit",
            "query/before",
            "path/number",
            "path/sequence"
        ]
        .contains(&path)
    );
    assert!(
        [
            "limit",
            "sequence-cursor",
            "number",
            "number-detail",
            "sequence",
            "wide-after-warm",
            "wide-detail-after-warm"
        ]
        .contains(&r["name"].as_str().ok_or("integer recipe")?)
    );
    assert_eq!(r["role"], "researcher");
    assert_eq!(status, 422);
    assert_eq!(
        response,
        &json!({"error":{"code":"validation_failed","message":"request validation failed","details":[{"path":path,"message":"Input should be a signed 64-bit integer"}]}})
    );
    Ok(())
}
fn assert_declared_profiles(cases: &[Value]) {
    for (path, count) in [
        ("query/limit", 20),
        ("query/before", 7),
        ("path/number", 17),
        ("path/sequence", 9),
    ] {
        assert_eq!(
            cases
                .iter()
                .filter(|recipe| recipe["native_integer_path"] == path)
                .count(),
            count,
            "declared integer profile {path}"
        );
    }
    for refusal in ["claimed_sheet", "log_reference"] {
        assert_eq!(
            cases
                .iter()
                .filter(|recipe| recipe["native_refusal"] == refusal)
                .count(),
            4,
            "declared read refusal {refusal}"
        );
    }
    assert_eq!(
        cases
            .iter()
            .filter(|recipe| recipe["name"] == "sheet-restored")
            .count(),
        1
    );
}
#[tokio::test]
#[ignore = "requires positive exact selection and a guarded fresh migrated child"]
async fn attempt_reads_match_production() -> Result<()> {
    let url = std::env::var("CANNERY_ATTEMPT_READS_HTTP_DATABASE_URL")?;
    let settings = load_settings(
        None,
        &BTreeMap::from([("CANNERY_DATABASE_URL".into(), url)]),
    )?;
    let (app, state) = application_with_attempt_read_context(settings, Arc::new(profile()))?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("owned nonce child required")?;
    if suffix.len() != 24 || !suffix.bytes().all(|v| v.is_ascii_hexdigit()) {
        return Err("owned nonce child required".into());
    }
    sqlx::raw_sql(include_str!("fixtures/attempt_reads_http/seed.sql"))
        .execute(&state.pool)
        .await?;
    let f = fixture()?;
    let cases = f["cases"].as_array().ok_or("recipes")?;
    assert_declared_profiles(cases);
    for r in cases {
        if let Some(setup) = r["setup"].as_str() {
            sqlx::raw_sql(setup).execute(&state.pool).await?;
        }
        let initial = storage(&state.pool).await?;
        let (status, allow, response, wire) = call(&app, r).await?;
        if r["native_integer_path"].is_string() {
            assert!(r["native_refusal"].is_null());
            assert_native_integer_refusal(r, status, &response)?;
        } else if r["native_refusal"].is_string() {
            assert!(r["native_integer_path"].is_null());
            assert_native_read_refusal(r, status, &response, &wire)?;
        } else {
            assert!(r["native_refusal"].is_null());
            assert!(r["native_integer_path"].is_null());
            assert_eq!(
                json!(status),
                r["status"],
                "status {}; response {}",
                r["name"],
                response
            );
            assert_eq!(
                output(response.clone()),
                normalized(r["response"].clone()),
                "response {}",
                r["name"]
            );
        }
        assert_eq!(json!(allow), r["allow"], "allow {}", r["name"]);
        if let Some(expected) = r["wire_hex"].as_str() {
            if r["native_wire_json"] == true {
                assert_eq!(r["name"], "fixed-model");
                assert_eq!(status, 200);
                assert_eq!(
                    json_wire(&wire)?,
                    json_wire(expected)?,
                    "typed JSON wire {}",
                    r["path"]
                );
            } else {
                assert_eq!(wire, expected, "wire {}", r["name"]);
            }
        }
        let stored = storage(&state.pool).await?;
        assert_eq!(stored, initial, "read changed raw storage {}", r["name"]);
        let key = r["storage"].as_str().ok_or("storage key")?;
        for (table, rows) in stored.as_object().ok_or("tables")? {
            assert_eq!(
                rows, &f["snapshots"][key][table],
                "raw storage {}; table {table}",
                r["name"]
            );
        }
    }
    state.pool.close().await;
    Ok(())
}
