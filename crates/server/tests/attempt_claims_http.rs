//! Independent live replay of the unchanged production claim controller.
#![forbid(unsafe_code)]
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use cannery_core::settings::load_settings;
use cannery_server::{
    application_with_attempt_claim_context, attempt_claim_routes::AttemptClaimContext,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::fmt::Write;
use std::{collections::BTreeMap, error::Error, sync::Arc};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
fn fixture() -> Result<Value> {
    decode(&std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/attempt_claims_http/reference.json"
    ))?)
}
fn decode(raw: &str) -> Result<Value> {
    let mut decoder = serde_json::Deserializer::from_str(raw);
    decoder.disable_recursion_limit();
    Ok(Value::deserialize(&mut decoder)?)
}
fn profile(fixed: bool) -> AttemptClaimContext {
    AttemptClaimContext {
        repository: cannery_attempts::model::JsonContext {
            encode_nesting_budget: 1000,
            decode_nesting_budget: 1000,
        },
        configuration: cannery_research::config_repo::JsonContext {
            encode_nesting_budget: 1000,
            decode_nesting_budget: 1000,
        },
        rendering: cannery_research::science::RenderingContext { nesting_budget: 80 },
        manifest_decode_budget: 1000,
        audit_budget: 80,
        response: cannery_server::attempt_read_wire::ResponseContext {
            inferred_nesting_budget: 255,
        },
        equality: Arc::new(cannery_server::step_binding::CheckedOutputEquality),
        mint: if fixed {
            || {
                Ok(cannery_core::principal::Secret::new(
                    "cr_lease_fixture_wire".into(),
                ))
            }
        } else {
            || {
                Ok(cannery_identity::secrets::new_secret("cr_lease_")?
                    .plaintext()
                    .clone())
            }
        },
    }
}
fn hex(bytes: &[u8]) -> Result<String> {
    let mut out = String::new();
    for byte in bytes {
        write!(&mut out, "{byte:02x}")?;
    }
    Ok(out)
}
fn output(value: Value) -> Value {
    if value
        .get("error")
        .is_some_and(|e| e["code"] == "validation_failed")
    {
        let e = &value["error"];
        return json!({"error":{"code":e["code"],"details":e["details"].as_array().map(|v|v.iter().map(|v|json!({"path":v["path"]})).collect::<Vec<_>>())}});
    }
    value
}
async fn reset(pool: &PgPool, r: &Value) -> Result<()> {
    sqlx::raw_sql("DROP FUNCTION IF EXISTS fixture_claim_fixed() CASCADE")
        .execute(pool)
        .await?;
    sqlx::raw_sql("DROP FUNCTION IF EXISTS fixture_claim_fault() CASCADE")
        .execute(pool)
        .await?;
    sqlx::raw_sql(include_str!("fixtures/attempt_claims_http/reset.sql"))
        .execute(pool)
        .await?;
    sqlx::raw_sql(include_str!("fixtures/attempt_claims_http/seed.sql"))
        .execute(pool)
        .await?;
    if r["predecessor"] == true {
        sqlx::raw_sql(include_str!("fixtures/attempt_claims_http/predecessor.sql"))
            .execute(pool)
            .await?;
    }
    for (table, name, field) in [
        ("config_revisions", "science", "kind"),
        ("producer_manifests", "producer", "name"),
        ("experiment_manifests", "experiment", "name"),
    ] {
        let content = serde_json::to_string(&r[name])?;
        sqlx::query(&format!("INSERT INTO {table}(project_id,{field},revision,content,created_by,created_at) VALUES('00000000-0000-0000-0000-000000000010',$1,1,$2::text::jsonb,'00000000-0000-0000-0000-000000000001','2001-01-01Z')")).bind(name).bind(content).execute(pool).await?;
    }
    let setup = r["setup"].as_str().ok_or("setup")?;
    if !setup.is_empty() {
        sqlx::raw_sql(setup).execute(pool).await?;
    }
    sqlx::query("UPDATE search_documents SET updated_at='2001-01-01Z'")
        .execute(pool)
        .await?;
    Ok(())
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
        ("config_revisions", "project_id,kind,revision"),
        ("producer_manifests", "project_id,name,revision"),
        ("experiment_manifests", "project_id,name,revision"),
    ] {
        let rows: Vec<String> = sqlx::query_scalar(&format!(
            "SELECT to_jsonb(t)::text FROM {table} t ORDER BY {order}"
        ))
        .fetch_all(pool)
        .await?;
        value[table] = json!(rows);
    }
    Ok(value)
}
async fn call(app: &Router, r: &Value) -> Result<(u16, Option<String>, Value, String)> {
    let role = r["role"].as_str().ok_or("role")?;
    let mut builder = Request::builder()
        .method(r["method"].as_str().ok_or("method")?)
        .uri(r["path"].as_str().ok_or("path")?);
    if role != "none" {
        let token = if role == "bad" {
            "bad".into()
        } else {
            format!(
                "{}track_http_{role}",
                if [
                    "agent",
                    "experimenter",
                    "tester",
                    "foreign-agent",
                    "agent-readonly"
                ]
                .contains(&role)
                {
                    "cr_svc_"
                } else {
                    "cr_pat_"
                }
            )
        };
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let body = if let Some(raw) = r["raw"].as_str() {
        builder = builder.header("content-type", "application/json");
        Body::from(raw.to_owned())
    } else {
        Body::empty()
    };
    let response = app.clone().oneshot(builder.body(body)?).await?;
    let status = response.status().as_u16();
    let allow = response
        .headers()
        .get("allow")
        .map(|v| v.to_str().map(str::to_owned))
        .transpose()?;
    let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024).await?;
    let value = if status == 500 || bytes.is_empty() {
        Value::Null
    } else {
        decode(std::str::from_utf8(&bytes)?)?
    };
    Ok((status, allow, value, hex(&bytes)?))
}
fn micros(value: &Value) -> Result<i64> {
    Ok(chrono::DateTime::parse_from_rfc3339(value.as_str().ok_or("clock")?)?.timestamp_micros())
}
#[allow(
    clippy::too_many_lines,
    reason = "Checks every generated relationship before projecting the complete raw storage"
)]
fn project(
    initial: &Value,
    stored: &Value,
    body: &Value,
    before: i64,
    after: i64,
    fixed: bool,
) -> Result<(Value, Value)> {
    let mut substitutions = BTreeMap::<String, String>::new();
    let old = initial["attempts"]
        .as_array()
        .ok_or("initial attempts")?
        .iter()
        .map(|raw| {
            serde_json::from_str::<Value>(raw.as_str().ok_or("raw")?)
                .map(|v| v["id"].clone())
                .map_err(Into::into)
        })
        .collect::<Result<Vec<_>>>()?;
    for raw in stored["attempts"].as_array().ok_or("attempts")? {
        let a: Value = serde_json::from_str(raw.as_str().ok_or("raw")?)?;
        if old.contains(&a["id"]) {
            continue;
        }
        let claimed = micros(&a["claimed_at"])?;
        if fixed {
            assert_eq!(
                claimed,
                chrono::DateTime::parse_from_rfc3339("2001-02-03T04:05:06.123456Z")?
                    .timestamp_micros()
            );
        } else {
            assert!(before <= claimed && claimed <= after);
        }
        let expires = micros(&a["lease_expires_at"])?;
        assert_eq!(expires, claimed + 90_000_000);
        assert_eq!(a["state"], "claimed");
        let prior_rows = initial["hypotheses"]
            .as_array()
            .ok_or("hypotheses")?
            .iter()
            .map(|raw| decode(raw.as_str().ok_or("raw")?))
            .collect::<Result<Vec<_>>>()?;
        let hypothesis = prior_rows
            .iter()
            .find(|h| h["id"] == a["hypothesis_id"])
            .ok_or("claim hypothesis")?;
        assert_eq!(
            a["lease_generation"].as_i64().ok_or("generation")?,
            hypothesis["lease_generation"]
                .as_i64()
                .ok_or("prior generation")?
                + 1
        );
        let attempts = initial["attempts"]
            .as_array()
            .ok_or("attempts")?
            .iter()
            .map(|raw| decode(raw.as_str().ok_or("raw")?))
            .collect::<Result<Vec<_>>>()?;
        let predecessor = attempts
            .iter()
            .filter(|p| p["hypothesis_id"] == a["hypothesis_id"])
            .max_by_key(|p| p["sequence"].as_i64());
        assert_eq!(
            a["sequence"].as_i64().ok_or("sequence")?,
            predecessor.map_or(0, |p| p["sequence"].as_i64().unwrap_or(-1)) + 1
        );
        assert_eq!(
            a["predecessor_id"],
            predecessor.map_or(Value::Null, |p| p["id"].clone())
        );
        if !fixed {
            substitutions.insert(
                a["id"].as_str().ok_or("id")?.into(),
                format!(
                    "@checked-attempt-{}-{}",
                    a["hypothesis_id"].as_str().ok_or("hypothesis")?,
                    a["sequence"]
                ),
            );
        }
        for field in ["claimed_at", "lease_expires_at", "deadline"] {
            if !a[field].is_null() {
                let time = chrono::DateTime::parse_from_rfc3339(a[field].as_str().ok_or("time")?)?;
                if field == "deadline" {
                    assert_eq!(time.timestamp_micros(), claimed + 360_000_000);
                }
                if !fixed {
                    substitutions.insert(
                        a[field].as_str().ok_or("time")?.into(),
                        "@checked-clock".into(),
                    );
                    substitutions.insert(
                        cannery_core::timestamps::Timestamp(time).model_isoformat(),
                        "@checked-clock".into(),
                    );
                    substitutions.insert(
                        cannery_core::timestamps::Timestamp(time).isoformat(),
                        "@checked-clock".into(),
                    );
                }
            }
        }
        if body["attempt"]["id"] == a["id"] {
            let token = body["lease_token"].as_str().ok_or("token")?;
            assert_eq!(
                hex(&cannery_identity::secrets::digest(token))?,
                a["lease_token_hash"].as_str().ok_or("digest")?[2..]
            );
            assert_eq!(body["attempt"]["sequence"], a["sequence"]);
            assert_eq!(body["attempt"]["lease_generation"], a["lease_generation"]);
            assert_eq!(micros(&body["lease_expires_at"])?, expires);
            if !fixed {
                substitutions.insert(token.into(), "@checked-token".into());
            }
        }
        if !fixed {
            substitutions.insert(
                a["lease_token_hash"].as_str().ok_or("digest")?.into(),
                "@checked-digest".into(),
            );
        }
        let events = stored["audit_events"]
            .as_array()
            .ok_or("events")?
            .iter()
            .map(|raw| {
                serde_json::from_str::<Value>(raw.as_str().ok_or("event")?).map_err(Into::into)
            })
            .collect::<Result<Vec<_>>>()?;
        let linked = events
            .iter()
            .filter(|e| e["subject_id"] == a["id"])
            .collect::<Vec<_>>();
        assert_eq!(linked.len(), 1);
        let number = a["hypothesis_id"].as_str().ok_or("hypothesis")?[33..].parse::<i32>()?;
        assert_eq!(
            linked[0]["new_state"]["ref"],
            format!("#{number}.{}", a["sequence"])
        );
    }
    for (table, field) in [
        ("hypotheses", "updated_at"),
        ("audit_events", "occurred_at"),
        ("search_documents", "updated_at"),
        ("search_documents", "occurred_at"),
    ] {
        let old = initial[table]
            .as_array()
            .ok_or("initial")?
            .iter()
            .map(|raw| {
                serde_json::from_str::<Value>(raw.as_str().ok_or("raw")?)
                    .map(|v| v[field].clone())
                    .map_err(Into::into)
            })
            .collect::<Result<Vec<_>>>()?;
        for raw in stored[table].as_array().ok_or("rows")? {
            let row: Value = serde_json::from_str(raw.as_str().ok_or("raw")?)?;
            if !old.contains(&row[field]) {
                if fixed && table == "search_documents" && field == "occurred_at" {
                    assert_eq!(
                        micros(&row[field])?,
                        chrono::DateTime::parse_from_rfc3339("2001-02-03T04:05:06.123456Z")?
                            .timestamp_micros()
                    );
                    continue;
                }
                let time = micros(&row[field])?;
                assert!(before <= time && time <= after);
                substitutions.insert(
                    row[field].as_str().ok_or("clock")?.into(),
                    "@checked-clock".into(),
                );
            }
        }
    }
    let mut substitutions = substitutions.into_iter().collect::<Vec<_>>();
    substitutions.sort_by_key(|(old, _)| std::cmp::Reverse(old.len()));
    let replace = |mut raw: String| {
        for (old, new) in &substitutions {
            raw = raw.replace(old, new);
        }
        raw
    };
    let mut projection = json!({});
    for (table, rows) in stored.as_object().ok_or("storage")? {
        let mut rows = rows
            .as_array()
            .ok_or("rows")?
            .iter()
            .map(|raw| raw.as_str().map(|v| replace(v.into())).ok_or("raw"))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.sort();
        projection[table] = json!(rows);
    }
    let body = decode(&replace(serde_json::to_string(body)?))?;
    Ok((projection, body))
}
#[tokio::test]
#[ignore = "requires positive exact selection and guarded fresh migrated child"]
#[allow(
    clippy::too_many_lines,
    reason = "Independent response, raw storage and physical lock replay"
)]
async fn attempt_claims_match_production() -> Result<()> {
    let url = std::env::var("CANNERY_ATTEMPT_CLAIMS_HTTP_DATABASE_URL")?;
    let mut settings = load_settings(
        None,
        &BTreeMap::from([("CANNERY_DATABASE_URL".into(), url)]),
    )?;
    settings.leases.ttl_seconds = 90_i64.into();
    settings.leases.job_overhead_seconds = 300_i64.into();
    let (app, state) =
        application_with_attempt_claim_context(settings.clone(), Arc::new(profile(false)))?;
    let (fixed, fixed_state) =
        application_with_attempt_claim_context(settings, Arc::new(profile(true)))?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("owned nonce child")?;
    if suffix.len() != 24 || !suffix.bytes().all(|v| v.is_ascii_hexdigit()) {
        return Err("owned nonce child".into());
    }
    let f = fixture()?;
    for r in f["cases"].as_array().ok_or("cases")? {
        reset(&state.pool, r).await?;
        let initial = storage(&state.pool).await?;
        let before: i64 =
            sqlx::query_scalar("SELECT (extract(epoch FROM clock_timestamp())*1000000)::bigint")
                .fetch_one(&state.pool)
                .await?;
        let mut held = if let Some(hold) = r["hold"].as_str() {
            let mut tx = state.pool.begin().await?;
            if hold.starts_with("track-") {
                sqlx::query("SELECT id FROM tracks WHERE slug='track-3' FOR UPDATE")
                    .execute(&mut *tx)
                    .await?;
            } else {
                sqlx::query("SELECT id FROM hypotheses WHERE number=$1 FOR UPDATE")
                    .bind(if hold == "hypothesis" { 1_i32 } else { 2_i32 })
                    .execute(&mut *tx)
                    .await?;
            }
            Some(tx)
        } else {
            None
        };
        let result = if r["hold"]
            .as_str()
            .is_some_and(|hold| hold.starts_with("track-"))
        {
            let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(&mut **held.as_mut().ok_or("held track")?)
                .await?;
            let mut tasks = Tasks(vec![]);
            let waiters = r["waiters"].as_u64().ok_or("waiters")?;
            for _ in 0..waiters {
                let app = app.clone();
                let recipe = r.clone();
                tasks
                    .0
                    .push(tokio::spawn(async move { call(&app, &recipe).await }));
            }
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid))").bind(pid).fetch_one(&state.pool).await?;
                    if count == i64::try_from(waiters)? { break; }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Ok::<_,Box<dyn Error+Send+Sync>>(())
            }).await??;
            for number in if waiters == 2 {
                vec![1_i32, 3]
            } else {
                vec![1]
            } {
                let mut check = state.pool.begin().await?;
                let err =
                    sqlx::query("SELECT id FROM hypotheses WHERE number=$1 FOR UPDATE NOWAIT")
                        .bind(number)
                        .execute(&mut *check)
                        .await
                        .err()
                        .ok_or("track waiter lacked hypothesis lock")?;
                assert_eq!(
                    err.as_database_error()
                        .and_then(sqlx::error::DatabaseError::code)
                        .as_deref(),
                    Some("55P03")
                );
                check.rollback().await?;
            }
            let change = if r["hold"] == "track-paused" {
                "state='paused'"
            } else {
                "mode='workflow',workflow='{\"steps\":[]}'"
            };
            let mut tx = held.take().ok_or("held track")?;
            sqlx::query(&format!("UPDATE tracks SET {change} WHERE slug='track-3'"))
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            let mut responses = vec![];
            while let Some(task) = tasks.0.pop() {
                responses
                    .push(tokio::time::timeout(std::time::Duration::from_secs(5), task).await???);
            }
            if waiters == 2 {
                assert_eq!(responses[0].0, 409);
                assert_eq!(responses[0], responses[1]);
            }
            responses.remove(0)
        } else {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                call(if r["fixed"] == true { &fixed } else { &app }, r),
            )
            .await??
        };
        let (status, allow, response, wire) = result;
        if let Some(tx) = held {
            tx.rollback().await?;
        }
        let after: i64 =
            sqlx::query_scalar("SELECT (extract(epoch FROM clock_timestamp())*1000000)::bigint")
                .fetch_one(&state.pool)
                .await?;
        let raw = storage(&state.pool).await?;
        if let Some(native) = r["native_profile"].as_str() {
            if native == "stored-workflow-dto" {
                let depth = r["name"]
                    .as_str()
                    .ok_or("name")?
                    .strip_prefix("workflow-response-depth-")
                    .ok_or("depth recipe")?
                    .parse::<usize>()?;
                assert!((250..=257).contains(&depth));
                assert!(r["experiment"]["metadata"].get("name").is_none());
                assert!(r["experiment"]["metadata"].get("unused").is_some());
                assert!(
                    serde_json::from_value::<cannery_server::api_models::StepManifestRequest>(
                        r["experiment"].clone()
                    )
                    .is_err()
                );
                assert_eq!(status, 500);
                assert_eq!(allow, None);
                assert_eq!(response, Value::Null);
                assert_eq!(wire, "496e7465726e616c20536572766572204572726f72");
                assert_eq!(
                    raw, initial,
                    "malformed workflow DTO must roll back before commit"
                );
                continue;
            }
            let body = r["raw"].as_str().ok_or("native body")?;
            let expected = match native {
                "request-dto" => {
                    assert!(
                        !serde_json::from_str::<Value>(body)?.is_object()
                            || serde_json::from_str::<cannery_server::api_models::ClaimRequest>(
                                body
                            )
                            .is_err()
                    );
                    json!({"error":{"code":"validation_failed","message":"request does not match the REST contract","details":null}})
                }
                "json-syntax" => {
                    let error = serde_json::from_str::<Value>(body)
                        .err()
                        .ok_or("invalid JSON required")?;
                    json!({"error":{"code":"validation_failed","message":"request validation failed","details":[{"path":format!("body/{}",error.column()),"message":"JSON decode error"}]}})
                }
                _ => return Err("unknown native attempt claim profile".into()),
            };
            assert_eq!(status, 422, "native refusal {}", r["name"]);
            assert_eq!(allow, None);
            assert_eq!(response, expected);
            let typed: cannery_server::api_models::ErrorResponse =
                serde_json::from_value(expected)?;
            assert_eq!(wire, hex(&serde_json::to_vec(&typed)?)?);
            assert_eq!(
                raw, initial,
                "native refusal changed raw storage {}",
                r["name"]
            );
            // Source bool/float bodies can commit; their observations remain in
            // the corpus, while the native request must leave the full seed intact.
            if !(200..300).contains(&r["status"].as_u64().ok_or("source status")?) {
                let (stored, _) = project(&initial, &raw, &response, before, after, false)?;
                assert_eq!(
                    stored,
                    f["snapshots"][r["storage"].as_str().ok_or("snapshot")?]
                );
            }
            continue;
        }
        if (200..300).contains(&r["status"].as_u64().ok_or("source status")?) {
            let _: cannery_server::api_models::ClaimOut =
                serde_json::from_value(r["response"].clone())
                    .map_err(|error| format!("ordinary source claim {}: {error}", r["name"]))?;
        }
        let mut expected_response = r["response"].clone();
        if let Some(quotes) = r["native_error_quotes"].as_array()
            && !quotes.is_empty()
        {
            assert_eq!(r["status"], 409);
            for label in quotes {
                let label = label.as_str().ok_or("quoted label")?;
                let from = format!("'{label}'");
                let to = serde_json::to_string(label)?;
                let mut hits = 0;
                let error = &mut expected_response["error"];
                if let Some(message) = error["message"].as_str() {
                    hits += message.matches(&from).count();
                    error["message"] = json!(message.replace(&from, &to));
                }
                if let Some(details) = error["details"].as_array_mut() {
                    for detail in details {
                        let message = detail["message"].as_str().ok_or("detail message")?;
                        hits += message.matches(&from).count();
                        detail["message"] = json!(message.replace(&from, &to));
                    }
                }
                assert!(hits > 0, "declared quoted label absent from source error");
            }
            if let Some(details) = expected_response["error"]["details"].as_array_mut() {
                for detail in details {
                    assert_eq!(detail.as_object().ok_or("validation detail")?.len(), 2);
                    *detail = json!({"path":detail["path"],"message":detail["message"]});
                }
            }
            let typed: cannery_server::api_models::ErrorResponse =
                serde_json::from_value(expected_response.clone())?;
            assert_eq!(
                wire,
                hex(&serde_json::to_vec(&typed)?)?,
                "complete native domain error wire {}",
                r["name"]
            );
        }
        let (stored, response) =
            project(&initial, &raw, &response, before, after, r["fixed"] == true)?;
        assert_eq!(
            json!(status),
            r["status"],
            "status {} {response}",
            r["name"]
        );
        assert_eq!(json!(allow), r["allow"], "allow {}", r["name"]);
        assert_eq!(
            output(response),
            expected_response,
            "response {}",
            r["name"]
        );
        assert_eq!(
            stored,
            f["snapshots"][r["storage"].as_str().ok_or("snapshot")?],
            "storage {}",
            r["name"]
        );
        if r["fixed"] == true {
            let source = r["wire"].as_str().ok_or("fixed wire")?;
            let bytes = (0..source.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&source[i..i + 2], 16))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let typed: cannery_server::api_models::ClaimOut = serde_json::from_slice(&bytes)?;
            assert_eq!(
                wire,
                hex(&serde_json::to_vec(&typed)?)?,
                "wire {}",
                r["name"]
            );
        }
        if status == 500 {
            assert_eq!(
                wire,
                r["failure_wire"].as_str().ok_or("failure wire")?,
                "failure wire {}",
                r["name"]
            );
        }
        if !(200..300).contains(&status) && r["post_commit"] != true && r["external_change"] != true
        {
            assert_eq!(raw, initial, "failed claim mutation {}", r["name"]);
        }
    }
    state.pool.close().await;
    fixed_state.pool.close().await;
    Ok(())
}
type Observation = (u16, Option<String>, Value, String);
struct Tasks(Vec<tokio::task::JoinHandle<Result<Observation>>>);
impl Drop for Tasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}
