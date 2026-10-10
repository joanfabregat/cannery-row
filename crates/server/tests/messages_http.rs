//! Questions, steering notes and transcripts over HTTP: a performer asks
//! under its lease, a blocking question stops the lease clock until a
//! researcher answers it, an unanswered one releases the attempt to the next,
//! a researcher steers a running attempt and escalates a question into a
//! concern, and the agent appends its transcript within the project's cap.
#![forbid(unsafe_code)]
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use cannery_core::settings::load_settings;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{collections::BTreeMap, error::Error};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const PROJECT: &str = "00000000-0000-0000-0000-000000000010";
const ADMIN: &str = "00000000-0000-0000-0000-000000000001";
const AGENT: &str = "00000000-0000-0000-0000-000000000020";
const SEED: &str = "
SET TIME ZONE 'UTC';
INSERT INTO users(id,issuer,subject,email,email_verified,display_name,is_admin) SELECT ('00000000-0000-0000-0000-'||lpad(i::text,12,'0'))::uuid,'https://messages.fixture',role,role||'@fixture.invalid',true,role,i=1 FROM (VALUES(1,'admin'),(2,'researcher'),(3,'member'),(4,'viewer')) AS u(i,role);
INSERT INTO projects(id,slug,title,created_by) VALUES('00000000-0000-0000-0000-000000000010','matrix','Matrix','00000000-0000-0000-0000-000000000001');
INSERT INTO memberships(project_id,user_id,role,granted_by) SELECT '00000000-0000-0000-0000-000000000010',('00000000-0000-0000-0000-'||lpad(i::text,12,'0'))::uuid,role,'00000000-0000-0000-0000-000000000001' FROM (VALUES(2,'researcher'),(3,'member'),(4,'viewer')) AS u(i,role);
INSERT INTO api_tokens(token_hash,display_prefix,kind,user_id,name,scopes,expires_at) SELECT sha256(convert_to('cr_pat_messages_'||role,'UTF8')),'cr_pat_fixture','personal',('00000000-0000-0000-0000-'||lpad(i::text,12,'0'))::uuid,role,ARRAY['read','write'],now()+interval '1 day' FROM (VALUES(2,'researcher'),(3,'member'),(4,'viewer')) AS u(i,role);
INSERT INTO service_accounts(id,project_id,kind,name,created_by) VALUES('00000000-0000-0000-0000-000000000020','00000000-0000-0000-0000-000000000010','agent','fixture-agent','00000000-0000-0000-0000-000000000001');
INSERT INTO api_tokens(token_hash,display_prefix,kind,service_account_id,name,scopes,expires_at) VALUES(sha256(convert_to('cr_svc_messages_agent','UTF8')),'cr_svc_fixture','service','00000000-0000-0000-0000-000000000020','agent',ARRAY['read','write'],now()+interval '1 day');
INSERT INTO tracks(id,project_id,slug,title,state,created_by) VALUES('00000000-0000-0000-0000-000000000030','00000000-0000-0000-0000-000000000010','alpha','Alpha','planning','00000000-0000-0000-0000-000000000001');
";

fn step(name: &str, role: &str, output: &str, path: &str) -> Value {
    json!({
        "apiVersion": "cannery-row/v1",
        "kind": "Step",
        "metadata": {"name": name},
        "spec": {
            "activeDeadlineSeconds": 60,
            "container": {
                "command": ["true"],
                "image": "fixture.invalid/step@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "resources": {"limits": {"cpu": "1"}},
            },
            "inputs": {"artifacts": []},
            "network": "none",
            "outputs": {"artifacts": [{"interface": "cr-evidence/v0.2", "name": output, "path": path}]},
            "role": role,
            "sandbox": "Authored isolated messages fixture",
        },
    })
}

async fn seed(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql(SEED).execute(pool).await?;
    let science = json!({
        "default_producer": {"name": "producer", "revision": 1},
        "evaluator": {},
        "metrics": [{
            "key": "score", "splits": ["test", "train"], "dimensions": [],
            "unit": "ratio", "direction": "maximize", "aggregation": "mean",
        }],
        "limits": {"max_output_bytes": 1234, "resource_ceilings": {"cpu": "1"}},
        "required_artifact_roles": {"attempt": []},
        "scorer": step("scorer", "scorer", "evidence", "/cr/outputs/evidence"),
    });
    for (table, field, name, content) in [
        ("config_revisions", "kind", "science", science),
        (
            "producer_manifests",
            "name",
            "producer",
            step("producer", "producer", "result", "/cr/outputs/result"),
        ),
    ] {
        sqlx::query(&format!(
            "INSERT INTO {table}(project_id,{field},revision,content,created_by) VALUES($1::uuid,$2,1,$3::text::jsonb,$4::uuid)"
        ))
        .bind(PROJECT)
        .bind(name)
        .bind(content.to_string())
        .bind(ADMIN)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// One request as `role` (`anonymous` sends no token) with extra headers;
/// the body is JSON, the response JSON or text, with its headers.
async fn send(
    app: &Router,
    role: &str,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: Option<Value>,
) -> Result<(u16, Value, axum::http::HeaderMap)> {
    let mut builder = Request::builder().method(method).uri(path);
    if role != "anonymous" {
        let prefix = if role == "agent" {
            "cr_svc_"
        } else {
            "cr_pat_"
        };
        builder = builder.header("authorization", format!("Bearer {prefix}messages_{role}"));
    }
    for (name, value) in headers {
        builder = builder.header(*name, value);
    }
    let body = if let Some(body) = body {
        builder = builder.header("content-type", "application/json");
        Body::from(body.to_string())
    } else {
        Body::empty()
    };
    let response = app.clone().oneshot(builder.body(body)?).await?;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024).await?;
    let value = if bytes.is_empty() {
        Value::Null
    } else if status == 500 {
        Value::String(String::from_utf8(bytes.to_vec())?)
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::String(String::from_utf8(bytes.to_vec())?))
    };
    Ok((status, value, headers))
}

async fn call(
    app: &Router,
    role: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<(u16, Value)> {
    let (status, value, _) = send(app, role, method, path, &[], body).await?;
    Ok((status, value))
}

fn unit(key: &str, title: &str) -> Value {
    json!({
        "key": key,
        "title": title,
        "question": format!("Does {title} beat the baseline?"),
        "intervention": format!("Run {title}."),
        "acceptance": {
            "selection_splits": ["train"],
            "confirmation_splits": ["test"],
            "primary_metric": "score",
            "required_slices": [],
            "success_criteria": "The score improves.",
            "falsification_criteria": "The score does not improve.",
            "regression_gates": [],
            "compute_budget": {"gpu_hours_max": 1},
        },
        "brief": format!("# {title}\n\nKeep the seed fixed.\n"),
    })
}

/// The lease an agent holds on one attempt.
struct Held {
    number: i64,
    sequence: i64,
    headers: Vec<(&'static str, String)>,
}

impl Held {
    fn of(claim: &Value) -> Result<Self> {
        Ok(Self {
            number: claim["attempt"]["number"].as_i64().ok_or("number")?,
            sequence: claim["attempt"]["sequence"].as_i64().ok_or("sequence")?,
            headers: vec![
                (
                    "x-lease-token",
                    claim["lease_token"].as_str().ok_or("token")?.to_owned(),
                ),
                ("x-lease-generation", claim["lease_generation"].to_string()),
            ],
        })
    }
    fn path(&self, tail: &str) -> String {
        format!(
            "/api/projects/matrix/units/{}/attempts/{}/{tail}",
            self.number, self.sequence
        )
    }
    fn with(&self, key: &str) -> Vec<(&'static str, String)> {
        let mut headers = self.headers.clone();
        headers.push(("idempotency-key", key.to_owned()));
        headers
    }
}

async fn plan(app: &Router) -> Result<()> {
    let plans = "/api/projects/matrix/tracks/alpha/plans";
    let draft = format!("{plans}/draft");
    let (status, value) = call(
        app,
        "researcher",
        "POST",
        "/api/projects/matrix/brief",
        Some(json!({
            "document": "---\ntitle: Matrix\ngoal: Rank the matrix fixtures.\n---\n# Domain\n\nFixtures only.\n",
            "expected_revision": 0,
        })),
    )
    .await?;
    assert_eq!(status, 201, "{value}");
    let (status, value) = call(app, "researcher", "POST", plans, None).await?;
    assert_eq!(status, 201, "{value}");
    let (status, value) = call(
        app,
        "researcher",
        "PUT",
        &format!("{draft}/approach"),
        Some(json!({"approach": "Establish a baseline, then vary one thing."})),
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    for (key, title) in [("baseline", "Baseline"), ("variant", "Variant")] {
        let (status, value) = call(
            app,
            "researcher",
            "POST",
            &format!("{draft}/units"),
            Some(unit(key, title)),
        )
        .await?;
        assert_eq!(status, 201, "{value}");
    }
    let (status, value) = call(
        app,
        "researcher",
        "POST",
        &format!("{draft}/submission"),
        None,
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    let (status, value) = call(
        app,
        "researcher",
        "POST",
        &format!("{plans}/1/review"),
        Some(json!({"action": "approve", "reason": "Ready to run."})),
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    Ok(())
}

async fn attempt_row(pool: &PgPool, sequence: i64) -> Result<(String, bool)> {
    Ok(sqlx::query_as(
        "SELECT state, EXISTS (SELECT 1 FROM lease_pauses p WHERE p.attempt_id = attempts.id AND p.job_id IS NULL) FROM attempts WHERE sequence = $1",
    )
    .bind(i32::try_from(sequence)?)
    .fetch_one(pool)
    .await?)
}

fn ids(items: &Value) -> Vec<Value> {
    items
        .as_array()
        .map(|items| items.iter().map(|item| item["id"].clone()).collect())
        .unwrap_or_default()
}

#[tokio::test]
#[ignore = "requires positive exact selection and guarded fresh migrated child"]
#[allow(
    clippy::too_many_lines,
    reason = "One unit's conversation, from the first question to the escalated last"
)]
async fn questions_steering_and_transcripts() -> Result<()> {
    let url = std::env::var("CANNERY_MESSAGES_HTTP_DATABASE_URL")?;
    let objects = std::env::temp_dir().join(format!("cannery-messages-{}", std::process::id()));
    let settings = load_settings(
        None,
        &BTreeMap::from([
            ("CANNERY_DATABASE_URL".into(), url),
            (
                "CANNERY_STORAGE_LOCAL_ROOT".into(),
                objects.display().to_string(),
            ),
        ]),
    )?;
    let (app, state, _cancellation) =
        cannery_server::production_application(settings, "127.0.0.1").await?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    if !name.starts_with("conformance_") {
        return Err("owned nonce child".into());
    }
    seed(&state.pool).await?;
    plan(&app).await?;
    let pool = &state.pool;

    // The protocol is public Markdown, named by every claim with its digest.
    let (status, text, headers) =
        send(&app, "anonymous", "GET", "/api/protocol", &[], None).await?;
    assert_eq!(status, 200);
    let text = text.as_str().ok_or("protocol")?.to_owned();
    assert!(text.contains("## Questions"), "{text}");
    assert!(
        headers["content-type"]
            .to_str()?
            .starts_with("text/markdown")
    );
    let etag = headers["etag"].to_str()?.to_owned();
    let (status, _, _) = send(
        &app,
        "anonymous",
        "GET",
        "/api/protocol",
        &[("if-none-match", etag.clone())],
        None,
    )
    .await?;
    assert_eq!(status, 304);
    let (status, claim) = call(
        &app,
        "agent",
        "POST",
        "/api/projects/matrix/claims",
        Some(json!({})),
    )
    .await?;
    assert_eq!(status, 201, "{claim}");
    assert_eq!(claim["protocol"]["ref"], "/api/protocol", "{claim}");
    assert_eq!(claim["protocol"]["bytes"], text.len());
    assert_eq!(
        format!("\"{}\"", claim["protocol"]["sha256"].as_str().ok_or("sha")?),
        etag
    );
    let held = Held::of(&claim)?;
    let questions = held.path("questions");

    // Asking needs the lease, a body, and a default unless it blocks.
    let (status, value) = call(
        &app,
        "agent",
        "POST",
        &questions,
        Some(json!({"body": "Which split?", "blocking": true})),
    )
    .await?;
    assert_eq!(
        (status, &value["error"]["code"]),
        (409, &json!("stale_lease")),
        "{value}"
    );
    let (status, value, _) = send(
        &app,
        "agent",
        "POST",
        &questions,
        &held.headers,
        Some(json!({"body": "Which split?", "blocking": false})),
    )
    .await?;
    assert_eq!(status, 422, "{value}");
    assert_eq!(
        value["error"]["details"][0]["path"], "body/default",
        "{value}"
    );
    let (status, value, _) = send(
        &app,
        "researcher",
        "POST",
        &questions,
        &held.headers,
        Some(json!({"body": "Not mine to ask.", "blocking": true})),
    )
    .await?;
    assert_eq!(status, 403, "{value}");

    // A non-blocking question states its default; a retry replays it.
    let ask = json!({
        "body": "Should the variant also vary the learning rate?",
        "blocking": false,
        "default": "Only the seed varies.",
    });
    let (status, open, _) = send(
        &app,
        "agent",
        "POST",
        &questions,
        &held.with("open-1"),
        Some(ask.clone()),
    )
    .await?;
    assert_eq!(status, 201, "{open}");
    assert_eq!(open["kind"], "question");
    assert_eq!(open["state"], "open");
    assert_eq!(open["blocking"], false);
    assert_eq!(open["default"], "Only the seed varies.");
    assert_eq!(open["author_kind"], "service");
    assert_eq!(open["author_name"], "fixture-agent");
    assert_eq!(open["unit"], held.number);
    assert_eq!(open["attempt"], held.sequence);
    let (status, replay, _) = send(
        &app,
        "agent",
        "POST",
        &questions,
        &held.with("open-1"),
        Some(ask),
    )
    .await?;
    assert_eq!(status, 200, "{replay}");
    assert_eq!(replay["id"], open["id"]);
    assert_eq!(
        attempt_row(pool, held.sequence).await?,
        ("claimed".into(), false)
    );

    // A blocking question stops the lease clock until it is answered.
    let (before, deadline): (String, Option<String>) = sqlx::query_as(
        "SELECT lease_expires_at::text, deadline::text FROM attempts WHERE sequence = $1",
    )
    .bind(i32::try_from(held.sequence)?)
    .fetch_one(pool)
    .await?;
    let (status, blocking, _) = send(
        &app,
        "agent",
        "POST",
        &questions,
        &held.headers,
        Some(
            json!({"body": "The baseline data is missing its test split. Wait?", "blocking": true}),
        ),
    )
    .await?;
    assert_eq!(status, 201, "{blocking}");
    let blocking_id = blocking["id"].as_str().ok_or("id")?.to_owned();
    assert_eq!(
        attempt_row(pool, held.sequence).await?,
        ("waiting_on_human".into(), true)
    );
    let (status, beat, _) = send(
        &app,
        "agent",
        "POST",
        &held.path("heartbeat"),
        &held.headers,
        None,
    )
    .await?;
    assert_eq!(status, 200, "{beat}");
    assert_eq!(beat["waiting_on_human"], true, "{beat}");
    assert_eq!(beat["answers"], json!([]));
    let (unchanged,): (String,) =
        sqlx::query_as("SELECT lease_expires_at::text FROM attempts WHERE sequence = $1")
            .bind(i32::try_from(held.sequence)?)
            .fetch_one(pool)
            .await?;
    assert_eq!(unchanged, before);
    // A waiting attempt takes no other work, and its lease never runs out.
    let (status, value, _) = send(
        &app,
        "agent",
        "POST",
        &held.path("uploads"),
        &held.headers,
        Some(json!({"role": "result", "name": "result.json", "size_bytes": 2, "sha256": "0".repeat(64), "media_type": "application/json"})),
    )
    .await?;
    assert_eq!(status, 409, "{value}");
    sqlx::query(
        "UPDATE attempts SET lease_expires_at = now() - interval '1 minute' WHERE sequence = $1",
    )
    .bind(i32::try_from(held.sequence)?)
    .execute(pool)
    .await?;
    cannery_server::sweeps::run(&state).await?;
    assert_eq!(
        attempt_row(pool, held.sequence).await?.0,
        "waiting_on_human"
    );
    let answer = format!("/api/projects/matrix/questions/{blocking_id}/answer");
    let (status, waited) = call(&app, "agent", "GET", &format!("{answer}?wait=1"), None).await?;
    assert_eq!(status, 200, "{waited}");
    assert_eq!(waited["state"], "open");
    assert_eq!(waited["answer"], Value::Null);

    // Researchers answer, from the queue; the attempt runs on.
    let (status, queue) = call(
        &app,
        "viewer",
        "GET",
        "/api/projects/matrix/questions?state=open",
        None,
    )
    .await?;
    assert_eq!(status, 200, "{queue}");
    assert_eq!(
        ids(&queue["items"]),
        vec![json!(blocking_id), open["id"].clone()]
    );
    let (status, value) = call(
        &app,
        "member",
        "POST",
        &answer,
        Some(json!({"body": "Use the train split."})),
    )
    .await?;
    assert_eq!(status, 403, "{value}");
    let (status, value) = call(
        &app,
        "researcher",
        "POST",
        &answer,
        Some(json!({"body": " "})),
    )
    .await?;
    assert_eq!(status, 422, "{value}");
    let (status, answered) = call(
        &app,
        "researcher",
        "POST",
        &answer,
        Some(json!({"body": "Hold out 10% of train instead."})),
    )
    .await?;
    assert_eq!(status, 200, "{answered}");
    assert_eq!(answered["state"], "answered");
    assert_eq!(answered["answer"]["body"], "Hold out 10% of train instead.");
    assert_eq!(answered["answer"]["author_name"], "researcher");
    let (status, value) = call(
        &app,
        "researcher",
        "POST",
        &answer,
        Some(json!({"body": "Again."})),
    )
    .await?;
    assert_eq!(status, 409, "{value}");
    let (state_now, paused) = attempt_row(pool, held.sequence).await?;
    assert_ne!(state_now, "waiting_on_human");
    assert!(!paused);
    let (renewed, moved): (bool, bool) = sqlx::query_as(
        "SELECT lease_expires_at > now(), coalesce(deadline >= $2::timestamptz, $2 IS NULL) FROM attempts WHERE sequence = $1",
    )
    .bind(i32::try_from(held.sequence)?)
    .bind(&deadline)
    .fetch_one(pool)
    .await?;
    assert!(renewed && moved);
    let (status, beat, _) = send(
        &app,
        "agent",
        "POST",
        &held.path("heartbeat"),
        &held.headers,
        None,
    )
    .await?;
    assert_eq!(status, 200, "{beat}");
    assert_eq!(beat["waiting_on_human"], false);
    assert_eq!(
        ids(&beat["answers"]),
        vec![answered["answer"]["id"].clone()]
    );
    // Waiting for it as its author acknowledges it.
    let (status, waited) = call(&app, "agent", "GET", &answer, None).await?;
    assert_eq!(status, 200, "{waited}");
    assert!(waited["answer"]["acknowledged_at"].is_string(), "{waited}");
    let (_, beat, _) = send(
        &app,
        "agent",
        "POST",
        &held.path("heartbeat"),
        &held.headers,
        None,
    )
    .await?;
    assert_eq!(beat["answers"], json!([]), "{beat}");

    // Steering: a researcher's note, read at the heartbeat, acknowledged.
    let steering = held.path("steering");
    let (status, value) = call(
        &app,
        "agent",
        "POST",
        &steering,
        Some(json!({"body": "Steer myself."})),
    )
    .await?;
    assert_eq!(status, 403, "{value}");
    let (status, note) = call(
        &app,
        "researcher",
        "POST",
        &steering,
        Some(json!({"body": "Log the seed in every run."})),
    )
    .await?;
    assert_eq!(status, 201, "{note}");
    assert_eq!(note["kind"], "steer");
    let (_, beat, _) = send(
        &app,
        "agent",
        "POST",
        &held.path("heartbeat"),
        &held.headers,
        None,
    )
    .await?;
    assert_eq!(ids(&beat["steering"]), vec![note["id"].clone()], "{beat}");
    let (status, pending) = call(
        &app,
        "agent",
        "GET",
        &format!("{steering}?pending=true"),
        None,
    )
    .await?;
    assert_eq!(status, 200, "{pending}");
    assert_eq!(ids(&pending["items"]), vec![note["id"].clone()]);
    let acknowledgements = "/api/projects/matrix/messages/acknowledgements";
    let (status, value) = call(
        &app,
        "researcher",
        "POST",
        acknowledgements,
        Some(json!({"ids": [note["id"]]})),
    )
    .await?;
    assert_eq!(status, 403, "{value}");
    let (status, value) = call(
        &app,
        "agent",
        "POST",
        acknowledgements,
        Some(json!({"ids": [note["id"]]})),
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    assert_eq!(value["acknowledged"], json!([note["id"]]), "{value}");
    assert_eq!(value["already_acknowledged"], json!([]), "{value}");
    let (status, value) = call(
        &app,
        "agent",
        "POST",
        acknowledgements,
        Some(json!({"ids": [note["id"]]})),
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    assert_eq!(value["acknowledged"], json!([note["id"]]), "{value}");
    assert_eq!(
        value["already_acknowledged"],
        json!([note["id"]]),
        "{value}"
    );
    let (_, beat, _) = send(
        &app,
        "agent",
        "POST",
        &held.path("heartbeat"),
        &held.headers,
        None,
    )
    .await?;
    assert_eq!(beat["steering"], json!([]), "{beat}");

    // The transcript: validated events, appended under the lease, capped.
    let transcript = held.path("transcript");
    let events = json!({"events": [
        {"ts": "2026-10-10T10:00:00Z", "kind": "assistant", "content": "Reading the brief."},
        {"ts": "2026-10-10T10:00:01Z", "kind": "tool_call", "tool": "get_brief", "call_id": "1", "content": {"project": "matrix"}},
        {"ts": "2026-10-10T10:00:02Z", "kind": "steer", "message": note["id"], "content": "Log the seed in every run."},
    ]});
    let (status, value) = call(&app, "agent", "POST", &transcript, Some(events.clone())).await?;
    assert_eq!(status, 409, "{value}");
    let (status, value, _) = send(
        &app,
        "agent",
        "POST",
        &transcript,
        &held.headers,
        Some(
            json!({"events": [{"ts": "2026-10-10T10:00:00Z", "kind": "thought", "content": "?"}]}),
        ),
    )
    .await?;
    assert_eq!(status, 422, "{value}");
    assert!(
        value["error"]["details"][0]["path"]
            .as_str()
            .ok_or("path")?
            .starts_with("body/events/0"),
        "{value}"
    );
    let (status, appended, _) = send(
        &app,
        "agent",
        "POST",
        &transcript,
        &held.with("chunk-1"),
        Some(events.clone()),
    )
    .await?;
    assert_eq!(status, 201, "{appended}");
    assert_eq!(appended["events"], 3);
    let (status, replay, _) = send(
        &app,
        "agent",
        "POST",
        &transcript,
        &held.with("chunk-1"),
        Some(events),
    )
    .await?;
    assert_eq!(status, 200, "{replay}");
    assert_eq!(replay["chunk"], appended["chunk"]);
    let (status, read) = call(&app, "viewer", "GET", &transcript, None).await?;
    assert_eq!(status, 200, "{read}");
    assert_eq!(read["total_events"], 3, "{read}");
    assert_eq!(read["sealed"], false);
    assert_eq!(read["events"][1]["event"]["tool"], "get_brief");
    let (status, page) = call(
        &app,
        "viewer",
        "GET",
        &format!("{transcript}?after=1"),
        None,
    )
    .await?;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["events"][0]["index"], 2, "{page}");
    let (_, mut limits) = call(&app, "member", "GET", "/api/projects/matrix/limits", None).await?;
    assert_eq!(limits["question_wait_seconds"], 86_400, "{limits}");
    assert_eq!(limits["transcript_max_bytes"], 67_108_864, "{limits}");
    limits["transcript_max_bytes"] = json!(65_536);
    let (status, value) = call(
        &app,
        "researcher",
        "PUT",
        "/api/projects/matrix/limits",
        Some(limits),
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    let (status, value, _) = send(
        &app,
        "agent",
        "POST",
        &transcript,
        &held.headers,
        Some(json!({"events": [{"ts": "2026-10-10T10:00:03Z", "kind": "tool_result", "content": "x".repeat(70_000)}]})),
    )
    .await?;
    assert_eq!(status, 422, "{value}");
    let detail = &value["error"]["details"][0];
    assert_eq!(detail["limit"], 65_536, "{value}");
    assert!(detail["size"].as_i64().ok_or("size")? > 65_536, "{value}");
    assert_eq!(
        detail["location"],
        format!("attempt #{}.{} transcript", held.number, held.sequence)
    );

    // Unanswered past the wait, the attempt is released and the unit queued
    // again; the next attempt carries the question and its answer.
    let (status, last, _) = send(
        &app,
        "agent",
        "POST",
        &questions,
        &held.headers,
        Some(json!({"body": "Is the GPU quota raised yet?", "blocking": true})),
    )
    .await?;
    assert_eq!(status, 201, "{last}");
    let last_id = last["id"].as_str().ok_or("id")?.to_owned();
    sqlx::raw_sql(&format!(
        "BEGIN; ALTER TABLE messages DISABLE TRIGGER messages_close_only; UPDATE messages SET created_at = created_at - interval '2 days' WHERE id = '{last_id}'; ALTER TABLE messages ENABLE TRIGGER messages_close_only; COMMIT;"
    ))
    .execute(pool)
    .await?;
    let report = cannery_server::sweeps::run(&state).await?;
    assert_eq!(report.errors, 0);
    assert_eq!(attempt_row(pool, held.sequence).await?.0, "failed");
    let (code, unit_state): (String, String) = sqlx::query_as(
        "SELECT f.code, u.state FROM attempt_failures f JOIN attempts a ON a.id = f.attempt_id JOIN units u ON u.id = a.unit_id WHERE a.sequence = $1",
    )
    .bind(i32::try_from(held.sequence)?)
    .fetch_one(pool)
    .await?;
    assert_eq!(
        (code.as_str(), unit_state.as_str()),
        ("unanswered_question", "queued")
    );
    let (status, released) = call(
        &app,
        "viewer",
        "GET",
        &format!("/api/projects/matrix/questions/{last_id}"),
        None,
    )
    .await?;
    assert_eq!(status, 200, "{released}");
    assert_eq!(released["state"], "open");
    assert!(released["released_at"].is_string(), "{released}");
    let (status, value) = call(
        &app,
        "researcher",
        "POST",
        &format!("/api/projects/matrix/questions/{last_id}/answer"),
        Some(json!({"body": "Yes, since this morning."})),
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    let (status, claim) = call(
        &app,
        "agent",
        "POST",
        "/api/projects/matrix/claims",
        Some(json!({"unit": held.number})),
    )
    .await?;
    assert_eq!(status, 201, "{claim}");
    let next = Held::of(&claim)?;
    assert_eq!(next.number, held.number);
    let bundle = claim["context"]["ref"]
        .as_str()
        .ok_or("context")?
        .to_owned();
    let (status, text) = call(&app, "agent", "GET", &bundle, None).await?;
    assert_eq!(status, 200, "{text}");
    let text = text.as_str().ok_or("context.md")?;
    for part in [
        "## Questions from earlier attempts",
        "Is the GPU quota raised yet?",
        "Yes, since this morning.",
    ] {
        assert!(text.contains(part), "{part}: {text}");
    }
    let (_, beat, _) = send(
        &app,
        "agent",
        "POST",
        &next.path("heartbeat"),
        &next.headers,
        None,
    )
    .await?;
    assert_eq!(
        beat["answers"][0]["body"], "Yes, since this morning.",
        "{beat}"
    );

    // A job's performer asks under the job's lease; the job pauses.
    let job = "00000000-0000-0000-0000-000000000040";
    sqlx::query(
        "INSERT INTO jobs(id,project_id,attempt_id,phase,run_number,state,science_revision,performer,spec,deadline_seconds,claimed_by_service,via_channel,lease_generation,lease_token_hash,lease_expires_at,claimed_at,deadline) SELECT $1::uuid,project_id,id,'document',1,'claimed',1,'agent',jsonb_build_object('performer','agent','track','alpha','unit',$2::int,'steps','[]'::jsonb,'parameters','{}'::jsonb,'output_prefix','projects/x/'),600,$3::uuid,'api',1,sha256(convert_to('job-held','UTF8')),now()+interval '1 hour',now(),now()+interval '2 hours' FROM attempts WHERE sequence = $4",
    )
    .bind(job)
    .bind(i32::try_from(held.number)?)
    .bind(AGENT)
    .bind(i32::try_from(held.sequence)?)
    .execute(pool)
    .await?;
    let job_lease = vec![
        ("x-lease-token", "job-held".to_owned()),
        ("x-lease-generation", "1".to_owned()),
    ];
    let (status, asked, _) = send(
        &app,
        "agent",
        "POST",
        &format!("/api/projects/matrix/jobs/{job}/questions"),
        &job_lease,
        Some(json!({"body": "Cite the failed attempt too?", "blocking": true})),
    )
    .await?;
    assert_eq!(status, 201, "{asked}");
    assert_eq!(asked["job"], job, "{asked}");
    assert_eq!(asked["job_phase"], "document", "{asked}");
    let (paused,): (bool,) =
        sqlx::query_as("SELECT EXISTS (SELECT 1 FROM lease_pauses WHERE job_id = $1::uuid)")
            .bind(job)
            .fetch_one(pool)
            .await?;
    assert!(paused);
    let (status, beat, _) = send(
        &app,
        "agent",
        "POST",
        &format!("/api/projects/matrix/jobs/{job}/heartbeat"),
        &job_lease,
        None,
    )
    .await?;
    assert_eq!(status, 200, "{beat}");
    assert_eq!(beat["waiting_on_human"], true, "{beat}");

    // Escalated, the question closes and its concern blocks the track.
    let escalation = format!(
        "/api/projects/matrix/questions/{}/escalation",
        asked["id"].as_str().ok_or("id")?
    );
    let (status, value) = call(
        &app,
        "member",
        "POST",
        &escalation,
        Some(json!({"kind": "blocker", "note": "The plan never says."})),
    )
    .await?;
    assert_eq!(status, 403, "{value}");
    let (status, escalated) = call(
        &app,
        "researcher",
        "POST",
        &escalation,
        Some(json!({"kind": "blocker", "note": "The plan never says which attempts a write-up cites."})),
    )
    .await?;
    assert_eq!(status, 200, "{escalated}");
    assert_eq!(escalated["state"], "escalated", "{escalated}");
    assert!(escalated["concern"].is_string(), "{escalated}");
    let (status, concern) = call(
        &app,
        "viewer",
        "GET",
        &format!(
            "/api/projects/matrix/concerns/{}",
            escalated["concern"].as_str().ok_or("concern")?
        ),
        None,
    )
    .await?;
    assert_eq!(status, 200, "{concern}");
    assert_eq!(concern["kind"], "blocker");
    assert_eq!(concern["state"], "open");
    assert!(
        concern["body"]
            .as_str()
            .ok_or("body")?
            .contains("Cite the failed attempt too?"),
        "{concern}"
    );
    let (status, value) = call(
        &app,
        "researcher",
        "POST",
        &escalation,
        Some(json!({"kind": "other", "note": "Again."})),
    )
    .await?;
    assert_eq!(status, 409, "{value}");
    let (paused,): (bool,) =
        sqlx::query_as("SELECT EXISTS (SELECT 1 FROM lease_pauses WHERE job_id = $1::uuid)")
            .bind(job)
            .fetch_one(pool)
            .await?;
    assert!(!paused);
    let (status, refused) = call(
        &app,
        "agent",
        "POST",
        "/api/projects/matrix/claims",
        Some(json!({})),
    )
    .await?;
    assert_eq!(
        (status, &refused["error"]["code"]),
        (409, &json!("concern_open")),
        "{refused}"
    );

    // The unit's messages, and what the audit recorded.
    let (status, all) = call(
        &app,
        "viewer",
        "GET",
        &format!("/api/projects/matrix/units/{}/messages", held.number),
        None,
    )
    .await?;
    assert_eq!(status, 200, "{all}");
    let kinds: Vec<_> = all["items"]
        .as_array()
        .ok_or("items")?
        .iter()
        .map(|item| item["kind"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        kinds.iter().filter(|kind| *kind == "question").count(),
        4,
        "{all}"
    );
    assert_eq!(
        kinds.iter().filter(|kind| *kind == "answer").count(),
        3,
        "{all}"
    );
    assert_eq!(
        kinds.iter().filter(|kind| *kind == "steer").count(),
        1,
        "{all}"
    );
    let (status, page) = call(
        &app,
        "viewer",
        "GET",
        &format!(
            "/api/projects/matrix/units/{}/messages?limit=2",
            held.number
        ),
        None,
    )
    .await?;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["items"].as_array().map(Vec::len), Some(2));
    assert!(page["next_before"].is_string(), "{page}");
    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT action FROM audit_events WHERE action LIKE 'question.%' OR action LIKE 'steering.%' ORDER BY action",
    )
    .fetch_all(pool)
    .await?;
    assert_eq!(
        actions,
        [
            "question.answered",
            "question.asked",
            "question.escalated",
            "steering.posted"
        ]
    );
    Ok(())
}
