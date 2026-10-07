//! Genuine stock-worker HTTP exchanges, independent of the API implementation.
#![forbid(unsafe_code)]
use cannery_core::{json as native_json, principal::Secret};
use cannery_runner::{
    cancellation::CancellationEvent,
    cli_depth::PolicyEntryPoint,
    policy,
    runtime::{evaluator::Evaluator, http::ApiClient},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpListener,
    sync::Arc,
    time::{Duration, Instant},
};
type Error = Box<dyn std::error::Error + Send + Sync>;
type PeerResult = (String, std::thread::JoinHandle<Result<Vec<Request>, Error>>);
struct Request {
    path: String,
    headers: BTreeMap<String, String>,
    body: Value,
}
struct Reply {
    status: u16,
    body: Value,
}
fn peer(replies: Vec<Reply>) -> Result<PeerResult, Error> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let root = format!("http://{}", listener.local_addr()?);
    let worker = std::thread::spawn(move || {
        let mut requests = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        for reply in replies {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() > deadline {
                            return Err("evaluator peer timed out".into());
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            stream.set_read_timeout(Some(Duration::from_secs(3)))?;
            let mut received = Vec::new();
            let (head, body) = loop {
                let mut chunk = [0u8; 4096];
                let count = stream.read(&mut chunk)?;
                if count == 0 || received.len() + count > 1 << 20 {
                    return Err("invalid evaluator peer request".into());
                }
                received.extend_from_slice(&chunk[..count]);
                let Some(split) = received.windows(4).position(|v| v == b"\r\n\r\n") else {
                    continue;
                };
                let head = std::str::from_utf8(&received[..split])?;
                let length = head
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .map(|(_, value)| value.trim().parse::<usize>())
                    .transpose()?
                    .unwrap_or(0);
                if received.len() >= split + 4 + length {
                    break (
                        head.to_owned(),
                        received[split + 4..split + 4 + length].to_vec(),
                    );
                }
            };
            let path = head
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .ok_or("request path missing")?
                .to_owned();
            let headers = head
                .lines()
                .filter_map(|line| line.split_once(':'))
                .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
                .collect();
            requests.push(Request {
                path,
                headers,
                body: if body.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&body)?
                },
            });
            let body = serde_json::to_vec(&reply.body)?;
            write!(
                stream,
                "HTTP/1.1 {} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                reply.status,
                body.len()
            )?;
            stream.write_all(&body)?;
        }
        Ok(requests)
    });
    Ok((root, worker))
}
fn document(value: &Value) -> Result<native_json::Document, Error> {
    Ok(native_json::decode(&serde_json::to_vec(value)?, 256)?)
}
fn policy() -> Result<Arc<policy::StockPolicy>, Error> {
    Ok(Arc::new(policy::parse_policy(
        Arc::new(document(
            &json!({"schema_version":"0.2","evaluator":{"id":"stock-evaluator","revision":"v1"},"gates":[{"id":"improvement","metric":"mrr","split":"dev","statistic":"value","compare":"control","op":">=","min_delta":0.02}],"baselines":[]}),
        )?),
        PolicyEntryPoint::Evaluator,
        256,
    )?))
}

#[test]
#[ignore = "Requires the actual installed cannery CLI"]
fn stock_commands_use_http_without_installation_or_launcher_settings() -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    let binary = std::env::var_os("CANNERY_EVALUATOR_BINARY").ok_or("CLI binary required")?;
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce)?;
    let directory = std::env::temp_dir().join(format!(
        "cr-evaluator-cli-{}",
        uuid::Uuid::from_bytes(nonce).simple()
    ));
    std::fs::create_dir(&directory)?;
    let result = (|| {
        let token = directory.join("token");
        std::fs::write(&token, "fixture-api")?;
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600))?;
        let policy = policy()?;
        let policy_path = directory.join("policy.json");
        std::fs::write(
            &policy_path,
            native_json::canonical::bytes(&policy.document, 256)?,
        )?;
        for configured_runner in [false, true] {
            let evidence = json!({"stage":"tester","provenance":{"source_revision":"commit","dataset_revision":"data"},"measurements":[{"metric":"mrr","split":"dev","value":0.42,"control_value":0.4,"authority":"tester_verified"}]});
            let hash = native_json::canonical::sha256(&document(&evidence)?, 256)?;
            let job = json!({"job_id":"00000000-0000-0000-0000-000000000123","attempt_id":"00000000-0000-0000-0000-000000000456","science_revision":1,"evaluator":{"id":"stock-evaluator","revision":"v1"},"lease":{"token":"fixture-lease","generation":3,"expires_at":(chrono::Utc::now()+chrono::Duration::seconds(30)).to_rfc3339()},"inputs":{"evidence":[{"ref":"evidence-id","sha256":hash}]}});
            let (root, worker) = peer(vec![
                Reply {
                    status: 201,
                    body: json!({"job":job,"heartbeat_seconds":10}),
                },
                Reply {
                    status: 200,
                    body: json!({"content":{"metrics":[{"key":"mrr","direction":"higher","splits":["dev"],"dimensions":[]}]}}),
                },
                Reply {
                    status: 200,
                    body: json!([evidence]),
                },
                Reply {
                    status: 200,
                    body: json!({"state":"completed"}),
                },
            ])?;
            let mut command = std::process::Command::new(&binary);
            command
                .env_remove("CANNERY_DATABASE_URL")
                .env_remove("CANNERY_SETTINGS");
            if configured_runner {
                let config = directory.join("runner.toml");
                std::fs::write(
                    &config,
                    format!(
                        "api_url = {root:?}\nproject = \"sample\"\n[[kinds]]\nkind = \"eval\"\npolicy = \"policy.json\"\ntoken_file = \"token\"\n"
                    ),
                )?;
                command
                    .arg("runner")
                    .arg("--config")
                    .arg(config)
                    .arg("--once");
            } else {
                command
                    .arg("evaluator")
                    .arg("--api-url")
                    .arg(&root)
                    .arg("--project")
                    .arg("sample")
                    .arg("--token-file")
                    .arg(&token)
                    .arg("--config")
                    .arg(&policy_path)
                    .arg("--once");
            }
            let output = command.output()?;
            let requests = worker.join().map_err(|_| "CLI HTTP peer panicked")??;
            assert!(output.status.success(), "installed stock command failed");
            assert!(std::str::from_utf8(&output.stdout)?.contains("completed"));
            assert_eq!(requests.len(), 4);
            assert_eq!(
                requests[3].body["evidence"]["assessment"]["verdict"],
                "pass"
            );
            assert!(requests[3].path.ends_with("/completion"));
        }
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644))?;
        assert_private_token_refusal(&binary, &token, &policy_path)?;
        Ok::<(), Error>(())
    })();
    std::fs::remove_dir_all(directory)?;
    result
}
fn assert_private_token_refusal(
    binary: &std::ffi::OsStr,
    token: &std::path::Path,
    policy_path: &std::path::Path,
) -> Result<(), Error> {
    let refused = std::process::Command::new(binary)
        .args([
            "evaluator",
            "--api-url",
            "http://127.0.0.1:1",
            "--project",
            "sample",
        ])
        .arg("--token-file")
        .arg(token)
        .arg("--config")
        .arg(policy_path)
        .arg("--once")
        .output()?;
    assert_eq!(refused.status.code(), Some(2));
    Ok(())
}
#[tokio::test]
async fn stock_worker_verifies_pins_and_reports_actual_exact_assessment() -> Result<(), Error> {
    for tamper in [false, true] {
        let evidence = json!({"stage":"tester","provenance":{"source_revision":"commit","dataset_revision":"data"},"measurements":[{"metric":"mrr","split":"dev","value":0.42,"control_value":0.4,"authority":"tester_verified"}]});
        let hash = native_json::canonical::sha256(&document(&evidence)?, 256)?;
        let id = "00000000-0000-0000-0000-000000000123";
        let expiry = (chrono::Utc::now() + chrono::Duration::seconds(30)).to_rfc3339();
        let job = json!({"job_id":id,"attempt_id":"00000000-0000-0000-0000-000000000456","science_revision":1,"evaluator":{"id":"stock-evaluator","revision":"v1"},"lease":{"token":"fixture-lease","generation":3,"expires_at":expiry},"inputs":{"evidence":[{"ref":"evidence-id","sha256":hash}]}});
        let mut served = evidence;
        if tamper {
            served["measurements"][0]["value"] = json!(0.8);
        }
        let (root, worker) = peer(vec![
            Reply {
                status: 201,
                body: json!({"job":job,"heartbeat_seconds":10}),
            },
            Reply {
                status: 200,
                body: json!({"content":{"metrics":[{"key":"mrr","direction":"higher","splits":["dev"],"dimensions":[]}]}}),
            },
            Reply {
                status: 200,
                body: json!([served]),
            },
            Reply {
                status: 200,
                body: json!({"state":if tamper {"failed"} else {"completed"}}),
            },
        ])?;
        let evaluator = Evaluator {
            client: ApiClient::new(&root)?,
            token: Secret::new("fixture-api".to_owned()),
            project: "sample".to_owned(),
            policy: policy()?,
        };
        let result = evaluator
            .run_once(CancellationEvent::new())
            .await?
            .ok_or("worker was idle")?;
        assert_eq!(result.state, if tamper { "failed" } else { "completed" });
        let requests = worker.join().map_err(|_| "evaluator peer panicked")??;
        assert_eq!(requests.len(), 4);
        assert_eq!(
            requests[0].body,
            json!({"stage":"evaluator","revision":"v1"})
        );
        assert!(
            requests
                .iter()
                .all(|r| r.headers.get("authorization").map(String::as_str)
                    == Some("Bearer fixture-api"))
        );
        assert!(!requests[1].headers.contains_key("x-lease-token"));
        assert_eq!(
            requests[2].headers.get("x-lease-token").map(String::as_str),
            Some("fixture-lease")
        );
        if tamper {
            assert!(requests[3].path.ends_with("/failure"));
            assert_eq!(requests[3].body["error_code"], "input_verification_failed");
        } else {
            assert!(requests[3].path.ends_with("/completion"));
            assert_eq!(
                requests[3]
                    .headers
                    .get("idempotency-key")
                    .map(String::as_str),
                Some("evaluate-00000000-0000-0000-0000-000000000123-3")
            );
            assert_eq!(
                requests[3].body["evidence"]["assessment"]["verdict"],
                "pass"
            );
            assert_eq!(
                requests[3].body["evidence"]["assessment"]["gates"][0]["result"],
                "pass"
            );
            assert_eq!(
                requests[3].body["evidence"]["assessment"]["evidence"],
                job["inputs"]["evidence"]
            );
            assert_eq!(
                requests[3].body["evidence"]["provenance"]["science_revision"],
                "1"
            );
        }
    }
    Ok(())
}
