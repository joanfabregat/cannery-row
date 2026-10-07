//! CLI workers use the same HTTP API and trusted local fixture as a deployment.
#![allow(clippy::too_many_arguments, clippy::too_many_lines)]
include!("support/runner_cli.rs");
use ring::digest::{SHA256, digest};
use std::{
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

fn sha(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in digest(&SHA256, bytes).as_ref() {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 15)]));
    }
    output
}
struct Work(PathBuf);
impl Drop for Work {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn private_file(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    Ok(())
}
async fn cli(args: Vec<String>, token_env: Option<(&str, &std::path::Path)>) -> Result<Output> {
    let binary = std::env::var("CANNERY_CONFORMANCE_CLI")?;
    let token_env = token_env.map(|(name, path)| (name.to_owned(), path.to_owned()));
    tokio::task::spawn_blocking(move || {
        let mut command = Command::new(binary);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some((name, path)) = token_env {
            command.env(name, path);
        }
        let mut child = command.spawn()?;
        let until = Instant::now() + Duration::from_secs(45);
        loop {
            if child.try_wait()?.is_some() {
                return Ok(child.wait_with_output()?);
            }
            if Instant::now() > until {
                child.kill()?;
                let _ = child.wait();
                return Err("CLI worker exceeded 45 seconds".into());
            }
            thread::sleep(Duration::from_millis(25));
        }
    })
    .await?
}
fn success(output: &Output, expected: &str) {
    assert!(
        output.status.success(),
        "CLI failed with exit status {}",
        output.status
    );
    if std::env::var("CANNERY_CONFORMANCE_SERVER_IMPLEMENTATION").as_deref() == Ok("rust") {
        return;
    }
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(expected),
        "CLI omitted expected outcome {expected}"
    );
}
fn toml_string(value: &str) -> String {
    Value::String(value.to_owned()).to_string()
}
async fn account(
    world: &mut World,
    admin: &Session,
    token: &str,
    base: &str,
    kind: &str,
    name: &str,
) -> Result<String> {
    world
        .api(
            Method::POST,
            "/api/projects/{slug}/service-accounts",
            &format!("{base}/service-accounts"),
            None,
            Some(token),
            Some(json!({"kind":kind,"name":name})),
            201,
        )
        .await?;
    let response = world
        .api(
            Method::POST,
            "/api/projects/{slug}/service-accounts/{name}/tokens",
            &format!("{base}/service-accounts/{name}/tokens"),
            Some(admin),
            None,
            Some(json!({"name":"cli","expires_in_days":1,"scopes":["read","write"]})),
            201,
        )
        .await?;
    Ok(string(&response, "token")?.to_owned())
}
async fn lease_api(
    world: &mut World,
    method: Method,
    template: &str,
    path: &str,
    token: &str,
    lease: &Value,
    body: Value,
    status: u16,
) -> Result<Value> {
    let response = world
        .harness
        .request(method.clone(), path)?
        .bearer_auth(token)
        .header("X-Lease-Token", string(lease, "lease_token")?)
        .header(
            "X-Lease-Generation",
            lease["lease_generation"]
                .as_u64()
                .ok_or("missing generation")?,
        )
        .json(&body)
        .send()
        .await?;
    Ok(world
        .harness
        .check_response(method, template, response, status)
        .await?
        .body)
}
async fn draft(
    world: &mut World,
    base: &str,
    agent: &str,
    admin: &str,
    document: Value,
) -> Result<u64> {
    let created = world
        .api(
            Method::POST,
            "/api/projects/{slug}/hypotheses",
            &format!("{base}/hypotheses"),
            None,
            Some(agent),
            Some(document),
            201,
        )
        .await?;
    let number = created["number"].as_u64().ok_or("draft omitted number")?;
    world
        .api(
            Method::POST,
            "/api/projects/{slug}/hypotheses/{number}/draft-review",
            &format!("{base}/hypotheses/{number}/draft-review"),
            None,
            Some(admin),
            Some(json!({"draft_revision":1,"action":"approve","reason":"Fixture CLI conformance"})),
            200,
        )
        .await?;
    Ok(number)
}
async fn submit(
    world: &mut World,
    base: &str,
    agent: &str,
    admin: &str,
    track: &str,
) -> Result<u64> {
    let mut document = fixture("examples/fixture/hypothesis.json")?;
    document["track"] = json!(track);
    let number = draft(world, base, agent, admin, document).await?;
    let claim = world
        .api(
            Method::POST,
            "/api/projects/{slug}/claims",
            &format!("{base}/claims"),
            None,
            Some(agent),
            Some(json!({"hypothesis":number})),
            201,
        )
        .await?;
    let attempt = &claim["attempt"];
    let path = format!(
        "{base}/hypotheses/{number}/attempts/{}",
        attempt["sequence"]
    );
    let bytes = fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/fixture/candidate.json"),
    )?;
    let grant=lease_api(world,Method::POST,"/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/uploads",&format!("{path}/uploads"),agent,&claim,json!({"role":"candidate","name":"candidate.json","size_bytes":bytes.len(),"sha256":sha(&bytes),"media_type":"application/json"}),201).await?;
    let upload_url = reqwest::Url::parse(string(&grant, "upload_url")?)?;
    let base_url = reqwest::Url::parse(&world.base)?;
    assert!(
        upload_url.origin() == base_url.origin(),
        "upload capability must stay on the API origin"
    );
    let mut upload_path = upload_url.path().to_owned();
    if let Some(query) = upload_url.query() {
        upload_path.push('?');
        upload_path.push_str(query);
    }
    let mut request = world.harness.request(Method::PUT, &upload_path)?;
    for (name, value) in grant["headers"]
        .as_object()
        .ok_or("upload headers absent")?
    {
        request = request.header(name, value.as_str().ok_or("upload header not string")?);
    }
    let uploaded = world
        .harness
        .check_response(
            Method::PUT,
            "/api/uploads/{upload_id}",
            request.body(bytes).send().await?,
            201,
        )
        .await?
        .body;
    let mut object = json!({});
    for key in ["role", "storage", "size_bytes", "sha256", "media_type"] {
        object[key] = uploaded[key].clone();
    }
    let manifest = lease_api(
        world,
        Method::POST,
        "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/manifest",
        &format!("{path}/manifest"),
        agent,
        &claim,
        json!({"schema_version":"0.2","attempt_id":attempt["id"],"objects":[object]}),
        201,
    )
    .await?;
    let mut evidence = fixture("tests/fixtures/contracts/evidence_envelope/valid/agent.json")?;
    evidence["attempt_id"] = attempt["id"].clone();
    evidence["manifest"] = manifest;
    evidence["artifact_roles"] = json!(["candidate"]);
    evidence["measurements"] = json!([{"metric":"mrr","value":0.9,"authority":"agent_claim","unit":"ratio","direction":"higher","split":"dev","dimensions":{"language":"en"}}]);
    evidence["provenance"]["science_revision"] = json!("1");
    let submitted = lease_api(
        world,
        Method::POST,
        "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/submission",
        &format!("{path}/submission"),
        agent,
        &claim,
        evidence,
        201,
    )
    .await?;
    assert_eq!(submitted["state"], "testing");
    Ok(number)
}
async fn jobs(world: &mut World, base: &str, number: u64, token: &str) -> Result<Vec<Value>> {
    let response = world
        .api(
            Method::GET,
            "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/jobs",
            &format!("{base}/hypotheses/{number}/attempts/1/jobs"),
            None,
            Some(token),
            None,
            200,
        )
        .await?;
    Ok(response["items"]
        .as_array()
        .ok_or("jobs omitted items")?
        .clone())
}
async fn inspect_test_job(
    world: &mut World,
    base: &str,
    number: u64,
    token: &str,
    producer: &str,
) -> Result<()> {
    let listed = jobs(world, base, number, token).await?;
    let job = listed
        .iter()
        .find(|job| job["stage"] == "tester")
        .ok_or("tester job absent")?;
    let detail = world
        .api(
            Method::GET,
            "/api/projects/{slug}/jobs/{job_id}",
            &format!("{base}/jobs/{}", string(job, "id")?),
            None,
            Some(token),
            None,
            200,
        )
        .await?;
    assert_eq!(detail["state"], "completed");
    assert_eq!(detail["steps"][0]["name"], producer);
    let mut values = std::collections::BTreeMap::new();
    for measurement in detail["evidence"]["measurements"]
        .as_array()
        .ok_or("measurements absent")?
    {
        assert_eq!(measurement["authority"], "tester_verified");
        let language = measurement["dimensions"]["language"]
            .as_str()
            .unwrap_or("all");
        values.insert(
            language.to_owned(),
            measurement["value"]
                .as_f64()
                .ok_or("measurement value absent")?,
        );
    }
    assert_eq!(values.len(), 3);
    assert!(values.values().all(|v| (0.0..=1.0).contains(v)));
    if producer == "overlap-producer" {
        assert!(values.values().all(|v| (*v - 1.0).abs() < f64::EPSILON));
    }
    let outputs = detail["outputs"].as_array().ok_or("outputs absent")?;
    assert!(outputs.iter().any(|o| o["role"] == "run"
        && o["interface"] == "ranked-run/v1"
        && o["content_validated"] == true));
    let mut producer_seen = false;
    for output in outputs.iter().filter(|o| o["role"] == "step_log") {
        let response = world
            .harness
            .request(
                Method::GET,
                &format!("{base}/artifacts/{}", string(output, "id")?),
            )?
            .bearer_auth(token)
            .send()
            .await?;
        let checked = world
            .harness
            .check_response(
                Method::GET,
                "/api/projects/{slug}/artifacts/{artifact_id}",
                response,
                200,
            )
            .await?;
        let log = String::from_utf8(checked.raw_body)?;
        if log.contains("producer inputs:") {
            producer_seen = true;
            assert!(log.contains("job.json"));
            assert!(log.contains("inputs/queries/queries.json"));
            assert!(!log.contains("qrels"));
            assert!(!log.contains("cr_job_"));
            assert!(!log.contains("held-out-labels"));
        }
    }
    assert!(
        producer_seen,
        "producer's local contract guard did not execute"
    );
    Ok(())
}
fn config(
    root: &std::path::Path,
    api: &str,
    project: &str,
    fixture_root: &std::path::Path,
    kind: &str,
    token: &str,
    policy: Option<&str>,
) -> String {
    let mut config = format!(
        "api_url = {}\nproject = {}\ndata_root = {}\nwork_root = {}\ncache_root = {}\n[launcher]\ntype = \"local\"\nstep_root = {}\n[[kinds]]\nkind = {}\ntoken_file = {}\n",
        toml_string(api),
        toml_string(project),
        toml_string(&fixture_root.join("data").to_string_lossy()),
        toml_string(&root.join("work").to_string_lossy()),
        toml_string(&root.join("cache").to_string_lossy()),
        toml_string(&fixture_root.join("steps").to_string_lossy()),
        toml_string(kind),
        toml_string(token)
    );
    if let Some(policy) = policy {
        config.push_str("policy = ");
        config.push_str(&toml_string(policy));
        config.push('\n');
    }
    config
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn local_runner_evaluator_and_workflow_cli() -> Result<()> {
    let mut world = World::new()?;
    let project = unique()?.replace("conformance", "cli");
    let root = PathBuf::from(std::env::var("CANNERY_CONFORMANCE_WORK_DIR")?).join(&project);
    fs::create_dir(&root)?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    let work = Work(root);
    let fixture_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/fixture")
        .canonicalize()?;
    let admin = world
        .login("conformance-admin", "admin@conformance.test")
        .await?;
    assert_ne!(admin.id, "");
    let (token, _) = world
        .token(&admin, "runner-cli-admin", &["read", "write"])
        .await?;
    let base = format!("/api/projects/{project}");
    world
        .api(
            Method::POST,
            "/api/projects",
            "/api/projects",
            None,
            Some(&token),
            Some(json!({"slug":project,"title":"CLI fixture"})),
            201,
        )
        .await?;
    world
        .api(
            Method::PUT,
            "/api/projects/{slug}/members/{user_id}",
            &format!("{base}/members/{}", admin.id),
            None,
            Some(&token),
            Some(json!({"role":"researcher"})),
            200,
        )
        .await?;
    let agent = account(&mut world, &admin, &token, &base, "agent", "cli-agent").await?;
    let tester = account(
        &mut world,
        &admin,
        &token,
        &base,
        "tester",
        "cannery-runner",
    )
    .await?;
    let evaluator = account(
        &mut world,
        &admin,
        &token,
        &base,
        "evaluator",
        "stock-evaluator",
    )
    .await?;
    let experimenter = account(
        &mut world,
        &admin,
        &token,
        &base,
        "experimenter",
        "cli-experimenter",
    )
    .await?;
    for (name, value) in [
        ("tester.token", &tester),
        ("evaluator.token", &evaluator),
        ("experimenter.token", &experimenter),
    ] {
        private_file(&work.0.join(name), value.as_bytes())?;
    }
    world
        .api(
            Method::POST,
            "/api/projects/{slug}/config/{kind}",
            &format!("{base}/config/science"),
            None,
            Some(&token),
            Some(fixture("examples/fixture/science.json")?),
            201,
        )
        .await?;
    for name in ["overlap-producer", "trigram-producer"] {
        world
            .api(
                Method::POST,
                "/api/projects/{slug}/producers",
                &format!("{base}/producers"),
                None,
                Some(&token),
                Some(fixture(&format!("examples/fixture/producers/{name}.json"))?),
                201,
            )
            .await?;
    }
    for track in fixture("examples/fixture/tracks.json")?
        .as_array()
        .ok_or("fixture tracks absent")?
    {
        world
            .api(
                Method::POST,
                "/api/projects/{slug}/tracks",
                &format!("{base}/tracks"),
                None,
                Some(&token),
                Some(track.clone()),
                201,
            )
            .await?;
    }
    let runner_args = vec![
        "runner".into(),
        "--api-url".into(),
        world.base.clone(),
        "--project".into(),
        project.clone(),
        "--data-root".into(),
        fixture_root.join("data").display().to_string(),
        "--step-root".into(),
        fixture_root.join("steps").display().to_string(),
        "--work-root".into(),
        work.0.join("work").display().to_string(),
        "--unisolated-local".into(),
        "--once".into(),
    ];
    let lexical = submit(&mut world, &base, &agent, &token, "lexical").await?;
    success(
        &cli(
            runner_args.clone(),
            Some(("CANNERY_RUNNER_TOKEN_FILE", &work.0.join("tester.token"))),
        )
        .await?,
        "completed",
    );
    inspect_test_job(&mut world, &base, lexical, &token, "overlap-producer").await?;
    let tester_config = config(
        &work.0,
        &world.base,
        &project,
        &fixture_root,
        "test",
        "tester.token",
        None,
    );
    fs::write(work.0.join("tester.toml"), tester_config)?;
    let character = submit(&mut world, &base, &agent, &token, "character").await?;
    success(
        &cli(
            vec![
                "runner".into(),
                "--config".into(),
                work.0.join("tester.toml").display().to_string(),
                "--once".into(),
            ],
            None,
        )
        .await?,
        "completed",
    );
    inspect_test_job(&mut world, &base, character, &token, "trigram-producer").await?;
    success(
        &cli(
            runner_args,
            Some(("CANNERY_RUNNER_TOKEN_FILE", &work.0.join("tester.token"))),
        )
        .await?,
        "no job waiting",
    );
    let evaluator_args = vec![
        "evaluator".into(),
        "--api-url".into(),
        world.base.clone(),
        "--project".into(),
        project.clone(),
        "--config".into(),
        fixture_root.join("evaluator.json").display().to_string(),
        "--once".into(),
    ];
    for number in [lexical, character] {
        success(
            &cli(
                evaluator_args.clone(),
                Some((
                    "CANNERY_EVALUATOR_TOKEN_FILE",
                    &work.0.join("evaluator.token"),
                )),
            )
            .await?,
            "completed",
        );
        let listed = jobs(&mut world, &base, number, &token).await?;
        assert!(
            listed
                .iter()
                .any(|job| job["stage"] == "evaluator" && job["state"] == "completed")
        );
    }
    success(
        &cli(
            evaluator_args,
            Some((
                "CANNERY_EVALUATOR_TOKEN_FILE",
                &work.0.join("evaluator.token"),
            )),
        )
        .await?,
        "no job waiting",
    );
    // Relative token paths, config/flag replacement and token file permissions are CLI contracts.
    let mixed = cli(
        vec![
            "runner".into(),
            "--config".into(),
            work.0.join("tester.toml").display().to_string(),
            "--project".into(),
            project.clone(),
            "--once".into(),
        ],
        None,
    )
    .await?;
    assert_eq!(mixed.status.code(), Some(2));
    assert_ne!(mixed.stderr.len(), 0);
    if std::env::var("CANNERY_CONFORMANCE_SERVER_IMPLEMENTATION").as_deref() != Ok("rust") {
        assert!(String::from_utf8_lossy(&mixed.stderr).contains("--config replaces"));
    }
    fs::set_permissions(
        work.0.join("tester.token"),
        fs::Permissions::from_mode(0o644),
    )?;
    let insecure = cli(
        vec![
            "runner".into(),
            "--config".into(),
            work.0.join("tester.toml").display().to_string(),
            "--once".into(),
        ],
        None,
    )
    .await?;
    assert_eq!(insecure.status.code(), Some(2));
    fs::set_permissions(
        work.0.join("tester.token"),
        fs::Permissions::from_mode(0o600),
    )?;
    // The experiment kind claims and submits a workflow, followed by test and a policy-step eval.
    let mut science = fixture("examples/fixture/science.json")?;
    science["evaluator"] = fixture("examples/fixture/policy-step.json")?["evaluator"].clone();
    world
        .api(
            Method::POST,
            "/api/projects/{slug}/config/{kind}",
            &format!("{base}/config/science"),
            None,
            Some(&token),
            Some(science),
            201,
        )
        .await?;
    world
        .api(
            Method::POST,
            "/api/projects/{slug}/experiment-steps",
            &format!("{base}/experiment-steps"),
            None,
            Some(&token),
            Some(fixture(
                "examples/fixture/experiments/fixture-experiment.json",
            )?),
            201,
        )
        .await?;
    world
        .api(
            Method::POST,
            "/api/projects/{slug}/tracks",
            &format!("{base}/tracks"),
            None,
            Some(&token),
            Some(fixture("examples/fixture/workflow-track.json")?),
            201,
        )
        .await?;
    let workflow = draft(
        &mut world,
        &base,
        &agent,
        &token,
        fixture("examples/fixture/workflow-hypothesis.json")?,
    )
    .await?;
    fs::write(
        work.0.join("experiment.toml"),
        config(
            &work.0,
            &world.base,
            &project,
            &fixture_root,
            "experiment",
            "experimenter.token",
            None,
        ),
    )?;
    success(
        &cli(
            vec![
                "runner".into(),
                "--config".into(),
                work.0.join("experiment.toml").display().to_string(),
                "--once".into(),
            ],
            None,
        )
        .await?,
        "testing",
    );
    success(
        &cli(
            vec![
                "runner".into(),
                "--config".into(),
                work.0.join("tester.toml").display().to_string(),
                "--once".into(),
            ],
            None,
        )
        .await?,
        "completed",
    );
    inspect_test_job(&mut world, &base, workflow, &token, "overlap-producer").await?;
    fs::copy(
        fixture_root.join("policy-step.json"),
        work.0.join("policy.json"),
    )?;
    fs::write(
        work.0.join("eval.toml"),
        config(
            &work.0,
            &world.base,
            &project,
            &fixture_root,
            "eval",
            "evaluator.token",
            Some("policy.json"),
        ),
    )?;
    success(
        &cli(
            vec![
                "runner".into(),
                "--config".into(),
                work.0.join("eval.toml").display().to_string(),
                "--once".into(),
            ],
            None,
        )
        .await?,
        "completed",
    );
    let listed = jobs(&mut world, &base, workflow, &token).await?;
    assert!(
        listed
            .iter()
            .any(|job| job["stage"] == "evaluator" && job["state"] == "completed")
    );
    world.export()?;
    Ok(())
}
