//! Five source HTTP operations with full relational/storage observations.
#![forbid(unsafe_code)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use cannery_core::{contracts::ContractValidator, json as native_json, settings::load_settings};
use cannery_server::{
    api_models::{NativeUnitDocument, RevisionOut, UnitOut},
    application_with_unit_context,
    unit_routes::UnitContext,
    unit_wire::ResponseContext,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::fmt::Write;
use std::{collections::BTreeMap, error::Error, sync::Arc};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
fn fixture() -> Result<Value> {
    Ok(serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/units_http/reference.json"
    ))?)
}
fn profile() -> Result<UnitContext> {
    Ok(UnitContext {
        mutations: None,
        contracts: ContractValidator::new()?,
        validation_walk_budget: 80,
        repr_budget: 80,

        repository: cannery_units::repo::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
        response: ResponseContext {
            inferred_nesting_budget: 80,
        },
        request_hash_budget: 80,
    })
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
    let native_error = if recipe["native_wire_profile"] == "unpaired-surrogate" {
        let bytes = body.as_deref().ok_or("surrogate body")?;
        assert!(bytes.windows(6).any(|v| v == br"\ud800"));
        let Err(native_json::DecodeError::Syntax { position }) =
            native_json::decode(bytes, cannery_server::body::REST_JSON_NESTING_BUDGET)
        else {
            return Err("surrogate profile must fail native syntax validation".into());
        };
        Some(
            json!({"error":{"code":"validation_failed","message":"request validation failed",
            "details":[{"path":format!("body/{position}"),"message":"JSON decode error"}]}}),
        )
    } else {
        assert!(recipe["native_wire_profile"].is_null());
        None
    };
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
    if let Some(expected) = native_error {
        assert_eq!(status, 422);
        assert_eq!(value, expected, "complete native syntax envelope");
    }
    if let Some(parameter) = recipe["native_integer_path"].as_str() {
        assert!(matches!(parameter, "number" | "revision"));
        let path = recipe["path"].as_str().ok_or("path")?;
        let token = path.rsplit('/').next().ok_or("integer path token")?;
        assert!(
            token.parse::<i64>().is_err(),
            "only declared non-native integer tokens"
        );
        assert_eq!(status, 422);
        assert_eq!(
            value,
            json!({"error":{"code":"validation_failed","message":"request validation failed",
            "details":[{"path":format!("path/{parameter}"),"message":"Input should be a signed 64-bit integer"}]}})
        );
    }
    if recipe["id"] == "query-before=1.0" {
        assert_eq!(recipe["status"], 200);
        assert_eq!(status, 422);
        assert_eq!(
            value,
            json!({"error":{"code":"validation_failed","message":"request validation failed",
            "details":[{"path":"query/before","message":"Input should be a signed 64-bit integer"}]}})
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
async fn storage(pool: &PgPool) -> Result<(BTreeMap<String, String>, Value)> {
    let tables = [
        ("units", "project_id,number"),
        ("unit_revisions", "unit_id,revision"),
        ("unit_relations", "unit_id,kind,target_id"),
        ("mentions", "source_type,source_id,target_id"),
        ("review_cases", "opened_at,id"),
        ("decisions", "decided_at,id"),
        ("audit_events", "seq"),
        ("idempotency_keys", "scope,actor,key"),
    ];
    let mut result = json!({});
    for (table, order) in tables {
        let rows = sqlx::query_scalar::<_, Value>(&format!(
            "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY {order}),'[]') FROM {table} t"
        ))
        .fetch_one(pool)
        .await?;
        result[table] = rows;
    }
    result["clock_relations"] = sqlx::query_scalar::<_,Value>(
        "SELECT coalesce(jsonb_agg(jsonb_build_array(a.seq,h.updated_at=a.occurred_at,\
        h.approved_at=a.occurred_at,c.resolved_at=a.occurred_at,d.decided_at=a.occurred_at) ORDER BY a.seq),'[]') \
        FROM audit_events a JOIN units h ON h.id::text=a.subject_id \
        JOIN review_cases c ON c.unit_id=h.id JOIN decisions d ON d.review_case_id=c.id \
        AND d.decided_at=a.occurred_at"
    ).fetch_one(pool).await?;
    let mut ids = BTreeMap::new();
    for (table, label) in [("review_cases", "case"), ("decisions", "decision")] {
        for (i, row) in result[table].as_array().ok_or("rows")?.iter().enumerate() {
            ids.insert(
                row["id"].as_str().ok_or("row id")?.into(),
                format!("@{label}:{i}"),
            );
        }
    }
    Ok((ids.clone(), canonical(result, &ids)))
}
async fn raw_storage(pool: &PgPool) -> Result<Vec<String>> {
    let mut rows = Vec::new();
    for table in [
        "units",
        "unit_revisions",
        "unit_relations",
        "mentions",
        "review_cases",
        "decisions",
        "audit_events",
        "idempotency_keys",
    ] {
        rows.push(sqlx::query_scalar::<_, String>(&format!(
            "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]')::text FROM {table} t"
        )).fetch_one(pool).await?);
    }
    Ok(rows)
}
async fn assert_authored_read_documents(pool: &PgPool) -> Result<()> {
    let rows: Vec<(i32, i32, String)> = sqlx::query_as(
        "SELECT h.number,r.revision,r.content::text FROM units h \
         JOIN unit_revisions r ON r.unit_id=h.id \
         JOIN projects p ON p.id=h.project_id WHERE p.slug='matrix' \
         ORDER BY h.number,r.revision",
    )
    .fetch_all(pool)
    .await?;
    assert_eq!(rows.len(), 19);
    for (number, revision, content) in rows {
        let value: Value = serde_json::from_str(&content)?;
        if (13..=15).contains(&number) {
            let corrupt = match number {
                13 => Value::Null,
                14 => json!([]),
                _ => json!(42),
            };
            assert_eq!(value, corrupt, "intentional corrupt recovery root");
            assert!(serde_json::from_value::<NativeUnitDocument>(value).is_err());
        } else if number == 11 {
            assert_eq!(
                value,
                json!({"schema_version":"0.2","track":"alpha", "title":"Unit 11","question":"Legacy authored question"})
            );
        } else {
            serde_json::from_value::<NativeUnitDocument>(value.clone())?;
            if number >= 16 {
                assert_eq!(
                    value["project_fields"]["wide"].to_string(),
                    format!("1{}", "0".repeat(500))
                );
                assert_eq!(value["project_fields"]["unicode"], "é");
                assert_eq!(value["project_fields"]["float"].as_f64(), Some(1e-7));
            } else if number == 2 && revision == 2 {
                assert_eq!(value["project_fields"], json!({"later":true}));
            } else {
                assert_eq!(value["project_fields"], json!({"raw":[1,true,null,"é"]}));
            }
        }
    }
    Ok(())
}
#[tokio::test]
#[ignore = "requires guarded uniquely owned migrated PostgreSQL"]
#[allow(
    clippy::too_many_lines,
    reason = "Keep ordered source requests and full response, storage and native boundary checks together"
)]
async fn units_match_production() -> Result<()> {
    let url = std::env::var("CANNERY_UNIT_HTTP_DATABASE_URL")?;
    let settings = load_settings(
        None,
        &BTreeMap::from([("CANNERY_DATABASE_URL".into(), url)]),
    )?;
    let (app, state) = application_with_unit_context(settings, Arc::new(profile()?))?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("owned child required")?;
    if suffix.len() != 24 || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("owned child required".into());
    }
    sqlx::raw_sql(include_str!("fixtures/units_http/seed.sql"))
        .execute(&state.pool)
        .await?;
    assert_authored_read_documents(&state.pool).await?;
    let f = fixture()?;
    assert_eq!(
        f["cases"]
            .as_array()
            .ok_or("cases")?
            .iter()
            .filter(|r| r["native_serialization_profile"] == "typed-unit")
            .count(),
        4,
        "retain all four fixed response serialization records"
    );
    assert_eq!(
        f["cases"]
            .as_array()
            .ok_or("cases")?
            .iter()
            .filter(|r| r["native_integer_path"].is_string())
            .count(),
        7,
        "retain oversized/underscore path tokens, including a warm request and revision route"
    );
    for r in f["cases"].as_array().ok_or("cases")? {
        let before = if r["native_integer_path"].is_string() || r["id"] == "query-before=1.0" {
            Some((
                storage(&state.pool).await?.1,
                raw_storage(&state.pool).await?,
            ))
        } else {
            None
        };
        let (status, allow, output, wire) = call(&app, r).await?;
        if let Some((before, raw_before)) = before {
            assert_eq!(status, 422);
            assert_eq!(allow, None);
            let (_, stored) = storage(&state.pool).await?;
            assert_eq!(stored, before, "native rejection must not write storage");
            assert_eq!(
                raw_storage(&state.pool).await?,
                raw_before,
                "all stored row values unchanged"
            );
            assert_eq!(stored, canonical(r["storage"].clone(), &BTreeMap::new()));
            continue;
        }
        assert_eq!(json!(status), r["status"], "status {}", r["id"]);
        assert_eq!(json!(allow), r["allow"], "allow {}", r["id"]);
        let (ids, stored) = storage(&state.pool).await?;
        assert_eq!(
            canonical(output, &ids),
            canonical(r["output"].clone(), &BTreeMap::new()),
            "body {}",
            r["id"]
        );
        assert_eq!(
            stored,
            canonical(r["storage"].clone(), &BTreeMap::new()),
            "storage {}",
            r["id"]
        );
        if let Some(expected) = r["wire_hex"].as_str() {
            if r["native_serialization_profile"] == "typed-unit" {
                // Source mapping order is retained in its record. Native serde
                // uses the actual declared response DTO; every field/value has
                // already compared against the complete source response above.
                let bytes = raw(&wire)?;
                let native = if r["path"].as_str().ok_or("path")?.contains("/revisions/") {
                    serde_json::to_vec(&serde_json::from_slice::<RevisionOut>(&bytes)?)?
                } else {
                    serde_json::to_vec(&serde_json::from_slice::<UnitOut>(&bytes)?)?
                };
                assert_eq!(bytes, native, "native typed model bytes {}", r["id"]);
                let text = std::str::from_utf8(&bytes)?;
                assert!(text.contains('é'));
                assert!(!text.contains("\\u00e9"));
                assert!(serde_json::from_slice::<Value>(&raw(expected)?).is_ok());
            } else {
                assert_eq!(wire, expected, "model bytes {}", r["id"]);
            }
        }
    }
    state.pool.close().await;
    Ok(())
}
