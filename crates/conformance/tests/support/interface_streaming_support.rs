use super::lifecycle::{ATTEMPT, Actors, Call, JOB, Lease, World, object, sha, string};
use conformance::Result;
use reqwest::Method;
use serde_json::{Value, json};

pub async fn prepared(label: &str, interfaces: Vec<Value>) -> Result<(World, Actors, Lease)> {
    let (mut world, actors) = submitted(label, interfaces, false).await?;
    let job = world.claim_job(&actors, false, false).await?;
    Ok((world, actors, job))
}

pub async fn submitted(
    label: &str,
    interfaces: Vec<Value>,
    runner: bool,
) -> Result<(World, Actors)> {
    let (mut world, actors) = World::new(label).await?;
    let mut science: Value =
        serde_json::from_str(include_str!("../../../../examples/fixture/science.json"))?;
    science["interfaces"]
        .as_array_mut()
        .ok_or("interfaces missing")?
        .extend(interfaces);
    if runner {
        science["limits"]["max_output_bytes"] = json!(90_000_000);
        let mut selected = science["interfaces"]
            .as_array()
            .ok_or("interfaces missing")?
            .last()
            .ok_or("runner interface missing")?
            .clone();
        selected["name"] = json!("ranked-run");
        science["interfaces"][0] = selected;
    }
    world
        .api(Call::post(
            "/api/projects/{slug}/config/{kind}",
            format!("{}/config/science", world.base()),
            &actors.admin,
            science,
            201,
        ))
        .await?;
    if runner {
        let mut producer: Value = serde_json::from_str(include_str!(
            "../../../../examples/fixture/producers/overlap-producer.json"
        ))?;
        producer["spec"]["container"]["command"] = json!(["sh", "write-output.sh"]);
        world
            .api(Call::post(
                "/api/projects/{slug}/producers",
                format!("{}/producers", world.base()),
                &actors.admin,
                producer,
                201,
            ))
            .await?;
        let response=world.h.request(Method::PATCH,&format!("{}/tracks/lexical",world.base()))?
            .bearer_auth(&actors.admin)
            .json(&json!({"expected_revision":1,"producer":{"name":"overlap-producer","revision":2},"reason":"Run bounded synthetic file checks"}))
            .send().await?;
        if response.status().as_u16() != 200 {
            // Only this synthetic non-secret configuration request may emit
            // its structured error; never apply this diagnostic to token APIs.
            let diagnostic: Value = response.json().await?;
            return Err(format!("synthetic track fixture refused: {}", diagnostic["error"]).into());
        }
        world
            .h
            .check_response(
                Method::PATCH,
                "/api/projects/{slug}/tracks/{track_slug}",
                response,
                200,
            )
            .await?;
    }
    let number = world.queue(&actors, "Interface content checks").await?;
    let lease = world.claim(&actors, number, false).await?;
    let path = world.attempt_path(&lease);
    let bytes = include_bytes!("../../../../examples/fixture/candidate.json");
    let grant = world.api(Call::post(&format!("{ATTEMPT}/uploads"), format!("{path}/uploads"), &actors.agent, json!({"role":"candidate","name":"candidate.json","size_bytes":bytes.len(),"sha256":sha(bytes),"media_type":"application/json"}),201).lease(&lease)?).await?.body;
    let artifact = world.put(&grant, bytes, false).await?;
    let manifest = world.api(Call::post(&format!("{ATTEMPT}/manifest"), format!("{path}/manifest"), &actors.agent, json!({"schema_version":"0.2","attempt_id":lease.document["id"],"objects":[object(&artifact)]}),201).lease(&lease)?).await?.body;
    let mut sheet: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/contracts/evidence_envelope/valid/agent.json"
    ))?;
    sheet["attempt_id"] = lease.document["id"].clone();
    sheet["manifest"] = manifest;
    sheet["provenance"]["science_revision"] = json!("2");
    sheet["artifact_roles"] = json!(["candidate"]);
    sheet["measurements"] = json!([{"metric":"mrr","value":0.9,"authority":"agent_claim","unit":"ratio","direction":"higher","split":"dev","dimensions":{"language":"en"}}]);
    world
        .api(
            Call::post(
                &format!("{ATTEMPT}/submission"),
                format!("{path}/submission"),
                &actors.agent,
                sheet,
                201,
            )
            .lease(&lease)?
            .key("interface-submission")?,
        )
        .await?;
    Ok((world, actors))
}

pub fn registration(name: &str, mut properties: Value) -> Value {
    properties["name"] = json!(name);
    properties["version"] = json!(1);
    properties
}

pub async fn grant(
    world: &mut World,
    actors: &Actors,
    job: &Lease,
    name: &str,
    interface: &str,
    bytes: &[u8],
) -> Result<Value> {
    let path = world.job_path(job)?;
    world
        .api(
            Call::post(
                &format!("{JOB}/heartbeat"),
                format!("{path}/heartbeat"),
                &actors.tester,
                json!({}),
                200,
            )
            .lease(job)?,
        )
        .await?;
    Ok(world.api(Call::post(&format!("{JOB}/uploads"), format!("{path}/uploads"), &actors.tester, json!({"role":"interface_probe","path":format!("interface-probes/{name}"),"interface":format!("{interface}/v1"),"size_bytes":bytes.len(),"sha256":sha(bytes),"media_type":"application/octet-stream"}),201).lease(job)?).await?.body)
}

pub async fn put(world: &mut World, grant: &Value, bytes: &[u8], expected: u16) -> Result<Value> {
    let url = reqwest::Url::parse(&string(&grant["upload_url"])?)?;
    let mut request = world
        .h
        .request(Method::PUT, url.path())?
        .body(bytes.to_vec());
    for (name, value) in grant["headers"].as_object().ok_or("headers missing")? {
        request = request.header(name, string(value)?);
    }
    Ok(world
        .h
        .check_response(
            Method::PUT,
            "/api/job-uploads/{upload_id}",
            request.send().await?,
            expected,
        )
        .await?
        .body)
}

/// Transport chunks are representative; ASGI may split or combine them before checker.feed.
/// This method intentionally makes no Harness coverage entry for its manually parsed response.
pub async fn chunked(grant: &Value, bytes: Vec<u8>, chunk_size: usize) -> Result<(u16, Value)> {
    let url = reqwest::Url::parse(&string(&grant["upload_url"])?)?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("chunked upload requires a plain loopback URL".into());
    }
    let port = url.port_or_known_default().ok_or("upload port missing")?;
    let path = url.path().to_owned();
    let secret = string(&grant["headers"]["X-Upload-Token"])?;
    if secret.contains(['\r', '\n']) {
        return Err("invalid capability header".into());
    }
    tokio::task::spawn_blocking(move || -> Result<(u16,Value)> {
        use std::io::{Read,Write};
        let mut socket=std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST,port)).map_err(|_|"chunked upload connection failed")?;
        socket.set_read_timeout(Some(std::time::Duration::from_secs(90)))?;
        socket.set_write_timeout(Some(std::time::Duration::from_secs(90)))?;
        write!(socket,"PUT {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Upload-Token: {secret}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\nContent-Type: application/octet-stream\r\n\r\n").map_err(|_|"chunked request headers failed")?;
        for chunk in bytes.chunks(chunk_size) {
            write!(socket,"{:x}\r\n",chunk.len()).map_err(|_|"chunked request failed")?;
            socket.write_all(chunk).map_err(|_|"chunked request failed")?;
            socket.write_all(b"\r\n").map_err(|_|"chunked request failed")?;
        }
        socket.write_all(b"0\r\n\r\n").map_err(|_|"chunked request failed")?;
        let mut response=Vec::new();
        socket.take(1024*1024).read_to_end(&mut response).map_err(|_|"chunked response failed")?;
        let split=response.windows(4).position(|v|v==b"\r\n\r\n").ok_or("chunked response headers absent")?;
        let headers=std::str::from_utf8(&response[..split])?;
        let status=headers.lines().next().and_then(|v|v.split_whitespace().nth(1)).ok_or("response status absent")?.parse()?;
        if !headers.to_ascii_lowercase().contains("content-type: application/json") {return Err("chunked response is not JSON".into());}
        let body=serde_json::from_slice(&response[split+4..]).map_err(|_|"chunked response JSON invalid")?;
        Ok((status,body))
    }).await?
}

struct Work(std::path::PathBuf);
impl Drop for Work {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub async fn runner_file(world: &World, actors: &Actors, mode: &str) -> Result<()> {
    use std::{
        io::Write,
        os::unix::fs::OpenOptionsExt,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let root = std::path::PathBuf::from(std::env::var("CANNERY_CONFORMANCE_WORK_DIR")?)
        .join(format!("interfaces-{}", world.project));
    std::fs::create_dir(&root)?;
    let work = Work(root);
    let step_root = work.0.join("steps");
    std::fs::create_dir(&step_root)?;
    // A bounded valid JSON file exceeds the check_file document parse cap.
    // This trusted shell fixture runs only inside the parent container's CLI process.
    let generation = match mode {
        "json-cap" => {
            "printf '{\"padding\":\"'; head -c 68157440 /dev/zero | tr '\\000' x; printf '\"}'"
        }
        "line-cap" => {
            "printf '{\"padding\":\"'; head -c 68157440 /dev/zero | tr '\\000' x; printf '\"}\\n'"
        }
        "lines" => {
            "i=0; while [ \"$i\" -lt 10000 ]; do printf '{\"query\":\"q\",\"score\":1}\\n'; i=$((i+1)); done; printf '[\"not\",\"object\"]\\n'"
        }
        _ => return Err("unknown runner file fixture".into()),
    };
    std::fs::write(
        step_root.join("write-output.sh"),
        format!(
            "set -eu\nmkdir -p \"$CR_ROOT/outputs/run\"\n{{ {generation}; }} > \"$CR_ROOT/outputs/run/run.json\"\n"
        ),
    )?;
    let token = work.0.join("tester.token");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&token)?;
    file.write_all(actors.tester.as_bytes())?;
    let binary = std::env::var("CANNERY_CONFORMANCE_CLI")?;
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/fixture/data")
        .canonicalize()?;
    let args = vec![
        "runner".to_owned(),
        "--api-url".into(),
        world.base_url.clone(),
        "--project".into(),
        world.project.clone(),
        "--data-root".into(),
        fixture.display().to_string(),
        "--step-root".into(),
        step_root.display().to_string(),
        "--work-root".into(),
        work.0.join("work").display().to_string(),
        "--unisolated-local".into(),
        "--once".into(),
    ];
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut child = Command::new(binary)
            .args(args)
            .env("CANNERY_RUNNER_TOKEN_FILE", token)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let until = Instant::now() + Duration::from_secs(120);
        loop {
            if child.try_wait()?.is_some() {
                let output = child.wait_with_output()?;
                if !output.status.success() {
                    return Err("runner CLI failed to report its outcome".into());
                }
                return Ok(());
            }
            if Instant::now() > until {
                child.kill()?;
                let _ = child.wait();
                return Err("runner CLI exceeded bounded deadline".into());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    })
    .await??;
    drop(work);
    Ok(())
}
