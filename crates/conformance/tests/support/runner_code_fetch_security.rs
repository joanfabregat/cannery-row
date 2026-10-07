use super::runner::{COMMIT_A, REPOSITORY, Rig};
use axum::{extract::State, response::IntoResponse};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use conformance::Result;
use reqwest::{StatusCode, header};
use serde_json::json;
use std::{
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

pub const STATIC_TOKEN: &str = "conformance-code-fetch-private-token";
pub const APP_TOKEN: &str = "conformance-code-fetch-installation-token";
#[derive(Clone)]
struct MockState {
    mode: String,
    base: String,
    archive: Vec<u8>,
    public: Vec<u8>,
    requests: Arc<AtomicUsize>,
    downloads: Arc<AtomicUsize>,
    mints: Arc<AtomicUsize>,
    metadata: Arc<AtomicUsize>,
    invalid_auth: Arc<AtomicBool>,
}
pub struct Mock {
    pub base: String,
    state: MockState,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Mock {
    pub async fn new(mode: &str, archive: Vec<u8>, public: Vec<u8>) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let state = MockState {
            mode: mode.into(),
            base: base.clone(),
            archive,
            public,
            requests: Arc::default(),
            downloads: Arc::default(),
            mints: Arc::default(),
            metadata: Arc::default(),
            invalid_auth: Arc::default(),
        };
        let router = axum::Router::new()
            .fallback(axum::routing::any(route))
            .with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, router).await });
        Ok(Self { base, state, task })
    }
    pub fn configure(&self, rig: &mut Rig, app: bool) -> Result<()> {
        rig.cli_extra = vec![
            "--github-api-url".into(),
            self.base.clone(),
            "--github-allowed-repos".into(),
            REPOSITORY.into(),
        ];
        if app {
            rig.cli_extra.extend([
                "--github-app-id".into(),
                "4242".into(),
                "--github-app-key-file".into(),
                rig.work.0.join("app.pem").display().to_string(),
                "--github-app-installation-id".into(),
                "77".into(),
            ]);
        } else {
            let path = rig.work.0.join("github-security.token");
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?
                .write_all(STATIC_TOKEN.as_bytes())?;
            rig.cli_extra
                .extend(["--github-token-file".into(), path.display().to_string()]);
        }
        Ok(())
    }
    pub fn requests(&self) -> usize {
        self.state.requests.load(Ordering::SeqCst)
    }
    pub fn downloads(&self) -> usize {
        self.state.downloads.load(Ordering::SeqCst)
    }
    pub fn mints(&self) -> usize {
        self.state.mints.load(Ordering::SeqCst)
    }
    pub fn metadata(&self) -> usize {
        self.state.metadata.load(Ordering::SeqCst)
    }
    pub fn assert_credentials(&self) {
        assert!(
            !self.state.invalid_auth.load(Ordering::SeqCst),
            "mock observed incorrect credential scope/signature"
        );
    }
}
#[allow(
    clippy::too_many_lines,
    reason = "One controlled provider routes the source-defined failure profiles"
)]
async fn route(
    State(state): State<MockState>,
    request: axum::extract::Request,
) -> axum::response::Response {
    state.requests.fetch_add(1, Ordering::SeqCst);
    let path = request.uri().path();
    let auth = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    if path == "/archive" || path == "/download-start" {
        if auth.is_some() {
            state.invalid_auth.store(true, Ordering::SeqCst);
        }
        if path == "/download-start" {
            return (
                StatusCode::FOUND,
                [(header::LOCATION, format!("{}/archive", state.base))],
            )
                .into_response();
        }
        state.downloads.fetch_add(1, Ordering::SeqCst);
        if state.mode == "download-missing" {
            return StatusCode::NOT_FOUND.into_response();
        }
        return (
            [(header::CONTENT_TYPE, "application/gzip")],
            state.archive.clone(),
        )
            .into_response();
    }
    if path == "/app/installations/77/access_tokens" {
        state.mints.fetch_add(1, Ordering::SeqCst);
        if !valid_jwt(auth, &state.public) {
            state.invalid_auth.store(true, Ordering::SeqCst);
        }
        if state.mode == "app-refused" {
            return (
                StatusCode::FORBIDDEN,
                axum::Json(json!({"message":"synthetic refused installation"})),
            )
                .into_response();
        }
        if state.mode == "app-no-expiry" {
            return (StatusCode::CREATED, axum::Json(json!({"token":APP_TOKEN}))).into_response();
        }
        let expiry = if state.mode == "app-bad-expiry" {
            "not-a-time"
        } else {
            "2099-01-01T00:00:00Z"
        };
        return (
            StatusCode::CREATED,
            axum::Json(json!({"token":APP_TOKEN,"expires_at":expiry})),
        )
            .into_response();
    }
    let expected = if state.mode.starts_with("app-") {
        format!("Bearer {APP_TOKEN}")
    } else {
        format!("Bearer {STATIC_TOKEN}")
    };
    if auth != Some(expected.as_str()) {
        state.invalid_auth.store(true, Ordering::SeqCst);
    }
    if path == format!("/repos/{REPOSITORY}") {
        let number = state.metadata.fetch_add(1, Ordering::SeqCst);
        match state.mode.as_str() {
            "missing" => return StatusCode::NOT_FOUND.into_response(),
            "app-remint" if number == 0 => return StatusCode::UNAUTHORIZED.into_response(),
            "app-always-401" => return StatusCode::UNAUTHORIZED.into_response(),
            "rate-once" if number == 0 => {
                return (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "1")])
                    .into_response();
            }
            "rate-reset" if number == 0 => {
                let reset = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |time| time.as_secs())
                    + 1;
                return (
                    StatusCode::FORBIDDEN,
                    [
                        ("x-ratelimit-remaining", "0".to_owned()),
                        ("x-ratelimit-reset", reset.to_string()),
                    ],
                )
                    .into_response();
            }
            "plain-forbidden" => return StatusCode::FORBIDDEN.into_response(),
            "rate-repeat" => {
                return (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "0")])
                    .into_response();
            }
            "rate-long" => {
                return (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "61")])
                    .into_response();
            }
            _ => {}
        }
        return axum::Json(json!({"default_branch":"main"})).into_response();
    }
    if path.contains("/compare/") {
        if state.mode == "invalid-revision" {
            return StatusCode::UNPROCESSABLE_ENTITY.into_response();
        }
        let reachable = state.mode != "fork"
            && (state.mode != "alternate-branch" || path.contains("tip-feature"));
        return axum::Json(json!({"status":if reachable {"identical"} else {"ahead"}}))
            .into_response();
    }
    if path.ends_with("/branches") {
        return axum::Json(if state.mode == "alternate-branch" {json!([{"name":"main","commit":{"sha":"tip-main"}},{"name":"feature/x","commit":{"sha":"tip-feature"}}])} else {json!([])}).into_response();
    }
    if path.contains("/tarball/") {
        if state.mode == "missing-redirect" {
            return StatusCode::FOUND.into_response();
        }
        if state.mode == "direct" {
            state.downloads.fetch_add(1, Ordering::SeqCst);
            return (
                [(header::CONTENT_TYPE, "application/gzip")],
                state.archive.clone(),
            )
                .into_response();
        }
        return (
            StatusCode::FOUND,
            [(header::LOCATION, format!("{}/download-start", state.base))],
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
    let message = format!("{}.{}", parts[0], parts[1]);
    if ring::signature::UnparsedPublicKey::new(&ring::signature::RSA_PKCS1_2048_8192_SHA256, key)
        .verify(message.as_bytes(), &signature)
        .is_err()
    {
        return false;
    }
    let Ok(bytes) = URL_SAFE_NO_PAD.decode(parts[1]) else {
        return false;
    };
    let Ok(claims) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    claims["iss"] == "4242"
        && claims["exp"]
            .as_i64()
            .zip(claims["iat"].as_i64())
            .is_some_and(|(exp, iat)| exp - iat == 600)
}
pub async fn archive(rig: &Rig, variant: &str, app: bool) -> Result<(Vec<u8>, Vec<u8>)> {
    let script = rig.work.0.join("package_security.py");
    fs::write(&script, PACKAGER)?;
    let root = rig.work.0.clone();
    let variant = variant.to_owned();
    let output = tokio::task::spawn_blocking(move || {
        Command::new("uv")
            .args(["run", "--frozen", "--offline", "python"])
            .arg(script)
            .arg(&root)
            .arg(variant)
            .arg(if app { "app" } else { "static" })
            .output()
    })
    .await??;
    assert!(output.status.success(), "trusted fixture packaging failed");
    let public = if app {
        fs::read(rig.work.0.join("app-public.der"))?
    } else {
        Vec::new()
    };
    Ok((fs::read(rig.work.0.join("security.tar.gz"))?, public))
}
const PACKAGER: &str = r"import io
import sys
import tarfile
from pathlib import Path

def main() -> None:
    root = Path(sys.argv[1])
    variant = sys.argv[2]
    if sys.argv[3] == 'app':
        from cryptography.hazmat.primitives import serialization
        from cryptography.hazmat.primitives.asymmetric import rsa
        key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        private = root / 'app.pem'
        private.write_bytes(key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()))
        private.chmod(0o600)
        (root / 'app-public.der').write_bytes(key.public_key().public_bytes(serialization.Encoding.DER, serialization.PublicFormat.PKCS1))
    destination = root / 'security.tar.gz'
    if variant == 'garbage':
        destination.write_bytes(b'not a tarball')
        return
    comment = 'a' * 40 if variant == 'wrong-commit' else '0' * 40
    headers = {} if variant == 'no-commit' else {'comment': comment}
    with tarfile.open(destination, 'w:gz', format=tarfile.PAX_FORMAT, pax_headers=headers) as archive:
        def add(name: str, kind: bytes = tarfile.REGTYPE, link: str = '', data: bytes = b'') -> None:
            member = tarfile.TarInfo(name)
            member.type = kind
            member.linkname = link
            member.mode = 0o755 if kind == tarfile.DIRTYPE else 0o644
            member.size = len(data) if kind == tarfile.REGTYPE else 0
            archive.addfile(member, io.BytesIO(data) if kind == tarfile.REGTYPE else None)
        add('owner-repo', tarfile.DIRTYPE)
        add('owner-repo/steps', tarfile.DIRTYPE)
        for source in sorted((root / 'steps').iterdir()):
            if source.is_file() and source.name != 'requirements.txt':
                add('owner-repo/steps/' + source.name, data=source.read_bytes())
        add('owner-repo/steps/requirements.txt', data=b'dep==1.0\n')
        cases = {
            'dotdot': ('owner-repo/../escape.txt', tarfile.REGTYPE, ''),
            'absolute': (str(root / 'outside-sentinel'), tarfile.REGTYPE, ''),
            'other-top': ('other-tree/out', tarfile.REGTYPE, ''),
            'symlink-out': ('owner-repo/out', tarfile.SYMTYPE, '../../escape.txt'),
            'symlink-absolute': ('owner-repo/out', tarfile.SYMTYPE, str(root / 'outside-sentinel')),
            'symlink-parent': ('owner-repo/out', tarfile.SYMTYPE, '..'),
            'hardlink': ('owner-repo/out', tarfile.LNKTYPE, 'owner-repo/steps/requirements.txt'),
            'char-device': ('owner-repo/out', tarfile.CHRTYPE, ''),
            'block-device': ('owner-repo/out', tarfile.BLKTYPE, ''),
            'fifo': ('owner-repo/out', tarfile.FIFOTYPE, ''),
            'duplicate': ('owner-repo/steps/requirements.txt', tarfile.REGTYPE, ''),
            'orphan': ('owner-repo/absent/child', tarfile.REGTYPE, ''),
        }
        if variant in cases:
            name, kind, link = cases[variant]
            add(name, kind, link, b'private-archive-payload')
        if variant == 'link-parent':
            add('owner-repo/link', tarfile.SYMTYPE, 'steps/requirements.txt')
            add('owner-repo/link/child', data=b'private-archive-payload')
        if variant == 'link-chain':
            add('owner-repo/a', tarfile.DIRTYPE)
            add('owner-repo/a/b', tarfile.DIRTYPE)
            add('owner-repo/a/b/s', tarfile.SYMTYPE, '../..')
            add('owner-repo/x', tarfile.SYMTYPE, 'a/b/s/../..')
    (root / 'outside-sentinel').write_text('untouched')

if __name__ == '__main__':
    main()
";
pub fn assert_redacted(rig: &Rig, output: &std::process::Output, job: &serde_json::Value) {
    let text = format!(
        "{}{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        job
    );
    for secret in [
        STATIC_TOKEN,
        APP_TOKEN,
        &rig.actors.tester,
        &rig.actors.agent,
        &rig.actors.admin,
        "private-archive-payload",
        "BEGIN PRIVATE KEY",
    ] {
        assert!(
            !text.contains(secret),
            "diagnostic exposed private material"
        );
    }
}
pub fn assert_no_publication(rig: &Rig) -> Result<()> {
    assert!(
        !rig.work
            .0
            .join("cache/code")
            .join(REPOSITORY)
            .join(COMMIT_A)
            .exists()
    );
    assert_eq!(super::runner::entries(&rig.work.0.join("cache/tmp"))?, 0);
    assert_eq!(super::runner::entries(&rig.work.0.join("cache/setup"))?, 0);
    rig.assert_clean()
}
