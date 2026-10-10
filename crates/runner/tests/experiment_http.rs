//! Real HTTP and local process proof; no live API, cloud, daemon or credentials.
#![forbid(unsafe_code)]
use cannery_core::principal::Secret;
use cannery_runner::{
    cancellation::CancellationEvent,
    runtime::{
        FutureResult, RuntimeError,
        backend::LocalBackend,
        experiment::Experiment,
        http::ApiClient,
        worker::{NoCode, OutputValidator, Resources},
    },
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
type Error = Box<dyn std::error::Error + Send + Sync>;
const ID: &str = "00000000-0000-0000-0000-000000000002";

#[test]
#[ignore = "requires positively selected installed native CLI"]
fn installed_experiment_command_runs_claimed_workflow() -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    let binary = PathBuf::from(std::env::var("CANNERY_NATIVE_CLI")?).canonicalize()?;
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce)?;
    let directory = std::env::temp_dir().join(format!(
        "cr-experiment-cli-{}",
        uuid::Uuid::from_bytes(nonce).simple()
    ));
    std::fs::create_dir(&directory)?;
    let result = (|| {
        let token = directory.join("token");
        std::fs::write(&token, "synthetic-experimenter")?;
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600))?;
        for case in [Case::Success, Case::StepFailed] {
            let peer = peer(case)?;
            let config = directory.join("runner.toml");
            std::fs::write(
                &config,
                format!(
                    "api_url = {:?}\nproject = \"fixture\"\ndata_root = {:?}\nwork_root = {:?}\n[launcher]\ntype = \"local\"\nstep_root = {:?}\n[[kinds]]\nkind = \"experiment\"\ntoken_file = {:?}\n",
                    peer.root,
                    directory.to_string_lossy(),
                    directory.join("work").to_string_lossy(),
                    directory.join("steps").to_string_lossy(),
                    token.to_string_lossy()
                ),
            )?;
            let mut child = std::process::Command::new(&binary)
                .args(["runner", "--config"])
                .arg(&config)
                .arg("--once")
                .env_remove("CANNERY_DATABASE_URL")
                .env_remove("CANNERY_SETTINGS")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()?;
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while child.try_wait()?.is_none() {
                if std::time::Instant::now() >= deadline {
                    child.kill()?;
                    child.wait()?;
                    return Err("installed experiment command timed out".into());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let output = child.wait_with_output()?;
            assert!(
                output.status.success(),
                "installed experiment command failed"
            );
            assert!(
                std::str::from_utf8(&output.stdout)?.contains(if matches!(case, Case::Success) {
                    "testing"
                } else {
                    "failed"
                }),
                "installed command result differs: stdout={} stderr={} requests={:?}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
                peer.requests.lock().map_err(|_| "peer mutex")?
            );
            let requests = peer.requests.lock().map_err(|_| "peer mutex")?;
            assert_eq!(requests[0].body, json!({"mode":"workflow"}));
            assert!(requests.iter().any(|request| request.path.ends_with(
                if matches!(case, Case::Success) {
                    "/submission"
                } else {
                    "/release"
                }
            )));
            assert_eq!(std::fs::read_dir(directory.join("work"))?.count(), 0);
        }
        Ok::<(), Error>(())
    })();
    std::fs::remove_dir_all(&directory)?;
    result
}
#[derive(Clone, Copy, Debug)]
enum Case {
    PinnedInputs,
    SubmissionRetry,
    Success,
    StepFailed,
    Concern,
    Invalid,
    Tamper,
    LinkTamper,
    Predecessor,
    BadPredecessor,
    LeaseLost,
    Cancel,
    Idle,
    Conflict,
    ManifestTamper,
    Grant422,
    Grant401,
    Grant403,
    Grant409,
}
#[derive(Debug)]
struct Request {
    path: String,
    headers: BTreeMap<String, String>,
    body: Value,
}
struct Peer {
    root: String,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<Request>>>,
    thread: Option<std::thread::JoinHandle<Result<(), Error>>>,
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn step(name: &str, script: &str, output: &str, inputs: &Value) -> Value {
    json!({"name":name,"revision":1,"manifest":{"apiVersion":"cannery-row/v1","kind":"Step","metadata":{"name":name},"spec":{"role":"experiment","container":{"image":format!("fixture.invalid/step@sha256:{}", "a".repeat(64)),"command":["/bin/sh","-c",script],"env":[],"resources":{"limits":{}}},"activeDeadlineSeconds":120,"network":"none","sandbox":"trusted fixture","inputs":{"artifacts":inputs},"outputs":{"artifacts":[{"name":output,"path":format!("/cr/outputs/{output}")}]}}}})
}
fn claim(case: Case) -> Value {
    let script = match case {
        Case::StepFailed => "echo observed-failure; exit 7",
        Case::Concern => {
            "mkdir -p \"$CR_ROOT/outputs/concern\"; printf '%s\\n' --- 'kind: blocker' --- 'The plan assumes a GPU.' > \"$CR_ROOT/outputs/concern/concern.md\"; exit 7"
        }
        Case::Invalid => "printf '[]' > \"$CR_ROOT/outputs/run/run.md\"",
        Case::LeaseLost | Case::Cancel => {
            "sleep 30 & child=$!; printf '%s %s' \"$$\" \"$child\" > \"$CR_ROOT/../../../pids\"; wait; printf '%s\\n' --- --- > \"$CR_ROOT/outputs/run/run.md\""
        }
        Case::PinnedInputs => {
            "test \"$(cat \"$CR_ROOT/inputs/data/pin.txt\")\" = data && test \"$(cat \"$CR_ROOT/inputs/base/pin.txt\")\" = baseline || exit 9; printf '%s\\n' --- --- > \"$CR_ROOT/outputs/run/run.md\""
        }
        Case::Predecessor | Case::BadPredecessor => {
            "test \"$(cat \"$CR_ROOT/inputs/previous/previous.txt\")\" = predecessor || exit 9; printf '%s\\n' --- --- > \"$CR_ROOT/outputs/run/run.md\""
        }
        _ => {
            "test -z \"$TOKEN$LEASE$GITHUB_TOKEN\" || exit 9; grep -q 'parameters' \"$CR_ROOT/job.json\" || exit 9; printf '%s\\n' --- 'provenance: {custom: true}' --- 'Notes stay.' > \"$CR_ROOT/outputs/run/run.md\""
        }
    };
    let previous = matches!(case, Case::Predecessor | Case::BadPredecessor);
    let inputs = if matches!(case, Case::PinnedInputs) {
        json!([{"name":"data","from":"dataset","path":"/cr/inputs/data"},{"name":"base","from":"baseline","path":"/cr/inputs/base"}])
    } else if previous {
        json!([{"name":"previous","from":"attempt","path":"/cr/inputs/previous"}])
    } else {
        json!([])
    };
    let mut steps = vec![step("agent", script, "run", &inputs)];
    if matches!(case, Case::Tamper) {
        steps.push(step("tamper", "printf '%s\\n' --- --- > \"$CR_ROOT/../0-agent/outputs/run/run.md\"; printf artifact > \"$CR_ROOT/outputs/artifact/out.txt\"", "artifact", &json!([])));
    }
    if matches!(case, Case::LinkTamper) {
        steps.push(step("tamper", "mkdir -p \"$CR_ROOT/outputs/run\"; cp \"$CR_ROOT/../0-agent/outputs/run/run.md\" \"$CR_ROOT/outputs/run/run.md\"; rm -r \"$CR_ROOT/../0-agent/outputs\"; ln -s \"$CR_ROOT/outputs\" \"$CR_ROOT/../0-agent/outputs\"; printf artifact > \"$CR_ROOT/outputs/artifact/out.txt\"", "artifact", &json!([])));
    }
    let predecessor = if previous {
        json!({"attempt_id":ID,"ref":"h1/a0","state":"rejected","failure_code":null,"artifacts":[{"id":"00000000-0000-0000-0000-000000000003","role":"previous","storage":{"key":"prefix/previous.txt"},"size_bytes":11,"sha256":format!("{:x}", Sha256::digest(b"predecessor"))}]})
    } else {
        Value::Null
    };
    json!({"attempt":{"id":ID,"number":1,"sequence":1,"ref":"h1/a1"},"lease_token":"synthetic-attempt-lease","lease_generation":1,"lease_expires_at":(chrono::Utc::now()+chrono::Duration::seconds(120)).to_rfc3339(),"heartbeat_seconds":if matches!(case, Case::LeaseLost | Case::Cancel) {0.15} else {60.0},"workflow":{"track":"workflow","science_revision":1,"deadline":(chrono::Utc::now()+chrono::Duration::seconds(300)).to_rfc3339(),"parameters":{"alpha":2},"steps":steps,"inputs":{"datasets":[{"id":"data","revision":"v1"}],"baselines":[{"id":"base","revision":"v2"}],"predecessor":predecessor}}})
}
#[allow(clippy::too_many_lines)] // Bounded deterministic protocol peer, including capability body bytes.
fn peer(case: Case) -> Result<Peer, Error> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let root = format!("http://{}", listener.local_addr()?);
    let stop = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let thread = {
        let stop = stop.clone();
        let requests = requests.clone();
        std::thread::spawn(move || {
            let mut grants: Vec<Value> = vec![];
            let mut submission_replies = 0;
            let mut release_replies = 0;
            while !stop.load(Ordering::SeqCst) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                };
                stream.set_read_timeout(Some(Duration::from_secs(3)))?;
                let mut received = Vec::new();
                let (head, bytes) = loop {
                    let mut chunk = [0; 4096];
                    let count = stream.read(&mut chunk)?;
                    if count == 0 || received.len() + count > 1 << 20 {
                        return Err("invalid bounded request".into());
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
                        .map(|(_, v)| v.trim().parse::<usize>())
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
                    .ok_or("missing path")?
                    .to_owned();
                let headers: BTreeMap<_, _> = head
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
                    .collect();
                let body = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
                let mut status = 200;
                let reply = if path.ends_with("/claims") {
                    if matches!(case, Case::Idle | Case::Conflict) {
                        status = 409;
                        json!({"error":{"code":if matches!(case, Case::Idle) {"workflow_unavailable"} else {"other_conflict"}}})
                    } else {
                        status = 201;
                        claim(case)
                    }
                } else if path.ends_with("/config/science/1") {
                    json!({"content":{"validators":[],"datasets":[{"id":"data","held_out_labels":false}]}})
                } else if path.ends_with("/heartbeat") {
                    if matches!(case, Case::LeaseLost) {
                        status = 409;
                        json!({"error":{"code":"stale_lease"}})
                    } else {
                        json!({"lease_expires_at":(chrono::Utc::now()+chrono::Duration::seconds(120)).to_rfc3339()})
                    }
                } else if path.contains("/inputs/predecessor/") {
                    requests.lock().map_err(|_| "peer mutex")?.push(Request {
                        path,
                        headers,
                        body,
                    });
                    let bytes: &[u8] = if matches!(case, Case::BadPredecessor) {
                        b"tampered"
                    } else {
                        b"predecessor"
                    };
                    write!(
                        stream,
                        "HTTP/1.1 200 Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    )?;
                    stream.write_all(bytes)?;
                    continue;
                } else if path.ends_with("/uploads") {
                    if matches!(
                        case,
                        Case::Grant422 | Case::Grant401 | Case::Grant403 | Case::Grant409
                    ) {
                        status = match case {
                            Case::Grant422 => 422,
                            Case::Grant401 => 401,
                            Case::Grant403 => 403,
                            _ => 409,
                        };
                        json!({"error":{"code":"fixture_grant_refusal"}})
                    } else {
                        let index = grants.len();
                        grants.push(body.clone());
                        json!({"upload_url":format!("/cap/{index}"),"headers":{"X-Capability":"synthetic-upload"}})
                    }
                } else if let Some(index) = path.strip_prefix("/cap/") {
                    if headers.contains_key("authorization")
                        || headers.get("x-capability").map(String::as_str)
                            != Some("synthetic-upload")
                    {
                        return Err("capability credential separation failed".into());
                    }
                    let index: usize = index.parse()?;
                    let grant = grants.get(index).ok_or("unknown grant")?;
                    if grant["size_bytes"].as_u64() != Some(bytes.len() as u64)
                        || grant["sha256"] != format!("{:x}", Sha256::digest(&bytes))
                    {
                        return Err("upload integrity differs".into());
                    }
                    let name = grant
                        .get("name")
                        .or_else(|| grant.get("path"))
                        .and_then(Value::as_str)
                        .ok_or("filename required")?;
                    json!({"role":grant["role"],"storage":{"bucket":"fixture","key":format!("prefix/{name}")},"size_bytes":grant["size_bytes"],"sha256":grant["sha256"],"media_type":grant["media_type"]})
                } else if path.ends_with("/manifest") {
                    let document = cannery_core::json::decode(&serde_json::to_vec(&body)?, 256)?;
                    let mut reply = json!({"ref":ID,"sha256":cannery_core::json::canonical::sha256(&document,256)?});
                    if matches!(case, Case::ManifestTamper) {
                        reply["sha256"] = json!("0".repeat(64));
                    }
                    reply
                } else if path.ends_with("/submission") || path.ends_with("/completion") {
                    submission_replies += 1;
                    if matches!(case, Case::SubmissionRetry) && submission_replies == 1 {
                        requests.lock().map_err(|_| "peer mutex")?.push(Request {
                            path,
                            headers,
                            body,
                        });
                        continue;
                    }
                    json!({"state":"testing"})
                } else if path.ends_with("/tracks/workflow/concerns") {
                    status = 201;
                    json!({"state":"open"})
                } else if path.ends_with("/release") {
                    release_replies += 1;
                    if matches!(case, Case::StepFailed) && release_replies == 1 {
                        status = 422;
                    }
                    json!({"state":"failed"})
                } else {
                    status = 404;
                    json!({"error":{"code":"unknown_fixture_route"}})
                };
                requests.lock().map_err(|_| "peer mutex")?.push(Request {
                    path,
                    headers,
                    body,
                });
                let bytes = serde_json::to_vec(&reply)?;
                write!(
                    stream,
                    "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                )?;
                stream.write_all(&bytes)?;
            }
            Ok(())
        })
    };
    Ok(Peer {
        root,
        stop,
        requests,
        thread: Some(thread),
    })
}
struct Validator;
impl OutputValidator for Validator {
    fn check<'a>(&'a self, _: Option<&'a str>, _: &'a Value, _: &'a Path) -> FutureResult<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}
#[tokio::test]
#[ignore = "requires explicit reviewed native CLI supervisor CANNERY_NATIVE_CLI"]
#[allow(clippy::too_many_lines)] // One actual local process/wire/settlement witness per scenario.
async fn experiment_http_and_owned_process_lifecycle() -> Result<(), Error> {
    let binary = PathBuf::from(std::env::var("CANNERY_NATIVE_CLI")?).canonicalize()?;
    for case in [
        Case::PinnedInputs,
        Case::SubmissionRetry,
        Case::Success,
        Case::StepFailed,
        Case::Concern,
        Case::Invalid,
        Case::Tamper,
        Case::LinkTamper,
        Case::Predecessor,
        Case::BadPredecessor,
        Case::LeaseLost,
        Case::Cancel,
        Case::Idle,
        Case::Conflict,
        Case::ManifestTamper,
        Case::Grant422,
        Case::Grant401,
        Case::Grant403,
        Case::Grant409,
    ] {
        let peer = peer(case)?;
        let root = std::env::temp_dir().join(format!(
            "experiment-proof-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir(&root)?;
        let work = root.join("work");
        let steps = root.join("steps");
        std::fs::create_dir(&steps)?;
        for (directory, value) in [
            ("datasets/data/v1", "data"),
            ("baselines/base/v2", "baseline"),
        ] {
            let pin = root.join(directory);
            std::fs::create_dir_all(&pin)?;
            std::fs::write(pin.join("pin.txt"), value)?;
        }
        let backend = Arc::new(LocalBackend::new(
            binary.clone(),
            PathBuf::from("/usr/bin/python3"),
            steps,
            true,
        )?);
        let resources = Resources {
            client: ApiClient::new(&peer.root)?,
            token: Secret::new("synthetic-experimenter".into()),
            project: "fixture".into(),
            data_root: root.clone(),
            work_root: work.clone(),
            backend,
            provisioner: Arc::new(NoCode),
            validator: Arc::new(Validator),
        };
        let cancel = CancellationEvent::new();
        let trigger = if matches!(case, Case::Cancel) {
            let cancel = cancel.clone();
            let ready = root.join("pids");
            Some(tokio::spawn(async move {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
                while !ready.exists() && tokio::time::Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                cancel.set();
            }))
        } else {
            None
        };
        let result = tokio::time::timeout(
            Duration::from_secs(8),
            Experiment::new(resources).run_once(cancel),
        )
        .await?;
        if let Some(trigger) = trigger {
            trigger.await?;
        }
        match case {
            Case::Idle => assert!(result?.is_none()),
            Case::Conflict => assert_eq!(result.err(), Some(RuntimeError::Http(409))),
            Case::Cancel => assert_eq!(result.err(), Some(RuntimeError::Cancelled)),
            Case::LeaseLost => assert_eq!(result?.ok_or("result missing")?.state, "abandoned"),
            Case::Success | Case::Predecessor | Case::PinnedInputs | Case::SubmissionRetry => {
                assert_eq!(result?.ok_or("result missing")?.state, "testing");
            }
            _ => assert_eq!(result?.ok_or("result missing")?.state, "failed", "{case:?}"),
        }
        let requests = peer.requests.lock().map_err(|_| "peer mutex")?;
        if matches!(
            case,
            Case::Grant422 | Case::Grant401 | Case::Grant403 | Case::Grant409
        ) {
            assert_eq!(
                requests
                    .iter()
                    .filter(|r| r.path.ends_with("/uploads"))
                    .count(),
                1
            );
            assert!(!requests.iter().any(|r| r.path.starts_with("/cap/")));
        }
        assert_eq!(requests[0].body, json!({"mode":"workflow"}));
        for request in requests.iter().filter(|r| !r.path.starts_with("/cap/")) {
            assert_eq!(
                request.headers.get("authorization").map(String::as_str),
                Some("Bearer synthetic-experimenter")
            );
        }
        if matches!(
            case,
            Case::Success | Case::Predecessor | Case::PinnedInputs | Case::SubmissionRetry
        ) {
            let submission = requests
                .iter()
                .find(|r| r.path.ends_with("/submission"))
                .ok_or("submission missing")?;
            let text = submission.body["document"]
                .as_str()
                .ok_or("submission without a run document")?;
            let document = cannery_core::front_matter::parse(
                text,
                cannery_core::front_matter::Limits::default(),
            )?;
            assert!(document.front_matter.get("stage").is_none());
            assert!(document.front_matter.get("status").is_none());
            assert_eq!(document.front_matter["provenance"]["science_revision"], "1");
            assert!(document.front_matter["manifest"]["ref"].is_string());
            if matches!(case, Case::Success | Case::SubmissionRetry) {
                assert_eq!(document.front_matter["provenance"]["custom"], true);
                assert_eq!(document.body, "Notes stay.\n");
            } else {
                assert_eq!(document.body, "");
            }
            assert!(
                submission
                    .headers
                    .get("idempotency-key")
                    .is_some_and(|key| key.starts_with("submit-"))
            );
            assert!(
                requests
                    .iter()
                    .filter(|r| r.path.ends_with("/uploads"))
                    .all(|r| r.body["role"] != "run" && r.body.get("path").is_none())
            );
        }
        let concerns: Vec<_> = requests
            .iter()
            .filter(|r| r.path.ends_with("/concerns"))
            .collect();
        if matches!(case, Case::Concern) {
            // Raised before the failed step's release, naming the attempt.
            assert_eq!(concerns.len(), 1);
            assert_eq!(
                concerns[0].path,
                "/api/projects/fixture/tracks/workflow/concerns"
            );
            let document = cannery_core::front_matter::parse(
                concerns[0].body["document"]
                    .as_str()
                    .ok_or("concern without a document")?,
                cannery_core::front_matter::Limits::default(),
            )?;
            assert_eq!(
                Value::Object(document.front_matter),
                json!({"kind":"blocker","hypothesis":1,"attempt":1})
            );
            assert_eq!(document.body, "The plan assumes a GPU.\n");
        } else {
            assert!(concerns.is_empty(), "{case:?}");
        }
        if matches!(case, Case::SubmissionRetry) {
            let retries: Vec<_> = requests
                .iter()
                .filter(|request| request.path.ends_with("/submission"))
                .collect();
            assert_eq!(retries.len(), 2);
            assert_eq!(retries[0].body, retries[1].body);
            assert_eq!(
                retries[0].headers["idempotency-key"],
                retries[1].headers["idempotency-key"]
            );
        }
        if matches!(case, Case::StepFailed) {
            let retries: Vec<_> = requests
                .iter()
                .filter(|request| request.path.ends_with("/release"))
                .collect();
            assert_eq!(retries.len(), 2);
            assert!(retries[1].body.get("step").is_none());
            assert!(retries[1].body.get("logs").is_none());
            assert_eq!(retries[1].body["code"], "step_failed");
        }
        if let Some(release) = requests.iter().find(|r| r.path.ends_with("/release")) {
            assert_eq!(
                release.body["step"],
                if matches!(case, Case::Tamper | Case::LinkTamper) {
                    "tamper"
                } else {
                    "agent"
                }
            );
            assert!(release.body["logs"].is_array());
            let code = match case {
                Case::StepFailed | Case::Concern => "step_failed",
                Case::Invalid => "invalid_step_output",
                Case::Grant422 => "invalid_output",
                Case::Grant401 | Case::Grant403 | Case::Grant409 => "runner_error",
                Case::Tamper | Case::LinkTamper | Case::BadPredecessor | Case::ManifestTamper => {
                    "input_verification_failed"
                }
                _ => return Err("unexpected release".into()),
            };
            assert_eq!(release.body["code"], code);
            let unchanged_status = match case {
                Case::Grant401 => Some(401),
                Case::Grant403 => Some(403),
                Case::Grant409 => Some(409),
                _ => None,
            };
            if let Some(status) = unchanged_status {
                assert_eq!(
                    release.body["reason"],
                    RuntimeError::Http(status).to_string()
                );
            }
        }
        drop(requests);
        if matches!(case, Case::Cancel | Case::LeaseLost) {
            for pid in std::fs::read_to_string(root.join("pids"))?.split_whitespace() {
                assert!(
                    !Path::new(&format!("/proc/{pid}")).exists(),
                    "owned process remains after settlement"
                );
            }
        }
        if work.exists() {
            assert_eq!(
                std::fs::read_dir(&work)?.count(),
                0,
                "settled directory cleanup {case:?}"
            );
        }
        std::fs::remove_dir_all(root)?;
    }
    Ok(())
}
