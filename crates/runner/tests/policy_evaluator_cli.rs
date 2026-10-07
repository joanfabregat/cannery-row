//! Actual leased HTTP, capability streaming and native local-process policy steps.
#![forbid(unsafe_code)]
use cannery_core::json::{self, Document};
use cannery_runner::{cli_depth::PolicyEntryPoint, policy};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
type Error = Box<dyn std::error::Error + Send + Sync>;
const ID: &str = "00000000-0000-0000-0000-000000000001";
const ATTEMPT: &str = "00000000-0000-0000-0000-000000000002";
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] // Preserve the bounded protocol peer recipes from the library proof.
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
    Durable,
    Idle,
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
    Ok(
        json!({"job":{"job_id":ID,"attempt_id":ATTEMPT,"track":"workflow","evaluator":{"id":if matches!(case,Case::Mismatch){"wrong-policy"}else{"policy-evaluator"},"revision":"v1"},"science_revision":if matches!(case,Case::Success|Case::CompletionRetry){json!("1")}else{json!(1)},"deadline":(chrono::Utc::now()+chrono::Duration::seconds(300)).to_rfc3339(),"parameters":{"alpha":2},"inputs":{"evidence":refs,"manifest":{"ref":"tester-manifest","sha256":digest(&manifest())?},"datasets":[],"baselines":[]},"lease":{"token":"synthetic-policy-lease","generation":1,"expires_at":(chrono::Utc::now()+chrono::Duration::seconds(120)).to_rfc3339()}},"attempt_ref":"h1/a1","heartbeat_seconds":if matches!(case,Case::LeaseLost|Case::Cancel){0.15}else{60.0}}),
    )
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
                    json!({"revision":1,"content":{"metrics":[{"key":"mrr","splits":["dev","test"],"dimensions":[]}],"datasets":[],"baselines":[],"validators":[],"interfaces":[],"code_repositories":{"trusted":["org/repo"]},"limits":{"resource_ceilings":{"cpu":"2","memory":"256Mi"},"max_deadline_seconds":if matches!(case,Case::Budget){30}else if matches!(case,Case::SetupBudget){620}else{3600}}}})
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
#[tokio::test]
#[ignore = "requires reviewed installed policy-capable CANNERY_NATIVE_CLI"]
#[allow(clippy::too_many_lines)] // Installed command, protocol and settled roots in one bounded witness.
async fn installed_runner_policy_success_failure_and_retry() -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    let binary = PathBuf::from(std::env::var("CANNERY_NATIVE_CLI")?).canonicalize()?;
    for case in [Case::Success, Case::Nonzero, Case::CompletionRetry] {
        let peer = peer(case)?;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce)?;
        let root = std::env::temp_dir().join(format!(
            "policy-cli-{}",
            uuid::Uuid::from_bytes(nonce).simple()
        ));
        std::fs::create_dir(&root)?;
        let token = root.join("token");
        std::fs::write(&token, "synthetic-evaluator")?;
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600))?;
        let policy_path = root.join("policy.json");
        std::fs::write(
            &policy_path,
            json::encode_http(&policy(case)?.envelope, 256)?,
        )?;
        let work = root.join("work");
        let steps = root.join("steps");
        std::fs::create_dir(root.join("data"))?;
        std::fs::create_dir(root.join("cache"))?;
        assert!(!steps.exists());
        let config = root.join("config.toml");
        std::fs::write(
            &config,
            format!(
                "api_url = {:?}\nproject = \"fixture\"\ndata_root = {:?}\nwork_root = {:?}\ncache_root = {:?}\n[launcher]\ntype = \"local\"\nstep_root = {:?}\n[[kinds]]\nkind = \"eval\"\ntoken_file = {:?}\npolicy = {:?}\n",
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
                entry_point: PolicyEntryPoint::RunnerEvalKind,
                repr_nesting_budget: 512,
            },
            &cannery_runner::config::NativePathResolver,
            256,
        )
        .map_err(|error| format!("typed config refusal: {error:?}"))?;
        let executable = binary.clone();
        let stdout_path = root.join("stdout");
        let stderr_path = root.join("stderr");
        let output = tokio::task::spawn_blocking(move || -> Result<std::process::Output, Error> {
            let mut child = std::process::Command::new(executable)
                .args(["runner", "--config"])
                .arg(config)
                .arg("--once")
                .stdout(std::fs::File::create(&stdout_path)?)
                .stderr(std::fs::File::create(&stderr_path)?)
                .spawn()?;
            let deadline = std::time::Instant::now() + Duration::from_secs(15);
            let status = loop {
                if let Some(status) = child.try_wait()? {
                    break status;
                }
                if std::time::Instant::now() >= deadline {
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
            assert!(!String::from_utf8_lossy(bytes).contains("synthetic-evaluator"));
            assert!(!String::from_utf8_lossy(bytes).contains("synthetic-policy-lease"));
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
        if matches!(case, Case::Nonzero) {
            let failure = requests
                .iter()
                .find(|r| r.path.ends_with("/failure"))
                .ok_or("missing failure")?;
            assert_eq!(failure.body["error_code"], "evaluator_error");
            assert!(
                failure.body["logs"]
                    .as_array()
                    .is_some_and(|logs| !logs.is_empty())
            );
            assert!(!requests.iter().any(|r| r.path.ends_with("/completion")));
        } else {
            let completions: Vec<_> = requests
                .iter()
                .filter(|r| r.path.ends_with("/completion"))
                .collect();
            let completion = completions.first().ok_or("missing completion")?;
            assert_eq!(
                completion.body["evidence"]["assessment"]["comparisons"][0]["value"],
                0.8
            );
            assert_eq!(
                completion.body["evidence"]["provenance"]["source_revision"],
                "commit-first"
            );
            assert_eq!(
                completion.body["evidence"]["provenance"]["science_revision"],
                "1"
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
