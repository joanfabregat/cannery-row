//! Independent frozen production leased predecessor input, raw storage and fixed wire bytes.
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
    application_with_predecessor_input_context, predecessor_input_routes::PredecessorInputContext,
};
use serde_json::{Value, json};
use sqlx::Connection;
use std::{
    collections::BTreeMap,
    error::Error,
    fmt::Write,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
struct CountedStore {
    inner: cannery_storage::ObjectStore,
    heads: AtomicUsize,
    reads: AtomicUsize,
}
impl cannery_server::artifact_download::DownloadStore for CountedStore {
    fn backend(&self) -> &str {
        self.inner.backend()
    }
    fn bucket(&self) -> &str {
        self.inner.bucket()
    }
    fn head<'a>(
        &'a self,
        key: &'a str,
    ) -> cannery_server::artifact_download::StoreFuture<'a, Option<cannery_storage::ObjectHead>>
    {
        self.heads.fetch_add(1, Ordering::Relaxed);
        Box::pin(self.inner.head(key))
    }
    fn read<'a>(
        &'a self,
        key: &'a str,
    ) -> cannery_server::artifact_download::StoreFuture<'a, cannery_storage::ObjectReader> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Box::pin(self.inner.read(key))
    }
    fn presigning(&self) -> Option<&cannery_storage::s3::S3Store> {
        self.inner.presigning()
    }
}
fn fixture() -> Result<Value> {
    Ok(serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/predecessor_input_http/reference.json"
    ))?)
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
fn profile(
    store: Arc<dyn cannery_server::artifact_download::DownloadStore>,
) -> PredecessorInputContext {
    PredecessorInputContext {
        repository: cannery_attempts::model::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
        rendering: cannery_research::science::RenderingContext { nesting_budget: 80 },
        manifest_decode_budget: 80,
        store,
        signing_clock: || {
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000)
        },
    }
}
fn projection(v: Value) -> Value {
    match v {
        Value::Object(v)
            if v.get("error")
                .is_some_and(|e| e["code"] == "validation_failed") =>
        {
            let e = &v["error"];
            json!({"error":{"code":e["code"],"details":e["details"].as_array().map(|a|a.iter().map(|d|json!({"path":d["path"]})).collect::<Vec<_>>())}})
        }
        Value::Number(n) if n.to_string().contains(['.', 'e', 'E']) => n
            .as_f64()
            .and_then(serde_json::Number::from_f64)
            .map_or(Value::Number(n), Value::Number),
        Value::Object(v) => Value::Object(v.into_iter().map(|(k, v)| (k, projection(v))).collect()),
        Value::Array(v) => Value::Array(v.into_iter().map(projection).collect()),
        v => v,
    }
}
async fn call(app: &Router, r: &Value) -> Result<(u16, Option<String>, Value, String, Value)> {
    let mut req = Request::builder()
        .method(r["method"].as_str().ok_or("method")?)
        .uri(r["path"].as_str().ok_or("path")?);
    let role = r["role"].as_str().ok_or("role")?;
    if role != "none" && role != "malformed" {
        req = req.header(
            "authorization",
            format!(
                "Bearer {}track_http_{role}",
                if matches!(
                    role,
                    "agent"
                        | "foreign-agent"
                        | "tester"
                        | "evaluator"
                        | "experimenter"
                        | "another-experimenter"
                        | "experimenter-readonly"
                        | "experimenter-writeonly"
                        | "another-tester"
                        | "tester-readonly"
                        | "tester-writeonly"
                ) {
                    "cr_svc_"
                } else {
                    "cr_pat_"
                }
            ),
        );
    }
    if role == "malformed" {
        req = req.header("authorization", "Basic x");
    }
    if let Some(headers) = r["lease"].as_object() {
        for (name, value) in headers {
            req = req.header(name, value.as_str().ok_or("lease header")?);
        }
    } else if r["lease"].is_null() {
        req = req
            .header("X-Lease-Token", "fixture-predecessor-held")
            .header("X-Lease-Generation", "9");
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(
            r["body"].as_str().unwrap_or_default().to_owned(),
        ))?)
        .await?;
    let status = response.status().as_u16();
    let allow = response
        .headers()
        .get("allow")
        .map(|v| v.to_str().map(str::to_owned))
        .transpose()?;
    let mut headers = json!({});
    for name in [
        "content-type",
        "content-length",
        "cache-control",
        "location",
        "retry-after",
    ] {
        headers[name] = response.headers().get(name).map_or(Value::Null, |value| {
            json!(String::from_utf8_lossy(value.as_bytes()).to_string())
        });
    }
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let mut hex = String::new();
    for b in &bytes {
        write!(hex, "{b:02x}")?;
    }
    let value = if status == 500 {
        assert_eq!(&bytes[..], b"Internal Server Error");
        Value::Null
    } else if bytes.is_empty()
        || (status == 200 && r["path"].as_str().ok_or("path")?.contains("/predecessor/"))
    {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)?
    };
    Ok((status, allow, projection(value), hex, headers))
}
async fn storage(pool: &sqlx::PgPool) -> Result<Value> {
    let mut output = json!({});
    for (table, order) in [
        ("tracks", "id"),
        ("hypotheses", "id"),
        ("hypothesis_revisions", "hypothesis_id,revision"),
        ("attempts", "id"),
        ("artifacts", "id"),
        ("measurements", "id"),
        ("comparisons", "id"),
        ("phase_outputs", "id"),
        ("review_cases", "id"),
        ("decisions", "id"),
        ("audit_events", "seq"),
        ("idempotency_keys", "scope,actor,key"),
        ("search_documents", "id"),
        ("jobs", "id"),
        ("manifests", "id"),
        ("uploads", "id"),
        ("attempt_failures", "id"),
        ("experiment_manifests", "project_id,name,revision"),
    ] {
        let rows = sqlx::query_scalar::<_, String>(&format!(
            "SELECT row_to_json(t)::text FROM {table} t ORDER BY {order}"
        ))
        .fetch_all(pool)
        .await?;
        output[table] = json!(rows);
    }
    Ok(output)
}
#[tokio::test]
#[ignore = "requires guarded uniquely owned migrated PostgreSQL"]
#[allow(
    clippy::too_many_lines,
    reason = "One guarded source sequence includes storage and lazy-transfer assertions"
)]
async fn predecessor_input_match_production() -> Result<()> {
    let url = std::env::var("CANNERY_PREDECESSOR_INPUT_HTTP_DATABASE_URL")?;
    let settings = load_settings(
        None,
        &BTreeMap::from([
            ("CANNERY_DATABASE_URL".into(), url),
            ("CANNERY_DATABASE_POOL_MAX_SIZE".into(), "1".into()),
        ]),
    )?;
    let directory =
        Objects(std::env::temp_dir().join(format!("cr-job-inputs-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(directory.0.join("local/inputs"))?;
    std::fs::write(directory.0.join("local/inputs/data"), b"fixture\x00bytes\n")?;
    let store = Arc::new(CountedStore {
        inner: cannery_storage::ObjectStore::Local(cannery_storage::local::LocalStore::new(
            &directory.0,
            "local",
        )?),
        heads: AtomicUsize::new(0),
        reads: AtomicUsize::new(0),
    });
    let (app, state) = application_with_predecessor_input_context(
        settings.clone(),
        Arc::new(profile(store.clone())),
    )?;
    let wire_calls = Arc::new(Mutex::new(Vec::new()));
    let (endpoint, wire_task) = wire_server(wire_calls.clone()).await?;
    let _wire_task = Task(wire_task);
    let mut env = BTreeMap::new();
    env.insert(
        "CANNERY_DATABASE_URL".to_owned(),
        std::env::var("CANNERY_PREDECESSOR_INPUT_HTTP_DATABASE_URL")?,
    );
    for (key, value) in [
        ("BACKEND", "s3"),
        ("BUCKET", "fixture"),
        ("S3_ENDPOINT", &endpoint),
        ("S3_PUBLIC_ENDPOINT", "https://s3.example.org"),
        ("S3_ACCESS_KEY_ID", "fixture-key"),
        ("S3_SECRET_ACCESS_KEY", "fixture-signing-only"),
        ("S3_REGION", "fixture"),
        ("S3_PATH_STYLE", "true"),
        ("S3_PREFIX", "proof/"),
    ] {
        env.insert(format!("CANNERY_STORAGE_{key}"), value.to_owned());
    }
    let storage_settings = load_settings(None, &env)?.storage;
    let s3 = Arc::new(cannery_storage::create_store(&storage_settings).await?);
    let (s3_app, s3_state) =
        application_with_predecessor_input_context(settings, Arc::new(profile(s3)))?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("owned child required")?;
    if suffix.len() != 24 || !suffix.bytes().all(|v| v.is_ascii_hexdigit()) {
        return Err("owned child required".into());
    }
    sqlx::raw_sql(include_str!("fixtures/attempt_reads_http/seed.sql"))
        .execute(&state.pool)
        .await?;
    sqlx::raw_sql(include_str!("fixtures/job_reads_http/seed.sql"))
        .execute(&state.pool)
        .await?;
    sqlx::raw_sql(include_str!("fixtures/predecessor_input_http/seed.sql"))
        .execute(&state.pool)
        .await?;
    let object_request = || {
        Request::builder()
            .uri("/api/projects/matrix/hypotheses/2/attempts/1/inputs/predecessor/00000000-0000-0000-0000-000000004008")
            .header("authorization", "Bearer cr_svc_track_http_experimenter")
            .header("X-Lease-Token", "fixture-predecessor-held")
            .header("X-Lease-Generation", "9")
            .body(Body::empty())
    };
    let response = app.clone().oneshot(object_request()?).await?;
    assert_eq!(response.status(), 200);
    let options = cannery_core::db::DatabaseOptions::parse(&std::env::var(
        "CANNERY_PREDECESSOR_INPUT_HTTP_DATABASE_URL",
    )?)?;
    let mut observer = sqlx::PgConnection::connect_with(options.connect_options()).await?;
    let mut observer_tx = observer.begin().await?;
    sqlx::query(
        "SELECT id FROM attempts WHERE id='00000000-0000-0000-0000-000000002002' FOR UPDATE NOWAIT",
    )
    .fetch_one(&mut *observer_tx)
    .await?;
    observer_tx.commit().await?;
    observer.close().await?;
    assert!(
        state.pool.try_acquire().is_none(),
        "source ConnDep stays borrowed through transfer"
    );
    drop(response);
    drop(tokio::time::timeout(std::time::Duration::from_secs(3), state.pool.acquire()).await??);
    let response = app.clone().oneshot(object_request()?).await?;
    std::fs::remove_file(directory.0.join("local/inputs/data"))?;
    assert!(
        to_bytes(response.into_body(), 1024).await.is_err(),
        "local read remains lazy after HEAD"
    );
    std::fs::write(directory.0.join("local/inputs/data"), b"fixture\x00bytes\n")?;
    let f = fixture()?;
    for (i, r) in f["cases"].as_array().ok_or("cases")?.iter().enumerate() {
        if let Some(setup) = r["setup"].as_str() {
            sqlx::raw_sql(setup).execute(&state.pool).await?;
        }
        let before = wire_calls.lock().map_err(|_| "wire mutex")?.len();
        let raw_before = if r["native_error"].is_string() {
            Some(storage(&state.pool).await?)
        } else {
            None
        };
        let local_calls = (
            store.heads.load(Ordering::Relaxed),
            store.reads.load(Ordering::Relaxed),
        );
        let selected = if r["backend"] == "s3" { &s3_app } else { &app };
        let (status, allow, value, wire, headers) = call(selected, r).await?;
        if let Some(raw_before) = raw_before {
            native_refusal(&state.pool, r, status, allow.as_ref(), &wire, &headers).await?;
            assert_eq!(
                (
                    store.heads.load(Ordering::Relaxed),
                    store.reads.load(Ordering::Relaxed)
                ),
                local_calls,
                "native refusal before HEAD/read"
            );
            assert_eq!(
                wire_calls.lock().map_err(|_| "wire")?.len(),
                before,
                "native refusal before S3 calls"
            );
            assert_eq!(
                storage(&state.pool).await?,
                raw_before,
                "all raw storage unchanged"
            );
            assert_eq!(
                raw_before,
                f["snapshots"][r["storage"].as_str().ok_or("snapshot")?],
                "source storage remains equivalent"
            );
            continue;
        }
        assert!(r["native_error"].is_null());
        assert_eq!(
            json!(status),
            r["status"],
            "case{i} {} response{value}",
            r["name"]
        );
        assert_eq!(json!(allow), r["allow"], "case{i}");
        let mut expected_headers = r["headers"].clone();
        let mut headers = headers;
        if status == 422 {
            // The validation prose may differ from the original; its byte length follows it.
            headers
                .as_object_mut()
                .ok_or("headers")?
                .remove("content-length");
            expected_headers
                .as_object_mut()
                .ok_or("headers")?
                .remove("content-length");
        }
        assert_eq!(headers, expected_headers, "case{i} headers");
        assert_eq!(
            json!(&wire_calls.lock().map_err(|_| "wire mutex")?[before..]),
            r["store_calls"],
            "case{i} store calls"
        );
        assert_eq!(
            value,
            projection(r["response"].clone()),
            "case{i} {}",
            r["name"]
        );
        if status != 422 && !r["wire_hex"].is_null() {
            assert_eq!(json!(wire), r["wire_hex"], "case{i} wire");
        }
        let snapshot = r["storage"].as_str().ok_or("storage")?;
        assert_eq!(
            storage(&state.pool).await?,
            f["snapshots"][snapshot],
            "case{i} storage"
        );
    }
    let counts = f["cases"]
        .as_array()
        .ok_or("cases")?
        .iter()
        .filter_map(|r| r["native_error"].as_str())
        .fold(BTreeMap::new(), |mut counts, name| {
            *counts.entry(name).or_insert(0_usize) += 1;
            counts
        });
    assert_eq!(
        counts,
        BTreeMap::from([
            ("header-integer", 4),
            ("path-number", 3),
            ("path-sequence", 3),
            ("aggregate-parameters", 3),
            ("workflow-revision", 1)
        ])
    );
    state.pool.close().await;
    s3_state.pool.close().await;
    Ok(())
}
#[allow(
    clippy::too_many_lines,
    reason = "Each finite profile checks its precise stored or transport cause and full native refusal"
)]
async fn native_refusal(
    pool: &sqlx::PgPool,
    r: &Value,
    status: u16,
    allow: Option<&String>,
    wire: &str,
    headers: &Value,
) -> Result<()> {
    assert_eq!(r["method"], "GET");
    assert_eq!(r["backend"], "local");
    assert_eq!(allow, None);
    let bytes = wire
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|v| Ok(u8::from_str_radix(std::str::from_utf8(v)?, 16)?))
        .collect::<Result<Vec<_>>>()?;
    let profile = r["native_error"].as_str().ok_or("profile")?;
    if profile == "workflow-revision" {
        assert_eq!(r["name"], "workflow");
        assert_eq!(r["role"], "experimenter");
        assert_eq!(
            r["status"], 200,
            "retain original source bool-as-integer transfer"
        );
        let workflow: Value = sqlx::query_scalar(
            "SELECT workflow FROM attempts WHERE id='00000000-0000-0000-0000-000000002002'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(
            workflow,
            json!({"steps":[{"name":"fixture-experiment","revision":true}]})
        );
        assert_eq!(status, 500);
        assert_eq!(bytes, b"Internal Server Error");
        assert_eq!(
            headers,
            &json!({"content-type":"text/plain; charset=utf-8","content-length":"21","cache-control":null,"location":null,"retry-after":null})
        );
        return Ok(());
    }
    let integer =
        |path: &str| json!({"path":path,"message":"Input should be a signed 64-bit integer"});
    let details = match profile {
        "header-integer" => {
            assert_eq!(r["name"], "generation");
            assert_eq!(r["role"], "experimenter");
            assert!(
                r["lease"]["X-Lease-Generation"]
                    .as_str()
                    .ok_or("header")?
                    .parse::<i64>()
                    .is_err()
            );
            vec![integer("header/X-Lease-Generation")]
        }
        "path-number" | "path-sequence" => {
            let (name, index) = if profile == "path-number" {
                ("number", 5)
            } else {
                ("sequence", 7)
            };
            assert_eq!(r["name"], name);
            assert_eq!(r["role"], "experimenter");
            assert!(
                r["path"]
                    .as_str()
                    .ok_or("path")?
                    .split('/')
                    .nth(index)
                    .ok_or("segment")?
                    .parse::<i64>()
                    .is_err()
            );
            vec![integer(&format!("path/{name}"))]
        }
        "aggregate-parameters" => {
            assert_eq!(r["name"], "aggregate");
            assert!(matches!(
                r["role"].as_str(),
                Some("agent" | "researcher" | "experimenter")
            ));
            let parts = r["path"]
                .as_str()
                .ok_or("path")?
                .split('/')
                .collect::<Vec<_>>();
            assert_eq!(parts[5], "bad");
            assert_eq!(parts[7], "bad");
            assert!(uuid::Uuid::parse_str(parts[10]).is_err());
            assert_eq!(r["lease"], json!({"X-Lease-Generation":"bad"}));
            vec![
                integer("path/number"),
                integer("path/sequence"),
                json!({"path":"path/artifact_id","message":"Input should be a valid UUID"}),
                integer("header/X-Lease-Generation"),
            ]
        }
        _ => return Err("unknown native predecessor profile".into()),
    };
    assert_eq!(status, 422);
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes)?,
        json!({"error":{"code":"validation_failed","message":"request validation failed","details":details}})
    );
    assert_eq!(
        headers,
        &json!({"content-type":"application/json","content-length":bytes.len().to_string(),"cache-control":null,"location":null,"retry-after":null})
    );
    Ok(())
}
struct Objects(std::path::PathBuf);
struct Task(tokio::task::JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}
impl Drop for Objects {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
