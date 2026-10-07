//! Actual source mutation corpus, fixed pre-write identities, real clock checks.
#![forbid(unsafe_code)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use cannery_core::{contracts::ContractValidator, json, settings::load_settings};
use cannery_server::{
    application_with_hypothesis_context, hypothesis_mutations::MutationContext,
    hypothesis_routes::HypothesisContext, hypothesis_wire::ResponseContext,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::fmt::Write;
use std::{error::Error, sync::Arc};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
fn parse_value(bytes: &[u8]) -> Result<Value> {
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    decoder.disable_recursion_limit();
    Ok(<Value as serde::Deserialize>::deserialize(&mut decoder)?)
}
async fn storage_json(pool: &PgPool, statement: &str) -> Result<Value> {
    let value: String = sqlx::query_scalar(statement).fetch_one(pool).await?;
    parse_value(value.as_bytes())
}
// Psycopg JSON loading renders finite floats using Python's binary64 spelling.
// SQLx preserves the original PG JSON numeric lexeme. Compare these decoded
// values numerically, while the independent ::text fields remain byte-exact.
fn same(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => {
            let a_text = a.to_string();
            let b_text = b.to_string();
            if a_text.contains(['.', 'e', 'E']) && b_text.contains(['.', 'e', 'E']) {
                return a
                    .as_f64()
                    .zip(b.as_f64())
                    .is_some_and(|(a, b)| a.to_bits() == b.to_bits());
            }
            a == b
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| same(a, b)))
        }
        _ => actual == expected,
    }
}
fn difference(actual: &Value, expected: &Value, path: &str) -> Value {
    match (actual, expected) {
        (Value::Object(a), Value::Object(b)) => {
            for (key, value) in a {
                if b.get(key) != Some(value) {
                    return difference(
                        value,
                        b.get(key).unwrap_or(&Value::Null),
                        &format!("{path}/{key}"),
                    );
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            for (index, value) in a.iter().enumerate() {
                if b.get(index) != Some(value) {
                    return difference(
                        value,
                        b.get(index).unwrap_or(&Value::Null),
                        &format!("{path}/{index}"),
                    );
                }
            }
        }
        _ => {}
    }
    json!({"path":path,"actual":actual,"expected":expected})
}
fn profile() -> Result<HypothesisContext> {
    Ok(HypothesisContext {
        mutations: Some(Arc::new(MutationContext {
            science: cannery_research::science::RenderingContext {
                nesting_budget: 9992,
            },
            config: cannery_research::config_repo::JsonContext {
                encode_nesting_budget: 9994,
                decode_nesting_budget: 9994,
            },
            tracks: cannery_tracks::repo::JsonContext {
                encode_nesting_budget: 9994,
                decode_nesting_budget: 9994,
            },
            mention_walk_budget: 992,
        })),
        contracts: ContractValidator::new()?,
        validation_walk_budget: 965,
        repr_budget: 9992,

        repository: cannery_hypotheses::repo::JsonContext {
            encode_nesting_budget: 9994,
            decode_nesting_budget: 9994,
        },
        response: ResponseContext {
            inferred_nesting_budget: 255,
        },
        request_hash_budget: 968,
    })
}
fn clock(v: Value) -> Value {
    if let Value::String(s) = &v
        && chrono::DateTime::parse_from_rfc3339(&s.replace(' ', "T"))
            .or_else(|_| chrono::DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f%#z"))
            .is_ok_and(|v| chrono::Datelike::year(&v) >= 2026)
    {
        json!("@checked-clock")
    } else {
        v
    }
}
fn projection(v: Value) -> Value {
    let Value::Object(mut map) = v else {
        return v;
    };
    if let Some(e) = map.get("error") {
        return json!({"error":{"code":e["code"],"details":e["details"].as_array().map(|v|v.iter().map(|v|json!({"path":v["path"]})).collect::<Vec<_>>())}});
    }
    for key in [
        "created_at",
        "updated_at",
        "opened_at",
        "resolved_at",
        "occurred_at",
    ] {
        if let Some(v) = map.get_mut(key) {
            *v = clock(v.take());
        }
    }
    if let Some(Value::Array(reviews)) = map.get_mut("reviews") {
        for review in reviews {
            *review = projection(review.take());
        }
    }
    Value::Object(map)
}
fn raw_body(recipe: &Value) -> Result<Vec<u8>> {
    recipe["raw_hex"]
        .as_str()
        .ok_or("raw")?
        .as_bytes()
        .chunks(2)
        .map(|v| u8::from_str_radix(std::str::from_utf8(v)?, 16).map_err(Into::into))
        .collect()
}
fn native_wire_error(recipe: &Value) -> Result<Option<Value>> {
    let raw = raw_body(recipe)?;
    let native_error = if let Some(profile) = recipe["native_wire_profile"].as_str() {
        assert!(match profile {
            "unpaired-surrogate" => raw.windows(6).any(|v| v == br"\ud800"),
            "nonfinite-json" => raw.windows(3).any(|v| v == b"NaN"),
            _ => return Err("unknown native wire profile".into()),
        });
        let Err(json::DecodeError::Syntax { position }) =
            json::decode(&raw, cannery_server::body::REST_JSON_NESTING_BUDGET)
        else {
            return Err("native invalid wire profile must fail syntax validation".into());
        };
        Some(
            json!({"error":{"code":"validation_failed","message":"request validation failed",
            "details":[{"path":format!("body/{position}"),"message":"JSON decode error"}]}}),
        )
    } else {
        None
    };
    Ok(native_error)
}
fn native_validation_error(recipe: &Value) -> Result<Option<Value>> {
    let id = recipe["id"].as_str().ok_or("recipe id")?;
    let error = match id {
        "create-role-researcher-7"
        | "create-role-agent-22"
        | "create-role-otheragent-28"
        | "core-budget_unknown_key"
        | "core-empty_compute_budget"
        | "core-project_fields_not_object"
        | "core-relation_number_zero"
        | "core-server_assigned_number"
        | "core-server_assigned_state"
        | "core-unknown_plan_field"
        | "core-unknown_relation_kind"
        | "core-whitespace_question"
        | "core-wrong_schema_version" => {
            assert_eq!(recipe["status"], 422);
            assert!(
                recipe["output"]["error"]["details"]
                    .as_array()
                    .ok_or("source violations")?
                    .iter()
                    .all(|v| v["path"] == "")
            );
            json!({"error":{"code":"validation_failed","message":"invalid hypothesis",
                "details":[{"path":"","message":"invalid project field"}]}})
        }
        "fields-67" => {
            assert_eq!(recipe["status"], 500);
            assert_eq!(
                recipe["schema"],
                json!({"properties":{"x":{"format":"uri"}}})
            );
            json!({"error":{"code":"validation_failed","message":"invalid project fields",
                "details":[{"path":"/project_fields/x","message":"invalid project field"}]}})
        }
        "revise-path-bad" | "update-body-96" | "update-body-97" | "update-body-98"
        | "update-body-99" | "update-body-100" | "update-body-101" | "update-body-102"
        | "update-body-103" | "update-body-104" => {
            assert_eq!(recipe["method"], "PUT");
            json!({"error":{"code":"validation_failed","message":"request does not match the REST contract","details":null}})
        }
        _ => return Ok(None),
    };
    Ok(Some(error))
}
fn exceeds_native_body_limit(recipe: &Value) -> Result<bool> {
    let raw = raw_body(recipe)?;
    Ok(raw.len() > cannery_server::body::REST_BODY_MAX_BYTES
        || matches!(
            json::decode(&raw, cannery_server::body::REST_JSON_NESTING_BUDGET),
            Err(json::DecodeError::Recursion)
        ))
}
async fn call(app: &Router, recipe: &Value) -> Result<(u16, Value, String, Option<String>)> {
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
    let raw = raw_body(recipe)?;
    let native_error = native_wire_error(recipe)?;
    let validation_error = native_validation_error(recipe)?;
    let before = chrono::Utc::now();
    let response = app
        .clone()
        .oneshot(
            request
                .header("content-type", "application/json")
                .body(Body::from(raw))?,
        )
        .await?;
    let after = chrono::Utc::now();
    let status = response.status().as_u16();
    let allow = response
        .headers()
        .get("allow")
        .map(|value| value.to_str().map(str::to_owned))
        .transpose()?;
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let output = if status == 500 {
        assert_eq!(&bytes[..], b"Internal Server Error");
        Value::Null
    } else if bytes.is_empty() {
        Value::Null
    } else {
        parse_value(&bytes)?
    };
    if let Some(expected) = native_error {
        assert_eq!(status, 422);
        assert_eq!(output, expected, "complete native syntax envelope");
    }
    if let Some(expected) = validation_error {
        assert_eq!(status, 422);
        assert_eq!(
            output, expected,
            "complete authored native validation envelope"
        );
    }
    if status == 201 {
        let instant = chrono::DateTime::parse_from_rfc3339(
            output["created_at"].as_str().ok_or("created clock")?,
        )?;
        assert!(instant >= before && instant <= after);
    }
    if status == 200 && recipe["method"] == "PUT" {
        let instant = chrono::DateTime::parse_from_rfc3339(
            output["updated_at"].as_str().ok_or("revision clock")?,
        )?;
        assert!(instant >= before && instant <= after);
    }
    if matches!(status, 200 | 201) && output["number"].as_i64().is_some_and(|n| n >= 30) {
        assert_eq!(
            output["id"],
            format!(
                "40000000-0000-4000-8000-{:012}",
                output["number"].as_i64().ok_or("number")?
            )
        );
    }
    let mut hex = String::new();
    for byte in bytes {
        write!(&mut hex, "{byte:02x}")?;
    }
    Ok((status, projection(output), hex, allow))
}
async fn storage_rows(pool: &PgPool) -> Result<Value> {
    let mut output = json!({});
    for (table, order) in [
        ("hypotheses", "project_id,number"),
        ("hypothesis_revisions", "hypothesis_id,revision"),
        ("hypothesis_relations", "hypothesis_id,kind,target_id"),
        ("mentions", "source_type,source_id,target_id"),
        ("review_cases", "hypothesis_id,subject_revision,id"),
        ("audit_events", "seq"),
        ("idempotency_keys", "scope,actor,key"),
    ] {
        let rows = storage_json(
            pool,
            &format!(
                "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY {order}),'[]')::text FROM {table} t"
            ),
        )
        .await?;
        output[table] = rows;
    }
    Ok(output)
}
async fn storage(pool: &PgPool) -> Result<Value> {
    let mut output = storage_rows(pool).await?;
    for rows in output.as_object_mut().ok_or("storage tables")?.values_mut() {
        for row in rows.as_array_mut().ok_or("rows")? {
            for key in [
                "created_at",
                "updated_at",
                "opened_at",
                "resolved_at",
                "occurred_at",
            ] {
                if let Some(v) = row.get_mut(key) {
                    *v = clock(v.take());
                }
            }
        }
    }
    output["jsonb_text"]=storage_json(pool,"SELECT coalesce(jsonb_agg(jsonb_build_array(hypothesis_id::text,revision,content::text) ORDER BY hypothesis_id,revision),'[]')::text FROM hypothesis_revisions").await?;
    output["audit_jsonb_text"]=storage_json(pool,"SELECT coalesce(jsonb_agg(jsonb_build_array(seq,prior_state::text,new_state::text) ORDER BY seq),'[]')::text FROM audit_events").await?;
    output["clock_relations"]=storage_json(pool,"SELECT coalesce(jsonb_agg(jsonb_build_array(a.seq,r.created_at=a.occurred_at,c.opened_at<=a.occurred_at,(h.revision<>r.revision OR h.updated_at=a.occurred_at)) ORDER BY a.seq),'[]')::text FROM audit_events a JOIN hypotheses h ON h.id::text=a.subject_id JOIN hypothesis_revisions r ON r.hypothesis_id=h.id AND r.revision=(a.new_state->>'revision')::int JOIN review_cases c ON c.hypothesis_id=h.id AND c.subject_revision=r.revision AND c.kind='draft' WHERE a.action IN ('hypothesis.draft_created','hypothesis.draft_revised')").await?;
    for row in output["clock_relations"].as_array().ok_or("clock rows")? {
        assert!(
            row.as_array().ok_or("clock row")?[1..]
                .iter()
                .all(|v| v == true)
        );
    }
    Ok(output)
}
#[test]
#[ignore = "Requires a newly Rust-migrated guarded database from the reference launcher"]
fn hypothesis_mutations_match_production() -> Result<()> {
    // The source corpus contains deep JSON deliberately beyond serde's default
    // recursion limit. This is test-oracle storage, not a production decoder.
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(compare_mutations())
        })?
        .join()
        .map_err(|_| "mutation comparison worker failed")?
}
// Keep replay order visible: later cases intentionally reuse earlier writes.
#[allow(clippy::too_many_lines)]
async fn compare_mutations() -> Result<()> {
    let mut decoder = serde_json::Deserializer::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/hypothesis_mutations/reference.json"
    ));
    decoder.disable_recursion_limit();
    let reference = <Value as serde::Deserialize>::deserialize(&mut decoder)?;
    let url = std::env::var("CANNERY_HYPOTHESIS_MUTATION_DATABASE_URL")?;
    let settings = load_settings(
        None,
        &std::collections::BTreeMap::from([("CANNERY_DATABASE_URL".into(), url)]),
    )?;
    let (app, state) = application_with_hypothesis_context(settings, Arc::new(profile()?))?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("isolated database required")?;
    if suffix.len() != 24
        || !suffix
            .bytes()
            .all(|v| v.is_ascii_hexdigit() && !v.is_ascii_uppercase())
    {
        return Err("isolated database required".into());
    }
    sqlx::raw_sql(include_str!("fixtures/hypotheses_http/seed.sql"))
        .execute(&state.pool)
        .await?;
    sqlx::raw_sql(include_str!("fixtures/hypothesis_mutations/seed.sql"))
        .execute(&state.pool)
        .await?;
    let cases = reference["cases"].as_array().ok_or("cases")?;
    assert_eq!(
        cases
            .iter()
            .filter(|r| r["native_wire_profile"].is_string())
            .count(),
        4,
        "corpus must retain three surrogate refusals and one nonfinite JSON refusal"
    );
    assert_eq!(cases.len(), 119);
    let mut failures = vec![];
    let mut native_limited = 0;
    // The source's deep frontier commits precede response-model failures.
    // Native body refusals never enter that transaction. Once they diverge,
    // retain the last fully matched snapshot for the terminal method checks.
    let mut body_limit_storage: Option<(Value, Value)> = None;
    for (index, case) in cases.iter().enumerate() {
        if case["race"] == true {
            assert!(
                body_limit_storage.is_none(),
                "race must precede body-limit divergence"
            );
            let mut connection = state.pool.acquire().await?;
            let mut transaction = sqlx::Acquire::begin(&mut *connection).await?;
            if case["method"] == "POST" {
                sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
                    .bind("hypothesis.create\nuser:00000000-0000-0000-0000-000000000002\nconcurrent-create")
                    .execute(&mut *transaction).await?;
            } else {
                let number = case["path"]
                    .as_str()
                    .ok_or("race path")?
                    .rsplit('/')
                    .next()
                    .ok_or("race number")?
                    .parse::<i32>()?;
                sqlx::query("SELECT id FROM hypotheses WHERE project_id='00000000-0000-0000-0000-000000000010' AND number=$1 FOR UPDATE")
                    .bind(number).fetch_one(&mut *transaction).await?;
            }
            let first_app = app.clone();
            let first_case = case.clone();
            let first = tokio::spawn(async move { call(&first_app, &first_case).await });
            let second_app = app.clone();
            let second_case = case.clone();
            let second = tokio::spawn(async move { call(&second_app, &second_case).await });
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                assert!(
                    !first.is_finished() && !second.is_finished(),
                    "race client finished before lock proof"
                );
                let waiters: i64 = tokio::time::timeout_at(deadline, sqlx::query_scalar(
                    "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock'",
                ).fetch_one(&state.pool)).await.map_err(|_| "lock monitor timed out")??;
                if waiters == 2 {
                    assert_eq!(case["observed_lock_waiters"], waiters);
                    println!("{}: observed two database lock waiters", case["id"]);
                    break;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "two database lock waiters not observed"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert!(!first.is_finished() && !second.is_finished());
            transaction.commit().await?;
            let first = first.await??;
            let second = second.await??;
            let mut statuses = [first.0, second.0];
            statuses.sort_unstable();
            let mut outputs = vec![first.1, second.1];
            // Mapping order is irrelevant for this unordered concurrent pair;
            // compare each complete response, retaining all array ordering.
            let expected = case["outputs"].as_array().ok_or("race outputs")?;
            if statuses
                .iter()
                .copied()
                .map(Value::from)
                .collect::<Vec<_>>()
                != *case["statuses"].as_array().ok_or("race statuses")?
                || !expected.iter().all(|value| {
                    outputs
                        .iter()
                        .position(|v| same(v, value))
                        .map(|i| {
                            outputs.remove(i);
                        })
                        .is_some()
                })
                || !outputs.is_empty()
            {
                failures.push(json!({"index":index,"kind":"race"}));
            }
            let actual = storage(&state.pool).await?;
            if !same(&actual, &case["storage"]) {
                failures.push(json!({"index":index,"kind":"race-storage","actual":actual}));
            }
            continue;
        }
        if case["schema_change"] == true {
            let schema = if case["schema"].is_null() {
                None
            } else {
                Some(serde_json::to_string(&case["schema"])?)
            };
            sqlx::query("INSERT INTO config_revisions(project_id,kind,revision,content,created_by,created_at) SELECT project_id,kind,revision+1,CASE WHEN $1::text::jsonb IS NULL THEN content-'hypothesis_fields' ELSE jsonb_set(content,'{hypothesis_fields}',$2::text::jsonb) END,created_by,'2001-01-01Z' FROM config_revisions WHERE project_id='00000000-0000-0000-0000-000000000010' AND kind='science' ORDER BY revision DESC LIMIT 1").bind(&schema).bind(&schema).execute(&state.pool).await?;
        }
        let body_limited = exceeds_native_body_limit(case)?;
        let native_wire =
            case["native_wire_profile"].is_string() || native_validation_error(case)?.is_some();
        let wire_before = if native_wire {
            Some(storage_rows(&state.pool).await?)
        } else {
            None
        };
        let before = if body_limited {
            Some((
                storage(&state.pool).await?,
                storage_rows(&state.pool).await?,
            ))
        } else {
            None
        };
        let (status, output, wire, allow) = call(&app, case).await?;
        if json!(allow) != case["allow"] {
            failures.push(json!({"index":index,"id":case["id"],"kind":"allow","actual":allow}));
        }
        if let Some((before, raw_before)) = before {
            let actual = storage(&state.pool).await?;
            let raw_actual = storage_rows(&state.pool).await?;
            let expected_bytes = br#"{"detail":"There was an error parsing the body"}"#;
            let mut expected_wire = String::new();
            for byte in expected_bytes {
                write!(&mut expected_wire, "{byte:02x}")?;
            }
            if status != 400
                || output != json!({"detail":"There was an error parsing the body"})
                || wire != expected_wire
                || !same(&actual, &before)
                || raw_actual != raw_before
            {
                failures.push(
                    json!({"index":index,"id":case["id"],"kind":"native-body-limit",
                    "actual_status":status,"output":output,"exact_wire":wire==expected_wire,
                    "storage_unchanged":same(&actual,&before),"raw_rows_unchanged":raw_actual==raw_before}),
                );
            }
            // A schema fixture may change between requests. Each refusal proves
            // its own pre-request rows unchanged; terminal method refusals use
            // the latest state after those explicit fixture changes.
            body_limit_storage = Some((before, raw_before));
            native_limited += 1;
            continue;
        }
        if let Some(before) = wire_before {
            assert_eq!(status, 422);
            assert_eq!(allow, None);
            assert_eq!(
                storage_rows(&state.pool).await?,
                before,
                "native rejection must not write any row"
            );
            assert!(
                same(&storage(&state.pool).await?, &case["storage"]),
                "source invalid-body storage remains equivalent"
            );
            continue;
        }
        if json!(status) != case["status"] || !same(&output, &case["output"]) {
            failures.push(json!({"index":index,"id":case["id"],"kind":"response","actual_status":status,"output":output}));
        }
        if case.get("wire_hex").is_some_and(|v| v != &json!(wire)) {
            failures.push(json!({"index":index,"kind":"wire"}));
        }
        let actual = storage(&state.pool).await?;
        let expected_storage = if let Some((baseline, raw_baseline)) = &body_limit_storage {
            assert!(
                matches!(
                    case["method"].as_str(),
                    Some("HEAD" | "DELETE" | "PATCH" | "OPTIONS")
                ),
                "later mutation needs its own source-equivalent fixture state"
            );
            assert_eq!(
                case["status"], 405,
                "only terminal method refusals follow the frontier"
            );
            if storage_rows(&state.pool).await? != *raw_baseline {
                failures.push(json!({"index":index,"id":case["id"],"kind":"method-raw-storage"}));
            }
            baseline
        } else {
            &case["storage"]
        };
        if !same(&actual, expected_storage) {
            failures.push(json!({"index":index,"id":case["id"],"kind":"storage","first_difference":difference(&actual,expected_storage,""),"actual":actual}));
        }
    }
    // Exercise the application byte limit independently of the removed
    // interpreter-depth calibration recipes, including exact refusal and rows.
    let raw_before = storage_rows(&state.pool).await?;
    let oversized = "x".repeat(cannery_server::body::REST_BODY_MAX_BYTES + 1);
    let mut body_hex = String::with_capacity(oversized.len() * 2);
    for byte in oversized.bytes() {
        write!(&mut body_hex, "{byte:02x}")?;
    }
    let limit_probe = json!({"id":"native-body-byte-limit","method":"POST","path":"/api/projects/matrix/hypotheses","role":"none","raw_hex":body_hex});
    let (status, output, _, allow) = call(&app, &limit_probe).await?;
    assert_eq!(status, 400);
    assert_eq!(
        output,
        json!({"detail":"There was an error parsing the body"})
    );
    assert_eq!(allow, None);
    assert_eq!(storage_rows(&state.pool).await?, raw_before);
    native_limited += 1;
    assert!(
        native_limited > 0,
        "corpus must prove native body-limit refusal"
    );
    println!(
        "checked {} reference cases and {native_limited} bounded native refusals",
        cases.len()
    );
    state.pool.close().await;
    std::fs::write(
        "target/hypothesis-mutation-failures.json",
        serde_json::to_vec_pretty(&failures)?,
    )?;
    assert!(
        failures.is_empty(),
        "{} mismatches; private synthetic diagnostics in target/hypothesis-mutation-failures.json",
        failures.len()
    );
    Ok(())
}
