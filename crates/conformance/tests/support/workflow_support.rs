use super::lifecycle::{ATTEMPT, Actors, Call, Lease, World, string};
use conformance::Result;
use reqwest::{Client, Method};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub fn fixture(path: &str) -> Result<Value> {
    Ok(serde_json::from_str(&fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path),
    )?)?)
}
pub fn experiment() -> Result<Value> {
    fixture("examples/fixture/experiments/fixture-experiment.json")
}
pub fn track() -> Result<Value> {
    fixture("examples/fixture/workflow-track.json")
}
pub fn code(body: &Value, expected: &str) {
    assert_eq!(body["error"]["code"], expected);
}
pub fn violation(body: &Value, path: &str) {
    code(body, "validation_failed");
    if std::env::var("CANNERY_CONFORMANCE_SERVER_IMPLEMENTATION").as_deref() == Ok("rust")
        && body["error"]["details"].is_null()
    {
        assert_eq!(
            body["error"]["message"],
            "request does not match the REST contract"
        );
        return;
    }
    assert!(
        body["error"]["details"]
            .as_array()
            .is_some_and(|details| details.iter().any(|d| d["path"] == path)),
        "missing expected violation path {path}; actual paths: {:?}",
        body["error"]["details"].as_array().map(|details| details
            .iter()
            .map(|detail| &detail["path"])
            .collect::<Vec<_>>())
    );
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
    pub worker: String,
    pub work: Work,
}
impl Rig {
    pub async fn new(label: &str) -> Result<Self> {
        let (mut world, actors) = World::new(label).await?;
        let worker = world.experimenter(&actors.admin).await?;
        let root = PathBuf::from(std::env::var("CANNERY_CONFORMANCE_WORK_DIR")?)
            .join(format!("workflow-{}", world.project));
        fs::create_dir(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let mut token = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join("worker.token"))?;
        token.write_all(worker.as_bytes())?;
        let mut rig = Self {
            world,
            actors,
            worker,
            work: Work(root),
        };
        rig.post("/experiment-steps", "/experiment-steps", experiment()?, 201)
            .await?;
        rig.post("/tracks", "/tracks", track()?, 201).await?;
        Ok(rig)
    }
    pub async fn post(
        &mut self,
        suffix: &str,
        template: &str,
        body: Value,
        status: u16,
    ) -> Result<Value> {
        Ok(self
            .world
            .api(Call::post(
                &format!("/api/projects/{{slug}}{template}"),
                format!("{}{suffix}", self.world.base()),
                &self.actors.admin,
                body,
                status,
            ))
            .await?
            .body)
    }
    pub async fn get(&mut self, suffix: &str, template: &str) -> Result<Value> {
        Ok(self
            .world
            .api(Call::get(
                &format!("/api/projects/{{slug}}{template}"),
                format!("{}{suffix}", self.world.base()),
                &self.actors.admin,
            ))
            .await?
            .body)
    }
    pub async fn patch(
        &mut self,
        suffix: &str,
        template: &str,
        body: Value,
        status: u16,
    ) -> Result<Value> {
        let mut call = Call::post(
            &format!("/api/projects/{{slug}}{template}"),
            format!("{}{suffix}", self.world.base()),
            &self.actors.admin,
            body,
            status,
        );
        call.method = Method::PATCH;
        Ok(self.world.api(call).await?.body)
    }
    pub async fn claim_as(&mut self, token: &str, body: Value, status: u16) -> Result<Value> {
        Ok(self
            .world
            .api(Call::post(
                "/api/projects/{slug}/claims",
                format!("{}/claims", self.world.base()),
                token,
                body,
                status,
            ))
            .await?
            .body)
    }
    pub async fn queue(&mut self, track: &str) -> Result<i64> {
        let mut body = fixture("examples/fixture/workflow-hypothesis.json")?;
        body["track"] = json!(track);
        let draft = self
            .world
            .api(Call::post(
                "/api/projects/{slug}/hypotheses",
                format!("{}/hypotheses", self.world.base()),
                &self.actors.agent,
                body,
                201,
            ))
            .await?
            .body;
        let number = draft["number"].as_i64().ok_or("number missing")?;
        self.post(
            &format!("/hypotheses/{number}/draft-review"),
            "/hypotheses/{number}/draft-review",
            json!({"draft_revision":1,"action":"approve","reason":"Workflow semantics"}),
            200,
        )
        .await?;
        Ok(number)
    }
    pub async fn hypothesis(&mut self, number: i64) -> Result<Value> {
        self.get(&format!("/hypotheses/{number}"), "/hypotheses/{number}")
            .await
    }
    pub async fn attempt(&mut self, number: i64, sequence: u64) -> Result<Value> {
        self.get(
            &format!("/hypotheses/{number}/attempts/{sequence}"),
            "/hypotheses/{number}/attempts/{sequence}",
        )
        .await
    }
    pub async fn bind(&mut self, command: Value, seconds: u64) -> Result<()> {
        let mut manifest = experiment()?;
        manifest["spec"]["container"]["command"] = command;
        manifest["spec"]["activeDeadlineSeconds"] = json!(seconds);
        let registered = self
            .post("/experiment-steps", "/experiment-steps", manifest, 201)
            .await?;
        let current = self.get("/tracks/scripted", "/tracks/{track_slug}").await?;
        self.patch("/tracks/scripted", "/tracks/{track_slug}", json!({"expected_revision":current["revision"],"workflow":{"steps":[{"name":"fixture-experiment","revision":registered["revision"]}]},"reason":"Workflow semantics"}), 200).await?;
        Ok(())
    }
    pub async fn release(
        &mut self,
        token: &str,
        lease: &Lease,
        body: Value,
        status: u16,
    ) -> Result<Value> {
        Ok(self
            .world
            .api(
                Call::post(
                    &format!("{ATTEMPT}/release"),
                    format!("{}/release", self.world.attempt_path(lease)),
                    token,
                    body,
                    status,
                )
                .lease(lease)?,
            )
            .await?
            .body)
    }
    pub async fn run(&self, expected: &str) -> Result<()> {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/fixture")
            .canonicalize()?;
        let quote = |path: &Path| json!(path.to_string_lossy()).to_string();
        let data = if self.work.0.join("hyphenated-data").exists() {
            self.work.0.join("hyphenated-data")
        } else {
            source.join("data")
        };
        let config = format!(
            "api_url = {}\nproject = {}\ndata_root = {}\nwork_root = {}\ncache_root = {}\n[launcher]\ntype = \"local\"\nstep_root = {}\n[[kinds]]\nkind = \"experiment\"\ntoken_file = \"worker.token\"\n",
            json!(self.world.base_url),
            json!(self.world.project),
            quote(&data),
            quote(&self.work.0.join("work")),
            quote(&self.work.0.join("cache")),
            quote(&source.join("steps"))
        );
        let path = self.work.0.join("runner.toml");
        fs::write(&path, config)?;
        let binary = std::env::var("CANNERY_CONFORMANCE_CLI")?;
        if !Path::new(&binary).is_absolute() {
            return Err("CLI path must be absolute".into());
        }
        let output = tokio::task::spawn_blocking(move || -> Result<_> {
            let mut child = Command::new(binary)
                .args([
                    "runner",
                    "--config",
                    path.to_str().ok_or("config path")?,
                    "--once",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            let until = Instant::now() + Duration::from_secs(45);
            loop {
                if child.try_wait()?.is_some() {
                    return Ok(child.wait_with_output()?);
                }
                if Instant::now() > until {
                    child.kill()?;
                    let _ = child.wait();
                    return Err("Workflow CLI exceeded 45 seconds".into());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        })
        .await??;
        assert!(
            output.status.success(),
            "Workflow CLI exit status {}",
            output.status
        );
        if std::env::var("CANNERY_CONFORMANCE_SERVER_IMPLEMENTATION").as_deref() == Ok("rust") {
            return Ok(());
        }
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(expected),
            "CLI omitted expected outcome {expected}"
        );
        Ok(())
    }
    pub async fn audit(&mut self) -> Result<Vec<Value>> {
        let project = self.get("", "").await?;
        let response = self.world.h.fetch_audit(&self.actors.admin, 0).await?;
        Ok(response.body["items"]
            .as_array()
            .ok_or("audit items")?
            .iter()
            .filter(|row| row["project_id"] == project["id"])
            .cloned()
            .collect())
    }
    pub async fn finish(&mut self, label: &str) -> Result<()> {
        self.world
            .finish_coverage(&self.actors.admin, &format!("workflow-{label}"))
            .await
    }
}
// Actual OIDC login and browser-CSRF PAT issuance: no fixture DB user insertion.
pub async fn member(rig: &mut Rig, label: &str, role: &str) -> Result<String> {
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let base = &rig.world.base_url;
    let login = client.get(format!("{base}/auth/login")).send().await?;
    assert_eq!(login.status().as_u16(), 302);
    let cookie = |r: &reqwest::Response, name: &str| -> Result<String> {
        r.headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|h| h.to_str().ok())
            .filter_map(|v| v.split(';').next())
            .find(|v| v.starts_with(&format!("{name}=")))
            .map(str::to_owned)
            .ok_or_else(|| "cookie absent".into())
    };
    let binding = cookie(&login, "cr_login")?;
    let location = login
        .headers()
        .get("location")
        .ok_or("location")?
        .to_str()?;
    let subject = format!("{}-{label}", rig.world.project);
    let approved:Value=client.post(format!("{}/__conformance/approve",std::env::var("CANNERY_CONFORMANCE_OIDC_URL")?)).bearer_auth(std::env::var("CANNERY_TEST_OIDC_CONTROL_TOKEN")?).json(&json!({"authorization_url":location,"claims":{"sub":subject,"email":format!("{subject}@conformance.test"),"email_verified":true,"name":label}})).send().await?.error_for_status()?.json().await?;
    let callback = client
        .get(format!("{base}/auth/callback"))
        .header("cookie", binding)
        .query(&[
            ("state", string(&approved["state"])?),
            ("code", string(&approved["code"])?),
        ])
        .send()
        .await
        .map_err(reqwest::Error::without_url)?;
    assert_eq!(callback.status().as_u16(), 302);
    let session = cookie(&callback, "cr_session")?;
    let me: Value = client
        .get(format!("{base}/api/me"))
        .header("cookie", &session)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let id = string(&me["user"]["id"])?;
    let mut call = Call::post(
        "/api/projects/{slug}/members/{user_id}",
        format!("{}/members/{id}", rig.world.base()),
        &rig.actors.admin,
        json!({"role":role}),
        200,
    );
    call.method = Method::PUT;
    rig.world.api(call).await?;
    let response = rig
        .world
        .h
        .request(Method::POST, "/api/tokens")?
        .header("cookie", session)
        .header("X-CSRF-Token", string(&me["csrf_token"])?)
        .json(&json!({"name":"workflow-member","scopes":["read","write"],"expires_in_days":1}))
        .send()
        .await?;
    let response = rig
        .world
        .h
        .check_response(Method::POST, "/api/tokens", response, 201)
        .await?;
    string(&response.body["token"])
}
pub async fn predecessor(rig: &mut Rig, lease: &Lease, id: &str, status: u16) -> Result<Value> {
    let mut call = Call::get(
        &format!("{ATTEMPT}/inputs/predecessor/{{artifact_id}}"),
        format!("{}/inputs/predecessor/{id}", rig.world.attempt_path(lease)),
        &rig.worker,
    )
    .lease(lease)?;
    call.status = status;
    let response = rig.world.api(call).await?;
    if status == 200 {
        Ok(serde_json::from_slice(&response.raw_body)?)
    } else {
        Ok(response.body)
    }
}
