//! Source-backed public HTTP and actual experiment CLI workflow failure semantics.
//! Migration 0014, SQL constraints, monkeypatched heartbeat interruption and retained
//! private worker directories remain separate from these black-box scenarios.
#![allow(
    clippy::too_many_lines,
    reason = "Keep each source-backed scenario together"
)]
#[allow(dead_code)]
#[path = "support/lifecycle_support.rs"]
mod lifecycle;
#[path = "support/workflow_support.rs"]
mod workflow;
use conformance::Result;
use lifecycle::{ATTEMPT, Call, Lease};
use serde_json::{Value, json};
use workflow::{Rig, code, experiment, fixture, member, predecessor, track, violation};

#[tokio::test]
#[ignore = "requires isolated HTTP/OIDC/CLI conformance deployment"]
async fn workflow_registration_mode_and_role_routing() -> Result<()> {
    let mut rig = Rig::new("workflow-routing").await?;
    let researcher = member(&mut rig, "researcher", "researcher").await?;
    let viewer = member(&mut rig, "viewer", "viewer").await?;
    let rejected = rig
        .post(
            "/experiment-steps",
            "/experiment-steps",
            fixture("examples/fixture/producers/overlap-producer.json")?,
            422,
        )
        .await?;
    code(&rejected, "validation_failed");
    let revision = rig
        .post("/experiment-steps", "/experiment-steps", experiment()?, 201)
        .await?;
    assert_eq!(revision["revision"], 2);
    rig.world
        .api(Call::post(
            "/api/projects/{slug}/experiment-steps",
            format!("{}/experiment-steps", rig.world.base()),
            &researcher,
            experiment()?,
            403,
        ))
        .await?;
    let listed = rig
        .world
        .api(Call::get(
            "/api/projects/{slug}/experiment-steps",
            format!("{}/experiment-steps", rig.world.base()),
            &viewer,
        ))
        .await?
        .body;
    assert_eq!(
        listed["items"]
            .as_array()
            .ok_or("items")?
            .iter()
            .map(|v| json!([v["name"], v["revision"]]))
            .collect::<Vec<_>>(),
        vec![
            json!(["fixture-experiment", 2]),
            json!(["fixture-experiment", 1])
        ]
    );
    let registered = rig
        .world
        .api(Call::get(
            "/api/projects/{slug}/experiment-steps/{name}/{revision}",
            format!("{}/experiment-steps/fixture-experiment/1", rig.world.base()),
            &rig.actors.agent,
        ))
        .await?
        .body;
    assert_eq!(registered["content"]["spec"]["role"], "experiment");
    let producers = rig.get("/producers", "/producers").await?;
    assert_eq!(producers["items"].as_array().ok_or("producers")?.len(), 1);
    assert_eq!(producers["items"][0]["name"], "overlap-producer");
    // The stock Rust validator locates a false property schema at that property;
    // Python's validator reports the containing object instead.
    let forbidden_workflow_path =
        if std::env::var("CANNERY_CONFORMANCE_SERVER_IMPLEMENTATION").as_deref() == Ok("rust") {
            "/workflow"
        } else {
            ""
        };
    for (change, path) in [
        (
            json!({"workflow":{"steps":[{"name":"fixture-experiment","revision":9}]}}),
            "/workflow/steps/0",
        ),
        (json!({"workflow":null}), ""),
        (json!({"mode":"agent"}), forbidden_workflow_path),
        (
            json!({"workflow":{"steps":[{"name":"fixture-experiment","revision":1},{"name":"fixture-experiment","revision":1}]}}),
            "/workflow/steps/0/spec/outputs/artifacts/1/name",
        ),
    ] {
        let mut value = track()?;
        value["slug"] = json!("rejected");
        for (key, value_change) in change.as_object().ok_or("change")? {
            if value_change.is_null() {
                value.as_object_mut().ok_or("track")?.remove(key);
            } else {
                value[key] = value_change.clone();
            }
        }
        violation(&rig.post("/tracks", "/tracks", value, 422).await?, path);
    }
    let workflow = json!({"steps":[{"name":"fixture-experiment","revision":1}]});
    for patch in [
        json!({"expected_revision":1,"mode":"workflow"}),
        json!({"expected_revision":1,"mode":"workflow","reason":"scripted now"}),
    ] {
        code(
            &rig.patch("/tracks/lexical", "/tracks/{track_slug}", patch, 422)
                .await?,
            "validation_failed",
        );
    }
    let changed=rig.patch("/tracks/lexical","/tracks/{track_slug}",json!({"expected_revision":1,"mode":"workflow","workflow":workflow,"reason":"scripted now"}),200).await?;
    assert_eq!(changed["mode"], "workflow");
    assert_eq!(changed["workflow"], workflow);
    let back = rig
        .patch(
            "/tracks/lexical",
            "/tracks/{track_slug}",
            json!({"expected_revision":2,"mode":"agent","reason":"agents again"}),
            200,
        )
        .await?;
    assert_eq!(back["mode"], "agent");
    assert!(back["workflow"].is_null());
    let events = rig.audit().await?;
    let changed: Vec<_> = events
        .iter()
        .filter(|e| e["action"] == "track.mode_changed")
        .map(|e| {
            json!([
                e["prior_state"]["mode"],
                e["new_state"]["mode"],
                e["reason"]
            ])
        })
        .collect();
    assert_eq!(
        changed,
        vec![
            json!(["agent", "workflow", "scripted now"]),
            json!(["workflow", "agent", "agents again"])
        ]
    );
    let scripted = rig.queue("scripted").await?;
    let lexical = rig.world.queue(&rig.actors, "Agent routing").await?;
    let summaries = rig.get("/hypotheses", "/hypotheses").await?;
    let modes: std::collections::BTreeMap<_, _> = summaries["items"]
        .as_array()
        .ok_or("summaries")?
        .iter()
        .map(|h| (h["number"].as_i64(), h["mode"].clone()))
        .collect();
    assert_eq!(
        modes,
        std::collections::BTreeMap::from([
            (Some(scripted), json!("workflow")),
            (Some(lexical), json!("agent"))
        ])
    );
    let agent = rig.actors.agent.clone();
    let worker = rig.worker.clone();
    for (token, body) in [
        (&agent, json!({"mode":"workflow"})),
        (&researcher, json!({"mode":"workflow"})),
        (&worker, json!({"mode":"agent"})),
    ] {
        rig.claim_as(token, body, 403).await?;
    }
    for (token, number) in [(&agent, scripted), (&worker, lexical)] {
        code(
            &rig.claim_as(token, json!({"hypothesis":number}), 409)
                .await?,
            "nothing_to_claim",
        );
    }
    rig.world
        .api(Call::post(
            "/api/projects/{slug}/hypotheses",
            format!("{}/hypotheses", rig.world.base()),
            &worker,
            fixture("examples/fixture/workflow-hypothesis.json")?,
            403,
        ))
        .await?;
    let claimed = rig.claim_as(&agent, json!({}), 201).await?;
    assert_eq!(claimed["attempt"]["number"], lexical);
    assert_eq!(claimed["attempt"]["mode"], "agent");
    assert!(claimed["workflow"].is_null());
    assert!(claimed["attempt"]["workflow"].is_null());
    let agent_lease = Lease::attempt(&claimed)?;
    for body in [
        json!({"reason":"crashed","code":"step_failed"}),
        json!({"reason":"crashed","step":"x"}),
    ] {
        rig.release(&agent, &agent_lease, body, 403).await?;
    }
    let mut denied = Call::get(
        &format!("{ATTEMPT}/inputs/predecessor/{{artifact_id}}"),
        format!(
            "{}/inputs/predecessor/00000000-0000-0000-0000-000000000000",
            rig.world.attempt_path(&agent_lease)
        ),
        &agent,
    )
    .lease(&agent_lease)?;
    denied.status = 403;
    rig.world.api(denied).await?;
    rig.release(&agent, &agent_lease, json!({"reason":"gave up"}), 200)
        .await?;
    assert_eq!(
        rig.hypothesis(lexical).await?["state"],
        "awaiting_human_review"
    );
    let failed = rig.attempt(lexical, 1).await?;
    assert_eq!(failed["failures"][0]["code"], "released");
    assert_eq!(failed["failures"][0]["requeued"], false);
    let claimed = rig.claim_as(&worker, json!({}), 201).await?;
    assert_eq!(claimed["attempt"]["number"], scripted);
    assert_eq!(claimed["attempt"]["mode"], "workflow");
    assert_eq!(claimed["attempt"]["workflow"], workflow);
    assert_eq!(claimed["workflow"]["parameters"], json!({"top_k":2}));
    assert!(claimed["workflow"]["inputs"]["predecessor"].is_null());
    assert!(claimed["workflow"]["deadline"].is_string());
    violation(
        &rig.release(
            &worker,
            &Lease::attempt(&claimed)?,
            json!({"reason":"no code"}),
            422,
        )
        .await?,
        "/code",
    );
    code(
        &rig.claim_as(&worker, json!({}), 409).await?,
        "nothing_to_claim",
    );
    rig.finish("routing").await
}

#[tokio::test]
#[ignore = "requires isolated HTTP/OIDC/CLI conformance deployment"]
async fn workflow_unavailable_track_is_skipped_and_named() -> Result<()> {
    let mut rig = Rig::new("workflow-unavailable").await?;
    let mut slow = experiment()?;
    slow["metadata"]["name"] = json!("slow-experiment");
    slow["spec"]["activeDeadlineSeconds"] = json!(600);
    rig.post("/experiment-steps", "/experiment-steps", slow, 201)
        .await?;
    let mut slow = track()?;
    slow["slug"] = json!("slow");
    slow["workflow"] = json!({"steps":[{"name":"slow-experiment","revision":1}]});
    rig.post("/tracks", "/tracks", slow, 201).await?;
    let stuck = rig.queue("slow").await?;
    let scripted = rig.queue("scripted").await?;
    let mut science = fixture("examples/fixture/science.json")?;
    science["limits"]["max_deadline_seconds"] = json!(300);
    rig.post("/config/science", "/config/{kind}", science, 201)
        .await?;
    let worker = rig.worker.clone();
    let claim = rig.claim_as(&worker, json!({}), 201).await?;
    assert_eq!(claim["attempt"]["number"], scripted);
    for body in [json!({}), json!({"hypothesis":stuck})] {
        let error = rig.claim_as(&worker, body, 409).await?;
        code(&error, "workflow_unavailable");
        assert_eq!(
            error["error"]["details"]
                .as_array()
                .ok_or("details")?
                .iter()
                .map(|v| v["path"].clone())
                .collect::<Vec<_>>(),
            vec![json!("/tracks/slow")]
        );
    }
    assert_eq!(rig.hypothesis(stuck).await?["state"], "queued");
    rig.finish("unavailable").await
}

fn failure<'a>(attempt: &'a Value, expected: &str, requeued: bool) -> &'a Value {
    assert_eq!(attempt["failures"].as_array().map(Vec::len), Some(1));
    let failures = &attempt["failures"];
    assert_eq!(failures[0]["code"], expected);
    assert_eq!(failures[0]["requeued"], requeued);
    &failures[0]
}
#[tokio::test]
#[ignore = "requires isolated HTTP/OIDC/CLI conformance deployment; lease TTL 60 seconds"]
async fn workflow_cli_step_failure_retries_then_opens_review() -> Result<()> {
    let mut rig = Rig::new("workflow-retry").await?;
    rig.bind(json!(["python3", "-c", "import sys; sys.exit(4)"]), 60)
        .await?;
    let number = rig.queue("scripted").await?;
    rig.run("failed").await?;
    assert_eq!(rig.hypothesis(number).await?["state"], "queued");
    let first = rig.attempt(number, 1).await?;
    failure(&first, "step_failed", true);
    rig.run("failed").await?;
    assert_eq!(
        rig.hypothesis(number).await?["state"],
        "awaiting_human_review"
    );
    let second = rig.attempt(number, 2).await?;
    assert_eq!(second["predecessor_id"], first["id"]);
    for (attempt, requeued) in [(&first, true), (&second, false)] {
        let failed = failure(attempt, "step_failed", requeued);
        assert_eq!(failed["details"]["step"], "fixture-experiment");
        let logs: Vec<_> = attempt["artifacts"]
            .as_array()
            .ok_or("artifacts")?
            .iter()
            .filter(|a| a["role"] == "step_log")
            .collect();
        assert_eq!(logs.len(), 1);
        assert_eq!(
            failed["log_refs"],
            json!([{"key":logs[0]["storage"]["key"],"size_bytes":logs[0]["size_bytes"],"sha256":logs[0]["sha256"]}])
        );
    }
    let cases = rig
        .get("/review-cases?kind=failure&state=pending", "/review-cases")
        .await?;
    assert_eq!(cases["items"].as_array().ok_or("cases")?.len(), 1);
    assert_eq!(cases["items"][0]["kind"], "failure");
    assert_eq!(cases["items"][0]["state"], "pending");
    let events = rig.audit().await?;
    let retries: Vec<_> = events
        .iter()
        .filter(|e| e["action"] == "hypothesis.requeued")
        .collect();
    assert_eq!(retries.len(), 1);
    assert_eq!(retries[0]["new_state"]["code"], "step_failed");
    assert_eq!(retries[0]["new_state"]["retry"], 1);
    assert_eq!(retries[0]["new_state"]["max_auto_retries"], 1);
    let failures: Vec<_> = events
        .iter()
        .filter(|e| e["action"] == "attempt.failed")
        .collect();
    assert_eq!(failures.len(), 2);
    assert_eq!(failures[0]["new_state"]["requeued"], true);
    assert!(failures[1]["new_state"]["requeued"].is_null());
    let accounts = rig.get("/service-accounts", "/service-accounts").await?;
    let worker = accounts["items"]
        .as_array()
        .ok_or("accounts")?
        .iter()
        .find(|account| account["name"] == "workflow-runner")
        .ok_or("workflow worker")?;
    for event in failures {
        assert_eq!(event["new_state"]["step"], "fixture-experiment");
        assert_eq!(event["actor_kind"], "service");
        assert_eq!(event["actor_service_id"], worker["id"]);
        assert!(event["actor_user_id"].is_null());
    }
    rig.finish("retry").await
}

// Extend the source fixture with a nonsecret readout of actual staged inputs.
// This reaches the CLI's normally-cleaned job.json without retaining private dirs.
const RESUME: &str = r#"import os, pathlib, runpy, json
root = pathlib.Path(os.environ.get("CR_ROOT", "/cr"))
if not (root / "inputs" / "candidate" / "candidate.json").is_file():
    for name, text in (("candidate", '{"top_k": 5}'), ("claimed_sheet", "[]")):
        (root / "outputs" / name).mkdir(parents=True, exist_ok=True)
        (root / "outputs" / name / (name + ".json")).write_text(text)
else:
    runpy.run_path("experiment.py", run_name="__main__")
    sheet_path = root / "outputs" / "claimed_sheet" / "claimed_sheet.json"
    sheet = json.loads(sheet_path.read_text())
    sheet['report']['configuration'] = json.dumps({'predecessor': json.loads((root / 'job.json').read_text())['inputs']['predecessor'], 'candidate': json.loads((root / 'inputs' / 'candidate' / 'candidate.json').read_text())})
    sheet_path.write_text(json.dumps(sheet))
"#;
const REFUSED: &str = r#"import os, pathlib
root = pathlib.Path(os.environ.get("CR_ROOT", "/cr"))
for name, text in (("candidate", '{"top_k": 5}'), ("claimed_sheet", '{"report": "none"}')):
    (root / "outputs" / name).mkdir(parents=True, exist_ok=True)
    (root / "outputs" / name / (name + ".json")).write_text(text)
"#;
#[tokio::test]
#[ignore = "requires isolated HTTP/OIDC/CLI conformance deployment; lease TTL 60 seconds"]
async fn workflow_cli_run_failure_resumes_and_submission_failure_does_not_retry() -> Result<()> {
    let mut rig = Rig::new("workflow-resume").await?;
    rig.bind(json!(["python3", "-c", RESUME]), 60).await?;
    let number = rig.queue("scripted").await?;
    rig.run("failed").await?;
    assert_eq!(rig.hypothesis(number).await?["state"], "queued");
    let first = rig.attempt(number, 1).await?;
    failure(&first, "invalid_step_output", true);
    rig.run("testing").await?;
    let second = rig.attempt(number, 2).await?;
    assert_eq!(second["predecessor_id"], first["id"]);
    assert_eq!(
        second["claimed_sheet"]["report"]["observations"],
        format!("Resumed from #{number}.1's candidate {{'top_k': 5}}.")
    );
    let staged: Value = serde_json::from_str(
        second["claimed_sheet"]["report"]["configuration"]
            .as_str()
            .ok_or("configuration")?,
    )?;
    assert_eq!(
        staged,
        json!({"predecessor":{"attempt_id":first["id"],"ref":format!("#{number}.1"),"state":"failed","failure_code":"invalid_step_output"},"candidate":{"top_k":5}})
    );
    // A separate queued run exposes the predecessor endpoint while its lease is live.
    let number = rig.queue("scripted").await?;
    rig.run("failed").await?;
    let first = rig.attempt(number, 1).await?;
    let worker = rig.worker.clone();
    let claimed = rig
        .claim_as(&worker, json!({"hypothesis":number}), 201)
        .await?;
    let lease = Lease::attempt(&claimed)?;
    let listed = &claimed["workflow"]["inputs"]["predecessor"];
    assert_eq!(listed["attempt_id"], first["id"]);
    assert_eq!(listed["ref"], format!("#{number}.1"));
    assert_eq!(listed["state"], "failed");
    assert_eq!(listed["failure_code"], "invalid_step_output");
    assert_eq!(
        listed["artifacts"]
            .as_array()
            .ok_or("predecessor artifacts")?
            .iter()
            .map(|a| a["role"].clone())
            .collect::<Vec<_>>(),
        vec![json!("candidate")]
    );
    let artifacts = first["artifacts"].as_array().ok_or("artifacts")?;
    let candidate = artifacts
        .iter()
        .find(|a| a["role"] == "candidate")
        .ok_or("candidate")?;
    let log = artifacts
        .iter()
        .find(|a| a["role"] == "step_log")
        .ok_or("log")?;
    assert_eq!(
        predecessor(&mut rig, &lease, candidate["id"].as_str().ok_or("id")?, 200).await?,
        json!({"top_k":5})
    );
    code(
        &predecessor(&mut rig, &lease, log["id"].as_str().ok_or("id")?, 404).await?,
        "not_found",
    );
    rig.release(&worker,&lease,json!({"reason":"stop predecessor inspection","code":"step_failed","step":"fixture-experiment"}),200).await?;
    rig.bind(json!(["python3", "-c", REFUSED]), 60).await?;
    let number = rig.queue("scripted").await?;
    rig.run("failed").await?;
    assert_eq!(
        rig.hypothesis(number).await?["state"],
        "awaiting_human_review"
    );
    failure(&rig.attempt(number, 1).await?, "invalid_submission", false);
    rig.finish("resume").await
}

// Genuine elapsed deadlines and leases; the original SQL-expiry/heartbeat
// monkeypatch tests additionally exercise private runner cancellation internals.
async fn expire_workflow(deadline: bool) -> Result<()> {
    let expected_ttl = if deadline { "60" } else { "3" };
    if std::env::var("CANNERY_LEASES_TTL_SECONDS")?.as_str() != expected_ttl {
        return Err(format!(
            "workflow expiry selector requires --lease-ttl-seconds {expected_ttl}"
        )
        .into());
    }
    if std::env::var("CANNERY_LEASES_JOB_OVERHEAD_SECONDS")?.as_str() != "0" {
        return Err("workflow expiry requires zero staging overhead profile".into());
    }
    let mut rig = Rig::new(if deadline {
        "workflow-deadline"
    } else {
        "workflow-lease"
    })
    .await?;
    rig.bind(
        json!(["python3", "experiment.py"]),
        if deadline { 1 } else { 120 },
    )
    .await?;
    let number = rig.queue("scripted").await?;
    let worker = rig.worker.clone();
    let claimed = rig
        .claim_as(&worker, json!({"hypothesis":number}), 201)
        .await?;
    let lease = Lease::attempt(&claimed)?;
    tokio::time::sleep(std::time::Duration::from_millis(if deadline {
        1500
    } else {
        3500
    }))
    .await;
    let late = rig
        .world
        .api(
            Call::post(
                &format!("{ATTEMPT}/heartbeat"),
                format!("{}/heartbeat", rig.world.attempt_path(&lease)),
                &worker,
                json!({}),
                409,
            )
            .lease(&lease)?,
        )
        .await?
        .body;
    code(&late, "stale_lease");
    let report = rig.world.h.sweep(&rig.actors.admin).await?;
    assert_eq!(report["attempts_expired"], 1);
    assert_eq!(report["attempts_requeued"], 1);
    assert_eq!(rig.hypothesis(number).await?["state"], "queued");
    let attempt = rig.attempt(number, 1).await?;
    failure(
        &attempt,
        if deadline {
            "deadline_exceeded"
        } else {
            "lease_expired"
        },
        true,
    );
    let cases = rig
        .get("/review-cases?kind=failure", "/review-cases")
        .await?;
    assert_eq!(cases["items"], json!([]));
    let events = rig.audit().await?;
    let failed: Vec<_> = events
        .iter()
        .filter(|e| e["action"] == "attempt.failed")
        .collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["actor_kind"], "system");
    assert_eq!(
        failed[0]["new_state"]["code"],
        if deadline {
            "deadline_exceeded"
        } else {
            "lease_expired"
        }
    );
    let second = rig.world.h.sweep(&rig.actors.admin).await?;
    assert_eq!(second["attempts_expired"], 0);
    assert_eq!(second["attempts_requeued"], 0);
    rig.finish(if deadline { "deadline" } else { "lease" })
        .await
}
#[tokio::test]
#[ignore = "requires --lease-ttl-seconds 60 and zero staging overhead"]
async fn workflow_real_deadline_sweep_requeues_once() -> Result<()> {
    expire_workflow(true).await
}
#[tokio::test]
#[ignore = "requires separate --lease-ttl-seconds 3 profile and zero staging overhead"]
async fn workflow_real_lease_expiry_sweep_requeues_once() -> Result<()> {
    expire_workflow(false).await
}

#[tokio::test]
#[ignore = "requires isolated HTTP/OIDC/CLI conformance deployment; lease TTL 60 seconds"]
async fn workflow_output_constraints_and_hyphenated_dataset_staging() -> Result<()> {
    let mut rig = Rig::new("workflow-constraints").await?;
    for (index, outputs, path) in [
        (
            0,
            json!([{"name":"candidate","interface":"fixture-candidate/v1","path":"/cr/outputs/candidate"}]),
            "/workflow/steps/0",
        ),
        (
            1,
            json!([{"name":"claimed_sheet","interface":"cr-evidence/v0.2","path":"/cr/outputs/claimed_sheet"}]),
            "/workflow/steps",
        ),
        (
            2,
            json!([{"name":"step_log","interface":"fixture-candidate/v1","path":"/cr/outputs/step_log"},{"name":"claimed_sheet","interface":"cr-evidence/v0.2","path":"/cr/outputs/claimed_sheet"}]),
            "/workflow/steps/0/spec/outputs/artifacts/0/name",
        ),
    ] {
        let mut manifest = experiment()?;
        let name = format!("invalid-output-{index}");
        manifest["metadata"]["name"] = json!(name);
        manifest["spec"]["outputs"]["artifacts"] = outputs;
        rig.post("/experiment-steps", "/experiment-steps", manifest, 201)
            .await?;
        let mut value = track()?;
        value["slug"] = json!(format!("bad-output-{index}"));
        value["workflow"] = json!({"steps":[{"name":name,"revision":1}]});
        violation(&rig.post("/tracks", "/tracks", value, 422).await?, path);
    }
    let mut peeking = experiment()?;
    peeking["spec"]["inputs"]["artifacts"]
        .as_array_mut()
        .ok_or("inputs")?
        .push(json!({"name":"qrels","from":"dataset","path":"/cr/inputs/qrels"}));
    violation(
        &rig.post("/experiment-steps", "/experiment-steps", peeking, 422)
            .await?,
        "/spec/inputs/artifacts/2",
    );
    let mut science = fixture("examples/fixture/science.json")?;
    science["datasets"]
        .as_array_mut()
        .ok_or("datasets")?
        .extend([
            json!({"id":"nanobeir-queries","revision":"queries-r1","held_out_labels":false}),
            json!({"id":"nanobeir-qrels","revision":"qrels-r1","held_out_labels":true}),
        ]);
    rig.post("/config/science", "/config/{kind}", science, 201)
        .await?;
    let mut peeking = experiment()?;
    peeking["spec"]["inputs"]["artifacts"].as_array_mut().ok_or("inputs")?.push(json!({"name":"labels","from":"dataset","id":"nanobeir-qrels","path":"/cr/inputs/labels"}));
    violation(
        &rig.post("/experiment-steps", "/experiment-steps", peeking, 422)
            .await?,
        "/spec/inputs/artifacts/2",
    );
    let root = rig.work.0.join("hyphenated-data");
    let source = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/fixture/data/datasets");
    for (name, revision, file) in [
        ("queries", "queries-r1", "queries.json"),
        ("qrels", "qrels-r1", "qrels.json"),
    ] {
        let target = root
            .join("datasets")
            .join(format!("nanobeir-{name}"))
            .join(revision);
        std::fs::create_dir_all(&target)?;
        std::fs::copy(
            source.join(name).join(revision).join(file),
            target.join(file),
        )?;
    }
    // Assert the actual local worker's dataset metadata and mounted files before
    // executing the source fixture. CLI cleanup need not expose private dirs.
    let mut manifest = experiment()?;
    manifest["spec"]["inputs"]["artifacts"][1]["id"] = json!("nanobeir-queries");
    manifest["spec"]["container"]["command"] = json!([
        "python3",
        "-c",
        r"import runpy
from fixture_step import job, root
assert job()['inputs']['datasets'] == [{'name':'queries','id':'nanobeir-queries','revision':'queries-r1'}]
assert sorted(p.name for p in (root()/'inputs'/'queries').iterdir()) == ['queries.json']
runpy.run_path('experiment.py', run_name='__main__')
"
    ]);
    let registered = rig
        .post("/experiment-steps", "/experiment-steps", manifest, 201)
        .await?;
    rig.patch("/tracks/scripted","/tracks/{track_slug}",json!({"expected_revision":1,"workflow":{"steps":[{"name":"fixture-experiment","revision":registered["revision"]}]},"reason":"Use hyphenated registered dataset"}),200).await?;
    let number = rig.queue("scripted").await?;
    rig.run("testing").await?;
    assert_eq!(rig.attempt(number, 1).await?["state"], "testing");
    rig.finish("constraints").await
}
