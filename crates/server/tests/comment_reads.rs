//! Independent comment read HTTP, fixed response bytes and complete raw storage.
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
use cannery_server::{application_with_comment_context, comment_routes::CommentContext};
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
fn native_integer_refusal(recipe: &Value) -> Result<Option<Value>> {
    let id = recipe["id"].as_str().ok_or("recipe id")?;
    let path = match id {
        "comment-read-161" | "comment-read-162" | "comment-read-163" | "comment-read-189"
        | "comment-read-190" | "comment-read-191" | "comment-read-217" | "comment-read-218"
        | "comment-read-219" => {
            assert_eq!(recipe["status"], 200);
            assert!(
                recipe["path"]
                    .as_str()
                    .ok_or("path")?
                    .ends_with("?limit=1.0")
                    || recipe["path"]
                        .as_str()
                        .ok_or("path")?
                        .ends_with("?limit=+1+")
                    || recipe["path"]
                        .as_str()
                        .ok_or("path")?
                        .ends_with("?limit=1_0")
            );
            "query/limit"
        }
        "comment-read-237" | "comment-read-238" | "comment-read-239" | "comment-read-240" => {
            assert!(recipe["status"] == 404 || recipe["status"] == 422);
            "path/number"
        }
        "comment-read-253" | "comment-read-254" => {
            assert!(recipe["status"] == 404 || recipe["status"] == 422);
            "path/sequence"
        }
        _ => return Ok(None),
    };
    let mut details =
        vec![json!({"path":path,"message":"Input should be a signed 64-bit integer"})];
    if matches!(
        id,
        "comment-read-237" | "comment-read-239" | "comment-read-253"
    ) {
        assert!(
            recipe["path"]
                .as_str()
                .ok_or("path")?
                .ends_with("?before=bad&limit=bad")
        );
        details.push(json!({"path":"query/before","message":"Input should be a valid UUID"}));
        details.push(
            json!({"path":"query/limit","message":"Input should be a signed 64-bit integer"}),
        );
    }
    Ok(Some(
        json!({"error":{"code":"validation_failed","message":"request validation failed",
        "details":details}}),
    ))
}
async fn raw_storage(pool: &sqlx::PgPool) -> Result<BTreeMap<String, Vec<String>>> {
    let mut result = BTreeMap::new();
    for table in [
        "comments",
        "comment_revisions",
        "hypotheses",
        "hypothesis_revisions",
        "attempts",
        "mentions",
        "audit_events",
        "search_documents",
    ] {
        let sql =
            format!("SELECT row_to_json(t)::text FROM {table} t ORDER BY row_to_json(t)::text");
        result.insert(
            table.to_owned(),
            sqlx::query_scalar(&sql).fetch_all(pool).await?,
        );
    }
    Ok(result)
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
    if let Some(expected) = native_integer_refusal(recipe)? {
        assert_eq!(status, 422);
        assert_eq!(allow, None);
        assert_eq!(output, expected, "complete native integer refusal");
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
    reason = "sequential read corpus verifies native DTO refusal and complete unchanged storage"
)]
async fn comment_reads_match_production() -> Result<()> {
    let url = std::env::var("CANNERY_COMMENT_READS_DATABASE_URL")?;
    let settings = load_settings(
        None,
        &BTreeMap::from([("CANNERY_DATABASE_URL".into(), url)]),
    )?;
    let (app, state) = application_with_comment_context(
        settings,
        Arc::new(CommentContext {
            hypotheses: cannery_hypotheses::repo::JsonContext {
                encode_nesting_budget: 80,
                decode_nesting_budget: 80,
            },
            mutations: None,
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
    sqlx::raw_sql(include_str!("fixtures/comment_reads/seed.sql"))
        .execute(&state.pool)
        .await?;
    let fixture: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/comment_reads/reference.json"
    ))?;
    let cases = fixture["cases"].as_array().ok_or("cases")?;
    if cases.len() != 278 || fixture["python"] != "3.13.11" {
        return Err("complete278 pinned source cases required".into());
    }
    let mut failures = Vec::new();
    assert_eq!(
        cases
            .iter()
            .map(native_integer_refusal)
            .collect::<Result<Vec<_>>>()?
            .iter()
            .filter(|v| v.is_some())
            .count(),
        15
    );
    for (index, recipe) in cases.iter().enumerate() {
        let before = if native_integer_refusal(recipe)?.is_some() {
            Some(raw_storage(&state.pool).await?)
        } else {
            None
        };
        let (status, allow, output, wire) = call(&app, recipe).await?;
        if let Some(before) = before {
            assert_eq!(
                raw_storage(&state.pool).await?,
                before,
                "native refusal changed storage"
            );
            continue;
        }
        for (kind, actual, expected) in [
            ("status", json!(status), recipe["status"].clone()),
            ("allow", json!(allow), recipe["allow"].clone()),
            ("output", output, recipe["output"].clone()),
        ] {
            if actual != expected {
                failures.push(json!({"index":index,"id":recipe["id"],"kind":kind,"actual":actual,"expected":expected}));
            }
        }
        if let Some(expected) = recipe["wire_hex"].as_str()
            && wire != expected
        {
            failures.push(json!({"index":index,"id":recipe["id"],"kind":"wire","actual":wire,"expected":expected}));
        }
    }
    let tables = [
        "comments",
        "comment_revisions",
        "hypotheses",
        "hypothesis_revisions",
        "attempts",
        "mentions",
        "audit_events",
        "search_documents",
    ];
    assert_eq!(fixture["tables"], json!(tables));
    for table in tables {
        let sql =
            format!("SELECT row_to_json(t)::text FROM {table} t ORDER BY row_to_json(t)::text");
        let storage: Vec<String> = sqlx::query_scalar(&sql).fetch_all(&state.pool).await?;
        if json!(storage) != fixture["storage"][table] {
            failures.push(json!({"kind":"storage","table":table}));
        }
    }
    state.pool.close().await;
    std::fs::write(
        "target/comment-reads-failures.json",
        serde_json::to_vec_pretty(&failures)?,
    )?;
    for failure in &failures {
        eprintln!("{} {} {}", failure["index"], failure["id"], failure["kind"]);
    }
    assert!(
        failures.is_empty(),
        "{} differences; see target/comment-reads-failures.json",
        failures.len()
    );
    Ok(())
}
