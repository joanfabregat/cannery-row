//! Actual source HTTP replay; operational clocks and tokens are checked before projection.
#![forbid(unsafe_code)]
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use cannery_core::{principal::Secret, settings::load_settings};
use cannery_server::{
    api_models::{JobClaimOut, JobClaimRequest},
    application_with_job_claim_context,
    job_claim_routes::JobClaimContext,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::fmt::Write;
use std::{collections::BTreeMap, error::Error, sync::Arc};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
const FIXED: &str = "cr_job_fixture_wire";
fn fixture() -> Result<Value> {
    Ok(serde_json::from_str(&std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/job_claims_http/reference.json"
    ))?)?)
}
fn profile(fixed: bool) -> JobClaimContext {
    JobClaimContext {
        jobs: cannery_jobs::repo::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
        attempts: cannery_attempts::model::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
        rendering: cannery_research::science::RenderingContext { nesting_budget: 80 },
        hash_budget: 80,
        response_budget: 80,
        mint: if fixed {
            || Ok(Secret::new(FIXED.into()))
        } else {
            || {
                Ok(cannery_identity::secrets::new_secret("cr_job_")?
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
    for (own, seed) in [
        (false, include_str!("fixtures/job_claims_http/reset.sql")),
        (false, include_str!("fixtures/attempt_reads_http/seed.sql")),
        (true, include_str!("fixtures/job_claims_http/seed.sql")),
    ] {
        let mut seed = seed.to_owned();
        if own && let Some(spec) = r["legacy_spec"].as_str() {
            let marker = "3,'agent',NULL,'";
            let start = seed.find(marker).ok_or("seed marker")? + marker.len();
            let end = start + seed[start..].find("',600,'").ok_or("seed delimiter")?;
            seed.replace_range(start..end, &spec.replace('\'', "''"));
        }
        sqlx::raw_sql(&seed).execute(pool).await?;
    }
    for statement in r["setup"]
        .as_str()
        .ok_or("setup")?
        .split("-- fixture boundary\n")
    {
        if !statement.is_empty() {
            sqlx::raw_sql(statement).execute(pool).await?;
        }
    }
    Ok(())
}
async fn storage(pool: &PgPool, projected: bool) -> Result<Value> {
    let mut value = json!({});
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
    ] {
        let mut expression = "to_jsonb(t)".to_owned();
        if projected {
            let clocks: &[&str] = match table {
                "jobs" => &["claimed_at", "deadline", "lease_expires_at"],
                "audit_events" => &["occurred_at"],
                "idempotency_keys" => &["created_at"],
                _ => &[],
            };
            for field in clocks {
                expression = format!(
                    "jsonb_set({expression},'{{{field}}}',CASE WHEN {field} BETWEEN '2002-01-01Z' AND '2090-01-01Z' THEN '\"@checked-clock\"'::jsonb ELSE to_jsonb(t)->'{field}' END)"
                );
            }
            if table == "jobs" {
                expression = format!(
                    "jsonb_set({expression},'{{lease_token_hash}}',CASE WHEN lease_token_hash IS NOT NULL AND lease_token_hash<>sha256(convert_to('{FIXED}','UTF8')) THEN '\"@checked-token-digest\"'::jsonb ELSE to_jsonb(t)->'lease_token_hash' END)"
                );
            }
        }
        value[table] = json!(
            sqlx::query_scalar::<_, String>(&format!(
                "SELECT ({expression})::text FROM {table} t ORDER BY {order}"
            ))
            .fetch_all(pool)
            .await?
        );
    }
    Ok(value)
}
async fn call(
    app: &Router,
    r: &Value,
    token: Option<&str>,
    revoked: Option<&str>,
) -> Result<(u16, Option<String>, Value, String)> {
    let role = r["role"].as_str().ok_or("role")?;
    let mut request = Request::builder()
        .method(r["method"].as_str().ok_or("method")?)
        .uri(r["path"].as_str().ok_or("path")?);
    if role != "none" {
        let prefix = if [
            "agent",
            "foreign-agent",
            "verifier",
            "other-verifier",
            "foreign-verifier",
            "verifier-readonly",
        ]
        .contains(&role)
        {
            "cr_svc_"
        } else {
            "cr_pat_"
        };
        request = request.header("authorization", format!("Bearer {prefix}track_http_{role}"));
    }
    for h in r["headers"].as_array().ok_or("headers")? {
        request = request.header(
            h[0].as_str().ok_or("header")?,
            h[1].as_str().ok_or("value")?,
        );
    }
    if r["use_previous"] == true {
        request = request
            .header("X-Lease-Token", token.ok_or("token")?)
            .header("X-Lease-Generation", "1");
    }
    if r["use_revoked"] == true {
        request = request
            .header("X-Lease-Token", revoked.ok_or("revoked")?)
            .header("X-Lease-Generation", "2");
    }
    let body = if let Some(raw) = r["raw"].as_str() {
        request = request.header("content-type", "application/json");
        Body::from(raw.to_owned())
    } else {
        Body::empty()
    };
    let response = app.clone().oneshot(request.body(body)?).await?;
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
fn checked_generated(initial: &Value, current: &Value, before: i64, after: i64) -> Result<()> {
    for (table, field) in [
        ("audit_events", "occurred_at"),
        ("idempotency_keys", "created_at"),
    ] {
        let mut original = vec![];
        for raw in initial[table].as_array().ok_or("initial rows")? {
            let row: Value = serde_json::from_str(raw.as_str().ok_or("raw")?)?;
            original.push(row[field].clone());
        }
        for raw in current[table].as_array().ok_or("current rows")? {
            let row: Value = serde_json::from_str(raw.as_str().ok_or("raw")?)?;
            if !original.contains(&row[field]) {
                let time =
                    chrono::DateTime::parse_from_rfc3339(row[field].as_str().ok_or("clock")?)?
                        .timestamp_micros();
                assert!(before <= time && time <= after, "generated {table}.{field}");
            }
        }
    }
    Ok(())
}
#[tokio::test]
#[ignore = "requires positive exact selection and a guarded fresh migrated child"]
#[allow(clippy::too_many_lines)] // Complete independent mutation/replay/race proof.
async fn job_claims_match_production() -> Result<()> {
    let url = std::env::var("CANNERY_JOB_CLAIMS_HTTP_DATABASE_URL")?;
    let mut settings = load_settings(
        None,
        &BTreeMap::from([("CANNERY_DATABASE_URL".into(), url)]),
    )?;
    settings.leases.job_ttl_seconds = 90_i64.into();
    let (app, state) =
        application_with_job_claim_context(settings.clone(), Arc::new(profile(false)))?;
    let (fixed, fixed_state) =
        application_with_job_claim_context(settings, Arc::new(profile(true)))?;
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
    let mut token = None;
    let mut revoked = None;
    for r in f["cases"].as_array().ok_or("recipes")? {
        if !r["setup"].is_null() {
            reset(&state.pool, r).await?;
        }
        if let Some(statement) = r["between_setup"].as_str() {
            sqlx::raw_sql(statement).execute(&state.pool).await?;
        }
        let initial = storage(&state.pool, false).await?;
        let before: i64 =
            sqlx::query_scalar("SELECT (extract(epoch FROM clock_timestamp())*1000000)::bigint")
                .fetch_one(&state.pool)
                .await?;
        let mut pending_lock = if r["pending_lock"] == true {
            let mut tx = state.pool.begin().await?;
            sqlx::query(
                "SELECT id FROM jobs WHERE id='00000000-0000-0000-0000-000000006005' FOR UPDATE",
            )
            .execute(&mut *tx)
            .await?;
            Some(tx)
        } else {
            None
        };
        let (status, allow, mut response, wire) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            call(
                if r["fixed"] == true { &fixed } else { &app },
                r,
                token.as_deref(),
                revoked.as_deref(),
            ),
        )
        .await??;
        if let Some(tx) = pending_lock.take() {
            tx.rollback().await?;
        }
        let after: i64 =
            sqlx::query_scalar("SELECT (extract(epoch FROM clock_timestamp())*1000000)::bigint")
                .fetch_one(&state.pool)
                .await?;
        checked_generated(&initial, &storage(&state.pool, false).await?, before, after)?;
        if let Some(native) = r["native_profile"].as_str() {
            let expected = match native {
                "request-dto" => {
                    let raw = r["raw"].as_str().ok_or("DTO body")?;
                    let body: Value = serde_json::from_str(raw)?;
                    assert!(
                        !body.is_object()
                            || serde_json::from_value::<JobClaimRequest>(body).is_err()
                    );
                    assert_eq!(r["status"], 422);
                    json!({"error":{"code":"validation_failed","message":"request does not match the REST contract","details":null}})
                }
                "json-syntax" => {
                    let raw = r["raw"].as_str().ok_or("syntax body")?;
                    let error = serde_json::from_str::<Value>(raw)
                        .err()
                        .ok_or("invalid JSON required")?;
                    json!({"error":{"code":"validation_failed","message":"request validation failed","details":[{"path":format!("body/{}",error.column()),"message":"JSON decode error"}]}})
                }
                "header-integer" => {
                    assert_eq!(r["name"], "heartbeat-headers");
                    json!({"error":{"code":"validation_failed","message":"request validation failed","details":[{"path":"header/X-Lease-Generation","message":"Input should be a signed 64-bit integer"}]}})
                }
                "waiting-message" => {
                    assert_eq!(r["status"], 409);
                    let name = match r["role"].as_str().ok_or("worker role")? {
                        "verifier" => "fixture-verifier",
                        "other-verifier" => "other-verifier",
                        _ => return Err("unexpected waiting worker".into()),
                    };
                    let body: Value = serde_json::from_str(r["raw"].as_str().ok_or("body")?)?;
                    let revision = body["revision"].as_str().ok_or("revision")?;
                    assert!(matches!(revision, "missing" | "policy-1" | "policy-2"));
                    let message = format!(
                        "no verify job is waiting for verifier {} under policy revision {}",
                        serde_json::to_string(name)?,
                        serde_json::to_string(revision)?
                    );
                    json!({"error":{"code":"conflict","message":message,"details":null}})
                }
                "incomplete-job-document" => {
                    assert_eq!(r["legacy_spec"], "{\"inputs\":{}}");
                    assert_eq!(r["status"], 201);
                    assert!(serde_json::from_value::<JobClaimOut>(r["response"].clone()).is_err());
                    assert_eq!(status, 500);
                    assert_eq!(allow, None);
                    assert_eq!(response, Value::Null);
                    assert_eq!(wire, "496e7465726e616c20536572766572204572726f72");
                    assert_eq!(
                        storage(&state.pool, false).await?,
                        initial,
                        "malformed claimed document must roll back before commit"
                    );
                    continue;
                }
                "replay-generation-whitespace" => {
                    assert_eq!(r["name"], "malformed-replay");
                    assert_eq!(r["status"], 200);
                    assert_eq!(
                        r["between_setup"],
                        "UPDATE idempotency_keys SET result_id='00000000-0000-0000-0000-000000006005: 1 ' WHERE key='replay'"
                    );
                    assert_eq!(status, 500);
                    assert_eq!(allow, None);
                    assert_eq!(response, Value::Null);
                    assert_eq!(wire, "496e7465726e616c20536572766572204572726f72");
                    assert_eq!(storage(&state.pool, false).await?, initial);
                    continue;
                }
                _ => return Err("unknown native claim profile".into()),
            };
            assert_eq!(
                status,
                if native == "waiting-message" {
                    409
                } else {
                    422
                }
            );
            assert_eq!(allow, None);
            assert_eq!(response, expected);
            let typed: cannery_server::api_models::ErrorResponse =
                serde_json::from_value(expected)?;
            assert_eq!(wire, hex(&serde_json::to_vec(&typed)?)?);
            assert_eq!(
                storage(&state.pool, false).await?,
                initial,
                "native refusal changed raw storage"
            );
            assert_eq!(
                storage(&state.pool, true).await?,
                f["snapshots"][r["storage"].as_str().ok_or("storage")?]
            );
            continue;
        }
        assert_eq!(
            json!(status),
            r["status"],
            "status {} {response}",
            r["name"]
        );
        assert_eq!(json!(allow), r["allow"], "allow {}", r["name"]);
        if status == 200 || status == 201 {
            let claim = r["path"].as_str().ok_or("path")?.ends_with("/claims");
            let id = if claim {
                response["job"]["job_id"].as_str().ok_or("job")?
            } else {
                r["path"]
                    .as_str()
                    .ok_or("path")?
                    .split('/')
                    .nth(5)
                    .ok_or("job path")?
            };
            let (generation,held,claimed,expires,deadline,seconds):(i32,Vec<u8>,i64,i64,i64,i32)=sqlx::query_as("SELECT lease_generation,lease_token_hash,(extract(epoch FROM claimed_at)*1000000)::bigint,(extract(epoch FROM lease_expires_at)*1000000)::bigint,(extract(epoch FROM deadline)*1000000)::bigint,deadline_seconds FROM jobs WHERE id=$1::text::uuid").bind(id).fetch_one(&state.pool).await?;
            if claim {
                let t = response["job"]["lease"]["token"]
                    .as_str()
                    .ok_or("token")?
                    .to_owned();
                assert_eq!(cannery_identity::secrets::digest(&t).as_slice(), held);
                assert_eq!(response["job"]["lease"]["generation"], generation);
                assert_eq!(
                    chrono::DateTime::parse_from_rfc3339(
                        response["job"]["deadline"].as_str().ok_or("deadline")?
                    )?
                    .timestamp_micros(),
                    deadline
                );
                assert_eq!(
                    chrono::DateTime::parse_from_rfc3339(
                        response["job"]["lease"]["expires_at"]
                            .as_str()
                            .ok_or("expiry")?
                    )?
                    .timestamp_micros(),
                    expires
                );
                if status == 200 {
                    let current = storage(&state.pool, false).await?;
                    let find = |storage: &Value| -> Result<Value> {
                        for raw in storage["jobs"].as_array().ok_or("jobs")? {
                            let value: Value = serde_json::from_str(raw.as_str().ok_or("raw")?)?;
                            if value["id"] == id {
                                return Ok(value);
                            }
                        }
                        Err("job".into())
                    };
                    let old = find(&initial)?;
                    let new = find(&current)?;
                    for field in ["claimed_at", "deadline", "lease_expires_at"] {
                        assert_eq!(new[field], old[field], "replay time {field}");
                    }
                }
                revoked = token;
                token = Some(t);
                if r["fixed"] != true {
                    if status == 201 {
                        assert!(before <= claimed && claimed <= after);
                        assert_eq!(claimed + 90_000_000, expires);
                        assert_eq!(claimed + i64::from(seconds) * 1_000_000, deadline);
                    }
                    response["job"]["lease"]["token"] = json!("@checked-token");
                    response["job"]["lease"]["expires_at"] = json!("@checked-clock");
                    response["job"]["deadline"] = json!("@checked-clock");
                }
            } else {
                assert!(before + 90_000_000 <= expires && expires <= after + 90_000_000);
                assert_eq!(response["lease_generation"], generation);
                assert_eq!(
                    chrono::DateTime::parse_from_rfc3339(
                        response["deadline"].as_str().ok_or("deadline")?
                    )?
                    .timestamp_micros(),
                    deadline
                );
                assert_eq!(
                    chrono::DateTime::parse_from_rfc3339(
                        response["lease_expires_at"].as_str().ok_or("expiry")?
                    )?
                    .timestamp_micros(),
                    expires
                );
                response["lease_expires_at"] = json!("@checked-clock");
                if deadline < 3_786_912_000_000_000 {
                    response["deadline"] = json!("@checked-clock");
                }
            }
        } else {
            assert_eq!(
                storage(&state.pool, false).await?,
                initial,
                "rollback {}",
                r["name"]
            );
        }
        assert_eq!(output(response), r["response"], "response {}", r["name"]);
        if let Some(expected) = r["wire_hex"].as_str() {
            if status == 200 || status == 201 {
                let bytes = (0..expected.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&expected[i..i + 2], 16))
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let typed: JobClaimOut = serde_json::from_slice(&bytes)?;
                assert_eq!(
                    wire,
                    hex(&serde_json::to_vec(&typed)?)?,
                    "typed wire {}",
                    r["name"]
                );
            } else {
                assert_eq!(wire, expected, "wire {}", r["name"]);
            }
        }
        let stored = storage(&state.pool, true).await?;
        assert_eq!(
            stored,
            f["snapshots"][r["storage"].as_str().ok_or("storage")?],
            "raw storage {}",
            r["name"]
        );
    }
    let race_recipe = json!({"setup":"","headers":[["Idempotency-Key","race"]],"method":"POST","path":"/api/projects/matrix/jobs/claims","role":"verifier","raw":"{\"revision\":\"policy-1\"}","use_previous":false});
    reset(&state.pool, &race_recipe).await?;
    let race_initial = storage(&state.pool, false).await?;
    let race_before: i64 =
        sqlx::query_scalar("SELECT (extract(epoch FROM clock_timestamp())*1000000)::bigint")
            .fetch_one(&state.pool)
            .await?;
    let mut lock = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind("job.claim\nservice:00000000-0000-0000-0000-000000000023\nrace")
        .execute(&mut *lock)
        .await?;
    let mut tasks = vec![];
    for _ in 0..2 {
        let app = app.clone();
        let r = race_recipe.clone();
        tasks.push(tokio::spawn(
            async move { call(&app, &r, None, None).await },
        ));
    }
    tokio::time::timeout(std::time::Duration::from_secs(10),async{
        loop {let waiting:i64=sqlx::query_scalar("SELECT count(*) FROM pg_locks WHERE locktype='advisory' AND NOT granted AND database=(SELECT oid FROM pg_database WHERE datname=current_database())").fetch_one(&state.pool).await?;if waiting==2{break;}tokio::time::sleep(std::time::Duration::from_millis(10)).await;}Ok::<_,Box<dyn Error+Send+Sync>>(())
    }).await??;
    lock.commit().await?;
    let mut responses = vec![];
    let mut statuses = vec![];
    for task in tasks {
        let (status, _, response, _) = task.await??;
        statuses.push(status);
        responses.push(response);
    }
    statuses.sort_unstable();
    let race_after: i64 =
        sqlx::query_scalar("SELECT (extract(epoch FROM clock_timestamp())*1000000)::bigint")
            .fetch_one(&state.pool)
            .await?;
    checked_generated(
        &race_initial,
        &storage(&state.pool, false).await?,
        race_before,
        race_after,
    )?;
    let (claimed,expires,deadline):(i64,i64,i64)=sqlx::query_as("SELECT (extract(epoch FROM claimed_at)*1000000)::bigint,(extract(epoch FROM lease_expires_at)*1000000)::bigint,(extract(epoch FROM deadline)*1000000)::bigint FROM jobs WHERE id='00000000-0000-0000-0000-000000006005'").fetch_one(&state.pool).await?;
    assert!(race_before <= claimed && claimed <= race_after);
    assert_eq!(claimed + 90_000_000, expires);
    assert_eq!(claimed + 600_000_000, deadline);
    assert_eq!(statuses, vec![200, 201]);
    responses.sort_by_key(|r| r["job"]["lease"]["generation"].as_i64());
    assert_eq!(responses[0]["job"]["lease"]["generation"], 1);
    assert_eq!(responses[1]["job"]["lease"]["generation"], 2);
    assert_ne!(
        responses[0]["job"]["lease"]["token"],
        responses[1]["job"]["lease"]["token"]
    );
    let held: Vec<u8> = sqlx::query_scalar(
        "SELECT lease_token_hash FROM jobs WHERE id='00000000-0000-0000-0000-000000006005'",
    )
    .fetch_one(&state.pool)
    .await?;
    assert_eq!(
        held,
        cannery_identity::secrets::digest(
            responses[1]["job"]["lease"]["token"]
                .as_str()
                .ok_or("race token")?
        )
        .as_slice()
    );
    for response in &mut responses {
        response["job"]["lease"]["token"] = json!("@checked-token");
        response["job"]["lease"]["expires_at"] = json!("@checked-clock");
        response["job"]["deadline"] = json!("@checked-clock");
    }
    assert_eq!(json!(responses), f["race"]["response"]);
    assert_eq!(storage(&state.pool, true).await?, f["race"]["storage"]);
    let heartbeat = f["cases"]
        .as_array()
        .ok_or("cases")?
        .iter()
        .find(|r| r["name"] == "heartbeat-auth" && r["role"] == "verifier")
        .ok_or("heartbeat fixture")?;
    reset(&state.pool, heartbeat).await?;
    let mut lock = state.pool.begin().await?;
    sqlx::query(
        "SELECT id FROM attempts WHERE id='00000000-0000-0000-0000-000000002005' FOR UPDATE",
    )
    .execute(&mut *lock)
    .await?;
    let (status, _, _, _) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        call(&app, heartbeat, None, None),
    )
    .await??;
    assert_eq!(status, 200);
    lock.rollback().await?;
    assert_eq!(f["race"]["advisory_waiters"], 2);
    assert_eq!(f["race"]["heartbeat_ignores_attempt_lock"], true);
    state.pool.close().await;
    fixed_state.pool.close().await;
    Ok(())
}
