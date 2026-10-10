//! Full source comment mutation corpus: raw storage, fixed wire and actual row-lock waiters.
#![forbid(unsafe_code)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use cannery_core::{json as native_json, settings::load_settings};
use cannery_server::{
    application_with_comment_context, comment_mutations::CommentMutationContext,
    comment_routes::CommentContext,
};
use chrono::{DateTime, Datelike, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::fmt::Write;
use std::{collections::BTreeMap, error::Error, sync::Arc};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
const CLOCKS: [&str; 4] = ["created_at", "edited_at", "updated_at", "occurred_at"];
const TABLES: [(&str, &str); 8] = [
    ("comments", "id"),
    ("comment_revisions", "comment_id,revision"),
    ("mentions", "source_type,source_id,target_id"),
    ("audit_events", "seq"),
    ("search_documents", "kind,id"),
    ("units", "id"),
    ("unit_revisions", "unit_id,revision"),
    ("attempts", "id"),
];
fn clock(value: &mut Value, lower: Option<DateTime<Utc>>, upper: DateTime<Utc>) -> Result<()> {
    if let Some(s) = value.as_str()
        && let Ok(t) = DateTime::parse_from_rfc3339(s)
        && t.year() >= 2026
    {
        if let Some(lower) = lower
            && (t < lower || t > upper)
        {
            return Err("unchecked storage clock".into());
        }
        *value = json!("@checked-clock");
    }
    Ok(())
}
fn projection(mut value: Value) -> Result<Value> {
    if let Some(error) = value.get("error") {
        return Ok(
            json!({"error":{"code":error["code"],"details":error["details"].as_array().map(|items|items.iter().map(|detail|json!({"path":detail["path"]})).collect::<Vec<_>>())}}),
        );
    }
    if let Some(object) = value.as_object_mut() {
        for field in CLOCKS {
            if let Some(v) = object.get_mut(field) {
                clock(v, None, Utc::now())?;
            }
        }
    }
    Ok(value)
}
async fn storage(pool: &PgPool, lower: DateTime<Utc>) -> Result<Value> {
    let mut result = serde_json::Map::new();
    for (table, order) in TABLES {
        let query = format!(
            "SELECT COALESCE(jsonb_agg(to_jsonb(t) ORDER BY {order}),'[]'::jsonb)::text FROM {table} t"
        );
        let raw: String = sqlx::query_scalar(&query).fetch_one(pool).await?;
        let mut records: Value = serde_json::from_str(&raw)?;
        for record in records.as_array_mut().ok_or("storage array")? {
            for field in CLOCKS {
                if let Some(value) = record.get_mut(field) {
                    clock(value, Some(lower), Utc::now())?;
                }
            }
        }
        result.insert(table.into(), records);
    }
    let raw: Vec<(i64, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT seq,prior_state::text,new_state::text FROM audit_events ORDER BY seq",
    )
    .fetch_all(pool)
    .await?;
    result.insert("audit_jsonb_text".into(), json!(raw));
    let fixed_times: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT id::text,created_at::text,edited_at::text FROM comments WHERE body_markdown='fixed serialization 日本語 é😀' ORDER BY id",
    ).fetch_all(pool).await?;
    result.insert("fixed_timestamp_text".into(), json!(fixed_times));
    let relations:Vec<(i64,bool,Option<bool>)>=sqlx::query_as("SELECT a.seq,r.created_at=a.occurred_at, CASE WHEN a.action='comment.created' THEN c.created_at=a.occurred_at OR c.body_markdown='fixed serialization 日本語 é😀' ELSE c.revision<>(a.new_state->>'revision')::int OR c.edited_at=a.occurred_at END FROM audit_events a JOIN comments c ON c.id::text=a.subject_id JOIN comment_revisions r ON r.comment_id=c.id AND r.revision=(a.new_state->>'revision')::int WHERE a.action IN ('comment.created','comment.edited') ORDER BY a.seq").fetch_all(pool).await?;
    if relations.iter().any(|(_, a, b)| !*a || *b != Some(true)) {
        return Err("comment transaction clock relation".into());
    }
    result.insert("clock_relations".into(), json!(relations));
    Ok(Value::Object(result))
}
async fn raw_storage(pool: &PgPool) -> Result<Vec<String>> {
    let mut rows = Vec::new();
    for (table, order) in TABLES {
        rows.push(sqlx::query_scalar::<_, String>(&format!(
            "SELECT COALESCE(jsonb_agg(to_jsonb(t) ORDER BY {order}),'[]'::jsonb)::text FROM {table} t"
        )).fetch_one(pool).await?);
    }
    Ok(rows)
}
fn native_error(recipe: &Value, bytes: &[u8]) -> Result<Option<Value>> {
    let profile = recipe["native_body_profile"].as_str();
    if recipe["id"] == "invalid-utf8"
        || matches!(profile, Some("unpaired-surrogate" | "nonfinite-json"))
    {
        if recipe["id"] == "invalid-utf8" {
            assert_eq!(recipe["status"], 400);
            assert!(std::str::from_utf8(bytes).is_err());
        } else {
            assert!(match profile {
                Some("unpaired-surrogate") => bytes
                    .windows(6)
                    .any(|v| matches!(v, br"\ud800" | br"\udfff")),
                Some("nonfinite-json") => bytes.windows(3).any(|v| v == b"NaN"),
                _ => false,
            });
        }
        let Err(native_json::DecodeError::Syntax { position }) =
            native_json::decode(bytes, cannery_server::body::REST_JSON_NESTING_BUDGET)
        else {
            return Err("declared invalid native JSON must fail syntax validation".into());
        };
        return Ok(Some(
            json!({"error":{"code":"validation_failed","message":"request validation failed",
            "details":[{"path":format!("body/{position}"),"message":"JSON decode error"}]}}),
        ));
    }
    if let Some(parameter) = recipe["native_integer_path"].as_str() {
        assert!(matches!(parameter, "number" | "sequence"));
        let path = recipe["path"].as_str().ok_or("path")?;
        let prefix = if parameter == "number" {
            "/units/"
        } else {
            "/attempts/"
        };
        let token = path
            .split_once(prefix)
            .ok_or("integer path")?
            .1
            .split('/')
            .next()
            .ok_or("token")?;
        assert!(token.parse::<i64>().is_err());
    }
    match profile {
        Some("create-shape") => {
            assert_eq!(recipe["method"], "POST");
            assert!(
                serde_json::from_slice::<cannery_server::api_models::CommentCreate>(bytes).is_err()
            );
        }
        Some("edit-shape") => {
            assert_eq!(recipe["method"], "PUT");
            assert!(
                serde_json::from_slice::<cannery_server::api_models::CommentEdit>(bytes).is_err()
            );
        }
        Some(_) => return Err("unknown native body profile".into()),
        None => {}
    }
    if profile.is_some() {
        return Ok(Some(json!({"error":{"code":"validation_failed",
            "message":"request does not match the REST contract","details":null}})));
    }
    if let Some(parameter) = recipe["native_integer_path"].as_str() {
        assert!(
            serde_json::from_slice::<cannery_server::api_models::CommentCreate>(bytes).is_ok(),
            "a valid DTO reaches transport parameter validation"
        );
        return Ok(Some(
            json!({"error":{"code":"validation_failed","message":"request validation failed",
            "details":[{"path":format!("path/{parameter}"),"message":"Input should be a signed 64-bit integer"}]}}),
        ));
    }
    Ok(None)
}
#[allow(
    clippy::too_many_lines,
    reason = "Independently enumerate the exact native DTO refusal corpus"
)]
fn assert_native_profiles(cases: &[Value]) -> Result<()> {
    let expected: [(&str, &[&str]); 4] = [
        (
            "create-shape",
            &[
                "role-3",
                "role-4",
                "role-6",
                "role-7",
                "role-9",
                "role-10",
                "role-12",
                "role-13",
                "role-15",
                "role-16",
                "role-18",
                "role-19",
                "role-21",
                "role-22",
                "role-24",
                "role-25",
                "role-27",
                "role-28",
                "role-39",
                "role-40",
                "role-42",
                "role-43",
                "role-45",
                "role-46",
                "role-48",
                "role-49",
                "role-51",
                "role-52",
                "role-54",
                "role-55",
                "role-57",
                "role-58",
                "role-60",
                "role-61",
                "role-63",
                "role-64",
                "body-108",
                "body-109",
                "body-110",
                "body-111",
                "body-112",
                "raw-158",
                "raw-159",
                "raw-160",
                "raw-161",
                "raw-162",
                "raw-164",
                "raw-165",
                "number-167",
                "number-169",
                "number-171",
                "number-173",
                "number-175",
                "number-177",
                "number-179",
                "number-181",
                "number-183",
                "all-path-body-errors",
            ],
        ),
        (
            "edit-shape",
            &[
                "role-75",
                "role-76",
                "role-78",
                "role-79",
                "role-81",
                "role-82",
                "role-84",
                "role-85",
                "role-87",
                "role-88",
                "role-90",
                "role-91",
                "role-93",
                "role-94",
                "role-96",
                "role-97",
                "role-99",
                "role-100",
                "revision-193",
                "revision-194",
                "revision-195",
                "revision-199",
                "revision-200",
                "revision-201",
                "revision-202",
                "revision-203",
                "revision-204",
                "revision-205",
                "revision-206",
                "revision-207",
                "revision-208",
                "revision-209",
                "revision-210",
                "revision-211",
                "edit-errors-212",
                "edit-errors-213",
            ],
        ),
        (
            "unpaired-surrogate",
            &["body-126", "body-127", "edit-errors-214"],
        ),
        ("nonfinite-json", &["raw-166"]),
    ];
    for (profile, ids) in expected {
        let actual = cases
            .iter()
            .filter(|r| r["native_body_profile"] == profile)
            .map(|r| r["id"].as_str().ok_or_else(|| "recipe id".into()))
            .collect::<Result<Vec<_>>>()?;
        assert_eq!(actual, ids, "exact declared {profile} recipes");
    }
    let paths = cases
        .iter()
        .filter(|r| r["native_integer_path"].is_string())
        .map(|r| r["id"].as_str().ok_or_else(|| "recipe id".into()))
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(
        paths,
        [
            "number-179",
            "number-180",
            "number-181",
            "number-182",
            "number-183",
            "number-184",
            "sequence-190",
            "sequence-191",
            "sequence-192"
        ]
    );
    Ok(())
}
#[allow(
    clippy::too_many_lines,
    reason = "Check the full request, response and clock contract together"
)]
async fn call(app: &Router, recipe: &Value) -> Result<(u16, Option<String>, Value, String)> {
    let role = recipe["role"].as_str().ok_or("role")?;
    let mut builder = Request::builder()
        .method(recipe["method"].as_str().ok_or("method")?)
        .uri(recipe["path"].as_str().ok_or("path")?)
        .header(
            "content-type",
            recipe["content_type"]
                .as_str()
                .unwrap_or("application/json"),
        );
    if role.starts_with("browser") {
        builder = builder.header("cookie", "cr_session=cr_ses_comment_fixture");
        if role == "browser" {
            builder = builder.header("x-csrf-token", "comment_fixture_csrf");
        } else if role == "browser-bad-csrf" {
            builder = builder.header("x-csrf-token", "wrong");
        }
    } else if role != "none" {
        builder = builder.header(
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
    let raw = recipe["raw_hex"].as_str().ok_or("raw")?;
    let bytes = (0..raw.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&raw[at..at + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let native_error = native_error(recipe, &bytes)?;
    let before = Utc::now();
    let expected_revision =
        cannery_core::json::decode(&bytes, cannery_server::body::REST_JSON_NESTING_BUDGET)
            .ok()
            .and_then(|d| {
                d.field(d.root(), "expected_revision")
                    .and_then(|id| d.node(id))
                    .and_then(|node| {
                        cannery_server::validation::validate_parameter_node(
                            node,
                            cannery_server::validation::Parameter::ConfigRevision,
                        )
                        .ok()
                    })
            });
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(bytes))?)
        .await?;
    let status = response.status().as_u16();
    let allow = response
        .headers()
        .get("allow")
        .map(|h| h.to_str().map(str::to_owned))
        .transpose()?;
    let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024).await?;
    let after = Utc::now();
    let output = if status == 500 {
        if bytes.as_ref() != b"Internal Server Error" {
            return Err("plain500 required".into());
        }
        Value::Null
    } else if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)?
    };
    if let Some(expected) = native_error {
        assert_eq!(status, 422, "native refusal {}", recipe["id"]);
        assert_eq!(
            output, expected,
            "complete native envelope {}",
            recipe["id"]
        );
        assert_eq!(allow, None);
    }
    if status == 201 && recipe["id"] != "fixed-wire" {
        let created =
            DateTime::parse_from_rfc3339(output["created_at"].as_str().ok_or("creation clock")?)?;
        if created < before || created > after {
            return Err("comment creation bounds".into());
        }
    }
    if status == 200 && recipe["method"] == "PUT" {
        let revision = output["revision"].as_i64().ok_or("comment revision")?;
        if let Some(cannery_server::validation::ParameterValue::ConfigRevision(expected)) =
            expected_revision
            && num_bigint::BigInt::from(revision) != expected
        {
            let edited =
                DateTime::parse_from_rfc3339(output["edited_at"].as_str().ok_or("edit clock")?)?;
            if edited < before || edited > after {
                return Err("comment edit bounds".into());
            }
        }
    }
    let mut wire = String::new();
    for byte in bytes {
        write!(wire, "{byte:02x}")?;
    }
    Ok((status, allow, projection(output)?, wire))
}
#[tokio::test]
#[ignore = "requires guarded owned migrated PostgreSQL child"]
#[allow(
    clippy::too_many_lines,
    reason = "Complete replay and held-lock proof share one owned database"
)]
async fn comment_mutations_match_production() -> Result<()> {
    let url = std::env::var("CANNERY_COMMENT_MUTATION_DATABASE_URL")?;
    let settings = load_settings(
        None,
        &BTreeMap::from([("CANNERY_DATABASE_URL".into(), url)]),
    )?;
    let (app, state) = application_with_comment_context(
        settings,
        Arc::new(CommentContext {
            units: cannery_units::repo::JsonContext {
                encode_nesting_budget: 80,
                decode_nesting_budget: 80,
            },
            mutations: Some(Arc::new(CommentMutationContext {
                mention_walk_budget: 80,
            })),
        }),
    )?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("owned database required")?;
    if suffix.len() != 24
        || !suffix
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err("owned database required".into());
    }
    let lower = Utc::now();
    for seed in [
        include_str!("fixtures/comment_reads/seed.sql"),
        include_str!("fixtures/comment_mutations/seed.sql"),
    ] {
        sqlx::raw_sql(seed).execute(&state.pool).await?;
    }
    let fixture: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/comment_mutations/reference.json"
    ))?;
    let cases = fixture["cases"].as_array().ok_or("cases")?;
    assert_eq!(cases.len(), 262);
    assert_native_profiles(cases)?;
    let mut failures = Vec::new();
    for (index, recipe) in cases.iter().enumerate() {
        sqlx::query("UPDATE fixture_comment_failure SET action=$1")
            .bind(recipe["fail"].as_str())
            .execute(&state.pool)
            .await?;
        let before = storage(&state.pool, lower).await?;
        let native_profile = recipe["native_body_profile"].is_string()
            || recipe["native_integer_path"].is_string()
            || recipe["id"] == "invalid-utf8";
        let raw_before = if native_profile {
            Some(raw_storage(&state.pool).await?)
        } else {
            None
        };
        if recipe["race"] == true {
            let mut controller = state.pool.begin().await?;
            sqlx::query("SELECT id FROM comments WHERE id='00000000-0000-0000-0000-000000040001' FOR UPDATE").fetch_all(&mut *controller).await?;
            let pending = (0..2)
                .map(|_| {
                    let app = app.clone();
                    let recipe = recipe.clone();
                    tokio::spawn(async move { call(&app, &recipe).await })
                })
                .collect::<Vec<_>>();
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                if pending.iter().any(tokio::task::JoinHandle::is_finished) {
                    return Err("race completed before lock proof".into());
                }
                let waiters:i64=tokio::time::timeout_at(deadline,sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock'").fetch_one(&state.pool)).await??;
                if waiters == 2 {
                    assert_eq!(recipe["observed_lock_waiters"], 2);
                    println!("comment race observed two PostgreSQL lock waiters");
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err("race lock proof timeout".into());
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            controller.commit().await?;
            let mut responses = Vec::new();
            for task in pending {
                responses.push(task.await??);
            }
            let mut statuses = responses.iter().map(|v| v.0).collect::<Vec<_>>();
            statuses.sort_unstable();
            let mut outputs = responses.into_iter().map(|v| v.2).collect::<Vec<_>>();
            // Source race collection sorts successful output before the stale error.
            outputs.sort_by_key(|value| value.get("error").is_some());
            if json!(statuses) != recipe["statuses"] || json!(outputs) != recipe["outputs"] {
                failures.push(
                    json!({"index":index,"kind":"race","statuses":statuses,"outputs":outputs}),
                );
            }
        } else {
            let (status, allow, output, wire) = call(&app, recipe).await?;
            if let Some(raw_before) = raw_before {
                assert_eq!(status, 422);
                assert_eq!(
                    raw_storage(&state.pool).await?,
                    raw_before,
                    "native refusal must preserve exact rows"
                );
                let actual = storage(&state.pool, lower).await?;
                assert_eq!(
                    actual, before,
                    "native refusal must preserve clocks and relations"
                );
                assert_eq!(
                    actual, recipe["storage"],
                    "complete source refusal storage remains equivalent"
                );
                continue;
            }
            for (kind, actual, expected) in [
                ("status", json!(status), recipe["status"].clone()),
                ("allow", json!(allow), recipe["allow"].clone()),
                ("output", output.clone(), recipe["output"].clone()),
            ] {
                if actual != expected {
                    failures.push(json!({"index":index,"id":recipe["id"],"kind":kind,"actual":actual,"expected":expected}));
                }
            }
            if let Some(expected) = recipe["wire_hex"].as_str()
                && wire != expected
            {
                failures.push(json!({"index":index,"kind":"wire"}));
            }
            if status >= 400 && before != storage(&state.pool, lower).await? {
                return Err("refused mutation changed storage".into());
            }
        }
        let actual = storage(&state.pool, lower).await?;
        if actual != recipe["storage"] {
            failures.push(json!({"index":index,"id":recipe["id"],"kind":"storage","actual":actual,"expected":recipe["storage"]}));
        }
    }
    state.pool.close().await;
    std::fs::write(
        "target/comment-mutations-failures.json",
        serde_json::to_vec_pretty(&failures)?,
    )?;
    for f in &failures {
        eprintln!("{} {} {}", f["index"], f["id"], f["kind"]);
    }
    assert!(
        failures.is_empty(),
        "{} mismatches; target/comment-mutations-failures.json",
        failures.len()
    );
    println!(
        "{} actual comment mutation observations matched",
        cases.len()
    );
    Ok(())
}
