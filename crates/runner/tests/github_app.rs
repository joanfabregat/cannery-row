#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Actual TCP requests and ring-verified signatures, with ephemeral RAM keys.
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

use cannery_runner::{
    cancellation::CancellationEvent,
    code_store::{CodeCancellation, GitHubSource},
    runtime::{
        RuntimeError,
        github::{
            Credentials, GitHub,
            app::{AppCredentials, Clock},
        },
    },
};
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Time(AtomicI64);
impl Clock for Time {
    fn seconds(&self) -> Result<i64, RuntimeError> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}
struct Keys {
    root: PathBuf,
    pem: PathBuf,
    public: Vec<u8>,
}
impl Keys {
    fn new() -> Self {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let root = std::env::temp_dir().join(format!(
            "cannery-app-test-{}",
            URL_SAFE_NO_PAD.encode(nonce)
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let pem = root.join("key.pem");
        let public = root.join("public.der");
        assert!(
            Command::new("openssl")
                .args([
                    "genpkey",
                    "-algorithm",
                    "RSA",
                    "-pkeyopt",
                    "rsa_keygen_bits:2048",
                    "-out"
                ])
                .arg(&pem)
                .output()
                .unwrap()
                .status
                .success()
        );
        std::fs::set_permissions(&pem, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            Command::new("openssl")
                .arg("rsa")
                .arg("-in")
                .arg(&pem)
                .args(["-RSAPublicKey_out", "-outform", "DER", "-out"])
                .arg(&public)
                .output()
                .unwrap()
                .status
                .success()
        );
        Self {
            root,
            pem,
            public: std::fs::read(public).unwrap(),
        }
    }
}
impl Drop for Keys {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
async fn request(stream: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        stream.read_exact(&mut byte).await.unwrap();
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(bytes).unwrap()
}
async fn reply(stream: &mut tokio::net::TcpStream, status: u16, body: &str, extra: &str) {
    stream.write_all(format!("HTTP/1.1 {status} response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",body.len()).as_bytes()).await.unwrap();
}
fn verify(request: &str, public: &[u8], now: i64) {
    let value = request
        .lines()
        .find_map(|line| line.strip_prefix("authorization: Bearer "))
        .unwrap();
    let parts: Vec<_> = value.split('.').collect();
    assert_eq!(parts.len(), 3);
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    assert_eq!(claims, json!({"iat":now-60,"exp":now+540,"iss":"123"}));
    assert_eq!(
        serde_json::from_slice::<Value>(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap(),
        json!({"alg":"RS256","typ":"JWT"})
    );
    UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, public)
        .verify(
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &URL_SAFE_NO_PAD.decode(parts[2]).unwrap(),
        )
        .unwrap();
}
async fn verify_commit(github: &GitHub) {
    github
        .verify_commit(
            &String::from("owner/repo"),
            &String::from("abc"),
            CodeCancellation::from_event(CancellationEvent::new()),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn signed_installations_cache_refresh_and_401() {
    let keys = Keys::new();
    let public = keys.public.clone();
    let clock = Arc::new(Time(AtomicI64::new(1_700_000_000)));
    let app = Arc::new(AppCredentials::with_clock("123", "456", &keys.pem, clock.clone()).unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let peer = tokio::spawn(async move {
        let mut tokens = 0;
        let mut gets = 0;
        for _ in 0..12 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let text = request(&mut stream).await;
            if text.starts_with("POST /app/installations/456/access_tokens ") {
                tokens += 1;
                let now = if tokens == 1 {
                    1_700_000_000
                } else {
                    1_700_003_300
                };
                verify(&text, &public, now);
                reply(&mut stream,201,&json!({"token":format!("installation-{tokens}"),"expires_at":chrono::DateTime::from_timestamp(now+3600,0).unwrap().to_rfc3339()}).to_string(),"").await;
            } else {
                gets += 1;
                let token = if gets <= 4 {
                    1
                } else if gets == 5 {
                    2
                } else {
                    3
                };
                assert!(text.contains(&format!("authorization: Bearer installation-{token}\r\n")));
                if gets == 5 {
                    reply(&mut stream, 401, "{}", "").await;
                } else {
                    reply(
                        &mut stream,
                        200,
                        if text.contains("/compare/") {
                            r#"{"status":"behind"}"#
                        } else {
                            r#"{"default_branch":"main"}"#
                        },
                        "",
                    )
                    .await;
                }
            }
        }
        (tokens, gets)
    });
    let github = GitHub::with_credentials(&api, Credentials::App(app)).unwrap();
    verify_commit(&github).await;
    verify_commit(&github).await;
    clock.0.store(1_700_003_300, Ordering::SeqCst);
    verify_commit(&github).await;
    verify_commit(&github).await;
    assert_eq!(peer.await.unwrap(), (3, 9));
}

#[tokio::test]
async fn app_failure_never_falls_back_and_redirect_has_no_bearer() {
    let keys = Keys::new();
    let clock = Arc::new(Time(AtomicI64::new(1_700_000_000)));
    let app = Arc::new(AppCredentials::with_clock("123", "456", &keys.pem, clock).unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let peer_api = api.clone();
    let public = keys.public.clone();
    let peer = tokio::spawn(async move {
        for index in 0..5 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let text = request(&mut stream).await;
            match index {
                0 => {
                    verify(&text, &public, 1_700_000_000);
                    reply(&mut stream, 403, "{}", "").await;
                }
                1 => {
                    verify(&text, &public, 1_700_000_000);
                    reply(
                        &mut stream,
                        201,
                        r#"{"token":"install","expires_at":"2023-11-14T23:13:20Z"}"#,
                        "",
                    )
                    .await;
                }
                2 => {
                    assert!(text.starts_with("GET /repos/owner/repo/tarball/abc "));
                    assert!(text.contains("authorization: Bearer install"));
                    reply(
                        &mut stream,
                        302,
                        "",
                        &format!("Location: {peer_api}/bucket\r\n"),
                    )
                    .await;
                }
                3 => {
                    assert!(text.starts_with("GET /bucket "));
                    assert!(!text.to_lowercase().contains("authorization:"));
                    reply(&mut stream, 200, "archive", "").await;
                }
                _ => unreachable!(),
            }
            if index == 3 {
                break;
            }
        }
    });
    let github = GitHub::with_credentials(&api, Credentials::App(app)).unwrap();
    let cancel = CodeCancellation::from_event(CancellationEvent::new());
    assert!(
        github
            .verify_commit(
                &String::from("owner/repo"),
                &String::from("abc"),
                cancel.clone()
            )
            .await
            .is_err()
    );
    let path = keys.root.join("archive");
    github
        .download_tarball(
            &String::from("owner/repo"),
            &String::from("abc"),
            path.clone(),
            100.into(),
            cancel,
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"archive");
    peer.await.unwrap();
}

#[test]
fn private_keys_and_provider_conflicts_fail_before_io() {
    let keys = Keys::new();
    assert!(AppCredentials::new("123", "456", &keys.pem).is_ok());
    std::fs::set_permissions(&keys.pem, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(AppCredentials::new("123", "456", &keys.pem).is_err());
    std::fs::set_permissions(&keys.pem, std::fs::Permissions::from_mode(0o600)).unwrap();
    let link = keys.root.join("link");
    std::os::unix::fs::symlink(&keys.pem, &link).unwrap();
    assert!(AppCredentials::new("123", "456", &link).is_err());
    assert!(AppCredentials::new("invalid", "456", &keys.pem).is_err());
    let settings = cannery_runner::config::GitHubSettings {
        token_file: Some(keys.pem.clone()),
        app_id: Some(String::from("123")),
        ..Default::default()
    };
    assert!(GitHub::from_settings(&settings).is_err());
    let pkcs1 = keys.root.join("pkcs1.pem");
    assert!(
        Command::new("openssl")
            .arg("rsa")
            .arg("-in")
            .arg(&keys.pem)
            .args(["-traditional", "-out"])
            .arg(&pkcs1)
            .output()
            .unwrap()
            .status
            .success()
    );
    std::fs::set_permissions(&pkcs1, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(AppCredentials::new("123", "456", &pkcs1).is_ok());
    std::fs::write(&pkcs1, "not a key").unwrap();
    assert!(AppCredentials::new("123", "456", &pkcs1).is_err());
}

#[tokio::test]
async fn cancelled_mint_settles_and_next_request_can_refresh() {
    let keys = Keys::new();
    let app = Arc::new(
        AppCredentials::with_clock(
            "123",
            "456",
            &keys.pem,
            Arc::new(Time(AtomicI64::new(1_700_000_000))),
        )
        .unwrap(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let github = Arc::new(
        GitHub::with_credentials(
            &format!("http://{}", listener.local_addr().unwrap()),
            Credentials::App(app),
        )
        .unwrap(),
    );
    let (ready, received) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        assert!(
            request(&mut stream)
                .await
                .starts_with("POST /app/installations/456/access_tokens ")
        );
        ready.send(()).unwrap();
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let text = request(&mut stream).await;
            if text.starts_with("POST ") {
                reply(
                    &mut stream,
                    201,
                    r#"{"token":"renewed","expires_at":"2023-11-14T23:13:20Z"}"#,
                    "",
                )
                .await;
            } else {
                assert!(text.contains("authorization: Bearer renewed"));
                reply(
                    &mut stream,
                    200,
                    if text.contains("/compare/") {
                        r#"{"status":"behind"}"#
                    } else {
                        r#"{"default_branch":"main"}"#
                    },
                    "",
                )
                .await;
            }
        }
    });
    let event = CancellationEvent::new();
    let task_event = event.clone();
    let task_github = github.clone();
    let operation = tokio::spawn(async move {
        task_github
            .verify_commit(
                &String::from("owner/repo"),
                &String::from("abc"),
                CodeCancellation::from_event(task_event),
            )
            .await
    });
    received.await.unwrap();
    event.set();
    assert!(matches!(
        operation.await.unwrap(),
        Err(cannery_runner::code_store::CodeError::Cancelled)
    ));
    verify_commit(&github).await;
    peer.await.unwrap();
}

#[tokio::test]
async fn concurrent_callers_share_one_mint() {
    let keys = Keys::new();
    let app = Arc::new(
        AppCredentials::with_clock(
            "123",
            "456",
            &keys.pem,
            Arc::new(Time(AtomicI64::new(1_700_000_000))),
        )
        .unwrap(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let github = GitHub::with_credentials(
        &format!("http://{}", listener.local_addr().unwrap()),
        Credentials::App(app),
    )
    .unwrap();
    let peer = tokio::spawn(async move {
        let mut mints = 0;
        for _ in 0..5 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let text = request(&mut stream).await;
            if text.starts_with("POST ") {
                mints += 1;
                reply(
                    &mut stream,
                    201,
                    r#"{"token":"shared","expires_at":"2023-11-14T23:13:20Z"}"#,
                    "",
                )
                .await;
            } else {
                assert!(text.contains("authorization: Bearer shared"));
                reply(
                    &mut stream,
                    200,
                    if text.contains("/compare/") {
                        r#"{"status":"behind"}"#
                    } else {
                        r#"{"default_branch":"main"}"#
                    },
                    "",
                )
                .await;
            }
        }
        mints
    });
    tokio::join!(verify_commit(&github), verify_commit(&github));
    assert_eq!(peer.await.unwrap(), 1);
}

#[tokio::test]
async fn malformed_installation_responses_fail_closed() {
    let keys = Keys::new();
    for body in [
        "{}",
        r#"{"token":"secret","expires_at":"invalid"}"#,
        r#"{"token":"secret","expires_at":"2023-11-14T22:13:20Z"}"#,
        r#"{"token":"bad\nheader","expires_at":"2023-11-14T23:13:20Z"}"#,
    ] {
        let app = Arc::new(
            AppCredentials::with_clock(
                "123",
                "456",
                &keys.pem,
                Arc::new(Time(AtomicI64::new(1_700_000_000))),
            )
            .unwrap(),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let github = GitHub::with_credentials(
            &format!("http://{}", listener.local_addr().unwrap()),
            Credentials::App(app),
        )
        .unwrap();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            assert!(request(&mut stream).await.starts_with("POST "));
            reply(&mut stream, 201, body, "").await;
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
        });
        assert!(
            github
                .verify_commit(
                    &String::from("owner/repo"),
                    &String::from("abc"),
                    CodeCancellation::from_event(CancellationEvent::new())
                )
                .await
                .is_err()
        );
        peer.await.unwrap();
    }
}
