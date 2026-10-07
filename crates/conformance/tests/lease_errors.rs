#![forbid(unsafe_code)]
#[path = "support/lifecycle_support.rs"]
#[allow(
    dead_code,
    reason = "Bootstrap helpers are shared by distinct integration binaries"
)]
mod support;
use conformance::Result;
use reqwest::{Method, header::HeaderValue};
use serde_json::{Value, json};
use support::{ATTEMPT, Actors, Call, JOB, Lease, World, object, sha, string};

const ABSENT_UUID: &str = "00000000-0000-0000-0000-000000000000";
struct Prepared {
    lease: Lease,
    manifest: Value,
    sheet: Value,
    artifact: Value,
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn mcp_submission_dispatches_real_evidence_and_marks_replay() -> Result<()> {
    let (mut world, actors) = World::new("mcp-submission").await?;
    let prepared = prepared(&mut world, &actors).await?;
    let mut args = prepared.lease.attempt_args(&world.project);
    args["document"] = prepared.sheet;
    args["idempotency_key"] = json!("mcp-submission-key");
    let initial = world
        .h
        .call_tool(&actors.agent, "submit_attempt", args.clone())
        .await?;
    assert_eq!(initial["state"], "testing");
    assert!(initial.get("replayed").is_none());
    let replay = world
        .h
        .call_tool(&actors.agent, "submit_attempt", args)
        .await?;
    assert_eq!(replay["id"], initial["id"]);
    assert_eq!(replay["state"], "testing");
    assert_eq!(replay["replayed"], true);
    world.finish_coverage(&actors.admin, "mcp-submission").await
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn submission_replays_after_freezing_and_rejects_changed_evidence() -> Result<()> {
    let (mut world, actors) = World::new("submission-replay").await?;
    let prepared = prepared(&mut world, &actors).await?;
    let path = format!("{}/submission", world.attempt_path(&prepared.lease));
    let template = format!("{ATTEMPT}/submission");
    let key = "submission-replay-key";
    let initial = world
        .api(
            Call::post(
                &template,
                path.clone(),
                &actors.agent,
                prepared.sheet.clone(),
                201,
            )
            .key(key)?
            .lease(&prepared.lease)?,
        )
        .await?
        .body;
    assert_eq!(initial["state"], "testing");
    let replay = world
        .api(
            Call::post(
                &template,
                path.clone(),
                &actors.agent,
                prepared.sheet.clone(),
                200,
            )
            .key(key)?,
        )
        .await?
        .body;
    assert_eq!(replay["id"], initial["id"]);
    assert_eq!(replay["state"], "testing");
    let mut changed = prepared.sheet.clone();
    changed["report"]["findings"] = json!("Changed evidence cannot reuse the key");
    let conflict = world
        .api(Call::post(&template, path.clone(), &actors.agent, changed, 409).key(key)?)
        .await?
        .body;
    assert_eq!(conflict["error"]["code"], "conflict");
    let stale = world
        .api(
            Call::post(&template, path, &actors.agent, prepared.sheet, 409)
                .lease(&prepared.lease)?,
        )
        .await?
        .body;
    assert_eq!(stale["error"]["code"], "stale_lease");
    world
        .finish_coverage(&actors.admin, "submission-replay")
        .await
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn invalid_leased_submission_commits_failure_and_expires_the_lease() -> Result<()> {
    let (mut world, actors) = World::new("submission-failure").await?;
    let prepared = prepared(&mut world, &actors).await?;
    let attempt_path = world.attempt_path(&prepared.lease);
    let path = format!("{attempt_path}/submission");
    let template = format!("{ATTEMPT}/submission");
    let mut invalid = prepared.sheet.clone();
    invalid["stage"] = json!("tester");
    let rejected = world
        .api(
            Call::post(&template, path.clone(), &actors.agent, invalid, 422)
                .lease(&prepared.lease)?,
        )
        .await?
        .body;
    assert_eq!(rejected["error"]["code"], "validation_failed");
    let failed = world
        .api(Call::get(ATTEMPT, attempt_path, &actors.admin))
        .await?
        .body;
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["failures"][0]["code"], "invalid_submission");
    let retry = world
        .api(
            Call::post(&template, path, &actors.agent, prepared.sheet, 409)
                .lease(&prepared.lease)?,
        )
        .await?
        .body;
    assert_eq!(retry["error"]["code"], "stale_lease");
    world
        .finish_coverage(&actors.admin, "submission-failure")
        .await
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn manifest_requires_verified_content_and_a_live_owner_lease() -> Result<()> {
    let (mut world, actors) = World::new("manifest-integrity").await?;
    let prepared = prepared(&mut world, &actors).await?;
    let path = format!("{}/manifest", world.attempt_path(&prepared.lease));
    let template = format!("{ATTEMPT}/manifest");
    let mut duplicate = prepared.manifest.clone();
    let first = duplicate["objects"][0].clone();
    duplicate["objects"]
        .as_array_mut()
        .ok_or("missing manifest objects")?
        .push(first);
    for document in [
        duplicate,
        {
            let mut mismatch = prepared.manifest.clone();
            mismatch["objects"][0]["sha256"] = json!(sha(b"unverified content"));
            mismatch
        },
        {
            let mut wrong_attempt = prepared.manifest.clone();
            wrong_attempt["attempt_id"] = json!(ABSENT_UUID);
            wrong_attempt
        },
    ] {
        let response = world
            .api(
                Call::post(&template, path.clone(), &actors.agent, document, 422)
                    .lease(&prepared.lease)?,
            )
            .await?;
        assert_eq!(response.body["error"]["code"], "validation_failed");
    }
    world
        .api(
            Call::post(
                &template,
                path.clone(),
                &actors.other_agent,
                prepared.manifest.clone(),
                403,
            )
            .lease(&prepared.lease)?,
        )
        .await?;
    world
        .api(Call::post(
            &template,
            path.clone(),
            &actors.agent,
            prepared.manifest.clone(),
            409,
        ))
        .await?;
    world
        .api(
            Call::post(
                &template,
                path,
                &actors.agent,
                prepared.manifest.clone(),
                201,
            )
            .lease(&prepared.lease)?,
        )
        .await?;
    world
        .finish_coverage(&actors.admin, "manifest-integrity")
        .await
}
async fn prepared(world: &mut World, actors: &Actors) -> Result<Prepared> {
    let number = world
        .queue(actors, "Probe lease errors without mutating the subject")
        .await?;
    let lease = world.claim(actors, number, false).await?;
    let bytes = include_bytes!("../../../examples/fixture/candidate.json");
    let grant=world.api(Call::post(&format!("{ATTEMPT}/uploads"),format!("{}/uploads",world.attempt_path(&lease)),&actors.agent,json!({"role":"candidate","name":"candidate.json","size_bytes":bytes.len(),"sha256":sha(bytes),"media_type":"application/json"}),201).lease(&lease)?).await?.body;
    let artifact = world.put(&grant, bytes, false).await?;
    let manifest = json!({"schema_version":"0.2","attempt_id":lease.document["id"],"objects":[object(&artifact)]});
    let reference = world
        .api(
            Call::post(
                &format!("{ATTEMPT}/manifest"),
                format!("{}/manifest", world.attempt_path(&lease)),
                &actors.agent,
                manifest.clone(),
                201,
            )
            .lease(&lease)?,
        )
        .await?
        .body;
    let mut sheet: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/contracts/evidence_envelope/valid/agent.json"
    ))?;
    sheet["attempt_id"] = lease.document["id"].clone();
    sheet["manifest"] = reference;
    sheet["provenance"]["science_revision"] = json!("1");
    sheet["artifact_roles"] = json!(["candidate"]);
    sheet["measurements"] = json!([{"metric":"mrr","value":0.9,"authority":"agent_claim","unit":"ratio","direction":"higher","split":"dev","dimensions":{"language":"en"}}]);
    Ok(Prepared {
        lease,
        manifest,
        sheet,
        artifact,
    })
}

struct Probe<'a> {
    template: String,
    path: String,
    absent_path: String,
    method: Method,
    body: Option<Value>,
    worker: &'a str,
    wrong_worker: &'a str,
}
// Every observation is an actual request and a frozen response-schema check.
// Bodies remain valid so authorization/lookup/lease failures do not masquerade
// as document validation errors. Only the 422 probe corrupts a typed header.
async fn lease_probes(world: &mut World, lease: &Lease, probe: Probe<'_>) -> Result<()> {
    for (status, code) in [
        (403, "forbidden"),
        (404, "not_found"),
        (409, "stale_lease"),
        (422, "validation_failed"),
    ] {
        let mut call = Call {
            method: probe.method.clone(),
            template: probe.template.clone(),
            path: if status == 404 {
                probe.absent_path.clone()
            } else {
                probe.path.clone()
            },
            token: if status == 403 {
                probe.wrong_worker
            } else {
                probe.worker
            },
            body: probe.body.clone(),
            status,
            headers: reqwest::header::HeaderMap::new(),
        }
        .lease(lease)?;
        if status == 409 {
            call.headers.insert(
                "X-Lease-Token",
                HeaderValue::from_static("cr_lease_invalid"),
            );
        }
        if status == 422 {
            call.headers.insert(
                "X-Lease-Generation",
                HeaderValue::from_static("not-an-integer"),
            );
        }
        let response = world.api(call).await?;
        assert_eq!(
            response.body["error"]["code"], code,
            "{} status{status}",
            probe.template
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
#[allow(
    clippy::too_many_lines,
    reason = "The fixture must remain leased while all four attempted mutations are rejected"
)]
async fn attempt_write_identity_lookup_lease_and_validation_errors() -> Result<()> {
    let (mut world, actors) = World::new("attempt-errors").await?;
    let prepared = prepared(&mut world, &actors).await?;
    let path = world.attempt_path(&prepared.lease);
    let missing = format!("{}/hypotheses/999999/attempts/1", world.base());
    let upload = json!({"role":"candidate","name":"probe.json","size_bytes":1,"sha256":sha(b"x"),"media_type":"application/json"});
    for (suffix, body) in [
        ("manifest", prepared.manifest.clone()),
        (
            "release",
            json!({"reason":"valid but unauthorized release"}),
        ),
        ("submission", prepared.sheet.clone()),
        ("uploads", upload.clone()),
    ] {
        lease_probes(
            &mut world,
            &prepared.lease,
            Probe {
                template: format!("{ATTEMPT}/{suffix}"),
                path: format!("{path}/{suffix}"),
                absent_path: format!("{missing}/{suffix}"),
                method: Method::POST,
                body: Some(body),
                worker: &actors.agent,
                wrong_worker: &actors.other_agent,
            },
        )
        .await?;
    }
    let mut invalid_manifest = prepared.manifest.clone();
    invalid_manifest["objects"][0]["sha256"] = json!(sha(b"different verified content"));
    let error = world
        .api(
            Call::post(
                &format!("{ATTEMPT}/manifest"),
                format!("{path}/manifest"),
                &actors.agent,
                invalid_manifest,
                422,
            )
            .lease(&prepared.lease)?,
        )
        .await?
        .body;
    assert_eq!(error["error"]["code"], "validation_failed");
    let mut invalid_upload = upload;
    invalid_upload["sha256"] = json!("not-a-sha256");
    world
        .api(
            Call::post(
                &format!("{ATTEMPT}/uploads"),
                format!("{path}/uploads"),
                &actors.agent,
                invalid_upload,
                422,
            )
            .lease(&prepared.lease)?,
        )
        .await?;
    world
        .api(
            Call::post(
                &format!("{ATTEMPT}/release"),
                format!("{path}/release"),
                &actors.agent,
                json!({"reason":""}),
                422,
            )
            .lease(&prepared.lease)?,
        )
        .await?;
    let intact = world
        .api(Call::get(ATTEMPT, path.clone(), &actors.admin))
        .await?
        .body;
    assert_eq!(intact["state"], "running");
    assert_eq!(
        intact["artifacts"]
            .as_array()
            .ok_or("artifacts absent")?
            .len(),
        1
    );
    assert_eq!(
        intact["artifacts"][0]["sha256"],
        prepared.artifact["sha256"]
    );
    let submitted = world
        .api(
            Call::post(
                &format!("{ATTEMPT}/submission"),
                format!("{path}/submission"),
                &actors.agent,
                prepared.sheet,
                201,
            )
            .lease(&prepared.lease)?,
        )
        .await?
        .body;
    assert_eq!(submitted["state"], "testing");
    world
        .finish_coverage(&actors.admin, "attempt-lease-errors")
        .await
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
#[allow(
    clippy::too_many_lines,
    reason = "One live claimed job backs the input and mutation error matrix"
)]
async fn job_inputs_writes_and_claims_enforce_identity_scope_and_typed_leases() -> Result<()> {
    let (mut world, actors) = World::new("job-errors").await?;
    let prepared = prepared(&mut world, &actors).await?;
    world
        .api(
            Call::post(
                &format!("{ATTEMPT}/submission"),
                format!("{}/submission", world.attempt_path(&prepared.lease)),
                &actors.agent,
                prepared.sheet,
                201,
            )
            .lease(&prepared.lease)?,
        )
        .await?;
    let lease = world.claim_job(&actors, false, false).await?;
    let path = world.job_path(&lease)?;
    let absent = format!("{}/jobs/{ABSENT_UUID}", world.base());
    let key = string(&prepared.artifact["storage"]["key"])?;
    for input in ["claimed-sheet", "evidence", "manifest", "object"] {
        let query = if input == "object" {
            let mut url = reqwest::Url::parse("http://127.0.0.1/")?;
            url.query_pairs_mut().append_pair("key", &key);
            format!("?{}", url.query().ok_or("key query absent")?)
        } else {
            String::new()
        };
        lease_probes(
            &mut world,
            &lease,
            Probe {
                template: format!("{JOB}/inputs/{input}"),
                path: format!("{path}/inputs/{input}{query}"),
                absent_path: format!("{absent}/inputs/{input}{query}"),
                method: Method::GET,
                body: None,
                worker: &actors.tester,
                wrong_worker: &actors.evaluator,
            },
        )
        .await?;
    }
    let upload = json!({"role":"step_log","path":"fixture-scorer/step_log/errors.log","size_bytes":1,"sha256":sha(b"x"),"media_type":"text/plain"});
    let failure = json!({"schema_version":"0.2","job_id":lease.document["job_id"],"error_code":"step_failed","reason":"Valid infrastructure failure body for lease probes","logs":[]});
    let mut completion: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/contracts/job_completion/valid/tester.json"
    ))?;
    completion["job_id"] = lease.document["job_id"].clone();
    for (suffix, body) in [
        ("completion", completion),
        ("failure", failure.clone()),
        ("heartbeat", json!({})),
        ("uploads", upload.clone()),
    ] {
        lease_probes(
            &mut world,
            &lease,
            Probe {
                template: format!("{JOB}/{suffix}"),
                path: format!("{path}/{suffix}"),
                absent_path: format!("{absent}/{suffix}"),
                method: Method::POST,
                body: Some(body),
                worker: &actors.tester,
                wrong_worker: &actors.evaluator,
            },
        )
        .await?;
    }
    let template = "/api/projects/{slug}/jobs/claims";
    let claims_path = format!("{}/jobs/claims", world.base());
    for (token, body, status) in [
        (&actors.agent, json!({}), 403),
        (&actors.tester, json!({}), 409),
        (&actors.tester, json!({"stage":"agent"}), 422),
        (
            &actors.tester,
            json!({"revision":"evaluator-policy-only"}),
            422,
        ),
        (&actors.evaluator, json!({}), 422),
        (
            &actors.tester,
            json!({"stage":"evaluator","revision":"fixture-policy-1"}),
            403,
        ),
    ] {
        world
            .api(Call::post(
                template,
                claims_path.clone(),
                token,
                body,
                status,
            ))
            .await?;
    }
    world
        .api(Call::post(
            template,
            "/api/projects/absent-lease-probe-project/jobs/claims".into(),
            &actors.tester,
            json!({}),
            404,
        ))
        .await?;
    let mut missing_evidence = Call::get(
        &format!("{JOB}/inputs/evidence"),
        format!("{path}/inputs/evidence"),
        &actors.tester,
    )
    .lease(&lease)?;
    missing_evidence.status = 404;
    world.api(missing_evidence).await?;
    let mut missing_object = Call::get(
        &format!("{JOB}/inputs/object"),
        format!("{path}/inputs/object?key=not-an-input"),
        &actors.tester,
    )
    .lease(&lease)?;
    missing_object.status = 404;
    world.api(missing_object).await?;
    let mut missing_query = Call::get(
        &format!("{JOB}/inputs/object"),
        format!("{path}/inputs/object"),
        &actors.tester,
    )
    .lease(&lease)?;
    missing_query.status = 422;
    world.api(missing_query).await?;
    let mut invalid_report = failure;
    invalid_report["step"] = json!("unregistered-step");
    world
        .api(
            Call::post(
                &format!("{JOB}/failure"),
                format!("{path}/failure"),
                &actors.tester,
                invalid_report,
                422,
            )
            .lease(&lease)?,
        )
        .await?;
    let mut unknown_interface = upload;
    unknown_interface["interface"] = json!("unknown/v1");
    world
        .api(
            Call::post(
                &format!("{JOB}/uploads"),
                format!("{path}/uploads"),
                &actors.tester,
                unknown_interface,
                422,
            )
            .lease(&lease)?,
        )
        .await?;
    let unchanged = world
        .api(Call::get(JOB, path.clone(), &actors.admin))
        .await?
        .body;
    assert_eq!(unchanged["state"], "claimed");
    assert_eq!(unchanged["outputs"], json!([]));
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
        .finish_coverage(&actors.admin, "job-lease-errors")
        .await
}
