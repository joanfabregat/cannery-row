//! Track plans over HTTP: drafting through the write routes, the checks that
//! block a submission, the researcher's review, the approval that writes the
//! hypotheses, the claim that pins the plan and the context bundle it names.
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
INSERT INTO users(id,issuer,subject,email,email_verified,display_name,is_admin) SELECT ('00000000-0000-0000-0000-'||lpad(i::text,12,'0'))::uuid,'https://plans.fixture',role,role||'@fixture.invalid',true,role,i=1 FROM (VALUES(1,'admin'),(2,'researcher'),(3,'member'),(4,'viewer')) AS u(i,role);
INSERT INTO projects(id,slug,title,created_by) VALUES('00000000-0000-0000-0000-000000000010','matrix','Matrix','00000000-0000-0000-0000-000000000001');
INSERT INTO memberships(project_id,user_id,role,granted_by) SELECT '00000000-0000-0000-0000-000000000010',('00000000-0000-0000-0000-'||lpad(i::text,12,'0'))::uuid,role,'00000000-0000-0000-0000-000000000001' FROM (VALUES(2,'researcher'),(3,'member'),(4,'viewer')) AS u(i,role);
INSERT INTO api_tokens(token_hash,display_prefix,kind,user_id,name,scopes,expires_at) SELECT sha256(convert_to('cr_pat_plans_'||role,'UTF8')),'cr_pat_fixture','personal',('00000000-0000-0000-0000-'||lpad(i::text,12,'0'))::uuid,role,ARRAY['read','write'],now()+interval '1 day' FROM (VALUES(2,'researcher'),(3,'member'),(4,'viewer')) AS u(i,role);
INSERT INTO service_accounts(id,project_id,kind,name,created_by) VALUES('00000000-0000-0000-0000-000000000020','00000000-0000-0000-0000-000000000010','agent','fixture-agent','00000000-0000-0000-0000-000000000001');
INSERT INTO api_tokens(token_hash,display_prefix,kind,service_account_id,name,scopes,expires_at) VALUES(sha256(convert_to('cr_svc_plans_agent','UTF8')),'cr_svc_fixture','service','00000000-0000-0000-0000-000000000020','agent',ARRAY['read','write'],now()+interval '1 day');
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
            "sandbox": "Authored isolated plan fixture",
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
        (
            "experiment_manifests",
            "name",
            "experiment",
            step("experiment", "experiment", "sheet", "/cr/outputs/sheet"),
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
        .header("authorization", format!("Bearer {prefix}plans_{role}"));
    let body = if let Some(body) = body {
        builder = builder.header("content-type", "application/json");
        Body::from(body.to_string())
    } else {
        Body::empty()
    };
    let response = app.clone().oneshot(builder.body(body)?).await?;
    let status = response.status().as_u16();
    let text = response
        .headers()
        .get("content-type")
        .is_some_and(|value| value.as_bytes().starts_with(b"text/markdown"));
    let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024).await?;
    let value = if bytes.is_empty() {
        Value::Null
    } else if text || status == 500 {
        Value::String(String::from_utf8(bytes.to_vec())?)
    } else {
        serde_json::from_slice(&bytes)?
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

async fn actions(pool: &PgPool) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT action FROM audit_events WHERE subject_type = 'plan_revision' ORDER BY seq",
    )
    .fetch_all(pool)
    .await?)
}

#[tokio::test]
#[ignore = "requires positive exact selection and guarded fresh migrated child"]
#[allow(
    clippy::too_many_lines,
    reason = "One track's plan history, from the first draft to a re-plan that obsoletes a claimed unit"
)]
async fn a_track_is_planned_reviewed_and_claimed() -> Result<()> {
    let url = std::env::var("CANNERY_PLANS_HTTP_DATABASE_URL")?;
    let objects = std::env::temp_dir().join(format!("cannery-plans-{}", std::process::id()));
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

    // A new track has no plan, and nothing in it can be claimed.
    let (status, page) = call(&app, "viewer", "GET", plans, None).await?;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["items"], json!([]));
    let (status, _) = call(&app, "viewer", "GET", &format!("{plans}/current"), None).await?;
    assert_eq!(status, 404);
    let (status, claimed) = call(
        &app,
        "agent",
        "POST",
        "/api/projects/matrix/claims",
        Some(json!({})),
    )
    .await?;
    assert_eq!(
        (status, &claimed["error"]["code"]),
        (409, &json!("nothing_to_claim"))
    );

    // Only researchers author plans; agents and members are refused.
    for role in ["agent", "member", "viewer"] {
        let (status, value) = call(&app, role, "POST", plans, None).await?;
        assert_eq!(status, 403, "{role} {value}");
    }
    let (status, first) = call(&app, "researcher", "POST", plans, None).await?;
    assert_eq!(status, 201, "{first}");
    assert_eq!(first["revision"], 1);
    assert_eq!(first["state"], "draft");
    assert_eq!(first["via_channel"], "api");
    let (status, value) = call(&app, "researcher", "POST", plans, None).await?;
    assert_eq!((status, &value["error"]["code"]), (409, &json!("conflict")));

    // Limits are per project and refused at the write call with size and limit.
    let (status, limits) = call(&app, "member", "GET", "/api/projects/matrix/limits", None).await?;
    assert_eq!(status, 200, "{limits}");
    assert_eq!(limits["brief_max_bytes"], 65_536);
    assert_eq!(limits["plan_approach_max_bytes"], 65_536);
    assert_eq!(limits["unit_brief_max_bytes"], 32_768);
    assert_eq!(limits["context_items_max"], 64);
    let mut tight = limits.clone();
    tight["plan_approach_max_bytes"] = json!(1024);
    tight["context_items_max"] = json!(1);
    let (status, _) = call(
        &app,
        "agent",
        "PUT",
        "/api/projects/matrix/limits",
        Some(tight.clone()),
    )
    .await?;
    assert_eq!(status, 403);
    let (status, value) = call(
        &app,
        "researcher",
        "PUT",
        "/api/projects/matrix/limits",
        Some(tight),
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    let (status, value) = call(
        &app,
        "researcher",
        "PUT",
        &format!("{draft}/approach"),
        Some(json!({"approach": "a".repeat(1025)})),
    )
    .await?;
    assert_eq!(status, 422, "{value}");
    assert_eq!(value["error"]["details"][0]["path"], "body/approach");
    assert_eq!(value["error"]["details"][0]["size"], 1025);
    assert_eq!(value["error"]["details"][0]["limit"], 1024);
    let mut crowded = unit("crowded", "Crowded");
    crowded["context"] = json!([
        {"kind": "unit", "unit": "baseline"},
        {"kind": "unit", "unit": "variant"},
    ]);
    let (status, value) = call(
        &app,
        "researcher",
        "POST",
        &format!("{draft}/units"),
        Some(crowded),
    )
    .await?;
    assert_eq!(status, 422, "{value}");
    assert_eq!(value["error"]["details"][0]["limit"], 1);
    let (status, _) = call(
        &app,
        "researcher",
        "PUT",
        "/api/projects/matrix/limits",
        Some(limits),
    )
    .await?;
    assert_eq!(status, 200);

    // The draft: an approach and two units, the second derived from the first.
    let (status, value) = call(
        &app,
        "researcher",
        "PUT",
        &format!("{draft}/approach"),
        Some(json!({"approach": "Establish a baseline, then vary one thing."})),
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    let (status, value) = call(
        &app,
        "agent",
        "POST",
        &format!("{draft}/units"),
        Some(unit("baseline", "Baseline")),
    )
    .await?;
    assert_eq!(status, 403, "{value}");
    // The hypothesis document a unit becomes is checked, under the unit's names.
    let mut unknown = unit("baseline", "Baseline");
    unknown["acceptance"]["primary_metric"] = json!("accuracy");
    let (status, value) = call(
        &app,
        "researcher",
        "POST",
        &format!("{draft}/units"),
        Some(unknown),
    )
    .await?;
    assert_eq!(status, 422, "{value}");
    assert_eq!(
        value["error"]["details"][0]["path"],
        "body/acceptance/primary_metric"
    );
    let (status, value) = call(
        &app,
        "researcher",
        "POST",
        &format!("{draft}/units"),
        Some(unit("baseline", "Baseline")),
    )
    .await?;
    assert_eq!(status, 201, "{value}");
    let (status, value) = call(
        &app,
        "researcher",
        "POST",
        &format!("{draft}/units"),
        Some(unit("baseline", "Baseline")),
    )
    .await?;
    assert_eq!(status, 409, "{value}");
    let mut variant = unit("variant", "Variant");
    variant["relations"] = json!([{"kind": "derived_from", "unit": "baseline"}]);
    variant["context"] = json!([{"kind": "unit", "unit": "baseline", "note": "the reference run"}]);
    let (status, value) = call(
        &app,
        "researcher",
        "POST",
        &format!("{draft}/units"),
        Some(variant),
    )
    .await?;
    assert_eq!(status, 201, "{value}");
    assert_eq!(value["key"], "variant");
    assert_eq!(value["number"], Value::Null);

    // No project brief yet: the check says so and the submission is refused.
    let (status, check) = call(&app, "researcher", "GET", &format!("{draft}/check"), None).await?;
    assert_eq!(status, 200, "{check}");
    assert_eq!(check["ready"], false);
    assert_eq!(check["problems"][0]["code"], "no_brief");
    let (status, value) = call(
        &app,
        "researcher",
        "POST",
        &format!("{draft}/submission"),
        None,
    )
    .await?;
    assert_eq!(status, 409, "{value}");
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
    let (_, check) = call(&app, "researcher", "GET", &format!("{draft}/check"), None).await?;
    assert_eq!(check["ready"], true, "{check}");

    // Submitted, sent back with a reason, and revised.
    let (status, submitted) = call(
        &app,
        "researcher",
        "POST",
        &format!("{draft}/submission"),
        None,
    )
    .await?;
    assert_eq!(status, 200, "{submitted}");
    assert_eq!(submitted["state"], "submitted");
    let review = format!("{plans}/1/review");
    let (status, _) = call(
        &app,
        "agent",
        "POST",
        &review,
        Some(json!({"action": "approve", "reason": "ok"})),
    )
    .await?;
    assert_eq!(status, 403);
    let (status, value) = call(
        &app,
        "researcher",
        "POST",
        &review,
        Some(json!({"action": "approve", "reason": " "})),
    )
    .await?;
    assert_eq!(status, 422, "{value}");
    let (status, sent) = call(
        &app,
        "researcher",
        "POST",
        &review,
        Some(json!({"action": "send_back", "reason": "Name the seed in the variant."})),
    )
    .await?;
    assert_eq!(status, 200, "{sent}");
    assert_eq!(sent["state"], "sent_back");
    let (status, second) = call(&app, "researcher", "POST", plans, None).await?;
    assert_eq!(status, 201, "{second}");
    assert_eq!(second["revision"], 2);
    assert_eq!(second["based_on"], 1);
    assert_eq!(second["units"].as_array().map(Vec::len), Some(2));
    let (status, value) = call(
        &app,
        "researcher",
        "PUT",
        &format!("{draft}/units/variant"),
        Some(json!({"title": "Variant, seed 7"})),
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    let (status, _) = call(
        &app,
        "researcher",
        "POST",
        &format!("{draft}/units"),
        Some(unit("extra", "Extra")),
    )
    .await?;
    assert_eq!(status, 201);
    let (status, value) = call(
        &app,
        "researcher",
        "DELETE",
        &format!("{draft}/units/extra"),
        None,
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    assert_eq!(value["units"].as_array().map(Vec::len), Some(2));
    let (status, _) = call(
        &app,
        "researcher",
        "POST",
        &format!("{draft}/submission"),
        None,
    )
    .await?;
    assert_eq!(status, 200);
    let (status, approved) = call(
        &app,
        "researcher",
        "POST",
        &format!("{plans}/2/review"),
        Some(json!({"action": "approve", "reason": "Ready to run."})),
    )
    .await?;
    assert_eq!(status, 200, "{approved}");
    assert_eq!(approved["state"], "approved");

    // Approval wrote two queued hypotheses and activated the track.
    let (status, track) = call(
        &app,
        "viewer",
        "GET",
        "/api/projects/matrix/tracks/alpha",
        None,
    )
    .await?;
    assert_eq!(status, 200);
    assert_eq!(track["state"], "active", "{track}");
    let (status, units) = call(
        &app,
        "viewer",
        "GET",
        "/api/projects/matrix/tracks/alpha/units",
        None,
    )
    .await?;
    assert_eq!(status, 200, "{units}");
    let index: Vec<_> = units["items"]
        .as_array()
        .ok_or("units")?
        .iter()
        .map(|unit| {
            (
                unit["number"].clone(),
                unit["key"].clone(),
                unit["state"].clone(),
            )
        })
        .collect();
    assert_eq!(index.len(), 2, "{units}");
    assert!(
        index.iter().all(|(_, _, state)| state == "queued"),
        "{units}"
    );
    let (status, variant) =
        call(&app, "viewer", "GET", "/api/projects/matrix/units/2", None).await?;
    assert_eq!(status, 200, "{variant}");
    assert_eq!(variant["title"], "Variant, seed 7");
    assert_eq!(variant["plan_revision"], 2);
    let (status, history) = call(
        &app,
        "viewer",
        "GET",
        "/api/projects/matrix/units/2/history",
        None,
    )
    .await?;
    assert_eq!(status, 200, "{history}");
    let (status, markdown) =
        call(&app, "viewer", "GET", &format!("{plans}/2/plan.md"), None).await?;
    assert_eq!(status, 200);
    let markdown = markdown.as_str().ok_or("plan.md")?;
    assert!(
        markdown.contains("Establish a baseline, then vary one thing."),
        "{markdown}"
    );
    assert!(markdown.contains("Variant, seed 7"), "{markdown}");
    let (status, current) = call(&app, "member", "GET", &format!("{plans}/current"), None).await?;
    assert_eq!(status, 200);
    assert_eq!(current["revision"], 2);

    // A claim pins the approved revision and names its context bundle.
    let (status, claim) = call(
        &app,
        "agent",
        "POST",
        "/api/projects/matrix/claims",
        Some(json!({})),
    )
    .await?;
    assert_eq!(status, 201, "{claim}");
    assert_eq!(claim["plan"]["revision"], 2, "{claim}");
    let number = claim["attempt"]["number"].as_i64().ok_or("number")?;
    let sequence = claim["attempt"]["sequence"].as_i64().ok_or("sequence")?;
    let bundle = format!("/api/projects/matrix/hypotheses/{number}/attempts/{sequence}/context.md");
    assert_eq!(claim["context"]["ref"], bundle);
    let (status, text) = call(&app, "agent", "GET", &bundle, None).await?;
    assert_eq!(status, 200, "{text}");
    let text = text.as_str().ok_or("context.md")?;
    assert_eq!(claim["context"]["bytes"], text.len(), "{text}");
    assert!(text.contains(&format!("bytes: {}", text.len())), "{text}");
    for part in [
        "Rank the matrix fixtures.",
        "Establish a baseline",
        "Keep the seed fixed.",
    ] {
        assert!(text.contains(part), "{part}: {text}");
    }
    let (status, compact) = call(
        &app,
        "agent",
        "GET",
        &format!("{bundle}?detail=compact"),
        None,
    )
    .await?;
    assert_eq!(status, 200);
    assert!(compact.as_str().ok_or("compact")?.len() <= 16_384);
    let pinned: Option<i32> =
        sqlx::query_scalar("SELECT plan_revision FROM attempts WHERE sequence = $1")
            .bind(i32::try_from(sequence)?)
            .fetch_one(&state.pool)
            .await?;
    assert_eq!(pinned, Some(2));

    // Re-planning names every in-flight unit; obsolete cancels its attempt.
    let (status, third) = call(&app, "researcher", "POST", plans, None).await?;
    assert_eq!(status, 201, "{third}");
    assert_eq!(third["needs_alignment"][0]["number"], number, "{third}");
    let (_, check) = call(&app, "researcher", "GET", &format!("{draft}/check"), None).await?;
    assert_eq!(check["problems"][0]["code"], "missing_alignment", "{check}");
    let (status, value) = call(
        &app,
        "researcher",
        "PUT",
        &format!("{draft}/alignments/{number}"),
        Some(json!({"decision": "maybe", "reason": "?"})),
    )
    .await?;
    assert_eq!(status, 422, "{value}");
    let (status, value) = call(
        &app,
        "researcher",
        "PUT",
        &format!("{draft}/alignments/{number}"),
        Some(json!({"decision": "obsolete", "reason": "The question changed."})),
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    let (_, check) = call(&app, "researcher", "GET", &format!("{draft}/check"), None).await?;
    assert_eq!(check["ready"], true, "{check}");
    let (status, _) = call(
        &app,
        "researcher",
        "POST",
        &format!("{draft}/submission"),
        None,
    )
    .await?;
    assert_eq!(status, 200);
    let (status, value) = call(
        &app,
        "researcher",
        "POST",
        &format!("{plans}/3/review"),
        Some(json!({"action": "approve", "reason": "Stop the old run."})),
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    let attempt: String = sqlx::query_scalar("SELECT state FROM attempts WHERE sequence = $1")
        .bind(i32::try_from(sequence)?)
        .fetch_one(&state.pool)
        .await?;
    assert_eq!(attempt, "cancelled");
    let (_, obsolete) = call(
        &app,
        "viewer",
        "GET",
        &format!("/api/projects/matrix/units/{number}"),
        None,
    )
    .await?;
    assert_eq!(obsolete["obsolete"], true, "{obsolete}");
    // Redo: the in-flight unit is obsoleted and a new unit derived from it is queued.
    let (status, claim) = call(
        &app,
        "agent",
        "POST",
        "/api/projects/matrix/claims",
        Some(json!({})),
    )
    .await?;
    assert_eq!(status, 201, "{claim}");
    assert_eq!(claim["plan"]["revision"], 3, "{claim}");
    let redone = claim["attempt"]["number"].as_i64().ok_or("number")?;
    let (status, _) = call(&app, "researcher", "POST", plans, None).await?;
    assert_eq!(status, 201);
    let (status, fourth) = call(
        &app,
        "researcher",
        "PUT",
        &format!("{draft}/alignments/{redone}"),
        Some(json!({"decision": "redo", "reason": "Run it again with the new seed."})),
    )
    .await?;
    assert_eq!(status, 200, "{fourth}");
    let (status, fourth) = call(&app, "member", "GET", &draft, None).await?;
    assert_eq!(status, 200, "{fourth}");
    let redo = fourth["units"]
        .as_array()
        .ok_or("units")?
        .iter()
        .find(|unit| unit["redo_of"] == redone)
        .ok_or("redo unit")?;
    assert_eq!(redo["relations"][0]["kind"], "derived_from", "{redo}");
    let (status, _) = call(
        &app,
        "researcher",
        "POST",
        &format!("{draft}/submission"),
        None,
    )
    .await?;
    assert_eq!(status, 200);
    let (status, value) = call(
        &app,
        "researcher",
        "POST",
        &format!("{plans}/4/review"),
        Some(json!({"action": "approve", "reason": "Redo it."})),
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    let (_, units) = call(
        &app,
        "viewer",
        "GET",
        "/api/projects/matrix/tracks/alpha/units?state=queued",
        None,
    )
    .await?;
    assert_eq!(units["items"].as_array().map(Vec::len), Some(1), "{units}");
    let (_, declined) = call(&app, "researcher", "POST", plans, None).await?;
    let (status, _) = call(
        &app,
        "researcher",
        "PUT",
        &format!("{draft}/approach"),
        Some(json!({"approach": "Something else."})),
    )
    .await?;
    assert_eq!(status, 200);
    let (status, _) = call(
        &app,
        "researcher",
        "POST",
        &format!("{draft}/submission"),
        None,
    )
    .await?;
    assert_eq!(status, 200);
    let (status, value) = call(
        &app,
        "researcher",
        "POST",
        &format!("{plans}/{}/review", declined["revision"]),
        Some(json!({"action": "decline", "reason": "Not now."})),
    )
    .await?;
    assert_eq!(
        (status, &value["state"]),
        (200, &json!("declined")),
        "{value}"
    );
    let (_, current) = call(&app, "viewer", "GET", &format!("{plans}/current"), None).await?;
    assert_eq!(current["revision"], 4);
    // Draft edits live in the revision; the audit records its lifecycle.
    assert_eq!(
        actions(&state.pool).await?,
        [
            "plan.started",
            "plan.submitted",
            "plan.sent_back",
            "plan.started",
            "plan.submitted",
            "plan.approved",
            "plan.started",
            "plan.submitted",
            "plan.approved",
            "plan.started",
            "plan.submitted",
            "plan.approved",
            "plan.started",
            "plan.submitted",
            "plan.declined",
        ]
    );
    let via: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT via_channel FROM audit_events WHERE subject_type = 'plan_revision'",
    )
    .fetch_all(&state.pool)
    .await?;
    assert_eq!(via, ["api"]);
    let lifecycle: Vec<String> = sqlx::query_scalar(
        "SELECT state FROM hypotheses WHERE track_id = '00000000-0000-0000-0000-000000000030' ORDER BY number",
    )
    .fetch_all(&state.pool)
    .await?;
    assert_eq!(lifecycle, ["cancelled", "cancelled", "queued"]);
    state.pool.close().await;
    let _ = std::fs::remove_dir_all(objects);
    Ok(())
}
