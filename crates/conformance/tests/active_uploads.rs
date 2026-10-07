//! Real held HTTP bodies exercise admission and lease settlement.
#![allow(dead_code, clippy::too_many_arguments, clippy::too_many_lines)]
include!("support/active_uploads.rs");
const ROWS: &[u8] = b"{\"query\":\"q1\",\"language\":\"en\",\"reciprocal_rank\":1.0}\n";
const OTHER_ROWS: &[u8] = b"{\"query\":\"q1\",\"language\":\"en\",\"reciprocal_rank\":0.25}\n";
#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn validation_slot_wait_is_bounded_and_receiving_grants_retry() -> Result<()> {
    async {
        for key in [
            "CANNERY_CONFORMANCE_MAX_CONCURRENT_VALIDATIONS",
            "CANNERY_CONFORMANCE_MAX_STREAM_SECONDS",
        ] {
            assert_eq!(
                std::env::var(key)?,
                "1",
                "requires single-slot one-second profile"
            );
        }
        let (mut w, a) = World::new("upload-slot").await?;
        let p = prepare(&mut w, &a, "Slot admission").await?;
        submit(&mut w, &a, &p).await?;
        let job = w.claim_job(&a, false, false).await?;
        let first = job_grant(
            &mut w,
            &a,
            &job,
            "scorer/a.jsonl",
            ROWS,
            Some("per-query-results/v1"),
        )
        .await?;
        let second = job_grant(
            &mut w,
            &a,
            &job,
            "scorer/b.jsonl",
            OTHER_ROWS,
            Some("per-query-results/v1"),
        )
        .await?;
        let held = HeldUpload::start(&w, &first, ROWS).await?;
        receiving(&mut w, &first, ROWS, true).await?;
        let started = Instant::now();
        let response = put_request(&w, &second, OTHER_ROWS)?.send().await?;
        let native = match std::env::var("CANNERY_CONFORMANCE_SERVER_IMPLEMENTATION")?.as_str() {
            "rust" => true,
            "python" => false,
            _ => return Err("unknown server implementation".into()),
        };
        let first_response =
            w.h.check_response(
                Method::PUT,
                "/api/job-uploads/{upload_id}",
                response,
                if native { 201 } else { 503 },
            )
            .await?;
        if !native {
            assert_eq!(first_response.body["error"]["code"], "unavailable");
            assert!(started.elapsed() >= Duration::from_millis(500));
            assert!(started.elapsed() < Duration::from_secs(8));
        }
        let landed =
            w.h.check_response(
                Method::PUT,
                "/api/job-uploads/{upload_id}",
                held.finish().await?,
                201,
            )
            .await?
            .body;
        let retried = if native {
            // The slow first client owns no validation permit. The second
            // upload already verified; replay cannot create another artifact.
            w.h.check_response(
                Method::PUT,
                "/api/job-uploads/{upload_id}",
                put_request(&w, &second, OTHER_ROWS)?.send().await?,
                409,
            )
            .await?;
            first_response.body
        } else {
            w.h.check_response(
                Method::PUT,
                "/api/job-uploads/{upload_id}",
                put_request(&w, &second, OTHER_ROWS)?.send().await?,
                201,
            )
            .await?
            .body
        };
        for (artifact, bytes) in [(&landed, ROWS), (&retried, OTHER_ROWS)] {
            assert_eq!(artifact["content_validated"], true);
            assert_eq!(download(&mut w, &a.admin, artifact).await?, bytes);
        }
        assert_ne!(landed["id"], retried["id"]);
        let current = w
            .api(Call::get(JOB, w.job_path(&job)?, &a.admin))
            .await?
            .body;
        assert_eq!(current["state"], "claimed");
        let outputs = current["outputs"].as_array().ok_or("outputs absent")?;
        assert_eq!(outputs.len(), 2, "replay must not add another artifact");
        for artifact in [&landed, &retried] {
            assert!(outputs.iter().any(|output| output["id"] == artifact["id"]));
        }
        w.finish_coverage(&a.admin, "active-slot").await
    }
    .await
    .map_err(safe)
}
#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn receiving_attempt_upload_cannot_land_after_submission() -> Result<()> {
    async {
        let (mut w, a) = World::new("upload-submit").await?;
        let p = prepare(&mut w, &a, "Submission ends upload lease").await?;
        let bytes = b"late log\n";
        let body = json!({"role":"train_log","name":"late.txt","size_bytes":bytes.len(),"sha256":sha(bytes),"media_type":"text/plain"});
        let grant = w.api(Call::post(
            &format!("{ATTEMPT}/uploads"),
            format!("{}/uploads", w.attempt_path(&p.lease)),
            &a.agent, body, 201,
        ).lease(&p.lease)?).await?.body;
        let held = HeldUpload::start(&w, &grant, bytes).await?;
        receiving(&mut w, &grant, bytes, false).await?;
        submit(&mut w, &a, &p).await?;
        let rejected = w.h.check_response(Method::PUT, "/api/uploads/{upload_id}", held.finish().await?, 409).await?;
        assert_eq!(rejected.body["error"]["code"], "stale_lease");
        for retry in [bytes.as_slice(), b"wrong bytes".as_slice()] {
            let rejected = w.h.check_response(Method::PUT, "/api/uploads/{upload_id}", put_request(&w, &grant, retry)?.send().await?, 409).await?;
            assert_eq!(rejected.body["error"]["code"], "stale_lease");
        }
        let detail = w.api(Call::get(ATTEMPT, w.attempt_path(&p.lease), &a.admin)).await?.body;
        assert_eq!(detail["state"], "testing");
        assert_eq!(detail["failures"], json!([]));
        assert_eq!(detail["artifacts"].as_array().ok_or("artifacts absent")?.len(), 1);
        assert_eq!(download(&mut w, &a.admin, &p.candidate).await?, include_bytes!("../../../examples/fixture/candidate.json"));
        w.finish_coverage(&a.admin, "active-submit").await
    }.await.map_err(safe)
}
async fn assert_completed(w: &mut World, a: &Actors, job: &Lease, attempt: &Lease) -> Result<()> {
    let actual = w
        .api(Call::get(JOB, w.job_path(job)?, &a.admin))
        .await?
        .body;
    assert_eq!(actual["state"], "completed");
    let jobs = w
        .api(Call::get(
            &format!("{ATTEMPT}/jobs"),
            format!("{}/jobs", w.attempt_path(attempt)),
            &a.admin,
        ))
        .await?
        .body;
    let items = jobs["items"].as_array().ok_or("jobs absent")?;
    assert_eq!(items.len(), 2);
    assert!(
        items
            .iter()
            .any(|j| j["stage"] == "tester" && j["state"] == "completed")
    );
    assert!(
        items
            .iter()
            .any(|j| j["stage"] == "evaluator" && j["state"] == "pending")
    );
    Ok(())
}
#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn receiving_job_upload_cannot_land_after_completion() -> Result<()> {
    async {
        let (mut w, a) = World::new("upload-complete").await?;
        let p = prepare(&mut w, &a, "Completion ends upload lease").await?;
        submit(&mut w, &a, &p).await?;
        let job = w.claim_job(&a, false, false).await?;
        let payload = completion(&mut w, &a, &job).await?;
        let grant = job_grant(&mut w, &a, &job, "scorer/late.jsonl", ROWS, None).await?;
        let held = HeldUpload::start(&w, &grant, ROWS).await?;
        receiving(&mut w, &grant, ROWS, true).await?;
        let response = leased_request(
            &w,
            &a,
            &job,
            &format!("{}/completion", w.job_path(&job)?),
            &payload,
        )?
        .send()
        .await?;
        w.h.check_response(Method::POST, &format!("{JOB}/completion"), response, 200)
            .await?;
        let rejected =
            w.h.check_response(
                Method::PUT,
                "/api/job-uploads/{upload_id}",
                held.finish().await?,
                409,
            )
            .await?;
        assert_eq!(rejected.body["error"]["code"], "stale_lease");
        let actual = w
            .api(Call::get(JOB, w.job_path(&job)?, &a.admin))
            .await?
            .body;
        assert_eq!(actual["evidence"], payload["evidence"]);
        let outputs = actual["outputs"].as_array().ok_or("outputs absent")?;
        assert_eq!(outputs.len(), 2, "held late upload must publish no output");
        for output in outputs {
            let bytes = download(&mut w, &a.admin, output).await?;
            assert_eq!(sha(&bytes), string(&output["sha256"])?);
            if output["role"] == "evidence" {
                assert_eq!(
                    serde_json::from_slice::<Value>(&bytes)?,
                    payload["evidence"]
                );
            } else {
                assert_eq!(output["role"], "step_log");
                assert_eq!(bytes, b"scored\n");
            }
        }
        assert_completed(&mut w, &a, &job, &p.lease).await?;
        w.finish_coverage(&a.admin, "active-complete").await
    }
    .await
    .map_err(safe)
}
#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn concurrent_upload_grant_put_and_completion_finish_without_deadlock() -> Result<()> {
    async {
        let (mut w, a) = World::new("upload-race").await?;
        let p = prepare(&mut w, &a, "Concurrent completion").await?;
        submit(&mut w, &a, &p).await?;
        let job = w.claim_job(&a, false, false).await?;
        let payload = completion(&mut w, &a, &job).await?;
        let grant = job_grant(&mut w, &a, &job, "scorer/race.jsonl", ROWS, None).await?;
        let put = put_request(&w, &grant, ROWS)?;
        let body = json!({"role":"per_query_results","path":"scorer/second.jsonl","size_bytes":ROWS.len(),"sha256":sha(ROWS),"media_type":"application/jsonl"});
        let create = leased_request(&w, &a, &job, &format!("{}/uploads", w.job_path(&job)?), &body)?;
        let complete = leased_request(&w, &a, &job, &format!("{}/completion", w.job_path(&job)?), &payload)?;
        let (put, create, complete) = tokio::join!(put.send(), create.send(), complete.send());
        w.h.check_response(Method::POST, &format!("{JOB}/completion"), complete?, 200).await?;
        for (method, template, response) in [
            (Method::PUT, "/api/job-uploads/{upload_id}".to_owned(), put?),
            (Method::POST, format!("{JOB}/uploads"), create?),
        ] {
            let status = response.status().as_u16();
            assert!([201,409].contains(&status), "race must settle or reject ended lease");
            let checked = w.h.check_response(method, &template, response, status).await?;
            if status == 409 { assert_eq!(checked.body["error"]["code"], "stale_lease"); }
        }
        assert_completed(&mut w, &a, &job, &p.lease).await?;
        w.finish_coverage(&a.admin, "active-race").await
    }.await.map_err(safe)
}
