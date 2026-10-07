//! Real official clients exercised against isolated Unix/TCP/WebSocket protocol peers.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "Protocol fixture assertions"
)]
#![allow(
    clippy::struct_excessive_bools,
    reason = "Independent wire fixture switches model daemon and API failure phases"
)]
#![allow(
    clippy::result_large_err,
    reason = "The official WebSocket handshake callback requires its concrete HTTP error type"
)]

#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;

use cannery_runner::{
    cancellation::CancellationEvent,
    container_backends::{
        Limits,
        docker::{DockerBackend, DockerOptions, GpuMode},
        kubernetes::{KubernetesBackend, KubernetesOptions},
    },
    launcher::{Manifest, Network, StepSpec},
    runtime::backend::Backend,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, UnixListener},
    sync::Notify,
};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{Request as WsRequest, Response as WsResponse},
    },
};

type Error = Box<dyn std::error::Error + Send + Sync>;
async fn idle_api() -> Result<(String, tokio::task::JoinHandle<()>), Error> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let root = format!("http://{}", listener.local_addr()?);
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("claim connection");
        let request = read_request(&mut stream).await.expect("claim request");
        assert_eq!(request.path, "/api/projects/fixture/jobs/claims");
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer fixture-api")
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&request.body).expect("claim body"),
            json!({"stage":"tester"})
        );
        json_reply(
            &mut stream,
            409,
            json!({"error":{"code":"nothing_to_claim"}}),
        )
        .await
        .expect("idle response");
    });
    Ok((root, task))
}

#[tokio::test]
#[ignore = "Requires the actual installed cannery CLI"]
async fn installed_container_factories_start_clients_and_claim_with_separate_credentials()
-> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    let binary = std::env::var_os("CANNERY_RUNNER_BINARY").ok_or("CLI binary required")?;
    for kubernetes in [false, true] {
        let root = Directory::new()?;
        let token = root.0.join("runner-token");
        std::fs::write(&token, "fixture-api")?;
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600))?;
        let cluster_token = root.0.join("cluster-token");
        std::fs::write(&cluster_token, "fixture-only")?;
        std::fs::set_permissions(&cluster_token, std::fs::Permissions::from_mode(0o600))?;
        let socket = root.0.join("engine.sock");
        let docker = if kubernetes {
            None
        } else {
            Some(DockerPeer::start(&socket)?)
        };
        let cluster = if kubernetes {
            Some(KubePeer::start().await?)
        } else {
            None
        };
        let (api, claimed) = idle_api().await?;
        let mut command = std::process::Command::new(&binary);
        command
            .env_remove("CANNERY_DATABASE_URL")
            .env_remove("CANNERY_SETTINGS")
            .args(["runner", "--once", "--api-url"])
            .arg(api)
            .args([
                "--project",
                "fixture",
                "--runner-id",
                "fixture-runner",
                "--token-file",
            ])
            .arg(&token)
            .arg("--data-root")
            .arg(&root.0)
            .arg("--work-root")
            .arg(&root.0);
        if let Some(cluster) = &cluster {
            command
                .args([
                    "--launcher",
                    "kubernetes",
                    "--k8s-namespace",
                    "fixture",
                    "--k8s-api-url",
                ])
                .arg(&cluster.url)
                .arg("--k8s-token-file")
                .arg(&cluster_token)
                .arg("--k8s-namespace-policy-acknowledged");
        } else {
            command
                .args(["--launcher", "docker", "--docker-host"])
                .arg(format!("unix://{}", socket.display()));
        }
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::task::spawn_blocking(move || command.output()),
        )
        .await???;
        assert!(
            output.status.success(),
            "installed container factory failed"
        );
        claimed.await?;
        if let Some(peer) = docker {
            assert!(
                peer.state
                    .lock()
                    .expect("state")
                    .requests
                    .iter()
                    .any(|(_, path)| path == "/version")
            );
            assert!(peer.state.lock().expect("state").containers.is_empty());
            peer.close().await?;
        }
        if let Some(peer) = cluster {
            assert!(
                peer.state
                    .lock()
                    .expect("state")
                    .requests
                    .iter()
                    .any(|(_, path)| path == "/version")
            );
            assert!(peer.state.lock().expect("state").pods.is_empty());
            peer.close().await?;
        }
    }
    Ok(())
}
fn source_reference() -> Value {
    serde_json::from_str(runtime_reference!(
        "/../../crates/runner/tests/fixtures/container_backend_reference.json"
    ))
    .expect("actual production builder fixture")
}
struct Directory(PathBuf);
impl Directory {
    fn new() -> Result<Self, Error> {
        let mut id = [0u8; 8];
        getrandom::fill(&mut id)?;
        let root = std::env::temp_dir().join(format!("cr-backend-{:x}", u64::from_le_bytes(id)));
        std::fs::create_dir(&root)?;
        std::fs::create_dir(root.join("inputs"))?;
        std::fs::create_dir(root.join("outputs"))?;
        std::fs::write(root.join("job.json"), b"{\"fixture\":true}")?;
        std::fs::write(root.join("inputs/value"), b"immutable input")?;
        Ok(Self(root))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn limits() -> Limits {
    Limits {
        input_bytes: 1 << 20,
        output_bytes: 1 << 20,
        files: 100,
        log_bytes: 1 << 20,
        scheduling: Duration::from_secs(5),
        transfer_idle: Duration::from_secs(5),
        cleanup: Duration::from_secs(5),
    }
}
fn spec(network: Network) -> StepSpec {
    StepSpec::from_manifest(
        &Manifest {
            image: String::from(&format!("fixture@sha256:{}", "a".repeat(64))),
            command: vec![String::from("/bin/step")],
            args: vec![String::from("argument")],
            env: vec![(String::from("WORKLOAD"), String::from("value"))],
            cpu: Some(String::from("500m")),
            memory: Some(String::from("256Mi")),
            gpus: None,
            network,
        },
        String::from("fixture-job"),
        String::from("fixture-step"),
    )
    .expect("valid recipe")
}
fn docker_options(root: &Path, socket: PathBuf) -> DockerOptions {
    DockerOptions {
        runner_id: "fixture-runner".into(),
        socket,
        uid: 10001,
        gid: 10001,
        job_root: root.into(),
        job_root_host: None,
        pids: 256,
        tmp_bytes: 16 << 20,
        shm_bytes: 8 << 20,
        default_cpu_millis: 1000,
        default_memory_bytes: 512 << 20,
        gpu_devices: vec![],
        gpu_mode: GpuMode::Nvidia,
        allow_unrestricted_egress: false,
        limits: limits(),
    }
}
fn kubernetes_options() -> KubernetesOptions {
    KubernetesOptions {
        runner_id: "fixture-runner".into(),
        namespace: "fixture".into(),
        uid: 10001,
        gid: 10001,
        volume_bytes: 1 << 30,
        storage_class: Some("fixture-storage".into()),
        gpu_runtime_class: Some("nvidia".into()),
        transfer_image: format!("busybox@sha256:{}", "b".repeat(64)),
        tmp_bytes: 16 << 20,
        shm_bytes: 8 << 20,
        default_cpu_millis: 1000,
        default_memory_bytes: 512 << 20,
        api_service_host: "10.43.0.1".into(),
        api_service_port: 443,
        namespace_policy_acknowledged: true,
        allow_unrestricted_egress: false,
        limits: limits(),
    }
}
struct HttpRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}
async fn read_request<S: AsyncRead + Unpin>(stream: &mut S) -> Result<HttpRequest, Error> {
    let mut bytes = Vec::new();
    let mut byte = [0];
    while !bytes.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await?;
        bytes.push(byte[0]);
        if bytes.len() > 65536 {
            return Err("oversized fixture request".into());
        }
    }
    let header = std::str::from_utf8(&bytes)?;
    let mut lines = header.split("\r\n");
    let mut first = lines.next().ok_or("line")?.split_whitespace();
    let method = first.next().ok_or("method")?.to_owned();
    let path = first.next().ok_or("path")?.to_owned();
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.to_lowercase(), value.trim().to_owned());
        }
    }
    let length = headers
        .get("content-length")
        .map(|v| v.parse::<usize>())
        .transpose()?
        .unwrap_or(0);
    if length > 1 << 20 {
        return Err("oversized fixture body".into());
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await?;
    Ok(HttpRequest {
        method,
        path,
        headers,
        body,
    })
}
async fn reply<S: AsyncWrite + Unpin>(
    stream: &mut S,
    status: u16,
    content: &str,
    body: &[u8],
) -> Result<(), Error> {
    let header = format!(
        "HTTP/1.1 {status} fixture\r\nContent-Type: {content}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await?;
    Ok(())
}
async fn json_reply<S: AsyncWrite + Unpin>(
    stream: &mut S,
    status: u16,
    body: Value,
) -> Result<(), Error> {
    reply(
        stream,
        status,
        "application/json",
        &serde_json::to_vec(&body)?,
    )
    .await
}
fn unversioned(path: &str) -> &str {
    if path.starts_with("/v1.") {
        path.get(path[1..].find('/').ok_or(()).unwrap_or(0) + 1..)
            .unwrap_or(path)
    } else {
        path
    }
}
#[derive(Default)]
struct DockerState {
    containers: BTreeMap<String, Value>,
    requests: Vec<(String, String)>,
    create_body: Option<Value>,
    oom: bool,
    hang: bool,
    killed: bool,
    start_error: Option<String>,
    hold_create: bool,
    refuse_remove: bool,
}
struct DockerPeer {
    state: Arc<Mutex<DockerState>>,
    started: Arc<Notify>,
    created: Arc<Notify>,
    continue_create: Arc<Notify>,
    stop: CancellationEvent,
    task: tokio::task::JoinHandle<()>,
}
impl DockerPeer {
    fn start(socket: &Path) -> Result<Self, Error> {
        let listener = UnixListener::bind(socket)?;
        let state = Arc::new(Mutex::new(DockerState::default()));
        let started = Arc::new(Notify::new());
        let created = Arc::new(Notify::new());
        let continue_create = Arc::new(Notify::new());
        let killed = Arc::new(Notify::new());
        let stop = CancellationEvent::new();
        let parts = (
            state.clone(),
            started.clone(),
            created.clone(),
            continue_create.clone(),
            killed.clone(),
            stop.clone(),
        );
        let task = tokio::spawn(async move {
            let (state, started, created, continue_create, killed, stop) = parts;
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                tokio::select! {()=stop.wait()=>break,answer=listener.accept()=>{let Ok((mut socket,_))=answer else{break;};let state=state.clone();let started=started.clone();let created=created.clone();let continue_create=continue_create.clone();let killed=killed.clone();tasks.spawn(async move{let answer=async{
                    let request=read_request(&mut socket).await?;let path=unversioned(&request.path).split('?').next().ok_or("path")?.to_owned();state.lock().expect("state").requests.push((request.method.clone(),path.clone()));
                    if path=="/version"{return json_reply(&mut socket,200,json!({"ApiVersion":"1.41","Version":"fixture"})).await;}
                    if path=="/containers/json"{let values=state.lock().expect("state").containers.keys().map(|id|json!({"Id":id})).collect::<Vec<_>>();return json_reply(&mut socket,200,json!(values)).await;}
                    if path.starts_with("/images/")&&path.ends_with("/json"){return json_reply(&mut socket,200,json!({"Id":"sha256:fixture"})).await;}
                    if path=="/containers/create"{let uri: http::Uri=request.path.parse()?;let name=uri.query().ok_or("query")?.split('&').find_map(|p|p.strip_prefix("name=")).ok_or("name")?.to_owned();let body:Value=serde_json::from_slice(&request.body)?;let hold={let mut guard=state.lock().expect("state");guard.create_body=Some(body.clone());guard.containers.insert(name.clone(),body);guard.hold_create};created.notify_one();if hold{continue_create.notified().await;}return json_reply(&mut socket,201,json!({"Id":name,"Warnings":[]})).await;}
                    let suffix=path.rsplit('/').next().ok_or("suffix")?;
                    match (request.method.as_str(),suffix){
                        ("POST","start")=>{let error=state.lock().expect("state").start_error.clone();started.notify_one();if let Some(message)=error{json_reply(&mut socket,400,json!({"message":message})).await}else{reply(&mut socket,204,"application/json",b"").await}},
                        ("POST","wait")=>{let wait={let state=state.lock().expect("state");state.hang&&!state.killed};if wait{killed.notified().await;}let killed=state.lock().expect("state").killed;json_reply(&mut socket,200,json!({"StatusCode":if killed{137}else{0}})).await},
                        ("POST","kill")=>{state.lock().expect("state").killed=true;killed.notify_waiters();reply(&mut socket,204,"application/json",b"").await},
                        ("GET","json")=>{let oom=state.lock().expect("state").oom;json_reply(&mut socket,200,json!({"State":{"Status":"exited","Running":false,"OOMKilled":oom,"ExitCode":0}})).await},
                        ("GET","logs")=>{let message=b"step stdout\nstep stderr\n";let mut frame=vec![1,0,0,0];frame.extend_from_slice(&u32::try_from(message.len())?.to_be_bytes());frame.extend_from_slice(message);reply(&mut socket,200,"application/vnd.docker.raw-stream",&frame).await},
                        ("DELETE",_)=>{let name=path.strip_prefix("/containers/").ok_or("container path")?;let refuse={let mut guard=state.lock().expect("state");if guard.refuse_remove{true}else{guard.containers.remove(name);false}};if refuse{json_reply(&mut socket,500,json!({"message":"fixture removal failure"})).await}else{reply(&mut socket,204,"application/json",b"").await}},
                        _=>Err("unexpected Docker request".into())
                    }
                }.await; if let Err(error)=answer{panic!("Docker fixture protocol failed: {error}");}});}}
            }
            stop.set();
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        });
        Ok(Self {
            state,
            started,
            created,
            continue_create,
            stop,
            task,
        })
    }
    async fn close(self) -> Result<(), Error> {
        self.stop.set();
        self.task.await?;
        Ok(())
    }
}
#[tokio::test]
async fn docker_real_client_success_oom_and_isolation() -> Result<(), Error> {
    let root = Directory::new()?;
    let socket = root.0.join("daemon.sock");
    let peer = DockerPeer::start(&socket)?;
    peer.state.lock().expect("state").oom = true;
    let backend = DockerBackend::new(docker_options(&root.0, socket))?;
    backend.start().await?;
    let prepared = backend.prepare(spec(Network::None), root.0.clone()).await?;
    let outcome = prepared
        .run(root.0.join("log"), 2.0, CancellationEvent::new())
        .await?;
    assert_eq!(outcome.exit_code, Some(0.into()));
    assert!(outcome.oom_killed);
    assert!(!outcome.cancelled);
    prepared.cleanup().await?;
    {
        let state = peer.state.lock().expect("state");
        let body = state.create_body.as_ref().expect("create");
        assert_eq!(body["Image"], format!("fixture@sha256:{}", "a".repeat(64)));
        assert_eq!(body["User"], "10001:10001");
        assert_eq!(body["HostConfig"]["NetworkMode"], "none");
        assert_eq!(body["HostConfig"]["CapDrop"], json!(["ALL"]));
        assert_eq!(body["HostConfig"]["ReadonlyRootfs"], true);
        assert_eq!(body["HostConfig"]["Memory"], 256 << 20);
        assert_eq!(body["HostConfig"]["MemorySwap"], 256 << 20);
        assert_eq!(body["HostConfig"]["NanoCpus"], 500_000_000);
        assert_eq!(body["HostConfig"]["Mounts"][0]["ReadOnly"], true);
        assert_eq!(body["HostConfig"]["Mounts"][1]["Target"], "/cr/outputs");
        assert_eq!(body["HostConfig"]["Mounts"][1]["ReadOnly"], false);
        assert_eq!(
            body["Env"],
            json!(["WORKLOAD=value", "CR_ROOT=/cr", "HOME=/tmp", "TMPDIR=/tmp"])
        );
        assert!(state.containers.is_empty());
    }
    assert_eq!(
        tokio::fs::read(root.0.join("log")).await?,
        b"step stdout\nstep stderr\n"
    );
    backend.release_job("fixture-job").await?;
    backend.close().await?;
    peer.close().await?;
    Ok(())
}
#[tokio::test]
async fn docker_real_client_deadline_cancel_drop_and_late_prepare() -> Result<(), Error> {
    for mode in ["deadline", "cancel", "drop", "prepare"] {
        let root = Directory::new()?;
        let socket = root.0.join("daemon.sock");
        let peer = DockerPeer::start(&socket)?;
        {
            let mut state = peer.state.lock().expect("state");
            state.hang = true;
            state.hold_create = mode == "prepare";
        }
        let backend = DockerBackend::new(docker_options(&root.0, socket))?;
        if mode == "prepare" {
            let owner = backend.clone();
            let path = root.0.clone();
            let pending =
                tokio::spawn(async move { owner.prepare(spec(Network::None), path).await });
            peer.created.notified().await;
            pending.abort();
            let _ = pending.await;
            peer.continue_create.notify_one();
            backend.close().await?;
        } else {
            let prepared = backend.prepare(spec(Network::None), root.0.clone()).await?;
            let cancel = CancellationEvent::new();
            let owner = prepared.clone();
            let path = root.0.join("log");
            let signal = cancel.clone();
            let pending = tokio::spawn(async move {
                owner
                    .run(path, if mode == "deadline" { 0.01 } else { 10.0 }, signal)
                    .await
            });
            peer.started.notified().await;
            if mode == "drop" {
                pending.abort();
                let _ = pending.await;
                prepared.cleanup().await?;
            } else {
                if mode == "cancel" {
                    cancel.set();
                }
                let outcome = pending.await??;
                assert_eq!(outcome.timed_out, mode == "deadline");
                assert_eq!(outcome.cancelled, mode == "cancel");
                assert_eq!(outcome.exit_code, Some(137.into()));
                prepared.cleanup().await?;
            }
            backend.close().await?;
        }
        assert!(
            peer.state.lock().expect("state").containers.is_empty(),
            "{mode}"
        );
        peer.close().await?;
    }
    Ok(())
}
#[tokio::test]
async fn docker_real_client_command_failures_and_preclaim_validation() -> Result<(), Error> {
    for (message, expected) in [
        ("executable file not found", Some(127)),
        ("permission denied", Some(126)),
        ("nvidia device permission denied", None),
    ] {
        let root = Directory::new()?;
        let socket = root.0.join("daemon.sock");
        let peer = DockerPeer::start(&socket)?;
        peer.state.lock().expect("state").start_error = Some(message.into());
        let options = docker_options(&root.0, socket);
        let backend = DockerBackend::new(options.clone())?;
        assert!(
            options
                .validate_step(&spec(Network::Egress(vec![String::from("example.com")])))
                .is_err()
        );
        let mut invalid = spec(Network::None);
        invalid
            .env
            .push((String::from("NVIDIA_VISIBLE_DEVICES"), String::from("all")));
        assert!(options.validate_step(&invalid).is_err());
        assert_eq!(
            peer.state.lock().expect("state").requests,
            [] as [(std::string::String, std::string::String); 0]
        );
        let prepared = backend.prepare(spec(Network::None), root.0.clone()).await?;
        let result = prepared
            .run(root.0.join("log"), 2.0, CancellationEvent::new())
            .await;
        if let Some(code) = expected {
            assert_eq!(result?.exit_code, Some(code.into()));
        } else {
            assert!(result.is_err());
        }
        prepared.cleanup().await?;
        backend.close().await?;
        assert!(peer.state.lock().expect("state").containers.is_empty());
        peer.close().await?;
    }
    Ok(())
}

#[derive(Default)]
struct KubeState {
    pods: BTreeMap<String, Value>,
    volumes: BTreeMap<String, Value>,
    created: Vec<Value>,
    inputs: Vec<u8>,
    logs: bool,
    hang: bool,
    oom: bool,
    invalid_output: bool,
    oversized_output: bool,
    requests: Vec<(String, String)>,
}
struct KubePeer {
    state: Arc<Mutex<KubeState>>,
    started: Arc<Notify>,
    url: String,
    stop: CancellationEvent,
    task: tokio::task::JoinHandle<()>,
}
fn status(code: u16) -> Value {
    json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","message":"fixture","code":code})
}
fn output_tar() -> Result<Vec<u8>, Error> {
    let mut builder = tar::Builder::new(Vec::new());
    let bytes = b"{\"result\":true}\n";
    let mut header = tar::Header::new_gnu();
    header.set_size(u64::try_from(bytes.len())?);
    header.set_mode(0o600);
    header.set_cksum();
    builder.append_data(&mut header, "outputs/result.json", &bytes[..])?;
    builder.finish()?;
    Ok(builder.into_inner()?)
}
impl KubePeer {
    async fn start() -> Result<Self, Error> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}", listener.local_addr()?);
        let state = Arc::new(Mutex::new(KubeState::default()));
        let started = Arc::new(Notify::new());
        let stop = CancellationEvent::new();
        let shared = (state.clone(), started.clone(), stop.clone());
        let task = tokio::spawn(async move {
            let (state, started, stop) = shared;
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                tokio::select! {()=stop.wait()=>break,accepted=listener.accept()=>{let Ok((mut socket,_))=accepted else{break};let state=state.clone();let started=started.clone();tasks.spawn(async move{let result=async{
                                 let mut peek=vec![0;65536];loop{let count=socket.peek(&mut peek).await?;if count==0{return Ok::<_,Error>(())}
                if peek[..count].windows(4).any(|v|v==b"\r\n\r\n"){peek.truncate(count);break;}tokio::task::yield_now().await;}
                                 if String::from_utf8_lossy(&peek).to_lowercase().contains("upgrade: websocket"){
                                  let mut websocket=accept_hdr_async(socket,|request:&WsRequest,mut response:WsResponse|{assert!(request.uri().path().ends_with("/exec"));assert!(request.headers().get("sec-websocket-protocol").expect("protocol").to_str().expect("header").contains("v5.channel.k8s.io"));response.headers_mut().insert("sec-websocket-protocol","v5.channel.k8s.io".parse().expect("header"));Ok(response)}).await?;
                                  let input=String::from_utf8_lossy(&peek).contains("stdin=true");
                                  if input{let mut archive=Vec::new();while let Some(message)=websocket.next().await{if let Message::Binary(data)=message?{if data.first()==Some(&0){archive.extend_from_slice(&data[1..]);}else if data.as_ref()==[255,0]{break;}}}state.lock().expect("state").inputs=archive;}else{let (invalid,large)={let guard=state.lock().expect("state");(guard.invalid_output,guard.oversized_output)};let mut payload=vec![1];payload.extend_from_slice(if large{vec![0;(1<<20)+1]}else if invalid{b"invalid archive".to_vec()}else{output_tar()?}.as_slice());websocket.send(Message::Binary(payload.into())).await?;if large{while websocket.next().await.is_some(){}return Ok(());}}
                                  for channel in [1,2]{websocket.send(Message::Binary(vec![255,channel].into())).await?;}let mut success=vec![3];success.extend_from_slice(b"{\"status\":\"Success\"}");websocket.send(Message::Binary(success.into())).await?;websocket.close(None).await?;return Ok(());
                                 }
                                 let request=read_request(&mut socket).await?;assert_eq!(request.headers.get("authorization").map(String::as_str),Some("Bearer fixture-only"));let path=request.path.split('?').next().ok_or("path")?.to_owned();state.lock().expect("state").requests.push((request.method.clone(),path.clone()));
                                 if path=="/version"{return json_reply(&mut socket,200,json!({"major":"1","minor":"34","gitVersion":"v1.34.0","gitCommit":"fixture","gitTreeState":"clean","buildDate":"2026-01-01T00:00:00Z","goVersion":"go1.24","compiler":"gc","platform":"linux/amd64"})).await;}
                                 let tail=path.strip_prefix("/api/v1/namespaces/fixture/").ok_or("namespace")?;let mut pieces=tail.split('/');let collection=pieces.next().ok_or("collection")?;let name=pieces.next();let action=pieces.next();
                                 if action==Some("log"){state.lock().expect("state").logs=true;started.notify_one();return reply(&mut socket,200,"text/plain",b"kubernetes step output\n").await;}
                                 let body={let mut guard=state.lock().expect("state");let kind=if collection=="pods"{"Pod"}else{"PersistentVolumeClaim"};match(request.method.as_str(),name){
                                  ("POST",None)=>{let mut body:Value=serde_json::from_slice(&request.body)?;let name=body["metadata"]["name"].as_str().ok_or("name")?.to_owned();guard.created.push(body.clone());if collection=="pods"{body["status"]=json!({"phase":"Running"});guard.pods.insert(name,body.clone());}else{guard.volumes.insert(name,body.clone());}Some((201,body))},
                                  ("GET",None)=>{let items=if collection=="pods"{guard.pods.values().cloned().collect::<Vec<_>>()}else{guard.volumes.values().cloned().collect::<Vec<_>>()};Some((200,json!({"apiVersion":"v1","kind":format!("{kind}List"),"metadata":{},"items":items})))},
                                  ("GET",Some(name))=>{let mut item=if collection=="pods"{guard.pods.get(name).cloned()}else{guard.volumes.get(name).cloned()};if collection=="pods"&&name.starts_with("cr-step-")&&guard.logs&&!guard.hang&& let Some(body)=&mut item{body["status"]=json!({"phase":"Succeeded","containerStatuses":[{"name":"step","ready":false,"restartCount":0,"image":"fixture","imageID":"fixture","state":{"terminated":{"exitCode":0,"reason":if guard.oom{"OOMKilled"}else{"Completed"}}}}]});}item.map(|v|(200,v))},
                                  ("DELETE",Some(name))=>{if collection=="pods"{guard.pods.remove(name);}else{guard.volumes.remove(name);}Some((200,json!({"apiVersion":"v1","kind":"Status","status":"Success"})))},
                                  _=>return Err("unexpected Kubernetes request".into())
                                 }};let(code,body)=body.unwrap_or((404,status(404)));json_reply(&mut socket,code,body).await
                                }.await;if let Err(error)=result{panic!("Kubernetes fixture protocol failed: {error}");}});}}
            }
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        });
        Ok(Self {
            state,
            started,
            url,
            stop,
            task,
        })
    }
    fn backend(&self) -> Result<KubernetesBackend, Error> {
        self.backend_with(kubernetes_options())
    }
    fn backend_with(&self, options: KubernetesOptions) -> Result<KubernetesBackend, Error> {
        let mut config = kube::Config::new(self.url.parse()?);
        config.auth_info.token = Some("fixture-only".into());
        Ok(KubernetesBackend::connect(options, config)?)
    }
    async fn close(self) -> Result<(), Error> {
        self.stop.set();
        self.task.await?;
        Ok(())
    }
}
#[tokio::test]
async fn kubernetes_real_client_transfer_isolation_and_cleanup() -> Result<(), Error> {
    let root = Directory::new()?;
    let peer = KubePeer::start().await?;
    peer.state.lock().expect("state").oom = true;
    let backend = peer.backend()?;
    backend.start().await?;
    let mut recipe = spec(Network::None);
    recipe.resources.gpus = 1.into();
    let prepared = backend.prepare(recipe, root.0.clone()).await?;
    let outcome = prepared
        .run(root.0.join("log"), 3.0, CancellationEvent::new())
        .await?;
    assert_eq!(outcome.exit_code, Some(0.into()));
    assert!(outcome.oom_killed);
    prepared.cleanup().await?;
    assert_eq!(
        tokio::fs::read(root.0.join("outputs/result.json")).await?,
        b"{\"result\":true}\n"
    );
    assert_eq!(
        tokio::fs::read(root.0.join("log")).await?,
        b"kubernetes step output\n"
    );
    {
        let state = peer.state.lock().expect("state");
        assert!(state.pods.is_empty());
        assert_eq!(state.volumes.len(), 1);
        let step = state
            .created
            .iter()
            .find(|v| {
                v["metadata"]["name"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("cr-step-"))
            })
            .expect("step");
        assert_eq!(step["spec"]["automountServiceAccountToken"], false);
        assert_eq!(step["spec"]["enableServiceLinks"], false);
        assert_eq!(step["spec"]["securityContext"]["runAsUser"], 10001);
        let container = &step["spec"]["containers"][0];
        let source = source_reference();
        let source = &source["kubernetes"];
        assert_eq!(container["securityContext"], source["container_security"]);
        assert_eq!(step["spec"]["securityContext"], source["pod_security"]);
        assert_eq!(container["resources"], source["resources"]);
        assert_eq!(step["spec"]["runtimeClassName"], source["runtime_class"]);
        assert_eq!(container["securityContext"]["readOnlyRootFilesystem"], true);
        assert_eq!(
            container["securityContext"]["capabilities"]["drop"],
            json!(["ALL"])
        );
        assert!(!container.to_string().contains("fixture-only"));
        let mut archive = tar::Archive::new(&state.inputs[..]);
        let names = archive
            .entries()?
            .map(|entry| entry.and_then(|e| e.path().map(std::borrow::Cow::into_owned)))
            .collect::<Result<Vec<_>, _>>()?;
        assert!(names.contains(&PathBuf::from("job.json")));
        assert!(names.contains(&PathBuf::from("inputs/value")));
    }
    backend.release_job("fixture-job").await?;
    backend.close().await?;
    assert!(peer.state.lock().expect("state").volumes.is_empty());
    peer.close().await?;
    Ok(())
}
#[tokio::test]
async fn kubernetes_real_client_cancel_drop_and_invalid_output() -> Result<(), Error> {
    for mode in ["cancel", "drop", "invalid", "deadline", "oversize"] {
        let root = Directory::new()?;
        let peer = KubePeer::start().await?;
        {
            let mut state = peer.state.lock().expect("state");
            state.hang = matches!(mode, "cancel" | "drop" | "deadline");
            state.invalid_output = mode == "invalid";
            state.oversized_output = mode == "oversize";
        }
        let mut options = kubernetes_options();
        if mode == "oversize" {
            options.limits.transfer_idle = Duration::from_millis(100);
        }
        let backend = peer.backend_with(options)?;
        let prepared = backend.prepare(spec(Network::None), root.0.clone()).await?;
        let cancel = CancellationEvent::new();
        let owner = prepared.clone();
        let signal = cancel.clone();
        let path = root.0.join("log");
        let running = tokio::spawn(async move {
            owner
                .run(path, if mode == "deadline" { 0.02 } else { 3.0 }, signal)
                .await
        });
        peer.started.notified().await;
        if mode == "drop" {
            running.abort();
            let _ = running.await;
            prepared.cleanup().await?;
        } else {
            if mode == "cancel" {
                cancel.set();
            }
            let outcome = running.await??;
            assert_eq!(outcome.cancelled, mode == "cancel");
            assert_eq!(outcome.timed_out, mode == "deadline");
            assert_eq!(
                outcome.output_error.is_some(),
                matches!(mode, "invalid" | "oversize")
            );
            prepared.cleanup().await?;
        }
        backend.release_job("fixture-job").await?;
        backend.close().await?;
        {
            let state = peer.state.lock().expect("state");
            assert!(state.pods.is_empty());
            assert!(state.volumes.is_empty());
        }
        peer.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn docker_real_client_gpu_modes_exclusive_wait_cancel_and_reuse() -> Result<(), Error> {
    for mode in [
        GpuMode::Nvidia,
        GpuMode::Cos {
            driver_root: PathBuf::from("/var/lib/nvidia"),
        },
    ] {
        let root = Directory::new()?;
        let socket = root.0.join("daemon.sock");
        let peer = DockerPeer::start(&socket)?;
        let mut options = docker_options(&root.0, socket);
        options.gpu_devices = vec![3];
        options.gpu_mode = mode.clone();
        let backend = DockerBackend::new(options.clone())?;
        let mut recipe = spec(Network::None);
        recipe.resources.gpus = 1.into();
        recipe.resources.cpu = None;
        recipe.resources.memory_bytes = None;
        let first = backend.prepare(recipe.clone(), root.0.clone()).await?;
        {
            let state = peer.state.lock().expect("state");
            let body = state.create_body.as_ref().expect("create");
            let host = &body["HostConfig"];
            assert_eq!(host["Memory"], 512 << 20);
            assert_eq!(host["NanoCpus"], 1_000_000_000);
            match mode {
                GpuMode::Nvidia => {
                    assert_eq!(host["DeviceRequests"][0]["DeviceIDs"], json!(["3"]));
                    assert_eq!(host["DeviceRequests"][0]["Capabilities"], json!([["gpu"]]));
                    assert!(host["Devices"].is_null());
                }
                GpuMode::Cos { .. } => {
                    assert_eq!(
                        host["Devices"],
                        json!([{"PathOnHost":"/dev/nvidia3","PathInContainer":"/dev/nvidia3","CgroupPermissions":"rw"},{"PathOnHost":"/dev/nvidiactl","PathInContainer":"/dev/nvidiactl","CgroupPermissions":"rw"},{"PathOnHost":"/dev/nvidia-uvm","PathInContainer":"/dev/nvidia-uvm","CgroupPermissions":"rw"}])
                    );
                    for (index, part) in [(2, "lib64"), (3, "bin")] {
                        assert_eq!(
                            host["Mounts"][index]["Source"],
                            format!("/var/lib/nvidia/{part}")
                        );
                        assert_eq!(host["Mounts"][index]["ReadOnly"], true);
                    }
                }
            }
        }
        let owner = backend.clone();
        let request = recipe.clone();
        let path = root.0.clone();
        let mut waiting = tokio::spawn(async move { owner.prepare(request, path).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut waiting)
                .await
                .is_err()
        );
        assert_eq!(peer.state.lock().expect("state").containers.len(), 1);
        first.cleanup().await?;
        let second = tokio::time::timeout(Duration::from_secs(2), waiting).await???;
        second.cleanup().await?;
        let held = backend.prepare(recipe.clone(), root.0.clone()).await?;
        let owner = backend.clone();
        let path = root.0.clone();
        let mut abandoned = tokio::spawn(async move { owner.prepare(recipe, path).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut abandoned)
                .await
                .is_err()
        );
        abandoned.abort();
        let _ = abandoned.await;
        held.cleanup().await?;
        let mut too_many = spec(Network::None);
        too_many.resources.gpus = 2.into();
        assert!(options.validate_step(&too_many).is_err());
        backend.close().await?;
        assert!(peer.state.lock().expect("state").containers.is_empty());
        peer.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn docker_real_client_matches_source_gpu_configuration() -> Result<(), Error> {
    let source = source_reference();
    for (name, mode) in [
        ("nvidia", GpuMode::Nvidia),
        (
            "cos",
            GpuMode::Cos {
                driver_root: PathBuf::from("/var/lib/nvidia"),
            },
        ),
    ] {
        let root = Directory::new()?;
        let peer = DockerPeer::start(&root.0.join("daemon.sock"))?;
        let mut options = docker_options(&root.0, root.0.join("daemon.sock"));
        options.gpu_mode = mode;
        options.gpu_devices = vec![3];
        let backend = DockerBackend::new(options)?;
        let mut recipe = spec(Network::None);
        recipe.resources.gpus = 1.into();
        let prepared = backend.prepare(recipe, root.0.clone()).await?;
        {
            let state = peer.state.lock().expect("state");
            let body = state.create_body.as_ref().expect("create");
            let host = &body["HostConfig"];
            let measured = json!({"devices":host["Devices"],"requests":host["DeviceRequests"],"driver_mounts":host["Mounts"].as_array().expect("mounts")[2..],"memory":host["Memory"],"cpu":host["NanoCpus"],"env":body["Env"]});
            assert_eq!(measured, source[name]);
        }
        prepared.cleanup().await?;
        backend.close().await?;
        peer.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn docker_gpu_failed_removal_never_releases_live_device() -> Result<(), Error> {
    let root = Directory::new()?;
    let peer = DockerPeer::start(&root.0.join("daemon.sock"))?;
    let mut options = docker_options(&root.0, root.0.join("daemon.sock"));
    options.gpu_devices = vec![3];
    // Allow ordinary daemon round trips under concurrent test load. Bound the
    // deliberately blocked GPU acquisition separately below.
    options.limits.scheduling = Duration::from_secs(5);
    let backend = DockerBackend::new(options)?;
    let mut recipe = spec(Network::None);
    recipe.resources.gpus = 1.into();
    let first = backend.prepare(recipe.clone(), root.0.clone()).await?;
    peer.state.lock().expect("state").refuse_remove = true;
    assert!(first.cleanup().await.is_err());
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            backend.prepare(recipe.clone(), root.0.clone())
        )
        .await
        .is_err()
    );
    assert_eq!(peer.state.lock().expect("state").containers.len(), 1);
    peer.state.lock().expect("state").refuse_remove = false;
    first.cleanup().await?;
    let reused = backend.prepare(recipe, root.0.clone()).await?;
    reused.cleanup().await?;
    backend.close().await?;
    peer.close().await?;
    Ok(())
}
