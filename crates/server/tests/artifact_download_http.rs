//! Independent download replay: actual files, PostgreSQL and official SDK HEAD/signing.
#![forbid(unsafe_code)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use cannery_core::settings::load_settings;
use cannery_server::{
    application_with_download_context,
    artifact_download::{DownloadContext, DownloadStore, StoreFuture},
};
use cannery_storage::{Error as StoreError, ObjectHead, ObjectReader, ObjectStore, s3::S3Store};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::{
    collections::BTreeMap,
    error::Error,
    fmt::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::{Duration, SystemTime},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
struct LocalFiles(PathBuf);
impl Drop for LocalFiles {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Task<T>(tokio::task::JoinHandle<T>);
impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
#[test]
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "Deliberately unwind to prove fixture resources survive failed assertions"
)]
fn local_fixture_cleanup_survives_setup_failure_and_unwind() {
    let root = std::env::temp_dir().join(format!("artifact-cleanup-{}", uuid::Uuid::new_v4()));
    let failed_setup = || -> Result<()> {
        let _files = LocalFiles(root.clone());
        std::fs::create_dir(&root)?;
        Err("injected setup failure".into())
    };
    assert!(failed_setup().is_err());
    assert!(!root.exists());
    let panic = std::panic::catch_unwind(|| {
        let _files = LocalFiles(root.clone());
        std::fs::create_dir(&root).expect("fixture directory");
        panic!("injected fixture assertion failure");
    });
    assert!(panic.is_err());
    assert!(!root.exists());
}
#[tokio::test]
async fn fixture_task_cleanup_aborts_on_drop() -> Result<()> {
    struct Finished(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for Finished {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }
    let (started, ready) = tokio::sync::oneshot::channel();
    let (finished, stopped) = tokio::sync::oneshot::channel();
    let task = Task(tokio::spawn(async move {
        let _finished = Finished(Some(finished));
        let _ = started.send(());
        std::future::pending::<()>().await;
    }));
    ready.await?;
    drop(task);
    tokio::time::timeout(Duration::from_secs(2), stopped).await??;
    Ok(())
}
fn fixture() -> Result<Value> {
    Ok(serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/artifact_download_http/reference.json"
    ))?)
}
fn hex(bytes: &[u8]) -> Result<String> {
    let mut out = String::new();
    for byte in bytes {
        write!(&mut out, "{byte:02x}")?;
    }
    Ok(out)
}
fn signing_clock() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}
struct ReleasedStore {
    store: ObjectStore,
    pool: OnceLock<PgPool>,
    calls: Mutex<[u64; 2]>,
    root: PathBuf,
    mode: AtomicU8,
    started: tokio::sync::Notify,
    resume: tokio::sync::Notify,
    cancelled: AtomicBool,
}
struct ReadGuard<'a>(&'a AtomicBool);
impl Drop for ReadGuard<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
impl ReleasedStore {
    async fn released(&self, index: usize) -> std::result::Result<(), StoreError> {
        let pool = self.pool.get().ok_or(StoreError::Worker)?;
        sqlx::query("SELECT 1")
            .execute(pool)
            .await
            .map_err(|_| StoreError::Worker)?;
        self.calls.lock().map_err(|_| StoreError::Worker)?[index] += 1;
        Ok(())
    }
}
impl DownloadStore for ReleasedStore {
    fn backend(&self) -> &str {
        self.store.backend()
    }
    fn bucket(&self) -> &str {
        self.store.bucket()
    }
    fn head<'a>(&'a self, key: &'a str) -> StoreFuture<'a, Option<ObjectHead>> {
        Box::pin(async move {
            self.released(0).await?;
            let result = self.store.head(key).await?;
            if self.mode.load(Ordering::Acquire) == 2 {
                std::fs::remove_file(self.root.join("fixture").join(key))?;
            }
            Ok(result)
        })
    }
    fn read<'a>(&'a self, key: &'a str) -> StoreFuture<'a, ObjectReader> {
        Box::pin(async move {
            self.released(1).await?;
            let _guard = ReadGuard(&self.cancelled);
            if self.mode.load(Ordering::Acquire) == 1 {
                self.started.notify_one();
                self.resume.notified().await;
            }
            self.store.read(key).await
        })
    }
    fn presigning(&self) -> Option<&S3Store> {
        self.store.presigning()
    }
}
fn projection(value: Value) -> Value {
    if value
        .get("error")
        .is_some_and(|v| v["code"] == "validation_failed")
    {
        let e = &value["error"];
        return json!({"error":{"code":e["code"],"details":e["details"].as_array().map(|v|v.iter().map(|v|json!({"path":v["path"]})).collect::<Vec<_>>())}});
    }
    value
}
async fn storage(pool: &PgPool) -> Result<Value> {
    let mut value = json!({});
    for (table, order) in [
        ("tracks", "id"),
        ("hypotheses", "id"),
        ("hypothesis_revisions", "hypothesis_id,revision"),
        ("attempts", "id"),
        ("artifacts", "id"),
        ("attempt_failures", "id"),
        ("phase_outputs", "id"),
        ("manifests", "id"),
        ("uploads", "id"),
        ("jobs", "id"),
        ("review_cases", "id"),
        ("decisions", "id"),
        ("audit_events", "seq"),
        ("idempotency_keys", "scope,actor,key"),
        ("search_documents", "id"),
    ] {
        value[table] = json!(
            sqlx::query_scalar::<_, String>(&format!(
                "SELECT row_to_json(t)::text FROM {table} t ORDER BY {order}"
            ))
            .fetch_all(pool)
            .await?
        );
    }
    Ok(value)
}
async fn wire_server(
    calls: Arc<Mutex<Vec<Value>>>,
) -> Result<(String, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let calls = calls.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buf = [0; 1024];
                loop {
                    let Ok(n) = stream.read(&mut buf).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if bytes.windows(4).any(|v| v == b"\r\n\r\n") {
                        break;
                    }
                    if bytes.len() > 32768 {
                        return;
                    }
                }
                let text = String::from_utf8_lossy(&bytes);
                let mut first = text.lines().next().unwrap_or("").split_whitespace();
                let method = first.next().unwrap_or("");
                let path = first.next().unwrap_or("");
                let checksum = text.lines().find_map(|v| {
                    v.split_once(':').and_then(|(k, v)| {
                        k.eq_ignore_ascii_case("x-amz-checksum-mode")
                            .then(|| v.trim())
                    })
                });
                if let Ok(mut calls) = calls.lock() {
                    calls.push(json!({"method":method,"path":path,"checksum":checksum}));
                }
                let status = if path.contains("missing") {
                    404
                } else if path.contains("error") {
                    500
                } else {
                    200
                };
                let size = if path.contains("wrong-size") { 6 } else { 5 };
                let response = format!(
                    "HTTP/1.1 {status} fixture\r\nContent-Length: {size}\r\nETag: \"wire-generation\"\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    Ok((format!("http://{address}"), task))
}
async fn make_app(
    url: &str,
    root: &Path,
    recipe: &Value,
    endpoint: &str,
) -> Result<(Router, cannery_server::AppState, Arc<ReleasedStore>)> {
    let mut env = BTreeMap::from([
        ("CANNERY_DATABASE_URL".into(), url.into()),
        ("CANNERY_DATABASE_POOL_MAX_SIZE".into(), "1".into()),
    ]);
    let store = if recipe["backend"] == "s3" {
        for (k, v) in [
            ("BACKEND", "s3"),
            ("BUCKET", "fixture"),
            ("S3_ENDPOINT", endpoint),
            ("S3_PUBLIC_ENDPOINT", "https://s3.example.org"),
            ("S3_ACCESS_KEY_ID", "fixture-key"),
            ("S3_SECRET_ACCESS_KEY", "fixture-signing-only"),
            ("S3_REGION", "fixture"),
            ("S3_PATH_STYLE", "true"),
            ("S3_PREFIX", "proof/"),
        ] {
            env.insert(format!("CANNERY_STORAGE_{k}"), v.into());
        }
        let settings = load_settings(None, &env)?;
        let mut store = cannery_storage::create_store(&settings.storage).await?;
        if let ObjectStore::S3(store) = &mut store {
            store.presign_ttl = recipe["ttl"].as_i64().ok_or("ttl")?.into();
        }
        store
    } else {
        ObjectStore::Local(cannery_storage::local::LocalStore::new(root, "fixture")?)
    };
    let settings = load_settings(None, &env)?;
    let store = Arc::new(ReleasedStore {
        store,
        pool: OnceLock::new(),
        calls: Mutex::new([0, 0]),
        root: root.to_owned(),
        mode: AtomicU8::new(0),
        started: tokio::sync::Notify::new(),
        resume: tokio::sync::Notify::new(),
        cancelled: AtomicBool::new(false),
    });
    let context = DownloadContext {
        store: store.clone(),
        signing_clock,
        repository: cannery_attempts::model::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
    };
    let (app, state) = application_with_download_context(settings, Arc::new(context))?;
    store
        .pool
        .set(state.pool.clone())
        .map_err(|_| "pool already set")?;
    Ok((app, state, store))
}
async fn call(app: &Router, r: &Value) -> Result<(u16, Value, Vec<u8>)> {
    let mut request = Request::builder()
        .method(r["method"].as_str().ok_or("method")?)
        .uri(r["path"].as_str().ok_or("path")?);
    let role = r["role"].as_str().ok_or("role")?;
    if role != "none" {
        request = request.header(
            "authorization",
            format!(
                "Bearer {}track_http_{role}",
                if role.contains("agent") {
                    "cr_svc_"
                } else {
                    "cr_pat_"
                }
            ),
        );
    }
    if let Some(values) = r["conditionals"].as_array() {
        for value in values {
            request = request.header("if-none-match", value.as_str().ok_or("header")?);
        }
    }
    let response = app.clone().oneshot(request.body(Body::empty())?).await?;
    let status = response.status().as_u16();
    let mut headers = json!({});
    for key in [
        "content-type",
        "content-length",
        "content-disposition",
        "etag",
        "cache-control",
        "x-content-type-options",
        "content-security-policy",
        "location",
        "allow",
        "www-authenticate",
        "retry-after",
    ] {
        if let Some(value) = response.headers().get(key) {
            headers[key] = json!(value.to_str()?);
        }
    }
    let bytes = to_bytes(response.into_body(), 8 << 20).await?.to_vec();
    Ok((status, headers, bytes))
}
#[tokio::test]
#[ignore = "requires positive exact selection and a guarded fresh migrated child"]
#[allow(
    clippy::too_many_lines,
    reason = "Keep full download wire, store calls and raw storage checks together"
)]
async fn artifact_download_matches_production() -> Result<()> {
    let url = std::env::var("CANNERY_ARTIFACT_DOWNLOAD_HTTP_DATABASE_URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(
            cannery_core::db::DatabaseOptions::parse(&url)?
                .connect_options()
                .clone(),
        )
        .await?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("owned nonce child required")?;
    if suffix.len() != 24 || !suffix.bytes().all(|v| v.is_ascii_hexdigit()) {
        return Err("owned nonce child required".into());
    }
    sqlx::raw_sql(include_str!("fixtures/artifact_download_http/seed.sql"))
        .execute(&pool)
        .await?;
    let root = std::env::temp_dir().join(format!("artifact-files-{}", uuid::Uuid::new_v4()));
    let files = LocalFiles(root.clone());
    let bucket = local_files(&root)?;
    let wire_calls = Arc::new(Mutex::new(Vec::new()));
    let (endpoint, wire_task) = wire_server(wire_calls.clone()).await?;
    let mut wire_task = Task(wire_task);
    let f = fixture()?;
    let result = async {
        for r in f["cases"].as_array().ok_or("recipes")? {
            if let Some(setup) = r["setup"].as_str() {
                sqlx::raw_sql(setup).execute(&pool).await?;
            }
            let before = wire_calls.lock().map_err(|_| "wire mutex")?.len();
            let raw_before = storage(&pool).await?;
            let (app, state, store) = make_app(&url, &root, r, &endpoint).await?;
            let (status, mut headers, bytes) = call(&app, r).await?;
            if r["native_expiry_refusal"] == true {
                assert!(matches!(r["name"].as_str(), Some("s3-ttl-0" | "s3-ttl--1")));
                assert_eq!(r["backend"], "s3");
                assert!(matches!(r["ttl"].as_i64(), Some(0 | -1)));
                assert_eq!(r["status"], 302, "retain source redirect observation");
                assert!(
                    r["headers"]["location"]
                        .as_str()
                        .is_some_and(|v| v.starts_with("https://s3.example.org/"))
                );
                assert_eq!(status, 500);
                assert_eq!(bytes, b"Internal Server Error");
                assert_eq!(
                    headers,
                    json!({"content-type":"text/plain; charset=utf-8","content-length":"21"})
                );
                assert_eq!(*store.calls.lock().map_err(|_| "calls")?, [1, 0]);
                assert_eq!(
                    json!(&wire_calls.lock().map_err(|_| "wire")?[before..]),
                    r["s3_calls"],
                    "only the source-equivalent object HEAD precedes the signing refusal"
                );
                assert_eq!(
                    storage(&pool).await?,
                    raw_before,
                    "all raw storage unchanged"
                );
                assert_eq!(
                    raw_before,
                    f["snapshots"][r["storage"].as_str().ok_or("snapshot")?],
                    "source redirect storage remains equivalent"
                );
                state.pool.close().await;
                continue;
            }
            assert_eq!(r["native_expiry_refusal"], false);
            assert_eq!(json!(status), r["status"], "status {}", r["name"]);
            let response = if r["response"].is_null() {
                Value::Null
            } else {
                projection(serde_json::from_slice(&bytes)?)
            };
            assert_eq!(response, r["response"], "response {}", r["name"]);
            if r["response"]["error"]["code"] == "validation_failed" {
                headers
                    .as_object_mut()
                    .ok_or("headers")?
                    .remove("content-length");
                let mut expected = r["headers"].clone();
                expected
                    .as_object_mut()
                    .ok_or("expected headers")?
                    .remove("content-length");
                assert_eq!(headers, expected, "headers {}", r["name"]);
            } else {
                assert_eq!(headers, r["headers"], "headers {}", r["name"]);
                assert_eq!(json!(bytes.len()), r["body_size"], "length {}", r["name"]);
            }
            if let Some(expected) = r["wire_hex"].as_str() {
                assert_eq!(hex(&bytes)?, expected, "wire {}", r["name"]);
            }
            if let Some(expected) = r["body_sha256"].as_str() {
                assert_eq!(
                    hex(&Sha256::digest(&bytes))?,
                    expected,
                    "all bytes {}",
                    r["name"]
                );
            }
            let calls = *store.calls.lock().map_err(|_| "calls")?;
            let release = if r["backend"] == "s3" {
                json!({"head":0,"read":0})
            } else {
                json!({"head":calls[0],"read":calls[1]})
            };
            assert_eq!(release, r["release"], "release {}", r["name"]);
            assert_eq!(
                json!(&wire_calls.lock().map_err(|_| "wire")?[before..]),
                r["s3_calls"],
                "S3 wire {}",
                r["name"]
            );
            assert_eq!(
                storage(&pool).await?,
                f["snapshots"][r["storage"].as_str().ok_or("snapshot")?],
                "full raw storage {}",
                r["name"]
            );
            state.pool.close().await;
        }
        assert_eq!(
            f["cases"]
                .as_array()
                .ok_or("cases")?
                .iter()
                .filter(|r| r["native_expiry_refusal"] == true)
                .count(),
            2
        );
        supplemental(&pool, &url, &root, &bucket, &endpoint, &f).await?;
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    }
    .await;
    wire_task.0.abort();
    let _ = (&mut wire_task.0).await;
    pool.close().await;
    std::fs::remove_dir_all(&files.0)?;
    result
}

async fn supplemental(
    pool: &PgPool,
    url: &str,
    root: &Path,
    bucket: &Path,
    endpoint: &str,
    fixture: &Value,
) -> Result<()> {
    // Real generation strings never enter the portable raw snapshots.
    sqlx::raw_sql("ALTER TABLE artifacts DISABLE TRIGGER artifacts_immutable; UPDATE artifacts SET backend='local',key='hello.txt',size_bytes=5,generation=NULL WHERE id='00000000-0000-0000-0000-000000003001'; ALTER TABLE artifacts ENABLE TRIGGER artifacts_immutable")
        .execute(pool).await?;
    let recipe = json!({"backend":"local","ttl":900,"path":"/api/projects/matrix/artifacts/00000000-0000-0000-0000-000000003001","method":"GET","role":"researcher","conditionals":null});
    let (app, state, store) = make_app(url, root, &recipe, endpoint).await?;
    let generation = store
        .store
        .head("hello.txt")
        .await?
        .ok_or("real head")?
        .generation
        .ok_or("real generation")?;
    sqlx::raw_sql("ALTER TABLE artifacts DISABLE TRIGGER artifacts_immutable")
        .execute(pool)
        .await?;
    sqlx::query(
        "UPDATE artifacts SET generation=$1 WHERE id='00000000-0000-0000-0000-000000003001'",
    )
    .bind(generation)
    .execute(pool)
    .await?;
    sqlx::raw_sql("ALTER TABLE artifacts ENABLE TRIGGER artifacts_immutable")
        .execute(pool)
        .await?;
    let (status, _, bytes) = call(&app, &recipe).await?;
    assert_eq!(status, 200);
    assert_eq!(bytes, b"hello");
    std::fs::write(bucket.join("replacement"), b"hello")?;
    std::fs::rename(bucket.join("replacement"), bucket.join("hello.txt"))?;
    let (status, _, bytes) = call(&app, &recipe).await?;
    assert_eq!(status, 404);
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes)?["error"]["message"],
        "the stored object is no longer the verified artifact"
    );
    sqlx::raw_sql("ALTER TABLE artifacts DISABLE TRIGGER artifacts_immutable; UPDATE artifacts SET generation=NULL WHERE id='00000000-0000-0000-0000-000000003001'; ALTER TABLE artifacts ENABLE TRIGGER artifacts_immutable")
        .execute(pool).await?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let mut server = Task(tokio::spawn(
        async move { axum::serve(listener, app).await },
    ));
    let result = transfer_checks(&state.pool, &store, address).await;
    server.0.abort();
    let _ = (&mut server.0).await;
    state.pool.close().await;
    result?;
    assert_eq!(
        fixture["generation_proof"],
        json!({"actual_match":true,"same_size_replacement_rejected":true})
    );
    assert_eq!(
        fixture["transfer_proof"],
        json!({"pool_available_while_read_blocked":true,"disconnect_cancels_blocked_read":true,"missing_after_head_starts_200_then_transfer_fails":true})
    );
    Ok(())
}
async fn transfer_checks(
    pool: &PgPool,
    store: &ReleasedStore,
    address: std::net::SocketAddr,
) -> Result<()> {
    let client = reqwest::Client::new();
    let uri = format!(
        "http://{address}/api/projects/matrix/artifacts/00000000-0000-0000-0000-000000003001"
    );
    store.cancelled.store(false, Ordering::Release);
    store.mode.store(1, Ordering::Release);
    let slow = client
        .get(&uri)
        .bearer_auth("cr_pat_track_http_researcher")
        .send()
        .await?;
    assert_eq!(slow.status(), 200);
    tokio::time::timeout(Duration::from_secs(2), store.started.notified()).await?;
    sqlx::query("SELECT 1").execute(pool).await?;
    drop(slow);
    tokio::time::timeout(Duration::from_secs(2), async {
        while !store.cancelled.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    store.mode.store(2, Ordering::Release);
    // The object vanishes after the head check, so the streamed body fails.
    // Whether the 200 headers reach the client before the connection closes
    // depends on scheduling; either way the client must not get the bytes.
    match client
        .get(&uri)
        .bearer_auth("cr_pat_track_http_researcher")
        .send()
        .await
    {
        Ok(lazy) => {
            assert_eq!(lazy.status(), 200);
            assert!(lazy.bytes().await.is_err());
        }
        Err(error) => assert!(error.is_request(), "{error}"),
    }
    Ok(())
}
fn local_files(root: &Path) -> Result<PathBuf> {
    let bucket = root.join("fixture");
    std::fs::create_dir_all(&bucket)?;
    for key in [
        "hello.txt",
        "hello-2.txt",
        "hello-3.txt",
        "other.txt",
        "file-parent",
        "nest/..unicode-é ;\".txt",
    ] {
        let path = bucket.join(key);
        std::fs::create_dir_all(path.parent().ok_or("parent")?)?;
        std::fs::write(path, b"hello")?;
    }
    std::fs::write(bucket.join("empty"), b"")?;
    std::fs::write(bucket.join("large"), vec![b'x'; (2 << 20) + 7])?;
    std::fs::create_dir(bucket.join("directory"))?;
    Ok(bucket)
}
