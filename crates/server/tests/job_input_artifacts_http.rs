//! The holder of a claimed job downloads that job's input artifacts, and only
//! those, for as long as its lease runs.
#![forbid(unsafe_code)]
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use cannery_core::settings::load_settings;
use serde_json::Value;
use std::{collections::BTreeMap, error::Error};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const JOB: &str = "00000000-0000-0000-0000-000000006006";
/// An object of the verify job's input manifest.
const INPUT: &str = "00000000-0000-0000-0000-000000009001";
/// An artifact of the same attempt that the manifest does not list.
const OTHER: &str = "00000000-0000-0000-0000-000000009002";
/// An artifact of another attempt.
const FOREIGN: &str = "00000000-0000-0000-0000-000000009003";
const BYTES: &[u8] = b"fixture\x00bytes\n";

struct Objects(std::path::PathBuf);
impl Drop for Objects {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The verifier service account, with the job's lease headers when `leased`.
async fn get(app: &Router, path: &str, leased: bool) -> Result<(u16, Vec<u8>)> {
    let mut request = Request::builder()
        .uri(path)
        .header("authorization", "Bearer cr_svc_track_http_verifier");
    if leased {
        request = request
            .header("X-Lease-Token", "fixture-input-held-verifier")
            .header("X-Lease-Generation", "9");
    }
    let response = app.clone().oneshot(request.body(Body::empty())?).await?;
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await?;
    Ok((status, bytes.to_vec()))
}

fn artifact(id: &str) -> String {
    format!("/api/projects/matrix/artifacts/{id}")
}

#[tokio::test]
#[ignore = "requires guarded uniquely owned migrated PostgreSQL"]
async fn job_holder_downloads_only_its_inputs_while_leased() -> Result<()> {
    let root = Objects(
        std::env::temp_dir().join(format!("cr-job-input-artifacts-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir_all(root.0.join("local/inputs"))?;
    for name in ["data", "other", "foreign"] {
        std::fs::write(root.0.join("local/inputs").join(name), BYTES)?;
    }
    let settings = load_settings(
        None,
        &BTreeMap::from([
            (
                "CANNERY_DATABASE_URL".into(),
                std::env::var("CANNERY_JOB_INPUT_ARTIFACTS_HTTP_DATABASE_URL")?,
            ),
            (
                "CANNERY_STORAGE_LOCAL_ROOT".into(),
                root.0.display().to_string(),
            ),
            ("CANNERY_STORAGE_BUCKET".into(), "local".into()),
            (
                "CANNERY_SERVER_PUBLIC_BASE_URL".into(),
                "https://cannery.example.org".into(),
            ),
        ]),
    )?;
    let (app, state, uploads) =
        cannery_server::production_application(settings, "127.0.0.1").await?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("owned child required")?;
    assert!(suffix.len() == 24 && suffix.bytes().all(|b| b.is_ascii_hexdigit()));
    for seed in [
        include_str!("fixtures/attempt_reads_http/seed.sql"),
        include_str!("fixtures/job_reads_http/seed.sql"),
        include_str!("fixtures/job_inputs_http/seed.sql"),
    ] {
        sqlx::raw_sql(seed).execute(&state.pool).await?;
    }
    // The job's manifest (7001, attempt 2008) lists inputs/data.
    let sha = "2e3b5cf9f0ecafdc0e19c1d505b9f9765eef004b043c3dcf1c9e0ffcd5e56394";
    sqlx::raw_sql(&format!(
        "INSERT INTO artifacts(id,project_id,attempt_id,role,backend,bucket,key,size_bytes,sha256,media_type) VALUES
         ('{INPUT}','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000002008','data','local','local','inputs/data',14,'{sha}','application/octet-stream'),
         ('{OTHER}','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000002008','notes','local','local','inputs/other',14,'{sha}','application/octet-stream'),
         ('{FOREIGN}','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000002007','data','local','local','inputs/foreign',14,'{sha}','application/octet-stream')"
    ))
    .execute(&state.pool)
    .await?;

    // The job lists its input artifacts with a download URL each.
    let listing = format!("/api/projects/matrix/jobs/{JOB}/inputs/artifacts");
    let (status, body) = get(&app, &listing, true).await?;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let listed: Value = serde_json::from_slice(&body)?;
    let items = listed["items"].as_array().ok_or("items")?;
    assert_eq!(items.len(), 1, "{listed}");
    assert_eq!(items[0]["artifact_id"], INPUT);
    assert_eq!(items[0]["role"], "data");
    assert_eq!(items[0]["tool"], "get_artifact");
    assert_eq!(
        items[0]["download_url"],
        format!("https://cannery.example.org{}", artifact(INPUT))
    );
    // Without the lease, the listing is refused.
    assert_eq!(get(&app, &listing, false).await?.0, 409);

    // The holder downloads its input.
    let (status, bytes) = get(&app, &artifact(INPUT), false).await?;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(bytes, BYTES);

    // Nothing else: an artifact of the same attempt outside the manifest,
    // or one of another attempt.
    for id in [OTHER, FOREIGN] {
        let (status, body) = get(&app, &artifact(id), false).await?;
        assert_eq!(status, 403, "{id}: {}", String::from_utf8_lossy(&body));
    }

    // Once the lease has run out, not its inputs either.
    sqlx::query(&format!(
        "UPDATE jobs SET lease_expires_at = now() - interval '1 second' WHERE id = '{JOB}'"
    ))
    .execute(&state.pool)
    .await?;
    let (status, body) = get(&app, &artifact(INPUT), false).await?;
    assert_eq!(status, 403, "{}", String::from_utf8_lossy(&body));
    assert_eq!(get(&app, &listing, true).await?.0, 409);

    uploads.drain().await?;
    state.pool.close().await;
    Ok(())
}
