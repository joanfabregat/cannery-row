//! Independent full search HTTP, cursors, fixed response bytes and raw storage.
#![forbid(unsafe_code)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use cannery_core::settings::load_settings;
use cannery_server::{application_with_search_context, search_routes::SearchContext};
use serde_json::{Value, json};
use std::fmt::Write;
use std::{collections::BTreeMap, error::Error, sync::Arc};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
fn projection(value: Value) -> Value {
    if value
        .get("error")
        .is_some_and(|error| error["code"] == "validation_failed")
    {
        let error = &value["error"];
        json!({"error":{"code":error["code"],"details":error["details"].as_array().map(|items|items.iter().map(|detail|json!({"path":detail["path"]})).collect::<Vec<_>>())}})
    } else {
        value
    }
}
fn native_expected_output(source: &Value) -> Result<Value> {
    let mut expected = source.clone();
    if let Some(cursor) = source["next_before"].as_str() {
        let decoded = URL_SAFE_NO_PAD.decode(cursor)?;
        let fields: Vec<Value> = serde_json::from_slice(&decoded)?;
        assert_eq!(fields.len(), 2);
        if fields[0].as_f64() == Some(0.0) {
            // Cursor scores use native number formatting. The decoded score
            // and row identity remain exact, and pagination cases consume it.
            assert!(decoded.starts_with(b"[0.0, "));
            let id = fields[1].as_i64().ok_or("cursor row identity")?;
            expected["next_before"] = json!(URL_SAFE_NO_PAD.encode(format!("[0, {id}]")));
        }
    }
    Ok(expected)
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
        .map(|value| value.to_str().map(str::to_owned))
        .transpose()?;
    let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024).await?;
    let output = if status == 500 {
        if bytes.as_ref() != b"Internal Server Error" {
            return Err("source plain internal failure required".into());
        }
        Value::Null
    } else if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)?
    };
    let expected = match recipe["native_profile"].as_str() {
        Some("strict-limit") => {
            let path = recipe["path"].as_str().ok_or("path")?;
            assert!(matches!(
                path,
                "/api/search?limit=1.0" | "/api/search?limit=+1+" | "/api/search?limit=1_0"
            ));
            Some(
                json!({"error":{"code":"validation_failed","message":"request validation failed",
                "details":[{"path":"query/limit","message":"Input should be a signed 64-bit integer"}]}}),
            )
        }
        Some("bom-cursor") => {
            assert_eq!(recipe["path"], "/api/search?before=77u_WzAsMV0");
            Some(
                json!({"error":{"code":"validation_failed","message":"invalid cursor",
                "details":[{"path":"before","message":"not a search cursor"}]}}),
            )
        }
        None => None,
        Some(_) => return Err("unknown native search profile".into()),
    };
    if let Some(expected) = expected {
        assert_eq!(status, 422);
        assert_eq!(allow, None);
        assert_eq!(output, expected, "complete native search envelope");
    }
    let mut wire = String::new();
    for byte in &bytes {
        write!(wire, "{byte:02x}")?;
    }
    Ok((status, allow, projection(output), wire))
}
#[tokio::test]
#[ignore = "requires guarded uniquely owned migrated PostgreSQL"]
#[allow(
    clippy::too_many_lines,
    reason = "sequential search corpus checks preserve authorization and exact native wire assertions"
)]
async fn search_matches_production() -> Result<()> {
    let url = std::env::var("CANNERY_SEARCH_HTTP_DATABASE_URL")?;
    let settings = load_settings(
        None,
        &BTreeMap::from([("CANNERY_DATABASE_URL".into(), url)]),
    )?;
    let (app, state) = application_with_search_context(
        settings,
        Arc::new(SearchContext {
            // A length-200 cursor decodes to at most150 bytes, so this explicit
            // fixture decoder profile covers every possible public cursor shape.
            cursor_decode_budget: 200,
        }),
    )?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("owned child required")?;
    if suffix.len() != 24
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err("owned child required".into());
    }
    sqlx::raw_sql(include_str!("fixtures/search_http/seed.sql"))
        .execute(&state.pool)
        .await?;
    let fixture: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/search_http/reference.json"
    ))?;
    let cases = fixture["cases"].as_array().ok_or("cases")?;
    if cases.len() != 258 || fixture["python"] != "3.13.11" {
        return Err("complete258 pinned source cases required".into());
    }
    let mut failures = Vec::new();
    assert_eq!(
        cases
            .iter()
            .filter(|r| r["native_profile"].is_string())
            .count(),
        4,
        "three unsupported limit spellings and one BOM cursor"
    );
    for (index, recipe) in cases.iter().enumerate() {
        let before: Option<Vec<String>> = if recipe["native_profile"].is_string() {
            Some(
                sqlx::query_scalar(
                    "SELECT row_to_json(t)::text FROM search_documents t ORDER BY id",
                )
                .fetch_all(&state.pool)
                .await?,
            )
        } else {
            None
        };
        let (status, allow, output, wire) = call(&app, recipe).await?;
        if let Some(before) = before {
            let after: Vec<String> = sqlx::query_scalar(
                "SELECT row_to_json(t)::text FROM search_documents t ORDER BY id",
            )
            .fetch_all(&state.pool)
            .await?;
            assert_eq!(status, 422);
            assert_eq!(before, after, "native search refusal changed raw storage");
            assert_eq!(
                json!(after),
                fixture["storage"],
                "all original search rows preserved"
            );
            continue;
        }
        for (kind, actual, expected) in [
            ("status", json!(status), recipe["status"].clone()),
            ("allow", json!(allow), recipe["allow"].clone()),
            (
                "output",
                output.clone(),
                native_expected_output(&recipe["output"])?,
            ),
        ] {
            if actual != expected {
                failures.push(json!({"index":index,"id":recipe["id"],"kind":kind,"actual":actual,"expected":expected}));
            }
        }
        if let Some(source_wire) = recipe["wire_hex"].as_str() {
            let expected = if status == 200 {
                let model: cannery_server::api_models::SearchPage =
                    serde_json::from_value(native_expected_output(&recipe["output"])?)?;
                let mut expected = String::new();
                for byte in serde_json::to_vec(&model)? {
                    write!(&mut expected, "{byte:02x}")?;
                }
                expected
            } else {
                source_wire.to_owned()
            };
            if wire != expected {
                failures.push(json!({"index":index,"id":recipe["id"],"kind":"wire","actual":wire,"expected":expected}));
            }
        }
    }
    let storage: Vec<String> =
        sqlx::query_scalar("SELECT row_to_json(t)::text FROM search_documents t ORDER BY id")
            .fetch_all(&state.pool)
            .await?;
    if json!(storage) != fixture["storage"] {
        failures.push(json!({"kind":"storage"}));
    }
    state.pool.close().await;
    std::fs::write(
        "target/search-http-failures.json",
        serde_json::to_vec_pretty(&failures)?,
    )?;
    for failure in &failures {
        eprintln!("{} {} {}", failure["index"], failure["id"], failure["kind"]);
    }
    assert!(
        failures.is_empty(),
        "{} differences; see target/search-http-failures.json",
        failures.len()
    );
    Ok(())
}
