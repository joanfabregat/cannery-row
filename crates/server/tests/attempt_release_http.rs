//! Actual native release HTTP, domain storage and PostgreSQL clock relationships.
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
    attempt_read_wire::ResponseContext, attempt_release_routes::AttemptReleaseContext,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::fmt::Write;
use std::{collections::BTreeMap, error::Error, sync::Arc};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
const ATTEMPT: &str = "00000000-0000-0000-0000-000000002001";
fn output(value: Value) -> Value {
    if let Some(error) = value
        .get("error")
        .filter(|e| e["code"] == "validation_failed")
    {
        return json!({"error":{"code":error["code"],"details":error["details"].as_array().map(|values|values.iter().map(|v|json!({"path":v["path"]})).collect::<Vec<_>>())}});
    }
    value
}
fn native_refusal(path: &str, message: &str) -> Value {
    json!({"error":{"code":"validation_failed","message":"request validation failed","details":[{"path":path,"message":message}]}})
}
fn dto_refusal() -> Value {
    json!({"error":{"code":"validation_failed","message":"request does not match the REST contract","details":null}})
}
fn assert_unicode_reason_storage(actual: &Value, expected: &Value) -> Result<()> {
    // Release reasons are stored verbatim by both implementations, including
    // C0 separators. Verify those leaves and every other persisted field.
    for table in ["attempt_failures", "audit_events"] {
        let actual_rows = actual[table].as_array().ok_or("native reason rows")?;
        let expected_rows = expected[table].as_array().ok_or("source reason rows")?;
        assert_eq!(actual_rows.len(), expected_rows.len());
        let mut changed = 0;
        for (row, expected_row) in actual_rows.iter().zip(expected_rows) {
            let parsed: Value = serde_json::from_str(row.as_str().ok_or("native reason row")?)?;
            let source: Value =
                serde_json::from_str(expected_row.as_str().ok_or("source reason row")?)?;
            if parsed["reason"] == "\u{001c}ok\u{001f}" {
                assert_eq!(source["reason"], "\u{001c}ok\u{001f}");
                assert_eq!(parsed, source);
                changed += 1;
            }
        }
        assert_eq!(changed, 1, "native retained reason {table}");
    }
    assert_eq!(actual, expected, "native reason storage");
    Ok(())
}
async fn storage(pool: &PgPool, project: bool) -> Result<Value> {
    let mut result = json!({});
    for (table, order) in [
        ("tracks", "id"),
        ("units", "id"),
        ("unit_revisions", "unit_id,revision"),
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
        ("config_revisions", "project_id,kind,revision"),
    ] {
        let mut expression = "to_jsonb(t)".to_owned();
        if project {
            let fields: &[&str] = match table {
                "attempts" => &["finished_at"],
                "units" | "search_documents" => &["updated_at"],
                _ => &[],
            };
            for field in fields {
                expression = format!(
                    "jsonb_set({expression},'{{{field}}}',CASE WHEN {field} BETWEEN '2002-01-01Z' AND '2090-01-01Z' THEN '\"@checked-clock\"'::jsonb ELSE to_jsonb(t)->'{field}' END)"
                );
            }
        }
        result[table] = json!(
            sqlx::query_scalar::<_, String>(&format!(
                "SELECT ({expression})::text FROM {table} t ORDER BY {order}"
            ))
            .fetch_all(pool)
            .await?
        );
    }
    Ok(result)
}
async fn clock(pool: &PgPool) -> Result<Timestamp> {
    Ok(sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(pool)
        .await?)
}
async fn check_index_clocks(pool: &PgPool, before: Timestamp, after: Timestamp) -> Result<()> {
    let timestamps: Vec<Timestamp> = sqlx::query_scalar(
        "SELECT updated_at FROM search_documents WHERE updated_at > '2002-01-01Z'",
    )
    .fetch_all(pool)
    .await?;
    for timestamp in timestamps {
        assert!(before.0 <= timestamp.0 && timestamp.0 <= after.0);
    }
    Ok(())
}
async fn call(app: &Router, recipe: &Value) -> Result<(u16, Option<String>, Value, Vec<u8>)> {
    let mut request = Request::builder()
        .method(recipe["method"].as_str().ok_or("method")?)
        .uri(recipe["path"].as_str().ok_or("path")?);
    let role = recipe["role"].as_str().ok_or("role")?;
    if !["none", "browser"].contains(&role) {
        let prefix = if [
            "agent",
            "foreign-agent",
            "experimenter",
            "verifier",
            "second-verifier",
        ]
        .contains(&role)
        {
            "cr_svc_"
        } else {
            "cr_pat_"
        };
        request = request.header("authorization", format!("Bearer {prefix}track_http_{role}"));
    }
    for h in recipe["headers"].as_array().ok_or("headers")? {
        request = request.header(
            h[0].as_str().ok_or("header")?,
            h[1].as_str().ok_or("header")?,
        );
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(
            recipe["body"].as_str().ok_or("body")?.to_owned(),
        ))?)
        .await?;
    let status = response.status().as_u16();
    let allow = response
        .headers()
        .get("allow")
        .map(|v| v.to_str().map(str::to_owned))
        .transpose()?;
    let bytes = to_bytes(response.into_body(), usize::MAX).await?.to_vec();
    let value = if status == 500 || bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)?
    };
    Ok((status, allow, value, bytes))
}
async fn reset(pool: &PgPool, setup: &str) -> Result<()> {
    sqlx::raw_sql("DROP TRIGGER IF EXISTS fixture_release_commit ON attempts; DROP TABLE IF EXISTS fixture_lease_fault CASCADE; DROP FUNCTION IF EXISTS fixture_lease_update_fault() CASCADE; DROP FUNCTION IF EXISTS fixture_lease_commit_fault() CASCADE; TRUNCATE users,projects RESTART IDENTITY CASCADE;")
        .execute(pool)
        .await?;
    for seed in [
        include_str!("fixtures/attempt_reads_http/seed.sql"),
        include_str!("fixtures/attempt_leases_http/seed.sql"),
        include_str!("fixtures/attempt_release_http/seed.sql"),
    ] {
        sqlx::raw_sql(seed).execute(pool).await?;
    }
    if !setup.is_empty() {
        sqlx::raw_sql(setup).execute(pool).await?;
    }
    sqlx::query("UPDATE search_documents SET updated_at='2001-01-01Z'")
        .execute(pool)
        .await?;
    Ok(())
}
async fn race(app: &Router, pool: &PgPool, recipe: &Value) -> Result<()> {
    reset(pool, recipe["setup"].as_str().ok_or("race setup")?).await?;
    let before = clock(pool).await?;
    let mut holder = pool.begin().await?;
    sqlx::query("SELECT id FROM attempts WHERE id=$1::text::uuid FOR UPDATE")
        .bind(ATTEMPT)
        .execute(&mut *holder)
        .await?;
    let mut pending = Vec::new();
    for _ in 0..2 {
        let app = app.clone();
        let recipe = recipe.clone();
        pending.push(tokio::spawn(async move { call(&app, &recipe).await }));
    }
    let mut observed = false;
    for _ in 0..200 {
        if pending.iter().any(tokio::task::JoinHandle::is_finished) {
            for task in &pending {
                task.abort();
            }
            holder.rollback().await?;
            return Err("release completed before controlled lock release".into());
        }
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND pid<>pg_backend_pid()")
            .fetch_one(pool).await?;
        if count == 2 {
            observed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    if !observed {
        for task in &pending {
            task.abort();
        }
        holder.rollback().await?;
        return Err("two release lock waiters were not observed".into());
    }
    holder.commit().await?;
    let mut replies = Vec::new();
    for task in pending {
        replies.push(task.await??);
    }
    let after = clock(pool).await?;
    let (stored_finished, updated): (Timestamp, Timestamp) = sqlx::query_as("SELECT a.finished_at,h.updated_at FROM attempts a JOIN units h ON h.id=a.unit_id WHERE a.id=$1::text::uuid")
        .bind(ATTEMPT).fetch_one(pool).await?;
    assert!(before.0 <= stored_finished.0 && stored_finished.0 <= after.0);
    assert_eq!(stored_finished.0, updated.0);
    check_index_clocks(pool, before, after).await?;
    let mut outputs = Vec::new();
    for (status, _, mut value, _) in replies {
        if status == 200 {
            let finished: Timestamp = value["finished_at"]
                .as_str()
                .ok_or("race timestamp")?
                .parse()?;
            assert!(before.0 <= finished.0 && finished.0 <= after.0);
            assert_eq!(finished.0, stored_finished.0);
            value["finished_at"] = json!("@checked-clock");
        }
        outputs.push(json!({"status":status,"response":output(value)}));
    }
    outputs.sort_by_key(|value| value["status"].as_u64());
    assert_eq!(
        outputs,
        recipe["outputs"].as_array().ok_or("race outputs")?.clone()
    );
    assert_eq!(recipe["observed_lock_waiters"], 2);
    assert_eq!(
        storage(pool, true).await?,
        recipe["storage"],
        "race storage"
    );
    Ok(())
}
#[tokio::test]
#[ignore = "isolated migrated PostgreSQL child required"]
#[allow(
    clippy::too_many_lines,
    reason = "Complete ordered fixture replay and owned database guard"
)]
async fn attempt_release_matches_production() -> Result<()> {
    let fixture: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/attempt_release_http/reference.json"
    ))?;
    assert_eq!(fixture["python_version"], "3.13.11");
    assert_eq!(fixture["unicode_version"], "15.1.0");
    let url = std::env::var("CANNERY_ATTEMPT_RELEASE_DATABASE_URL")?;
    let settings = load_settings(
        None,
        &BTreeMap::from([("CANNERY_DATABASE_URL".into(), url)]),
    )?;
    let profile = AttemptLeaseContext {
        repository: cannery_attempts::model::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
        release: Some(Arc::new(AttemptReleaseContext {
            configuration: cannery_research::config_repo::JsonContext {
                encode_nesting_budget: 80,
                decode_nesting_budget: 80,
            },
            science: cannery_research::science::RenderingContext {
                nesting_budget: 100,
            },
            response: ResponseContext {
                inferred_nesting_budget: 100,
            },
        })),
    };
    let (app, state) = application_with_attempt_lease_context(settings, Arc::new(profile))?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("owned child required")?;
    if suffix.len() != 24 || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("owned child required".into());
    }
    let cases = fixture["cases"].as_array().ok_or("cases")?;
    assert_eq!(cases.len(), 151);
    for (boundary, count) in [
        ("json_syntax", 4),
        ("log_size", 2),
        ("configuration_integer", 5),
        ("reason_unicode", 1),
        ("dto_shape", 30),
    ] {
        assert_eq!(
            cases
                .iter()
                .filter(|recipe| recipe["native_boundary"] == boundary)
                .count(),
            count
        );
    }
    let mut dto_profiles = [
        "body-missing-reason",
        "body-array",
        "body-integer",
        "body-boolean",
        "reason-null",
        "reason-boolean",
        "code-and-logs-wrong-shape",
        "body-unknown-field",
        "logs-missing-fields",
        "raw-null",
        "step-integer",
        "log-size-boolean",
        "log-unknown-field",
        "log-size-fraction",
        "log-size-underscore-string",
    ]
    .map(str::to_owned)
    .to_vec();
    dto_profiles.extend(
        [
            "admin",
            "researcher",
            "member",
            "viewer",
            "outsider",
            "readonly",
            "agent",
            "foreign-agent",
            "experimenter",
            "verifier",
            "second-verifier",
        ]
        .map(|role| format!("auth-before-validation-{role}")),
    );
    dto_profiles.extend((0..4).map(|index| format!("combined-errors-{index}")));
    for profile in &dto_profiles {
        assert_eq!(
            cases
                .iter()
                .filter(|recipe| recipe["native_boundary"] == "dto_shape"
                    && recipe["native_dto_profile"] == *profile)
                .count(),
            1,
            "declared DTO profile {profile}"
        );
    }
    for recipe in cases {
        reset(&state.pool, recipe["setup"].as_str().ok_or("setup")?).await?;
        let initial = storage(&state.pool, false).await?;
        let before = clock(&state.pool).await?;
        let (status, allow, mut value, mut bytes) = call(&app, recipe).await?;
        let after = clock(&state.pool).await?;
        let boundary = recipe["native_boundary"].as_str();
        if let Some(
            boundary @ ("json_syntax" | "log_size" | "configuration_integer" | "dto_shape"),
        ) = boundary
        {
            assert_eq!(
                initial, recipe["native_initial"],
                "native initial {}",
                recipe["name"]
            );
            assert_eq!(json!(allow), recipe["allow"]);
            match boundary {
                "dto_shape" => {
                    let profile = recipe["native_dto_profile"].as_str().ok_or("DTO profile")?;
                    assert!(dto_profiles.iter().any(|expected| expected == profile));
                    assert_ne!(recipe["role"], "none");
                    assert_eq!(recipe["status"], 422);
                    assert_eq!(recipe["committed"], false);
                    assert!(
                        serde_json::from_str::<cannery_server::api_models::ReleaseRequest>(
                            recipe["body"].as_str().ok_or("DTO body")?
                        )
                        .is_err(),
                        "authored DTO shape probe {profile}"
                    );
                    assert_eq!(status, 422);
                    assert_eq!(value, dto_refusal());
                }
                "json_syntax" => {
                    assert!(
                        ["body-model", "raw-body", "step"]
                            .contains(&recipe["name"].as_str().ok_or("syntax name")?)
                    );
                    assert_eq!(recipe["status"], 422);
                    assert_eq!(recipe["committed"], false);
                    let raw = recipe["body"].as_str().ok_or("syntax body")?.as_bytes();
                    let error = serde_json::from_slice::<Value>(raw)
                        .err()
                        .ok_or("invalid JSON probe accepted")?;
                    assert_eq!(status, 422);
                    assert_eq!(
                        value,
                        native_refusal(&format!("body/{}", error.column()), "JSON decode error")
                    );
                }
                "log_size" => {
                    assert_eq!(recipe["name"], "verified-logs");
                    assert_eq!(recipe["status"], 200);
                    assert_eq!(recipe["committed"], true);
                    assert_eq!(status, 422);
                    assert!(
                        serde_json::from_str::<cannery_server::api_models::ReleaseRequest>(
                            recipe["body"].as_str().ok_or("log DTO body")?
                        )
                        .is_err(),
                        "authored log integer DTO probe"
                    );
                    assert_eq!(value, dto_refusal());
                }
                "configuration_integer" => {
                    assert_eq!(recipe["name"], "retry-limit");
                    assert_eq!(recipe["status"], 200);
                    assert_eq!(recipe["committed"], true);
                    assert_eq!(status, 500);
                    assert_eq!(value, Value::Null);
                    assert_eq!(bytes, b"Internal Server Error");
                }
                _ => unreachable!(),
            }
            assert_eq!(
                storage(&state.pool, false).await?,
                initial,
                "native refusal changed raw storage {}",
                recipe["name"]
            );
            // This checked refusal has no commit; the source observation and
            // its successful snapshot remain independently in the fixture.
            continue;
        }
        assert!(
            recipe["native_boundary"].is_null() || boundary == Some("reason_unicode"),
            "unknown native release boundary"
        );
        assert_eq!(
            json!(status),
            recipe["status"],
            "status {}: {value}",
            recipe["name"]
        );
        assert_eq!(json!(allow), recipe["allow"], "allow {}", recipe["name"]);
        let (stored_state,finished,updated):(String,Option<Timestamp>,Timestamp)=sqlx::query_as("SELECT a.state,a.finished_at,h.updated_at FROM attempts a JOIN units h ON h.id=a.unit_id WHERE a.id=$1::text::uuid").bind(ATTEMPT).fetch_one(&state.pool).await?;
        let committed = stored_state == "failed"
            && finished.is_some_and(|value| before.0 <= value.0 && value.0 <= after.0);
        assert_eq!(json!(committed), recipe["committed"]);
        if committed {
            let finished = finished.ok_or("finished")?;
            assert!(before.0 <= finished.0 && finished.0 <= after.0);
            assert_eq!(finished.0, updated.0);
            check_index_clocks(&state.pool, before, after).await?;
            if status == 200 {
                assert_eq!(
                    value["finished_at"],
                    cannery_server::timestamps::public_timestamp(finished)
                );
                let timestamp = value["finished_at"].as_str().ok_or("finished timestamp")?;
                bytes = String::from_utf8(bytes)?
                    .replace(timestamp, "@checked-clock")
                    .into_bytes();
                value["finished_at"] = json!("@checked-clock");
            }
        } else {
            assert_eq!(
                storage(&state.pool, false).await?,
                initial,
                "refusal changed domain"
            );
        }
        assert_eq!(
            output(value),
            recipe["response"],
            "response {}",
            recipe["name"]
        );
        if let Some(expected) = recipe["wire_hex"].as_str() {
            let mut actual = String::new();
            for byte in bytes {
                write!(&mut actual, "{byte:02x}")?;
            }
            assert_eq!(actual, expected, "wire {}", recipe["name"]);
        }
        let stored = storage(&state.pool, true).await?;
        let expected = &fixture["snapshots"][recipe["storage"].as_str().ok_or("storage")?];
        if boundary == Some("reason_unicode") {
            assert_eq!(recipe["name"], "body-model");
            assert!(committed);
            assert_eq!(status, 200);
            assert_unicode_reason_storage(&stored, expected)?;
        } else {
            assert_eq!(stored, *expected, "storage {}", recipe["name"]);
        }
    }
    // Keep the application log-count boundary reachable after malformed log
    // objects began failing the declared DTO shape before domain validation.
    let log_bound = cases
        .iter()
        .find(|recipe| recipe["native_dto_profile"] == "logs-missing-fields")
        .ok_or("log-count fixture")?;
    let mut log_bound = log_bound.clone();
    log_bound["body"] = json!(serde_json::to_string(&json!({
        "reason":"ok", "logs":vec![json!({"key":"logs/é😀","size_bytes":5,"sha256":"a".repeat(64)});65]
    }))?);
    assert!(
        serde_json::from_str::<cannery_server::api_models::ReleaseRequest>(
            log_bound["body"].as_str().ok_or("log-count body")?
        )
        .is_ok()
    );
    reset(
        &state.pool,
        log_bound["setup"].as_str().ok_or("log-count setup")?,
    )
    .await?;
    let initial = storage(&state.pool, false).await?;
    let (status, _, response, _) = call(&app, &log_bound).await?;
    assert_eq!(status, 422);
    assert_eq!(
        response,
        native_refusal(
            "body/logs",
            "List should have at most 64 items after validation"
        )
    );
    assert_eq!(
        storage(&state.pool, false).await?,
        initial,
        "valid log-count refusal changed raw storage"
    );
    let races = fixture["races"].as_array().ok_or("races")?;
    assert_eq!(races.len(), 2);
    for recipe in races {
        race(&app, &state.pool, recipe).await?;
    }
    state.pool.close().await;
    println!("{} release observations matched", cases.len());
    Ok(())
}
