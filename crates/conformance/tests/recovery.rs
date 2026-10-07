//! Isolated launcher-owned fixtures exercise otherwise unreachable recovery branches.
#![forbid(unsafe_code)]
#[path = "support/lifecycle_support.rs"]
#[allow(dead_code, reason = "reuse the public lifecycle bootstrap")]
mod lifecycle;
#[path = "support/recovery_support.rs"]
mod support;
use conformance::Result;
use lifecycle::{ATTEMPT, Call, JOB, Lease, World, sha, string};
use reqwest::Method;
use serde_json::{Value, json};
use std::io::Write;
use support::{CLAIM_KEY, KEPT, Login};

#[tokio::test]
#[ignore = "launcher prepares HTTP fixtures before its isolated SQL recovery provisioning"]
async fn prepare_recovery_fixture() -> Result<()> {
    let (mut upload, actors) = World::new("recovery-upload").await?;
    let attempt = support::submit(&mut upload, &actors).await?;
    let claimed = upload
        .api(
            Call::post(
                "/api/projects/{slug}/jobs/claims",
                format!("{}/jobs/claims", upload.base()),
                &actors.tester,
                json!({}),
                201,
            )
            .key(CLAIM_KEY)?,
        )
        .await?
        .body;
    let job = Lease::job(&claimed)?;
    let (grant, artifact) = support::upload(
        &mut upload,
        &actors,
        &job,
        "step_log",
        "fixture-scorer/step_log/kept.log",
        KEPT,
        "text/plain",
    )
    .await?;
    let url = reqwest::Url::parse(&string(&grant["upload_url"])?)?;
    let upload_id = url.path().rsplit('/').next().ok_or("upload ID absent")?;
    let upload_meta = json!({"slug":upload.project,"project_id":support::project_id(&mut upload,&actors).await?,"attempt_id":attempt.document["id"],"number":attempt.document["number"],"job_id":job.document["job_id"],"upload_id":upload_id,"artifact_id":artifact["id"]});
    upload
        .finish_coverage(&actors.admin, "recovery-prepare-upload")
        .await?;
    let (mut evaluation, actors) = World::new("recovery-evaluation").await?;
    let attempt = support::submit(&mut evaluation, &actors).await?;
    let job = evaluation.claim_job(&actors, false, false).await?;
    assert_eq!(
        support::complete_tester(&mut evaluation, &actors, &job).await?["state"],
        "completed"
    );
    let jobs = evaluation
        .api(Call::get(
            &format!("{ATTEMPT}/jobs"),
            format!("{}/jobs", evaluation.attempt_path(&attempt)),
            &actors.admin,
        ))
        .await?
        .body;
    let pending = jobs["items"]
        .as_array()
        .ok_or("jobs absent")?
        .iter()
        .filter(|row| row["stage"] == "evaluator")
        .collect::<Vec<_>>();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["state"], "pending");
    let evaluation_meta = json!({"slug":evaluation.project,"project_id":support::project_id(&mut evaluation,&actors).await?,"attempt_id":attempt.document["id"],"number":attempt.document["number"],"test_job_id":job.document["job_id"],"removed_job_id":pending[0]["id"]});
    evaluation
        .finish_coverage(&actors.admin, "recovery-prepare-evaluation")
        .await?;
    let metadata = json!({"version":1,"upload":upload_meta,"evaluation":evaluation_meta});
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(support::metadata_path()?)?;
    file.write_all(&serde_json::to_vec_pretty(&metadata)?)?;
    Ok(())
}

fn fixture_base(fixture: &Value) -> Result<String> {
    Ok(format!("/api/projects/{}", string(&fixture["slug"])?))
}
fn attempt_path(fixture: &Value) -> Result<String> {
    Ok(format!(
        "{}/hypotheses/{}/attempts/1",
        fixture_base(fixture)?,
        fixture["number"]
    ))
}
fn rows(body: &Value) -> Result<&Vec<Value>> {
    body["items"]
        .as_array()
        .ok_or("response items absent".into())
}

#[tokio::test]
#[ignore = "requires the launcher's isolated two-phase recovery profile"]
async fn exercise_recovery_http() -> Result<()> {
    let metadata: Value = serde_json::from_slice(&std::fs::read(support::metadata_path()?)?)?;
    assert_eq!(metadata["version"], 1);
    let mut login = Login::new().await?;
    let audit = login.h.fetch_audit(&login.admin, 0).await?;
    let after = audit.body["next_after"]
        .as_u64()
        .ok_or("audit cursor absent")?;
    for action in ["upload.failed", "attempt.evaluation_restarted"] {
        assert!(!rows(&audit.body)?.iter().any(|row| row["action"] == action));
    }
    let report = login.h.sweep(&login.admin).await?;
    assert_eq!(report["uploads_failed"], 1);
    assert_eq!(report["evaluations_started"], 1);
    assert_eq!(report["objects_deleted"], 0);
    assert_eq!(report["errors"], 0);
    for field in [
        "attempts_expired",
        "attempts_requeued",
        "jobs_failed",
        "jobs_rerun",
        "evaluations_refused",
        "uploads_expired",
        "staging_deleted",
    ] {
        assert_eq!(report[field], 0);
    }
    let events = login.h.fetch_audit(&login.admin, after).await?.body;
    let again = login.h.sweep(&login.admin).await?;
    for field in [
        "uploads_failed",
        "evaluations_started",
        "objects_deleted",
        "errors",
    ] {
        assert_eq!(again[field], 0);
    }
    assert_eq!(login.h.fetch_audit(&login.admin, after).await?.body, events);
    assert_events(&events, &metadata)?;
    assert_job_created(&events, &metadata["evaluation"])?;
    exercise_upload(&mut login, &metadata["upload"]).await?;
    exercise_evaluation(&mut login, &metadata["evaluation"], &events, &audit.body).await?;
    login.finish().await
}

fn assert_events(events: &Value, metadata: &Value) -> Result<()> {
    assert_eq!(rows(events)?.len(), 3);
    for (action, kind, subject, fixture) in [
        ("upload.failed", "upload", "upload_id", &metadata["upload"]),
        (
            "attempt.evaluation_restarted",
            "attempt",
            "attempt_id",
            &metadata["evaluation"],
        ),
    ] {
        let events = rows(events)?
            .iter()
            .filter(|row| row["action"] == action)
            .collect::<Vec<_>>();
        assert_eq!(events.len(), 1);
        let event = events[0];
        assert_eq!(event["subject_type"], kind);
        assert_eq!(event["subject_id"], fixture[subject]);
        assert_eq!(event["project_id"], fixture["project_id"]);
        assert_eq!(event["actor_kind"], "system");
        assert_eq!(event["via_channel"], "system");
        for field in [
            "actor_user_id",
            "actor_service_id",
            "via_client",
            "idempotency_key",
        ] {
            assert!(event[field].is_null());
        }
        assert_ne!(string(&event["occurred_at"])?, "");
        assert_ne!(string(&event["reason"])?, "");
        if kind == "upload" {
            assert_eq!(event["prior_state"], json!({"state":"pending"}));
            assert_eq!(event["new_state"]["state"], "failed");
            assert_eq!(event["new_state"]["object_deleted"], false);
            assert_eq!(
                event["new_state"]
                    .as_object()
                    .ok_or("upload state absent")?
                    .len(),
                3
            );
            assert_ne!(string(&event["new_state"]["key"])?, "");
        } else {
            assert_eq!(event["prior_state"], json!({"state":"evaluating"}));
            assert_eq!(event["new_state"]["test_job_id"], fixture["test_job_id"]);
            assert_eq!(
                event["new_state"]
                    .as_object()
                    .ok_or("evaluation state absent")?
                    .len(),
                3
            );
            assert_ne!(
                event["new_state"]["evaluation_job_id"],
                fixture["removed_job_id"]
            );
            assert_ne!(string(&event["new_state"]["evidence_id"])?, "");
        }
    }
    Ok(())
}

fn assert_job_created(events: &Value, fixture: &Value) -> Result<()> {
    let events = rows(events)?;
    let created = events
        .iter()
        .find(|row| row["action"] == "job.created")
        .ok_or("created event absent")?;
    let restarted = events
        .iter()
        .find(|row| row["action"] == "attempt.evaluation_restarted")
        .ok_or("restart event absent")?;
    assert_eq!(
        created["subject_id"],
        restarted["new_state"]["evaluation_job_id"]
    );
    assert_eq!(created["subject_type"], "job");
    assert_eq!(created["project_id"], fixture["project_id"]);
    assert_eq!(created["actor_kind"], "system");
    assert_eq!(created["via_channel"], "system");
    for field in [
        "actor_user_id",
        "actor_service_id",
        "via_client",
        "prior_state",
        "reason",
        "idempotency_key",
    ] {
        assert!(created[field].is_null());
    }
    assert_eq!(
        created["new_state"],
        json!({"state":"pending","stage":"evaluator","run_number":1,"attempt_id":fixture["attempt_id"],"service":"stock-evaluator","origin":"submission","previous_run_id":null,"steps":[]})
    );
    assert_eq!(created["occurred_at"], restarted["occurred_at"]);
    Ok(())
}

async fn exercise_upload(login: &mut Login, fixture: &Value) -> Result<()> {
    let admin = login.admin.clone();
    let base = fixture_base(fixture)?;
    let template = "/api/projects/{slug}/artifacts/{artifact_id}";
    let path = format!("{base}/artifacts/{}", string(&fixture["artifact_id"])?);
    let response = login
        .h
        .request(Method::GET, &path)?
        .bearer_auth(&admin)
        .send()
        .await?;
    let artifact = login
        .h
        .check_response(Method::GET, template, response, 200)
        .await?;
    assert_eq!(artifact.raw_body, KEPT);
    assert_eq!(sha(&artifact.raw_body), sha(KEPT));
    let tester = login
        .mint(
            "/api/projects/{slug}/service-accounts/{name}/tokens",
            &format!("{base}/service-accounts/cannery-runner/tokens"),
            "recovery-tester",
        )
        .await?;
    let claim = login
        .api(
            Call::post(
                "/api/projects/{slug}/jobs/claims",
                format!("{base}/jobs/claims"),
                &tester,
                json!({}),
                200,
            )
            .key(CLAIM_KEY)?,
        )
        .await?;
    let lease = Lease::job(&claim)?;
    assert_eq!(lease.document["job_id"], fixture["job_id"]);
    assert_eq!(lease.generation, 2);
    let conflict=login.api(Call::post(&format!("{JOB}/uploads"),format!("{base}/jobs/{}/uploads",string(&fixture["job_id"])?),&tester,json!({"role":"step_log","path":"fixture-scorer/step_log/kept.log","size_bytes":KEPT.len(),"sha256":sha(KEPT),"media_type":"text/plain"}),409).lease(&lease)?).await?;
    assert_eq!(conflict["error"]["code"], "conflict");
    let attempt = login
        .api(Call::get(ATTEMPT, attempt_path(fixture)?, &admin))
        .await?;
    assert_eq!(attempt["state"], "testing");
    assert_eq!(attempt["failures"], json!([]));
    Ok(())
}

async fn exercise_evaluation(
    login: &mut Login,
    fixture: &Value,
    events: &Value,
    prior_audit: &Value,
) -> Result<()> {
    let base = fixture_base(fixture)?;
    let admin = login.admin.clone();
    let restarted = rows(events)?
        .iter()
        .find(|row| row["action"] == "attempt.evaluation_restarted")
        .ok_or("restart event absent")?;
    let id = &restarted["new_state"]["evaluation_job_id"];
    let jobs = login
        .api(Call::get(
            &format!("{ATTEMPT}/jobs"),
            format!("{}/jobs", attempt_path(fixture)?),
            &admin,
        ))
        .await?;
    let evaluator = rows(&jobs)?
        .iter()
        .filter(|row| row["stage"] == "evaluator")
        .collect::<Vec<_>>();
    assert_eq!(evaluator.len(), 1);
    assert_eq!(evaluator[0]["id"], *id);
    assert_eq!(evaluator[0]["state"], "pending");
    assert_eq!(evaluator[0]["science_revision"], 1);
    assert_eq!(evaluator[0]["run_number"], 1);
    assert_eq!(evaluator[0]["tester"], "stock-evaluator");
    let token = login
        .mint(
            "/api/projects/{slug}/service-accounts/{name}/tokens",
            &format!("{base}/service-accounts/stock-evaluator/tokens"),
            "recovery-evaluator",
        )
        .await?;
    let claim = login
        .api(Call::post(
            "/api/projects/{slug}/jobs/claims",
            format!("{base}/jobs/claims"),
            &token,
            json!({"stage":"evaluator","revision":"fixture-policy-1"}),
            201,
        ))
        .await?;
    let lease = Lease::job(&claim)?;
    assert_eq!(lease.document["job_id"], *id);
    assert_eq!(
        lease.document["evaluator"],
        json!({"id":"stock-evaluator","revision":"fixture-policy-1"})
    );
    assert_eq!(lease.document["science_revision"], "1");
    let completed = rows(prior_audit)?
        .iter()
        .find(|row| row["action"] == "job.completed" && row["subject_id"] == fixture["test_job_id"])
        .ok_or("tester completion absent")?;
    assert_eq!(
        lease.document["inputs"]["evidence"],
        json!([{"ref":restarted["new_state"]["evidence_id"],"sha256":completed["new_state"]["evidence_sha256"]}])
    );
    assert_eq!(
        lease.document["inputs"]["manifest"]["sha256"],
        completed["new_state"]["manifest_sha256"]
    );
    let attempt = login
        .api(Call::get(ATTEMPT, attempt_path(fixture)?, &admin))
        .await?;
    assert_eq!(attempt["state"], "evaluating");
    assert_eq!(attempt["failures"], json!([]));
    let detail = login
        .api(Call::get(
            "/api/projects/{slug}/jobs/{job_id}",
            format!("{base}/jobs/{}", string(id)?),
            &admin,
        ))
        .await?;
    assert_eq!(detail["state"], "claimed");
    assert_eq!(detail["lease_generation"], 1);
    Ok(())
}
