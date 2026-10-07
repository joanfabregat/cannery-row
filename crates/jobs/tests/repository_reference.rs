#![forbid(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic)]

#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::{
    ids::{AttemptId, JobId, ProjectId, ServiceAccountId},
    json::{self, Document, DocumentBuilder, Node},
    principal::{Channel, Scope, ServiceKind, ServicePrincipal, Via},
    timestamps::Timestamp,
};
use cannery_jobs::repo::{
    self, EvidenceId, Failure, Job, JobError, JsonContext, ManifestId, NewJob, Origin, Stage,
};
use num_bigint::BigInt;
use serde_json::{Value, json as value};
use sqlx::{Connection, PgConnection, Row};
use std::{collections::BTreeSet, error::Error};
use uuid::Uuid;

const CONTEXT: JsonContext = JsonContext {
    encode_nesting_budget: 64,
    decode_nesting_budget: 64,
};
const SEED: &str = include_str!("fixtures/seed.sql");

fn id(number: u64) -> Uuid {
    format!("00000000-0000-0000-0000-{number:012}")
        .parse()
        .expect("fixture UUID")
}
fn number(recipe: &Value, key: &str, default: Option<i64>) -> Option<BigInt> {
    if let Some(digits) = recipe.get(format!("{key}_nines")).and_then(Value::as_u64) {
        return Some(BigInt::from(10).pow(u32::try_from(digits).expect("fixture exponent")) - 1);
    }
    if let Some(power) = recipe.get(format!("{key}_power")).and_then(Value::as_u64) {
        let integer = BigInt::from(10).pow(u32::try_from(power).expect("fixture exponent"));
        return Some(if recipe["negative"] == true {
            -integer
        } else {
            integer
        });
    }
    match recipe.get(key) {
        Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.parse().expect("fixture integer")),
        Some(value) => Some(value.as_i64().expect("fixture integer").into()),
        None => default.map(BigInt::from),
    }
}
fn index(recipe: &Value, key: &str, default: u64) -> Uuid {
    id(recipe[key].as_u64().unwrap_or(default))
}
fn text<'a>(recipe: &'a Value, key: &str, default: &'a str) -> &'a str {
    recipe[key].as_str().unwrap_or(default)
}
fn stage(recipe: &Value) -> Stage {
    if recipe["stage"] == "evaluator" {
        Stage::Evaluator
    } else {
        Stage::Tester
    }
}
fn principal(recipe: &Value) -> ServicePrincipal {
    ServicePrincipal {
        service_account_id: ServiceAccountId(id(3)),
        project_id: ProjectId(id(if recipe["foreign_principal"] == true {
            0
        } else {
            2
        })),
        kind: if recipe["foreign_principal"] == true {
            ServiceKind::Agent
        } else {
            ServiceKind::Tester
        },
        name: "tester".into(),
        via: Via {
            channel: Channel::Cli,
            client: Some(text(recipe, "client", "fixture").into()),
        },
        scopes: if recipe["foreign_principal"] == true {
            BTreeSet::new()
        } else {
            BTreeSet::from([Scope::Read, Scope::Write])
        },
    }
}
fn document(recipe: &Value) -> Document {
    if let Some(power) = recipe["payload_power"].as_u64() {
        let mut builder = DocumentBuilder::new();
        let integer = builder
            .push(Node::Integer(
                BigInt::from(10).pow(u32::try_from(power).expect("fixture power")),
            ))
            .expect("integer node");
        let root = builder
            .push(Node::Object(vec![(String::from("n"), integer)]))
            .expect("object node");
        return builder.finish(root).expect("fixture document");
    }
    if let Some(depth) = recipe["payload_depth"].as_u64() {
        let mut builder = DocumentBuilder::new();
        let mut root = builder.push(Node::Integer(0.into())).expect("integer node");
        for _ in 0..depth {
            root = builder.push(Node::Array(vec![root])).expect("array node");
        }
        return builder.finish(root).expect("fixture document");
    }
    json::decode(text(recipe, "payload", r#"{"value":1}"#).as_bytes(), 64)
        .expect("fixture document")
}

async fn projection(conn: &mut PgConnection, job: Option<Job>) -> Result<Value, JobError> {
    let Some(job) = job else {
        return Ok(Value::Null);
    };
    assert_eq!(format!("{job:?}"), "Job([redacted])");
    let row = sqlx::query("SELECT created_at=now(),claimed_at=now(),finished_at=now(),extract(epoch FROM deadline-now())::text,extract(epoch FROM lease_expires_at-now())::text,spec::text,logs::text FROM jobs WHERE id=$1")
        .bind(job.id).fetch_one(&mut *conn).await.map_err(|_|JobError::CorruptData)?;
    let now: Timestamp = sqlx::query_scalar("SELECT now()")
        .fetch_one(&mut *conn)
        .await
        .map_err(|_| JobError::CorruptData)?;
    let delta = |timestamp: Option<Timestamp>| {
        timestamp.map(|timestamp| {
            let micros = timestamp.0.timestamp_micros() - now.0.timestamp_micros();
            format!(
                "{}{}.{:06}",
                if micros < 0 { "-" } else { "" },
                micros.unsigned_abs() / 1_000_000,
                micros.unsigned_abs() % 1_000_000
            )
        })
    };
    let clock = value!([
        job.created_at == now,
        job.claimed_at.map(|timestamp| timestamp == now),
        job.finished_at.map(|timestamp| timestamp == now),
        delta(job.deadline),
        delta(job.lease_expires_at)
    ]);
    assert_eq!(
        clock,
        value!([
            row.get::<bool, _>(0),
            row.get::<Option<bool>, _>(1),
            row.get::<Option<bool>, _>(2),
            row.get::<Option<String>, _>(3),
            row.get::<Option<String>, _>(4)
        ]),
        "native decoded clock differs from stored clock"
    );
    let hash = job.lease_token_hash.as_ref().map(|bytes| {
        if bytes == &vec![7; 32] {
            "seed"
        } else if bytes == &vec![8; 32] {
            "new"
        } else {
            "unexpected"
        }
    });
    let encoded =
        |document: &Document| json::encode_ascii_pretty(document, 64).expect("fixture encoding");
    Ok(value!({
        "id":job.id.to_string(),"project_id":job.project_id.to_string(),"attempt_id":job.attempt_id.to_string(),
        "stage":job.stage.as_str(),"run_number":job.run_number,"state":job.state.as_str(),
        "science_revision":job.science_revision,"tester_id":job.tester_id,"spec":encoded(&job.spec),
        "deadline_seconds":job.deadline_seconds,"claimed_by_service":job.claimed_by_service.map(|id|id.to_string()),
        "via_channel":job.via_channel,"via_client":job.via_client,"lease_generation":job.lease_generation,
        "lease_token_hash":hash,"evidence_id":job.evidence_id.map(|id|id.0.to_string()),
        "manifest_id":job.manifest_id.map(|id|id.0.to_string()),"error_step":job.error_step,
        "error_code":job.error_code,"error_reason":job.error_reason,"logs":encoded(&job.logs),
        "origin":job.origin.as_str(),"previous_run_id":job.previous_run_id.map(|id|id.to_string()),
        "clock":clock,
        "storage_json":[row.get::<String,_>(5),row.get::<String,_>(6)]
    }))
}

// One-to-one recipe dispatcher keeps all source calls in the same mapping.
#[allow(clippy::too_many_lines)]
async fn operation(conn: &mut PgConnection, recipe: &Value) -> Result<Value, JobError> {
    let action = text(recipe, "action", "");
    let target = JobId(index(
        recipe,
        "target",
        if ["rotate", "extend", "complete", "fail"].contains(&action) {
            109
        } else {
            106
        },
    ));
    let hash = vec![if recipe["seed_hash"] == true { 7 } else { 8 }; 32];
    match action {
        "create" => {
            let science = number(recipe, "science", Some(1)).expect("science");
            let deadline = number(recipe, "deadline", Some(600)).expect("deadline");
            let payload = document(recipe);
            let origin = match text(recipe, "origin", "submission") {
                "auto_retry" => Origin::AutoRetry,
                "human_retry" => Origin::HumanRetry,
                _ => Origin::Submission,
            };
            let row = repo::create_job(
                conn,
                NewJob {
                    id: JobId(id(110)),
                    project_id: ProjectId(index(recipe, "project", 2)),
                    attempt_id: AttemptId(index(recipe, "attempt", 23)),
                    stage: Stage::Tester,
                    science_revision: &science,
                    tester_id: text(recipe, "tester", "fixture"),
                    spec: &payload,
                    deadline_seconds: &deadline,
                    origin,
                    previous_run_id: recipe["previous"].as_u64().map(|n| JobId(id(n))),
                },
                CONTEXT,
            )
            .await?;
            projection(conn, Some(row)).await
        }
        "get" | "get_locked" => {
            let row = repo::get_job(conn, target, action == "get_locked", CONTEXT).await?;
            projection(conn, row).await
        }
        "latest" => {
            let row = repo::latest_job(
                conn,
                AttemptId(index(recipe, "target", 21)),
                stage(recipe),
                CONTEXT,
            )
            .await?;
            projection(conn, row).await
        }
        "automatic" => Ok(value!(
            repo::automatic_reruns(conn, AttemptId(index(recipe, "target", 21)), stage(recipe))
                .await?
        )),
        "sum" => Ok(value!(
            repo::committed_output_bytes(conn, target)
                .await?
                .to_string()
        )),
        "list" => {
            let limit = number(recipe, "limit", None);
            let rows = repo::list_jobs(
                conn,
                AttemptId(id(21)),
                recipe["after"].as_u64().map(|n| JobId(id(n))),
                limit.as_ref(),
                CONTEXT,
            )
            .await?;
            let mut values = Vec::new();
            for row in rows {
                values.push(projection(conn, Some(row)).await?);
            }
            Ok(value!(values))
        }
        "expired" => {
            let limit = number(recipe, "limit", None);
            let exclude = recipe["exclude"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .map(|n| JobId(id(n.as_u64().expect("exclude ID"))))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Ok(value!(
                repo::expired_claims(conn, limit.as_ref(), &exclude)
                    .await?
                    .into_iter()
                    .map(|(a, b)| [a.to_string(), b.to_string()])
                    .collect::<Vec<_>>()
            ))
        }
        "pick" => Ok(value!(
            repo::pick_pending(
                conn,
                ProjectId(id(2)),
                stage(recipe),
                text(recipe, "tester", "fixture"),
                recipe["revision"].as_str()
            )
            .await?
            .map(|id| id.to_string())
        )),
        "claim" => {
            let ttl = number(recipe, "ttl", Some(60)).expect("TTL");
            let row =
                repo::claim_job(conn, target, &principal(recipe), &hash, &ttl, CONTEXT).await?;
            projection(conn, Some(row)).await
        }
        "rotate" => {
            let row = repo::rotate_token(conn, target, &hash, CONTEXT).await?;
            projection(conn, Some(row)).await
        }
        "extend" => {
            let ttl = number(recipe, "ttl", Some(60)).expect("TTL");
            let row = repo::extend_lease(conn, target, &ttl, CONTEXT).await?;
            projection(conn, Some(row)).await
        }
        "complete" => {
            let row = repo::complete_job(
                conn,
                target,
                EvidenceId(index(recipe, "evidence", 32)),
                if recipe["manifest"] == 0 {
                    None
                } else {
                    Some(ManifestId(index(recipe, "manifest", 31)))
                },
                CONTEXT,
            )
            .await?;
            projection(conn, Some(row)).await
        }
        "fail" => {
            let logs = document(recipe);
            let row = repo::fail_job(
                conn,
                target,
                Failure {
                    step: Some("step"),
                    code: text(recipe, "code", "fixture_failure"),
                    reason: "é reason",
                    logs: &logs,
                },
                CONTEXT,
            )
            .await?;
            projection(conn, Some(row)).await
        }
        _ => panic!("unknown fixture operation"),
    }
}

async fn observe(conn: &mut PgConnection, recipe: &Value) -> Result<Value, Box<dyn Error>> {
    let mut seed = if recipe["null_spec"] == true {
        SEED.replace(
            r#"{"evaluator":{"revision":"r1"},"label":"é😀","float":1.0}"#,
            "null",
        )
    } else {
        SEED.into()
    };
    if let Some(power) = recipe["stored_power"].as_u64() {
        seed = seed.replace(
            r#"{"evaluator":{"revision":"r1"},"label":"é😀","float":1.0}"#,
            &format!(
                "{{\"n\":1{}}}",
                "0".repeat(usize::try_from(power).expect("fixture power"))
            ),
        );
    }
    if recipe["auto_pending"] == true {
        seed = seed
            .replace("i IN(2,3,5)", "i IN(2,3,5,6)")
            .replace("i IN(2,3,4,5)", "i IN(2,3,4,5,6)");
    }
    sqlx::raw_sql(&seed).execute(&mut *conn).await?;
    if recipe["disabled"] == true {
        sqlx::query("UPDATE service_accounts SET disabled_at=now() WHERE id=$1")
            .bind(id(3))
            .execute(&mut *conn)
            .await?;
    }
    if recipe["claim_first"] == true {
        repo::claim_job(
            conn,
            JobId(index(recipe, "target", 106)),
            &principal(recipe),
            &[6; 32],
            &60.into(),
            CONTEXT,
        )
        .await?;
    }
    let before_checked_refusal = if checked_ttl_refusal(recipe) {
        Some(remaining_jobs(conn).await?)
    } else {
        None
    };
    sqlx::query("SAVEPOINT operation")
        .execute(&mut *conn)
        .await?;
    let result = operation(conn, recipe).await;
    if result.is_err() {
        sqlx::query("ROLLBACK TO operation")
            .execute(&mut *conn)
            .await?;
    }
    let mut outcome = match result {
        Ok(value) => value!({"value":value}),
        Err(JobError::Database { sqlstate }) => value!({"sqlstate":sqlstate}),
        Err(JobError::Invariant) => value!({"error":"Invariant"}),
        Err(JobError::StaleLease) => value!({"error":"StaleLease"}),
        Err(
            JobError::Encode(json::EncodeError::IntegerLimit)
            | JobError::Decode(json::DecodeError::IntegerLimit),
        ) => value!({"error":"ValueError"}),
        Err(error) => panic!("unexpected sanitized job error: {error}"),
    };
    outcome["remaining"] = remaining_jobs(conn).await?;
    if let Some(before) = before_checked_refusal {
        assert_eq!(outcome["sqlstate"], Value::Null, "checked native TTL error");
        assert!(outcome.get("value").is_none());
        assert_eq!(
            outcome["remaining"], before,
            "native TTL refusal must not modify any job"
        );
    }
    Ok(outcome)
}

fn url() -> String {
    let url =
        std::env::var("CANNERY_JOBS_TEST_DATABASE_URL").expect("isolated job database required");
    let options: sqlx::postgres::PgConnectOptions =
        url.parse().expect("invalid isolated database URI");
    let name = options.get_database().unwrap_or("");
    assert!(
        name.starts_with("conformance_")
            && name.len() == 36
            && name[12..]
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "requires guarded PostgreSQL URI"
    );
    url
}

async fn concurrency(url: &str) -> Result<Value, Box<dyn Error>> {
    let mut first = PgConnection::connect(url)
        .await
        .map_err(|_| "fixture connection failed")?;
    let mut second = PgConnection::connect(url)
        .await
        .map_err(|_| "fixture connection failed")?;
    let mut third = PgConnection::connect(url)
        .await
        .map_err(|_| "fixture connection failed")?;
    for conn in [&mut first, &mut second, &mut third] {
        sqlx::query("SET TIME ZONE 'UTC'").execute(conn).await?;
    }
    sqlx::raw_sql(SEED).execute(&mut first).await?;
    let mut a = first.begin().await?;
    let mut b = second.begin().await?;
    let picked =
        repo::pick_pending(&mut a, ProjectId(id(2)), Stage::Tester, "fixture", None).await?;
    let repeat =
        repo::pick_pending(&mut a, ProjectId(id(2)), Stage::Tester, "fixture", None).await?;
    let skipped =
        repo::pick_pending(&mut b, ProjectId(id(2)), Stage::Tester, "fixture", None).await?;
    let exhausted =
        repo::pick_pending(&mut third, ProjectId(id(2)), Stage::Tester, "fixture", None).await?;
    let queue = [picked, repeat, skipped, exhausted].map(|id| id.map(|id| id.to_string()));
    a.commit().await?;
    b.commit().await?;
    let p = principal(&Value::Null);
    let mut tx = first.begin().await?;
    let claimed =
        repo::claim_job(&mut tx, JobId(id(106)), &p, &[8; 32], &60.into(), CONTEXT).await?;
    let rotated = repo::rotate_token(&mut tx, JobId(id(106)), &[6; 32], CONTEXT).await?;
    let inside = value!([
        claimed.state.as_str(),
        claimed.lease_generation,
        rotated.lease_generation
    ]);
    tx.rollback().await?;
    let restored = repo::get_job(&mut second, JobId(id(106)), false, CONTEXT)
        .await?
        .expect("restored fixture");
    let mut tx = first.begin().await?;
    repo::get_job(&mut tx, JobId(id(107)), true, CONTEXT).await?;
    let (started, ready) = tokio::sync::oneshot::channel();
    let competitor = tokio::spawn(async move {
        started.send(()).expect("fixture barrier receiver");
        let result = repo::claim_job(
            &mut second,
            JobId(id(107)),
            &principal(&Value::Null),
            &[10; 32],
            &60.into(),
            CONTEXT,
        )
        .await;
        match result {
            Ok(row) => row.state.as_str(),
            Err(JobError::Invariant) => "Invariant",
            Err(error) => panic!("unexpected sanitized claim result: {error}"),
        }
    });
    ready.await?;
    let winner =
        repo::claim_job(&mut tx, JobId(id(107)), &p, &[9; 32], &60.into(), CONTEXT).await?;
    tx.commit().await?;
    let loser = competitor.await?;
    let final_row = repo::get_job(&mut third, JobId(id(107)), false, CONTEXT)
        .await?
        .expect("claimed fixture");
    Ok(
        value!({"queue":queue,"rollback_inside":inside,"rollback_after":[restored.state.as_str(),restored.lease_generation],"claims":[winner.state.as_str(),loser],"claim_final":[final_row.state.as_str(),final_row.lease_generation]}),
    )
}

async fn remaining_jobs(conn: &mut PgConnection) -> Result<Value, Box<dyn Error>> {
    let rows = sqlx::query(
        "SELECT id::text,state,stage,run_number,lease_generation FROM jobs ORDER BY id",
    )
    .fetch_all(conn)
    .await?;
    Ok(value!(
        rows.into_iter()
            .map(|row| value!([
                row.get::<String, _>(0),
                row.get::<String, _>(1),
                row.get::<String, _>(2),
                row.get::<i32, _>(3),
                row.get::<i32, _>(4)
            ]))
            .collect::<Vec<_>>()
    ))
}

// Named source cases probe values outside the native BIGINT lease interface.
fn checked_ttl_refusal(recipe: &Value) -> bool {
    let selected = matches!(
        text(recipe, "name", ""),
        "claim-ttl-9223372036854775808"
            | "extend-ttl-9223372036854775808"
            | "claim-power-1000"
            | "claim-power-131071"
            | "claim-power-131072"
            | "extend-power-1000"
            | "extend-power-131071"
            | "extend-power-131072"
            | "claim-negative-power"
            | "extend-negative-power"
            | "claim-nonmatching-0-power-1000"
            | "claim-nonmatching-0-power-131072"
            | "claim-nonmatching-101-power-1000"
            | "claim-nonmatching-101-power-131072"
            | "extend-nonmatching-0-power-1000"
            | "extend-nonmatching-0-power-131072"
            | "extend-nonmatching-101-power-1000"
            | "extend-nonmatching-101-power-131072"
            | "claim-numeric-max-weight-and-digit-count"
    );
    if selected {
        assert!(matches!(text(recipe, "action", ""), "claim" | "extend"));
        assert!(
            number(recipe, "ttl", Some(600))
                .expect("fixture TTL")
                .to_string()
                .parse::<i64>()
                .is_err()
        );
    }
    selected
}

fn native_checked_ttl_outcome(recipe: &Value) -> Value {
    let source = &recipe["outcome"];
    let name = text(recipe, "name", "");
    let mut remaining = source["remaining"].clone();
    if name == "claim-ttl-9223372036854775808"
        || name == "claim-power-1000"
        || name == "claim-power-131071"
        || name == "claim-numeric-max-weight-and-digit-count"
    {
        assert_eq!(source["value"]["id"], id(106).to_string());
        assert_eq!(source["value"]["state"], "claimed");
        assert_eq!(source["value"]["lease_generation"], 1);
        let row = remaining
            .as_array_mut()
            .expect("source remaining rows")
            .iter_mut()
            .find(|row| row[0] == id(106).to_string())
            .expect("source claimed row");
        assert_eq!(row[1], "claimed");
        assert_eq!(row[4], 1);
        row[1] = value!("pending");
        row[4] = value!(0);
    } else {
        assert!(source.get("value").is_none());
        if name.starts_with("claim-nonmatching") && name.ends_with("1000") {
            assert_eq!(source["error"], "Invariant");
        } else {
            let sqlstate = if name.ends_with("131072") {
                Value::Null
            } else if name == "extend-ttl-9223372036854775808" {
                value!("22008")
            } else {
                value!("22003")
            };
            assert_eq!(source["sqlstate"], sqlstate, "exact source lease refusal");
        }
    }
    value!({"sqlstate":null,"remaining":remaining})
}

// Only the decoded Document's display follows serde. SQL storage text, clocks,
// lease fields, query outcomes and remaining rows retain the exact source oracle.
fn native_json_numbers(value: &mut Value) {
    match value {
        Value::Number(number) if number.to_string().contains(['.', 'e', 'E']) => {
            *number = serde_json::Number::from_f64(number.as_f64().expect("finite fixture float"))
                .expect("finite fixture float");
        }
        Value::Array(items) => items.iter_mut().for_each(native_json_numbers),
        Value::Object(fields) => fields.values_mut().for_each(native_json_numbers),
        _ => {}
    }
}

fn native_job_json_projection(actual: &Value, expected: &mut Value) -> Result<(), Box<dyn Error>> {
    match (actual, expected) {
        (Value::Array(actual), Value::Array(expected)) => {
            assert_eq!(actual.len(), expected.len(), "job list length");
            for (actual, expected) in actual.iter().zip(expected) {
                native_job_json_projection(actual, expected)?;
            }
        }
        (Value::Object(actual), Value::Object(expected))
            if expected.contains_key("storage_json") =>
        {
            assert_eq!(
                actual.get("storage_json"),
                expected.get("storage_json"),
                "raw PostgreSQL JSON text"
            );
            for field in ["spec", "logs"] {
                let source_text = expected[field]
                    .as_str()
                    .expect("source job JSON projection");
                let actual_text = actual[field].as_str().expect("native job JSON projection");
                let mut source: Value = serde_json::from_str(source_text)?;
                native_json_numbers(&mut source);
                let native: Value = serde_json::from_str(actual_text)?;
                assert_eq!(native, source, "decoded job {field} semantics");
                let native_text = serde_json::to_string_pretty(&source)?;
                assert_eq!(
                    actual_text, native_text,
                    "standard serde job {field} display"
                );
                expected.insert(field.into(), Value::String(native_text));
            }
        }
        _ => {}
    }
    Ok(())
}

#[test]
fn native_job_projection_changes_only_document_display() -> Result<(), Box<dyn Error>> {
    let stored = r#"{"label": "é😀", "tiny": 0.0000001}"#;
    let mut expected = value!({
        "id": "job", "state": "claimed", "lease_generation": 3,
        "storage_json": [stored, "null"],
        "spec": "{\n  \"label\": \"\\u00e9\\ud83d\\ude00\",\n  \"tiny\": 1e-07,\n  \"wide\": 123456789012345678901234567890\n}",
        "logs": "null"
    });
    let mut actual = expected.clone();
    actual["spec"] = Value::String("{\n  \"label\": \"é😀\",\n  \"tiny\": 1e-7,\n  \"wide\": 123456789012345678901234567890\n}".into());
    native_job_json_projection(&actual, &mut expected)?;
    assert_eq!(actual, expected);
    assert_eq!(expected["storage_json"], value!([stored, "null"]));
    assert_eq!(expected["lease_generation"], 3);
    assert_eq!(expected["state"], "claimed");
    Ok(())
}

#[tokio::test]
#[ignore = "requires guarded Rust-migrated PostgreSQL and fresh production reference"]
async fn actual_python_jobs_repository_corpus() -> Result<(), Box<dyn Error>> {
    let fixture: Value =
        serde_json::from_str(runtime_reference!("/tests/fixtures/jobs_reference.json"))?;
    assert_eq!(fixture["python"], "3.13.11");
    assert_eq!(fixture["unicode"], "15.1.0");
    let recipes = fixture["cases"].as_array().expect("fixture cases");
    assert_eq!(
        fixture["count"].as_u64(),
        Some(u64::try_from(recipes.len())?)
    );
    assert_eq!(recipes.len(), 141);
    let mut conn = PgConnection::connect(&url())
        .await
        .map_err(|_| "isolated job connection failed")?;
    sqlx::query("SET TIME ZONE 'UTC'")
        .execute(&mut conn)
        .await?;
    for recipe in recipes {
        let mut tx = conn.begin().await?;
        let actual = observe(&mut tx, recipe).await?;
        let mut expected = if checked_ttl_refusal(recipe) {
            native_checked_ttl_outcome(recipe)
        } else {
            recipe["outcome"].clone()
        };
        if let Some(expected_value) = expected.get_mut("value") {
            native_job_json_projection(&actual["value"], expected_value)?;
        }
        assert_eq!(actual, expected, "{}", recipe["name"]);
        tx.rollback().await?;
    }
    drop(conn);
    assert_eq!(concurrency(&url()).await?, fixture["concurrency"]);
    Ok(())
}

#[test]
fn public_errors_and_jobs_are_redacted() {
    assert_eq!(
        JobError::Database {
            sqlstate: Some("23505".into())
        }
        .to_string(),
        "job database operation failed"
    );
    assert_ne!(Stage::Tester, Stage::Evaluator);
}
