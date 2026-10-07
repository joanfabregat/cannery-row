use conformance::Harness;
use reqwest::{Client, Method, header};
use serde_json::{Value, json};
use std::{
    error::Error,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

struct Session {
    cookie: String,
    csrf: String,
    id: String,
}
struct World {
    harness: Harness,
    base: String,
    client: Client,
}
impl World {
    fn new() -> Result<Self> {
        let base = std::env::var("CANNERY_CONFORMANCE_URL")?;
        Ok(Self {
            harness: Harness::new(&base)?,
            base,
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }
    async fn api(
        &mut self,
        method: Method,
        template: &str,
        path: &str,
        session: Option<&Session>,
        token: Option<&str>,
        body: Option<Value>,
        status: u16,
    ) -> Result<Value> {
        let mut request = self.harness.request(method.clone(), path)?;
        if let Some(session) = session {
            request = request
                .header(header::COOKIE, &session.cookie)
                .header("X-CSRF-Token", &session.csrf);
        }
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        Ok(self
            .harness
            .check_response(method, template, request.send().await?, status)
            .await?
            .body)
    }
    async fn login(&mut self, subject: &str, email: &str) -> Result<Session> {
        let login = self
            .client
            .get(format!("{}/auth/login?return_to=%2F", self.base))
            .send()
            .await?;
        assert_eq!(login.status().as_u16(), 302);
        let cookie = response_cookie(&login, "cr_login")?;
        let location = login
            .headers()
            .get(header::LOCATION)
            .ok_or("login omitted location")?
            .to_str()?;
        let provider = std::env::var("CANNERY_CONFORMANCE_OIDC_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:9011".into());
        let control = std::env::var("CANNERY_TEST_OIDC_CONTROL_TOKEN")?;
        let approved = self.client.post(format!("{provider}/__conformance/approve")).bearer_auth(control).json(&json!({"authorization_url":location,"claims":{"sub":subject,"email":email,"email_verified":true,"name":subject}})).send().await?;
        assert_eq!(approved.status().as_u16(), 200);
        let approved: Value = approved.json().await?;
        let callback = self
            .client
            .get(format!("{}/auth/callback", self.base))
            .header(header::COOKIE, cookie)
            .query(&[
                ("state", string(&approved, "state")?),
                ("code", string(&approved, "code")?),
            ])
            .send()
            .await?;
        assert_eq!(callback.status().as_u16(), 302);
        let cookie = response_cookie(&callback, "cr_session")?;
        let me = self
            .api(Method::GET, "/api/me", "/api/me", None, None, None, 401)
            .await?;
        assert_code(&me, "unauthenticated");
        let response = self
            .harness
            .request(Method::GET, "/api/me")?
            .header(header::COOKIE, &cookie)
            .send()
            .await?;
        let me = self
            .harness
            .check_response(Method::GET, "/api/me", response, 200)
            .await?
            .body;
        Ok(Session {
            cookie,
            csrf: string(&me, "csrf_token")?.into(),
            id: me
                .get("user")
                .and_then(|u| u.get("id"))
                .or_else(|| me.get("user_id"))
                .and_then(Value::as_str)
                .ok_or("me omitted user id")?
                .into(),
        })
    }
    async fn token(
        &mut self,
        session: &Session,
        name: &str,
        scopes: &[&str],
    ) -> Result<(String, String)> {
        let token = self
            .api(
                Method::POST,
                "/api/tokens",
                "/api/tokens",
                Some(session),
                None,
                Some(json!({"name":name,"expires_in_days":1,"scopes":scopes})),
                201,
            )
            .await?;
        let secret = string(&token, "token")?.to_owned();
        assert!(secret.starts_with("cr_pat_"));
        Ok((secret, string(&token, "id")?.into()))
    }
    fn export(&self) -> Result<()> {
        if let Ok(directory) = std::env::var("CANNERY_CONFORMANCE_COVERAGE_DIR") {
            fs::create_dir_all(&directory)?;
            let coverage = self.harness.coverage();
            fs::write(
                PathBuf::from(directory).join("runner-fetch-cache.json"),
                serde_json::to_vec_pretty(
                    &json!({"operations":coverage.operations,"tools":coverage.tools,"audit_actions":coverage.audit_actions}),
                )?,
            )?;
        }
        Ok(())
    }
}
fn response_cookie(response: &reqwest::Response, name: &str) -> Result<String> {
    for cookie in response.headers().get_all(header::SET_COOKIE) {
        let value = cookie.to_str()?.split(';').next().ok_or("empty cookie")?;
        if value.starts_with(&format!("{name}=")) {
            return Ok(value.into());
        }
    }
    Err(format!("missing {name} cookie").into())
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string {key}").into())
}
fn assert_code(value: &Value, code: &str) {
    assert_eq!(
        value.pointer("/error/code").and_then(Value::as_str),
        Some(code),
        "response error code must match"
    );
}
fn fixture(path: &str) -> Result<Value> {
    Ok(serde_json::from_str(&fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path),
    )?)?)
}
fn unique() -> Result<String> {
    Ok(format!(
        "conformance-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis()
    ))
}
use ring::digest::{SHA256, digest};
use std::{
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

fn sha(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in digest(&SHA256, bytes).as_ref() {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 15)]));
    }
    output
}
struct Work(PathBuf);
impl Drop for Work {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn private_file(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    Ok(())
}
async fn cli(args: Vec<String>, token_env: Option<(&str, &std::path::Path)>) -> Result<Output> {
    let binary = std::env::var("CANNERY_CONFORMANCE_CLI")?;
    let token_env = token_env.map(|(name, path)| (name.to_owned(), path.to_owned()));
    tokio::task::spawn_blocking(move || {
        let mut command = Command::new(binary);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some((name, path)) = token_env {
            command.env(name, path);
        }
        let mut child = command.spawn()?;
        let until = Instant::now() + Duration::from_secs(45);
        loop {
            if child.try_wait()?.is_some() {
                return Ok(child.wait_with_output()?);
            }
            if Instant::now() > until {
                child.kill()?;
                let _ = child.wait();
                return Err("CLI worker exceeded 45 seconds".into());
            }
            thread::sleep(Duration::from_millis(25));
        }
    })
    .await?
}
fn success(output: &Output, expected: &str) {
    assert!(
        output.status.success(),
        "CLI failed with exit status {}",
        output.status
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(expected),
        "CLI omitted expected outcome {expected}"
    );
}
fn toml_string(value: &str) -> String {
    Value::String(value.to_owned()).to_string()
}
async fn account(
    world: &mut World,
    admin: &Session,
    token: &str,
    base: &str,
    kind: &str,
    name: &str,
) -> Result<String> {
    world
        .api(
            Method::POST,
            "/api/projects/{slug}/service-accounts",
            &format!("{base}/service-accounts"),
            None,
            Some(token),
            Some(json!({"kind":kind,"name":name})),
            201,
        )
        .await?;
    let response = world
        .api(
            Method::POST,
            "/api/projects/{slug}/service-accounts/{name}/tokens",
            &format!("{base}/service-accounts/{name}/tokens"),
            Some(admin),
            None,
            Some(json!({"name":"cli","expires_in_days":1,"scopes":["read","write"]})),
            201,
        )
        .await?;
    Ok(string(&response, "token")?.to_owned())
}
async fn lease_api(
    world: &mut World,
    method: Method,
    template: &str,
    path: &str,
    token: &str,
    lease: &Value,
    body: Value,
    status: u16,
) -> Result<Value> {
    let response = world
        .harness
        .request(method.clone(), path)?
        .bearer_auth(token)
        .header("X-Lease-Token", string(lease, "lease_token")?)
        .header(
            "X-Lease-Generation",
            lease["lease_generation"]
                .as_u64()
                .ok_or("missing generation")?,
        )
        .json(&body)
        .send()
        .await?;
    Ok(world
        .harness
        .check_response(method, template, response, status)
        .await?
        .body)
}
async fn draft(
    world: &mut World,
    base: &str,
    agent: &str,
    admin: &str,
    document: Value,
) -> Result<u64> {
    let created = world
        .api(
            Method::POST,
            "/api/projects/{slug}/hypotheses",
            &format!("{base}/hypotheses"),
            None,
            Some(agent),
            Some(document),
            201,
        )
        .await?;
    let number = created["number"].as_u64().ok_or("draft omitted number")?;
    world
        .api(
            Method::POST,
            "/api/projects/{slug}/hypotheses/{number}/draft-review",
            &format!("{base}/hypotheses/{number}/draft-review"),
            None,
            Some(admin),
            Some(json!({"draft_revision":1,"action":"approve","reason":"Fixture CLI conformance"})),
            200,
        )
        .await?;
    Ok(number)
}
async fn submit(
    world: &mut World,
    base: &str,
    agent: &str,
    admin: &str,
    track: &str,
) -> Result<u64> {
    let mut document = fixture("examples/fixture/hypothesis.json")?;
    document["track"] = json!(track);
    let number = draft(world, base, agent, admin, document).await?;
    let claim = world
        .api(
            Method::POST,
            "/api/projects/{slug}/claims",
            &format!("{base}/claims"),
            None,
            Some(agent),
            Some(json!({"hypothesis":number})),
            201,
        )
        .await?;
    let attempt = &claim["attempt"];
    let path = format!(
        "{base}/hypotheses/{number}/attempts/{}",
        attempt["sequence"]
    );
    let bytes = fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/fixture/candidate.json"),
    )?;
    let grant=lease_api(world,Method::POST,"/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/uploads",&format!("{path}/uploads"),agent,&claim,json!({"role":"candidate","name":"candidate.json","size_bytes":bytes.len(),"sha256":sha(&bytes),"media_type":"application/json"}),201).await?;
    let upload_url=reqwest::Url::parse(string(&grant,"upload_url")?)?;
    let base_url=reqwest::Url::parse(&world.base)?;
    assert!(upload_url.origin()==base_url.origin(),"upload capability must stay on the API origin");
    let mut upload_path=upload_url.path().to_owned();
    if let Some(query)=upload_url.query(){upload_path.push('?');upload_path.push_str(query);}
    let mut request = world.harness.request(Method::PUT,&upload_path)?;
    for (name, value) in grant["headers"]
        .as_object()
        .ok_or("upload headers absent")?
    {
        request = request.header(name, value.as_str().ok_or("upload header not string")?);
    }
    let uploaded = world
        .harness
        .check_response(
            Method::PUT,
            "/api/uploads/{upload_id}",
            request.body(bytes).send().await?,
            201,
        )
        .await?
        .body;
    let mut object = json!({});
    for key in ["role", "storage", "size_bytes", "sha256", "media_type"] {
        object[key] = uploaded[key].clone();
    }
    let manifest = lease_api(
        world,
        Method::POST,
        "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/manifest",
        &format!("{path}/manifest"),
        agent,
        &claim,
        json!({"schema_version":"0.2","attempt_id":attempt["id"],"objects":[object]}),
        201,
    )
    .await?;
    let mut evidence = fixture("tests/fixtures/contracts/evidence_envelope/valid/agent.json")?;
    evidence["attempt_id"] = attempt["id"].clone();
    evidence["manifest"] = manifest;
    evidence["artifact_roles"] = json!(["candidate"]);
    evidence["measurements"] = json!([{"metric":"mrr","value":0.9,"authority":"agent_claim","unit":"ratio","direction":"higher","split":"dev","dimensions":{"language":"en"}}]);
    evidence["provenance"]["science_revision"] = json!("1");
    let submitted = lease_api(
        world,
        Method::POST,
        "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/submission",
        &format!("{path}/submission"),
        agent,
        &claim,
        evidence,
        201,
    )
    .await?;
    assert_eq!(submitted["state"], "testing");
    Ok(number)
}
async fn jobs(world: &mut World, base: &str, number: u64, token: &str) -> Result<Vec<Value>> {
    let response = world
        .api(
            Method::GET,
            "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/jobs",
            &format!("{base}/hypotheses/{number}/attempts/1/jobs"),
            None,
            Some(token),
            None,
            200,
        )
        .await?;
    Ok(response["items"]
        .as_array()
        .ok_or("jobs omitted items")?
        .clone())
}
async fn inspect_test_job(
    world: &mut World,
    base: &str,
    number: u64,
    token: &str,
    producer: &str,
) -> Result<()> {
    let listed = jobs(world, base, number, token).await?;
    let job = listed
        .iter()
        .find(|job| job["stage"] == "tester")
        .ok_or("tester job absent")?;
    let detail = world
        .api(
            Method::GET,
            "/api/projects/{slug}/jobs/{job_id}",
            &format!("{base}/jobs/{}", string(job, "id")?),
            None,
            Some(token),
            None,
            200,
        )
        .await?;
    assert_eq!(detail["state"], "completed");
    assert_eq!(detail["steps"][0]["name"], producer);
    let mut values = std::collections::BTreeMap::new();
    for measurement in detail["evidence"]["measurements"]
        .as_array()
        .ok_or("measurements absent")?
    {
        assert_eq!(measurement["authority"], "tester_verified");
        let language = measurement["dimensions"]["language"]
            .as_str()
            .unwrap_or("all");
        values.insert(
            language.to_owned(),
            measurement["value"]
                .as_f64()
                .ok_or("measurement value absent")?,
        );
    }
    assert_eq!(values.len(), 3);
    assert!(values.values().all(|v| (0.0..=1.0).contains(v)));
    if producer == "overlap-producer" {
        assert!(values.values().all(|v| (*v - 1.0).abs() < f64::EPSILON));
    }
    let outputs = detail["outputs"].as_array().ok_or("outputs absent")?;
    assert!(outputs.iter().any(|o| o["role"] == "run"
        && o["interface"] == "ranked-run/v1"
        && o["content_validated"] == true));
    let mut producer_seen = false;
    for output in outputs.iter().filter(|o| o["role"] == "step_log") {
        let response = world
            .harness
            .request(
                Method::GET,
                &format!("{base}/artifacts/{}", string(output, "id")?),
            )?
            .bearer_auth(token)
            .send()
            .await?;
        let checked = world
            .harness
            .check_response(
                Method::GET,
                "/api/projects/{slug}/artifacts/{artifact_id}",
                response,
                200,
            )
            .await?;
        let log = String::from_utf8(checked.raw_body)?;
        if log.contains("producer inputs:") {
            producer_seen = true;
            assert!(log.contains("job.json"));
            assert!(log.contains("inputs/queries/queries.json"));
            assert!(!log.contains("qrels"));
            assert!(!log.contains("cr_job_"));
            assert!(!log.contains("held-out-labels"));
        }
    }
    assert!(
        producer_seen,
        "producer's local contract guard did not execute"
    );
    Ok(())
}
fn config(
    root: &std::path::Path,
    api: &str,
    project: &str,
    fixture_root: &std::path::Path,
    kind: &str,
    token: &str,
    policy: Option<&str>,
) -> String {
    let mut config = format!(
        "api_url = {}\nproject = {}\ndata_root = {}\nwork_root = {}\ncache_root = {}\n[launcher]\ntype = \"local\"\nstep_root = {}\n[[kinds]]\nkind = {}\ntoken_file = {}\n",
        toml_string(api),
        toml_string(project),
        toml_string(&fixture_root.join("data").to_string_lossy()),
        toml_string(&root.join("work").to_string_lossy()),
        toml_string(&root.join("cache").to_string_lossy()),
        toml_string(&fixture_root.join("steps").to_string_lossy()),
        toml_string(kind),
        toml_string(token)
    );
    if let Some(policy) = policy {
        config.push_str("policy = ");
        config.push_str(&toml_string(policy));
        config.push('\n');
    }
    config
}
