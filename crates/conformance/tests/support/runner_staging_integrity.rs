use super::runner::Rig;
use axum::{extract::State, response::IntoResponse};
use conformance::Result;
use reqwest::{Client, Method, StatusCode, Url, header};
use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
#[derive(Clone)]
struct RelayState {
    upstream: Url,
    base: String,
    client: Client,
    held: Arc<AtomicBool>,
    artifact: Arc<Mutex<Option<Value>>>,
    released: Arc<tokio::sync::Notify>,
    fault: Arc<AtomicBool>,
}
pub struct Relay {
    pub base: String,
    state: RelayState,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl Drop for Relay {
    fn drop(&mut self) {
        self.release();
        self.task.abort();
    }
}
impl Relay {
    pub async fn new(upstream: &str) -> Result<Self> {
        let upstream = Url::parse(upstream)?;
        if upstream.scheme() != "http"
            || !matches!(
                upstream.host_str(),
                Some("127.0.0.1" | "localhost" | "[::1]")
            )
            || upstream.port() != Some(9010)
            || !upstream.username().is_empty()
            || upstream.password().is_some()
            || upstream.path() != "/"
            || upstream.query().is_some()
        {
            return Err("relay upstream must be the fixed loopback conformance API".into());
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let state = RelayState {
            upstream,
            base: base.clone(),
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .build()?,
            held: Arc::default(),
            artifact: Arc::default(),
            released: Arc::default(),
            fault: Arc::default(),
        };
        let router = axum::Router::new()
            .fallback(axum::routing::any(route))
            .with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, router).await });
        Ok(Self { base, state, task })
    }
    pub async fn wait(&self) -> Result<Value> {
        let until = Instant::now() + Duration::from_secs(20);
        while !self.state.held.load(Ordering::SeqCst) {
            if self.state.fault.load(Ordering::SeqCst) {
                return Err("relay forwarding failed".into());
            }
            if Instant::now() > until {
                return Err("verified run upload barrier was not reached".into());
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        self.state
            .artifact
            .lock()
            .map_err(|_| "relay artifact lock poisoned")?
            .clone()
            .ok_or_else(|| "verified upload observation missing".into())
    }
    pub fn release(&self) {
        self.state.released.notify_one();
    }
    pub fn assert_clean(&self) {
        assert!(
            !self.state.fault.load(Ordering::SeqCst),
            "relay failed to forward actual upstream response"
        );
    }
}
async fn route(
    State(state): State<RelayState>,
    request: axum::extract::Request,
) -> axum::response::Response {
    if let Ok(response) = forward(&state, request).await {
        response
    } else {
        state.fault.store(true, Ordering::SeqCst);
        (
            StatusCode::BAD_GATEWAY,
            "controlled relay forwarding failure",
        )
            .into_response()
    }
}
fn transport_header(name: &header::HeaderName) -> bool {
    matches!(
        name.as_str(),
        "host"
            | "connection"
            | "transfer-encoding"
            | "content-length"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "upgrade"
    )
}
async fn forward(
    state: &RelayState,
    request: axum::extract::Request,
) -> Result<axum::response::Response> {
    let (parts, body) = request.into_parts();
    let path = parts
        .uri
        .path_and_query()
        .ok_or("relay path missing")?
        .as_str();
    if !path.starts_with("/api/") {
        return Err("relay accepts only API transport paths".into());
    }
    let mut target = state.upstream.clone();
    target.set_path(parts.uri.path());
    target.set_query(parts.uri.query());
    let bytes = axum::body::to_bytes(body, 1 << 20).await?;
    let headers: header::HeaderMap = parts
        .headers
        .iter()
        .filter(|(name, _)| !transport_header(name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let upstream = state
        .client
        .request(parts.method.clone(), target)
        .headers(headers)
        .body(bytes)
        .send()
        .await?;
    let status = upstream.status();
    let headers: header::HeaderMap = upstream
        .headers()
        .iter()
        .filter(|(name, _)| !transport_header(name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let mut bytes = upstream.bytes().await?.to_vec();
    if parts.method == Method::POST
        && parts.uri.path().ends_with("/uploads")
        && status == StatusCode::CREATED
    {
        let grant: Value = serde_json::from_slice(&bytes)?;
        if grant.get("direct").is_some_and(|direct| !direct.is_null()) {
            return Err("local upload relay does not emulate direct object-store plans".into());
        }
        let upload = Url::parse(
            grant["upload_url"]
                .as_str()
                .ok_or("grant upload URL missing")?,
        )?;
        if upload.origin() != state.upstream.origin()
            || !upload.path().starts_with("/api/job-uploads/")
        {
            return Err("grant escaped fixed upstream capability origin".into());
        }
        let mut proxied = Url::parse(&state.base)?;
        proxied.set_path(upload.path());
        proxied.set_query(upload.query());
        let old = serde_json::to_string(
            grant["upload_url"]
                .as_str()
                .ok_or("grant upload URL missing")?,
        )?;
        let new = serde_json::to_string(proxied.as_str())?;
        let original = std::str::from_utf8(&bytes)?;
        if original.matches(&old).count() != 1 {
            return Err("capability URL did not have one unambiguous wire value".into());
        }
        // Preserve all other bytes, including object order and whitespace.
        bytes = original.replacen(&old, &new, 1).into_bytes();
    }
    if parts.method == Method::PUT
        && parts.uri.path().starts_with("/api/job-uploads/")
        && status == StatusCode::CREATED
    {
        let artifact: Value = serde_json::from_slice(&bytes)?;
        if artifact["role"] == "run" && !state.held.load(Ordering::SeqCst) {
            *state
                .artifact
                .lock()
                .map_err(|_| "relay artifact lock poisoned")? = Some(artifact);
            state.held.store(true, Ordering::SeqCst);
            tokio::time::timeout(Duration::from_secs(25), state.released.notified())
                .await
                .map_err(|_| "upload barrier exceeded bound")?;
        }
    }
    Ok((status, headers, bytes).into_response())
}
pub fn output(rig: &Rig) -> Result<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(rig.work.0.join("work"))? {
        let path = entry?.path().join("0-overlap-producer/outputs/run");
        if path.is_dir() {
            found.push(path);
        }
    }
    if found.len() != 1 {
        return Err("expected exactly one owned producer output directory".into());
    }
    Ok(found.remove(0))
}
pub fn scorer_marker(rig: &Rig) -> Result<()> {
    let steps = rig.work.0.join("steps");
    fs::rename(steps.join("score.py"), steps.join("score_original.py"))?;
    rig.script("score.py",&format!("import runpy\nfrom pathlib import Path\n\ndef main() -> None:\n    Path({}).write_text('started')\n    runpy.run_path('score_original.py', run_name='__main__')\n\nif __name__ == '__main__':\n    main()\n",serde_json::to_string(&rig.work.0.join("scorer-started").to_str().ok_or("marker path not UTF8")?)?))
}
