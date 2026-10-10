//! Actual leased HTTP, capability streaming and native local-process steps of
//! the verify kind: producer, scorer, then a stock or step policy.
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
        validation::NativeOutputValidator,
        verify::{AppliedPolicy, Verifier},
        worker::{NoCode, Resources},
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
const RESUMED_KEY: &str = "jobs/00000000-0000-0000-0000-000000000001/produce/predictions/out.txt";
#[derive(Clone, Copy, Debug, PartialEq)]
enum Case {
    Stock,
    Step,
    Mismatch,
    RunTamper,
    ManifestTamper,
    HeldOutProducer,
    Nonzero,
    BadEvidence,
    Citation,
    Multiple,
    Budget,
    Resume,
    ResumeTamper,
    CompletionRetry,
    Durable,
    LeaseLost,
    Cancel,
    Idle,
}
impl Case {
    const fn step_policy(self) -> bool {
        matches!(
            self,
            Self::Step | Self::Citation | Self::Multiple | Self::Budget | Self::Resume
        )
    }
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
fn run() -> Value {
    json!({"provenance":{"source_revision":"commit"},"claimed":true})
}
fn manifest() -> Value {
    json!({"schema_version":"0.2","attempt_id":ATTEMPT,"objects":[{"role":"candidate_checkpoint","storage":{"backend":"s3","bucket":"fixture","key":"candidate/model.txt"},"size_bytes":5,"sha256":format!("{:x}",Sha256::digest(b"model")),"media_type":"text/plain"}]})
}
fn science(case: Case) -> Value {
    json!({"metrics":[{"key":"mrr","unit":"ratio","direction":"higher","splits":["dev"],"dimensions":[]}],"datasets":[{"id":"labels","revision":"labels-r1","held_out_labels":true}],"baselines":[],"validators":[],"interfaces":[],"code_repositories":{"trusted":[]},"limits":{"resource_ceilings":{"cpu":"2","memory":"256Mi"},"max_deadline_seconds":if matches!(case,Case::Budget){30}else{3600},"report_max_bytes":65536}})
}
fn step(name: &str, role: &str, script: &str, inputs: &Value, output: &Value) -> Value {
    json!({"apiVersion":"cannery-row/v1","kind":"Step","metadata":{"name":name},"spec":{"role":role,"container":{"image":format!("fixture.invalid/step@sha256:{}","a".repeat(64)),"command":["/bin/sh","-c",script],"env":[],"resources":{"limits":{"cpu":"1","memory":"128Mi"}}},"activeDeadlineSeconds":5,"network":"none","sandbox":"controlled native local-process fixture","inputs":{"artifacts":inputs},"outputs":{"artifacts":[output]}}})
}
fn evidence(case: Case) -> Value {
    let mut evidence = json!({"provenance":{"source_revision":"commit","dataset_revision":"labels-r1"},"measurements":[{"metric":"mrr","split":"dev","dimensions":{},"value":0.8,"authority":"tester_verified","unit":"ratio","direction":"higher"}],"artifact_roles":["evidence"],"observations":"Scored every query."});
    if matches!(case, Case::BadEvidence) {
        evidence["verdict"] = json!("pass");
    }
    evidence
}
fn steps(case: Case) -> Result<Value, Error> {
    let produce = match case {
        Case::Nonzero => "echo observed-failure; exit 7".to_owned(),
        Case::Resume | Case::ResumeTamper => "exit 9".to_owned(),
        _ => "test -z \"$TOKEN$LEASE$GITHUB_TOKEN\" || exit 9;grep -q '\"claimed\": true' \"$CR_ROOT/inputs/claimed_sheet/claimed.json\" || exit 9;test \"$(cat \"$CR_ROOT/inputs/candidate_checkpoint/model.txt\")\" = model || exit 9;grep -q alpha \"$CR_ROOT/job.json\" || exit 9;printf ranked > \"$CR_ROOT/outputs/predictions/out.txt\"".to_owned(),
    };
    let mut produce_inputs = vec![
        json!({"name":"claimed_sheet","from":"attempt","path":"/cr/inputs/claimed_sheet"}),
        json!({"name":"candidate_checkpoint","from":"attempt","path":"/cr/inputs/candidate_checkpoint"}),
    ];
    if matches!(case, Case::HeldOutProducer) {
        produce_inputs.push(json!({"name":"labels","from":"dataset","path":"/cr/inputs/labels"}));
    }
    let encoded = serde_json::to_string(&evidence(case))?;
    let score = if matches!(case, Case::LeaseLost | Case::Cancel) {
        "sleep 30 & child=$!; printf '%s %s' \"$$\" \"$child\" > \"$CR_ROOT/../../../pids\";wait"
            .to_owned()
    } else {
        format!(
            "test \"$(cat \"$CR_ROOT/inputs/labels/labels.txt\")\" = held-out || exit 9;test \"$(cat \"$CR_ROOT/inputs/predictions/out.txt\")\" = ranked || exit 9;printf '%s' '{encoded}' > \"$CR_ROOT/outputs/evidence/evidence.json\""
        )
    };
    Ok(json!([
        {"name":"produce","revision":"v1","manifest":step("produce","producer",&produce,&json!(produce_inputs),&json!({"name":"predictions","path":"/cr/outputs/predictions"}))},
        {"name":"score","revision":"v1","manifest":step("score","scorer",&score,&json!([{"name":"predictions","from":"step","path":"/cr/inputs/predictions"},{"name":"labels","from":"dataset","path":"/cr/inputs/labels"}]),&json!({"name":"evidence","interface":"cr-evidence/v0.2","path":"/cr/outputs/evidence"}))},
    ]))
}
fn revision(case: Case) -> &'static str {
    if case.step_policy() {
        "step-v1"
    } else {
        "stock-v1"
    }
}
fn claim(case: Case) -> Result<Value, Error> {
    let verifier = if matches!(case, Case::Mismatch) {
        json!({"id":"fixture-verifier","revision":"other"})
    } else {
        json!({"id":"fixture-verifier","revision":revision(case)})
    };
    let mut job = json!({"schema_version":"0.2","job_id":ID,"attempt_id":ATTEMPT,"phase":"verify","performer":"runner","verifier":verifier,"track":"workflow","science_revision":1,"deadline":(chrono::Utc::now()+chrono::Duration::seconds(300)).to_rfc3339(),"parameters":{"alpha":2},"inputs":{"run":{"ref":"run","sha256":digest(&run())?},"manifest":{"ref":"manifest","sha256":digest(&manifest())?},"datasets":[{"id":"labels","revision":"labels-r1"}],"baselines":[]},"steps":steps(case)?,"resume":null,"lease":{"token":"synthetic-verify-lease","generation":1,"expires_at":(chrono::Utc::now()+chrono::Duration::seconds(120)).to_rfc3339()}});
    if matches!(case, Case::Resume | Case::ResumeTamper) {
        job["resume"] = json!({"from_step":"score","outputs":[{"step":"produce","name":"predictions","key":RESUMED_KEY,"size_bytes":6,"sha256":format!("{:x}",Sha256::digest(b"ranked")),"media_type":"text/plain"}]});
    }
    Ok(
        json!({"job":job,"attempt_ref":"h1/a1","heartbeat_seconds":if matches!(case,Case::LeaseLost|Case::Cancel){0.15}else{60.0}}),
    )
}
fn verdict(case: Case) -> Value {
    json!({"gates":[{"id":"quality","result":"pass"}],"comparisons":[{"metric":"mrr","split":"dev","dimensions":{},"source":"tester","value":if matches!(case,Case::Citation){0.9}else{0.8},"reference":{"value":0.4,"kind":"baseline","label":"baseline"}}],"verdict":"pass","reason":"assessed"})
}
/// The verify kind's policy configuration: a stock policy or a policy step.
fn policy_config(case: Case) -> Result<Value, Error> {
    if !case.step_policy() {
        return Ok(
            json!({"schema_version":"0.2","verifier":{"id":"fixture-verifier","revision":"stock-v1"},"gates":[{"id":"holds-control","metric":"mrr","split":"dev","statistic":"value","compare":"control","op":">=","min_delta":0.02}],"baselines":[{"id":"control","revision":"r1","label":"Control","measurements":[{"metric":"mrr","split":"dev","dimensions":{},"value":0.4}]}],"default_control":{"id":"control","revision":"r1"}}),
        );
    }
    let encoded = serde_json::to_string(&verdict(case))?;
    let script = if matches!(case, Case::Multiple) {
        format!(
            "printf '%s' '{encoded}' > \"$CR_ROOT/outputs/verdict/a.json\";printf '%s' '{encoded}' > \"$CR_ROOT/outputs/verdict/b.json\""
        )
    } else {
        format!(
            "test -z \"$TOKEN$LEASE$GITHUB_TOKEN\" || exit 9;grep -q '\"metrics\"' \"$CR_ROOT/job.json\" || exit 9;grep -q commit \"$CR_ROOT/inputs/evidence/evidence.json\" || exit 9;printf '%s' '{encoded}' > \"$CR_ROOT/outputs/verdict/verdict.json\""
        )
    };
    let step = step(
        "policy",
        "policy",
        &script,
        &json!([{"name":"evidence","from":"step","interface":"cr-evidence/v0.2","path":"/cr/inputs/evidence"}]),
        &json!({"name":"verdict","path":"/cr/outputs/verdict"}),
    );
    Ok(
        json!({"schema_version":"0.2","verifier":{"id":"fixture-verifier","revision":"step-v1"},"step":step}),
    )
}
fn applied(case: Case) -> Result<AppliedPolicy, Error> {
    let config = Arc::new(document(&policy_config(case)?)?);
    let entry_point = PolicyEntryPoint::RunnerVerifyKind;
    Ok(if case.step_policy() {
        AppliedPolicy::Step(Arc::new(policy::parse_step_policy(
            config,
            entry_point,
            256,
        )?))
    } else {
        AppliedPolicy::Stock(Arc::new(policy::parse_policy(config, entry_point, 256)?))
    })
}
fn respond(stream: &mut std::net::TcpStream, status: u16, reply: &Value) -> Result<(), Error> {
    let bytes = serde_json::to_vec(reply)?;
    write!(
        stream,
        "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    )?;
    stream.write_all(&bytes)?;
    Ok(())
}
/// A request's path, lowercased headers and body.
type Received = (String, BTreeMap<String, String>, Vec<u8>);
fn read_request(stream: &mut std::net::TcpStream) -> Result<Received, Error> {
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
    let headers = head
        .lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    Ok((path, headers, bytes))
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
                let (path, headers, bytes) = read_request(&mut stream)?;
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
                    json!({"revision":1,"content":science(case)})
                } else if path.ends_with("/inputs/run") {
                    let mut run = run();
                    if matches!(case, Case::RunTamper) {
                        run["claimed"] = json!(false);
                    }
                    run
                } else if path.ends_with("/inputs/manifest") {
                    let mut manifest = manifest();
                    if matches!(case, Case::ManifestTamper) {
                        manifest["objects"][0]["media_type"] = json!("application/json");
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
                    let key = format!("jobs/{ID}/{}", grant["path"].as_str().ok_or("path")?);
                    json!({"role":grant["role"],"storage":{"backend":"s3","bucket":"fixture","key":key,"generation":"1"},"sha256":grant["sha256"],"size_bytes":grant["size_bytes"],"media_type":grant["media_type"]})
                } else if path.ends_with("/uploads") {
                    let index = grants.len();
                    grants.push(body.clone());
                    json!({"upload_url":format!("/cap/{index}"),"headers":{"X-Capability":"synthetic-capability"}})
                } else if path.contains("/inputs/object?") {
                    let bytes: &[u8] = if !path.contains("predictions") {
                        b"model"
                    } else if matches!(case, Case::ResumeTamper) {
                        b"ranket"
                    } else {
                        b"ranked"
                    };
                    requests.lock().map_err(|_| "peer mutex")?.push(Request {
                        path,
                        headers,
                        body,
                    });
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
                requests.lock().map_err(|_| "peer mutex")?.push(Request {
                    path,
                    headers,
                    body,
                });
                respond(&mut stream, status, &reply)?;
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
fn expected_failure(case: Case) -> Option<(&'static str, Option<&'static str>)> {
    Some(match case {
        Case::Mismatch => ("policy_mismatch", None),
        Case::RunTamper | Case::ManifestTamper | Case::ResumeTamper => {
            ("input_verification_failed", Some("produce"))
        }
        Case::HeldOutProducer => ("runner_error", Some("produce")),
        Case::Nonzero => ("step_failed", Some("produce")),
        Case::BadEvidence => ("invalid_step_output", Some("score")),
        Case::Citation | Case::Multiple => ("invalid_step_output", Some("policy")),
        Case::Budget => ("deadline_exceeded", None),
        _ => return None,
    })
}
fn check_completion(case: Case, requests: &[Request]) -> Result<(), Error> {
    let completions: Vec<_> = requests
        .iter()
        .filter(|r| r.path.ends_with("/completion"))
        .collect();
    let completion = completions.first().ok_or("completion missing")?;
    assert_eq!(
        completion
            .headers
            .get("idempotency-key")
            .map(String::as_str),
        Some(format!("complete-{ID}-1").as_str())
    );
    assert_eq!(completion.body["schema_version"], "0.2");
    assert_eq!(completion.body["job_id"], ID);
    let text = completion.body["document"].as_str().ok_or("document")?;
    let report =
        cannery_core::front_matter::parse(text, cannery_core::front_matter::Limits::default())?;
    let front_matter = &report.front_matter;
    assert_eq!(front_matter["verdict"], "pass");
    assert_eq!(front_matter["policy_revision"], revision(case));
    assert_eq!(front_matter["provenance"]["science_revision"], "1");
    assert_eq!(front_matter["provenance"]["source_revision"], "commit");
    assert_eq!(front_matter["measurements"], evidence(case)["measurements"]);
    assert_eq!(report.body, "Scored every query.\n");
    let gate = if case.step_policy() {
        "quality"
    } else {
        "holds-control"
    };
    assert_eq!(front_matter["gates"][0]["id"], gate);
    let objects = completion.body["manifest"]["objects"]
        .as_array()
        .ok_or("objects")?;
    assert_eq!(completion.body["manifest"]["attempt_id"], ATTEMPT);
    let mut roles = vec!["step_log", "predictions", "evidence"];
    if case.step_policy() {
        roles.extend(["verdict", "policy_step"]);
    }
    for role in roles {
        assert!(objects.iter().any(|o| o["role"] == role), "{case:?} {role}");
    }
    if matches!(case, Case::Resume) {
        let resumed = objects
            .iter()
            .find(|o| o["role"] == "predictions")
            .ok_or("resumed output")?;
        assert_eq!(
            resumed["storage"],
            json!({"backend":"s3","bucket":"fixture","key":RESUMED_KEY})
        );
        assert!(
            !requests.iter().any(|r| r.path.ends_with("/uploads")
                && r.body["path"] == "produce/step_log/produce.log")
        );
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
    Ok(())
}
#[tokio::test]
#[ignore = "requires explicit reviewed supervisor CANNERY_NATIVE_CLI"]
#[allow(clippy::too_many_lines)] // One real process, wire and settled cleanup witness per scenario.
async fn verify_http_and_owned_process_lifecycle() -> Result<(), Error> {
    let binary = PathBuf::from(std::env::var("CANNERY_NATIVE_CLI")?).canonicalize()?;
    for case in [
        Case::Stock,
        Case::Step,
        Case::Mismatch,
        Case::RunTamper,
        Case::ManifestTamper,
        Case::HeldOutProducer,
        Case::Nonzero,
        Case::BadEvidence,
        Case::Citation,
        Case::Multiple,
        Case::Budget,
        Case::Resume,
        Case::ResumeTamper,
        Case::CompletionRetry,
        Case::Durable,
        Case::LeaseLost,
        Case::Cancel,
        Case::Idle,
    ] {
        let peer = peer(case)?;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce)?;
        let root = std::env::temp_dir().join(format!(
            "verify-proof-{}",
            uuid::Uuid::from_bytes(nonce).simple()
        ));
        std::fs::create_dir(&root)?;
        let labels = root.join("datasets/labels/labels-r1");
        std::fs::create_dir_all(&labels)?;
        std::fs::write(labels.join("labels.txt"), "held-out")?;
        let steps = root.join("steps");
        std::fs::create_dir(&steps)?;
        let work = root.join("work");
        let resources = Resources {
            client: ApiClient::new(&peer.root)?,
            token: Secret::new("synthetic-verifier".into()),
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
        let worker = Verifier::new(resources, applied(case)?)?;
        let cancel = CancellationEvent::new();
        let trigger = if matches!(case, Case::Cancel) {
            let ready = root.join("pids");
            let cancel = cancel.clone();
            Some(tokio::spawn(async move {
                let deadline = Instant::now() + Duration::from_secs(3);
                while Instant::now() < deadline && !ready.exists() {
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
            Case::Stock | Case::Step | Case::Resume | Case::CompletionRetry => {
                assert_eq!(
                    result?.ok_or("missing result")?.state,
                    "completed",
                    "{case:?}"
                );
            }
            Case::LeaseLost => assert_eq!(result?.ok_or("missing result")?.state, "abandoned"),
            Case::Cancel => assert_eq!(result.err(), Some(RuntimeError::Cancelled)),
            Case::Idle => assert!(result?.is_none()),
            _ => assert_eq!(result?.ok_or("missing result")?.state, "failed", "{case:?}"),
        }
        let requests = peer.requests.lock().map_err(|_| "peer mutex")?;
        assert_eq!(
            requests[0].body,
            json!({"phase":"verify","revision":revision(case)})
        );
        for request in requests.iter().filter(|r| !r.path.starts_with("/cap/")) {
            assert_eq!(
                request.headers.get("authorization").map(String::as_str),
                Some("Bearer synthetic-verifier")
            );
        }
        if matches!(case, Case::Mismatch) {
            assert!(
                !requests
                    .iter()
                    .any(|r| r.path.contains("/inputs/") || r.path.contains("/config/"))
            );
        }
        if matches!(case, Case::Budget | Case::HeldOutProducer) {
            assert!(!requests.iter().any(|r| r.path.ends_with("/uploads")));
        }
        if matches!(case, Case::Resume | Case::ResumeTamper) {
            assert!(
                requests
                    .iter()
                    .any(|r| r.path.contains("/inputs/object?") && r.path.contains("predictions"))
            );
        }
        let failure = requests.iter().find(|r| r.path.ends_with("/failure"));
        match (expected_failure(case), failure) {
            (Some((code, step)), Some(failure)) => {
                assert_eq!(failure.body["error_code"], code, "{case:?}");
                assert_eq!(failure.body["job_id"], ID);
                assert_eq!(
                    failure.body.get("step").and_then(Value::as_str),
                    step,
                    "{case:?}"
                );
                if matches!(case, Case::Budget) {
                    assert_eq!(
                        failure.body["reason"],
                        "policy step policy needs up to 35s (5s for the step, 30s kept by the runner) but a verify job of science revision 1 allows its policy 30s (max_deadline_seconds)"
                    );
                }
            }
            (None, None) => {}
            (expected, failure) => {
                return Err(format!(
                    "{case:?}: expected {expected:?}, reported {:?}",
                    failure.map(|f| &f.body)
                )
                .into());
            }
        }
        if matches!(
            case,
            Case::Stock | Case::Step | Case::Resume | Case::CompletionRetry | Case::Durable
        ) {
            check_completion(case, &requests)?;
        } else {
            assert!(!requests.iter().any(|r| r.path.ends_with("/completion")));
        }
        drop(requests);
        if matches!(case, Case::Cancel | Case::LeaseLost) {
            for pid in std::fs::read_to_string(root.join("pids"))?.split_whitespace() {
                assert!(
                    !Path::new(&format!("/proc/{pid}")).exists(),
                    "unsettled process {pid} for {case:?}"
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
#[tokio::test]
#[ignore = "requires reviewed installed CANNERY_NATIVE_CLI"]
#[allow(clippy::too_many_lines)] // Installed command, protocol and settled roots in one bounded witness.
async fn installed_runner_verify_kind_completes_fails_and_retries() -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    let binary = PathBuf::from(std::env::var("CANNERY_NATIVE_CLI")?).canonicalize()?;
    for case in [Case::Step, Case::Nonzero, Case::CompletionRetry] {
        let peer = peer(case)?;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce)?;
        let root = std::env::temp_dir().join(format!(
            "verify-cli-{}",
            uuid::Uuid::from_bytes(nonce).simple()
        ));
        std::fs::create_dir(&root)?;
        let token = root.join("token");
        std::fs::write(&token, "synthetic-verifier")?;
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600))?;
        let policy_path = root.join("policy.json");
        std::fs::write(&policy_path, serde_json::to_vec(&policy_config(case)?)?)?;
        let labels = root.join("data/datasets/labels/labels-r1");
        std::fs::create_dir_all(&labels)?;
        std::fs::write(labels.join("labels.txt"), "held-out")?;
        let work = root.join("work");
        let steps = root.join("steps");
        std::fs::create_dir(root.join("cache"))?;
        let config = root.join("config.toml");
        std::fs::write(
            &config,
            format!(
                "api_url = {:?}\nproject = \"fixture\"\ndata_root = {:?}\nwork_root = {:?}\ncache_root = {:?}\n[launcher]\ntype = \"local\"\nstep_root = {:?}\n[[kinds]]\nkind = \"verify\"\ntoken_file = {:?}\npolicy = {:?}\n",
                peer.root,
                root.join("data"),
                work,
                root.join("cache"),
                steps,
                token,
                policy_path,
            ),
        )?;
        cannery_runner::config::load_config_file(
            &config,
            &policy::FilePolicyLoader {
                entry_point: PolicyEntryPoint::RunnerVerifyKind,
                repr_nesting_budget: 512,
            },
            &cannery_runner::config::NativePathResolver,
            256,
        )
        .map_err(|error| format!("typed config refusal: {error:?}"))?;
        let stdout_path = root.join("stdout");
        let stderr_path = root.join("stderr");
        let executable = binary.clone();
        let output = tokio::task::spawn_blocking(move || -> Result<std::process::Output, Error> {
            let mut child = std::process::Command::new(executable)
                .args(["runner", "--config"])
                .arg(config)
                .arg("--once")
                .stdout(std::fs::File::create(&stdout_path)?)
                .stderr(std::fs::File::create(&stderr_path)?)
                .spawn()?;
            let deadline = Instant::now() + Duration::from_secs(15);
            let status = loop {
                if let Some(status) = child.try_wait()? {
                    break status;
                }
                if Instant::now() >= deadline {
                    child.kill()?;
                    child.wait()?;
                    return Err("installed runner smoke deadline exceeded".into());
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            Ok(std::process::Output {
                status,
                stdout: std::fs::read(stdout_path)?,
                stderr: std::fs::read(stderr_path)?,
            })
        })
        .await??;
        assert!(
            output.status.success(),
            "installed runner failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        for bytes in [&output.stdout, &output.stderr] {
            assert!(!String::from_utf8_lossy(bytes).contains("synthetic-verifier"));
            assert!(!String::from_utf8_lossy(bytes).contains("synthetic-verify-lease"));
        }
        let requests = peer.requests.lock().map_err(|_| "peer mutex")?;
        assert_eq!(
            requests[0].body,
            json!({"phase":"verify","revision":revision(case)})
        );
        if matches!(case, Case::Nonzero) {
            let failure = requests
                .iter()
                .find(|r| r.path.ends_with("/failure"))
                .ok_or("missing failure")?;
            assert_eq!(failure.body["error_code"], "step_failed");
            assert_eq!(failure.body["step"], "produce");
            assert!(
                failure.body["logs"]
                    .as_array()
                    .is_some_and(|logs| !logs.is_empty())
            );
            assert!(!requests.iter().any(|r| r.path.ends_with("/completion")));
        } else {
            check_completion(case, &requests)?;
        }
        drop(requests);
        assert!(
            steps.is_dir(),
            "backend creates absent step root before claim"
        );
        for owned in [&work, &steps] {
            if owned.exists() {
                assert_eq!(
                    std::fs::read_dir(owned)?.count(),
                    0,
                    "settled installed cleanup"
                );
            }
        }
        std::fs::remove_dir_all(root)?;
    }
    Ok(())
}
