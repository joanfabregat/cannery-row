//! Plan concerns over HTTP: raised by an agent from its attempt and by a
//! member, the claims they block while open, a researcher's dismissal, and
//! the plan revision that answers one.
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
const SEED: &str = "
SET TIME ZONE 'UTC';
INSERT INTO users(id,issuer,subject,email,email_verified,display_name,is_admin) SELECT ('00000000-0000-0000-0000-'||lpad(i::text,12,'0'))::uuid,'https://concerns.fixture',role,role||'@fixture.invalid',true,role,i=1 FROM (VALUES(1,'admin'),(2,'researcher'),(3,'member'),(4,'viewer')) AS u(i,role);
INSERT INTO projects(id,slug,title,created_by) VALUES('00000000-0000-0000-0000-000000000010','matrix','Matrix','00000000-0000-0000-0000-000000000001');
INSERT INTO memberships(project_id,user_id,role,granted_by) SELECT '00000000-0000-0000-0000-000000000010',('00000000-0000-0000-0000-'||lpad(i::text,12,'0'))::uuid,role,'00000000-0000-0000-0000-000000000001' FROM (VALUES(2,'researcher'),(3,'member'),(4,'viewer')) AS u(i,role);
INSERT INTO api_tokens(token_hash,display_prefix,kind,user_id,name,scopes,expires_at) SELECT sha256(convert_to('cr_pat_concerns_'||role,'UTF8')),'cr_pat_fixture','personal',('00000000-0000-0000-0000-'||lpad(i::text,12,'0'))::uuid,role,ARRAY['read','write'],now()+interval '1 day' FROM (VALUES(2,'researcher'),(3,'member'),(4,'viewer')) AS u(i,role);
INSERT INTO service_accounts(id,project_id,kind,name,created_by) VALUES('00000000-0000-0000-0000-000000000020','00000000-0000-0000-0000-000000000010','agent','fixture-agent','00000000-0000-0000-0000-000000000001');
INSERT INTO api_tokens(token_hash,display_prefix,kind,service_account_id,name,scopes,expires_at) VALUES(sha256(convert_to('cr_svc_concerns_agent','UTF8')),'cr_svc_fixture','service','00000000-0000-0000-0000-000000000020','agent',ARRAY['read','write'],now()+interval '1 day');
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
            "sandbox": "Authored isolated concern fixture",
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

/// One request as `role`; the body is JSON, the response JSON or text.
async fn call(
    app: &Router,
    role: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<(u16, Value)> {
    let prefix = if role == "agent" {
        "cr_svc_"
    } else {
        "cr_pat_"
    };
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {prefix}concerns_{role}"));
    let body = if let Some(body) = body {
        builder = builder.header("content-type", "application/json");
        Body::from(body.to_string())
    } else {
        Body::empty()
    };
    let response = app.clone().oneshot(builder.body(body)?).await?;
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024).await?;
    let value = if bytes.is_empty() {
        Value::Null
    } else if status == 500 {
        Value::String(String::from_utf8(bytes.to_vec())?)
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::String(String::from_utf8(bytes.to_vec())?))
    };
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

/// Submit and approve the open draft as revision `revision`.
async fn approve(app: &Router, revision: u32) -> Result<()> {
    let plans = "/api/projects/matrix/tracks/alpha/plans";
    let (status, value) = call(
        app,
        "researcher",
        "POST",
        &format!("{plans}/draft/submission"),
        None,
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    let (status, value) = call(
        app,
        "researcher",
        "POST",
        &format!("{plans}/{revision}/review"),
        Some(json!({"action": "approve", "reason": "Ready to run."})),
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    Ok(())
}

async fn claim(app: &Router) -> Result<(u16, Value)> {
    call(
        app,
        "agent",
        "POST",
        "/api/projects/matrix/claims",
        Some(json!({})),
    )
    .await
}

#[tokio::test]
#[ignore = "requires positive exact selection and guarded fresh migrated child"]
#[allow(
    clippy::too_many_lines,
    reason = "One track's concerns, from the first raised to the revision that answers the last"
)]
async fn concerns_block_claims_until_answered_or_dismissed() -> Result<()> {
    let url = std::env::var("CANNERY_CONCERNS_HTTP_DATABASE_URL")?;
    let objects = std::env::temp_dir().join(format!("cannery-concerns-{}", std::process::id()));
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
    let plans = "/api/projects/matrix/tracks/alpha/plans";
    let draft = format!("{plans}/draft");
    let concerns = "/api/projects/matrix/tracks/alpha/concerns";

    // An approved plan of two units, and the agent claims the first.
    let (status, value) = call(
        &app,
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
    let (status, value) = call(&app, "researcher", "POST", plans, None).await?;
    assert_eq!(status, 201, "{value}");
    let (status, value) = call(
        &app,
        "researcher",
        "PUT",
        &format!("{draft}/approach"),
        Some(json!({"approach": "Establish a baseline, then vary one thing."})),
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    for (key, title) in [("baseline", "Baseline"), ("variant", "Variant")] {
        let (status, value) = call(
            &app,
            "researcher",
            "POST",
            &format!("{draft}/units"),
            Some(unit(key, title)),
        )
        .await?;
        assert_eq!(status, 201, "{value}");
    }
    approve(&app, 1).await?;
    let (status, claimed) = claim(&app).await?;
    assert_eq!(status, 201, "{claimed}");
    let number = claimed["attempt"]["number"].as_i64().ok_or("number")?;
    let sequence = claimed["attempt"]["sequence"].as_i64().ok_or("sequence")?;

    // A concern names a hypothesis and an attempt of its own track only.
    let (status, value) = call(
        &app,
        "agent",
        "POST",
        concerns,
        Some(json!({"document": "---\nkind: blocker\nhypothesis: 99\n---\nNo such hypothesis.\n"})),
    )
    .await?;
    assert_eq!(status, 422, "{value}");
    let (status, value) = call(
        &app,
        "agent",
        "POST",
        concerns,
        Some(json!({"document": "---\nkind: blocker\n---\n"})),
    )
    .await?;
    assert_eq!(status, 422, "{value}");
    let (status, value) = call(
        &app,
        "agent",
        "POST",
        concerns,
        Some(json!({"document": "---\nkind: complaint\n---\nNot a kind.\n"})),
    )
    .await?;
    assert_eq!(status, 422, "{value}");
    let (status, value) = call(
        &app,
        "viewer",
        "POST",
        concerns,
        Some(json!({"document": "---\nkind: other\n---\nViewers only read.\n"})),
    )
    .await?;
    assert_eq!(status, 403, "{value}");

    // The agent raises one from its attempt; the next claim is refused.
    let (status, first) = call(
        &app,
        "agent",
        "POST",
        concerns,
        Some(json!({"document": format!(
            "---\nkind: wrong_assumption\nhypothesis: {number}\nattempt: {sequence}\n---\nThe train split leaks into test.\n"
        )})),
    )
    .await?;
    assert_eq!(status, 201, "{first}");
    assert_eq!(first["state"], "open");
    assert_eq!(first["kind"], "wrong_assumption");
    assert_eq!(first["hypothesis"], number);
    assert_eq!(first["attempt"], sequence);
    assert_eq!(first["raised_by_kind"], "service");
    assert_eq!(first["raised_by_name"], "fixture-agent");
    assert_eq!(first["via_channel"], "api");
    assert_eq!(first["body"], "The train split leaks into test.\n");
    let first_id = first["id"].as_str().ok_or("id")?.to_owned();
    let (status, refused) = claim(&app).await?;
    assert_eq!(
        (status, &refused["error"]["code"]),
        (409, &json!("concern_open")),
        "{refused}"
    );
    // Work already claimed goes on.
    let attempt: String = sqlx::query_scalar("SELECT state FROM attempts WHERE sequence = $1")
        .bind(i32::try_from(sequence)?)
        .fetch_one(&state.pool)
        .await?;
    assert_eq!(attempt, "claimed");
    let (status, open) = call(
        &app,
        "viewer",
        "GET",
        &format!("{concerns}?state=open"),
        None,
    )
    .await?;
    assert_eq!(status, 200, "{open}");
    assert_eq!(open["items"].as_array().map(Vec::len), Some(1), "{open}");
    let (status, read) = call(
        &app,
        "viewer",
        "GET",
        &format!("/api/projects/matrix/concerns/{first_id}"),
        None,
    )
    .await?;
    assert_eq!(status, 200, "{read}");
    assert_eq!(read["id"], first_id);

    // Only researchers dismiss, with a reason.
    let dismissal = format!("/api/projects/matrix/concerns/{first_id}/dismissal");
    for role in ["agent", "member"] {
        let (status, value) = call(
            &app,
            role,
            "POST",
            &dismissal,
            Some(json!({"reason": "Not a leak."})),
        )
        .await?;
        assert_eq!(status, 403, "{role} {value}");
    }
    let (status, value) = call(
        &app,
        "researcher",
        "POST",
        &dismissal,
        Some(json!({"reason": " "})),
    )
    .await?;
    assert_eq!(status, 422, "{value}");
    let (status, dismissed) = call(
        &app,
        "researcher",
        "POST",
        &dismissal,
        Some(json!({"reason": "The splits are disjoint by seed."})),
    )
    .await?;
    assert_eq!(status, 200, "{dismissed}");
    assert_eq!(dismissed["state"], "dismissed");
    assert_eq!(dismissed["dismissed_by_name"], "researcher");
    assert_eq!(
        dismissed["dismissal_reason"],
        "The splits are disjoint by seed."
    );
    let (status, value) = call(
        &app,
        "researcher",
        "POST",
        &dismissal,
        Some(json!({"reason": "Again."})),
    )
    .await?;
    assert_eq!(status, 409, "{value}");

    // A member raises another; a plan revision answers it.
    let (status, second) = call(
        &app,
        "member",
        "POST",
        concerns,
        Some(json!({"document": "---\nkind: better_idea\n---\nTry the variant on a larger split first.\n"})),
    )
    .await?;
    assert_eq!(status, 201, "{second}");
    assert_eq!(second["raised_by_kind"], "user");
    assert_eq!(second["hypothesis"], Value::Null);
    let second_id = second["id"].as_str().ok_or("id")?.to_owned();
    let (status, refused) = claim(&app).await?;
    assert_eq!(
        (status, &refused["error"]["code"]),
        (409, &json!("concern_open"))
    );
    let (status, revision) = call(&app, "researcher", "POST", plans, None).await?;
    assert_eq!(status, 201, "{revision}");
    assert_eq!(revision["needs_answer"][0]["id"], second_id, "{revision}");
    let (status, value) = call(
        &app,
        "researcher",
        "PUT",
        &format!("{draft}/alignments/{number}"),
        Some(json!({"decision": "keep", "reason": "The baseline stands."})),
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    let (_, check) = call(&app, "researcher", "GET", &format!("{draft}/check"), None).await?;
    assert_eq!(check["ready"], false, "{check}");
    assert_eq!(
        check["problems"][0]["code"], "unanswered_concern",
        "{check}"
    );
    let answer = format!("{draft}/answers/{second_id}");
    let (status, value) = call(
        &app,
        "agent",
        "PUT",
        &answer,
        Some(json!({"how": "Variant runs on the larger split."})),
    )
    .await?;
    assert_eq!(status, 403, "{value}");
    let (status, value) = call(
        &app,
        "researcher",
        "PUT",
        &format!("{draft}/answers/{first_id}"),
        Some(json!({"how": "Too late."})),
    )
    .await?;
    assert_eq!(status, 409, "{value}");
    let (status, value) = call(
        &app,
        "researcher",
        "PUT",
        &answer,
        Some(json!({"how": "Variant runs on the larger split."})),
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    assert_eq!(value["concern"], second_id, "{value}");
    assert_eq!(value["kind"], "better_idea");
    let (status, opened) = call(&app, "researcher", "GET", &format!("{plans}/2"), None).await?;
    assert_eq!(status, 200, "{opened}");
    assert_eq!(opened["answers"][0]["concern"], second_id, "{opened}");
    assert_eq!(opened["needs_answer"], json!([]), "{opened}");
    let (_, check) = call(&app, "researcher", "GET", &format!("{draft}/check"), None).await?;
    assert_eq!(check["ready"], true, "{check}");
    approve(&app, 2).await?;
    let (status, answered) = call(
        &app,
        "viewer",
        "GET",
        &format!("/api/projects/matrix/concerns/{second_id}"),
        None,
    )
    .await?;
    assert_eq!(status, 200, "{answered}");
    assert_eq!(answered["state"], "answered");
    assert_eq!(answered["answered_by_revision"], 2);
    let (status, markdown) =
        call(&app, "viewer", "GET", &format!("{plans}/2/plan.md"), None).await?;
    assert_eq!(status, 200);
    assert!(
        markdown
            .as_str()
            .ok_or("plan.md")?
            .contains("Variant runs on the larger split."),
        "{markdown}"
    );

    // No concern is open: the track's next hypothesis is claimed again.
    let (status, claimed) = claim(&app).await?;
    assert_eq!(status, 201, "{claimed}");
    let (status, all) = call(&app, "viewer", "GET", "/api/projects/matrix/concerns", None).await?;
    assert_eq!(status, 200, "{all}");
    let closed: Vec<_> = all["items"]
        .as_array()
        .ok_or("items")?
        .iter()
        .map(|item| item["state"].clone())
        .collect();
    assert_eq!(closed, vec![json!("answered"), json!("dismissed")], "{all}");
    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_events WHERE subject_type = 'concern' ORDER BY seq",
    )
    .fetch_all(&state.pool)
    .await?;
    assert_eq!(
        actions,
        [
            "concern.raised",
            "concern.dismissed",
            "concern.raised",
            "concern.answered"
        ]
    );
    Ok(())
}
