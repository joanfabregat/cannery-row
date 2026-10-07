use super::lifecycle::{ATTEMPT, Actors, Call, World, object, sha, string};
use conformance::Result;
use reqwest::Method;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

pub fn fixture(path: &str) -> Result<Value> {
    Ok(serde_json::from_str(&fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path),
    )?)?)
}
pub fn producer() -> Result<Value> {
    fixture("examples/fixture/producers/overlap-producer.json")
}
pub fn science() -> Result<Value> {
    fixture("examples/fixture/science.json")
}
pub struct Work(pub PathBuf);
impl Drop for Work {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
pub struct Rig {
    pub world: World,
    pub actors: Actors,
    pub work: Work,
    pub track_revision: u64,
    pub producer_revision: u64,
    pub cli_extra: Vec<String>,
}
impl Rig {
    pub async fn new(label: &str, producer: Value, science: Value) -> Result<Self> {
        let (mut world, actors) = World::new(label).await?;
        let root = PathBuf::from(std::env::var("CANNERY_CONFORMANCE_WORK_DIR")?)
            .join(format!("semantics-{}", world.project));
        fs::create_dir(&root)?;
        let work = Work(root);
        fs::create_dir(work.0.join("steps"))?;
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/fixture/steps");
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                fs::copy(entry.path(), work.0.join("steps").join(entry.file_name()))?;
            }
        }
        let mut token = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(work.0.join("tester.token"))?;
        token.write_all(actors.tester.as_bytes())?;
        world
            .api(Call::post(
                "/api/projects/{slug}/config/{kind}",
                format!("{}/config/science", world.base()),
                &actors.admin,
                science,
                201,
            ))
            .await?;
        let mut rig = Self {
            world,
            actors,
            work,
            track_revision: 1,
            producer_revision: 1,
            cli_extra: Vec::new(),
        };
        rig.bind(producer).await?;
        Ok(rig)
    }
    pub fn script(&self, name: &str, content: &str) -> Result<()> {
        fs::write(self.work.0.join("steps").join(name), content)?;
        Ok(())
    }
    pub async fn bind(&mut self, producer: Value) -> Result<()> {
        let registered = self
            .world
            .api(Call::post(
                "/api/projects/{slug}/producers",
                format!("{}/producers", self.world.base()),
                &self.actors.admin,
                producer,
                201,
            ))
            .await?
            .body;
        let reference = json!({"name":"overlap-producer","revision":registered["revision"]});
        self.producer_revision = registered["revision"]
            .as_u64()
            .ok_or("producer revision missing")?;
        let mut call = Call::post(
            "/api/projects/{slug}/tracks/{track_slug}",
            format!("{}/tracks/lexical", self.world.base()),
            &self.actors.admin,
            json!({"expected_revision":self.track_revision,"producer":reference,"reason":"Run synthetic local runner semantics"}),
            200,
        );
        call.method = Method::PATCH;
        let track = self.world.api(call).await?.body;
        self.track_revision = track["revision"].as_u64().ok_or("track revision missing")?;
        Ok(())
    }
    pub async fn submit(&mut self) -> Result<i64> {
        let number = self
            .world
            .queue(&self.actors, "Synthetic runner semantics")
            .await?;
        let lease = self.world.claim(&self.actors, number, false).await?;
        let path = self.world.attempt_path(&lease);
        let bytes = include_bytes!("../../../../examples/fixture/candidate.json");
        let grant=self.world.api(Call::post(&format!("{ATTEMPT}/uploads"),format!("{path}/uploads"),&self.actors.agent,json!({"role":"candidate","name":"candidate.json","size_bytes":bytes.len(),"sha256":sha(bytes),"media_type":"application/json"}),201).lease(&lease)?).await?.body;
        let artifact = self.world.put(&grant, bytes, false).await?;
        let manifest=self.world.api(Call::post(&format!("{ATTEMPT}/manifest"),format!("{path}/manifest"),&self.actors.agent,json!({"schema_version":"0.2","attempt_id":lease.document["id"],"objects":[object(&artifact)]}),201).lease(&lease)?).await?.body;
        let mut sheet = fixture("tests/fixtures/contracts/evidence_envelope/valid/agent.json")?;
        sheet["attempt_id"] = lease.document["id"].clone();
        sheet["manifest"] = manifest;
        sheet["provenance"]["science_revision"] = json!("2");
        sheet["artifact_roles"] = json!(["candidate"]);
        sheet["measurements"] = json!([{"metric":"mrr","value":0.9,"authority":"agent_claim","unit":"ratio","direction":"higher","split":"dev","dimensions":{"language":"en"}}]);
        self.world
            .api(
                Call::post(
                    &format!("{ATTEMPT}/submission"),
                    format!("{path}/submission"),
                    &self.actors.agent,
                    sheet,
                    201,
                )
                .lease(&lease)?
                .key(&format!("runner-semantics-{number}"))?,
            )
            .await?;
        Ok(number)
    }
    pub fn command(&self) -> Result<Command> {
        self.command_for_api(&self.world.base_url)
    }
    pub fn command_for_api(&self, api: &str) -> Result<Command> {
        let mut command = Command::new(std::env::var("CANNERY_CONFORMANCE_CLI")?);
        let data = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/fixture/data")
            .canonicalize()?;
        command.args([
            "runner",
            "--api-url",
            api,
            "--project",
            &self.world.project,
            "--data-root",
            &data.display().to_string(),
            "--step-root",
            &self.work.0.join("steps").display().to_string(),
            "--work-root",
            &self.work.0.join("work").display().to_string(),
            "--cache-root",
            &self.work.0.join("cache").display().to_string(),
            "--unisolated-local",
            "--once",
        ]);
        command
            .args(&self.cli_extra)
            .env(
                "CANNERY_RUNNER_TOKEN_FILE",
                self.work.0.join("tester.token"),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Ok(command)
    }
    pub async fn run(&self) -> Result<Output> {
        Running::new(self.command()?.spawn()?)
            .wait(Duration::from_secs(90))
            .await
    }
    pub async fn job(&mut self, number: i64) -> Result<Value> {
        let listed = self
            .world
            .api(Call::get(
                &format!("{ATTEMPT}/jobs"),
                format!("{}/hypotheses/{number}/attempts/1/jobs", self.world.base()),
                &self.actors.admin,
            ))
            .await?
            .body;
        let item = listed["items"]
            .as_array()
            .ok_or("job items missing")?
            .iter()
            .find(|job| job["run_number"] == 1 && job["stage"] == "tester")
            .ok_or("initial job missing")?;
        Ok(item.clone())
    }
    pub async fn logs(&mut self, job: &Value) -> Result<String> {
        let mut content = String::new();
        for output in job["outputs"]
            .as_array()
            .ok_or("outputs missing")?
            .iter()
            .filter(|output| {
                output["role"] == "step_log"
                    || output["role"] == "validator_log"
                    || output["role"] == "setup_log"
            })
        {
            let response = self
                .world
                .h
                .request(
                    Method::GET,
                    &format!("{}/artifacts/{}", self.world.base(), string(&output["id"])?),
                )?
                .bearer_auth(&self.actors.admin)
                .send()
                .await?;
            let checked = self
                .world
                .h
                .check_response(
                    Method::GET,
                    "/api/projects/{slug}/artifacts/{artifact_id}",
                    response,
                    200,
                )
                .await?;
            content.push_str(&String::from_utf8(checked.raw_body)?);
        }
        for secret in [&self.actors.admin, &self.actors.agent, &self.actors.tester] {
            assert!(!content.contains(secret));
        }
        Ok(content)
    }
    pub fn assert_clean(&self) -> Result<()> {
        assert_eq!(entries(&self.work.0.join("work"))?, 0);
        Ok(())
    }
    pub async fn finish(&mut self, label: &str) -> Result<()> {
        self.world.finish_coverage(&self.actors.admin, label).await
    }
}

pub struct Running(Option<Child>);
fn terminate_owned(pid: u32) -> Result<()> {
    if pid == 0 {
        return Err("invalid owned child PID".into());
    }
    let status = Command::new("/bin/sh")
        .args(["-c", "kill -TERM \"$1\"", "owned-runner", &pid.to_string()])
        .status()?;
    if !status.success() {
        return Err("cannot signal owned CLI process".into());
    }
    Ok(())
}
impl Drop for Running {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = terminate_owned(child.id());
                let until = Instant::now() + Duration::from_secs(5);
                while child.try_wait().ok().flatten().is_none() && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(25));
                }
                if child.try_wait().ok().flatten().is_none() {
                    let _ = child.kill();
                }
            }
            let _ = child.wait();
        }
    }
}
impl Running {
    pub fn new(child: Child) -> Self {
        Self(Some(child))
    }
    pub fn terminate(&self) -> Result<()> {
        terminate_owned(self.0.as_ref().ok_or("owned child absent")?.id())
    }
    pub async fn wait(mut self, limit: Duration) -> Result<Output> {
        let until = Instant::now() + limit;
        loop {
            let child = self.0.as_mut().ok_or("owned child absent")?;
            if child.try_wait()?.is_some() {
                return Ok(self
                    .0
                    .take()
                    .ok_or("owned child absent")?
                    .wait_with_output()?);
            }
            if Instant::now() > until {
                return Err("runner CLI exceeded bounded deadline".into());
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }
}
pub fn successful_process(output: &Output) {
    assert!(
        output.status.success(),
        "CLI failed with status {}",
        output.status
    );
}
pub fn entries(path: &Path) -> Result<usize> {
    if !path.exists() {
        return Ok(0);
    }
    Ok(fs::read_dir(path)?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .len())
}

pub const REPOSITORY: &str = "fixture-owner/fixture-repo";
pub const COMMIT_A: &str = "0000000000000000000000000000000000000000";
pub const COMMIT_B: &str = "1111111111111111111111111111111111111111";
pub const COMMIT_C: &str = "2222222222222222222222222222222222222222";
#[derive(Clone)]
struct GithubState {
    archives: std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, Vec<u8>>>>,
    fetches: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    base: String,
}
pub struct GithubFixture {
    pub base: String,
    state: GithubState,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl Drop for GithubFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn github_route(
    axum::extract::State(state): axum::extract::State<GithubState>,
    request: axum::extract::Request,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    use reqwest::{StatusCode, header};
    let path = request.uri().path();
    if let Some(commit) = path.strip_prefix("/archive/") {
        assert!(request.headers().get(header::AUTHORIZATION).is_none());
        let archive = state
            .archives
            .lock()
            .ok()
            .and_then(|values| values.get(commit).cloned());
        return archive.map_or_else(
            || StatusCode::NOT_FOUND.into_response(),
            |bytes| ([(header::CONTENT_TYPE, "application/gzip")], bytes).into_response(),
        );
    }
    assert_eq!(
        request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer conformance-fake-github-token")
    );
    if path == format!("/repos/{REPOSITORY}") {
        return axum::Json(json!({"full_name":REPOSITORY,"default_branch":"main"})).into_response();
    }
    if path.starts_with(&format!("/repos/{REPOSITORY}/compare/main...")) {
        return axum::Json(json!({"status":"identical","commits":[]})).into_response();
    }
    if let Some(commit) = path.strip_prefix(&format!("/repos/{REPOSITORY}/tarball/")) {
        state
            .fetches
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        return (
            StatusCode::FOUND,
            [(header::LOCATION, format!("{}/archive/{commit}", state.base))],
        )
            .into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}
impl GithubFixture {
    pub async fn new() -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let state = GithubState {
            archives: std::sync::Arc::default(),
            fetches: std::sync::Arc::default(),
            base: base.clone(),
        };
        let router = axum::Router::new()
            .fallback(axum::routing::get(github_route))
            .with_state(state.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        Ok(Self {
            base,
            state,
            server,
        })
    }
    pub async fn publish(&self, rig: &Rig, commit: &str, requirements: &str) -> Result<()> {
        let source = rig.work.0.join(format!("archive-{commit}"));
        let steps = source.join("owner-repo/steps");
        fs::create_dir_all(&steps)?;
        for entry in fs::read_dir(rig.work.0.join("steps"))? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                fs::copy(entry.path(), steps.join(entry.file_name()))?;
            }
        }
        fs::write(steps.join("requirements.txt"), requirements)?;
        let destination = rig.work.0.join(format!("archive-{commit}.tar.gz"));
        let tar_destination = destination.clone();
        let tar_commit = commit.to_owned();
        let output = tokio::task::spawn_blocking(move || {
            Command::new("tar")
                .args([
                    "--format=pax",
                    &format!("--pax-option=comment={tar_commit}"),
                    "-czf",
                ])
                .arg(&tar_destination)
                .arg("-C")
                .arg(source)
                .arg("owner-repo")
                .output()
        })
        .await??;
        assert!(output.status.success(), "trusted archive packaging failed");
        self.state
            .archives
            .lock()
            .map_err(|_| "fixture archive lock poisoned")?
            .insert(commit.to_owned(), fs::read(destination)?);
        Ok(())
    }
    pub fn fetches(&self) -> usize {
        self.state.fetches.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub fn configure(&self, rig: &mut Rig) -> Result<()> {
        let token = rig.work.0.join("github.token");
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&token)?;
        file.write_all(b"conformance-fake-github-token")?;
        rig.cli_extra = vec![
            "--github-api-url".into(),
            self.base.clone(),
            "--github-token-file".into(),
            token.display().to_string(),
            "--github-allowed-repos".into(),
            REPOSITORY.into(),
        ];
        Ok(())
    }
}
pub fn cached_producer(commit: &str) -> Result<Value> {
    let mut result = producer()?;
    result["spec"]["code"] = json!({"repo":REPOSITORY,"commit":commit,"path":"steps"});
    result["spec"]["setup"] = json!({"run":"mkdir -p \"$CR_ROOT/cache/site\" && cat requirements.txt > \"$CR_ROOT/cache/site/fixture.txt\"","network":"none","cache":{"key_files":["requirements.txt"],"paths":["site"]},"activeDeadlineSeconds":30});
    Ok(result)
}
pub fn cache_science() -> Result<Value> {
    let mut result = science()?;
    result["code_repositories"] = json!({"candidate":[REPOSITORY],"trusted":[REPOSITORY]});
    Ok(result)
}
pub fn cached_values(rig: &Rig) -> Result<Vec<String>> {
    let mut values = Vec::new();
    let root = rig.work.0.join("cache/setup");
    if root.exists() {
        for entry in fs::read_dir(root)? {
            values.push(fs::read_to_string(entry?.path().join("site/fixture.txt"))?);
        }
    }
    values.sort();
    Ok(values)
}
