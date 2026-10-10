//! Production read-route observations with full storage and exact model bytes.
#![forbid(unsafe_code)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use cannery_core::{
    contracts::phases::{Phase, PhaseSchemas},
    settings::load_settings,
};
use cannery_server::{
    api_models::{AttentionOut, ReviewCasePage, cannery_row__reviews__routes__ReviewCaseOut},
    application_with_review_attention_context,
    review_attention_routes::ReviewAttentionContext,
    review_attention_wire::ResponseContext,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::fmt::Write;
use std::{collections::BTreeMap, error::Error, sync::Arc};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
fn fixture() -> Result<Value> {
    Ok(serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/review_attention_http/reference.json"
    ))?)
}
#[test]
fn stalled_verification_interval_requires_nonnegative_checked_seconds() {
    for seconds in ["-1", "9223372036854775808"] {
        assert!(
            load_settings(
                None,
                &BTreeMap::from([
                    ("CANNERY_DATABASE_URL".into(), "unused".into()),
                    (
                        "CANNERY_LEASES_STALLED_VERIFICATION_SECONDS".into(),
                        seconds.into()
                    )
                ])
            )
            .is_err(),
            "invalid native installation interval {seconds}"
        );
    }
    for seconds in ["0", "600", "2147483648"] {
        assert!(
            load_settings(
                None,
                &BTreeMap::from([
                    ("CANNERY_DATABASE_URL".into(), "unused".into()),
                    (
                        "CANNERY_LEASES_STALLED_VERIFICATION_SECONDS".into(),
                        seconds.into()
                    )
                ])
            )
            .is_ok(),
            "nonnegative checked native installation interval {seconds}"
        );
    }
}
fn profile() -> ReviewAttentionContext {
    ReviewAttentionContext {
        reviews: cannery_reviews::JsonContext {
            decode_nesting_budget: 80,
        },
        hypotheses: cannery_hypotheses::repo::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
        attempts: cannery_attempts::model::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
        response: ResponseContext {
            inferred_nesting_budget: 80,
        },
    }
}
fn canonical(value: Value, ids: &BTreeMap<String, String>) -> Value {
    match value {
        Value::Number(number) if number.to_string().contains(['.', 'e', 'E']) => number
            .as_f64()
            .and_then(serde_json::Number::from_f64)
            .map_or(Value::Number(number), Value::Number),
        Value::Array(v) => Value::Array(v.into_iter().map(|v| canonical(v, ids)).collect()),
        Value::Object(v) if v.contains_key("error") => {
            let e = &v["error"];
            let mut projected = json!({"code":e["code"],"details":e["details"].as_array().map(|a|a.iter().map(|d|json!({"path":d["path"]})).collect::<Vec<_>>())});
            if e["code"] != "validation_failed" {
                projected["message"] = e["message"].clone();
            }
            json!({"error":projected})
        }
        Value::Object(v) => {
            Value::Object(v.into_iter().map(|(k, v)| (k, canonical(v, ids))).collect())
        }
        Value::String(s) => ids.get(&s).map_or_else(
            || {
                if chrono::DateTime::parse_from_rfc3339(&s.replace(' ', "T"))
                    .or_else(|_| chrono::DateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S%.f%#z"))
                    .is_ok_and(|instant| chrono::Datelike::year(&instant) >= 2026)
                {
                    json!("@clock")
                } else {
                    json!(s)
                }
            },
            |v| json!(v),
        ),
        v => v,
    }
}
fn raw(hex: &str) -> Result<Vec<u8>> {
    hex.as_bytes()
        .chunks(2)
        .map(|v| Ok(u8::from_str_radix(std::str::from_utf8(v)?, 16)?))
        .collect()
}
fn native_limit_refusal(recipe: &Value) -> Result<bool> {
    if !matches!(
        recipe["name"].as_str(),
        Some("review-query" | "attention-query" | "wide-limit-after-warm")
    ) {
        return Ok(false);
    }
    let Some((_, query)) = recipe["path"].as_str().ok_or("path")?.split_once('?') else {
        return Ok(false);
    };
    let params: Vec<_> = url::form_urlencoded::parse(query.as_bytes()).collect();
    Ok(params.len() == 1 && params[0].0 == "limit" && params[0].1.parse::<i64>().is_err())
}
async fn call(app: &Router, recipe: &Value) -> Result<(u16, Option<String>, Value, String)> {
    let mut request = Request::builder()
        .method(recipe["method"].as_str().ok_or("method")?)
        .uri(recipe["path"].as_str().ok_or("path")?);
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
    if let Some(key) = recipe["key"].as_str() {
        request = request.header("idempotency-key", key);
    }
    let body = recipe["body_hex"].as_str().map(raw).transpose()?;
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let r = app
        .clone()
        .oneshot(request.body(body.map_or_else(Body::empty, Body::from))?)
        .await?;
    let status = r.status().as_u16();
    let allow = r
        .headers()
        .get("allow")
        .map(|v| v.to_str().map(str::to_owned))
        .transpose()?;
    let bytes = to_bytes(r.into_body(), usize::MAX).await?;
    let value = if status == 500 {
        assert_eq!(&bytes[..], b"Internal Server Error");
        Value::Null
    } else if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)?
    };
    if native_limit_refusal(recipe)? {
        assert_eq!(status, 422);
        assert_eq!(allow, None);
        assert_eq!(
            value,
            json!({"error":{"code":"validation_failed","message":"request validation failed",
            "details":[{"path":"query/limit","message":"Input should be a signed 64-bit integer"}]}})
        );
    }
    Ok((status, allow, value, hex(&bytes)))
}
fn hex(bytes: &[u8]) -> String {
    let mut out = String::new();
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}
async fn storage(pool: &PgPool) -> Result<Value> {
    let tables = [
        ("hypotheses", "project_id,number"),
        ("attempts", "id"),
        ("attempt_failures", "id"),
        ("phase_outputs", "id"),
        ("review_cases", "opened_at,id"),
        ("decisions", "decided_at,id"),
        ("jobs", "id"),
        ("audit_events", "seq"),
        ("idempotency_keys", "scope,actor,key"),
    ];
    let mut result = json!({});
    for (table, order) in tables {
        result[table] = sqlx::query_scalar::<_, Value>(&format!(
            "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY {order}),'[]') FROM {table} t"
        ))
        .fetch_one(pool)
        .await?;
    }
    result["stalled_clock_relations"]=sqlx::query_scalar::<_,Value>("SELECT coalesce(jsonb_agg(jsonb_build_array(id,created_at < now()-make_interval(secs=>3600),created_at > now()-make_interval(secs=>3600)) ORDER BY id),'[]') FROM jobs").fetch_one(pool).await?;
    Ok(canonical(result, &BTreeMap::new()))
}
fn native_wire(wire: &str, path: &str) -> Result<String> {
    let bytes = raw(wire)?;
    let encoded = if path.contains("/attention") {
        serde_json::to_vec(&serde_json::from_slice::<AttentionOut>(&bytes)?)?
    } else if path
        .split('?')
        .next()
        .is_some_and(|path| path.ends_with("/review-cases"))
    {
        serde_json::to_vec(&serde_json::from_slice::<ReviewCasePage>(&bytes)?)?
    } else {
        serde_json::to_vec(&serde_json::from_slice::<
            cannery_row__reviews__routes__ReviewCaseOut,
        >(&bytes)?)?
    };
    Ok(hex(&encoded))
}
fn assert_recovery(r: &Value, status: u16, output: &Value) -> Result<()> {
    if r["name"] != "recovery-model" {
        return Ok(());
    }
    match r["path"].as_str().ok_or("path")?.rsplit('/').next() {
        Some(
            "00000000-0000-0000-0000-000000000307"
            | "00000000-0000-0000-0000-000000000308"
            | "00000000-0000-0000-0000-000000000310"
            | "00000000-0000-0000-0000-000000000311",
        ) => {
            assert_eq!(
                status, 500,
                "corrupt failure map/log item or non-object verification"
            );
            assert_eq!(output, &Value::Null);
        }
        Some("00000000-0000-0000-0000-000000000309") => {
            assert_eq!(status, 200);
            assert_eq!(
                output["verification"],
                Value::Null,
                "absent verification remains readable"
            );
        }
        Some("00000000-0000-0000-0000-000000000312") => {
            assert_eq!(status, 200);
            let fields = &output["verification"]["front_matter"]["extensions"]["project_fields"];
            assert_eq!(fields["large"].to_string(), "9".repeat(501));
            assert_eq!(fields["float"].as_f64(), Some(1e-7));
            assert_eq!(fields["unicode"], "é😀");
            assert_eq!(fields["raw"], json!([true, null]));
        }
        _ => return Err("unknown recovery model recipe".into()),
    }
    Ok(())
}
#[tokio::test]
#[ignore = "requires guarded uniquely owned migrated PostgreSQL"]
#[allow(
    clippy::too_many_lines,
    reason = "sequential attention corpus preserves settings transitions and exact storage assertions"
)]
async fn review_attention_match_production() -> Result<()> {
    let url = std::env::var("CANNERY_REVIEW_ATTENTION_HTTP_DATABASE_URL")?;
    let settings = load_settings(
        None,
        &BTreeMap::from([("CANNERY_DATABASE_URL".into(), url.clone())]),
    )?;
    let (mut app, mut state) =
        application_with_review_attention_context(settings, Arc::new(profile()))?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("owned child required")?;
    if suffix.len() != 24 || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("owned child required".into());
    }
    sqlx::raw_sql(include_str!("fixtures/review_attention_http/seed.sql"))
        .execute(&state.pool)
        .await?;
    let documents: Vec<Value> = sqlx::query_scalar(
        "SELECT front_matter FROM phase_outputs WHERE stage='verification' ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?;
    assert_eq!(documents.len(), 5);
    let phases = PhaseSchemas::new()?;
    for document in documents {
        assert_eq!(
            phases.violations(Phase::Verification, &document),
            [],
            "seeded verification reports satisfy the phase schema"
        );
    }
    let f = fixture()?;
    assert_eq!(
        f["cases"]
            .as_array()
            .ok_or("cases")?
            .iter()
            .map(native_limit_refusal)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|v| *v)
            .count(),
        12
    );
    let mut installation_seconds = None;
    for (i, r) in f["cases"].as_array().ok_or("cases")?.iter().enumerate() {
        if let Some(seconds) = r["stalled_seconds"].as_str()
            && installation_seconds.as_deref() != Some(seconds)
        {
            state.pool.close().await;
            let settings = load_settings(
                None,
                &BTreeMap::from([
                    ("CANNERY_DATABASE_URL".into(), url.clone()),
                    (
                        "CANNERY_LEASES_STALLED_VERIFICATION_SECONDS".into(),
                        seconds.into(),
                    ),
                ]),
            )?;
            (app, state) =
                application_with_review_attention_context(settings, Arc::new(profile()))?;
            installation_seconds = Some(seconds.to_owned());
        }
        if let Some(setup) = r["setup"].as_str() {
            sqlx::raw_sql(setup).execute(&state.pool).await?;
        }
        let before = storage(&state.pool).await?;
        let (status, allow, output, wire) = call(&app, r).await?;
        assert_eq!(
            storage(&state.pool).await?,
            before,
            "read must preserve all durable storage"
        );
        assert_recovery(r, status, &output)?;
        if native_limit_refusal(r)? {
            assert_eq!(
                storage(&state.pool).await?,
                canonical(r["storage"].clone(), &BTreeMap::new()),
                "native refusal preserves the complete source storage snapshot"
            );
            continue;
        }
        assert_eq!(json!(status), r["status"], "status {i} {}", r["name"]);
        assert_eq!(json!(allow), r["allow"], "allow {i}");
        assert_eq!(
            canonical(output, &BTreeMap::new()),
            canonical(r["response"].clone(), &BTreeMap::new()),
            "body {i}"
        );
        assert_eq!(
            storage(&state.pool).await?,
            canonical(r["storage"].clone(), &BTreeMap::new()),
            "storage {i}"
        );
        if let Some(expected) = r["wire_hex"].as_str() {
            if r["native_serialization_profile"] == "typed-review-attention" {
                // Complete semantic source response/storage assertions above
                // remain strict; the DTO defines native JSON spelling/order.
                assert_eq!(
                    wire,
                    native_wire(&wire, r["path"].as_str().ok_or("path")?)?,
                    "native typed bytes {i}"
                );
            } else {
                assert_eq!(wire, expected, "fixed model bytes {i}");
            }
        }
    }
    state.pool.close().await;
    Ok(())
}
