#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Real HTTP branch scans; a hostile Link header never supplies request URLs.

use cannery_runner::{
    cancellation::CancellationEvent,
    code_store::{CodeCancellation, CodeError, GitHubSource},
    runtime::github::GitHub,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{Mutex, Notify},
    task::JoinHandle,
};

#[derive(Clone, Copy)]
enum Scenario {
    Branch101,
    DefaultReachable,
    FirstBranchReachable,
    Empty,
    Endless,
    Malformed,
    Oversized,
    CancelSecondPage,
}

struct Peer {
    api: String,
    requests: Arc<Mutex<Vec<String>>>,
    second_page: Arc<Notify>,
    task: JoinHandle<()>,
}
impl Peer {
    async fn new(scenario: Scenario) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}/api", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = requests.clone();
        let second_page = Arc::new(Notify::new());
        let second_seen = second_page.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).await.unwrap();
                    bytes.push(byte[0]);
                    assert!(bytes.len() < 16 * 1024);
                }
                let request = String::from_utf8(bytes).unwrap();
                let path = request.split_whitespace().nth(1).unwrap().to_owned();
                observed.lock().await.push(path.clone());
                assert!(path.starts_with("/api/repos/owner/repo"));
                let body = if path == "/api/repos/owner/repo" {
                    json!({"default_branch":"main"})
                } else if path.contains("/compare/") {
                    let reachable = matches!(scenario, Scenario::DefaultReachable)
                        || (matches!(scenario, Scenario::Branch101)
                            && path.contains("/branch101...target"))
                        || (matches!(scenario, Scenario::FirstBranchReachable)
                            && path.contains("/branch1...target"));
                    json!({"status":if reachable {"behind"} else {"ahead"}})
                } else {
                    assert!(path.contains("/branches?per_page=100&page="));
                    if path.ends_with("page=2")
                        && matches!(scenario, Scenario::Branch101 | Scenario::CancelSecondPage)
                    {
                        second_seen.notify_one();
                        if matches!(scenario, Scenario::CancelSecondPage) {
                            std::future::pending::<()>().await;
                        }
                        json!([{"name":"branch101","commit":{"sha":"branch101"}}])
                    } else {
                        match scenario {
                            Scenario::Branch101 | Scenario::CancelSecondPage => {
                                branches(100, false)
                            }
                            Scenario::FirstBranchReachable => branches(3, false),
                            Scenario::Empty => json!([]),
                            Scenario::Endless => branches(100, true),
                            Scenario::Malformed => json!({"branches":[]}),
                            Scenario::Oversized => branches(101, true),
                            Scenario::DefaultReachable => panic!("unexpected branch request"),
                        }
                    }
                };
                let body = body.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nLink: <http://127.0.0.1:9/foreign>; rel=\"next\"\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        Self {
            api,
            requests,
            second_page,
            task,
        }
    }
    async fn paths(&self) -> Vec<String> {
        self.requests.lock().await.clone()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn branches(count: usize, default_only: bool) -> Value {
    json!((1..=count).map(|number| {
        json!({"name":if default_only {"main".to_owned()} else {format!("branch{number}")},"commit":{"sha":format!("branch{number}")}})
    }).collect::<Vec<_>>())
}
async fn verify(peer: &Peer, event: CancellationEvent) -> Result<(), CodeError> {
    let github = GitHub::new(&peer.api, None).unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        github.verify_commit(
            &String::from("owner/repo"),
            &String::from("target"),
            CodeCancellation::from_event(event),
        ),
    )
    .await
    .expect("bounded GitHub scan")
}

#[tokio::test]
async fn reachable_commit_on_branch_101_uses_fixed_origin_pages() {
    let peer = Peer::new(Scenario::Branch101).await;
    verify(&peer, CancellationEvent::new()).await.unwrap();
    let paths = peer.paths().await;
    assert_eq!(paths.len(), 105);
    assert_eq!(
        paths[2],
        "/api/repos/owner/repo/branches?per_page=100&page=1"
    );
    assert_eq!(
        paths[103],
        "/api/repos/owner/repo/branches?per_page=100&page=2"
    );
    assert_eq!(
        paths[104],
        "/api/repos/owner/repo/compare/branch101...target?per_page=1"
    );
}

#[tokio::test]
async fn earlier_reachability_and_short_pages_stop_scanning() {
    let default = Peer::new(Scenario::DefaultReachable).await;
    verify(&default, CancellationEvent::new()).await.unwrap();
    assert_eq!(default.paths().await.len(), 2);
    let first = Peer::new(Scenario::FirstBranchReachable).await;
    verify(&first, CancellationEvent::new()).await.unwrap();
    assert_eq!(first.paths().await.len(), 4);
    let empty = Peer::new(Scenario::Empty).await;
    assert_eq!(
        verify(&empty, CancellationEvent::new()).await,
        Err(CodeError::GitHub)
    );
    assert_eq!(empty.paths().await.len(), 3);
}

#[tokio::test]
async fn malformed_and_oversized_pages_fail_without_further_requests() {
    for scenario in [Scenario::Malformed, Scenario::Oversized] {
        let peer = Peer::new(scenario).await;
        assert_eq!(
            verify(&peer, CancellationEvent::new()).await,
            Err(CodeError::GitHub)
        );
        assert_eq!(peer.paths().await.len(), 3);
    }
}

#[tokio::test]
async fn repeated_full_pages_terminate_at_the_explicit_scan_bound() {
    let peer = Peer::new(Scenario::Endless).await;
    assert_eq!(
        verify(&peer, CancellationEvent::new()).await,
        Err(CodeError::GitHub)
    );
    let paths = peer.paths().await;
    assert_eq!(paths.len(), 102);
    assert_eq!(
        paths.last().unwrap(),
        "/api/repos/owner/repo/branches?per_page=100&page=100"
    );
}

#[tokio::test]
async fn cancellation_during_a_later_page_stops_the_request() {
    let peer = Peer::new(Scenario::CancelSecondPage).await;
    let event = CancellationEvent::new();
    let operation = verify(&peer, event.clone());
    let signal = async {
        tokio::time::timeout(Duration::from_secs(10), peer.second_page.notified())
            .await
            .expect("second page request observed");
        event.set();
    };
    let (result, ()) = tokio::join!(operation, signal);
    assert_eq!(result, Err(CodeError::Cancelled));
    assert_eq!(peer.paths().await.len(), 104);
}
