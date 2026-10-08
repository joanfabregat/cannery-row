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
use std::{collections::BTreeMap, sync::Arc};
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
        repr_budget: 80,
        response: cannery_server::job_read_wire::ResponseContext {
            inferred_nesting_budget: 80,
            representation_budget: 80,
        },
    })
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
        .header("authorization", format!("Bearer cr_svc_track_http_{role}"))
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
        ("tester", "wrong", "9", 409),
        ("tester", "fixture-held", "8", 409),
        ("evaluator", "fixture-held", "9", 403),
        ("agent", "fixture-held", "9", 403),
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
            "tester",
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
        "tester",
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
        "tester",
        "fixture-held",
        "9",
        &failure,
        None,
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    let states:Vec<(String,)> =sqlx::query_as("SELECT state FROM jobs WHERE attempt_id='00000000-0000-0000-0000-000000002005' ORDER BY run_number").fetch_all(&state.pool).await?;
    assert_eq!(states, vec![("failed".into(),)]);
    let attempt: String = sqlx::query_scalar(
        "SELECT state FROM attempts WHERE id='00000000-0000-0000-0000-000000002005'",
    )
    .fetch_one(&state.pool)
    .await?;
    assert_eq!(attempt, "failed");
    let code:String=sqlx::query_scalar("SELECT code FROM attempt_failures WHERE attempt_id='00000000-0000-0000-0000-000000002005' ORDER BY created_at DESC LIMIT 1").fetch_one(&state.pool).await?;
    assert_eq!(code, "no_evaluator");
    let audits:i64=sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE subject_id IN ('00000000-0000-0000-0000-000000006005','00000000-0000-0000-0000-000000002005')").fetch_one(&state.pool).await?;
    assert!(audits >= 2);
    let (status, value, _) = call(
        &app,
        "failure",
        "tester",
        "fixture-held",
        "9",
        &failure,
        None,
    )
    .await?;
    assert_eq!(status, 409, "{value}");
    publication_and_upload(&app, &state.pool, &uploads, &root.0, &job_uploads).await?;
    rerun_budget(&app, &state.pool).await?;
    evaluator_publication_race(&app, &state.pool).await?;
    multi_record_evaluator_publication(&app, &state.pool).await?;
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
                .header("authorization", "Bearer cr_svc_track_http_tester")
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
    let evidence = json!({"schema_version":"0.2","attempt_id":"00000000-0000-0000-0000-000000002006","stage":"tester","status":"completed","producer":{"kind":"service","id":"fixture-tester"},"started_at":"2026-10-01T00:00:00Z","finished_at":"2026-10-01T00:01:00Z","provenance":{"source_revision":"source-1","science_revision":"3","tester_revision":"tester-1","dataset_revision":"data-1"},"observations":"bounded tester output","measurements":[],"artifact_roles":[]});
    let object = json!({"role":artifact["role"],"storage":artifact["storage"],"size_bytes":artifact["size_bytes"],"sha256":artifact["sha256"],"media_type":artifact["media_type"]});
    let completion = json!({"schema_version":"0.2","job_id":"00000000-0000-0000-0000-000000006006","evidence":evidence,"manifest":{"schema_version":"0.2","attempt_id":"00000000-0000-0000-0000-000000002006","objects":[object]}});
    let (status, value, replay) = call(
        app,
        "completion",
        "tester",
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
        "tester",
        "wrong",
        "1",
        &completion,
        Some("complete-once"),
    )
    .await?;
    assert_eq!(status, 200, "{value}");
    assert!(replay);
    let mut conflict = completion.clone();
    conflict["evidence"]["observations"] = json!("different output");
    let (status, value, _) = call(
        app,
        "completion",
        "tester",
        "fixture-held-second",
        "9",
        &conflict,
        Some("complete-once"),
    )
    .await?;
    assert_eq!(status, 409, "{value}");
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM phase_outputs WHERE attempt_id='00000000-0000-0000-0000-000000002006' AND stage='tester'),(SELECT count(*) FROM manifests WHERE attempt_id='00000000-0000-0000-0000-000000002006' AND stage='tester'),(SELECT count(*) FROM idempotency_keys WHERE scope='job.complete')").fetch_one(pool).await?;
    assert_eq!(counts, (1, 1, 1));
    Ok(())
}
async fn rerun_budget(app: &Router, pool: &sqlx::PgPool) -> Result<()> {
    sqlx::raw_sql("INSERT INTO config_revisions(project_id,kind,revision,content,created_by) SELECT project_id,kind,4,content||'{\"evaluator\":{\"id\":\"fixture-evaluator\",\"revision\":\"policy-1\"},\"limits\":{\"max_output_bytes\":10000}}'::jsonb,created_by FROM config_revisions WHERE project_id='00000000-0000-0000-0000-000000000010' AND kind='science' AND revision=3;
    INSERT INTO attempts(id,project_id,hypothesis_id,sequence,state,hypothesis_revision,science_revision,track_id,claimed_by_service,via_channel,lease_generation) VALUES('00000000-0000-0000-0000-000000002405','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000001005',2,'testing',2,4,'00000000-0000-0000-0000-000000000004','00000000-0000-0000-0000-000000000020','api',0);
    INSERT INTO jobs(id,project_id,attempt_id,stage,run_number,state,science_revision,tester_id,spec,deadline_seconds,claimed_by_service,via_channel,lease_generation,lease_token_hash,lease_expires_at,claimed_at,deadline) SELECT '00000000-0000-0000-0000-000000006107',project_id,'00000000-0000-0000-0000-000000002405','tester',1,'claimed',4,tester_id,spec,600,'00000000-0000-0000-0000-000000000023','api',1,sha256(convert_to('rerun-initial','UTF8')),now()+interval '1 hour',now(),now()+interval '2 hours' FROM jobs WHERE id='00000000-0000-0000-0000-000000006006';").execute(pool).await?;
    let mut report = json!({"schema_version":"0.2","job_id":"00000000-0000-0000-0000-000000006107","error_code":"runner_failed","reason":"worker crash","logs":[]});
    let (status, value, _) = call(
        app,
        "completion",
        "tester",
        "rerun-initial",
        "1",
        &report,
        None,
    )
    .await?;
    assert_eq!(
        status, 422,
        "invalid completion must durably fail and rerun: {value}"
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
    assert_eq!(state, "testing");
    let prefix: String = sqlx::query_scalar("SELECT spec->>'output_prefix' FROM jobs WHERE id=$1")
        .bind(id)
        .fetch_one(pool)
        .await?;
    assert!(prefix.contains(&id.to_string()));
    sqlx::query("UPDATE jobs SET state='claimed',claimed_by_service='00000000-0000-0000-0000-000000000023',lease_generation=1,lease_token_hash=sha256(convert_to('rerun-second','UTF8')),lease_expires_at=now()+interval '1 hour',claimed_at=now(),deadline=now()+interval '2 hours' WHERE id=$1").bind(id).execute(pool).await?;
    report["job_id"] = json!(id.to_string());
    let (status, value, _) =
        call(app, "failure", "tester", "rerun-second", "1", &report, None).await?;
    assert_eq!(status, 200, "{value}");
    let states:Vec<String>=sqlx::query_scalar("SELECT state FROM jobs WHERE attempt_id='00000000-0000-0000-0000-000000002405' ORDER BY run_number").fetch_all(pool).await?;
    assert_eq!(states, vec!["failed", "failed"]);
    let (stage,code):(String,String)=sqlx::query_as("SELECT stage,code FROM attempt_failures WHERE attempt_id='00000000-0000-0000-0000-000000002405'").fetch_one(pool).await?;
    assert_eq!((stage, code), ("tester".into(), "runner_failed".into()));
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
                .header("authorization", "Bearer cr_svc_track_http_tester")
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
async fn evaluator_publication_race(app: &Router, pool: &sqlx::PgPool) -> Result<()> {
    sqlx::raw_sql("INSERT INTO attempts(id,project_id,hypothesis_id,sequence,state,hypothesis_revision,science_revision,track_id,claimed_by_service,via_channel,lease_generation) VALUES('00000000-0000-0000-0000-000000002406','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000001006',2,'evaluating',2,4,'00000000-0000-0000-0000-000000000004','00000000-0000-0000-0000-000000000020','api',0);
    INSERT INTO phase_outputs(id,project_id,attempt_id,stage,status,front_matter,sha256,producer_service,via_channel) VALUES('00000000-0000-0000-0000-000000003406','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000002406','tester','completed','{\"provenance\":{\"source_revision\":\"source-1\",\"dataset_revision\":\"data-1\"},\"measurements\":[]}',repeat('a',64),'00000000-0000-0000-0000-000000000023','api');
    INSERT INTO jobs(id,project_id,attempt_id,stage,run_number,state,science_revision,tester_id,spec,deadline_seconds,claimed_by_service,via_channel,lease_generation,lease_token_hash,lease_expires_at,claimed_at,deadline) VALUES('00000000-0000-0000-0000-000000006108','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000002406','evaluator',1,'claimed',4,'fixture-evaluator','{\"evaluator\":{\"id\":\"fixture-evaluator\",\"revision\":\"policy-1\"},\"track\":\"track-4\",\"parameters\":{},\"control\":null,\"output_prefix\":\"evaluation/\",\"inputs\":{\"evidence\":[{\"ref\":\"00000000-0000-0000-0000-000000003406\",\"sha256\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}]}}',600,'00000000-0000-0000-0000-000000000024','api',1,sha256(convert_to('evaluation-held','UTF8')),now()+interval '1 hour',now(),now()+interval '2 hours');").execute(pool).await?;
    let record = json!({"schema_version":"0.2","job_id":"00000000-0000-0000-0000-000000006108","evidence":{"schema_version":"0.2","attempt_id":"00000000-0000-0000-0000-000000002406","stage":"evaluator","status":"completed","producer":{"kind":"service","id":"fixture-evaluator"},"started_at":"2026-10-01T00:00:00Z","finished_at":"2026-10-01T00:01:00Z","provenance":{"source_revision":"source-1","science_revision":"4","dataset_revision":"data-1"},"assessment":{"policy_revision":"policy-1","gates":[{"id":"coverage","result":"unknown"}],"evidence":[{"ref":"00000000-0000-0000-0000-000000003406","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}],"verdict":"inconclusive","reason":"bounded evaluation"}}});
    let (first, second) = tokio::join!(
        call(
            app,
            "completion",
            "evaluator",
            "evaluation-held",
            "1",
            &record,
            Some("evaluation-once")
        ),
        call(
            app,
            "completion",
            "evaluator",
            "evaluation-held",
            "1",
            &record,
            Some("evaluation-once")
        )
    );
    let first = first?;
    let second = second?;
    assert_eq!(first.0, 200, "{}", first.1);
    assert_eq!(second.0, 200, "{}", second.1);
    assert_ne!(first.2, second.2);
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM phase_outputs WHERE attempt_id='00000000-0000-0000-0000-000000002406' AND stage='evaluator'),(SELECT count(*) FROM review_cases WHERE attempt_id='00000000-0000-0000-0000-000000002406' AND kind='result'),(SELECT count(*) FROM audit_events WHERE action='attempt.evaluated' AND subject_id='00000000-0000-0000-0000-000000002406')").fetch_one(pool).await?;
    assert_eq!(counts, (1, 1, 1));
    let (attempt,hypothesis):(String,String)=sqlx::query_as("SELECT a.state,h.state FROM attempts a JOIN hypotheses h ON h.id=a.hypothesis_id WHERE a.id='00000000-0000-0000-0000-000000002406'").fetch_one(pool).await?;
    assert_eq!(
        (attempt, hypothesis),
        (
            "awaiting_human_review".into(),
            "awaiting_human_review".into()
        )
    );
    let linked:bool=sqlx::query_scalar("SELECT j.evidence_id=c.evidence_id AND c.subject_revision=e.revision AND c.resolved_at IS NULL FROM jobs j JOIN phase_outputs e ON e.id=j.evidence_id JOIN review_cases c ON c.evidence_id=e.id WHERE j.id='00000000-0000-0000-0000-000000006108'").fetch_one(pool).await?;
    assert!(linked);
    Ok(())
}
async fn multi_record_evaluator_publication(app: &Router, pool: &sqlx::PgPool) -> Result<()> {
    sqlx::raw_sql(r#"
INSERT INTO config_revisions(project_id,kind,revision,content,created_by)
SELECT project_id,kind,5,content||'{"metrics":[{"key":"mrr","splits":["dev"],"dimensions":[],"unit":"ratio","direction":"higher"}]}'::jsonb,created_by FROM config_revisions WHERE project_id='00000000-0000-0000-0000-000000000010' AND kind='science' AND revision=4;
INSERT INTO hypotheses(id,project_id,number,track_id,title,created_by_user,state,revision,approved_revision,approved_at)
SELECT '00000000-0000-0000-0000-000000001416',project_id,1416,track_id,'Multi-record citation',created_by_user,'active',2,2,approved_at FROM hypotheses WHERE id='00000000-0000-0000-0000-000000001006';
INSERT INTO hypothesis_revisions(hypothesis_id,revision,content,science_revision,author_user,via_channel)
SELECT '00000000-0000-0000-0000-000000001416',2,content,5,author_user,via_channel FROM hypothesis_revisions WHERE hypothesis_id='00000000-0000-0000-0000-000000001006' AND revision=2;
INSERT INTO attempts(id,project_id,hypothesis_id,sequence,state,hypothesis_revision,science_revision,track_id,claimed_by_service,via_channel,lease_generation)
SELECT '00000000-0000-0000-0000-000000002416',project_id,'00000000-0000-0000-0000-000000001416',1,'evaluating',2,5,track_id,claimed_by_service,via_channel,0 FROM attempts WHERE id='00000000-0000-0000-0000-000000002406';
INSERT INTO phase_outputs(id,project_id,attempt_id,stage,status,front_matter,sha256,producer_service,via_channel)
SELECT '00000000-0000-0000-0000-000000003416',project_id,'00000000-0000-0000-0000-000000002416',stage,status,front_matter,sha256,producer_service,via_channel FROM phase_outputs WHERE id='00000000-0000-0000-0000-000000003406';
INSERT INTO phase_outputs(id,project_id,attempt_id,stage,status,revision,front_matter,sha256,producer_service,via_channel)
SELECT '00000000-0000-0000-0000-000000003417',project_id,attempt_id,stage,status,2,front_matter||'{"measurements":[{"metric":"mrr","split":"dev","dimensions":{},"authority":"tester_verified","value":0.42,"unit":"ratio","direction":"higher"}]}'::jsonb,repeat('b',64),producer_service,via_channel FROM phase_outputs WHERE id='00000000-0000-0000-0000-000000003416';
"#).execute(pool).await?;
    let refs = json!([
        {"ref":"00000000-0000-0000-0000-000000003416","sha256":"a".repeat(64)},
        {"ref":"00000000-0000-0000-0000-000000003417","sha256":"b".repeat(64)}
    ]);
    let mut spec: Value =
        sqlx::query_scalar("SELECT spec FROM jobs WHERE id='00000000-0000-0000-0000-000000006108'")
            .fetch_one(pool)
            .await?;
    spec["inputs"]["evidence"] = refs.clone();
    sqlx::query("INSERT INTO jobs(id,project_id,attempt_id,stage,run_number,state,science_revision,tester_id,spec,deadline_seconds,claimed_by_service,via_channel,lease_generation,lease_token_hash,lease_expires_at,claimed_at,deadline) SELECT '00000000-0000-0000-0000-000000006118',project_id,'00000000-0000-0000-0000-000000002416',stage,1,'claimed',5,tester_id,$1,600,claimed_by_service,via_channel,1,sha256(convert_to('evaluation-held','UTF8')),now()+interval '1 hour',now(),now()+interval '2 hours' FROM jobs WHERE id='00000000-0000-0000-0000-000000006108'")
        .bind(spec).execute(pool).await?;
    let record = json!({"schema_version":"0.2","job_id":"00000000-0000-0000-0000-000000006118","evidence":{"schema_version":"0.2","attempt_id":"00000000-0000-0000-0000-000000002416","stage":"evaluator","status":"completed","producer":{"kind":"service","id":"fixture-evaluator"},"started_at":"2026-10-01T00:00:00Z","finished_at":"2026-10-01T00:01:00Z","provenance":{"source_revision":"source-1","science_revision":"5","dataset_revision":"data-1"},"assessment":{"policy_revision":"policy-1","gates":[{"id":"quality","result":"pass"}],"evidence":refs,"comparisons":[{"metric":"mrr","split":"dev","dimensions":{},"source":"tester","value":0.42,"reference":{"value":0.4,"label":"baseline","kind":"baseline"}}],"verdict":"pass","reason":"second pinned record cited"}}});
    let (status, response, _) = call(
        app,
        "completion",
        "evaluator",
        "evaluation-held",
        "1",
        &record,
        Some("multi-record-once"),
    )
    .await?;
    assert_eq!(status, 200, "{response}");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM comparisons WHERE attempt_id='00000000-0000-0000-0000-000000002416' AND metric='mrr' AND value=0.42")
        .fetch_one(pool).await?;
    assert_eq!(count, 1);
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
                .header("authorization", "Bearer cr_svc_track_http_tester")
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
