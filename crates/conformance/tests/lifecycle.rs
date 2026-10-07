#![forbid(unsafe_code)]
#[path = "support/lifecycle_support.rs"]
mod support;
use conformance::Result;
use reqwest::Method;
use serde_json::{Value, json};
use support::{ATTEMPT, Actors, Call, JOB, Lease, World, add, object, sha, string};

struct Submitted {
    lease: Lease,
    sheet: Value,
    artifact: Value,
}

async fn tool_matches_read(
    world: &mut World,
    token: &str,
    tool: &str,
    arguments: Value,
    template: &str,
    path: String,
) -> Result<Value> {
    let expected = world.api(Call::get(template, path, token)).await?.body;
    let actual = world.h.call_tool(token, tool, arguments).await?;
    let expected = if expected.is_array() {
        json!({"items":expected})
    } else {
        expected
    };
    assert_eq!(
        actual, expected,
        "MCP {tool} must return the same populated read model"
    );
    Ok(actual)
}

#[allow(
    clippy::too_many_lines,
    reason = "One sequential submission scenario checks its lease, uploads and replay"
)]
async fn submit(world: &mut World, actors: &Actors, number: i64, mcp: bool) -> Result<Submitted> {
    let lease = world.claim(actors, number, mcp).await?;
    let path = world.attempt_path(&lease);
    let wrong = world
        .api(
            Call::post(
                &format!("{ATTEMPT}/heartbeat"),
                format!("{path}/heartbeat"),
                &actors.other_agent,
                json!({}),
                403,
            )
            .lease(&lease)?,
        )
        .await?;
    assert_eq!(wrong.body["error"]["code"], "forbidden");
    world
        .api(
            Call::post(
                &format!("{ATTEMPT}/heartbeat"),
                format!("{path}/heartbeat"),
                &actors.agent,
                json!({}),
                200,
            )
            .lease(&lease)?,
        )
        .await?;
    world
        .h
        .call_tool(
            &actors.agent,
            "heartbeat_attempt",
            lease.attempt_args(&world.project),
        )
        .await?;
    let bytes = include_bytes!("../../../examples/fixture/candidate.json");
    let grant_body = json!({"role":"candidate","name":"candidate.json","size_bytes":bytes.len(),"sha256":sha(bytes),"media_type":"application/json"});
    let grant = if mcp {
        let mut args = lease.attempt_args(&world.project);
        args.as_object_mut()
            .ok_or("lease args missing")?
            .extend(grant_body.as_object().ok_or("grant body missing")?.clone());
        world
            .h
            .call_tool(&actors.agent, "create_upload", args)
            .await?
    } else {
        world
            .api(
                Call::post(
                    &format!("{ATTEMPT}/uploads"),
                    format!("{path}/uploads"),
                    &actors.agent,
                    grant_body,
                    201,
                )
                .lease(&lease)?,
            )
            .await?
            .body
    };
    let artifact = world.put(&grant, bytes, false).await?;
    let manifest = json!({"schema_version":"0.2","attempt_id":lease.document["id"],"objects":[object(&artifact)]});
    let manifest_ref = if mcp {
        world
            .h
            .call_tool(
                &actors.agent,
                "post_manifest",
                add(lease.attempt_args(&world.project), "document", manifest),
            )
            .await?
    } else {
        world
            .api(
                Call::post(
                    &format!("{ATTEMPT}/manifest"),
                    format!("{path}/manifest"),
                    &actors.agent,
                    manifest,
                    201,
                )
                .lease(&lease)?,
            )
            .await?
            .body
    };
    let mut sheet: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/contracts/evidence_envelope/valid/agent.json"
    ))?;
    sheet["attempt_id"] = lease.document["id"].clone();
    sheet["manifest"] = manifest_ref;
    sheet["provenance"]["science_revision"] = json!("1");
    sheet["artifact_roles"] = json!(["candidate"]);
    sheet["measurements"] = json!([{"metric":"mrr","value":0.9,"authority":"agent_claim","unit":"ratio","direction":"higher","split":"dev","dimensions":{"language":"en"}}]);
    let submitted = if mcp {
        let args = add(
            add(
                lease.attempt_args(&world.project),
                "document",
                sheet.clone(),
            ),
            "idempotency_key",
            json!(format!("submit-{number}")),
        );
        world
            .h
            .call_tool(&actors.agent, "submit_attempt", args)
            .await?
    } else {
        world
            .api(
                Call::post(
                    &format!("{ATTEMPT}/submission"),
                    format!("{path}/submission"),
                    &actors.agent,
                    sheet.clone(),
                    201,
                )
                .lease(&lease)?
                .key(&format!("submit-{number}"))?,
            )
            .await?
            .body
    };
    assert_eq!(submitted["state"], "testing");
    let replay = world
        .api(
            Call::post(
                &format!("{ATTEMPT}/submission"),
                format!("{path}/submission"),
                &actors.agent,
                sheet.clone(),
                200,
            )
            .lease(&lease)?
            .key(&format!("submit-{number}"))?,
        )
        .await?;
    assert_eq!(replay.body["id"], lease.document["id"]);
    let changed = add(sheet.clone(), "observations", json!("changed submission"));
    let conflict = world
        .api(
            Call::post(
                &format!("{ATTEMPT}/submission"),
                format!("{path}/submission"),
                &actors.agent,
                changed,
                409,
            )
            .lease(&lease)?
            .key(&format!("submit-{number}"))?,
        )
        .await?;
    assert_eq!(conflict.body["error"]["code"], "conflict");
    Ok(Submitted {
        lease,
        sheet,
        artifact,
    })
}

fn tester_evidence(lease: &Lease) -> Value {
    json!({"schema_version":"0.2","attempt_id":lease.document["attempt_id"],"stage":"tester","status":"completed","producer":{"kind":"service","id":"cannery-runner"},"started_at":"2026-09-29T00:00:00Z","finished_at":"2026-09-29T00:01:00Z","provenance":{"source_revision":"4f2a9c1","tester_revision":"runner-fixture-1","dataset_revision":"qrels-r1","control_revision":"fixture-r1","science_revision":"1"},"observations":"Synthetic independently verified outputs","measurements":[{"metric":"mrr","value":1.0,"authority":"tester_verified","unit":"ratio","direction":"higher","split":"dev","dimensions":{"language":"en"},"sample_count":2},{"metric":"mrr","value":0.5,"authority":"tester_verified","unit":"ratio","direction":"higher","split":"dev","dimensions":{"language":"fr"},"sample_count":2}],"discrepancies":[],"artifact_roles":["evidence"]})
}
async fn job_outputs(
    world: &mut World,
    actors: &Actors,
    lease: &Lease,
    evidence: &Value,
    mcp: bool,
) -> Result<Vec<Value>> {
    let path = world.job_path(lease)?;
    let mut objects = Vec::new();
    for (role, name, bytes, media) in [
        (
            "evidence",
            "fixture-scorer/evidence/evidence.json",
            serde_json::to_vec(evidence)?,
            "application/json",
        ),
        (
            "step_log",
            "fixture-scorer/step_log/fixture-scorer.log",
            b"scored\n".to_vec(),
            "text/plain",
        ),
    ] {
        let body = json!({"role":role,"path":name,"size_bytes":bytes.len(),"sha256":sha(&bytes),"media_type":media});
        let grant = if mcp {
            let mut args = lease.job_args(&world.project);
            args.as_object_mut()
                .ok_or("job args missing")?
                .extend(body.as_object().ok_or("upload args missing")?.clone());
            world
                .h
                .call_tool(&actors.tester, "create_job_upload", args)
                .await?
        } else {
            world
                .api(
                    Call::post(
                        &format!("{JOB}/uploads"),
                        format!("{path}/uploads"),
                        &actors.tester,
                        body,
                        201,
                    )
                    .lease(lease)?,
                )
                .await?
                .body
        };
        objects.push(object(&world.put(&grant, &bytes, true).await?));
    }
    Ok(objects)
}

#[allow(
    clippy::too_many_lines,
    reason = "Preserve the ordered input, output, completion and replay assertions"
)]
async fn test_job(
    world: &mut World,
    actors: &Actors,
    submitted: &Submitted,
    mcp: bool,
) -> Result<Value> {
    let lease = world.claim_job(actors, false, mcp).await?;
    assert_eq!(lease.document["attempt_id"], submitted.lease.document["id"]);
    let path = world.job_path(&lease)?;
    world
        .api(
            Call::post(
                &format!("{JOB}/heartbeat"),
                format!("{path}/heartbeat"),
                &actors.tester,
                json!({}),
                200,
            )
            .lease(&lease)?,
        )
        .await?;
    world
        .h
        .call_tool(
            &actors.tester,
            "heartbeat_job",
            lease.job_args(&world.project),
        )
        .await?;
    let sheet = world
        .api(
            Call::get(
                &format!("{JOB}/inputs/claimed-sheet"),
                format!("{path}/inputs/claimed-sheet"),
                &actors.tester,
            )
            .lease(&lease)?,
        )
        .await?
        .body;
    assert_eq!(sheet, submitted.sheet);
    let manifest = world
        .api(
            Call::get(
                &format!("{JOB}/inputs/manifest"),
                format!("{path}/inputs/manifest"),
                &actors.tester,
            )
            .lease(&lease)?,
        )
        .await?
        .body;
    assert_eq!(
        manifest["objects"][0]["sha256"],
        submitted.artifact["sha256"]
    );
    let served_manifest = world
        .h
        .call_tool(
            &actors.tester,
            "get_job_input",
            add(lease.job_args(&world.project), "input", json!("manifest")),
        )
        .await?;
    assert_eq!(served_manifest, manifest);
    let evidence_missing = world
        .api({
            let mut call = Call::get(
                &format!("{JOB}/inputs/evidence"),
                format!("{path}/inputs/evidence"),
                &actors.tester,
            )
            .lease(&lease)?;
            call.status = 404;
            call
        })
        .await?;
    assert_eq!(evidence_missing.body["error"]["code"], "not_found");
    let mut object_url = reqwest::Url::parse(&format!("{}{path}/inputs/object", world.base_url))?;
    object_url.query_pairs_mut().append_pair(
        "key",
        submitted.artifact["storage"]["key"]
            .as_str()
            .ok_or("candidate key missing")?,
    );
    let response = world
        .h
        .request(Method::GET, object_url.path())?
        .query(&[(
            "key",
            submitted.artifact["storage"]["key"]
                .as_str()
                .ok_or("candidate key missing")?,
        )])
        .bearer_auth(&actors.tester)
        .header("X-Lease-Token", &lease.token)
        .header("X-Lease-Generation", lease.generation.to_string())
        .send()
        .await?;
    let bytes = world
        .h
        .check_response(Method::GET, &format!("{JOB}/inputs/object"), response, 200)
        .await?
        .raw_body;
    assert_eq!(sha(&bytes), submitted.artifact["sha256"]);
    let evidence = tester_evidence(&lease);
    let objects = job_outputs(world, actors, &lease, &evidence, mcp).await?;
    let completion = json!({"schema_version":"0.2","job_id":lease.document["job_id"],"evidence":evidence,"manifest":{"schema_version":"0.2","attempt_id":lease.document["attempt_id"],"objects":objects}});
    let completed = if mcp {
        world
            .h
            .call_tool(
                &actors.tester,
                "complete_job",
                add(
                    lease.job_args(&world.project),
                    "document",
                    completion.clone(),
                ),
            )
            .await?
    } else {
        world
            .api(
                Call::post(
                    &format!("{JOB}/completion"),
                    format!("{path}/completion"),
                    &actors.tester,
                    completion.clone(),
                    200,
                )
                .lease(&lease)?,
            )
            .await?
            .body
    };
    assert_eq!(completed["state"], "completed");
    let replay = world
        .api(
            Call::post(
                &format!("{JOB}/completion"),
                format!("{path}/completion"),
                &actors.tester,
                completion,
                200,
            )
            .lease(&lease)?,
        )
        .await?
        .body;
    assert_eq!(replay["id"], completed["id"]);
    let stale = world
        .api(
            Call::post(
                &format!("{JOB}/heartbeat"),
                format!("{path}/heartbeat"),
                &actors.tester,
                json!({}),
                409,
            )
            .lease(&lease)?,
        )
        .await?;
    assert_eq!(stale.body["error"]["code"], "stale_lease");
    Ok(evidence)
}

async fn evaluate(
    world: &mut World,
    actors: &Actors,
    evidence: &Value,
    verdict: &str,
) -> Result<Lease> {
    let lease = world.claim_job(actors, true, false).await?;
    let path = world.job_path(&lease)?;
    let tested = world
        .api(
            Call::get(
                &format!("{JOB}/inputs/evidence"),
                format!("{path}/inputs/evidence"),
                &actors.evaluator,
            )
            .lease(&lease)?,
        )
        .await?
        .body;
    assert_eq!(tested[0], *evidence);
    let served_evidence = world
        .h
        .call_tool(
            &actors.evaluator,
            "get_job_input",
            add(lease.job_args(&world.project), "input", json!("evidence")),
        )
        .await?;
    assert_eq!(served_evidence, json!({"items":tested}));
    let gate = match verdict {
        "pass" => "pass",
        "fail" => "fail",
        _ => "unknown",
    };
    let record = json!({"schema_version":"0.2","attempt_id":lease.document["attempt_id"],"stage":"evaluator","status":"completed","producer":{"kind":"service","id":"stock-evaluator"},"started_at":"2026-09-29T01:00:00Z","finished_at":"2026-09-29T01:00:05Z","provenance":{"source_revision":"4f2a9c1","dataset_revision":"qrels-r1","control_revision":"fixture-r1","science_revision":"1"},"assessment":{"policy_revision":"fixture-policy-1","gates":[{"id":"synthetic-policy","result":gate}],"evidence":lease.document["inputs"]["evidence"],"verdict":verdict,"reason":"Conformance synthetic policy"}});
    let mut record = record;
    record["assessment"]["comparisons"] = json!([{
        "metric":"mrr", "split":"dev", "dimensions":{"language":"en"},
        "value":1.0, "source":"tester",
        "reference":{"value":0.75,"label":"Base camp","kind":"baseline","ref":"base-camp"}
    }]);
    let completion =
        json!({"schema_version":"0.2","job_id":lease.document["job_id"],"evidence":record});
    let done = world
        .api(
            Call::post(
                &format!("{JOB}/completion"),
                format!("{path}/completion"),
                &actors.evaluator,
                completion,
                200,
            )
            .lease(&lease)?,
        )
        .await?;
    assert_eq!(done.body["state"], "completed");
    Ok(lease)
}

async fn pending_case(
    world: &mut World,
    actors: &Actors,
    number: i64,
    kind: &str,
) -> Result<Value> {
    let response = world
        .api(Call::get(
            "/api/projects/{slug}/review-cases",
            format!("{}/review-cases?kind={kind}&state=pending", world.base()),
            &actors.admin,
        ))
        .await?
        .body;
    response["items"]
        .as_array()
        .ok_or("cases missing")?
        .iter()
        .find(|case| case["hypothesis"] == number)
        .cloned()
        .ok_or_else(|| "expected pending review case absent".into())
}
fn decision(case: &Value, action: &str) -> Value {
    json!({"review_case_id":case["id"],"evidence_revision":case["subject_revision"],"action":action,"reason":"Conformance human decision"})
}
async fn decide(
    world: &mut World,
    actors: &Actors,
    case: &Value,
    action: &str,
    mcp: bool,
) -> Result<Value> {
    if mcp {
        world.h.call_tool(&actors.admin,"record_decision",json!({"project":world.project,"case_id":case["id"],"evidence_revision":case["subject_revision"],"action":action,"reason":"Conformance human decision"})).await
    } else {
        Ok(world
            .api(Call::post(
                "/api/projects/{slug}/review-cases/{case_id}/decisions",
                format!(
                    "{}/review-cases/{}/decisions",
                    world.base(),
                    string(&case["id"])?
                ),
                &actors.admin,
                decision(case, action),
                201,
            ))
            .await?
            .body)
    }
}

#[tokio::test]
#[ignore = "requires the URL-driven Python/Rust conformance environment"]
#[allow(
    clippy::too_many_lines,
    reason = "Keep each outcome and its immutable read checks in one scenario"
)]
async fn attempt_testing_evaluation_and_human_outcomes() -> Result<()> {
    let (mut world, actors) = World::new("results").await?;
    for (index, (verdict, action, state)) in [
        ("pass", "promote", "promoted"),
        ("fail", "reject", "rejected"),
        ("inconclusive", "inconclusive", "inconclusive"),
    ]
    .into_iter()
    .enumerate()
    {
        let number = world.queue(&actors, &format!("Result {index}")).await?;
        let submitted = submit(&mut world, &actors, number, index == 1).await?;
        let evidence = test_job(&mut world, &actors, &submitted, index == 1).await?;
        let evaluator = evaluate(&mut world, &actors, &evidence, verdict).await?;
        let case = pending_case(&mut world, &actors, number, "result").await?;
        assert_eq!(case["evaluation"]["assessment"]["verdict"], verdict);
        let path = format!(
            "{}/review-cases/{}/decisions",
            world.base(),
            string(&case["id"])?
        );
        let mut stale_decision = decision(&case, action);
        stale_decision["evidence_revision"] = json!(999);
        let stale = world
            .api(Call::post(
                "/api/projects/{slug}/review-cases/{case_id}/decisions",
                path.clone(),
                &actors.admin,
                stale_decision,
                409,
            ))
            .await?;
        assert_eq!(stale.body["error"]["code"], "stale_revision");
        let denied = world
            .api(Call::post(
                "/api/projects/{slug}/review-cases/{case_id}/decisions",
                path.clone(),
                &actors.agent,
                decision(&case, action),
                403,
            ))
            .await?;
        assert_eq!(denied.body["error"]["code"], "forbidden");
        if verdict == "fail" {
            let invalid = world
                .api(Call::post(
                    "/api/projects/{slug}/review-cases/{case_id}/decisions",
                    path.clone(),
                    &actors.admin,
                    decision(&case, "promote"),
                    422,
                ))
                .await?;
            assert_eq!(invalid.body["error"]["code"], "validation_failed");
            assert_eq!(invalid.body["error"]["details"][0]["path"], "/action");
        }
        let resolved = decide(&mut world, &actors, &case, action, index == 1).await?;
        assert_eq!(resolved["hypothesis_state"], state);
        let replay = world
            .api(Call::post(
                "/api/projects/{slug}/review-cases/{case_id}/decisions",
                path.clone(),
                &actors.admin,
                decision(&case, action),
                200,
            ))
            .await?;
        assert_eq!(
            replay.body["decisions"]
                .as_array()
                .ok_or("decisions missing")?
                .len(),
            1
        );
        let conflict = world
            .api(Call::post(
                "/api/projects/{slug}/review-cases/{case_id}/decisions",
                path,
                &actors.admin,
                decision(
                    &case,
                    if action == "reject" {
                        "inconclusive"
                    } else {
                        "reject"
                    },
                ),
                409,
            ))
            .await?;
        assert_eq!(conflict.body["error"]["code"], "conflict");
        let attempt_path = world.attempt_path(&submitted.lease);
        let stored = world
            .api(Call::get(ATTEMPT, attempt_path.clone(), &actors.admin))
            .await?
            .body;
        assert_eq!(stored["state"], state);
        assert_eq!(stored["claimed_sheet"], submitted.sheet);
        world
            .api(Call::get(
                "/api/projects/{slug}/hypotheses/{number}/attempts",
                format!("{}/hypotheses/{number}/attempts", world.base()),
                &actors.admin,
            ))
            .await?;
        world
            .api(Call::get(
                "/api/projects/{slug}/attempts",
                format!("{}/attempts", world.base()),
                &actors.admin,
            ))
            .await?;
        world
            .api(Call::get(
                &format!("{ATTEMPT}/jobs"),
                format!("{attempt_path}/jobs"),
                &actors.admin,
            ))
            .await?;
        world
            .api(Call::get(JOB, world.job_path(&evaluator)?, &actors.admin))
            .await?;
        let base = world.base();
        let project = world.project.clone();
        let attempt_args = json!({"project":project,"number":number,"sequence":submitted.lease.document["sequence"]});
        let attempt = tool_matches_read(
            &mut world,
            &actors.admin,
            "get_attempt",
            attempt_args.clone(),
            ATTEMPT,
            attempt_path.clone(),
        )
        .await?;
        assert_eq!(attempt["claimed_sheet"], submitted.sheet);
        let attempts = tool_matches_read(
            &mut world,
            &actors.admin,
            "list_attempts",
            json!({"project":project}),
            "/api/projects/{slug}/attempts",
            format!("{base}/attempts"),
        )
        .await?;
        assert!(attempts["items"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| item["id"] == submitted.lease.document["id"])
        }));
        let job_path = world.job_path(&evaluator)?;
        let job = tool_matches_read(
            &mut world,
            &actors.admin,
            "get_job",
            json!({"project":project,"job_id":evaluator.document["job_id"]}),
            JOB,
            job_path,
        )
        .await?;
        assert_eq!(job["state"], "completed");
        let jobs = tool_matches_read(
            &mut world,
            &actors.admin,
            "list_attempt_jobs",
            attempt_args.clone(),
            &format!("{ATTEMPT}/jobs"),
            format!("{attempt_path}/jobs"),
        )
        .await?;
        assert_eq!(jobs["items"].as_array().ok_or("jobs missing")?.len(), 2);
        let review = tool_matches_read(
            &mut world,
            &actors.admin,
            "get_review_case",
            json!({"project":project,"case_id":case["id"]}),
            "/api/projects/{slug}/review-cases/{case_id}",
            format!("{base}/review-cases/{}", string(&case["id"])?),
        )
        .await?;
        assert_eq!(review["evaluation"]["assessment"]["verdict"], verdict);
        tool_matches_read(
            &mut world,
            &actors.admin,
            "list_review_cases",
            json!({"project":project}),
            "/api/projects/{slug}/review-cases",
            format!("{base}/review-cases"),
        )
        .await?;
        tool_matches_read(
            &mut world,
            &actors.admin,
            "get_report",
            attempt_args,
            &format!("{ATTEMPT}/report"),
            format!("{attempt_path}/report"),
        )
        .await?;
        let reports = tool_matches_read(
            &mut world,
            &actors.admin,
            "list_reports",
            json!({"project":project}),
            "/api/projects/{slug}/reports",
            format!("{base}/reports"),
        )
        .await?;
        assert!(
            reports["items"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        );
        let metrics = tool_matches_read(
            &mut world,
            &actors.admin,
            "query_metrics",
            json!({"project":project,"metric":"mrr","all_slices":true}),
            "/api/projects/{slug}/metrics/query",
            format!("{base}/metrics/query?metric=mrr&all_slices=true"),
        )
        .await?;
        assert!(
            metrics["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["hypothesis"] == number
                    && item["value"] == 1.0
                    && item["authority"] == "tester_verified"))
        );
        let comparisons = tool_matches_read(
            &mut world,
            &actors.admin,
            "query_comparisons",
            json!({"project":project,"metric":"mrr"}),
            "/api/projects/{slug}/comparisons",
            format!("{base}/comparisons?metric=mrr"),
        )
        .await?;
        assert!(
            comparisons["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["hypothesis"] == number
                    && item["reference"]["value"] == 0.75
                    && item["verdict"] == verdict))
        );
        world
            .h
            .call_tool(
                &actors.admin,
                "get_artifact",
                json!({"project":world.project,"artifact_id":submitted.artifact["id"]}),
            )
            .await?;
    }
    world.finish_coverage(&actors.admin, "results").await
}

async fn fail_job(
    world: &mut World,
    actors: &Actors,
    lease: &Lease,
    evaluation: bool,
    mcp: bool,
) -> Result<Value> {
    let token = if evaluation {
        &actors.evaluator
    } else {
        &actors.tester
    };
    let report = json!({"schema_version":"0.2","job_id":lease.document["job_id"],"error_code":"step_failed","reason":"Synthetic infrastructure failure","logs":[]});
    if mcp {
        world
            .h
            .call_tool(
                token,
                "fail_job",
                add(lease.job_args(&world.project), "document", report),
            )
            .await
    } else {
        Ok(world
            .api(
                Call::post(
                    &format!("{JOB}/failure"),
                    format!("{}/failure", world.job_path(lease)?),
                    token,
                    report,
                    200,
                )
                .lease(lease)?,
            )
            .await?
            .body)
    }
}

async fn release(world: &mut World, actors: &Actors, lease: &Lease, mcp: bool) -> Result<Value> {
    if mcp {
        world
            .h
            .call_tool(
                &actors.agent,
                "release_attempt",
                add(
                    lease.attempt_args(&world.project),
                    "reason",
                    json!("Conformance voluntary release"),
                ),
            )
            .await
    } else {
        Ok(world
            .api(
                Call::post(
                    &format!("{ATTEMPT}/release"),
                    format!("{}/release", world.attempt_path(lease)),
                    &actors.agent,
                    json!({"reason":"Conformance voluntary release"}),
                    200,
                )
                .lease(lease)?,
            )
            .await?
            .body)
    }
}

#[tokio::test]
#[ignore = "requires the URL-driven Python/Rust conformance environment"]
#[allow(
    clippy::too_many_lines,
    reason = "Failure stages and their ordered retry/history assertions form one scenario"
)]
async fn failures_retries_lease_reissue_and_immutable_history() -> Result<()> {
    let (mut world, actors) = World::new("failures").await?;
    let claims_path = format!("{}/claims", world.base());
    let unavailable = world
        .api(Call::post(
            "/api/projects/{slug}/claims",
            claims_path.clone(),
            &actors.agent,
            json!({}),
            409,
        ))
        .await?;
    assert_eq!(unavailable.body["error"]["code"], "nothing_to_claim");
    let denied = world
        .api(Call::post(
            "/api/projects/{slug}/claims",
            claims_path.clone(),
            &actors.tester,
            json!({}),
            403,
        ))
        .await?;
    assert_eq!(denied.body["error"]["code"], "forbidden");
    let invalid = world
        .api(Call::post(
            "/api/projects/{slug}/claims",
            claims_path,
            &actors.agent,
            json!({"hypothesis":0}),
            422,
        ))
        .await?;
    assert_eq!(invalid.body["error"]["code"], "validation_failed");
    let other_project = format!("{}-other", world.project);
    world
        .api(Call::post(
            "/api/projects",
            "/api/projects".into(),
            &actors.admin,
            json!({"slug":other_project,"title":"Isolated project"}),
            201,
        ))
        .await?;
    let isolated = world
        .api(Call::post(
            "/api/projects/{slug}/claims",
            format!("/api/projects/{other_project}/claims"),
            &actors.agent,
            json!({}),
            404,
        ))
        .await?;
    assert_eq!(isolated.body["error"]["code"], "not_found");
    let number = world.queue(&actors, "Agent release and retry").await?;
    let first = world.claim(&actors, number, false).await?;
    let invalid = world
        .api(
            Call::post(
                &format!("{ATTEMPT}/release"),
                format!("{}/release", world.attempt_path(&first)),
                &actors.agent,
                json!({"reason":"Synthetic release","code":"step_failed"}),
                403,
            )
            .lease(&first)?,
        )
        .await?;
    assert_eq!(invalid.body["error"]["code"], "forbidden");
    assert_eq!(
        release(&mut world, &actors, &first, false).await?["state"],
        "failed"
    );
    let case = pending_case(&mut world, &actors, number, "failure").await?;
    assert_eq!(case["failure"]["stage"], "agent");
    let retired = decide(&mut world, &actors, &case, "retry", false).await?;
    assert_eq!(retired["hypothesis_state"], "queued");
    let second = world.claim(&actors, number, true).await?;
    assert_eq!(
        second.document["sequence"]
            .as_u64()
            .ok_or("sequence missing")?,
        first.document["sequence"]
            .as_u64()
            .ok_or("sequence missing")?
            + 1
    );
    assert_eq!(
        release(&mut world, &actors, &second, true).await?["state"],
        "failed"
    );
    let case = pending_case(&mut world, &actors, number, "failure").await?;
    assert_eq!(
        decide(&mut world, &actors, &case, "close_failed", true).await?["hypothesis_state"],
        "failed"
    );
    let history = world
        .api(Call::get(
            ATTEMPT,
            world.attempt_path(&first),
            &actors.admin,
        ))
        .await?
        .body;
    assert_eq!(history["state"], "failed");
    assert_eq!(history["failures"][0]["code"], "released");

    let number = world.queue(&actors, "Tester failures").await?;
    let submitted = submit(&mut world, &actors, number, false).await?;
    let claim_path = format!("{}/jobs/claims", world.base());
    let old = world
        .api(
            Call::post(
                "/api/projects/{slug}/jobs/claims",
                claim_path.clone(),
                &actors.tester,
                json!({}),
                201,
            )
            .key("tester-reissue")?,
        )
        .await?
        .body;
    let old = Lease::job(&old)?;
    let new = world
        .api(
            Call::post(
                "/api/projects/{slug}/jobs/claims",
                claim_path,
                &actors.tester,
                json!({}),
                200,
            )
            .key("tester-reissue")?,
        )
        .await?
        .body;
    let new = Lease::job(&new)?;
    assert_eq!(old.document["job_id"], new.document["job_id"]);
    assert_eq!(new.generation, old.generation + 1);
    assert!(
        new.token != old.token,
        "lease reissue did not rotate the token"
    );
    let expired = world
        .api(
            Call::post(
                &format!("{JOB}/heartbeat"),
                format!("{}/heartbeat", world.job_path(&old)?),
                &actors.tester,
                json!({}),
                409,
            )
            .lease(&old)?,
        )
        .await?;
    assert_eq!(expired.body["error"]["code"], "stale_lease");
    for (header, value) in [
        ("X-Lease-Token", old.token.clone()),
        ("X-Lease-Generation", old.generation.to_string()),
    ] {
        let mut call = Call::post(
            &format!("{JOB}/heartbeat"),
            format!("{}/heartbeat", world.job_path(&new)?),
            &actors.tester,
            json!({}),
            409,
        )
        .lease(&new)?;
        call.headers
            .insert(header, reqwest::header::HeaderValue::from_str(&value)?);
        let stale = world.api(call).await?;
        assert_eq!(stale.body["error"]["code"], "stale_lease");
    }
    world
        .api(
            Call::post(
                &format!("{JOB}/heartbeat"),
                format!("{}/heartbeat", world.job_path(&new)?),
                &actors.tester,
                json!({}),
                200,
            )
            .lease(&new)?,
        )
        .await?;
    assert_eq!(
        fail_job(&mut world, &actors, &new, false, false).await?["state"],
        "failed"
    );
    let rerun = world.claim_job(&actors, false, true).await?;
    assert_ne!(rerun.document["job_id"], new.document["job_id"]);
    let run = world
        .api(Call::get(JOB, world.job_path(&rerun)?, &actors.admin))
        .await?
        .body;
    assert_eq!(run["origin"], "auto_retry");
    assert_eq!(run["previous_run_id"], new.document["job_id"]);
    assert_eq!(
        fail_job(&mut world, &actors, &rerun, false, true).await?["state"],
        "failed"
    );
    let case = pending_case(&mut world, &actors, number, "failure").await?;
    assert_eq!(case["failure"]["stage"], "tester");
    assert_eq!(
        decide(&mut world, &actors, &case, "retry", false).await?["hypothesis_state"],
        "active"
    );
    let retry = world.claim_job(&actors, false, false).await?;
    let retry_before = world
        .api(Call::get(JOB, world.job_path(&retry)?, &actors.admin))
        .await?
        .body;
    assert_eq!(retry_before["origin"], "human_retry");
    let invalid = world
        .api(
            Call::post(
                &format!("{JOB}/completion"),
                format!("{}/completion", world.job_path(&retry)?),
                &actors.tester,
                json!({"schema_version":"0.2","job_id":retry.document["job_id"],"evidence":{}}),
                422,
            )
            .lease(&retry)?,
        )
        .await?;
    assert_eq!(invalid.body["error"]["code"], "validation_failed");
    let fresh_budget = world.claim_job(&actors, false, false).await?;
    let run = world
        .api(Call::get(
            JOB,
            world.job_path(&fresh_budget)?,
            &actors.admin,
        ))
        .await?
        .body;
    assert_eq!(run["origin"], "auto_retry");
    assert_eq!(run["previous_run_id"], retry.document["job_id"]);
    let invalid = world.api(Call::post(
        &format!("{JOB}/completion"),
        format!("{}/completion", world.job_path(&fresh_budget)?),
        &actors.tester,
        json!({"schema_version":"0.2","job_id":fresh_budget.document["job_id"],"evidence":{}}),
        422,
    ).lease(&fresh_budget)?).await?;
    assert_eq!(invalid.body["error"]["code"], "validation_failed");
    let case = pending_case(&mut world, &actors, number, "failure").await?;
    assert_eq!(case["failure"]["code"], "invalid_output");
    assert_eq!(
        decide(&mut world, &actors, &case, "close_failed", false).await?["hypothesis_state"],
        "failed"
    );
    let immutable = world
        .api(Call::get(
            ATTEMPT,
            world.attempt_path(&submitted.lease),
            &actors.admin,
        ))
        .await?
        .body;
    assert_eq!(immutable["claimed_sheet"], submitted.sheet);
    assert_eq!(
        immutable["failures"]
            .as_array()
            .ok_or("failure history absent")?
            .len(),
        2
    );

    let number = world
        .queue(&actors, "Evaluator infrastructure failures")
        .await?;
    let submitted = submit(&mut world, &actors, number, false).await?;
    test_job(&mut world, &actors, &submitted, false).await?;
    for mcp in [false, true] {
        let lease = world.claim_job(&actors, true, false).await?;
        assert_eq!(
            fail_job(&mut world, &actors, &lease, true, mcp).await?["state"],
            "failed"
        );
    }
    let case = pending_case(&mut world, &actors, number, "failure").await?;
    assert_eq!(case["failure"]["stage"], "evaluator");
    assert!(case["evaluation"].is_null());
    assert_eq!(
        decide(&mut world, &actors, &case, "close_failed", false).await?["hypothesis_state"],
        "failed"
    );
    world.finish_coverage(&actors.admin, "failures").await
}

#[tokio::test]
#[ignore = "requires five-second leases and the real-time conformance sweep trigger"]
async fn expired_leases_and_short_step_deadlines_are_reconciled() -> Result<()> {
    let (mut world, actors) = World::new("expiry").await?;
    let number = world.queue(&actors, "Expired agent lease").await?;
    let lease = world.claim(&actors, number, false).await?;
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    world.h.sweep(&actors.admin).await?;
    let expired = world
        .api(
            Call::post(
                &format!("{ATTEMPT}/heartbeat"),
                format!("{}/heartbeat", world.attempt_path(&lease)),
                &actors.agent,
                json!({}),
                409,
            )
            .lease(&lease)?,
        )
        .await?;
    assert_eq!(expired.body["error"]["code"], "stale_lease");
    let case = pending_case(&mut world, &actors, number, "failure").await?;
    assert_eq!(case["failure"]["code"], "lease_expired");
    decide(&mut world, &actors, &case, "close_failed", false).await?;
    let number = world.queue(&actors, "Expired tester lease").await?;
    let submitted = submit(&mut world, &actors, number, false).await?;
    for mcp in [false, true] {
        let job = world.claim_job(&actors, false, mcp).await?;
        tokio::time::sleep(std::time::Duration::from_secs(6)).await;
        world.h.sweep(&actors.admin).await?;
        let read = world
            .api(Call::get(JOB, world.job_path(&job)?, &actors.admin))
            .await?
            .body;
        assert_eq!(read["state"], "failed");
        assert_eq!(read["error_code"], "lease_expired");
    }
    let case = pending_case(&mut world, &actors, number, "failure").await?;
    assert_eq!(case["failure"]["stage"], "tester");
    assert_eq!(case["failure"]["code"], "lease_expired");
    let attempt = world
        .api(Call::get(
            ATTEMPT,
            world.attempt_path(&submitted.lease),
            &actors.admin,
        ))
        .await?
        .body;
    assert_eq!(attempt["state"], "failed");
    world.finish_coverage(&actors.admin, "expiry").await?;
    let (mut deadlines, actors) = World::with_deadline("deadlines", Some(1)).await?;
    let number = deadlines
        .queue(&actors, "Short registered step deadline")
        .await?;
    submit(&mut deadlines, &actors, number, false).await?;
    for _ in 0..2 {
        let job = deadlines.claim_job(&actors, false, false).await?;
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        deadlines.h.sweep(&actors.admin).await?;
        let read = deadlines
            .api(Call::get(JOB, deadlines.job_path(&job)?, &actors.admin))
            .await?
            .body;
        assert_eq!(read["state"], "failed");
        assert_eq!(read["error_code"], "deadline_exceeded");
    }
    let case = pending_case(&mut deadlines, &actors, number, "failure").await?;
    assert_eq!(case["failure"]["code"], "deadline_exceeded");
    deadlines.finish_coverage(&actors.admin, "deadlines").await
}
