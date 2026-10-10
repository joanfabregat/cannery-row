//! Full production metric HTTP observations, fixed wire bytes and raw storage.
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
    api_models::{CatalogOut, DashboardOut, MetricsPage, ViewOut},
    application_with_metric_context,
    metric_routes::MetricContext,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::fmt::Write;
use std::{collections::BTreeMap, error::Error, sync::Arc};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
fn profile() -> MetricContext {
    MetricContext {
        repository: cannery_metrics::repo::JsonContext {
            nesting_budget: 128,
        },
        config_repository: cannery_research::config_repo::JsonContext {
            encode_nesting_budget: 128,
            decode_nesting_budget: 128,
        },
        science: cannery_research::science::RenderingContext {
            nesting_budget: 128,
        },
        rendering_budget: 128,
        response: cannery_metrics::projection::Context {
            inferred_nesting_budget: 128,
        },
    }
}
fn projection(value: Value) -> Value {
    if value
        .get("error")
        .is_some_and(|e| e["code"] == "validation_failed")
    {
        let e = &value["error"];
        json!({"error":{"code":e["code"],"details":e["details"].as_array().map(|a|a.iter().map(|d|json!({"path":d["path"]})).collect::<Vec<_>>())}})
    } else {
        value
    }
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
    let response = app.clone().oneshot(request.body(Body::empty())?).await?;
    let status = response.status().as_u16();
    let allow = response
        .headers()
        .get("allow")
        .map(|v| v.to_str().map(str::to_owned))
        .transpose()?;
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let output = if status == 500 {
        assert_eq!(&bytes[..], b"Internal Server Error");
        Value::Null
    } else if bytes.is_empty() {
        Value::Null
    } else {
        let mut deserializer = serde_json::Deserializer::from_slice(&bytes);
        deserializer.disable_recursion_limit();
        serde::Deserialize::deserialize(&mut deserializer)?
    };
    let mut wire = String::new();
    for byte in &bytes {
        write!(wire, "{byte:02x}")?;
    }
    Ok((status, allow, projection(output), wire))
}
fn typed_bytes(path: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    Ok(if path.contains("/metrics/query") {
        serde_json::to_vec(&serde_json::from_slice::<MetricsPage>(bytes)?)?
    } else if path.contains("/dashboard/views/") {
        serde_json::to_vec(&serde_json::from_slice::<ViewOut>(bytes)?)?
    } else if path.contains("/dashboard") {
        serde_json::to_vec(&serde_json::from_slice::<DashboardOut>(bytes)?)?
    } else {
        serde_json::to_vec(&serde_json::from_slice::<CatalogOut>(bytes)?)?
    })
}
#[allow(
    clippy::too_many_lines,
    reason = "Keep finite native response profiles together"
)]
fn expected(recipe: &Value) -> Result<(Value, Value)> {
    let mut output = recipe["output"].clone();
    let native = &recipe["native"];
    if !native.is_null() {
        let id = recipe["id"].as_str().ok_or("id")?;
        let path = recipe["path"].as_str().ok_or("path")?;
        assert_eq!(recipe["method"], "GET");
        match native["kind"].as_str().ok_or("native kind")? {
            "missing-view-message" => {
                assert_eq!(recipe["status"], 404);
                let view = native["view"].as_str().ok_or("view")?;
                assert_eq!(
                    output["error"]["message"],
                    format!("view '{view}' not found")
                );
                output["error"]["message"] =
                    json!(format!("view {} not found", serde_json::to_string(view)?));
            }
            "nonfinite-aggregation-refusal" => {
                let metric = native["metric"].as_str().ok_or("metric")?;
                assert!(matches!(
                    metric,
                    "nan_value" | "pos_inf_value" | "neg_inf_value"
                ));
                assert_eq!(id, format!("special-view-{metric}"));
                assert_eq!(recipe["status"], 200);
                assert_eq!(output["series"][0]["points"][0]["value"], Value::Null);
                return Ok((json!(500), Value::Null));
            }
            "checked-integer" | "filter-control-refusal" => {
                let field = native["field"].as_str().ok_or("query field")?;
                assert!(matches!(
                    field,
                    "science_revision" | "before" | "limit" | "filter"
                ));
                assert!(path.contains(&format!("{field}=")));
                let location = if native["kind"] == "filter-control-refusal" {
                    assert_eq!(recipe["id"], "query-filter-7");
                    "filter/0".to_owned()
                } else {
                    format!("query/{field}")
                };
                return Ok((
                    json!(422),
                    json!({"error":{"code":"validation_failed","details":[{"path":location}]}}),
                ));
            }
            "stored-dto-refusal" => {
                assert!(
                    id.starts_with("legacy-")
                        || id.starts_with("aggregation-")
                        || id.starts_with("track-shape-")
                        || id.starts_with("empty-group-")
                        || matches!(id, "special-query-huge_count" | "special-view-huge_count")
                );
                assert_ne!(
                    native["fields"].as_array().ok_or("fields")?.as_slice(),
                    &[] as &[Value]
                );
                // An explicit corrupt-state recipe can differ only when its source
                // success cannot inhabit the actual published response DTO.
                if recipe["status"] == 200 {
                    assert!(
                        typed_bytes(path, &serde_json::to_vec(&output)?).is_err(),
                        "{id}: source response is a valid DTO; preserve its ordinary success"
                    );
                    return Ok((json!(500), Value::Null));
                }
            }
            "storage-depth-refusal" => {
                let depth = native["depth"].as_u64().ok_or("depth")?;
                assert!(matches!(depth, 126..=128));
                assert!(
                    id == format!("reachable-nested-control-{depth}")
                        || id == format!("reachable-nested-summary-{depth}")
                );
                assert!(path.contains("metric=missing_count"));
                assert!(output["context"]["controls"][0].get("nested").is_some());
                assert_eq!(recipe["status"], 200);
                return Ok((json!(500), Value::Null));
            }
            _ => return Err("unknown native recipe".into()),
        }
    }
    if let Some(warnings) = output.get_mut("warnings").and_then(Value::as_array_mut) {
        // Finite authored warning prose changes from Python repr to JSON quoting.
        for warning in warnings {
            for (prefix, label, suffix) in [
                ("metric ", "unknown", " is not registered"),
                ("metric ", "score", " is not registered"),
                ("metric ", "7", " is not registered"),
                (
                    "group_by dimension ",
                    "missing",
                    " is not registered; it is null",
                ),
                (
                    "filter on ",
                    "missing",
                    " ignored: not a registered dimension",
                ),
                ("x field ", "unknown", " is not supported; x is null"),
                ("x field ", "unknown.field", " is not supported; x is null"),
            ] {
                if *warning == json!(format!("{prefix}'{label}'{suffix}")) {
                    *warning = json!(format!("{prefix}{}{suffix}", serde_json::to_string(label)?));
                }
            }
        }
    }
    Ok((recipe["status"].clone(), output))
}
async fn storage(pool: &PgPool) -> Result<Value> {
    let mut result = json!({});
    for (table, order) in [
        ("config_revisions", "project_id,kind,revision"),
        ("tracks", "project_id,slug"),
        ("units", "project_id,number"),
        ("unit_revisions", "unit_id,revision"),
        ("attempts", "id"),
        ("phase_outputs", "id"),
        ("measurements", "id"),
        ("comparisons", "id"),
        ("audit_events", "seq"),
    ] {
        let rows: Vec<String> = sqlx::query_scalar(&format!(
            "SELECT row_to_json(t)::text FROM {table} t ORDER BY {order}"
        ))
        .fetch_all(pool)
        .await?;
        result[table] = json!(rows);
    }
    Ok(result)
}
#[tokio::test]
#[ignore = "requires guarded uniquely owned migrated PostgreSQL"]
async fn metrics_match_production() -> Result<()> {
    let url = std::env::var("CANNERY_METRIC_HTTP_DATABASE_URL")?;
    let settings = load_settings(
        None,
        &BTreeMap::from([("CANNERY_DATABASE_URL".into(), url)]),
    )?;
    let (app, state) = application_with_metric_context(settings, Arc::new(profile()))?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("owned child required")?;
    if suffix.len() != 24 || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("owned child required".into());
    }
    sqlx::raw_sql(include_str!("fixtures/metrics_http/seed.sql"))
        .execute(&state.pool)
        .await?;
    let text =
        runtime_reference!("/../../crates/server/tests/fixtures/metrics_http/reference.json");
    let mut deserializer = serde_json::Deserializer::from_str(text);
    deserializer.disable_recursion_limit();
    let fixture: Value = serde::Deserialize::deserialize(&mut deserializer)?;
    let mut failures = Vec::new();
    for (index, recipe) in fixture["cases"]
        .as_array()
        .ok_or("cases")?
        .iter()
        .enumerate()
    {
        if let Some(setup) = recipe["setup"].as_str() {
            sqlx::raw_sql(setup).execute(&state.pool).await?;
        }
        let before = storage(&state.pool).await?;
        let (expected_status, expected_output) = expected(recipe)?;
        let (status, allow, output, wire) = call(&app, recipe).await?;
        if recipe["native"]["kind"] == "missing-view-message" {
            let typed: cannery_server::api_models::ErrorResponse =
                serde_json::from_value(expected_output.clone())?;
            let bytes = serde_json::to_vec(&typed)?;
            let mut expected_wire = String::new();
            for byte in bytes {
                write!(expected_wire, "{byte:02x}")?;
            }
            assert_eq!(
                wire, expected_wire,
                "case{index} exact native missing-view error"
            );
        }
        for (kind, actual, expected) in [
            ("status", json!(status), expected_status),
            ("allow", json!(allow), recipe["allow"].clone()),
            ("output", output, expected_output),
        ] {
            if actual != expected {
                failures.push(json!({"index":index,"id":recipe["id"],"kind":kind,"actual":actual,"expected":expected}));
            }
        }
        if status == 200 && recipe["wire_hex"].is_string() {
            let bytes = wire
                .as_bytes()
                .chunks(2)
                .map(|part| Ok(u8::from_str_radix(std::str::from_utf8(part)?, 16)?))
                .collect::<Result<Vec<_>>>()?;
            assert_eq!(
                typed_bytes(recipe["path"].as_str().ok_or("path")?, &bytes)?,
                bytes,
                "case{index} actual handler DTO serialization"
            );
        } else if status == 500 {
            assert_eq!(wire, "496e7465726e616c20536572766572204572726f72");
        } else if let Some(expected) = recipe["wire_hex"].as_str()
            && wire != expected
        {
            failures.push(json!({"index":index,"id":recipe["id"],"kind":"wire","actual":wire,"expected":expected}));
        }
        assert_eq!(
            storage(&state.pool).await?,
            before,
            "case{index} GET modified raw storage"
        );
        let snapshot = recipe["snapshot"].as_str().ok_or("snapshot key")?;
        if storage(&state.pool).await? != fixture["snapshots"][snapshot] {
            failures.push(json!({"index":index,"id":recipe["id"],"kind":"storage"}));
        }
    }
    state.pool.close().await;
    std::fs::write(
        "target/metrics-http-failures.json",
        serde_json::to_vec_pretty(&failures)?,
    )?;
    for failure in &failures {
        eprintln!("{} {} {}", failure["index"], failure["id"], failure["kind"]);
    }
    assert!(
        failures.is_empty(),
        "{} differences; see target/metrics-http-failures.json",
        failures.len()
    );
    Ok(())
}
