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
    world
        .h
        .call_tool(
            &actors.tester,
            "get_job_input",
            add(lease.job_args(&world.project), "input", json!("manifest")),
        )
        .await?;
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
    world
        .h
        .call_tool(
            &actors.evaluator,
            "get_job_input",
            add(lease.job_args(&world.project), "input", json!("evidence")),
        )
        .await?;
    let gate = match verdict {
        "pass" => "pass",
        "fail" => "fail",
        _ => "unknown",
    };
    let record = json!({"schema_version":"0.2","attempt_id":lease.document["attempt_id"],"stage":"evaluator","status":"completed","producer":{"kind":"service","id":"stock-evaluator"},"started_at":"2026-09-29T01:00:00Z","finished_at":"2026-09-29T01:00:05Z","provenance":{"source_revision":"4f2a9c1","dataset_revision":"qrels-r1","control_revision":"fixture-r1","science_revision":"1"},"assessment":{"policy_revision":"fixture-policy-1","gates":[{"id":"synthetic-policy","result":gate}],"evidence":lease.document["inputs"]["evidence"],"verdict":verdict,"reason":"Conformance synthetic policy"}});
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

async fn put_status(
    world: &mut World,
    grant: &Value,
    bytes: &[u8],
    job: bool,
    status: u16,
    capability: Option<&str>,
) -> Result<Value> {
    let url = reqwest::Url::parse(&string(&grant["upload_url"])?)?;
    let mut request = world
        .h
        .request(Method::PUT, url.path())?
        .body(bytes.to_vec());
    if let Some(token) = capability {
        request = request.header("X-Upload-Token", token);
    }
    let response = request.send().await?;
    Ok(world
        .h
        .check_response(
            Method::PUT,
            if job {
                "/api/job-uploads/{upload_id}"
            } else {
                "/api/uploads/{upload_id}"
            },
            response,
            status,
        )
        .await?
        .body)
}
async fn grant(
    world: &mut World,
    lease: &Lease,
    token: &str,
    bytes: &[u8],
    name: &str,
) -> Result<Value> {
    Ok(world.api(Call::post(&format!("{ATTEMPT}/uploads"),format!("{}/uploads",world.attempt_path(lease)),token,json!({"role":"candidate","name":name,"size_bytes":bytes.len(),"sha256":sha(bytes),"media_type":"application/json"}),201).lease(lease)?).await?.body)
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
#[allow(
    clippy::too_many_lines,
    reason = "An ordered download, comment and immutable decision correction scenario"
)]
async fn artifact_reads_comments_reports_and_retired_case_corrections() -> Result<()> {
    let (mut world, actors) = World::new("extensions").await?;
    let number = world
        .queue(&actors, "Download and correct a completed result")
        .await?;
    let submitted = submit(&mut world, &actors, number, false).await?;
    let path = world.attempt_path(&submitted.lease);
    let artifact_path = format!(
        "{}/artifacts/{}",
        world.base(),
        string(&submitted.artifact["id"])?
    );
    let artifact_template = "/api/projects/{slug}/artifacts/{artifact_id}";
    let downloaded = world
        .api(Call::get(
            artifact_template,
            artifact_path.clone(),
            &actors.admin,
        ))
        .await?;
    assert_eq!(
        downloaded.raw_body,
        include_bytes!("../../../examples/fixture/candidate.json")
    );
    assert_eq!(sha(&downloaded.raw_body), submitted.artifact["sha256"]);
    let mut cached = Call::get(artifact_template, artifact_path, &actors.admin);
    cached.status = 304;
    cached.headers.insert(
        "If-None-Match",
        downloaded
            .headers
            .get("etag")
            .ok_or("artifact etag absent")?
            .clone(),
    );
    assert_eq!(world.api(cached).await?.raw_body, Vec::<u8>::new());
    let report = world
        .api(Call::get(
            &format!("{ATTEMPT}/report"),
            format!("{path}/report"),
            &actors.admin,
        ))
        .await?;
    assert!(!report.body.is_null());
    let comment = world
        .api(Call::post(
            &format!("{ATTEMPT}/comments"),
            format!("{path}/comments"),
            &actors.admin,
            json!({"body_markdown":"Review note with a reproducible artifact."}),
            201,
        ))
        .await?
        .body;
    let comments = world
        .api(Call::get(
            &format!("{ATTEMPT}/comments"),
            format!("{path}/comments"),
            &actors.admin,
        ))
        .await?
        .body;
    assert!(
        comments["items"]
            .as_array()
            .ok_or("comment list absent")?
            .iter()
            .any(|item| item["id"] == comment["id"])
    );
    let evidence = test_job(&mut world, &actors, &submitted, false).await?;
    evaluate(&mut world, &actors, &evidence, "pass").await?;
    let case = pending_case(&mut world, &actors, number, "result").await?;
    let promoted = decide(&mut world, &actors, &case, "promote", false).await?;
    let first_id = promoted["decisions"][0]["id"].clone();
    let corrected_body = add(
        decision(&case, "inconclusive"),
        "supersedes",
        first_id.clone(),
    );
    let decision_path = format!(
        "{}/review-cases/{}/decisions",
        world.base(),
        string(&case["id"])?
    );
    let template = "/api/projects/{slug}/review-cases/{case_id}/decisions";
    let corrected = world
        .api(Call::post(
            template,
            decision_path.clone(),
            &actors.admin,
            corrected_body.clone(),
            201,
        ))
        .await?
        .body;
    assert!(
        corrected["decisions"]
            .as_array()
            .ok_or("correction history absent")?
            .iter()
            .any(|item| item["supersedes"] == first_id && item["action"] == "inconclusive")
    );
    let replay = world
        .api(Call::post(
            template,
            decision_path.clone(),
            &actors.admin,
            corrected_body,
            200,
        ))
        .await?
        .body;
    assert_eq!(replay["id"], corrected["id"]);
    assert_eq!(replay["decisions"], corrected["decisions"]);
    let stale = world
        .api(Call::post(
            template,
            decision_path,
            &actors.admin,
            add(decision(&case, "reject"), "supersedes", first_id.clone()),
            409,
        ))
        .await?
        .body;
    assert_eq!(stale["error"]["code"], "conflict");
    let history = world
        .api(Call::get(
            &format!("{ATTEMPT}/report"),
            format!("{path}/report"),
            &actors.admin,
        ))
        .await?
        .body;
    assert!(
        history["decisions"]
            .as_array()
            .ok_or("decision history absent")?
            .iter()
            .any(|item| item["id"] == first_id)
    );
    world
        .finish_coverage(&actors.admin, "extensions-results")
        .await
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
#[allow(
    clippy::too_many_lines,
    reason = "Upload capability and verification probes require distinct failed attempts"
)]
async fn upload_capabilities_verification_and_content_refusals() -> Result<()> {
    let (mut world, actors) = World::new("uploads").await?;
    let bytes = include_bytes!("../../../examples/fixture/candidate.json");
    let number = world.queue(&actors, "Single use capabilities").await?;
    let lease = world.claim(&actors, number, false).await?;
    let granted = grant(&mut world, &lease, &actors.agent, bytes, "single.json").await?;
    for capability in [None, Some("cr_upl_invalid")] {
        let hidden = put_status(&mut world, &granted, bytes, false, 404, capability).await?;
        assert_eq!(hidden["error"]["code"], "not_found");
    }
    let capability = string(&granted["headers"]["X-Upload-Token"])?;
    let artifact = put_status(&mut world, &granted, bytes, false, 201, Some(&capability)).await?;
    assert_eq!(artifact["sha256"], sha(bytes));
    let replay = put_status(&mut world, &granted, bytes, false, 409, Some(&capability)).await?;
    assert_eq!(replay["error"]["code"], "conflict");
    let slot=world.api(Call::post(&format!("{ATTEMPT}/uploads"),format!("{}/uploads",world.attempt_path(&lease)),&actors.agent,json!({"role":"candidate","name":"single.json","size_bytes":bytes.len(),"sha256":sha(bytes),"media_type":"application/json"}),409).lease(&lease)?).await?.body;
    assert_eq!(slot["error"]["code"], "conflict");
    world.api(Call::post(&format!("{ATTEMPT}/uploads"),format!("{}/uploads",world.attempt_path(&lease)),&actors.agent,json!({"role":"candidate","name":"../../outside","size_bytes":1,"sha256":sha(b"x"),"media_type":"application/json"}),422).lease(&lease)?).await?;
    for (label, sent) in [
        ("checksum", vec![b'x'; bytes.len()]),
        ("size", b"short".to_vec()),
    ] {
        let number = world.queue(&actors, &format!("Bad {label}")).await?;
        let bad = world.claim(&actors, number, false).await?;
        let granted = grant(&mut world, &bad, &actors.agent, bytes, "bad.json").await?;
        let token = string(&granted["headers"]["X-Upload-Token"])?;
        let rejected = put_status(&mut world, &granted, &sent, false, 422, Some(&token)).await?;
        assert_eq!(rejected["error"]["code"], "validation_failed");
        let detail = world
            .api(Call::get(ATTEMPT, world.attempt_path(&bad), &actors.admin))
            .await?
            .body;
        assert_eq!(detail["state"], "failed");
        assert_eq!(detail["artifacts"], json!([]));
        assert_eq!(detail["failures"][0]["code"], "upload_verification_failed");
        let stale = world
            .api(
                Call::post(
                    &format!("{ATTEMPT}/heartbeat"),
                    format!("{}/heartbeat", world.attempt_path(&bad)),
                    &actors.agent,
                    json!({}),
                    409,
                )
                .lease(&bad)?,
            )
            .await?
            .body;
        assert_eq!(stale["error"]["code"], "stale_lease");
    }
    let number = world
        .queue(&actors, "Refuse untrusted producer content")
        .await?;
    submit(&mut world, &actors, number, false).await?;
    let job = world.claim_job(&actors, false, false).await?;
    let bad_json = b"{\"queries\":12}";
    let body = json!({"role":"run","path":"overlap-producer/run/run.json","interface":"ranked-run/v1","size_bytes":bad_json.len(),"sha256":sha(bad_json),"media_type":"application/json"});
    let refused_grant = world
        .api(
            Call::post(
                &format!("{JOB}/uploads"),
                format!("{}/uploads", world.job_path(&job)?),
                &actors.tester,
                body,
                201,
            )
            .lease(&job)?,
        )
        .await?
        .body;
    let token = string(&refused_grant["headers"]["X-Upload-Token"])?;
    for capability in [None, Some("cr_upl_invalid")] {
        let hidden =
            put_status(&mut world, &refused_grant, bad_json, true, 404, capability).await?;
        assert_eq!(hidden["error"]["code"], "not_found");
    }
    let refused = put_status(
        &mut world,
        &refused_grant,
        bad_json,
        true,
        422,
        Some(&token),
    )
    .await?;
    assert_eq!(refused["error"]["code"], "invalid_content");
    let replay = put_status(
        &mut world,
        &refused_grant,
        bad_json,
        true,
        409,
        Some(&token),
    )
    .await?;
    assert_eq!(replay["error"]["code"], "conflict");
    let detail = world
        .api(Call::get(JOB, world.job_path(&job)?, &actors.admin))
        .await?
        .body;
    assert_eq!(detail["state"], "claimed");
    world
        .finish_coverage(&actors.admin, "extensions-uploads")
        .await
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
#[allow(
    clippy::too_many_lines,
    reason = "A sequential workflow verifies retry predecessor role isolation and immutable hashes"
)]
async fn experiment_registration_dashboard_mode_and_predecessor_isolation() -> Result<()> {
    let (mut world, actors) = World::new("workflow-extensions").await?;
    let runner = world.experimenter(&actors.admin).await?;
    let experiment: Value = serde_json::from_str(include_str!(
        "../../../examples/fixture/experiments/fixture-experiment.json"
    ))?;
    let registered = world
        .api(Call::post(
            "/api/projects/{slug}/experiment-steps",
            format!("{}/experiment-steps", world.base()),
            &actors.admin,
            experiment.clone(),
            201,
        ))
        .await?
        .body;
    assert_eq!(registered["revision"], 1);
    let listing = world
        .api(Call::get(
            "/api/projects/{slug}/experiment-steps",
            format!("{}/experiment-steps", world.base()),
            &actors.admin,
        ))
        .await?
        .body;
    assert!(
        listing["items"]
            .as_array()
            .ok_or("experiment list absent")?
            .iter()
            .any(|item| item["name"] == "fixture-experiment")
    );
    let read = world
        .api(Call::get(
            "/api/projects/{slug}/experiment-steps/{name}/{revision}",
            format!("{}/experiment-steps/fixture-experiment/1", world.base()),
            &actors.admin,
        ))
        .await?
        .body;
    assert_eq!(read["content"], experiment);
    let mut dashboard: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/contracts/dashboard_views/valid/tracks_vs_control.json"
    ))?;
    dashboard["views"][0]["metric"] = json!("mrr");
    world
        .api(Call::post(
            "/api/projects/{slug}/config/{kind}",
            format!("{}/config/dashboard", world.base()),
            &actors.admin,
            dashboard,
            201,
        ))
        .await?;
    let track_template = "/api/projects/{slug}/tracks/{track_slug}";
    let track_path = format!("{}/tracks/lexical", world.base());
    let workflow = json!({"steps":[{"name":"fixture-experiment","revision":1}]});
    let mut switch = Call::post(
        track_template,
        track_path.clone(),
        &actors.admin,
        json!({"expected_revision":1,"mode":"workflow","workflow":workflow,"reason":"Run registered experiments"}),
        200,
    );
    switch.method = Method::PATCH;
    let switched = world.api(switch).await?.body;
    assert_eq!(switched["mode"], "workflow");
    let number = world
        .queue(&actors, "Carry only pinned predecessor inputs")
        .await?;
    let claim = world
        .api(Call::post(
            "/api/projects/{slug}/claims",
            format!("{}/claims", world.base()),
            &runner,
            json!({"hypothesis":number}),
            201,
        ))
        .await?
        .body;
    let lease = Lease::attempt(&claim)?;
    let bytes = include_bytes!("../../../examples/fixture/candidate.json");
    let granted = grant(&mut world, &lease, &runner, bytes, "candidate.json").await?;
    let artifact = world.put(&granted, bytes, false).await?;
    let log_body = json!({"role":"step_log","name":"experiment.log","size_bytes":4,"sha256":sha(b"log\n"),"media_type":"text/plain"});
    let log_grant = world
        .api(
            Call::post(
                &format!("{ATTEMPT}/uploads"),
                format!("{}/uploads", world.attempt_path(&lease)),
                &runner,
                log_body,
                201,
            )
            .lease(&lease)?,
        )
        .await?
        .body;
    let log = world.put(&log_grant, b"log\n", false).await?;
    let failure = json!({"reason":"Synthetic registered experiment failure","code":"step_failed","step":"fixture-experiment","logs":[{"key":log["storage"]["key"],"size_bytes":log["size_bytes"],"sha256":log["sha256"]}]});
    world
        .api(
            Call::post(
                &format!("{ATTEMPT}/release"),
                format!("{}/release", world.attempt_path(&lease)),
                &runner,
                failure,
                200,
            )
            .lease(&lease)?,
        )
        .await?;
    let retry = world
        .api(Call::post(
            "/api/projects/{slug}/claims",
            format!("{}/claims", world.base()),
            &runner,
            json!({"hypothesis":number}),
            201,
        ))
        .await?
        .body;
    let next = Lease::attempt(&retry)?;
    assert_eq!(next.document["sequence"], 2);
    let inputs = &retry["workflow"]["inputs"]["predecessor"]["artifacts"];
    let items = inputs.as_array().ok_or("predecessor inputs absent")?;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["role"], "candidate");
    assert_eq!(items[0]["sha256"], sha(bytes));
    let template = format!("{ATTEMPT}/inputs/predecessor/{{artifact_id}}");
    let path = format!(
        "{}/inputs/predecessor/{}",
        world.attempt_path(&next),
        string(&artifact["id"])?
    );
    let input = world
        .api(Call::get(&template, path.clone(), &runner).lease(&next)?)
        .await?;
    assert_eq!(input.raw_body, bytes);
    assert_eq!(sha(&input.raw_body), items[0]["sha256"]);
    let mut forbidden = Call::get(&template, path, &actors.other_agent).lease(&next)?;
    forbidden.status = 403;
    world.api(forbidden).await?;
    let mut log_read = Call::get(
        &template,
        format!(
            "{}/inputs/predecessor/{}",
            world.attempt_path(&next),
            string(&log["id"])?
        ),
        &runner,
    )
    .lease(&next)?;
    log_read.status = 404;
    world.api(log_read).await?;
    let mut old = Call::get(
        &template,
        format!(
            "{}/inputs/predecessor/{}",
            world.attempt_path(&lease),
            string(&artifact["id"])?
        ),
        &runner,
    )
    .lease(&lease)?;
    old.status = 409;
    world.api(old).await?;
    world
        .finish_coverage(&actors.admin, "extensions-workflow")
        .await
}

#[tokio::test]
#[ignore = "requires one-minute upload grants, real time, and the conformance sweep trigger"]
async fn expired_upload_grants_are_reconciled_without_expiring_the_attempt() -> Result<()> {
    let (mut world, actors) = World::new("upload-expiry").await?;
    let number = world.queue(&actors, "Expire only the upload grant").await?;
    let lease = world.claim(&actors, number, false).await?;
    let bytes = include_bytes!("../../../examples/fixture/candidate.json");
    let granted = grant(&mut world, &lease, &actors.agent, bytes, "expired.json").await?;
    let token = string(&granted["headers"]["X-Upload-Token"])?;
    for _ in 0..31 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        world
            .api(
                Call::post(
                    &format!("{ATTEMPT}/heartbeat"),
                    format!("{}/heartbeat", world.attempt_path(&lease)),
                    &actors.agent,
                    json!({}),
                    200,
                )
                .lease(&lease)?,
            )
            .await?;
    }
    world.h.sweep(&actors.admin).await?;
    let expired = put_status(&mut world, &granted, bytes, false, 409, Some(&token)).await?;
    assert_eq!(expired["error"]["code"], "conflict");
    let detail = world
        .api(Call::get(
            ATTEMPT,
            world.attempt_path(&lease),
            &actors.admin,
        ))
        .await?
        .body;
    assert_eq!(detail["state"], "running");
    assert_eq!(detail["artifacts"], json!([]));
    let replacement = grant(&mut world, &lease, &actors.agent, bytes, "expired.json").await?;
    assert_ne!(replacement["upload_url"], granted["upload_url"]);
    let artifact = world.put(&replacement, bytes, false).await?;
    assert_eq!(artifact["sha256"], sha(bytes));
    world
        .finish_coverage(&actors.admin, "extensions-upload-expiry")
        .await
}
