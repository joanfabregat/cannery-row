//! Source intent: stored-object checks and connection-free streaming in `test_read_side.py`.
#[path = "support/lifecycle_support.rs"]
#[allow(dead_code, reason = "reuse the reviewed public lifecycle bootstrap")]
mod lifecycle;
#[path = "support/runner_security_semantics.rs"]
#[allow(
    dead_code,
    reason = "reuse fixture registration without executing a runner"
)]
mod runner;

use conformance::Result;
use lifecycle::{Call, JOB, sha, string};
use reqwest::Method;
use serde_json::{Value, json};
use std::{fs, path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpSocket,
};

const ARTIFACT: &str = "/api/projects/{slug}/artifacts/{artifact_id}";

async fn prepared(label: &str, bytes: &[u8]) -> Result<(runner::Rig, Value, Value)> {
    let mut science = runner::science()?;
    science["limits"]["max_output_bytes"] = json!(bytes.len() + 1024 * 1024);
    let mut rig = runner::Rig::new(label, runner::producer()?, science).await?;
    rig.submit().await?;
    let lease = rig.world.claim_job(&rig.actors, false, false).await?;
    let grant = rig.world.api(
        Call::post(
            &format!("{JOB}/uploads"),
            format!("{}/uploads", rig.world.job_path(&lease)?),
            &rig.actors.tester,
            json!({"role":"step_log","path":"fixture-scorer/step_log/integrity.log", "size_bytes":bytes.len(),"sha256":sha(bytes),"media_type":"text/plain"}),
            201,
        ).lease(&lease)?,
    ).await?.body;
    let artifact = rig.world.put(&grant, bytes, true).await?;
    Ok((rig, grant, artifact))
}

fn object_path(grant: &Value) -> Result<PathBuf> {
    let root = PathBuf::from(std::env::var("CANNERY_CONFORMANCE_WORK_DIR")?).canonicalize()?;
    let bucket = string(&grant["storage"]["bucket"])?;
    let key = string(&grant["storage"]["key"])?;
    let object = root.join(bucket).join(key).canonicalize()?;
    if !object.starts_with(&root) || !object.is_file() {
        return Err("artifact fixture object lies outside its owned local store".into());
    }
    Ok(object)
}

#[tokio::test]
#[ignore = "requires the isolated local conformance store"]
async fn missing_and_changed_objects_refuse_bytes_but_matching_etags_short_circuit() -> Result<()> {
    let bytes = b"original fixture log\n";
    let (mut rig, grant, artifact) = prepared("artifact-integrity", bytes).await?;
    let path = format!(
        "{}/artifacts/{}",
        rig.world.base(),
        string(&artifact["id"])?
    );
    let first = rig
        .world
        .api(Call::get(ARTIFACT, path.clone(), &rig.actors.admin))
        .await?;
    assert_eq!(first.raw_body, bytes);
    let etag = first.headers["etag"].to_str()?.to_owned();
    let object = object_path(&grant)?;
    fs::write(&object, b"original fixture LOG\n")?; // Same length, different digest.
    for cached in [false, true] {
        let mut request = rig
            .world
            .h
            .request(Method::GET, &path)?
            .bearer_auth(&rig.actors.admin);
        if cached {
            request = request.header("If-None-Match", &etag);
        }
        let status = if cached { 304 } else { 404 };
        let response = rig
            .world
            .h
            .check_response(Method::GET, ARTIFACT, request.send().await?, status)
            .await?;
        if cached {
            assert_eq!(response.raw_body, [] as [u8; 0]);
        } else {
            assert_eq!(response.body["error"]["code"], "not_found");
        }
    }
    fs::remove_file(&object)?;
    let mut missing = Call::get(ARTIFACT, path, &rig.actors.admin);
    missing.status = 404;
    assert_eq!(
        rig.world.api(missing).await?.body["error"]["code"],
        "not_found"
    );
    rig.finish("artifact-integrity").await
}

#[tokio::test]
#[ignore = "requires --database-pool-size 1 and the isolated local store"]
async fn a_backpressured_download_releases_the_single_database_connection() -> Result<()> {
    if std::env::var("CANNERY_CONFORMANCE_DATABASE_POOL_SIZE")? != "1" {
        return Err("streaming scenario requires --database-pool-size 1".into());
    }
    // Larger than loopback buffers; the tiny advertised receive window keeps
    // the server streaming while the unrelated request needs its sole DB slot.
    let bytes = vec![b'x'; 20 * 1024 * 1024];
    let (mut rig, _, artifact) = prepared("artifact-streaming", &bytes).await?;
    let url = reqwest::Url::parse(&rig.world.base_url)?;
    if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") {
        return Err("raw streaming fixture requires the loopback HTTP profile".into());
    }
    let address = format!(
        "127.0.0.1:{}",
        url.port_or_known_default().ok_or("missing API port")?
    );
    let socket = TcpSocket::new_v4()?;
    socket.set_recv_buffer_size(4096)?;
    let mut stream = socket.connect(address.parse()?).await?;
    let path = format!(
        "{}/artifacts/{}",
        rig.world.base(),
        string(&artifact["id"])?
    );
    stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n", rig.actors.admin).as_bytes()).await?;
    let mut head = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !head.ends_with(b"\r\n\r\n") {
            if head.len() >= 16_384 {
                return Err("download response headers exceed fixture bound".into());
            }
            head.push(stream.read_u8().await?);
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await??;
    let head = String::from_utf8(head)?;
    assert!(head.starts_with("HTTP/1.1 200 "));
    assert!(
        head.to_ascii_lowercase()
            .contains(&format!("content-length: {}\r\n", bytes.len()))
    );
    // Read no body. Holding this socket applies genuine transport backpressure.
    let reports = tokio::time::timeout(
        Duration::from_secs(5),
        rig.world.api(Call::get(
            "/api/projects/{slug}/reports",
            format!("{}/reports", rig.world.base()),
            &rig.actors.admin,
        )),
    )
    .await??;
    assert_eq!(
        reports.body["items"]
            .as_array()
            .ok_or("reports items missing")?
            .len(),
        1
    );
    drop(stream);
    // The socket check does not fabricate operation coverage; a complete normal
    // download below goes through the same frozen response checker as other tests.
    let completed = rig
        .world
        .api(Call::get(ARTIFACT, path, &rig.actors.admin))
        .await?;
    assert_eq!(completed.raw_body, bytes);
    rig.finish("artifact-streaming").await
}
