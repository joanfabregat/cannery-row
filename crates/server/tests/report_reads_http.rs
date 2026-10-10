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
    api_models::{Page_ReportSummary_UUID_, ReportOut},
    application_with_report_context,
    report_routes::ReportContext,
    report_wire::ResponseContext,
};
use serde_json::{Value, json};
use std::fmt::Write;
use std::{collections::BTreeMap, error::Error, sync::Arc};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
fn fixture() -> Result<Value> {
    Ok(serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/report_reads_http/reference.json"
    ))?)
}
fn profile() -> ReportContext {
    ReportContext {
        reports: cannery_comments_reports::reports::JsonContext {
            decode_nesting_budget: 80,
        },
        attempts: cannery_attempts::model::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
        hypotheses: cannery_hypotheses::repo::JsonContext {
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
    if role != "none" {
        req = req.header(
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
    let response = app.clone().oneshot(req.body(Body::empty())?).await?;
    let status = response.status().as_u16();
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
    Ok((status, allow, value, hex))
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
fn native_wire(wire: &str, path: &str) -> Result<String> {
    let bytes = wire
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| Ok(u8::from_str_radix(std::str::from_utf8(pair)?, 16)?))
        .collect::<Result<Vec<_>>>()?;
    let native = if path.contains("/reports") {
        serde_json::to_vec(&serde_json::from_slice::<Page_ReportSummary_UUID_>(&bytes)?)?
    } else {
        serde_json::to_vec(&serde_json::from_slice::<ReportOut>(&bytes)?)?
    };
    let mut hex = String::new();
    for byte in native {
        write!(hex, "{byte:02x}")?;
    }
    Ok(hex)
}
fn assert_response(
    r: &Value,
    observed: &(u16, Option<String>, Value, String),
    i: usize,
) -> Result<()> {
    let (status, allow, value, wire) = observed;
    let native_refusal = match r["native_contract_refusal"].as_str() {
        None => false,
        Some(
            "partial-report"
            | "incomplete-claimed-measurement"
            | "incomplete-discrepancy"
            | "incomplete-comparison",
        ) => true,
        Some(_) => return Err("unknown native report refusal profile".into()),
    };
    if !native_refusal
        && r["status"] == 200
        && !r["path"].as_str().ok_or("path")?.contains("/reports")
    {
        let _: ReportOut = serde_json::from_value(r["response"].clone())
            .map_err(|error| format!("ordinary source report case{i}: {error}"))?;
    }
    let integer_paths = r["native_integer_paths"]
        .as_array()
        .ok_or("integer profile paths")?;
    if !integer_paths.is_empty() {
        assert!(!native_refusal);
        assert_integer_refusal(r, observed, i)?;
        return Ok(());
    }
    if native_refusal {
        assert_eq!(
            r["status"], 200,
            "frozen source accepts these malformed nested maps"
        );
        assert_eq!(
            *status, 500,
            "case{i} malformed fixed response must fail closed"
        );
        assert_eq!(allow, &None);
        assert_eq!(value, &Value::Null);
        assert_eq!(wire, "496e7465726e616c20536572766572204572726f72");
    } else {
        assert_eq!(
            json!(*status),
            r["status"],
            "case{i} {} response{value}",
            r["name"]
        );
        assert_eq!(json!(allow), r["allow"], "case{i}");
        assert_eq!(
            projection(value.clone()),
            projection(r["response"].clone()),
            "case{i} {}",
            r["name"]
        );
        if !r["wire_hex"].is_null() {
            if r["native_serialization_profile"] == "typed-report" {
                // Semantic response comparisons above remain complete. Native
                // bytes follow the declared DTO's serde field order and JSON
                // spelling; frozen source bytes remain in their original record.
                assert_eq!(
                    wire,
                    &native_wire(wire, r["path"].as_str().ok_or("path")?)?,
                    "case{i} native typed wire"
                );
            } else {
                assert_eq!(json!(wire), r["wire_hex"], "case{i} wire");
            }
        }
    }
    Ok(())
}
fn assert_integer_refusal(
    r: &Value,
    observed: &(u16, Option<String>, Value, String),
    i: usize,
) -> Result<()> {
    assert_eq!(r["role"], "researcher");
    assert!(
        [
            "query",
            "wide-query",
            "path",
            "wide-path",
            "wide-after-warm"
        ]
        .contains(&r["name"].as_str().ok_or("integer recipe")?)
    );
    let details = r["native_integer_paths"]
        .as_array()
        .ok_or("integer paths")?
        .iter()
        .map(|path| {
            assert!(
                [
                    "query/hypothesis",
                    "query/limit",
                    "path/number",
                    "path/sequence"
                ]
                .iter()
                .any(|expected| path == expected)
            );
            json!({"path":path,"message":"Input should be a signed 64-bit integer"})
        })
        .collect::<Vec<_>>();
    let (status, allow, value, _) = observed;
    assert_eq!(*status, 422, "case{i} checked integer refusal");
    assert_eq!(json!(allow), r["allow"]);
    assert_eq!(
        value,
        &json!({"error":{"code":"validation_failed","message":"request validation failed","details":details}})
    );
    Ok(())
}
#[tokio::test]
#[ignore = "requires guarded uniquely owned migrated PostgreSQL"]
async fn report_reads_match_production() -> Result<()> {
    let url = std::env::var("CANNERY_REPORT_READS_HTTP_DATABASE_URL")?;
    let settings = load_settings(
        None,
        &BTreeMap::from([("CANNERY_DATABASE_URL".into(), url)]),
    )?;
    let (app, state) = application_with_report_context(settings, Arc::new(profile()))?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("owned child required")?;
    if suffix.len() != 24 || !suffix.bytes().all(|v| v.is_ascii_hexdigit()) {
        return Err("owned child required".into());
    }
    sqlx::raw_sql(include_str!("fixtures/report_reads_http/seed.sql"))
        .execute(&state.pool)
        .await?;
    let f = fixture()?;
    for (path, count) in [
        ("query/hypothesis", 7),
        ("query/limit", 4),
        ("path/number", 8),
        ("path/sequence", 5),
    ] {
        assert_eq!(
            f["cases"]
                .as_array()
                .ok_or("cases")?
                .iter()
                .filter(|recipe| recipe["native_integer_paths"]
                    .as_array()
                    .is_some_and(|paths| paths.iter().any(|value| value == path)))
                .count(),
            count,
            "declared checked integer profile {path}"
        );
    }
    for (profile, count) in [
        ("partial-report", 2),
        ("incomplete-claimed-measurement", 1),
        ("incomplete-discrepancy", 1),
        ("incomplete-comparison", 1),
    ] {
        assert_eq!(
            f["cases"]
                .as_array()
                .ok_or("cases")?
                .iter()
                .filter(|r| r["native_contract_refusal"] == profile)
                .count(),
            count,
            "retain explicit malformed fixed-contract refusal coverage"
        );
    }
    for (i, r) in f["cases"].as_array().ok_or("cases")?.iter().enumerate() {
        if let Some(setup) = r["setup"].as_str() {
            sqlx::raw_sql(setup).execute(&state.pool).await?;
        }
        let before = storage(&state.pool).await?;
        let observed = call(&app, r).await?;
        assert_eq!(
            storage(&state.pool).await?,
            before,
            "case{i} read must not mutate durable storage"
        );
        assert_response(r, &observed, i)?;
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
