//! Installed native workers against the assembled Rust HTTP service and PostgreSQL.
#![forbid(unsafe_code)]
#[path = "support/lifecycle_support.rs"]
#[allow(
    dead_code,
    reason = "Shared public lifecycle fixture serves several suites"
)]
mod lifecycle;
#[path = "support/runner_security_semantics.rs"]
#[allow(
    dead_code,
    reason = "Shared installed tester and process ownership fixture"
)]
mod runner;
#[path = "support/workflow_support.rs"]
#[allow(
    dead_code,
    reason = "Shared public workflow bootstrap serves several suites"
)]
mod workflow;
use conformance::Result;
use lifecycle::{ATTEMPT, Call, World, sha, string};
use reqwest::Method;
use runner::{Rig, Running, fixture, producer, science, successful_process};
use serde_json::{Value, json};
use std::{
    fmt::Write as _,
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

const JUDGE: &str = r"import json
import os
from pathlib import Path
root = Path(os.environ['CR_ROOT'])
assert not any(key in os.environ for key in ['CANNERY_RUNNER_TOKEN_FILE', 'GITHUB_TOKEN', 'CANNERY_DATABASE_URL'])
job = json.loads((root / 'job.json').read_text())
assert job['role'] == 'evaluator' and job['metrics']
assert not any('lease' in key or 'token' in key for key in job)
pinned = json.loads((root / 'inputs/evidence/evidence.json').read_text())
manifest = json.loads((root / 'inputs/manifest/manifest.json').read_text())
assert pinned and manifest['objects']
measurement = next(m for item in pinned for m in item['record']['measurements'] if m['metric'] == 'mrr' and m['authority'] == 'tester_verified' and not m.get('dimensions'))
comparison = {key: measurement[key] for key in ['metric', 'split', 'value']}
comparison['dimensions'] = measurement.get('dimensions', {})
comparison.update(source='tester', reference={'value': 0.0, 'kind': 'other', 'label': 'zero witness'})
result = {'gates': [{'id':'actual-measurement', 'result':'pass'}], 'comparisons':[comparison], 'verdict':'pass', 'reason':'Verified actual tester measurement'}
(root / 'outputs/verdict/verdict.json').write_text(json.dumps(result))
print('native policy produced verified verdict')
";
fn private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    Ok(())
}
fn require_native_cli() -> Result<()> {
    if std::env::var("CANNERY_CONFORMANCE_SERVER_IMPLEMENTATION")?.as_str() != "rust" {
        return Err("native worker proof requires the explicitly selected installed CLI".into());
    }
    std::path::PathBuf::from(std::env::var("CANNERY_CONFORMANCE_CLI")?).canonicalize()?;
    Ok(())
}
async fn run_config(
    root: &Path,
    world: &World,
    token: &str,
    kind: &str,
    policy: Option<&Value>,
    steps: &Path,
) -> Result<()> {
    let token_path = root.join(format!("{kind}.token"));
    private_file(&token_path, token.as_bytes())?;
    let mut config = format!(
        "api_url = {}\nproject = {}\ndata_root = {}\nwork_root = {}\ncache_root = {}\n[launcher]\ntype = \"local\"\nstep_root = {}\n[[kinds]]\nkind = {}\ntoken_file = {}\n",
        json!(world.base_url),
        json!(world.project),
        json!(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../examples/fixture/data")
                .canonicalize()?
                .to_string_lossy()
        ),
        json!(root.join("work").to_string_lossy()),
        json!(root.join("cache").to_string_lossy()),
        json!(steps.to_string_lossy()),
        json!(kind),
        json!(token_path.to_string_lossy())
    );
    if let Some(policy) = policy {
        let path = root.join("policy.json");
        fs::write(&path, serde_json::to_vec(policy)?)?;
        writeln!(config, "policy = {}", json!(path.to_string_lossy()))?;
    }
    let path = root.join("worker.toml");
    fs::write(&path, config)?;
    let child = Command::new(std::env::var("CANNERY_CONFORMANCE_CLI")?)
        .args(["runner", "--config"])
        .arg(path)
        .arg("--once")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let output = Running::new(child).wait(Duration::from_secs(90)).await?;
    successful_process(&output);
    for bytes in [&output.stdout, &output.stderr] {
        assert!(!String::from_utf8_lossy(bytes).contains(token));
    }
    Ok(())
}
async fn jobs(world: &mut World, token: &str, number: i64) -> Result<Vec<Value>> {
    Ok(world
        .api(Call::get(
            &format!("{ATTEMPT}/jobs"),
            format!("{}/hypotheses/{number}/attempts/1/jobs", world.base()),
            token,
        ))
        .await?
        .body["items"]
        .as_array()
        .ok_or("job items")?
        .clone())
}
async fn verify_outputs(world: &mut World, token: &str, job: &Value, roles: &[&str]) -> Result<()> {
    let objects = job["outputs"].as_array().ok_or("job outputs")?;
    for role in roles {
        assert!(objects.iter().any(|object| object["role"] == *role));
    }
    for object in objects {
        let response = world
            .h
            .request(
                Method::GET,
                &format!("{}/artifacts/{}", world.base(), string(&object["id"])?),
            )?
            .bearer_auth(token)
            .send()
            .await?;
        let bytes = world
            .h
            .check_response(
                Method::GET,
                "/api/projects/{slug}/artifacts/{artifact_id}",
                response,
                200,
            )
            .await?
            .raw_body;
        assert_eq!(sha(&bytes), object["sha256"]);
        assert_eq!(bytes.len() as u64, object["size_bytes"]);
    }
    Ok(())
}
#[tokio::test]
#[ignore = "requires isolated assembled native HTTP, OIDC, PG and installed CLI"]
#[allow(
    clippy::too_many_lines,
    reason = "Keep real tester, evaluation, publication and refusal in one isolated project"
)]
async fn native_policy_workers_publish_and_reject_real_work() -> Result<()> {
    require_native_cli()?;
    let mut science = science()?;
    science["evaluator"]["revision"] = json!("fixture-policy-step-1");
    science["max_auto_retries"] = json!(0);
    let mut rig = Rig::new("native-policy", producer()?, science).await?;
    rig.script("native_judge.py", JUDGE)?;
    let mut policy = fixture("examples/fixture/policy-step.json")?;
    policy["step"]["spec"]["container"]["command"] = json!(["python3", "native_judge.py"]);
    let number = rig.submit().await?;
    successful_process(&rig.run().await?);
    let tested = rig.job(number).await?;
    assert_eq!(tested["state"], "completed");
    assert_eq!(tested["evidence"]["stage"], "tester");
    verify_outputs(
        &mut rig.world,
        &rig.actors.admin,
        &tested,
        &["evidence", "step_log"],
    )
    .await?;
    let eval_root = rig.work.0.join("evaluation");
    fs::create_dir(&eval_root)?;
    run_config(
        &eval_root,
        &rig.world,
        &rig.actors.evaluator,
        "eval",
        Some(&policy),
        &rig.work.0.join("steps"),
    )
    .await?;
    let stored = jobs(&mut rig.world, &rig.actors.admin, number).await?;
    let evaluated = stored
        .iter()
        .find(|job| job["stage"] == "evaluator")
        .ok_or("evaluator job")?;
    assert_eq!(evaluated["state"], "completed");
    assert_eq!(evaluated["evidence"]["assessment"]["verdict"], "pass");
    assert_eq!(
        evaluated["evidence"]["assessment"]["comparisons"]
            .as_array()
            .ok_or("comparisons")?
            .len(),
        1
    );
    verify_outputs(
        &mut rig.world,
        &rig.actors.admin,
        evaluated,
        &["step_log", "verdict", "policy_step"],
    )
    .await?;
    let attempt = rig
        .world
        .api(Call::get(
            ATTEMPT,
            format!("{}/hypotheses/{number}/attempts/1", rig.world.base()),
            &rig.actors.admin,
        ))
        .await?
        .body;
    assert_eq!(attempt["state"], "awaiting_human_review");
    let cases = rig
        .world
        .api(Call::get(
            "/api/projects/{slug}/review-cases",
            format!(
                "{}/review-cases?kind=result&state=pending",
                rig.world.base()
            ),
            &rig.actors.admin,
        ))
        .await?
        .body;
    assert_eq!(cases["items"].as_array().ok_or("review cases")?.len(), 1);
    assert_eq!(cases["items"][0]["hypothesis"], number);
    assert_eq!(cases["items"][0]["attempt_state"], "awaiting_human_review");
    let comparisons = rig
        .world
        .api(Call::get(
            "/api/projects/{slug}/comparisons",
            format!("{}/comparisons", rig.world.base()),
            &rig.actors.admin,
        ))
        .await?
        .body;
    assert_eq!(
        comparisons["items"]
            .as_array()
            .ok_or("published comparisons")?
            .len(),
        1
    );
    let audit = rig.world.h.fetch_audit(&rig.actors.admin, 0).await?.body;
    for job in [&tested, evaluated] {
        assert_eq!(
            audit["items"]
                .as_array()
                .ok_or("audit")?
                .iter()
                .filter(|row| row["action"] == "job.completed" && row["subject_id"] == job["id"])
                .count(),
            1
        );
    }
    assert_eq!(audit["items"].as_array().ok_or("audit")?.iter().filter(|row|row["action"] == "attempt.evaluated" && row["subject_id"] == attempt["id"]).count(),1);
    let negative = rig.submit().await?;
    successful_process(&rig.run().await?);
    rig.script("native_judge.py", "raise SystemExit(7)\n")?;
    let failed_root = rig.work.0.join("failed-evaluation");
    fs::create_dir(&failed_root)?;
    run_config(
        &failed_root,
        &rig.world,
        &rig.actors.evaluator,
        "eval",
        Some(&policy),
        &rig.work.0.join("steps"),
    )
    .await?;
    let stored = jobs(&mut rig.world, &rig.actors.admin, negative).await?;
    let failed = stored
        .iter()
        .find(|job| job["stage"] == "evaluator")
        .ok_or("failed evaluator job")?;
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["error_code"], "evaluator_error");
    assert!(failed["evidence"].is_null());
    verify_outputs(&mut rig.world, &rig.actors.admin, failed, &["step_log"]).await?;
    for root in [&eval_root, &failed_root] {
        assert_eq!(runner::entries(&root.join("work"))?, 0);
    }
    rig.assert_clean()?;
    rig.finish("native-policy-workers").await
}
#[tokio::test]
#[ignore = "requires isolated assembled native HTTP, OIDC, PG and installed CLI"]
async fn native_experiment_worker_submits_and_tester_consumes() -> Result<()> {
    require_native_cli()?;
    let mut rig = workflow::Rig::new("native-workflow").await?;
    let number = rig.queue("scripted").await?;
    let steps = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/fixture/steps")
        .canonicalize()?;
    run_config(
        &rig.work.0,
        &rig.world,
        &rig.worker,
        "experiment",
        None,
        &steps,
    )
    .await?;
    let attempt = rig.attempt(number, 1).await?;
    assert_eq!(attempt["state"], "testing");
    assert!(attempt.get("lease_expires_at").is_some_and(Value::is_null));
    let root = rig.work.0.join("testing");
    fs::create_dir(&root)?;
    run_config(&root, &rig.world, &rig.actors.tester, "test", None, &steps).await?;
    let stored = jobs(&mut rig.world, &rig.actors.admin, number).await?;
    let tested = stored
        .iter()
        .find(|job| job["stage"] == "tester")
        .ok_or("tester job")?;
    assert_eq!(tested["state"], "completed");
    assert_eq!(tested["evidence"]["stage"], "tester");
    verify_outputs(
        &mut rig.world,
        &rig.actors.admin,
        tested,
        &["evidence", "step_log"],
    )
    .await?;
    assert_eq!(rig.attempt(number, 1).await?["state"], "evaluating");
    let evaluation = rig.work.0.join("stock-evaluation");
    fs::create_dir(&evaluation)?;
    run_config(
        &evaluation,
        &rig.world,
        &rig.actors.evaluator,
        "eval",
        Some(&fixture("examples/fixture/evaluator.json")?),
        &steps,
    )
    .await?;
    let stored = jobs(&mut rig.world, &rig.actors.admin, number).await?;
    let evaluated = stored
        .iter()
        .find(|job| job["stage"] == "evaluator")
        .ok_or("stock evaluator job")?;
    assert_eq!(evaluated["state"], "completed");
    assert_eq!(
        rig.attempt(number, 1).await?["state"],
        "awaiting_human_review"
    );
    assert_eq!(runner::entries(&evaluation.join("work"))?, 0);
    assert_eq!(runner::entries(&root.join("work"))?, 0);
    assert_eq!(runner::entries(&rig.work.0.join("work"))?, 0);
    rig.finish("native-experiment-workers").await
}
