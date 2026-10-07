//! Real incomplete HTTP bodies exercise concrete workers and owned cleanup.
#![forbid(unsafe_code)]
use cannery_core::{json, principal::Secret};
use cannery_runner::{
    cancellation::CancellationEvent,
    cli_depth::PolicyEntryPoint,
    policy,
    runtime::{
        FutureResult, RuntimeError,
        backend::{Backend, PreparedStep},
        evaluator::Evaluator,
        experiment::Experiment,
        http::{ApiClient, LeaseHeaders, read_json},
        policy_evaluator::PolicyEvaluator,
        process::RuntimeWorker,
        validation::NativeOutputValidator,
        worker::{NoCode, Tester},
    },
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinSet,
};
type Error = Box<dyn std::error::Error + Send + Sync>;
const ID: &str = "00000000-0000-0000-0000-000000000001";
#[derive(Clone, Copy)]
enum Stall {
    ClaimHeaders,
    ClaimBody,
    ClaimDrip,
    Input,
    Failure,
    Download,
}
struct ObservedBackend(AtomicUsize);
impl Backend for ObservedBackend {
    fn start(&self) -> FutureResult<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn prepare(
        &self,
        _spec: cannery_runner::launcher::StepSpec,
        _root: PathBuf,
    ) -> FutureResult<'_, Arc<dyn PreparedStep>> {
        Box::pin(async { Err(RuntimeError::Contract) })
    }
    fn release_job<'a>(&'a self, _job: &'a str) -> FutureResult<'a, ()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
    fn close(&self) -> FutureResult<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct StopGuard(CancellationEvent);
impl Drop for StopGuard {
    fn drop(&mut self) {
        self.0.set();
    }
}
fn document(value: &Value) -> Result<Arc<json::Document>, Error> {
    Ok(Arc::new(json::decode(&serde_json::to_vec(value)?, 256)?))
}
fn step_policy() -> Result<Arc<policy::StepPolicy>, Error> {
    let step = json!({"apiVersion":"cannery-row/v1","kind":"Step","metadata":{"name":"policy"},"spec":{"role":"evaluator","container":{"image":format!("fixture.invalid/step@sha256:{}","a".repeat(64)),"command":["/bin/true"],"env":[],"resources":{"limits":{"cpu":"1","memory":"128Mi"}}},"activeDeadlineSeconds":5,"network":"none","sandbox":"controlled cancellation fixture","inputs":{"artifacts":[]},"outputs":{"artifacts":[{"name":"verdict","path":"/cr/outputs/verdict"}]}}});
    Ok(Arc::new(policy::parse_step_policy(
        document(
            &json!({"schema_version":"0.2","evaluator":{"id":"policy-evaluator","revision":"v1"},"step":step}),
        )?,
        PolicyEntryPoint::Evaluator,
        256,
    )?))
}
fn claim() -> Value {
    json!({"job":{"job_id":ID,"attempt_id":ID,"track":"fixture","evaluator":{"id":"policy-evaluator","revision":"v1"},"science_revision":1,"deadline":(chrono::Utc::now()+chrono::Duration::seconds(300)).to_rfc3339(),"steps":null,"lease":{"token":"synthetic-lease","generation":1,"expires_at":(chrono::Utc::now()+chrono::Duration::seconds(120)).to_rfc3339()}},"heartbeat_seconds":0.05})
}
async fn peer(
    mode: Stall,
    lost: bool,
    ready: CancellationEvent,
    stop: CancellationEvent,
) -> Result<(String, tokio::task::JoinHandle<Result<(), Error>>), Error> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let root = format!("http://{}", listener.local_addr()?);
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            let accepted = tokio::select! {
                () = stop.wait() => break,
                accepted = listener.accept() => accepted?,
            };
            let ready = ready.clone();
            let stop = stop.clone();
            connections.spawn(async move {
                let mut stream = accepted.0;
                let mut bytes = Vec::new();
                loop {
                    let mut buffer = [0; 4096];
                    let size = tokio::select! { ()=stop.wait()=>return Ok::<_, Error>(()), n=stream.read(&mut buffer)=>n? };
                    if size == 0 { return Ok(()); }
                    bytes.extend_from_slice(&buffer[..size]);
                    if bytes.len() > 64 * 1024 { return Err("request exceeds fixture limit".into()); }
                    if bytes.windows(4).any(|v| v == b"\r\n\r\n") { break; }
                }
                let header = String::from_utf8(bytes)?;
                let path = header.lines().next().and_then(|line|line.split_whitespace().nth(1)).ok_or("request path")?;
                let stall = match mode {
                    Stall::ClaimHeaders | Stall::ClaimBody | Stall::ClaimDrip => path.ends_with("/claims"),
                    Stall::Input => path.contains("/config/science/"),
                    Stall::Failure => path.ends_with("/failure"),
                    Stall::Download => path.starts_with("/download?"),
                };
                if stall {
                    if matches!(mode, Stall::Download) {
                        stream.write_all(b"HTTP/1.1 200 Fixture\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n").await?;
                        stream.write_all(&vec![b'a';64*1024]).await?;
                    } else if !matches!(mode, Stall::ClaimHeaders) {
                        stream.write_all(b"HTTP/1.1 201 Fixture\r\nContent-Type: application/json\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n{\"pending\":").await?;
                    }
                    ready.set();
                    if matches!(mode, Stall::ClaimDrip) {
                        let (mut reader, mut writer) = stream.into_split();
                        loop {
                            let mut byte = [0; 1];
                            tokio::select! {
                                ()=stop.wait()=>return Ok(()),
                                n=reader.read(&mut byte)=>if n? == 0 { return Ok(()); },
                                ()=tokio::time::sleep(Duration::from_secs(1))=>writer.write_all(b" ").await?,
                            }
                        }
                    }
                    // Keep the response unfinished until cancellation closes the
                    // client socket. A readiness barrier replaces latency guesses.
                    loop {
                        let mut byte = [0; 1];
                        let size = tokio::select! { ()=stop.wait()=>return Ok(()), n=stream.read(&mut byte)=>n? };
                        if size == 0 { return Ok(()); }
                    }
                }
                let (status, body) = if path.ends_with("/claims") { (201, claim()) }
                    else if path.contains("/config/science/") { (200, json!({"revision":1,"content":{}})) }
                    else if path.ends_with("/heartbeat") {
                        ready.wait().await;
                        if lost { (409, json!({"error":{"code":"stale_lease"}})) }
                        else { (200, json!({"lease_expires_at":(chrono::Utc::now()+chrono::Duration::seconds(120)).to_rfc3339()})) }
                    } else { return Err("unexpected fixture route".into()); };
                let body = serde_json::to_vec(&body)?;
                stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await?;
                stream.write_all(&body).await?;
                Ok(())
            });
        }
        connections.abort_all();
        while let Some(result) = connections.join_next().await {
            if let Ok(result) = result {
                result?;
            }
        }
        Ok(())
    });
    Ok((root, task))
}
#[tokio::test]
async fn incomplete_claim_headers_and_json_cancel_all_worker_kinds() -> Result<(), Error> {
    for mode in [Stall::ClaimHeaders, Stall::ClaimBody] {
        for kind in 0..4 {
            exercise(mode, kind, false).await?;
        }
    }
    Ok(())
}
#[tokio::test]
async fn input_and_failure_body_cancellation_settle_owned_work() -> Result<(), Error> {
    exercise(Stall::Input, 3, false).await?;
    exercise(Stall::Input, 3, true).await?;
    exercise(Stall::Failure, 0, false).await?;
    Ok(())
}
#[tokio::test]
async fn overall_api_deadline_stops_a_continuously_progressing_body() -> Result<(), Error> {
    let ready = CancellationEvent::new();
    let stop = CancellationEvent::new();
    let _guard = StopGuard(stop.clone());
    let (root, server) = peer(Stall::ClaimDrip, false, ready, stop.clone()).await?;
    let client = ApiClient::new(&root)?;
    let response = client
        .request(
            reqwest::Method::POST,
            "/claims",
            &Secret::new("synthetic".into()),
            None,
            None,
        )
        .await?;
    // A byte arrives every second, so the per-read timeout cannot terminate this
    // response. Only the real client's overall deadline bounds it.
    let result = tokio::time::timeout(Duration::from_secs(90), read_json(response)).await;
    stop.set();
    server.await??;
    assert_eq!(result?.err(), Some(RuntimeError::Transport));
    Ok(())
}
#[tokio::test]
async fn canceled_stream_settles_partial_disk_writes_before_cleanup() -> Result<(), Error> {
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce)?;
    let directory = Directory(std::env::temp_dir().join(format!(
        "download-cancel-{}",
        uuid::Uuid::from_bytes(nonce).simple()
    )));
    std::fs::create_dir(&directory.0)?;
    let file = directory.0.join("partial");
    let stop = CancellationEvent::new();
    let _guard = StopGuard(stop.clone());
    let (root, server) = peer(
        Stall::Download,
        false,
        CancellationEvent::new(),
        stop.clone(),
    )
    .await?;
    let client = ApiClient::new(&root)?;
    let cancel = CancellationEvent::new();
    let lease = LeaseHeaders {
        token: Secret::new("synthetic-lease".into()),
        generation: "1".into(),
        idempotency: None,
    };
    let mut downloaded = {
        let file = file.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            client
                .download(
                    "/download",
                    "object",
                    &Secret::new("synthetic".into()),
                    &lease,
                    &file,
                    100_000,
                    &cancel,
                )
                .await
        })
    };
    let observed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if tokio::fs::metadata(&file)
                .await
                .is_ok_and(|metadata| metadata.len() == 64 * 1024)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    cancel.set();
    let result = tokio::time::timeout(Duration::from_secs(10), &mut downloaded).await;
    stop.set();
    server.await??;
    if result.is_err() {
        // Closing the peer releases a regressed network wait. Settle the task
        // before the directory guard removes its owned file.
        if tokio::time::timeout(Duration::from_secs(10), &mut downloaded)
            .await
            .is_err()
        {
            downloaded.abort();
            let _ = downloaded.await;
        }
    }
    observed?;
    assert_eq!(result??.err(), Some(RuntimeError::LostLease));
    assert_eq!(std::fs::read(&file)?, vec![b'a'; 64 * 1024]);
    std::fs::remove_file(file)?;
    assert_eq!(std::fs::read_dir(&directory.0)?.count(), 0);
    Ok(())
}

#[tokio::test]
async fn download_refuses_excess_chunks_before_writing() -> Result<(), Error> {
    for expected_size in [0, 1024, 65_535] {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce)?;
        let directory = Directory(std::env::temp_dir().join(format!(
            "download-limit-{}",
            uuid::Uuid::from_bytes(nonce).simple()
        )));
        std::fs::create_dir(&directory.0)?;
        let stop = CancellationEvent::new();
        let _guard = StopGuard(stop.clone());
        let (root, server) = peer(
            Stall::Download,
            false,
            CancellationEvent::new(),
            stop.clone(),
        )
        .await?;
        let client = ApiClient::new(&root)?;
        let file = directory.0.join("partial");
        let lease = LeaseHeaders {
            token: Secret::new("synthetic-lease".into()),
            generation: "1".into(),
            idempotency: None,
        };
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            client.download(
                "/download",
                "object",
                &Secret::new("synthetic".into()),
                &lease,
                &file,
                expected_size,
                &CancellationEvent::new(),
            ),
        )
        .await;
        stop.set();
        server.await??;
        assert_eq!(result?.err(), Some(RuntimeError::Integrity));
        assert!(std::fs::metadata(&file)?.len() <= expected_size);
        std::fs::remove_file(file)?;
        assert_eq!(std::fs::read_dir(&directory.0)?.count(), 0);
    }
    Ok(())
}

async fn exercise(mode: Stall, kind: usize, lost: bool) -> Result<(), Error> {
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce)?;
    let directory = Directory(std::env::temp_dir().join(format!(
        "http-cancel-{}",
        uuid::Uuid::from_bytes(nonce).simple()
    )));
    std::fs::create_dir(&directory.0)?;
    let work = directory.0.join("work");
    let ready = CancellationEvent::new();
    let stop = CancellationEvent::new();
    let _stop_guard = StopGuard(stop.clone());
    let (root, server) = peer(mode, lost, ready.clone(), stop.clone()).await?;
    let backend = Arc::new(ObservedBackend(AtomicUsize::new(0)));
    let resources = Tester {
        client: ApiClient::new(&root)?,
        token: Secret::new("synthetic-token".into()),
        project: "fixture".into(),
        data_root: directory.0.clone(),
        work_root: work.clone(),
        backend: backend.clone(),
        provisioner: Arc::new(NoCode),
        validator: Arc::new(NativeOutputValidator::runtime_policy()),
    };
    let worker: Arc<dyn RuntimeWorker> = match kind {
        0 => Arc::new(resources),
        1 => Arc::new(Experiment::new(resources)),
        2 => Arc::new(Evaluator {
            client: resources.client,
            token: resources.token,
            project: resources.project,
            policy: Arc::new(policy::parse_policy(
                document(
                    &json!({"schema_version":"0.2","evaluator":{"id":"stock","revision":"v1"},"gates":[{"id":"quality","metric":"mrr","split":"dev","statistic":"value","compare":"control","op":">=","min_delta":0.02}],"baselines":[]}),
                )?,
                PolicyEntryPoint::Evaluator,
                256,
            )?),
        }),
        _ => Arc::new(PolicyEvaluator::new(resources, step_policy()?)?),
    };
    let cancel = CancellationEvent::new();
    let mut operation = {
        let worker = worker.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move { worker.run_once(cancel).await })
    };
    let result = async {
        tokio::time::timeout(Duration::from_secs(10), ready.wait()).await?;
        if !lost {
            cancel.set();
        }
        let result = tokio::time::timeout(Duration::from_secs(10), &mut operation).await??;
        if lost {
            assert_eq!(result?.ok_or("missing abandoned job")?.state, "abandoned");
        } else {
            assert_eq!(result.err(), Some(RuntimeError::Cancelled));
        }
        if matches!(mode, Stall::Input | Stall::Failure) {
            assert_eq!(backend.0.load(Ordering::SeqCst), 1);
            assert_eq!(std::fs::read_dir(&work)?.count(), 0);
        } else {
            assert!(!work.exists());
        }
        Ok::<_, Error>(())
    }
    .await;
    cancel.set();
    if !operation.is_finished()
        && tokio::time::timeout(Duration::from_secs(10), &mut operation)
            .await
            .is_err()
    {
        // This test backend owns no step process. Abort a failed HTTP-only
        // regression before its private directory guard removes the tree.
        operation.abort();
        let _ = operation.await;
    }
    stop.set();
    server.await??;
    result
}
