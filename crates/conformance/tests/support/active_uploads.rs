mod foundation {
    include!("lifecycle_support.rs");
}
use conformance::Result;
use foundation::{ATTEMPT, Actors, Call, JOB, Lease, World, object, sha, string};
use reqwest::{
    Method, ResponseBuilderExt,
    header::{HeaderMap, HeaderValue},
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{IpAddr, SocketAddr, TcpStream},
    time::{Duration, Instant},
};

struct HeldUpload {
    // Owning this socket ensures every error/panic closes the held body. A
    // blocked operation has finite I/O timeouts; no detached stream survives.
    stream: TcpStream,
    remaining: Vec<u8>,
    url: reqwest::Url,
}
impl HeldUpload {
    async fn start(world: &World, grant: &Value, body: &[u8]) -> Result<Self> {
        let url = grant_url(world, grant)?;
        let headers = grant["headers"]
            .as_object()
            .ok_or("grant headers absent")?
            .clone();
        let body = body.to_vec();
        tokio::task::spawn_blocking(move || {
            assert!(body.len() > 1, "held stream needs at least two bytes");
            let ip: IpAddr = url.host_str().ok_or("upload host absent")?.parse()?;
            assert!(ip.is_loopback(), "held upload must target loopback");
            let address = SocketAddr::new(ip, url.port_or_known_default().ok_or("upload port absent")?);
            let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
            stream.set_read_timeout(Some(Duration::from_secs(8)))?;
            stream.set_write_timeout(Some(Duration::from_secs(5)))?;
            let mut path = url.path().to_owned();
            if let Some(query) = url.query() {
                path.push('?');
                path.push_str(query);
            }
            write!(stream,"PUT {path} HTTP/1.1\r\nHost: {}:{}\r\nContent-Length: {}\r\nExpect: 100-continue\r\nConnection: close\r\n",url.host_str().ok_or("host absent")?,address.port(),body.len())?;
            for (name, value) in headers {
                let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())?;
                let value = reqwest::header::HeaderValue::from_str(value.as_str().ok_or("grant header not string")?)?;
                stream.write_all(name.as_str().as_bytes())?;
                stream.write_all(b": ")?;
                stream.write_all(value.as_bytes())?;
                stream.write_all(b"\r\n")?;
            }
            stream.write_all(b"\r\n")?;
            stream.flush()?;
            // HTTP100 is emitted only when the server reads the body. It is
            // an observable barrier after grant locking and slot admission.
            let interim = read_head(&mut stream)?;
            assert_eq!(status(&interim)?, 100, "server must admit the held body");
            let cut = body.len() / 2;
            stream.write_all(&body[..cut])?;
            stream.flush()?;
            Ok(Self { stream, remaining: body[cut..].to_vec(), url })
        }).await?
    }
    async fn finish(mut self) -> Result<reqwest::Response> {
        tokio::task::spawn_blocking(move || {
            self.stream.write_all(&self.remaining)?;
            self.stream.flush()?;
            let head = read_head(&mut self.stream)?;
            let status = status(&head)?;
            let mut headers = HeaderMap::new();
            for line in head.split("\r\n").skip(1).filter(|line| !line.is_empty()) {
                let (name, value) = line.split_once(':').ok_or("malformed response header")?;
                headers.append(
                    reqwest::header::HeaderName::from_bytes(name.as_bytes())?,
                    HeaderValue::from_str(value.trim())?,
                );
            }
            let mut wire = Vec::new();
            Read::by_ref(&mut self.stream)
                .take(1024 * 1024)
                .read_to_end(&mut wire)?;
            assert!(
                wire.len() < 1024 * 1024,
                "held upload response exceeds bound"
            );
            let body = if let Some(encoding) = headers.get(reqwest::header::TRANSFER_ENCODING) {
                assert_eq!(
                    encoding.to_str()?.to_ascii_lowercase(),
                    "chunked",
                    "unsupported transfer coding"
                );
                decode_chunks(&wire)?
            } else {
                if let Some(length) = headers.get(reqwest::header::CONTENT_LENGTH) {
                    assert_eq!(
                        wire.len(),
                        length.to_str()?.parse::<usize>()?,
                        "wire Content-Length mismatch"
                    );
                }
                wire
            };
            let mut response = axum::http::Response::builder()
                .url(self.url.clone())
                .status(status)
                .body(body)?;
            *response.headers_mut() = headers;
            Ok(reqwest::Response::from(response))
        })
        .await?
    }
}
fn read_head(stream: &mut TcpStream) -> Result<String> {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        head.push(byte[0]);
        assert!(head.len() < 32 * 1024, "HTTP header exceeds bound");
    }
    Ok(String::from_utf8(head)?)
}
fn status(head: &str) -> Result<u16> {
    head.lines()
        .next()
        .ok_or("status line absent")?
        .split_whitespace()
        .nth(1)
        .ok_or("status code absent")?
        .parse()
        .map_err(Into::into)
}
fn decode_chunks(wire: &[u8]) -> Result<Vec<u8>> {
    let mut offset = 0;
    let mut body = Vec::new();
    loop {
        let end = wire[offset..]
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("chunk header absent")?
            + offset;
        let line = std::str::from_utf8(&wire[offset..end])?;
        let size = usize::from_str_radix(line.split(';').next().ok_or("chunk size absent")?, 16)?;
        offset = end + 2;
        if size == 0 {
            // Match reqwest: consume trailers without merging them into the
            // response's initial header map or the decoded representation.
            loop {
                let end = wire[offset..]
                    .windows(2)
                    .position(|w| w == b"\r\n")
                    .ok_or("trailer terminator absent")?
                    + offset;
                if end == offset {
                    assert_eq!(end + 2, wire.len(), "bytes after chunked message");
                    return Ok(body);
                }
                assert!(wire[offset..end].contains(&b':'), "malformed trailer");
                offset = end + 2;
            }
        }
        let end = offset.checked_add(size).ok_or("chunk size overflow")?;
        let delimiter_end = end.checked_add(2).ok_or("chunk delimiter overflow")?;
        assert!(delimiter_end <= wire.len(), "truncated chunk");
        body.extend_from_slice(&wire[offset..end]);
        assert_eq!(&wire[end..end + 2], b"\r\n", "chunk delimiter");
        offset = end + 2;
    }
}
fn grant_url(world: &World, grant: &Value) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(&string(&grant["upload_url"])?)?;
    let base = reqwest::Url::parse(&world.base_url)?;
    assert!(
        url.origin() == base.origin(),
        "upload capability must remain on API origin"
    );
    assert_eq!(url.scheme(), "http", "held fixture uses loopback HTTP");
    Ok(url)
}
fn put_request(world: &World, grant: &Value, bytes: &[u8]) -> Result<reqwest::RequestBuilder> {
    let url = grant_url(world, grant)?;
    let mut path = url.path().to_owned();
    if let Some(query) = url.query() {
        path.push('?');
        path.push_str(query);
    }
    let mut request = world
        .h
        .request(Method::PUT, &path)?
        .timeout(Duration::from_secs(8))
        .body(bytes.to_vec());
    for (name, value) in grant["headers"].as_object().ok_or("headers absent")? {
        request = request.header(name, string(value)?);
    }
    Ok(request)
}
async fn receiving(world: &mut World, grant: &Value, bytes: &[u8], job: bool) -> Result<()> {
    let response = put_request(world, grant, bytes)?.send().await?;
    let response = world
        .h
        .check_response(
            Method::PUT,
            if job {
                "/api/job-uploads/{upload_id}"
            } else {
                "/api/uploads/{upload_id}"
            },
            response,
            409,
        )
        .await?;
    assert_eq!(response.body["error"]["code"], "conflict");
    Ok(())
}
struct Prepared {
    lease: Lease,
    sheet: Value,
    candidate: Value,
}
async fn prepare(world: &mut World, actors: &Actors, title: &str) -> Result<Prepared> {
    let number = world.queue(actors, title).await?;
    let lease = world.claim(actors, number, false).await?;
    let bytes = include_bytes!("../../../../examples/fixture/candidate.json");
    let grant=world.api(Call::post(&format!("{ATTEMPT}/uploads"),format!("{}/uploads",world.attempt_path(&lease)),&actors.agent,json!({"role":"candidate","name":"candidate.json","size_bytes":bytes.len(),"sha256":sha(bytes),"media_type":"application/json"}),201).lease(&lease)?).await?.body;
    let candidate = world
        .h
        .check_response(
            Method::PUT,
            "/api/uploads/{upload_id}",
            put_request(world, &grant, bytes)?.send().await?,
            201,
        )
        .await?
        .body;
    let manifest=world.api(Call::post(&format!("{ATTEMPT}/manifest"),format!("{}/manifest",world.attempt_path(&lease)),&actors.agent,json!({"schema_version":"0.2","attempt_id":lease.document["id"],"objects":[object(&candidate)]}),201).lease(&lease)?).await?.body;
    let mut sheet: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/contracts/evidence_envelope/valid/agent.json"
    ))?;
    sheet["attempt_id"] = lease.document["id"].clone();
    sheet["manifest"] = manifest;
    sheet["provenance"]["science_revision"] = json!("1");
    sheet["artifact_roles"] = json!(["candidate"]);
    sheet["measurements"] = json!([{"metric":"mrr","value":0.9,"authority":"agent_claim","unit":"ratio","direction":"higher","split":"dev","dimensions":{"language":"en"}}]);
    Ok(Prepared {
        lease,
        sheet,
        candidate,
    })
}
async fn submit(world: &mut World, actors: &Actors, prepared: &Prepared) -> Result<Value> {
    Ok(world
        .api(
            Call::post(
                &format!("{ATTEMPT}/submission"),
                format!("{}/submission", world.attempt_path(&prepared.lease)),
                &actors.agent,
                prepared.sheet.clone(),
                201,
            )
            .lease(&prepared.lease)?,
        )
        .await?
        .body)
}
async fn job_grant(
    world: &mut World,
    actors: &Actors,
    lease: &Lease,
    path: &str,
    bytes: &[u8],
    interface: Option<&str>,
) -> Result<Value> {
    let mut body = json!({"role":"per_query_results","path":path,"size_bytes":bytes.len(),"sha256":sha(bytes),"media_type":"application/jsonl"});
    if let Some(interface) = interface {
        body["interface"] = json!(interface);
    }
    Ok(world
        .api(
            Call::post(
                &format!("{JOB}/uploads"),
                format!("{}/uploads", world.job_path(lease)?),
                &actors.tester,
                body,
                201,
            )
            .lease(lease)?,
        )
        .await?
        .body)
}
async fn completion(world: &mut World, actors: &Actors, lease: &Lease) -> Result<Value> {
    let evidence = json!({"schema_version":"0.2","attempt_id":lease.document["attempt_id"],"stage":"tester","status":"completed","producer":{"kind":"service","id":"cannery-runner"},"started_at":"2026-09-29T00:00:00Z","finished_at":"2026-09-29T00:01:00Z","provenance":{"source_revision":"4f2a9c1","tester_revision":"runner-fixture-1","dataset_revision":"qrels-r1","control_revision":"fixture-r1","science_revision":"1"},"observations":"Fixture verified outputs","measurements":[{"metric":"mrr","value":1.0,"authority":"tester_verified","unit":"ratio","direction":"higher","split":"dev","dimensions":{"language":"en"},"sample_count":2},{"metric":"mrr","value":0.5,"authority":"tester_verified","unit":"ratio","direction":"higher","split":"dev","dimensions":{"language":"fr"},"sample_count":2}],"discrepancies":[],"artifact_roles":["evidence"]});
    let mut objects = Vec::new();
    for (role, path, bytes, media) in [
        (
            "evidence",
            "fixture-scorer/evidence/evidence.json",
            serde_json::to_vec(&evidence)?,
            "application/json",
        ),
        (
            "step_log",
            "fixture-scorer/step_log/log.txt",
            b"scored\n".to_vec(),
            "text/plain",
        ),
    ] {
        let grant=world.api(Call::post(&format!("{JOB}/uploads"),format!("{}/uploads",world.job_path(lease)?),&actors.tester,json!({"role":role,"path":path,"size_bytes":bytes.len(),"sha256":sha(&bytes),"media_type":media}),201).lease(lease)?).await?.body;
        let artifact = world
            .h
            .check_response(
                Method::PUT,
                "/api/job-uploads/{upload_id}",
                put_request(world, &grant, &bytes)?.send().await?,
                201,
            )
            .await?
            .body;
        objects.push(object(&artifact));
    }
    Ok(
        json!({"schema_version":"0.2","job_id":lease.document["job_id"],"evidence":evidence,"manifest":{"schema_version":"0.2","attempt_id":lease.document["attempt_id"],"objects":objects}}),
    )
}
fn leased_request(
    world: &World,
    actors: &Actors,
    lease: &Lease,
    path: &str,
    body: &Value,
) -> Result<reqwest::RequestBuilder> {
    Ok(world
        .h
        .request(Method::POST, path)?
        .timeout(Duration::from_secs(8))
        .bearer_auth(&actors.tester)
        .header("X-Lease-Token", &lease.token)
        .header("X-Lease-Generation", lease.generation.to_string())
        .json(&body))
}
async fn download(world: &mut World, admin: &str, artifact: &Value) -> Result<Vec<u8>> {
    let response = world
        .h
        .request(
            Method::GET,
            &format!("{}/artifacts/{}", world.base(), string(&artifact["id"])?),
        )?
        .bearer_auth(admin)
        .send()
        .await?;
    Ok(world
        .h
        .check_response(
            Method::GET,
            "/api/projects/{slug}/artifacts/{artifact_id}",
            response,
            200,
        )
        .await?
        .raw_body)
}
fn safe(
    error: Box<dyn std::error::Error + Send + Sync>,
) -> Box<dyn std::error::Error + Send + Sync> {
    match error.downcast::<reqwest::Error>() {
        Ok(error) => Box::new(error.without_url()),
        Err(error) => error,
    }
}
