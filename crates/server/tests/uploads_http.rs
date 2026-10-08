//! Actual upload HTTP replay against the independent unchanged-source corpus.
#![forbid(unsafe_code)]
#[path = "support/upload_wire.rs"]
mod wire;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use cannery_core::{principal::Secret, settings::load_settings};
use cannery_server::{
    api_models::{ErrorDetail, ErrorResponse, PresignRequest, UploadRequest},
    application_with_upload_context,
    upload_routes::{CancellationTasks, UploadContext},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    error::Error,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
const TOKEN: &str = "cr_upl_fixture_wire";
const BASE: &str = "/api/projects/matrix/hypotheses/1/attempts/1/uploads";
fn hex(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        value.push(char::from(b"0123456789abcdef"[usize::from(byte & 15)]));
    }
    value
}
struct Objects(PathBuf);
impl Drop for Objects {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn fixture() -> Result<Value> {
    Ok(serde_json::from_slice(&std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/upload_http/reference.json"),
    )?)?)
}
fn output(value: Value) -> Value {
    if value["error"]["message"] == "request validation failed" {
        return json!({"error":{"code":"validation_failed","details":value["error"]["details"].as_array().map(|v|v.iter().map(|v|json!({"path":v["path"]})).collect::<Vec<_>>())}});
    }
    value
}
fn presign_details(recipe: &Value) -> Result<Value> {
    assert!(matches!(
        recipe["name"].as_str().ok_or("name")?,
        "presign-body" | "s3-single-presign" | "s3-multipart-presign"
    ));
    let body: Value = serde_json::from_str(recipe["body"].as_str().ok_or("body")?)?;
    let (paths, message) = match recipe["native"]["case"].as_str().ok_or("presign case")? {
        "mixed-parts" => {
            assert_eq!(body, json!({"part_numbers":[0,true,"１",1.5,null]}));
            (
                vec![
                    "body/part_numbers/1",
                    "body/part_numbers/2",
                    "body/part_numbers/3",
                    "body/part_numbers/4",
                ],
                "Input should be a signed 64-bit integer",
            )
        }
        "mixed-multipart-parts" => {
            assert_eq!(body, json!({"part_numbers":[true,"２"]}));
            (
                vec!["body/part_numbers/0", "body/part_numbers/1"],
                "Input should be a signed 64-bit integer",
            )
        }
        "extra-field" => {
            assert_eq!(body, json!({"part_numbers":[1],"extra":false}));
            (vec!["body/extra"], "Extra inputs are not permitted")
        }
        "scalar-parts" => {
            assert_eq!(body, json!({"part_numbers":1}));
            (vec!["body/part_numbers"], "Input should be a valid list")
        }
        "empty-parts" => {
            assert_eq!(body, json!({"part_numbers":[]}));
            (
                vec!["body/part_numbers"],
                "List should have at least 1 item after validation",
            )
        }
        "too-many-parts" => {
            assert!(
                body == json!({"part_numbers":vec![1;101]})
                    || body
                        == json!({"part_numbers":std::iter::once(Value::Null).chain(std::iter::repeat_n(json!(1),100)).collect::<Vec<_>>()})
                    || body
                        == json!({"part_numbers":std::iter::repeat_n(json!(1),100).chain(std::iter::once(Value::Null)).collect::<Vec<_>>()})
            );
            (
                vec!["body/part_numbers"],
                "List should have at most 100 items after validation",
            )
        }
        "wide-parts" => {
            let numbers = body["part_numbers"].as_array().ok_or("parts")?;
            assert_eq!(numbers.len(), 1);
            let number = numbers[0].as_str().ok_or("wide part")?;
            assert!(matches!(number.len(), 4300 | 4301) && number.bytes().all(|byte| byte == b'9'));
            (
                vec!["body/part_numbers/0"],
                "Input should be a signed 64-bit integer",
            )
        }
        _ => return Err("unknown presign field profile".into()),
    };
    Ok(json!(
        paths
            .into_iter()
            .map(|path| json!({"path":path,"message":message}))
            .collect::<Vec<_>>()
    ))
}
fn native_expected(recipe: &Value) -> Result<Option<(u16, Value, Vec<u8>)>> {
    let native = &recipe["native"];
    if native.is_null() {
        return Ok(None);
    }
    let name = recipe["name"].as_str().ok_or("recipe name")?;
    let text = recipe["body"].as_str().ok_or("native body")?;
    let error = match native["kind"].as_str().ok_or("native kind")? {
        "dto-refusal" => {
            assert!(matches!(
                name,
                "body"
                    | "field-role"
                    | "field-name"
                    | "field-size_bytes"
                    | "field-sha256"
                    | "field-media_type"
                    | "presign-body"
                    | "s3-single-presign"
                    | "s3-multipart-presign"
            ));
            let root: Value = serde_json::from_str(text)?;
            match native["contract"].as_str().ok_or("contract")? {
                "UploadRequest" => assert!(
                    !root.is_object() || serde_json::from_str::<UploadRequest>(text).is_err()
                ),
                "PresignRequest" => {
                    assert!(
                        !root.is_object()
                            || serde_json::from_str::<Option<PresignRequest>>(text).is_err()
                    );
                }
                _ => return Err("unknown upload request DTO".into()),
            }
            ErrorDetail {
                code: "validation_failed".to_owned(),
                message: "request does not match the REST contract".to_owned(),
                details: Value::Null,
            }
        }
        "presign-fields" => ErrorDetail {
            code: "validation_failed".to_owned(),
            message: "request validation failed".to_owned(),
            details: presign_details(recipe)?,
        },
        "json-boundary" => {
            assert!(matches!(
                name,
                "field-role" | "field-media_type" | "field-size_bytes"
            ));
            assert!(text.contains("\\ud800") || text.contains("NaN") || text.contains("Infinity"));
            let Err(error) = serde_json::from_str::<Value>(text) else {
                return Err("invalid native UTF-8 JSON input was accepted".into());
            };
            ErrorDetail {
                code: "validation_failed".to_owned(),
                message: "request validation failed".to_owned(),
                details: json!([{"path":format!("body/{}",error.column()),"message":"JSON decode error"}]),
            }
        }
        "checked-path" => {
            assert_eq!(name, "grant-path");
            assert_eq!(native["fields"], json!(["number", "sequence"]));
            assert!(
                recipe["path"]
                    .as_str()
                    .ok_or("path")?
                    .contains("1267650600228229401496703205376")
            );
            serde_json::from_str::<UploadRequest>(text)?;
            ErrorDetail {
                code: "validation_failed".to_owned(),
                message: "request validation failed".to_owned(),
                details: json!([
                    {"path":"path/number","message":"Input should be a signed 64-bit integer"},
                    {"path":"path/sequence","message":"Input should be a signed 64-bit integer"}
                ]),
            }
        }
        _ => return Err("unknown native upload profile".into()),
    };
    let response = ErrorResponse { error };
    let value = output(serde_json::to_value(&response)?);
    Ok(Some((422, value, serde_json::to_vec(&response)?)))
}
fn check_storage(actual: &Value, expected: &Value, name: &Value) {
    if let Some(tables) = actual.as_object() {
        for (table, rows) in tables {
            assert_eq!(rows, &expected[table], "storage {name} table {table}");
        }
    }
}
async fn clock(pool: &sqlx::PgPool) -> Result<cannery_core::timestamps::Timestamp> {
    Ok(sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(pool)
        .await?)
}
fn checked_signing_clocks(
    value: &mut Value,
    before: cannery_core::timestamps::Timestamp,
    after: cannery_core::timestamps::Timestamp,
) -> Result<()> {
    if let Some(direct) = value.get_mut("direct").filter(|v| !v.is_null()) {
        checked_signing_clocks(direct, before, after)?;
    }
    if let Some(request) = value.get_mut("request").filter(|v| !v.is_null()) {
        check_expiry(request, before, after)?;
    }
    if let Some(parts) = value.get_mut("parts").and_then(Value::as_array_mut) {
        for part in parts {
            check_expiry(part, before, after)?;
        }
    }
    Ok(())
}
fn signing_expiries(value: &Value, out: &mut Vec<String>) {
    if let Some(direct) = value.get("direct") {
        signing_expiries(direct, out);
    }
    if let Some(request) = value.get("request")
        && let Some(expiry) = request["expires_at"].as_str()
    {
        out.push(expiry.to_owned());
    }
    if let Some(parts) = value.get("parts").and_then(Value::as_array) {
        for part in parts {
            if let Some(expiry) = part["expires_at"].as_str() {
                out.push(expiry.to_owned());
            }
        }
    }
}
fn check_expiry(
    request: &mut Value,
    before: cannery_core::timestamps::Timestamp,
    after: cannery_core::timestamps::Timestamp,
) -> Result<()> {
    let expiry = request["expires_at"]
        .as_str()
        .ok_or("presign expiry")?
        .parse::<cannery_core::timestamps::Timestamp>()?;
    assert!(
        before.0 + chrono::TimeDelta::seconds(900) <= expiry.0
            && expiry.0 <= after.0 + chrono::TimeDelta::seconds(900)
    );
    request["expires_at"] = json!("@checked-presign-expiry");
    Ok(())
}
async fn reset(pool: &sqlx::PgPool, setup: &str) -> Result<()> {
    sqlx::raw_sql("DROP FUNCTION IF EXISTS fixture_upload_row() CASCADE; DROP TABLE IF EXISTS fixture_upload_profile; TRUNCATE users,projects,idempotency_keys RESTART IDENTITY CASCADE").execute(pool).await?;
    for seed in [
        include_str!("fixtures/attempt_reads_http/seed.sql"),
        include_str!("fixtures/upload_http/seed.sql"),
    ] {
        sqlx::raw_sql(seed).execute(pool).await?;
    }
    if !setup.is_empty() {
        sqlx::raw_sql(setup).execute(pool).await?;
    }
    Ok(())
}
async fn storage(pool: &sqlx::PgPool, root: &Path) -> Result<Value> {
    let mut out = json!({});
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
        ("config_revisions", "project_id,kind,revision"),
    ] {
        let expression = if table == "artifacts" {
            let records:Vec<(String,Option<String>,i64,String)>=sqlx::query_as("SELECT key,generation,size_bytes,sha256 FROM artifacts WHERE id='00000000-0000-0000-0000-000000090002' AND backend='local'").fetch_all(pool).await?;
            for (key, generation, size, sha) in records {
                use std::os::unix::fs::MetadataExt;
                let path = root.join("local").join(key);
                let meta = std::fs::metadata(&path)?;
                let nanos =
                    i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec());
                assert_eq!(generation, Some(format!("{}:{nanos}", meta.ino())));
                assert_eq!(u64::try_from(size)?, meta.len());
                assert_eq!(sha, format!("{:x}", Sha256::digest(std::fs::read(&path)?)));
            }
            "CASE WHEN id='00000000-0000-0000-0000-000000090002' AND backend='local' THEN jsonb_set(to_jsonb(t),'{generation}','\"@checked-filesystem-generation\"'::jsonb) ELSE to_jsonb(t) END"
        } else {
            "to_jsonb(t)"
        };
        out[table] = json!(
            sqlx::query_scalar::<_, String>(&format!(
                "SELECT ({expression})::text FROM {table} t ORDER BY {order}"
            ))
            .fetch_all(pool)
            .await?
        );
    }
    Ok(out)
}
fn files(root: &Path) -> Result<Value> {
    fn visit(root: &Path, path: &Path, out: &mut Value) -> Result<()> {
        for item in std::fs::read_dir(path)? {
            let item = item?;
            let path = item.path();
            if path.is_dir() {
                visit(root, &path, out)?;
            } else {
                out[path.strip_prefix(root)?.to_str().ok_or("file path")?] =
                    json!(hex(&std::fs::read(&path)?));
            }
        }
        Ok(())
    }
    let mut out = json!({});
    visit(root, root, &mut out)?;
    Ok(out)
}
async fn call(
    pool: &sqlx::PgPool,
    app: &Router,
    path: &str,
    method: &str,
    role: &str,
    headers: &Value,
    body: Option<&str>,
) -> Result<(u16, Value, Value, Vec<u8>)> {
    let mut req = Request::builder().uri(path).method(method);
    if role != "none" {
        let prefix = if [
            "agent",
            "foreign-agent",
            "tester",
            "evaluator",
            "experimenter",
        ]
        .contains(&role)
        {
            "cr_svc_"
        } else {
            "cr_pat_"
        };
        req = req.header("authorization", format!("Bearer {prefix}track_http_{role}"));
    }
    for h in headers.as_array().ok_or("headers")? {
        req = req.header(
            h[0].as_str().ok_or("header")?,
            h[1].as_str().ok_or("header")?,
        );
    }
    let before = clock(pool).await?;
    let response = app
        .clone()
        .oneshot(req.body(Body::from(body.unwrap_or_default().to_owned()))?)
        .await?;
    let status = response.status().as_u16();
    let mut headers = json!({});
    for key in [
        "content-type",
        "content-length",
        "allow",
        "location",
        "cache-control",
        "retry-after",
        "www-authenticate",
    ] {
        headers[key] = json!(
            response
                .headers()
                .get(key)
                .map(|v| v.to_str())
                .transpose()?
        );
    }
    let mut bytes = to_bytes(response.into_body(), usize::MAX).await?.to_vec();
    let after = clock(pool).await?;
    let mut value = if status == 500 || bytes.is_empty() {
        if status == 500 {
            assert_eq!(bytes, b"Internal Server Error");
        }
        Value::Null
    } else {
        output(serde_json::from_slice(&bytes)?)
    };
    if matches!(status, 200 | 201) {
        let mut expiries = Vec::new();
        signing_expiries(&value, &mut expiries);
        checked_signing_clocks(&mut value, before, after)?;
        let mut wire = String::from_utf8(bytes)?;
        for expiry in expiries {
            wire = wire.replace(
                &serde_json::to_string(&expiry)?,
                "\"@checked-presign-expiry\"",
            );
        }
        bytes = wire.into_bytes();
    }
    Ok((status, headers, value, bytes))
}
#[allow(
    clippy::unnecessary_wraps,
    reason = "Explicit controller callback signature"
)]
fn mint() -> std::result::Result<Secret, cannery_identity::error::IdentityError> {
    Ok(Secret::new(TOKEN.to_owned()))
}
#[allow(
    clippy::unnecessary_wraps,
    reason = "Explicit controller callback signature"
)]
fn nonce() -> std::result::Result<String, cannery_identity::error::IdentityError> {
    Ok("0123456789abcdef".to_owned())
}
fn signing_clock() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}
struct HttpTask(tokio::task::JoinHandle<std::io::Result<()>>);
impl Drop for HttpTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn disconnect_retry(pool: &sqlx::PgPool, app: &Router, root: &Path) -> Result<()> {
    use sqlx::Connection;
    use tokio::io::AsyncWriteExt;
    let mut observer = sqlx::PgConnection::connect_with(&pool.connect_options()).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = app.clone();
    let _server = HttpTask(tokio::spawn(
        async move { axum::serve(listener, app).await },
    ));
    let mut stream = tokio::net::TcpStream::connect(address).await?;
    stream.write_all(format!("PUT /api/uploads/00000000-0000-0000-0000-000000090001 HTTP/1.1\r\nHost: localhost\r\nX-Upload-Token: {TOKEN}\r\nContent-Length: 5\r\n\r\nhel").as_bytes()).await?;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(entries) = std::fs::read_dir(root.join("local/.staging")) {
                for entry in entries {
                    if entry?.metadata()?.len() == 3 {
                        let state: String = sqlx::query_scalar("SELECT state FROM uploads")
                            .fetch_one(&mut observer)
                            .await?;
                        assert_eq!(state, "receiving");
                        return Ok::<_, Box<dyn Error + Send + Sync>>(());
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    drop(stream);
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let state: String = sqlx::query_scalar("SELECT state FROM uploads")
                .fetch_one(pool)
                .await?;
            if state == "pending"
                && std::fs::read_dir(root.join("local/.staging"))?
                    .next()
                    .is_none()
            {
                return Ok::<_, Box<dyn Error + Send + Sync>>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    Ok(())
}
struct RequestCancellation(tokio::task::AbortHandle);
impl Drop for RequestCancellation {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn dropped_receive_recovers(
    pool: &sqlx::PgPool,
    app: &Router,
    root: &Path,
    cleanup: &CancellationTasks,
) -> Result<()> {
    use futures_util::{StreamExt, stream};
    let chunks = stream::iter([Ok::<_, std::io::Error>(b"hel".to_vec())]).chain(stream::pending());
    let request = Request::builder()
        .method("PUT")
        .uri("/api/uploads/00000000-0000-0000-0000-000000090001")
        .header("x-upload-token", TOKEN)
        .body(Body::from_stream(chunks))?;
    let app = app.clone();
    let mut task = tokio::spawn(async move { app.oneshot(request).await });
    let _cancel = RequestCancellation(task.abort_handle());
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if task.is_finished() {
                return Err("receive ended before cancellation".into());
            }
            if let Ok(entries) = std::fs::read_dir(root.join("local/.staging")) {
                for entry in entries {
                    if entry?.metadata()?.len() == 3 {
                        return Ok::<_, Box<dyn Error + Send + Sync>>(());
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    task.abort();
    assert!((&mut task).await.is_err_and(|e| e.is_cancelled()));
    cleanup.drain().await?;
    assert!(
        std::fs::read_dir(root.join("local/.staging"))?
            .next()
            .is_none()
    );
    let state: String = sqlx::query_scalar("SELECT state FROM uploads")
        .fetch_one(pool)
        .await?;
    assert_eq!(state, "pending");
    Ok(())
}
#[tokio::test]
#[ignore = "requires exact positive selection and an owned migrated PostgreSQL child"]
#[allow(
    clippy::too_many_lines,
    reason = "Full source HTTP and raw storage replay"
)]
async fn uploads_match_production() -> Result<()> {
    let url = std::env::var("CANNERY_UPLOAD_HTTP_DATABASE_URL")?;
    let settings = load_settings(
        None,
        &BTreeMap::from([
            ("CANNERY_DATABASE_URL".into(), url),
            (
                "CANNERY_SERVER_PUBLIC_BASE_URL".into(),
                "https://cannery.test".into(),
            ),
            ("CANNERY_DATABASE_POOL_MAX_SIZE".into(), "1".into()),
        ]),
    )?;
    let root = Objects(std::env::temp_dir().join(format!("cr-upload-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(&root.0)?;
    let context = Arc::new(UploadContext {
        repository: cannery_attempts::model::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
        store: Arc::new(cannery_storage::ObjectStore::Local(
            cannery_storage::local::LocalStore::new(&root.0, "local")?,
        )),
        signing_clock,
        mint,
        nonce,
        settlement_timeout: Duration::from_secs(20),
        cancellation: CancellationTasks::new(),
    });
    let (app, state) = application_with_upload_context(settings.clone(), context.clone())?;
    let (endpoint, wire_state, _wire_task) = wire::start().await?;
    let mut env = BTreeMap::from([(
        "CANNERY_DATABASE_URL".to_owned(),
        std::env::var("CANNERY_UPLOAD_HTTP_DATABASE_URL")?,
    )]);
    for (key, value) in [
        ("BACKEND", "s3"),
        ("BUCKET", "fixture"),
        ("S3_ENDPOINT", endpoint.as_str()),
        ("S3_PUBLIC_ENDPOINT", "https://s3.example.org"),
        ("S3_ACCESS_KEY_ID", "fixture-key"),
        ("S3_SECRET_ACCESS_KEY", "fixture-signing-only"),
        ("S3_REGION", "fixture"),
        ("S3_PATH_STYLE", "true"),
        ("S3_PREFIX", "proof/"),
        ("S3_MULTIPART_THRESHOLD_BYTES", "5242880"),
        ("S3_PART_SIZE_BYTES", "5242880"),
        ("MAX_OBJECT_BYTES", "20971520"),
    ] {
        env.insert(format!("CANNERY_STORAGE_{key}"), value.to_owned());
    }
    let storage_settings = load_settings(None, &env)?.storage;
    let s3_context = Arc::new(UploadContext {
        repository: context.repository,
        store: Arc::new(cannery_storage::ObjectStore::S3(
            cannery_storage::s3::S3Store::from_settings(&storage_settings).await?,
        )),
        signing_clock,
        mint,
        nonce,
        settlement_timeout: Duration::from_secs(20),
        cancellation: CancellationTasks::new(),
    });
    let (s3_app, s3_state) = application_with_upload_context(settings.clone(), s3_context.clone())?;
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    let suffix = database
        .strip_prefix("conformance_")
        .ok_or("owned child required")?;
    if suffix.len() != 24 || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("owned child required".into());
    }
    let fixture = fixture()?;
    for recipe in fixture["cases"].as_array().ok_or("source cases")? {
        let selected = if recipe["backend"] == "s3" {
            &s3_app
        } else {
            &app
        };
        {
            let mut wire = wire_state.lock().map_err(|_| "wire state")?;
            wire.mode = recipe["wire_mode"].as_str().unwrap_or("single").to_owned();
            wire.calls.clear();
        }
        reset(&state.pool, recipe["setup"].as_str().ok_or("setup")?).await?;
        if root.0.join("local").exists() {
            std::fs::remove_dir_all(root.0.join("local"))?;
        }
        if recipe["grant"] == true {
            let valid = if recipe["grant_body"].is_null() {
                json!({"role":"data","name":"payload.bin","size_bytes":5,"sha256":format!("{:x}",Sha256::digest(b"hello")),"media_type":"application/octet-stream"}).to_string()
            } else {
                recipe["grant_body"].to_string()
            };
            let (status, _, body, bytes) = call(
                &state.pool,
                selected,
                BASE,
                "POST",
                "researcher",
                &json!([
                    ["X-Lease-Token", "cr_lease_fixture"],
                    ["X-Lease-Generation", "1"],
                    ["content-type", "application/json"]
                ]),
                Some(&valid),
            )
            .await?;
            assert_eq!(status, 201, "grant {:?}", recipe["name"]);
            assert_eq!(body, recipe["prepared"]["body"]);
            if let Some(wire) = recipe["prepared"]["wire"].as_str() {
                assert_eq!(hex(&bytes), wire);
            }
            if let Some(setup) = recipe["after_grant"].as_str().filter(|s| !s.is_empty()) {
                sqlx::raw_sql(setup).execute(&state.pool).await?;
            }
        }
        check_storage(
            &storage(&state.pool, &root.0).await?,
            &recipe["initial"],
            &recipe["name"],
        );
        if recipe["disconnect"] == true {
            disconnect_retry(&state.pool, selected, &root.0).await?;
            dropped_receive_recovers(&state.pool, selected, &root.0, &context.cancellation).await?;
            check_storage(
                &storage(&state.pool, &root.0).await?,
                &recipe["initial"],
                &recipe["name"],
            );
        }
        let native = native_expected(recipe)?;
        let (status, mut headers, body, bytes) = call(
            &state.pool,
            selected,
            recipe["path"].as_str().ok_or("path")?,
            recipe["method"].as_str().ok_or("method")?,
            recipe["role"].as_str().ok_or("role")?,
            &recipe["headers"],
            recipe["body"].as_str(),
        )
        .await?;
        assert_eq!(
            status,
            native.as_ref().map_or(
                u16::try_from(recipe["status"].as_u64().ok_or("status")?)?,
                |(status, _, _)| *status,
            ),
            "status {:?}",
            recipe["name"]
        );
        assert_eq!(
            body,
            native
                .as_ref()
                .map_or(&recipe["output"], |(_, value, _)| value)
                .clone(),
            "output {:?}",
            recipe["name"]
        );
        let mut expected = recipe["response_headers"].clone();
        if let Some((_, _, expected_bytes)) = &native {
            expected["content-type"] = json!("application/json");
            expected["content-length"] = json!(expected_bytes.len().to_string());
        } else if status == 422 && recipe["output"]["error"]["message"].is_null() {
            headers["content-length"] = Value::Null;
            expected["content-length"] = Value::Null;
        }
        assert_eq!(headers, expected, "headers {:?}", recipe["name"]);
        if let Some((_, _, expected_bytes)) = &native {
            assert_eq!(
                &bytes, expected_bytes,
                "native error wire {:?}",
                recipe["name"]
            );
        } else if let Some(wire) = recipe["wire"].as_str() {
            assert_eq!(hex(&bytes), wire, "wire {:?}", recipe["name"]);
        }
        check_storage(
            &storage(&state.pool, &root.0).await?,
            if native.is_some() {
                &recipe["initial"]
            } else {
                &fixture["snapshots"][recipe["snapshot"].as_str().ok_or("snapshot")?]
            },
            &recipe["name"],
        );
        assert_eq!(
            json!(wire_state.lock().map_err(|_| "wire state")?.calls),
            if native.is_some() {
                recipe["initial_store_calls"].clone()
            } else {
                recipe["store_calls"].clone()
            },
            "wire {:?}",
            recipe["name"]
        );
        assert_eq!(
            files(&root.0)?,
            if native.is_some() {
                recipe["initial_files"].clone()
            } else {
                recipe["files"].clone()
            },
            "files {:?}",
            recipe["name"]
        );
    }
    {
        use sqlx::Connection;
        reset(&state.pool, "").await?;
        if root.0.join("local").exists() {
            std::fs::remove_dir_all(root.0.join("local"))?;
        }
        let race = &fixture["grant_race"];
        check_storage(
            &storage(&state.pool, &root.0).await?,
            &race["initial"],
            &json!("grant race initial"),
        );
        let (second, second_state) = application_with_upload_context(settings, context.clone())?;
        let mut holder = sqlx::PgConnection::connect_with(&state.pool.connect_options()).await?;
        let mut monitor = sqlx::PgConnection::connect_with(&state.pool.connect_options()).await?;
        let mut lock = holder.begin().await?;
        sqlx::query(
            "SELECT id FROM attempts WHERE id='00000000-0000-0000-0000-000000002001' FOR UPDATE",
        )
        .execute(&mut *lock)
        .await?;
        let mut waiters = Vec::new();
        let mut cancellations = Vec::new();
        for (router, role) in [(app.clone(), "researcher"), (second, "researcher-copy")] {
            let pool = state.pool.clone();
            let task = tokio::spawn(async move {
                call(&pool, &router, BASE, "POST", role, &json!([["X-Lease-Token","cr_lease_fixture"],["X-Lease-Generation","1"],["content-type","application/json"]]), Some(r#"{"role":"data","name":"payload.bin","size_bytes":5,"sha256":"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824","media_type":"application/octet-stream"}"#)).await
            });
            cancellations.push(RequestCancellation(task.abort_handle()));
            waiters.push(task);
        }
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let count:i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%FROM attempts%'").fetch_one(&mut monitor).await?;
                if count == 2 { return Ok::<_,sqlx::Error>(()); }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await??;
        lock.rollback().await?;
        let mut responses = Vec::new();
        for waiter in waiters {
            let (status, _headers, output, bytes) =
                tokio::time::timeout(Duration::from_secs(30), waiter).await???;
            responses.push(json!({"status":status,"output":output,"wire":hex(&bytes)}));
        }
        responses.sort_by_key(|response| response["status"].as_u64());
        assert_eq!(
            responses,
            race["responses"]
                .as_array()
                .ok_or("race responses")?
                .clone()
        );
        assert_eq!(race["two_row_lock_waiters"], true);
        check_storage(
            &storage(&state.pool, &root.0).await?,
            &race["storage"],
            &json!("grant race"),
        );
        second_state.pool.close().await;
    }
    {
        use futures_util::stream;
        use std::task::Poll;
        let before = storage(&state.pool, &root.0).await?;
        {
            let mut wire = wire_state.lock().map_err(|_| "relay tracking")?;
            wire.mode = "relay".to_owned();
            wire.calls.clear();
        }
        let pending = Arc::new(tokio::sync::Notify::new());
        let notify = pending.clone();
        let mut first = true;
        let chunks = stream::poll_fn(move |_| {
            if first {
                first = false;
                Poll::Ready(Some(Ok(vec![b'a'; 8 * (1 << 20)])))
            } else {
                notify.notify_one();
                Poll::Pending
            }
        });
        let mut write = Box::pin(s3_context.store.write_new_with_cancellation(
            "relay-cancel",
            chunks,
            9 * (1 << 20),
            s3_context.cancellation.store_sink(Duration::from_secs(20)),
        ));
        tokio::select! {
            result=&mut write=>{result?;return Err("relay ended before cancellation".into());},
            result=tokio::time::timeout(Duration::from_secs(20),pending.notified())=>{result?;}
        }
        drop(write);
        s3_context.cancellation.drain().await?;
        check_storage(
            &storage(&state.pool, &root.0).await?,
            &before,
            &json!("relay cancellation"),
        );
        assert_eq!(fixture["relay_cancellation"]["unchanged_storage"], true);
        assert_eq!(
            json!(wire_state.lock().map_err(|_| "relay tracking")?.calls),
            fixture["relay_cancellation"]["calls"]
        );
    }
    context.cancellation.drain().await?;
    s3_context.cancellation.drain().await?;
    state.pool.close().await;
    s3_state.pool.close().await;
    Ok(())
}
