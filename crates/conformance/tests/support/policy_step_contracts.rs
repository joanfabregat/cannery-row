include!("evaluator_control_semantics.rs");

fn install_policy_observer(c: &ControlWorld, policy: &Value) -> Result<()> {
    let script = r"import json
from pathlib import Path
from fixture_step import job, read_json, root
import policy_step
base = root()
Path(POLICY_MARKER).write_text('launched', encoding='utf-8')
details = job()
evidence = read_json(base / 'inputs/evidence/evidence.json')
manifest = read_json(base / 'inputs/manifest/manifest.json')
inputs = base / 'inputs'
observation = {
    'role': details['role'], 'evaluator': details['evaluator'],
    'metrics': [m['key'] for m in details['metrics']],
    'control': details.get('control'), 'parameters': details['parameters'],
    'lease_present': 'lease' in json.dumps(details),
    'evidence': [{'keys': sorted(item), 'stage': item['record']['stage']} for item in evidence],
    'manifest_roles': sorted({item['role'] for item in manifest['objects']}),
    'input_names': sorted(p.name for p in inputs.iterdir()),
    'input_files': {p.name: sorted(f.name for f in p.iterdir()) for p in inputs.iterdir()},
    'datasets': details['inputs']['datasets'],
}
print('conformance-policy-contract:' + json.dumps(observation, sort_keys=True), flush=True)
raise SystemExit(policy_step.main())
";
    let marker = serde_json::to_string(&c.work.0.join("policy-launched").to_string_lossy())?;
    fs::write(c.work.0.join("steps/observe_policy.py"), script.replace("POLICY_MARKER", &marker))?;
    let mut policy = policy.clone();
    policy["step"]["spec"]["container"]["command"] = json!(["python3","observe_policy.py"]);
    fs::write(c.work.0.join("observed-policy.json"), serde_json::to_vec(&policy)?)?;
    let fixture_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/fixture").canonicalize()?;
    fs::write(c.work.0.join("observed-policy.toml"), config(&c.work.0, &c.world.base, &c.project, &fixture_root, "eval", "evaluator.token", Some("observed-policy.json")).replace(&toml_string(&fixture_root.join("steps").to_string_lossy()), &toml_string(&c.work.0.join("steps").to_string_lossy())))?;
    Ok(())
}
async fn run_policy(c: &ControlWorld, expected: &str) -> Result<()> {
    success(&cli(vec!["runner".into(), "--config".into(), c.work.0.join("observed-policy.toml").display().to_string(), "--once".into()], None).await?, expected);
    Ok(())
}
async fn policy_observation(c: &mut ControlWorld, job: &Value) -> Result<Value> {
    let mut observations = Vec::new();
    for artifact in job["outputs"].as_array().ok_or("outputs missing")? {
        if artifact["role"] != "step_log" { continue; }
        let response = c.world.harness.request(Method::GET, &format!("{}/artifacts/{}", c.base, string(artifact,"id")?))?.bearer_auth(&c.admin).send().await?;
        let checked = c.world.harness.check_response(Method::GET, "/api/projects/{slug}/artifacts/{artifact_id}", response, 200).await?;
        for line in std::str::from_utf8(&checked.raw_body)?.lines() {
            if let Some(value) = line.strip_prefix("conformance-policy-contract:") {
                observations.push(serde_json::from_str::<Value>(value)?);
            }
        }
    }
    assert_eq!(observations.len(), 1, "policy must publish exactly one observation");
    Ok(observations.remove(0))
}

fn copy_policy_data(source: &std::path::Path, destination: &std::path::Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_policy_data(&entry.path(), &target)?;
        } else if entry.file_type()?.is_file() {
            fs::copy(entry.path(), target)?;
        } else {
            return Err("policy fixture data contains a non-regular entry".into());
        }
    }
    Ok(())
}
