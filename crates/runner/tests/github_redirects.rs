#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Actual TCP archive redirects retain no API credential or response cookies.

use cannery_runner::{
    cancellation::CancellationEvent,
    code_store::{CodeCancellation, GitHubSource},
    runtime::github::GitHub,
};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Mutex,
};

async fn run(location: &str, expected_requests: usize, succeeds: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let observed = seen.clone();
    let location = location.to_owned();
    let peer = tokio::spawn(async move {
        for index in 0..expected_requests {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            while !bytes.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).await.unwrap();
                bytes.push(byte[0]);
                assert!(bytes.len() < 16 * 1024);
            }
            let request = String::from_utf8(bytes).unwrap();
            assert!(!request.to_lowercase().contains("cookie:"));
            if index == 0 {
                assert!(request.contains("authorization: Bearer fixture-only"));
            } else {
                assert!(!request.to_lowercase().contains("authorization:"));
            }
            observed.lock().await.push(request);
            let reply = if location == "rate-long" {
                "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 61\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
            } else if succeeds && index == 2 {
                "HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\narchive"
                    .to_owned()
            } else {
                let target = if succeeds && index == 1 {
                    "/archive"
                } else {
                    &location
                };
                format!(
                    "HTTP/1.1 302 Found\r\nLocation: {target}\r\nSet-Cookie: never-forward=secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
            };
            stream.write_all(reply.as_bytes()).await.unwrap();
        }
    });
    // Tests in this binary run concurrently and the clock can repeat, so the
    // listener's port, held for the whole test, keeps the name unique.
    let root = std::env::temp_dir().join(format!("github-redirect-{}-{port}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let token = root.join("token");
    std::fs::write(&token, "fixture-only").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let github = GitHub::new(&api, Some(token)).unwrap();
    let output = root.join("archive");
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        github.download_tarball(
            &String::from("owner/repo"),
            &String::from("abc"),
            output.clone(),
            100.into(),
            CodeCancellation::from_event(CancellationEvent::new()),
        ),
    )
    .await
    .unwrap();
    peer.await.unwrap();
    assert_eq!(result.is_ok(), succeeds);
    assert_eq!(seen.lock().await.len(), expected_requests);
    if succeeds {
        assert_eq!(std::fs::read(output).unwrap(), b"archive");
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn two_redirects_download_without_credentials_or_cookies() {
    run("/download-start", 3, true).await;
}

#[tokio::test]
async fn unsafe_destinations_fail_before_request() {
    for target in [
        "file:///private",
        "http://user:password@127.0.0.1/private",
        "/archive#fragment",
    ] {
        run(target, 1, false).await;
    }
}

#[tokio::test]
async fn redirect_loop_has_a_finite_request_budget() {
    run("/loop", 11, false).await;
}

#[tokio::test]
async fn excessive_rate_limit_is_refused_without_sleep_or_retry() {
    run("rate-long", 1, false).await;
}
