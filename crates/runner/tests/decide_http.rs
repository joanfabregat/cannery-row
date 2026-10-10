//! The decide kind over real HTTP and local processes: no live API, cloud,
//! daemon or credentials.
#![forbid(unsafe_code)]
use cannery_core::principal::Secret;
use cannery_runner::{
    cancellation::CancellationEvent,
    cli_depth::PolicyEntryPoint,
    policy::parse_decider,
    runtime::{
        FutureResult,
        backend::LocalBackend,
        decide::Decider,
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
const JOB: &str = "00000000-0000-0000-0000-0000000000d1";
const BUNDLE: &str = "---\nphase: decide\n---\n# The decider's bundle\n";
const VERIFICATION: &str = "/api/projects/fixture/hypotheses/1/attempts/1/verification";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Case {
    /// The step decides; the runner completes the job with its document.
    Success,
    /// The API refuses the decision document: the job fails with the reason.
    Refused,
    /// The step exits non-zero without a decision.
    StepFailed,
    /// The claimed job names another decider revision: no step runs.
    Mismatch,
    /// Nothing to claim.
    Idle,
}
#[derive(Debug)]
struct Request {
    method: String,
    path: String,
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
fn envelope(script: &str) -> Value {
    json!({"schema_version":"0.2","decider":{"id":"fixture-decider","revision":"decider-1"},"step":{"apiVersion":"cannery-row/v1","kind":"Step","metadata":{"name":"fixture-decider"},"spec":{"role":"decider","container":{"image":format!("fixture.invalid/decider@sha256:{}", "d".repeat(64)),"command":["/bin/sh","-c",script],"env":[],"resources":{"limits":{"cpu":"1","memory":"256Mi"}}},"activeDeadlineSeconds":60,"network":"none","sandbox":"trusted fixture","inputs":{"artifacts":[]},"outputs":{"artifacts":[{"name":"decision","path":"/cr/outputs/decision"}]}}}})
}
fn script(case: Case) -> &'static str {
    match case {
        Case::StepFailed => "echo no-decision; exit 7",
        _ => {
            "test -z \"$TOKEN$LEASE\" || exit 9; grep -q '# The decider' \"$CR_ROOT/context/context.md\" || exit 9; grep -q 'fixture-decider' \"$CR_ROOT/job.json\" || exit 9; printf '%s\\n' --- 'outcome: promote' --- 'Every gate passed.' > \"$CR_ROOT/outputs/decision/decision.md\""
        }
    }
}
fn claim(case: Case) -> Value {
    let revision = if case == Case::Mismatch {
        "decider-0"
    } else {
        "decider-1"
    };
    let lease = (chrono::Utc::now() + chrono::Duration::seconds(120)).to_rfc3339();
    json!({"job":{"schema_version":"0.2","job_id":JOB,"phase":"decide","attempt_id":"00000000-0000-0000-0000-000000000002","performer":"runner","decider":{"id":"fixture-decider","revision":revision},"track":"workflow","hypothesis":1,"science_revision":"1","review_case_id":"00000000-0000-0000-0000-0000000000c1","inputs":{"verification":{"ref":VERIFICATION,"sha256":"a".repeat(64)}},"output_prefix":"jobs/decide","deadline":lease,"limits":{"max_output_bytes":1_048_576},"lease":{"token":"synthetic-job-lease","generation":1,"expires_at":lease}},"attempt_ref":"#1.1","heartbeat_seconds":60.0,"context":{"ref":"/api/projects/fixture/hypotheses/1/attempts/1/context.md?phase=decide","bytes":BUNDLE.len()}})
}
#[allow(clippy::too_many_lines)] // One bounded protocol peer for every decide route.
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
            while !stop.load(Ordering::SeqCst) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                };
                stream.set_nonblocking(false)?;
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
                let mut words = head.lines().next().unwrap_or_default().split_whitespace();
                let method = words.next().ok_or("missing method")?.to_owned();
                let path = words.next().ok_or("missing path")?.to_owned();
                let headers: BTreeMap<_, _> = head
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
                    .collect();
                let body = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
                let mut status = 200;
                let mut text = None;
                let reply = if path.ends_with("/jobs/claims") {
                    if case == Case::Idle {
                        status = 409;
                        json!({"error":{"code":"nothing_to_claim"}})
                    } else {
                        status = 201;
                        claim(case)
                    }
                } else if path.ends_with("/config/science/1") {
                    json!({"revision":1,"content":{"metrics":[],"datasets":[],"baselines":[],"validators":[],"interfaces":[],"code_repositories":{"trusted":[]},"limits":{"resource_ceilings":{"cpu":"2","memory":"512Mi"},"max_deadline_seconds":600}}})
                } else if path.contains("/context.md?phase=decide") {
                    text = Some(BUNDLE);
                    Value::Null
                } else if path.ends_with("/heartbeat") {
                    json!({"lease_expires_at":(chrono::Utc::now()+chrono::Duration::seconds(120)).to_rfc3339()})
                } else if path.ends_with("/uploads") {
                    let index = grants.len();
                    grants.push(body.clone());
                    json!({"upload_url":format!("/cap/{index}"),"headers":{"X-Capability":"synthetic-upload"}})
                } else if let Some(index) = path.strip_prefix("/cap/") {
                    if headers.contains_key("authorization") {
                        return Err("capability credential separation failed".into());
                    }
                    let grant = grants.get(index.parse::<usize>()?).ok_or("unknown grant")?;
                    if grant["sha256"] != format!("{:x}", Sha256::digest(&bytes)) {
                        return Err("upload integrity differs".into());
                    }
                    let name = grant
                        .get("name")
                        .or_else(|| grant.get("path"))
                        .and_then(Value::as_str)
                        .ok_or("filename required")?;
                    json!({"role":grant["role"],"storage":{"bucket":"fixture","key":format!("prefix/{name}")},"size_bytes":grant["size_bytes"],"sha256":grant["sha256"],"media_type":grant["media_type"]})
                } else if path.ends_with("/completion") {
                    if case == Case::Refused {
                        status = 422;
                        json!({"error":{"code":"validation_failed","message":"a promotion needs a pass verdict"}})
                    } else {
                        json!({"state":"completed"})
                    }
                } else if path.ends_with("/failure") {
                    json!({"state":"failed"})
                } else {
                    status = 404;
                    json!({"error":{"code":"unknown_fixture_route"}})
                };
                requests
                    .lock()
                    .map_err(|_| "peer mutex")?
                    .push(Request { method, path, body });
                let (content_type, bytes) = match text {
                    Some(text) => ("text/markdown", text.as_bytes().to_vec()),
                    None => ("application/json", serde_json::to_vec(&reply)?),
                };
                write!(
                    stream,
                    "HTTP/1.1 {status} Fixture\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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
#[allow(clippy::too_many_lines)] // One local process and wire witness per scenario.
async fn decide_http_claims_runs_the_decider_and_completes_or_fails() -> Result<(), Error> {
    let binary = PathBuf::from(std::env::var("CANNERY_NATIVE_CLI")?).canonicalize()?;
    for case in [
        Case::Success,
        Case::Refused,
        Case::StepFailed,
        Case::Mismatch,
        Case::Idle,
    ] {
        let peer = peer(case)?;
        let root = std::env::temp_dir().join(format!(
            "decide-proof-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir(&root)?;
        let work = root.join("work");
        let steps = root.join("steps");
        std::fs::create_dir(&steps)?;
        let backend = Arc::new(LocalBackend::new(
            binary.clone(),
            PathBuf::from("/usr/bin/python3"),
            steps,
            true,
        )?);
        let resources = Resources {
            client: ApiClient::new(&peer.root)?,
            token: Secret::new("synthetic-decider".into()),
            project: "fixture".into(),
            data_root: root.clone(),
            work_root: work.clone(),
            backend,
            provisioner: Arc::new(NoCode),
            validator: Arc::new(Validator),
        };
        let envelope =
            cannery_core::json::decode(&serde_json::to_vec(&envelope(script(case)))?, 256)?;
        let step = parse_decider(&envelope, PolicyEntryPoint::RunnerVerifyKind, 256)
            .map_err(|e| format!("fixture decider refused: {:?} {:?}", e.paths, e.violations))?;
        let decider = Decider::new(resources, Arc::new(step)).map_err(|e| format!("{e:?}"))?;
        let result = tokio::time::timeout(
            Duration::from_secs(8),
            decider.run_once(CancellationEvent::new()),
        )
        .await
        .map_err(|_| format!("{case:?} timed out"))?
        .map_err(|e| format!("{case:?}: {e:?}"))?;
        let requests = peer.requests.lock().map_err(|_| "peer mutex")?;
        let paths: Vec<_> = requests
            .iter()
            .map(|r| format!("{} {}", r.method, r.path))
            .collect();
        assert_eq!(
            requests[0].body,
            json!({"phase":"decide","revision":"decider-1"}),
            "{case:?}"
        );
        let completion = requests.iter().find(|r| r.path.ends_with("/completion"));
        let failure = requests.iter().find(|r| r.path.ends_with("/failure"));
        match case {
            Case::Idle => {
                assert!(result.is_none());
                assert_eq!(requests.len(), 1);
            }
            Case::Success => {
                let result = result.ok_or("no job")?;
                assert_eq!(
                    result.state,
                    "completed",
                    "{paths:?} {:?}",
                    failure.map(|f| &f.body)
                );
                let body = &completion.ok_or("no completion")?.body;
                assert_eq!(body["job_id"], JOB);
                let document = cannery_core::front_matter::parse(
                    body["document"].as_str().ok_or("no document")?,
                    cannery_core::front_matter::Limits::default(),
                )?;
                // The runner cites what the job names; the step only decides.
                assert_eq!(
                    Value::Object(document.front_matter),
                    json!({"outcome":"promote","verification":{"ref":VERIFICATION,"sha256":"a".repeat(64)},"writeup":null})
                );
                assert_eq!(document.body, "Every gate passed.\n");
                assert!(failure.is_none());
                // The decision is a document, not an upload; only the log is uploaded.
                assert!(
                    requests
                        .iter()
                        .filter(|r| r.path.ends_with("/uploads"))
                        .all(|r| r.body["role"] != "decision")
                );
            }
            Case::Refused => {
                assert_eq!(result.ok_or("no job")?.state, "failed");
                let body = &failure.ok_or("no failure")?.body;
                assert_eq!(body["error_code"], "invalid_step_output");
                assert_eq!(body["step"], "fixture-decider");
                assert_eq!(
                    body["reason"],
                    "the decision document was refused: a promotion needs a pass verdict"
                );
                assert_eq!(body["logs"].as_array().map(Vec::len), Some(1));
            }
            Case::StepFailed => {
                assert_eq!(result.ok_or("no job")?.state, "failed");
                assert!(completion.is_none());
                let body = &failure.ok_or("no failure")?.body;
                assert_eq!(body["error_code"], "step_failed");
                assert_eq!(body["step"], "fixture-decider");
            }
            Case::Mismatch => {
                assert_eq!(result.ok_or("no job")?.state, "failed");
                assert!(completion.is_none());
                assert_eq!(
                    failure.ok_or("no failure")?.body["error_code"],
                    "decider_mismatch"
                );
                // Refused before the science revision is read or a step runs.
                assert!(
                    !paths
                        .iter()
                        .any(|p| p.contains("/config/science/") || p.ends_with("/uploads"))
                );
            }
        }
        drop(requests);
        assert!(!work.exists() || std::fs::read_dir(&work)?.count() == 0);
        std::fs::remove_dir_all(&root)?;
    }
    Ok(())
}
