//! Actual leased HTTP, capability streaming and native local-process policy steps.
#![forbid(unsafe_code)]
use cannery_core::{
    json::{self, Document},
    principal::Secret,
};
use cannery_runner::{
    cancellation::CancellationEvent,
    cli_depth::PolicyEntryPoint,
    policy,
    runtime::{
        RuntimeError,
        backend::LocalBackend,
        http::ApiClient,
        policy_evaluator::PolicyEvaluator,
        validation::NativeOutputValidator,
        worker::{NoCode, Tester},
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
    time::{Duration, Instant},
};
type Error = Box<dyn std::error::Error + Send + Sync>;
const ID: &str = "00000000-0000-0000-0000-000000000001";
const ATTEMPT: &str = "00000000-0000-0000-0000-000000000002";
#[derive(Clone, Copy, Debug)]
enum Case {
    Success,
    Mismatch,
    EvidenceTamper,
    ManifestTamper,
    Invalid,
    Citation,
    Multiple,
    Oversize,
    Nonzero,
    Timeout,
    LeaseLost,
    Cancel,
    Semantic,
    Budget,
    SetupBudget,
    CompletionRetry,
    CompletionBodyCancel,
    Durable,
    Idle,
    HeldOutBaseline,
    TesterHeldOut,
    TesterVisibleLabels,
}
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
fn document(value: &Value) -> Result<Document, Error> {
    Ok(json::decode(&serde_json::to_vec(value)?, 256)?)
}
fn digest(value: &Value) -> Result<String, Error> {
    Ok(json::canonical::sha256(&document(value)?, 256)?)
}
fn records() -> Value {
    json!([
        {"provenance":{"source_revision":"commit-first","dataset_revision":"data"},"measurements":[{"metric":"mrr","split":"dev","dimensions":{},"authority":"tester_verified","value":0.42}]},
        {"provenance":{"source_revision":"commit-second","dataset_revision":"data"},"measurements":[{"metric":"mrr","split":"test","dimensions":{},"authority":"tester_verified","value":0.8}]}
    ])
}
fn manifest() -> Value {
    json!({"schema_version":"0.2","attempt_id":ATTEMPT,"objects":[{"role":"candidate_checkpoint","storage":{"bucket":"fixture","key":"candidate/model.txt"},"size_bytes":5,"sha256":format!("{:x}",Sha256::digest(b"model")),"media_type":"text/plain"}]})
}
fn claim(case: Case) -> Result<Value, Error> {
    let records = records();
    let mut refs = vec![];
    for (index, record) in records.as_array().ok_or("records")?.iter().enumerate() {
        refs.push(json!({"ref":format!("record-{index}"),"sha256":digest(record)?}));
    }
    let mut claimed = json!({"job":{"job_id":ID,"attempt_id":ATTEMPT,"track":"workflow","evaluator":{"id":if matches!(case,Case::Mismatch){"wrong-policy"}else{"policy-evaluator"},"revision":"v1"},"science_revision":if matches!(case,Case::Success|Case::CompletionRetry){json!("1")}else{json!(1)},"deadline":(chrono::Utc::now()+chrono::Duration::seconds(300)).to_rfc3339(),"parameters":{"alpha":2},"inputs":{"evidence":refs,"manifest":{"ref":"tester-manifest","sha256":digest(&manifest())?},"datasets":[],"baselines":[]},"lease":{"token":"synthetic-policy-lease","generation":1,"expires_at":(chrono::Utc::now()+chrono::Duration::seconds(120)).to_rfc3339()}},"attempt_ref":"h1/a1","heartbeat_seconds":if matches!(case,Case::LeaseLost|Case::Cancel){0.15}else{60.0}});
    if matches!(
        case,
        Case::HeldOutBaseline | Case::TesterHeldOut | Case::TesterVisibleLabels
    ) {
        claimed["job"]["inputs"]["datasets"] =
            json!([{"id":"labels-registered","revision":"labels-r1"}]);
        claimed["job"]["inputs"]["baselines"] =
            json!([{"id":"baseline-registered","revision":"baseline-r1"}]);
    }
    if matches!(case, Case::TesterHeldOut | Case::TesterVisibleLabels) {
        let mut manifest = serde_json::from_slice::<Value>(&json::encode_http(
            &policy(Case::HeldOutBaseline)?.document,
            256,
        )?)?;
        manifest["spec"]["role"] = json!("producer");
        manifest["spec"]["inputs"]["artifacts"] = json!([
            {"name":"labels","from":"dataset","id":"labels-registered","path":"/cr/inputs/labels"},
            {"name":"control","from":"baseline","id":"baseline-registered","path":"/cr/inputs/control"},
        ]);
        manifest["spec"]["container"]["command"] = json!([
            "/bin/sh",
            "-c",
            "printf launched > \"$CR_ROOT/../../../launched\";test \"$(cat \"$CR_ROOT/inputs/labels/labels.txt\")\" = held-out || exit 9;test \"$(cat \"$CR_ROOT/inputs/control/control.txt\")\" = pinned-baseline || exit 9;printf '{}' > \"$CR_ROOT/outputs/verdict/result.json\""
        ]);
        claimed["job"]["tester"] = json!({"id":"fixture-tester","revision":"v1"});
        claimed["job"]["steps"] = json!([{"name":"policy","revision":"v1","manifest":manifest}]);
    }
    Ok(claimed)
}
fn assessment(case: Case) -> Value {
    if matches!(case, Case::Invalid) {
        return json!({"gates":[],"verdict":"unsupported","reason":"candidate-supplied"});
    }
    json!({"gates":[{"id":"quality","result":"pass"}],"comparisons":[{"metric":"mrr","split":"test","dimensions":{},"source":"tester","value":if matches!(case,Case::Citation){0.9}else{0.8},"reference":{"value":0.4,"kind":"baseline","label":"baseline"}}],"verdict":"pass","reason":"assessed"})
}
fn policy(case: Case) -> Result<Arc<policy::StepPolicy>, Error> {
    let encoded = serde_json::to_string(&assessment(case))?;
    let script=match case{
        Case::Nonzero=>"echo observed-failure;exit 7".into(),
        Case::Timeout=>"sleep 30".into(),
        Case::LeaseLost|Case::Cancel=>"sleep 30 & child=$!; printf '%s %s' \"$$\" \"$child\" > \"$CR_ROOT/../../../pids\";wait".into(),
        Case::Oversize=>"head -c 1048577 /dev/zero > \"$CR_ROOT/outputs/verdict/verdict.json\"".into(),
        Case::Multiple=>format!("printf '%s' '{encoded}' > \"$CR_ROOT/outputs/verdict/a.json\";printf '%s' '{encoded}' > \"$CR_ROOT/outputs/verdict/b.json\""),
        _=>format!("test -z \"$TOKEN$LEASE$GITHUB_TOKEN\" || exit 9;grep -q '\"metrics\"' \"$CR_ROOT/job.json\" || exit 9; first=$(grep -n commit-first \"$CR_ROOT/inputs/evidence/evidence.json\"|cut -d: -f1);second=$(grep -n commit-second \"$CR_ROOT/inputs/evidence/evidence.json\"|cut -d: -f1);test \"$first\" -lt \"$second\" || exit 9;test \"$(cat \"$CR_ROOT/inputs/candidate_checkpoint/model.txt\")\" = model || exit 9;grep -q candidate_checkpoint \"$CR_ROOT/inputs/manifest/manifest.json\" || exit 9;printf '%s' '{encoded}' > \"$CR_ROOT/outputs/verdict/verdict.json\""),
    };
    let mut step = json!({"apiVersion":"cannery-row/v1","kind":"Step","metadata":{"name":"policy"},"spec":{"role":"evaluator","container":{"image":format!("fixture.invalid/step@sha256:{}","a".repeat(64)),"command":["/bin/sh","-c",script],"env":[],"resources":{"limits":{"cpu":"1","memory":"128Mi"}}},"activeDeadlineSeconds":if matches!(case,Case::Timeout){1}else{5},"network":"none","sandbox":"controlled native local-process fixture","inputs":{"artifacts":[{"name":"evidence","from":"attempt","path":"/cr/inputs/evidence"},{"name":"manifest","from":"attempt","path":"/cr/inputs/manifest"},{"name":"candidate_checkpoint","from":"attempt","path":"/cr/inputs/candidate_checkpoint"}]},"outputs":{"artifacts":[{"name":"verdict","path":"/cr/outputs/verdict"}]}}});
    if matches!(case, Case::Semantic) {
        step["spec"]["container"]["env"] =
            json!([{"name":"NVIDIA_VISIBLE_DEVICES","value":"invalid"}]);
    }
    if matches!(case, Case::HeldOutBaseline) {
        step["spec"]["inputs"]["artifacts"].as_array_mut().ok_or("inputs")?.extend([
            json!({"name":"labels","from":"dataset","id":"labels-registered","path":"/cr/inputs/labels"}),
            json!({"name":"control","from":"baseline","id":"baseline-registered","path":"/cr/inputs/control"}),
        ]);
        let command = step["spec"]["container"]["command"][2]
            .as_str()
            .ok_or("script")?;
        step["spec"]["container"]["command"][2] = json!(format!(
            "printf launched > \"$CR_ROOT/../../../launched\";test \"$(cat \"$CR_ROOT/inputs/labels/labels.txt\")\" = held-out || exit 9;test \"$(cat \"$CR_ROOT/inputs/control/control.txt\")\" = pinned-baseline || exit 9;{command}"
        ));
    }
    if matches!(case, Case::SetupBudget) {
        step["spec"]["code"] = json!({"repo":"org/repo","commit":"a".repeat(40)});
        step["spec"]["setup"] = json!({"run":"echo setup","cache":{"key_files":["requirements.txt"],"paths":["site"]},"activeDeadlineSeconds":600});
    }
    Ok(Arc::new(policy::parse_step_policy(
        Arc::new(document(
            &json!({"schema_version":"0.2","evaluator":{"id":"policy-evaluator","revision":"v1"},"step":step}),
        )?),
        PolicyEntryPoint::Evaluator,
        256,
    )?))
}
#[allow(clippy::too_many_lines)] // Controlled finite wire peer records raw capability body integrity.
fn peer(case: Case) -> Result<Peer, Error> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let root = format!("http://{}", listener.local_addr()?);
    let stop = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(vec![]));
    let thread = {
        let stop = stop.clone();
        let requests = requests.clone();
        std::thread::spawn(move || {
            let mut grants: Vec<Value> = vec![];
            let mut completions = 0;
            while !stop.load(Ordering::SeqCst) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => return Err(error.into()),
                };
                stream.set_read_timeout(Some(Duration::from_secs(3)))?;
                let mut received = vec![];
                let (head, bytes) = loop {
                    let mut chunk = [0; 4096];
                    let count = stream.read(&mut chunk)?;
                    if count == 0 || received.len() + count > 2 * 1024 * 1024 {
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
                    if matches!(case, Case::Idle) {
                        status = 409;
                        json!({"error":{"code":"nothing_to_claim"}})
                    } else {
                        status = 201;
                        claim(case)?
                    }
                } else if path.ends_with("/config/science/1") {
                    json!({"revision":1,"content":{"metrics":[{"key":"mrr","splits":["dev","test"],"dimensions":[]}],"datasets":if matches!(case,Case::HeldOutBaseline|Case::TesterHeldOut|Case::TesterVisibleLabels){json!([{"id":"labels-registered","revision":"labels-r1","held_out_labels":!matches!(case,Case::TesterVisibleLabels)}])}else{json!([])},"baselines":if matches!(case,Case::HeldOutBaseline|Case::TesterHeldOut|Case::TesterVisibleLabels){json!([{"id":"baseline-registered","revision":"baseline-r1"}])}else{json!([])},"validators":[],"interfaces":[],"code_repositories":{"trusted":["org/repo"]},"limits":{"resource_ceilings":{"cpu":"2","memory":"256Mi"},"max_deadline_seconds":if matches!(case,Case::Budget){30}else if matches!(case,Case::SetupBudget){620}else{3600}}}})
                } else if path.ends_with("/inputs/evidence") {
                    let mut records = records();
                    records.as_array_mut().ok_or("records")?.reverse();
                    if matches!(case, Case::EvidenceTamper) {
                        records[0]["measurements"][0]["value"] = json!(0.99);
                    }
                    records
                } else if path.ends_with("/inputs/manifest") {
                    let mut manifest = manifest();
                    if matches!(case, Case::ManifestTamper) {
                        manifest["objects"] = json!([]);
                    }
                    manifest
                } else if path.starts_with("/cap/") {
                    if headers.contains_key("authorization")
                        || headers.get("x-capability").map(String::as_str)
                            != Some("synthetic-capability")
                    {
                        return Err("credential isolation failed".into());
                    }
                    let index = path
                        .strip_prefix("/cap/")
                        .ok_or("capability path")?
                        .parse::<usize>()?;
                    let grant = grants.get(index).ok_or("unknown grant")?;
                    if grant["size_bytes"].as_u64() != Some(bytes.len() as u64)
                        || grant["sha256"] != format!("{:x}", Sha256::digest(&bytes))
                    {
                        return Err("upload integrity differs".into());
                    }
                    json!({"role":grant["role"],"storage":{"bucket":"fixture","key":format!("uploads/{index}")},"sha256":grant["sha256"],"size_bytes":grant["size_bytes"],"media_type":grant["media_type"]})
                } else if path.ends_with("/uploads") {
                    let index = grants.len();
                    grants.push(body.clone());
                    json!({"upload_url":format!("/cap/{index}"),"headers":{"X-Capability":"synthetic-capability"}})
                } else if path.contains("/inputs/object?") {
                    requests.lock().map_err(|_| "peer mutex")?.push(Request {
                        path,
                        headers,
                        body,
                    });
                    let bytes = b"model";
                    write!(
                        stream,
                        "HTTP/1.1 200 Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    )?;
                    stream.write_all(bytes)?;
                    continue;
                } else if path.ends_with("/heartbeat") {
                    if matches!(case, Case::LeaseLost) {
                        status = 409;
                        json!({"error":{"code":"stale_lease"}})
                    } else {
                        json!({"lease_expires_at":(chrono::Utc::now()+chrono::Duration::seconds(120)).to_rfc3339()})
                    }
                } else if path.ends_with("/completion") {
                    completions += 1;
                    if matches!(case, Case::CompletionRetry) && completions == 1 {
                        requests.lock().map_err(|_| "peer mutex")?.push(Request {
                            path,
                            headers,
                            body,
                        });
                        continue;
                    }
                    if matches!(case, Case::Durable) {
                        status = 422;
                        json!({"error":{"code":"invalid_submission"}})
                    } else {
                        json!({"state":"completed"})
                    }
                } else if path.ends_with("/failure") {
                    json!({"state":"failed"})
                } else {
                    status = 404;
                    json!({"error":{"code":"unknown_fixture_route"}})
                };
                let stalled_completion =
                    matches!(case, Case::CompletionBodyCancel) && path.ends_with("/completion");
                requests.lock().map_err(|_| "peer mutex")?.push(Request {
                    path,
                    headers,
                    body,
                });
                if stalled_completion {
                    stream.set_read_timeout(Some(Duration::from_millis(200)))?;
                    stream.write_all(b"HTTP/1.1 200 Fixture\r\nContent-Type: application/json\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n{\"state\":")?;
                    while !stop.load(Ordering::SeqCst) {
                        let mut byte = [0; 1];
                        match stream.read(&mut byte) {
                            Ok(0) => break,
                            Ok(_) => {}
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                                ) => {}
                            Err(error) => return Err(error.into()),
                        }
                    }
                    continue;
                }
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
#[tokio::test]
#[ignore = "requires explicit reviewed supervisor CANNERY_NATIVE_CLI"]
#[allow(clippy::too_many_lines)] // One real process, wire and settled cleanup witness per scenario.
async fn policy_evaluator_http_and_owned_process_lifecycle() -> Result<(), Error> {
    let binary = PathBuf::from(std::env::var("CANNERY_NATIVE_CLI")?).canonicalize()?;
    for case in [
        Case::Success,
        Case::Mismatch,
        Case::EvidenceTamper,
        Case::ManifestTamper,
        Case::Invalid,
        Case::Citation,
        Case::Multiple,
        Case::Oversize,
        Case::Nonzero,
        Case::Timeout,
        Case::LeaseLost,
        Case::Cancel,
        Case::Semantic,
        Case::Budget,
        Case::SetupBudget,
        Case::CompletionRetry,
        Case::CompletionBodyCancel,
        Case::Durable,
        Case::Idle,
        Case::HeldOutBaseline,
        Case::TesterHeldOut,
        Case::TesterVisibleLabels,
    ] {
        let peer = peer(case)?;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce)?;
        let root = std::env::temp_dir().join(format!(
            "policy-proof-{}",
            uuid::Uuid::from_bytes(nonce).simple()
        ));
        std::fs::create_dir(&root)?;
        for (directory, file, bytes) in [
            (
                "datasets/labels-registered/labels-r1",
                "labels.txt",
                "held-out",
            ),
            (
                "baselines/baseline-registered/baseline-r1",
                "control.txt",
                "pinned-baseline",
            ),
        ] {
            let directory = root.join(directory);
            std::fs::create_dir_all(&directory)?;
            std::fs::write(directory.join(file), bytes)?;
        }
        let steps = root.join("steps");
        std::fs::create_dir(&steps)?;
        let work = root.join("work");
        let resources = Tester {
            client: ApiClient::new(&peer.root)?,
            token: Secret::new("synthetic-evaluator".into()),
            project: "fixture".into(),
            data_root: root.clone(),
            work_root: work.clone(),
            backend: Arc::new(LocalBackend::new(
                binary.clone(),
                PathBuf::from("/usr/bin/python3"),
                steps,
                true,
            )?),
            provisioner: Arc::new(NoCode),
            validator: Arc::new(NativeOutputValidator::runtime_policy()),
        };
        if matches!(case, Case::TesterHeldOut | Case::TesterVisibleLabels) {
            let result = resources
                .run_once(CancellationEvent::new())
                .await?
                .ok_or("tester result")?;
            assert_eq!(result.state, "failed");
            assert_eq!(
                root.join("launched").exists(),
                matches!(case, Case::TesterVisibleLabels),
                "only the public-label producer may launch"
            );
            let requests = peer.requests.lock().map_err(|_| "peer mutex")?;
            assert!(
                requests
                    .iter()
                    .any(|request| request.path.ends_with("/failure"))
            );
            assert_eq!(
                requests
                    .iter()
                    .any(|request| request.path.ends_with("/uploads")),
                matches!(case, Case::TesterVisibleLabels)
            );
            assert!(
                !requests
                    .iter()
                    .any(|request| request.path.ends_with("/completion")),
                "this producer-only fixture has no scorer"
            );
            assert_eq!(
                std::fs::read_dir(&work)?.count(),
                0,
                "tester held-out rejection settles its root"
            );
            std::fs::remove_dir_all(root)?;
            continue;
        }
        let worker = PolicyEvaluator::new(resources, policy(case)?)?;
        let cancel = CancellationEvent::new();
        let trigger = if matches!(case, Case::Cancel | Case::CompletionBodyCancel) {
            let ready = root.join("pids");
            let cancel = cancel.clone();
            let requests = peer.requests.clone();
            Some(tokio::spawn(async move {
                let deadline = Instant::now() + Duration::from_secs(3);
                while Instant::now() < deadline {
                    let response_ready = if matches!(case, Case::CompletionBodyCancel) {
                        requests.lock().is_ok_and(|requests| {
                            requests
                                .iter()
                                .any(|request| request.path.ends_with("/completion"))
                        })
                    } else {
                        ready.exists()
                    };
                    if response_ready {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                cancel.set();
            }))
        } else {
            None
        };
        let result = tokio::time::timeout(Duration::from_secs(8), worker.run_once(cancel)).await?;
        if let Some(trigger) = trigger {
            trigger.await?;
        }
        match case {
            Case::Success | Case::CompletionRetry | Case::HeldOutBaseline => {
                assert_eq!(result?.ok_or("missing result")?.state, "completed");
            }
            Case::LeaseLost => assert_eq!(result?.ok_or("missing result")?.state, "abandoned"),
            Case::Cancel | Case::CompletionBodyCancel => {
                assert_eq!(result.err(), Some(RuntimeError::Cancelled));
            }
            Case::Idle => assert!(result?.is_none()),
            _ => assert_eq!(result?.ok_or("missing result")?.state, "failed", "{case:?}"),
        }
        if matches!(case, Case::HeldOutBaseline) {
            assert_eq!(std::fs::read_to_string(root.join("launched"))?, "launched");
        }
        let requests = peer.requests.lock().map_err(|_| "peer mutex")?;
        assert_eq!(
            requests[0].body,
            json!({"stage":"evaluator","revision":"v1"})
        );
        for request in requests.iter().filter(|r| !r.path.starts_with("/cap/")) {
            assert_eq!(
                request.headers.get("authorization").map(String::as_str),
                Some("Bearer synthetic-evaluator")
            );
        }
        if matches!(case, Case::Mismatch) {
            assert!(
                !requests
                    .iter()
                    .any(|r| r.path.contains("/inputs/") || r.path.contains("/config/"))
            );
        }
        if matches!(case, Case::Semantic | Case::Budget | Case::SetupBudget) {
            assert!(!requests.iter().any(|r| r.path.contains("/inputs/")));
        }
        if let Some(failure) = requests.iter().find(|r| r.path.ends_with("/failure")) {
            let code = match case {
                Case::Mismatch => "policy_mismatch",
                Case::EvidenceTamper | Case::ManifestTamper => "input_verification_failed",
                _ => "evaluator_error",
            };
            assert_eq!(failure.body["error_code"], code, "{case:?}");
            assert!(failure.body.get("step").is_none());
            if matches!(case, Case::Budget | Case::SetupBudget) {
                let (needed, parts, budget) = if matches!(case, Case::Budget) {
                    (35, "5s for the step", 30)
                } else {
                    (635, "5s for the step, 600s for its setup", 620)
                };
                assert_eq!(
                    failure.body["reason"],
                    format!(
                        "policy step policy needs up to {needed}s ({parts}, 30s kept by the runner) but an evaluation job of science revision 1 has {budget}s (max_deadline_seconds)"
                    )
                );
                assert_eq!(failure.body["logs"], json!([]));
            }
        }
        if matches!(case, Case::Success | Case::CompletionRetry | Case::Durable) {
            let completions: Vec<_> = requests
                .iter()
                .filter(|r| r.path.ends_with("/completion"))
                .collect();
            let completion = completions.first().ok_or("completion missing")?;
            assert_eq!(
                completion.body["evidence"]["assessment"]["comparisons"][0]["value"],
                0.8
            );
            assert_eq!(
                completion.body["evidence"]["provenance"]["source_revision"],
                "commit-first"
            );
            let objects = completion.body["manifest"]["objects"]
                .as_array()
                .ok_or("objects")?;
            for role in ["step_log", "verdict", "policy_step"] {
                assert!(objects.iter().any(|o| o["role"] == role));
            }
            if matches!(case, Case::CompletionRetry) {
                assert_eq!(completions.len(), 2);
                assert_eq!(completions[0].body, completions[1].body);
                assert_eq!(
                    completions[0].headers["idempotency-key"],
                    completions[1].headers["idempotency-key"]
                );
            }
            if matches!(case, Case::Durable) {
                assert!(!requests.iter().any(|r| r.path.ends_with("/failure")));
            }
        }
        if matches!(case, Case::Multiple | Case::Oversize) {
            assert!(
                !requests
                    .iter()
                    .any(|r| r.path.ends_with("/uploads") && r.body["role"] == "verdict")
            );
        }
        drop(requests);
        if matches!(case, Case::Cancel | Case::LeaseLost) {
            for pid in std::fs::read_to_string(root.join("pids"))?.split_whitespace() {
                assert!(
                    !Path::new(&format!("/proc/{pid}")).exists(),
                    "unsettled process {pid} for {case:?}: {:?}",
                    std::fs::read_to_string(format!("/proc/{pid}/stat"))
                );
            }
        }
        if work.exists() {
            assert_eq!(
                std::fs::read_dir(&work)?.count(),
                0,
                "settled cleanup {case:?}"
            );
        }
        std::fs::remove_dir_all(root)?;
    }
    Ok(())
}
