use super::{
    lifecycle::{ATTEMPT, Call},
    runner::{REPOSITORY, Rig},
};
use axum::{extract::State, response::IntoResponse};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use conformance::Result;
use reqwest::{StatusCode, header};
use serde_json::{Value, json};
use std::{
    fs,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const TOKEN: &str = "owned-refresh-installation-secret";
#[derive(Clone)]
struct ProviderState {
    archive: Vec<u8>,
    public: Vec<u8>,
    base: String,
    mints: Arc<AtomicUsize>,
    downloads: Arc<AtomicUsize>,
    invalid: Arc<AtomicBool>,
}
pub struct Provider {
    state: ProviderState,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Provider {
    pub async fn new(archive: Vec<u8>, public: Vec<u8>) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let state = ProviderState {
            archive,
            public,
            base: format!("http://{}", listener.local_addr()?),
            mints: Arc::default(),
            downloads: Arc::default(),
            invalid: Arc::default(),
        };
        let router = axum::Router::new()
            .fallback(axum::routing::any(route))
            .with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, router).await });
        Ok(Self { state, task })
    }
    pub fn github(&self, rig: &Rig) -> Value {
        json!({"api_url":self.state.base,"allowed_repos":[REPOSITORY],"app_id":"4242","app_installation_id":"77","app_key_file":rig.work.0.join("app.pem")})
    }
    pub fn mints(&self) -> usize {
        self.state.mints.load(Ordering::SeqCst)
    }
    pub fn downloads(&self) -> usize {
        self.state.downloads.load(Ordering::SeqCst)
    }
    pub fn check(&self) {
        assert!(
            !self.state.invalid.load(Ordering::SeqCst),
            "provider authentication scope or signature invalid"
        );
    }
}
async fn route(
    State(state): State<ProviderState>,
    request: axum::extract::Request,
) -> axum::response::Response {
    let path = request.uri().path();
    let auth = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    if path == "/app/installations/77/access_tokens" {
        let generation = state.mints.fetch_add(1, Ordering::SeqCst) + 1;
        if !valid_jwt(auth, &state.public) {
            state.invalid.store(true, Ordering::SeqCst);
        }
        // Real wall time; the worker's five-minute refresh margin is never overridden.
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |time| time.as_secs())
            + 330;
        let expiry = Command::new("date")
            .args(["-u", "-d", &format!("@{seconds}"), "+%Y-%m-%dT%H:%M:%SZ"])
            .output();
        let Ok(expiry) = expiry else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        if !expiry.status.success() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        return (
            StatusCode::CREATED,
            axum::Json(
                json!({"token":format!("{TOKEN}-{generation}"),"expires_at":String::from_utf8_lossy(&expiry.stdout).trim()}),
            ),
        )
            .into_response();
    }
    if path == "/archive" {
        if auth.is_some() {
            state.invalid.store(true, Ordering::SeqCst);
        }
        state.downloads.fetch_add(1, Ordering::SeqCst);
        return (
            [(header::CONTENT_TYPE, "application/gzip")],
            state.archive.clone(),
        )
            .into_response();
    }
    if auth != Some(format!("Bearer {TOKEN}-{}", state.mints.load(Ordering::SeqCst)).as_str()) {
        state.invalid.store(true, Ordering::SeqCst);
    }
    if path == format!("/repos/{REPOSITORY}") {
        return axum::Json(json!({"default_branch":"main"})).into_response();
    }
    if path.contains("/compare/") {
        return axum::Json(json!({"status":"identical"})).into_response();
    }
    if path.contains("/tarball/") {
        return (
            StatusCode::FOUND,
            [(header::LOCATION, format!("{}/archive", state.base))],
        )
            .into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}
fn valid_jwt(auth: Option<&str>, key: &[u8]) -> bool {
    let Some(token) = auth.and_then(|value| value.strip_prefix("Bearer ")) else {
        return false;
    };
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    let Ok(signature) = URL_SAFE_NO_PAD.decode(parts[2]) else {
        return false;
    };
    if ring::signature::UnparsedPublicKey::new(&ring::signature::RSA_PKCS1_2048_8192_SHA256, key)
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .is_err()
    {
        return false;
    }
    let Ok(bytes) = URL_SAFE_NO_PAD.decode(parts[1]) else {
        return false;
    };
    let Ok(claims) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    claims["iss"] == "4242"
        && claims["exp"]
            .as_i64()
            .zip(claims["iat"].as_i64())
            .is_some_and(|(exp, iat)| exp - iat == 600)
}
pub fn command(
    rig: &Rig,
    kind: &str,
    token_file: &str,
    github: Option<Value>,
    once: bool,
) -> Result<Command> {
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/fixture/data")
        .canonicalize()?;
    let mut config = json!({"api_url":rig.world.base_url,"project":rig.world.project,"data_root":data,"work_root":rig.work.0.join("work"),"cache_root":rig.work.0.join("cache"),"cache_max_bytes":1,"launcher":{"type":"local","step_root":rig.work.0.join("steps")},"kinds":[{"kind":kind,"token_file":rig.work.0.join(token_file),"poll_seconds":0.1}]});
    if let Some(github) = github {
        config["github"] = github;
    }
    let path = rig.work.0.join(format!("renewal-{kind}.json"));
    fs::write(&path, serde_json::to_vec(&config)?)?;
    let mut command = Command::new(std::env::var("CANNERY_CONFORMANCE_CLI")?);
    command.args(["runner", "--config"]).arg(path);
    if once {
        command.arg("--once");
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Ok(command)
}
pub async fn completed(rig: &mut Rig, number: i64) -> Result<Value> {
    let until = Instant::now() + Duration::from_secs(25);
    loop {
        let job = rig.job(number).await?;
        if job["state"] == "completed" {
            return Ok(job);
        }
        if job["state"] == "failed" || Instant::now() > until {
            return Err("worker did not complete owned job within bound".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
pub async fn attempt(rig: &mut Rig, number: i64) -> Result<Value> {
    Ok(rig
        .world
        .api(Call::get(
            ATTEMPT,
            format!("{}/hypotheses/{number}/attempts/1", rig.world.base()),
            &rig.actors.admin,
        ))
        .await?
        .body)
}
pub async fn started(rig: &Rig) -> Result<()> {
    let until = Instant::now() + Duration::from_secs(20);
    while !rig.work.0.join("experiment-started").exists() {
        if Instant::now() > until {
            return Err("experiment step did not start".into());
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    Ok(())
}
