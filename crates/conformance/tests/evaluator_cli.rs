//! Real stock evaluator gate decisions and policy-step infrastructure failures.
#![allow(clippy::too_many_arguments, clippy::too_many_lines)]
include!("support/evaluator_cli.rs");
async fn cases(world: &mut World, base: &str, token: &str, kind: &str) -> Result<Vec<Value>> {
    let response = world
        .api(
            Method::GET,
            "/api/projects/{slug}/review-cases",
            &format!("{base}/review-cases?kind={kind}&state=pending"),
            None,
            Some(token),
            None,
            200,
        )
        .await?;
    Ok(response["items"]
        .as_array()
        .ok_or("review cases missing items")?
        .clone())
}
async fn evaluator_job(world: &mut World, base: &str, number: u64, token: &str) -> Result<Value> {
    let listed = jobs(world, base, number, token).await?;
    Ok(listed
        .iter()
        .find(|job| job["stage"] == "evaluator" && job["state"] == "completed")
        .ok_or("completed evaluator absent")?
        .clone())
}
fn worker_config_args(path: &std::path::Path) -> Vec<String> {
    vec![
        "runner".into(),
        "--config".into(),
        path.display().to_string(),
        "--once".into(),
    ]
}
fn stock_args(api: &str, project: &str, path: &std::path::Path) -> Vec<String> {
    vec![
        "evaluator".into(),
        "--api-url".into(),
        api.into(),
        "--project".into(),
        project.into(),
        "--config".into(),
        path.display().to_string(),
        "--once".into(),
    ]
}

// Gate detail is human-facing text, separate from arithmetic and canonical digests.
// Both implementations must still produce the exact declared explanation.
fn gate_details(implementation: &str) -> Result<[&'static str; 5]> {
    match implementation {
        "python" => Ok([
            "mrr on dev: value 1.0 - control 0.625 = 0.375 >= 0.375; control_source: resolved",
            "mrr on dev: value 1.0 - control 0.625 = 0.375, not > 0.375; control_source: resolved",
            "mrr on dev: no finite uncertainty.lower; control_source: resolved",
            "mrr on dev: control_mismatch: the tester's control_value 0.625 is not 0.6, the value registered for baseline base-camp revision fixture-r1; control_source: resolved",
            "mrr on dev: value 1.0 - control 0.625 = 0.375 >= 0.0; control_source: tester_reported",
        ]),
        "rust" => Ok([
            "mrr on dev: value 1 - control 0.625 = 0.375 >= 0.375; control_source: resolved",
            "mrr on dev: value 1 - control 0.625 = 0.375, not > 0.375; control_source: resolved",
            "mrr on dev: no finite uncertainty.lower; control_source: resolved",
            "mrr on dev: control_mismatch: the tester's control_value 0.625 is not 0.6, the value registered for baseline base-camp revision fixture-r1; control_source: resolved",
            "mrr on dev: value 1 - control 0.625 = 0.375 >= 0; control_source: tester_reported",
        ]),
        _ => Err("unknown evaluator implementation".into()),
    }
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn stock_gate_profiles_and_malformed_policy_step_retry() -> Result<()> {
    let expected_details =
        gate_details(&std::env::var("CANNERY_CONFORMANCE_SERVER_IMPLEMENTATION")?)?;
    let mut world = World::new()?;
    let project = unique()?.replace("conformance", "evaluator");
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
    let (token, _) = world
        .token(&admin, "evaluator-cli-admin", &["read", "write"])
        .await?;
    let base = format!("/api/projects/{project}");
    world
        .api(
            Method::POST,
            "/api/projects",
            "/api/projects",
            None,
            Some(&token),
            Some(json!({"slug":project,"title":"Evaluator CLI profiles"})),
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
    let agent = account(
        &mut world,
        &admin,
        &token,
        &base,
        "agent",
        "evaluator-agent",
    )
    .await?;
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
    private_file(&work.0.join("tester.token"), tester.as_bytes())?;
    private_file(&work.0.join("evaluator.token"), evaluator.as_bytes())?;
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
    world
        .api(
            Method::POST,
            "/api/projects/{slug}/producers",
            &format!("{base}/producers"),
            None,
            Some(&token),
            Some(fixture("examples/fixture/producers/overlap-producer.json")?),
            201,
        )
        .await?;
    world.api(Method::POST,"/api/projects/{slug}/tracks",&format!("{base}/tracks"),None,Some(&token),Some(json!({"slug":"lexical","title":"Evaluator lexical fixture","producer":{"name":"overlap-producer","revision":1}})),201).await?;
    fs::write(
        work.0.join("tester.toml"),
        config(
            &work.0,
            &world.base,
            &project,
            &fixture_root,
            "test",
            "tester.token",
            None,
        ),
    )?;
    let tester_args = worker_config_args(&work.0.join("tester.toml"));
    let mut wrong = fixture("examples/fixture/evaluator.json")?;
    wrong["evaluator"]["revision"] = json!("not-the-pinned-policy");
    fs::write(
        work.0.join("wrong-policy.json"),
        serde_json::to_vec(&wrong)?,
    )?;
    for (index, name, verdict, gate_result) in [
        (0, "gte-boundary", "pass", "pass"),
        (1, "strict-boundary", "fail", "fail"),
        (2, "missing-uncertainty", "inconclusive", "unknown"),
        (3, "mismatched-control", "inconclusive", "unknown"),
        (4, "tester-reported-control", "pass", "pass"),
    ] {
        let number = submit(&mut world, &base, &agent, &token, "lexical").await?;
        success(&cli(tester_args.clone(), None).await?, "completed");
        let mut policy = fixture("examples/fixture/evaluator.json")?;
        match index {
            0 => {
                policy["gates"][0]["min_delta"] = json!(0.375);
            }
            1 => {
                policy["gates"][0]["min_delta"] = json!(0.375);
                policy["gates"][0]["op"] = json!(">");
            }
            2 => {
                policy["gates"][0]["statistic"] = json!("uncertainty.lower");
            }
            3 => {
                policy["baselines"][0]["measurements"][0]["value"] = json!(0.6);
            }
            4 => {
                policy["baselines"][0]["measurements"] = json!([]);
            }
            _ => return Err("unknown evaluator profile".into()),
        }
        let policy_path = work.0.join(format!("{name}.json"));
        fs::write(&policy_path, serde_json::to_vec(&policy)?)?;
        if index == 0 {
            success(
                &cli(
                    stock_args(&world.base, &project, &work.0.join("wrong-policy.json")),
                    Some((
                        "CANNERY_EVALUATOR_TOKEN_FILE",
                        &work.0.join("evaluator.token"),
                    )),
                )
                .await?,
                "no job waiting",
            );
            let listed = jobs(&mut world, &base, number, &token).await?;
            assert!(
                listed
                    .iter()
                    .any(|job| job["stage"] == "evaluator" && job["state"] == "pending")
            );
        }
        success(
            &cli(
                stock_args(&world.base, &project, &policy_path),
                Some((
                    "CANNERY_EVALUATOR_TOKEN_FILE",
                    &work.0.join("evaluator.token"),
                )),
            )
            .await?,
            "completed",
        );
        let job = evaluator_job(&mut world, &base, number, &token).await?;
        let assessment = &job["evidence"]["assessment"];
        assert_eq!(assessment["verdict"], verdict);
        assert_eq!(assessment["policy_revision"], "fixture-policy-1");
        assert_eq!(assessment["gates"][0]["result"], gate_result);
        assert_eq!(assessment["gates"][1]["result"], "pass");
        let detail = string(&assessment["gates"][0], "detail")?;
        assert_eq!(detail, expected_details[index]);
        let compared = assessment["comparisons"]
            .as_array()
            .ok_or("assessment comparisons absent")?;
        assert_eq!(compared.len(), if index == 2 || index == 3 { 2 } else { 3 });
        for comparison in compared {
            assert_eq!(comparison["source"], "tester");
            assert_eq!(
                comparison["reference"]["kind"],
                if index == 4 { "other" } else { "baseline" }
            );
        }
        let queue = cases(&mut world, &base, &token, "result").await?;
        let review = queue
            .iter()
            .find(|case| case["hypothesis"] == number)
            .ok_or("evaluation did not open result case")?;
        assert_eq!(review["evaluation"]["assessment"], *assessment);
        assert_eq!(review["attempt_state"], "awaiting_human_review");
        // Scientific rejection/inconclusive decisions remain researcher choices after every verdict.
        let action = if verdict == "pass" {
            "promote"
        } else if verdict == "fail" {
            "reject"
        } else {
            "inconclusive"
        };
        let response=world.api(Method::POST,"/api/projects/{slug}/review-cases/{case_id}/decisions",&format!("{base}/review-cases/{}/decisions",string(review,"id")?),None,Some(&token),Some(json!({"review_case_id":review["id"],"evidence_revision":review["subject_revision"],"action":action,"reason":"Reviewed CLI evaluator profile"})),201).await?;
        assert_eq!(response["state"], "resolved");
    }
    // A malformed policy verdict follows infrastructure reruns and can be retried with a corrected policy.
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
    let number = submit(&mut world, &base, &agent, &token, "lexical").await?;
    success(&cli(tester_args, None).await?, "completed");
    let mut policy = fixture("examples/fixture/policy-step.json")?;
    policy["step"]["spec"]["container"]["env"] =
        json!([{"name":"FIXTURE_POLICY_MODE","value":"malformed"}]);
    fs::write(
        work.0.join("step-policy.json"),
        serde_json::to_vec(&policy)?,
    )?;
    fs::write(
        work.0.join("step-eval.toml"),
        config(
            &work.0,
            &world.base,
            &project,
            &fixture_root,
            "eval",
            "evaluator.token",
            Some("step-policy.json"),
        ),
    )?;
    let step_args = worker_config_args(&work.0.join("step-eval.toml"));
    for _ in 0..2 {
        success(&cli(step_args.clone(), None).await?, "failed");
    }
    let listed = jobs(&mut world, &base, number, &token).await?;
    let failed = listed
        .iter()
        .filter(|job| job["stage"] == "evaluator")
        .collect::<Vec<_>>();
    assert_eq!(failed.len(), 2);
    assert!(
        failed
            .iter()
            .all(|job| job["state"] == "failed" && job["error_code"] == "evaluator_error")
    );
    let pending = cases(&mut world, &base, &token, "failure").await?;
    let review = pending
        .iter()
        .find(|case| case["hypothesis"] == number)
        .ok_or("failed policy did not open failure case")?;
    let response=world.api(Method::POST,"/api/projects/{slug}/review-cases/{case_id}/decisions",&format!("{base}/review-cases/{}/decisions",string(review,"id")?),None,Some(&token),Some(json!({"review_case_id":review["id"],"evidence_revision":review["subject_revision"],"action":"retry","reason":"Policy verdict corrected"})),201).await?;
    assert_eq!(response["state"], "resolved");
    fs::write(
        work.0.join("step-policy.json"),
        serde_json::to_vec(&fixture("examples/fixture/policy-step.json")?)?,
    )?;
    success(&cli(step_args, None).await?, "completed");
    let listed = jobs(&mut world, &base, number, &token).await?;
    assert!(listed.iter().any(|job| job["stage"] == "evaluator"
        && job["state"] == "completed"
        && job["origin"] == "human_retry"));
    let _ = world.harness.fetch_audit(&token, 0).await?;
    world.export()?;
    Ok(())
}
