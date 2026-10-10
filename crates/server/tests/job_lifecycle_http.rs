//! Focused integrity tests for explicitly installed worker lifecycle controllers.
#![allow(
    clippy::too_many_lines,
    clippy::similar_names,
    reason = "Scenario assertions retain mutation and rollback order"
)]
#[path = "support/upload_wire.rs"]
mod wire;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use cannery_core::{contracts::ContractValidator, settings::load_settings};
use cannery_server::{
    application_with_job_lifecycle_and_upload_context, job_completion_routes::JobLifecycleContext,
    job_lifecycle,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fmt::Write as _, sync::Arc};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
fn profile() -> Result<JobLifecycleContext> {
    Ok(JobLifecycleContext {
        flow: job_lifecycle::Context {
            jobs: cannery_jobs::repo::JsonContext {
                encode_nesting_budget: 80,
                decode_nesting_budget: 80,
            },
            attempts: cannery_attempts::model::JsonContext {
                encode_nesting_budget: 80,
                decode_nesting_budget: 80,
            },
            config: cannery_research::config_repo::JsonContext {
                encode_nesting_budget: 80,
                decode_nesting_budget: 80,
            },
            hypotheses: cannery_hypotheses::repo::JsonContext {
                encode_nesting_budget: 80,
                decode_nesting_budget: 80,
            },
            rendering: cannery_research::science::RenderingContext { nesting_budget: 80 },
        },
        contracts: ContractValidator::new()?,
        phases: cannery_core::contracts::phases::PhaseSchemas::new()?,
        repr_budget: 80,
        response: cannery_server::job_read_wire::ResponseContext {
            inferred_nesting_budget: 80,
            representation_budget: 80,
        },
    })
}
/// A researcher authenticates with a personal token, a service account with its own.
fn bearer(role: &str) -> String {
    if role == "researcher" {
        "Bearer cr_pat_track_http_researcher".into()
    } else {
        format!("Bearer cr_svc_track_http_{role}")
    }
}
/// A verification report: the front matter, one YAML key per line, then a short body.
fn report(front_matter: &Value) -> String {
    let mut text = String::from("---\n");
    for (key, value) in front_matter.as_object().into_iter().flatten() {
        let _ = writeln!(text, "{key}: {value}");
    }
    text.push_str("---\n\nThe scorer completed with no missing rows.\n");
    text
}
/// The front matter of a passing report on the fixture run under science revision 3.
fn verification(policy: &str, dataset: bool) -> Value {
    let mut provenance = json!({"source_revision":"source-1","science_revision":"3"});
    if dataset {
        provenance["dataset_revision"] = json!("data-1");
    }
    json!({"verdict":"pass","reason":"bounded verification","policy_revision":policy,"gates":[{"id":"coverage","result":"pass"}],"measurements":[],"provenance":provenance})
}
async fn call(
    app: &Router,
    endpoint: &str,
    role: &str,
    token: &str,
    generation: &str,
    value: &Value,
    key: Option<&str>,
) -> Result<(u16, Value, bool)> {
    let mut request = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/projects/matrix/jobs/{}/{endpoint}",
            value["job_id"]
                .as_str()
                .unwrap_or("00000000-0000-0000-0000-000000006005")
        ))
        .header("authorization", bearer(role))
        .header("content-type", "application/json")
        .header("x-lease-token", token)
        .header("x-lease-generation", generation);
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(serde_json::to_vec(value)?))?)
        .await?;
    let status = response.status().as_u16();
    let replay = response
        .extensions()
        .get::<cannery_server::job_completion_routes::CompletionReplay>()
        .is_some_and(|r| r.0);
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await?;
    Ok((
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({"wire":String::from_utf8_lossy(&bytes)})),
        replay,
    ))
}
#[tokio::test]
#[ignore = "requires guarded uniquely owned migrated PostgreSQL"]
async fn job_lifecycle_integrity() -> Result<()> {
    let url = std::env::var("CANNERY_JOB_LIFECYCLE_DATABASE_URL")?;
    let settings = load_settings(
        None,
        &BTreeMap::from([("CANNERY_DATABASE_URL".into(), url)]),
    )?;
    let lifecycle = Arc::new(profile()?);
    let root =
        Objects(std::env::temp_dir().join(format!("cannery-job-uploads-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(&root.0)?;
    let uploads = Arc::new(cannery_server::upload_routes::UploadContext {
        cancellation: cannery_server::upload_routes::CancellationTasks::new(),
        settlement_timeout: std::time::Duration::from_secs(10),
        repository: cannery_attempts::model::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
        store: Arc::new(cannery_storage::ObjectStore::Local(
            cannery_storage::local::LocalStore::new(&root.0, "local")?,
        )),
        signing_clock: std::time::SystemTime::now,
        mint: || {
            Ok(cannery_identity::secrets::new_secret("cr_upl_")?
                .plaintext()
                .clone())
        },
        nonce: || Ok(uuid::Uuid::new_v4().simple().to_string()),
    });
    let job_uploads = Arc::new(cannery_server::job_upload_routes::JobUploadContext {
        lifecycle: lifecycle.clone(),
        uploads: uploads.clone(),
        json_max_bytes: 1024,
        validation_slots: Arc::new(tokio::sync::Semaphore::new(1)),
        validation_timeout: std::time::Duration::from_secs(10),
    });
    let (app, state) = application_with_job_lifecycle_and_upload_context(
        settings,
        lifecycle,
        job_uploads.clone(),
    )?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("owned child required")?;
    assert!(suffix.len() == 24 && suffix.bytes().all(|b| b.is_ascii_hexdigit()));
    for seed in [
        include_str!("fixtures/attempt_reads_http/seed.sql"),
        include_str!("fixtures/job_claims_http/seed.sql"),
        include_str!("fixtures/job_lifecycle_http/seed.sql"),
    ] {
        sqlx::raw_sql(seed).execute(&state.pool).await?;
    }
    let failure = json!({"schema_version":"0.2","job_id":"00000000-0000-0000-0000-000000006005","step":"scorer","error_code":"runner_failed","reason":"bounded fixture failure","logs":[]});
    for (role, token, generation, expected) in [
        ("verifier", "wrong", "9", 409),
        ("verifier", "fixture-held", "8", 409),
        ("other-verifier", "fixture-held", "9", 403),
        ("agent", "fixture-held", "9", 403),
        ("researcher", "fixture-held", "9", 403),
    ] {
        let (status, value, _) =
            call(&app, "failure", role, token, generation, &failure, None).await?;
        assert_eq!(status, expected, "{value}");
    }
    for column in ["lease_expires_at", "deadline"] {
        let sql = format!(
            "UPDATE jobs SET {column}=now()-interval '1 second' WHERE id='00000000-0000-0000-0000-000000006005'"
        );
        sqlx::query(&sql).execute(&state.pool).await?;
        let (status, value, _) = call(
            &app,
            "failure",
            "verifier",
            "fixture-held",
            "9",
            &failure,
            None,
        )
        .await?;
        assert_eq!(status, 409, "expired {column}: {value}");
        let sql = format!(
            "UPDATE jobs SET {column}=now()+interval '1 hour' WHERE id='00000000-0000-0000-0000-000000006005'"
        );
        sqlx::query(&sql).execute(&state.pool).await?;
    }
    let before: String = sqlx::query_scalar(
        "SELECT row_to_json(j)::text FROM jobs j WHERE id='00000000-0000-0000-0000-000000006005'",
    )
    .fetch_one(&state.pool)
    .await?;
    sqlx::query("UPDATE fixture_job_lifecycle_fault SET enabled=true")
        .execute(&state.pool)
        .await?;
    let (status, value, _) = call(
        &app,
        "failure",
        "verifier",
        "fixture-held",
        "9",
        &failure,
        None,
    )
    .await?;
    assert_eq!(status, 500, "{value}");
    sqlx::query("UPDATE fixture_job_lifecycle_fault SET enabled=false")
        .execute(&state.pool)
        .await?;
    let after: String = sqlx::query_scalar(
        "SELECT row_to_json(j)::text FROM jobs j WHERE id='00000000-0000-0000-0000-000000006005'",
    )
    .fetch_one(&state.pool)
    .await?;
    assert_eq!(before, after, "audit failure must roll back the job");
    let (status, value, _) = call(
        &app,
        "failure",
        "verifier",
        "fixture-held",
        "9",
        &failure,
        None,
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    // The runner's failure is within the rerun budget: a second run waits for the verifier.
    let states:Vec<(String,String,Option<String>)> =sqlx::query_as("SELECT state,origin,error_code FROM jobs WHERE attempt_id='00000000-0000-0000-0000-000000002005' ORDER BY run_number").fetch_all(&state.pool).await?;
    assert_eq!(
        states,
        vec![
            (
                "failed".into(),
                "submission".into(),
                Some("runner_failed".into())
            ),
            ("pending".into(), "auto_retry".into(), None)
        ]
    );
    let attempt: String = sqlx::query_scalar(
        "SELECT state FROM attempts WHERE id='00000000-0000-0000-0000-000000002005'",
    )
    .fetch_one(&state.pool)
    .await?;
    assert_eq!(attempt, "verifying");
    let failures:i64=sqlx::query_scalar("SELECT count(*) FROM attempt_failures WHERE attempt_id='00000000-0000-0000-0000-000000002005'").fetch_one(&state.pool).await?;
    assert_eq!(failures, 0);
    let audits:i64=sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action='job.failed' AND subject_id='00000000-0000-0000-0000-000000006005'").fetch_one(&state.pool).await?;
    assert_eq!(audits, 1);
    let (status, value, _) = call(
        &app,
        "failure",
        "verifier",
        "fixture-held",
        "9",
        &failure,
        None,
    )
    .await?;
    assert_eq!(status, 409, "{value}");
    publication_and_upload(&app, &state.pool, &uploads, &root.0, &job_uploads).await?;
    rerun_budget(&app, &state.pool).await?;
    agent_publication_race(&app, &state.pool).await?;
    researcher_comparison_publication(&app, &state.pool).await?;
    document_and_skip(&app, &state.pool).await?;
    uploads.cancellation.drain().await?;
    state.pool.close().await;
    Ok(())
}
struct Objects(std::path::PathBuf);
impl Drop for Objects {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
async fn publication_and_upload(
    app: &Router,
    pool: &sqlx::PgPool,
    uploads: &cannery_server::upload_routes::UploadContext,
    root: &std::path::Path,
    validation: &cannery_server::job_upload_routes::JobUploadContext,
) -> Result<()> {
    let grant = json!({"role":"output","path":"scorer/output/file.json","size_bytes":2,"sha256":"44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a","media_type":"application/json","interface":"fixture-json/v1"});
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/projects/matrix/jobs/00000000-0000-0000-0000-000000006006/uploads")
                .header("authorization", "Bearer cr_svc_track_http_verifier")
                .header("content-type", "application/json")
                .header("x-lease-token", "fixture-held-second")
                .header("x-lease-generation", "9")
                .body(Body::from(serde_json::to_vec(&grant)?))?,
        )
        .await?;
    let status = response.status().as_u16();
    let grant: Value = serde_json::from_slice(&to_bytes(response.into_body(), 10000).await?)?;
    assert_eq!(status, 201, "{grant}");
    let url = grant["upload_url"].as_str().ok_or("upload URL")?;
    let parsed = reqwest::Url::parse(url)?;
    let path = parsed.path();
    let token = grant["headers"]["X-Upload-Token"]
        .as_str()
        .ok_or("upload token")?;
    let denied = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(path)
                .header("x-upload-token", "wrong")
                .body(Body::from("{}"))?,
        )
        .await?;
    assert_eq!(denied.status(), 404);
    for endpoint in [format!("{path}/presign"), format!("{path}/finish")] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(endpoint)
                    .header("x-upload-token", token)
                    .body(Body::empty())?,
            )
            .await?;
        assert!(
            response.status().is_client_error(),
            "local capability must not perform direct-S3 phases"
        );
    }
    // Hold the real controller's only validation permit. Timeout must reset
    // the fully received grant for retry without publishing an artifact.
    let count_before: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM artifacts WHERE job_id='00000000-0000-0000-0000-000000006006'",
    )
    .fetch_one(pool)
    .await?;
    let permit = validation.validation_slots.acquire().await?;
    let started = tokio::time::Instant::now();
    let blocked = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(path)
                .header("x-upload-token", token)
                .body(Body::from("{}"))?,
        )
        .await?;
    assert_eq!(blocked.status(), 503);
    let refusal: Value = serde_json::from_slice(&to_bytes(blocked.into_body(), 4096).await?)?;
    assert_eq!(refusal["error"]["code"], "store_unavailable");
    assert!(started.elapsed() >= validation.validation_timeout);
    let upload_id = uuid::Uuid::parse_str(path.rsplit('/').next().ok_or("upload ID")?)?;
    let reset: String = sqlx::query_scalar("SELECT state FROM uploads WHERE id=$1")
        .bind(upload_id)
        .fetch_one(pool)
        .await?;
    assert_eq!(reset, "pending");
    let count_after: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM artifacts WHERE job_id='00000000-0000-0000-0000-000000006006'",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(
        count_before, count_after,
        "timeout must publish no artifact"
    );
    assert_eq!(std::fs::read_dir(root.join("local/.staging"))?.count(), 0);
    drop(permit);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(path)
                .header("x-upload-token", token)
                .body(Body::from("{}"))?,
        )
        .await?;
    let status = response.status().as_u16();
    let artifact: Value = serde_json::from_slice(&to_bytes(response.into_body(), 10000).await?)?;
    assert_eq!(status, 201, "{artifact}");
    assert_eq!(artifact["size_bytes"], 2);
    let checked:bool=sqlx::query_scalar("SELECT content_validated FROM artifacts WHERE job_id='00000000-0000-0000-0000-000000006006'").fetch_one(pool).await?;
    assert!(checked);
    cancelled_receive(app, pool, uploads, root).await?;
    direct_s3_upload().await?;
    let mut front_matter = verification("policy-1", true);
    front_matter["artifact_roles"] = json!(["output"]);
    let object = json!({"role":artifact["role"],"storage":artifact["storage"],"size_bytes":artifact["size_bytes"],"sha256":artifact["sha256"],"media_type":artifact["media_type"]});
    let completion = json!({"schema_version":"0.2","job_id":"00000000-0000-0000-0000-000000006006","document":report(&front_matter),"manifest":{"schema_version":"0.2","attempt_id":"00000000-0000-0000-0000-000000002006","objects":[object]}});
    let (status, value, replay) = call(
        app,
        "completion",
        "verifier",
        "fixture-held-second",
        "9",
        &completion,
        Some("complete-once"),
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    assert!(!replay);
    let (status, value, replay) = call(
        app,
        "completion",
        "verifier",
        "wrong",
        "1",
        &completion,
        Some("complete-once"),
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    assert!(replay);
    let mut conflict = completion.clone();
    front_matter["reason"] = json!("different output");
    conflict["document"] = json!(report(&front_matter));
    let (status, value, _) = call(
        app,
        "completion",
        "verifier",
        "fixture-held-second",
        "9",
        &conflict,
        Some("complete-once"),
    )
    .await?;
    assert_eq!(status, 409, "{value}");
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM phase_outputs WHERE attempt_id='00000000-0000-0000-0000-000000002006' AND stage='verification'),(SELECT count(*) FROM manifests WHERE attempt_id='00000000-0000-0000-0000-000000002006' AND stage='verify'),(SELECT count(*) FROM idempotency_keys WHERE scope='job.complete')").fetch_one(pool).await?;
    assert_eq!(counts, (1, 1, 1));
    // Publication marks the attempt verified and queues the hypothesis's write-up.
    let (attempt,hypothesis,cases,jobs):(String,String,i64,i64)=sqlx::query_as("SELECT a.state,h.state,(SELECT count(*) FROM review_cases c WHERE c.attempt_id=a.id),(SELECT count(*) FROM jobs j WHERE j.attempt_id=a.id AND j.phase='document' AND j.state='pending') FROM attempts a JOIN hypotheses h ON h.id=a.hypothesis_id WHERE a.id='00000000-0000-0000-0000-000000002006'").fetch_one(pool).await?;
    assert_eq!(
        (attempt, hypothesis, cases, jobs),
        ("verified".into(), "documenting".into(), 0, 1)
    );
    Ok(())
}
async fn rerun_budget(app: &Router, pool: &sqlx::PgPool) -> Result<()> {
    sqlx::raw_sql("INSERT INTO hypotheses(id,project_id,number,track_id,title,created_by_user,state,revision,approved_revision,approved_at) SELECT '00000000-0000-0000-0000-000000001405',project_id,1405,track_id,'Rerun budget',created_by_user,'active',2,2,approved_at FROM hypotheses WHERE id='00000000-0000-0000-0000-000000001005';
    INSERT INTO hypothesis_revisions(hypothesis_id,revision,content,science_revision,author_user,via_channel) SELECT '00000000-0000-0000-0000-000000001405',2,content,3,author_user,via_channel FROM hypothesis_revisions WHERE hypothesis_id='00000000-0000-0000-0000-000000001005' AND revision=2;
    INSERT INTO attempts(id,project_id,hypothesis_id,sequence,state,hypothesis_revision,science_revision,track_id,claimed_by_service,via_channel,lease_generation) VALUES('00000000-0000-0000-0000-000000002405','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000001405',1,'verifying',2,3,'00000000-0000-0000-0000-000000000004','00000000-0000-0000-0000-000000000020','api',0);
    INSERT INTO phase_outputs(id,project_id,attempt_id,stage,status,front_matter,sha256,producer_service,via_channel) VALUES('00000000-0000-0000-0000-000000003405','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000002405','agent','completed','{\"provenance\":{\"source_revision\":\"source-1\"}}',repeat('a',64),'00000000-0000-0000-0000-000000000020','api');
    INSERT INTO jobs(id,project_id,attempt_id,phase,run_number,state,science_revision,performer,verifier_id,spec,deadline_seconds,claimed_by_service,via_channel,lease_generation,lease_token_hash,lease_expires_at,claimed_at,deadline) SELECT '00000000-0000-0000-0000-000000006107',project_id,'00000000-0000-0000-0000-000000002405','verify',1,'claimed',3,performer,verifier_id,jsonb_set(spec,'{inputs,run,ref}','\"00000000-0000-0000-0000-000000003405\"'),600,'00000000-0000-0000-0000-000000000023','api',1,sha256(convert_to('rerun-initial','UTF8')),now()+interval '1 hour',now(),now()+interval '2 hours' FROM jobs WHERE id='00000000-0000-0000-0000-000000006006';").execute(pool).await?;
    // The runner's report names another policy revision than the registered verifier's.
    let completion = json!({"schema_version":"0.2","job_id":"00000000-0000-0000-0000-000000006107","document":report(&verification("policy-2", true))});
    let (status, value, _) = call(
        app,
        "completion",
        "verifier",
        "rerun-initial",
        "1",
        &completion,
        None,
    )
    .await?;
    assert_eq!(
        status, 422,
        "invalid completion must durably fail and rerun: {value}"
    );
    assert_eq!(
        value["error"]["message"],
        "must match the registered verifier's policy revision; another run was queued"
    );
    let (id,origin,previous):(uuid::Uuid,String,uuid::Uuid)=sqlx::query_as("SELECT id,origin,previous_run_id FROM jobs WHERE attempt_id='00000000-0000-0000-0000-000000002405' AND run_number=2").fetch_one(pool).await?;
    assert_eq!(origin, "auto_retry");
    assert_eq!(previous.to_string(), "00000000-0000-0000-0000-000000006107");
    let error: String = sqlx::query_scalar(
        "SELECT error_code FROM jobs WHERE id='00000000-0000-0000-0000-000000006107'",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(error, "invalid_output");
    let state: String = sqlx::query_scalar(
        "SELECT state FROM attempts WHERE id='00000000-0000-0000-0000-000000002405'",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(state, "verifying");
    let prefix: String = sqlx::query_scalar("SELECT spec->>'output_prefix' FROM jobs WHERE id=$1")
        .bind(id)
        .fetch_one(pool)
        .await?;
    assert!(prefix.contains(&id.to_string()));
    sqlx::query("UPDATE jobs SET state='claimed',claimed_by_service='00000000-0000-0000-0000-000000000023',lease_generation=1,lease_token_hash=sha256(convert_to('rerun-second','UTF8')),lease_expires_at=now()+interval '1 hour',claimed_at=now(),deadline=now()+interval '2 hours' WHERE id=$1").bind(id).execute(pool).await?;
    let failure = json!({"schema_version":"0.2","job_id":id.to_string(),"error_code":"runner_failed","reason":"worker crash","logs":[]});
    let (status, value, _) = call(
        app,
        "failure",
        "verifier",
        "rerun-second",
        "1",
        &failure,
        None,
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    // The budget is spent: the attempt fails at the verify phase and awaits review.
    let states:Vec<String>=sqlx::query_scalar("SELECT state FROM jobs WHERE attempt_id='00000000-0000-0000-0000-000000002405' ORDER BY run_number").fetch_all(pool).await?;
    assert_eq!(states, vec!["failed", "failed"]);
    let (stage,code):(String,String)=sqlx::query_as("SELECT stage,code FROM attempt_failures WHERE attempt_id='00000000-0000-0000-0000-000000002405'").fetch_one(pool).await?;
    assert_eq!((stage, code), ("verify".into(), "runner_failed".into()));
    let state: String = sqlx::query_scalar(
        "SELECT state FROM attempts WHERE id='00000000-0000-0000-0000-000000002405'",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(state, "failed");
    Ok(())
}
async fn cancelled_receive(
    app: &Router,
    pool: &sqlx::PgPool,
    uploads: &cannery_server::upload_routes::UploadContext,
    root: &std::path::Path,
) -> Result<()> {
    let body = json!({"role":"output","path":"scorer/output/cancel.json","size_bytes":2,"sha256":"44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a","media_type":"application/json","interface":"fixture-json/v1"});
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/projects/matrix/jobs/00000000-0000-0000-0000-000000006006/uploads")
                .header("authorization", "Bearer cr_svc_track_http_verifier")
                .header("content-type", "application/json")
                .header("x-lease-token", "fixture-held-second")
                .header("x-lease-generation", "9")
                .body(Body::from(serde_json::to_vec(&body)?))?,
        )
        .await?;
    assert_eq!(response.status(), 201);
    let grant: Value = serde_json::from_slice(&to_bytes(response.into_body(), 10000).await?)?;
    let parsed = reqwest::Url::parse(grant["upload_url"].as_str().ok_or("upload URL")?)?;
    let token = grant["headers"]["X-Upload-Token"]
        .as_str()
        .ok_or("upload token")?;
    let id = uuid::Uuid::parse_str(parsed.path().rsplit('/').next().ok_or("upload ID")?)?;
    let (sender, receiver) = tokio::sync::mpsc::channel::<axum::body::Bytes>(2);
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver
            .recv()
            .await
            .map(|chunk| (Ok::<_, std::io::Error>(chunk), receiver))
    });
    let request = Request::builder()
        .method("PUT")
        .uri(parsed.path())
        .header("x-upload-token", token)
        .body(Body::from_stream(stream))?;
    let task = tokio::spawn(app.clone().oneshot(request));
    sender.send(axum::body::Bytes::from_static(b"{")).await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let count = std::fs::read_dir(root.join("local/.staging")).map_or(0, Iterator::count);
            if count > 0 {
                break;
            }
            if task.is_finished() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await?;
    if task.is_finished() {
        let response = task.await??;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 10000).await?;
        return Err(format!(
            "cancel receive ended early: {status} {}",
            String::from_utf8_lossy(&bytes)
        )
        .into());
    }
    task.abort();
    let _ = task.await;
    drop(sender);
    uploads.cancellation.drain().await?;
    let state: String = sqlx::query_scalar("SELECT state FROM uploads WHERE id=$1")
        .bind(id)
        .fetch_one(pool)
        .await?;
    assert_eq!(state, "pending");
    assert_eq!(
        std::fs::read_dir(root.join("local/.staging"))?.count(),
        0,
        "cancelled staging path must be unlinked"
    );
    let retry = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(parsed.path())
                .header("x-upload-token", token)
                .body(Body::from("{}"))?,
        )
        .await?;
    let status = retry.status();
    let bytes = to_bytes(retry.into_body(), 10000).await?;
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&bytes));
    Ok(())
}
async fn agent_publication_race(app: &Router, pool: &sqlx::PgPool) -> Result<()> {
    // A researcher ran attempt 2406; the agent service account verifies it.
    sqlx::raw_sql(r#"
INSERT INTO hypotheses(id,project_id,number,track_id,title,created_by_user,state,revision,approved_revision,approved_at)
SELECT '00000000-0000-0000-0000-000000001406',project_id,1406,track_id,'Agent verification',created_by_user,'active',2,2,approved_at FROM hypotheses WHERE id='00000000-0000-0000-0000-000000001006';
INSERT INTO hypothesis_revisions(hypothesis_id,revision,content,science_revision,author_user,via_channel)
SELECT '00000000-0000-0000-0000-000000001406',2,content,3,author_user,via_channel FROM hypothesis_revisions WHERE hypothesis_id='00000000-0000-0000-0000-000000001006' AND revision=2;
INSERT INTO attempts(id,project_id,hypothesis_id,sequence,state,hypothesis_revision,science_revision,track_id,claimed_by_user,via_channel,lease_generation) VALUES('00000000-0000-0000-0000-000000002406','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000001406',1,'verifying',2,3,'00000000-0000-0000-0000-000000000004','00000000-0000-0000-0000-000000000002','api',0);
INSERT INTO phase_outputs(id,project_id,attempt_id,stage,status,front_matter,sha256,producer_user,via_channel) VALUES('00000000-0000-0000-0000-000000003406','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000002406','agent','completed','{"provenance":{"source_revision":"source-1"}}',repeat('a',64),'00000000-0000-0000-0000-000000000002','api');
INSERT INTO jobs(id,project_id,attempt_id,phase,run_number,state,science_revision,performer,verifier_id,spec,deadline_seconds,claimed_by_service,via_channel,lease_generation,lease_token_hash,lease_expires_at,claimed_at,deadline) SELECT '00000000-0000-0000-0000-000000006108',project_id,'00000000-0000-0000-0000-000000002406','verify',1,'claimed',3,'agent',NULL,jsonb_set((spec-'verifier')||'{"performer":"agent"}','{inputs,run,ref}','"00000000-0000-0000-0000-000000003406"'),600,'00000000-0000-0000-0000-000000000020','api',1,sha256(convert_to('agent-held','UTF8')),now()+interval '1 hour',now(),now()+interval '2 hours' FROM jobs WHERE id='00000000-0000-0000-0000-000000006006';
"#).execute(pool).await?;
    // An agent's invalid report is refused, and the agent keeps the lease to correct it.
    let mut front_matter = verification("agent-checklist-1", true);
    front_matter["provenance"]["science_revision"] = json!("4");
    let invalid = json!({"schema_version":"0.2","job_id":"00000000-0000-0000-0000-000000006108","document":report(&front_matter)});
    let (status, value, _) = call(
        app,
        "completion",
        "agent",
        "agent-held",
        "1",
        &invalid,
        Some("agent-invalid"),
    )
    .await?;
    assert_eq!(status, 422, "{value}");
    assert_eq!(
        value["error"]["message"],
        "must match the job's pinned science revision"
    );
    let kept:(String,String,i64,i64)=sqlx::query_as("SELECT j.state,a.state,(SELECT count(*) FROM jobs WHERE attempt_id=a.id),(SELECT count(*) FROM attempt_failures WHERE attempt_id=a.id) FROM jobs j JOIN attempts a ON a.id=j.attempt_id WHERE j.id='00000000-0000-0000-0000-000000006108'").fetch_one(pool).await?;
    assert_eq!(kept, ("claimed".into(), "verifying".into(), 1, 0));
    let record = json!({"schema_version":"0.2","job_id":"00000000-0000-0000-0000-000000006108","document":report(&verification("agent-checklist-1", true))});
    let (first, second) = tokio::join!(
        call(
            app,
            "completion",
            "agent",
            "agent-held",
            "1",
            &record,
            Some("verification-once")
        ),
        call(
            app,
            "completion",
            "agent",
            "agent-held",
            "1",
            &record,
            Some("verification-once")
        )
    );
    let first = first?;
    let second = second?;
    assert_eq!(first.0, 200, "{}", first.1);
    assert_eq!(second.0, 200, "{}", second.1);
    assert_ne!(first.2, second.2);
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM phase_outputs WHERE attempt_id='00000000-0000-0000-0000-000000002406' AND stage='verification'),(SELECT count(*) FROM jobs WHERE attempt_id='00000000-0000-0000-0000-000000002406' AND phase='document' AND state='pending'),(SELECT count(*) FROM audit_events WHERE action='attempt.verified' AND subject_id='00000000-0000-0000-0000-000000002406')").fetch_one(pool).await?;
    assert_eq!(counts, (1, 1, 1));
    let (attempt,hypothesis):(String,String)=sqlx::query_as("SELECT a.state,h.state FROM attempts a JOIN hypotheses h ON h.id=a.hypothesis_id WHERE a.id='00000000-0000-0000-0000-000000002406'").fetch_one(pool).await?;
    assert_eq!(
        (attempt, hypothesis),
        ("verified".into(), "documenting".into())
    );
    let linked:bool=sqlx::query_scalar("SELECT e.producer_service='00000000-0000-0000-0000-000000000020' AND NOT EXISTS (SELECT 1 FROM review_cases c WHERE c.attempt_id=j.attempt_id) FROM jobs j JOIN phase_outputs e ON e.id=j.evidence_id WHERE j.id='00000000-0000-0000-0000-000000006108'").fetch_one(pool).await?;
    assert!(linked);
    Ok(())
}
async fn researcher_comparison_publication(app: &Router, pool: &sqlx::PgPool) -> Result<()> {
    // The agent service account ran attempt 2416; a researcher verifies it with a comparison.
    sqlx::raw_sql(r#"
INSERT INTO config_revisions(project_id,kind,revision,content,created_by)
SELECT project_id,kind,5,content||'{"metrics":[{"key":"mrr","splits":["dev"],"dimensions":[],"unit":"ratio","direction":"higher"}]}'::jsonb,created_by FROM config_revisions WHERE project_id='00000000-0000-0000-0000-000000000010' AND kind='science' AND revision=3;
INSERT INTO hypotheses(id,project_id,number,track_id,title,created_by_user,state,revision,approved_revision,approved_at)
SELECT '00000000-0000-0000-0000-000000001416',project_id,1416,track_id,'Researcher verification',created_by_user,'active',2,2,approved_at FROM hypotheses WHERE id='00000000-0000-0000-0000-000000001006';
INSERT INTO hypothesis_revisions(hypothesis_id,revision,content,science_revision,author_user,via_channel)
SELECT '00000000-0000-0000-0000-000000001416',2,content,5,author_user,via_channel FROM hypothesis_revisions WHERE hypothesis_id='00000000-0000-0000-0000-000000001006' AND revision=2;
INSERT INTO attempts(id,project_id,hypothesis_id,sequence,state,hypothesis_revision,science_revision,track_id,claimed_by_service,via_channel,lease_generation) VALUES('00000000-0000-0000-0000-000000002416','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000001416',1,'verifying',2,5,'00000000-0000-0000-0000-000000000004','00000000-0000-0000-0000-000000000020','api',0);
INSERT INTO phase_outputs(id,project_id,attempt_id,stage,status,front_matter,sha256,producer_service,via_channel) VALUES('00000000-0000-0000-0000-000000003416','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000002416','agent','completed','{"provenance":{"source_revision":"source-1"}}',repeat('a',64),'00000000-0000-0000-0000-000000000020','api');
INSERT INTO jobs(id,project_id,attempt_id,phase,run_number,state,science_revision,performer,verifier_id,spec,deadline_seconds,claimed_by_user,via_channel,lease_generation,lease_token_hash,lease_expires_at,claimed_at,deadline) SELECT '00000000-0000-0000-0000-000000006118',project_id,'00000000-0000-0000-0000-000000002416','verify',1,'claimed',5,'agent',NULL,jsonb_set(spec,'{inputs,run,ref}','"00000000-0000-0000-0000-000000003416"'),600,'00000000-0000-0000-0000-000000000002','api',1,sha256(convert_to('researcher-held','UTF8')),now()+interval '1 hour',now(),now()+interval '2 hours' FROM jobs WHERE id='00000000-0000-0000-0000-000000006108';
"#).execute(pool).await?;
    let mut front_matter = verification("reviewer-checklist-2", true);
    front_matter["provenance"]["science_revision"] = json!("5");
    front_matter["measurements"] = json!([{"metric":"mrr","split":"dev","dimensions":{},"authority":"tester_verified","value":0.42,"unit":"ratio","direction":"higher"}]);
    front_matter["comparisons"] = json!([{"metric":"mrr","split":"dev","dimensions":{},"source":"tester","value":0.42,"reference":{"value":0.4,"label":"baseline","kind":"baseline"}}]);
    let record = json!({"schema_version":"0.2","job_id":"00000000-0000-0000-0000-000000006118","document":report(&front_matter)});
    let (status, response, _) = call(
        app,
        "completion",
        "researcher",
        "researcher-held",
        "1",
        &record,
        Some("researcher-once"),
    )
    .await?;
    assert_eq!(status, 200, "{response}");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM comparisons WHERE attempt_id='00000000-0000-0000-0000-000000002416' AND metric='mrr' AND value=0.42")
        .fetch_one(pool).await?;
    assert_eq!(count, 1);
    let producer: Option<uuid::Uuid> = sqlx::query_scalar("SELECT producer_user FROM phase_outputs WHERE attempt_id='00000000-0000-0000-0000-000000002416' AND stage='verification'")
        .fetch_one(pool).await?;
    assert_eq!(
        producer.map(|v| v.to_string()).as_deref(),
        Some("00000000-0000-0000-0000-000000000002")
    );
    Ok(())
}
/// A write-up of `attempts` citing the verification report on `attempt`.
async fn writeup(
    pool: &sqlx::PgPool,
    attempt: &str,
    attempts: &str,
    summary: &str,
) -> Result<String> {
    let (id, sha256): (uuid::Uuid, String) = sqlx::query_as("SELECT id,sha256 FROM phase_outputs WHERE attempt_id=$1::uuid AND stage='verification' AND status='completed'")
        .bind(attempt)
        .fetch_one(pool)
        .await?;
    Ok(format!(
        "---\nsummary: \"{summary}\"\nattempts: [{attempts}]\nverification: {{\"ref\":\"{id}\",\"sha256\":\"{sha256}\"}}\n---\n\n## Results\n\nThe verifier confirmed the run.\n"
    ))
}
async fn researcher(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Result<(u16, Value)> {
    let request = Request::builder()
        .method(method)
        .uri(format!("/api/projects/matrix{uri}"))
        .header("authorization", bearer("researcher"))
        .header("content-type", "application/json");
    let response = app
        .clone()
        .oneshot(request.body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))?)
        .await?;
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await?;
    Ok((
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({"wire":String::from_utf8_lossy(&bytes)})),
    ))
}
/// The hypothesis's state, its open decision cases and the write-up the case cites.
async fn deciding(pool: &sqlx::PgPool, number: i32) -> Result<(String, i64, Option<uuid::Uuid>)> {
    Ok(sqlx::query_as("SELECT h.state,(SELECT count(*) FROM review_cases c WHERE c.hypothesis_id=h.id AND c.kind='decision' AND c.state='pending'),(SELECT max(c.writeup_id::text)::uuid FROM review_cases c WHERE c.hypothesis_id=h.id AND c.kind='decision') FROM hypotheses h WHERE h.project_id='00000000-0000-0000-0000-000000000010' AND h.number=$1")
        .bind(number)
        .fetch_one(pool)
        .await?)
}
async fn waiting(app: &Router) -> Result<Vec<i64>> {
    let (status, queue) = researcher(app, "GET", "/writeups", None).await?;
    assert_eq!(status, 200, "{queue}");
    Ok(queue["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["hypothesis"].as_i64())
        .collect())
}
async fn document_and_skip(app: &Router, pool: &sqlx::PgPool) -> Result<()> {
    // Hypotheses 6, 1406 and 1416 were verified above and wait for their write-up.
    let queue = waiting(app).await?;
    assert!(
        [6, 1406, 1416].iter().all(|n| queue.contains(n)),
        "{queue:?}"
    );
    let (status, pending) = researcher(app, "GET", "/hypotheses/1406/writeup", None).await?;
    assert_eq!(status, 200, "{pending}");
    assert_eq!(pending["status"], "pending");
    assert_eq!(pending["inputs"]["attempts"], json!([1]));
    let job = pending["job_id"].as_str().ok_or("document job")?.to_owned();
    // The agent service account holds the document job and completes it like any job.
    sqlx::query("UPDATE jobs SET state='claimed',claimed_by_service='00000000-0000-0000-0000-000000000020',lease_generation=1,lease_token_hash=sha256(convert_to('documenter-held','UTF8')),lease_expires_at=now()+interval '1 hour',claimed_at=now(),deadline=now()+interval '2 hours' WHERE id=$1::uuid")
        .bind(&job)
        .execute(pool)
        .await?;
    let attempt = "00000000-0000-0000-0000-000000002406";
    let wrong = json!({"schema_version":"0.2","job_id":job,"document":writeup(pool, attempt, "1, 2", "The agent verified the run.").await?});
    let (status, value, _) = call(
        app,
        "completion",
        "agent",
        "documenter-held",
        "1",
        &wrong,
        None,
    )
    .await?;
    assert_eq!(status, 422, "{value}");
    let record = json!({"schema_version":"0.2","job_id":job,"document":writeup(pool, attempt, "1", "The agent verified the run.").await?});
    let (status, value, _) = call(
        app,
        "completion",
        "agent",
        "documenter-held",
        "1",
        &record,
        Some("writeup-once"),
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    let (state, open, writeup_id) = deciding(pool, 1406).await?;
    assert_eq!((state.as_str(), open), ("deciding", 1));
    let (status, written) = researcher(app, "GET", "/hypotheses/1406/writeup", None).await?;
    assert_eq!(status, 200, "{written}");
    assert_eq!(written["status"], "written");
    assert_eq!(
        written["writeup"]["front_matter"]["summary"],
        "The agent verified the run."
    );
    assert_eq!(
        written["writeup"]["id"].as_str(),
        writeup_id.map(|id| id.to_string()).as_deref()
    );
    // A researcher writes hypothesis 6 up: the job is claimed and completed in one action.
    let attempt = "00000000-0000-0000-0000-000000002006";
    let (status, value) = researcher(
        app,
        "POST",
        "/hypotheses/6/writeup",
        Some(&json!({"document":"   "})),
    )
    .await?;
    assert_eq!(status, 422, "{value}");
    let document = writeup(pool, attempt, "1", "The researcher wrote the run up.").await?;
    let (status, value) = researcher(
        app,
        "POST",
        "/hypotheses/6/writeup",
        Some(&json!({ "document": document })),
    )
    .await?;
    assert_eq!(status, 201, "{value}");
    assert_eq!(
        (&value["status"], &value["hypothesis_state"]),
        (&json!("written"), &json!("deciding"))
    );
    assert_eq!(
        value["writeup"]["written_by_user"],
        "00000000-0000-0000-0000-000000000002"
    );
    let (status, value) = researcher(
        app,
        "POST",
        "/hypotheses/6/writeup",
        Some(&json!({ "document": document })),
    )
    .await?;
    assert_eq!(status, 409, "{value}");
    let (state, open, writeup_id) = deciding(pool, 6).await?;
    assert_eq!(
        (state.as_str(), open, writeup_id.is_some()),
        ("deciding", 1, true)
    );
    // A researcher skips hypothesis 1416's write-up, and says why.
    let (status, value) = researcher(
        app,
        "POST",
        "/hypotheses/1416/writeup/skip",
        Some(&json!({"reason":" "})),
    )
    .await?;
    assert_eq!(status, 422, "{value}");
    let (status, value) = researcher(
        app,
        "POST",
        "/hypotheses/1416/writeup/skip",
        Some(&json!({"reason":"The comparison says it all."})),
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    assert_eq!(
        (&value["status"], &value["skip_reason"]),
        (&json!("skipped"), &json!("The comparison says it all."))
    );
    let (state, open, writeup_id) = deciding(pool, 1416).await?;
    assert_eq!((state.as_str(), open, writeup_id), ("deciding", 1, None));
    let (status, value) = researcher(
        app,
        "POST",
        "/hypotheses/1416/writeup/skip",
        Some(&json!({"reason":"Again."})),
    )
    .await?;
    assert_eq!(status, 409, "{value}");
    let queue = waiting(app).await?;
    assert!(
        [6, 1406, 1416].iter().all(|n| !queue.contains(n)),
        "{queue:?}"
    );
    let audits: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_events WHERE subject_type='job' AND subject_id IN (SELECT id::text FROM jobs WHERE phase='document') ORDER BY action")
        .fetch_all(pool)
        .await?;
    assert_eq!(
        audits,
        vec![
            "job.claimed",
            "job.completed",
            "job.completed",
            "job.created",
            "job.created",
            "job.created",
            "job.skipped"
        ]
    );
    Ok(())
}
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        out.push(char::from(b"0123456789abcdef"[usize::from(byte & 15)]));
    }
    out
}
async fn direct_s3_upload() -> Result<()> {
    let (endpoint, wire_state, _wire_task) = wire::start().await?;
    let mut env = BTreeMap::from([(
        "CANNERY_DATABASE_URL".to_owned(),
        std::env::var("CANNERY_JOB_LIFECYCLE_DATABASE_URL")?,
    )]);
    for (name, value) in [
        ("BACKEND", "s3"),
        ("BUCKET", "fixture"),
        ("S3_ENDPOINT", endpoint.as_str()),
        ("S3_PUBLIC_ENDPOINT", "https://s3.example.org"),
        ("S3_ACCESS_KEY_ID", "fixture-key"),
        ("S3_SECRET_ACCESS_KEY", "fixture-signing-only"),
        ("S3_REGION", "fixture"),
        ("S3_PATH_STYLE", "true"),
        ("S3_PREFIX", "proof/"),
    ] {
        env.insert(format!("CANNERY_STORAGE_{name}"), value.to_owned());
    }
    let settings = load_settings(None, &env)?;
    let lifecycle = Arc::new(profile()?);
    let uploads = Arc::new(cannery_server::upload_routes::UploadContext {
        cancellation: cannery_server::upload_routes::CancellationTasks::new(),
        settlement_timeout: std::time::Duration::from_secs(10),
        repository: cannery_attempts::model::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
        store: Arc::new(cannery_storage::ObjectStore::S3(
            cannery_storage::s3::S3Store::from_settings(&settings.storage).await?,
        )),
        signing_clock: std::time::SystemTime::now,
        mint: || {
            Ok(cannery_identity::secrets::new_secret("cr_upl_")?
                .plaintext()
                .clone())
        },
        nonce: || Ok(uuid::Uuid::new_v4().simple().to_string()),
    });
    let context = Arc::new(cannery_server::job_upload_routes::JobUploadContext {
        lifecycle: lifecycle.clone(),
        uploads: uploads.clone(),
        json_max_bytes: 1024,
        validation_slots: Arc::new(tokio::sync::Semaphore::new(1)),
        validation_timeout: std::time::Duration::from_secs(10),
    });
    let (app, state) =
        application_with_job_lifecycle_and_upload_context(settings, lifecycle, context)?;
    let body = json!({"role":"binary_output","path":"scorer/binary_output/direct.bin","size_bytes":5,"sha256":"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824","media_type":"application/octet-stream"});
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/projects/matrix/jobs/00000000-0000-0000-0000-000000006006/uploads")
                .header("authorization", "Bearer cr_svc_track_http_verifier")
                .header("content-type", "application/json")
                .header("x-lease-token", "fixture-held-second")
                .header("x-lease-generation", "9")
                .body(Body::from(serde_json::to_vec(&body)?))?,
        )
        .await?;
    let status = response.status();
    let grant: Value = serde_json::from_slice(&to_bytes(response.into_body(), 20000).await?)?;
    assert_eq!(status, 201, "{grant}");
    let signed = reqwest::Url::parse(
        grant["direct"]["request"]["url"]
            .as_str()
            .ok_or("signed URL")?,
    )?;
    assert_eq!(signed.host_str(), Some("s3.example.org"));
    assert!(
        signed
            .query_pairs()
            .any(|(key, value)| key == "X-Amz-Signature" && value.len() == 64)
    );
    assert_eq!(grant["direct"]["request"]["method"], "PUT");
    let capability = reqwest::Url::parse(grant["upload_url"].as_str().ok_or("capability URL")?)?;
    let token = grant["headers"]["X-Upload-Token"]
        .as_str()
        .ok_or("capability token")?;
    for (phase, expected) in [("presign", 200), ("finish", 201), ("finish", 409)] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("{}/{phase}", capability.path()))
                    .header("x-upload-token", token)
                    .body(Body::empty())?,
            )
            .await?;
        let status = response.status().as_u16();
        let value: Value = serde_json::from_slice(&to_bytes(response.into_body(), 20000).await?)?;
        assert_eq!(status, expected, "{value}");
    }
    {
        let wire = wire_state.lock().map_err(|_| "wire state")?;
        assert!(
            wire.calls
                .iter()
                .any(|call| call["method"] == "HEAD" && call["checksum_mode"] == "ENABLED")
        );
    }
    let generation:String=sqlx::query_scalar("SELECT generation FROM artifacts WHERE job_id='00000000-0000-0000-0000-000000006006' AND backend='s3'").fetch_one(&state.pool).await?;
    assert_eq!(generation, "\"wire-generation\"");
    uploads.cancellation.drain().await?;
    state.pool.close().await;
    Ok(())
}
