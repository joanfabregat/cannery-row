//! Actual Claude connection/discovery, without model credentials or model calls.
#![forbid(unsafe_code)]
#[path = "support/identity_security_support.rs"]
#[allow(dead_code, reason = "Reuse actual OIDC fixture")]
mod identity;
#[path = "support/r2_identity_projects_support.rs"]
#[allow(dead_code, reason = "Reuse actual project and scoped token fixture")]
mod support;

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, Response, StatusCode},
    routing::any,
};
use conformance::Result;
use reqwest::Method;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
struct Peer {
    client: reqwest::Client,
    target: String,
    authorization: String,
    methods: Arc<Mutex<Vec<String>>>,
    tools: Arc<Mutex<BTreeSet<String>>>,
}
fn refusal(status: StatusCode) -> Response<Body> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}
async fn forward(State(peer): State<Peer>, request: Request<Body>) -> Response<Body> {
    let (parts, body) = request.into_parts();
    if parts
        .headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some(peer.authorization.as_str())
    {
        return refusal(StatusCode::UNAUTHORIZED);
    }
    let Ok(body) = axum::body::to_bytes(body, 1_048_576).await else {
        return refusal(StatusCode::PAYLOAD_TOO_LARGE);
    };
    let method = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|value| value["method"].as_str().map(str::to_owned));
    if let Some(method) = &method {
        let Ok(mut methods) = peer.methods.lock() else {
            return refusal(StatusCode::INTERNAL_SERVER_ERROR);
        };
        methods.push(method.clone());
    }
    let mut headers = parts.headers;
    for name in ["host", "connection", "transfer-encoding", "content-length"] {
        headers.remove(name);
    }
    let result = peer
        .client
        .request(parts.method, &peer.target)
        .headers(headers)
        .body(body)
        .send()
        .await;
    let Ok(result) = result else {
        return refusal(StatusCode::BAD_GATEWAY);
    };
    let status = result.status();
    let headers = result.headers().clone();
    let Ok(body) = result.bytes().await else {
        return refusal(StatusCode::BAD_GATEWAY);
    };
    if method.as_deref() == Some("tools/list")
        && status == 200
        && let Ok(value) = serde_json::from_slice::<Value>(&body)
        && let Some(tools) = value["result"]["tools"].as_array()
    {
        let Ok(mut observed) = peer.tools.lock() else {
            return refusal(StatusCode::INTERNAL_SERVER_ERROR);
        };
        for tool in tools {
            if let Some(name) = tool["name"].as_str() {
                observed.insert(name.to_owned());
            }
        }
    }
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    for (name, value) in &headers {
        if !["connection", "transfer-encoding", "content-length"].contains(&name.as_str()) {
            response.headers_mut().insert(name, value.clone());
        }
    }
    response
}
struct Ram(PathBuf);
impl Drop for Ram {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Proxy(tokio::task::JoinHandle<()>);
impl Drop for Proxy {
    fn drop(&mut self) {
        self.0.abort();
    }
}
fn private_file(path: &std::path::Path, bytes: &[u8]) -> Result<fs::File> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    Ok(file)
}

#[tokio::test]
#[ignore = "requires native conformance server and verified Claude ELF mounted via approved runner"]
#[allow(
    clippy::too_many_lines,
    reason = "Keep isolated actual-client acceptance sequence visible"
)]
async fn actual_claude_connects_to_authenticated_native_mcp() -> Result<()> {
    assert_eq!(
        std::env::var("CANNERY_CONFORMANCE_SERVER_IMPLEMENTATION")?,
        "rust"
    );
    let binary = PathBuf::from(std::env::var("CLAUDE_ACCEPTANCE_BINARY")?).canonicalize()?;
    assert!(binary.is_file());
    let mut world = support::World::new().await?;
    world.memberships().await?;
    let token = identity::string(&world.readonly["token"])?;
    let project = world
        .ctx
        .api(
            Method::GET,
            "/api/projects/{slug}",
            &world.base(),
            &token,
            None,
            200,
        )
        .await?;
    assert_eq!(project["id"], world.project["id"]);
    assert_eq!(project["role"], "researcher");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/mcp", listener.local_addr()?);
    let peer = Peer {
        client: reqwest::Client::new(),
        target: format!("{}/mcp", world.ctx.base),
        authorization: format!("Bearer {token}"),
        methods: Arc::default(),
        tools: Arc::default(),
    };
    let router = Router::new()
        .route("/mcp", any(forward))
        .with_state(peer.clone());
    let _proxy = Proxy(tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    }));
    let scratch = std::env::var_os("CLAUDE_ACCEPTANCE_RAM_DIR")
        .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    assert!(scratch.is_absolute() && scratch.is_dir());
    let ram = Ram(PathBuf::from(format!(
        "{}/claude-mcp-{}-{}",
        scratch.display(),
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    )));
    fs::create_dir(&ram.0)?;
    fs::set_permissions(&ram.0, fs::Permissions::from_mode(0o700))?;
    for name in ["config", "cwd", "home"] {
        fs::create_dir(ram.0.join(name))?;
    }
    let config = json!({"mcpServers":{"cannery":{"type":"http","url":url,"headers":{"Authorization":"Bearer ${CANNERY_MCP_TEST_TOKEN}"}}}});
    private_file(
        &ram.0.join("config/.claude.json"),
        &serde_json::to_vec(&config)?,
    )?;
    let stdout = private_file(&ram.0.join("stdout"), &[])?;
    let stderr = private_file(&ram.0.join("stderr"), &[])?;
    let root = ram.0.clone();
    let success = tokio::task::spawn_blocking(move || -> Result<bool> {
        let mut child = Running(
            Command::new(binary)
                .args(["--setting-sources", "user", "mcp", "list"])
                .current_dir(root.join("cwd"))
                .env_clear()
                .env("PATH", "/usr/local/bin:/usr/bin:/bin")
                .env("HOME", root.join("home"))
                .env("CLAUDE_CONFIG_DIR", root.join("config"))
                .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
                .env("DISABLE_AUTOUPDATER", "1")
                .env("CANNERY_MCP_TEST_TOKEN", token)
                .stdin(Stdio::null())
                .stdout(stdout)
                .stderr(stderr)
                .spawn()?,
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = child.0.try_wait()? {
                return Ok(status.success());
            }
            if Instant::now() >= deadline {
                return Err("actual Claude health check exceeded deadline".into());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    })
    .await??;
    assert!(success, "actual Claude health check failed");
    let output = fs::read_to_string(ram.0.join("stdout"))?;
    let error = fs::read_to_string(ram.0.join("stderr"))?;
    assert!(
        output.contains("Connected"),
        "actual Claude did not connect"
    );
    assert!(
        !output.contains("Failed"),
        "actual Claude reported connection failure"
    );
    assert!(
        !error.contains("Failed"),
        "actual Claude reported connection failure"
    );
    let methods = peer
        .methods
        .lock()
        .map_err(|_| "peer observation lock")?
        .clone();
    assert!(methods.iter().any(|method| method == "initialize"));
    assert!(methods.iter().any(|method| method == "tools/list"));
    assert!(!methods.iter().any(|method| method == "tools/call"));
    let expected: Value = serde_json::from_str(include_str!("../../server/src/mcp/tools.json"))?;
    let expected: BTreeSet<String> = expected
        .as_array()
        .ok_or("tool registry shape")?
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .map(str::to_owned)
                .ok_or("tool registry name")
        })
        .collect::<std::result::Result<_, _>>()?;
    assert_eq!(expected.len(), 42);
    assert_eq!(
        *peer.tools.lock().map_err(|_| "peer discovery lock")?,
        expected
    );
    fs::write(
        PathBuf::from(std::env::var("CANNERY_CONFORMANCE_COVERAGE_DIR")?)
            .join("claude-client.json"),
        serde_json::to_vec_pretty(
            &json!({"operations":{},"tools":[],"audit_actions":[],"client":"Claude Code 2.1.289","scope":"authenticated connection and tool discovery; no model or tool execution","methods":methods,"discovered_tools":42}),
        )?,
    )?;
    Ok(())
}
