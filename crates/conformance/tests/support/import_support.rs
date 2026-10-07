use conformance::{Harness, Result};
use reqwest::{Client, Method, header::HeaderMap};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const PROJECT: &str = "/api/projects/{slug}";
pub const HYPOTHESIS: &str = "/api/projects/{slug}/hypotheses/{number}";
pub const ATTEMPT: &str = "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}";
pub const REPORT: &str = "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/report";
pub const CONFIG: &str = "/api/projects/{slug}/config/{kind}";

pub fn string(value: &Value) -> Result<String> {
    Ok(value.as_str().ok_or("missing response string")?.to_owned())
}

pub struct Identity {
    pub id: String,
    pub token: String,
    pub read_token: String,
    pub email: String,
}

pub struct World {
    pub h: Harness,
    pub admin: Identity,
    pub ana: Identity,
    pub ben: Identity,
    pub slug: String,
    pub directory: PathBuf,
    pub science: PathBuf,
    cli: PathBuf,
}

pub struct CliOutput {
    pub stdout: String,
    pub stderr: String,
}

impl CliOutput {
    /// Check the source presentation or the native value-free structured diagnostic.
    #[allow(
        clippy::expect_used,
        reason = "malformed native diagnostic JSON must fail this assertion without printing private stderr"
    )]
    pub fn assert_problem(&self, source: &str, file: &str, pointer: &str, message: &str) {
        if native_cli() {
            let problems: Vec<Value> = self
                .stderr
                .lines()
                .filter_map(|line| line.strip_prefix("cannery import: "))
                .map(|line| serde_json::from_str(line).expect("native import problem JSON"))
                .collect();
            assert!(
                problems.iter().any(|problem| {
                    problem == &json!({"file":file,"pointer":pointer,"message":message})
                }),
                "missing expected value-free native import diagnostic at {file}:{pointer}"
            );
        } else {
            assert!(
                self.stderr.contains(source),
                "missing source import diagnostic"
            );
        }
    }

    #[allow(
        clippy::expect_used,
        reason = "the assertion's source diagnostic literals have fixed file/pointer/message grammar"
    )]
    pub fn assert_located_problem(&self, source: &str) {
        let (file, rest) = source.split_once(": ").expect("diagnostic file");
        let (pointer, message) = rest.split_once(": ").expect("diagnostic pointer");
        let native_message = match message {
            "no user has this verified email" => {
                self.assert_problem(
                    source,
                    "users",
                    "",
                    "each named email must identify one verified user",
                );
                return;
            }
            "before the verdict it decides on" | "before the hypothesis was created"
                if pointer == "/decision/decided_at" =>
            {
                "before the event it decides"
            }
            "not a metric of the science revision" => "not a registered metric",
            "does not lead to the hypothesis's state" => "does not lead to the hypothesis state",
            "no policy of the bundle has this id" => "no such historical policy",
            "names no artifact of the bundle (URI and SHA-256)" => {
                "must cite an artifact of the bundle with a JSON pointer"
            }
            "the report file differs from the imported one (updates are not supported)" => {
                self.assert_problem(
                    source,
                    file,
                    &format!("{pointer}/sha256"),
                    "differs from the immutable imported entry",
                );
                return;
            }
            other => other,
        };
        self.assert_problem(source, file, pointer, native_message);
    }
}

fn native_cli() -> bool {
    std::env::var("CANNERY_CONFORMANCE_SERVER_IMPLEMENTATION").as_deref() == Ok("rust")
}

impl World {
    pub async fn new() -> Result<Self> {
        let base = std::env::var("CANNERY_CONFORMANCE_URL")?;
        let mut h = Harness::new(&base)?;
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let slug = format!("import-{nonce}");
        let cli = PathBuf::from(std::env::var("CANNERY_CONFORMANCE_CLI")?);
        if !cli.is_absolute() || !cli.is_file() {
            return Err("CANNERY_CONFORMANCE_CLI must name an absolute executable".into());
        }
        // Fail before login or fixture writes if the CLI database environment is absent.
        let _database = std::env::var("CANNERY_DATABASE_URL")?;
        let work = PathBuf::from(std::env::var("CANNERY_CONFORMANCE_WORK_DIR")?);
        if !work.is_absolute() || !work.is_dir() {
            return Err(
                "CANNERY_CONFORMANCE_WORK_DIR must name an existing absolute directory".into(),
            );
        }
        let admin = login(&mut h, &base, "conformance-admin", "admin@conformance.test").await?;
        let ana = login(
            &mut h,
            &base,
            &format!("ana-{nonce}"),
            &format!("ana-{nonce}@conformance.test"),
        )
        .await?;
        let ben = login(
            &mut h,
            &base,
            &format!("ben-{nonce}"),
            &format!("ben-{nonce}@conformance.test"),
        )
        .await?;
        let directory = work.join(&slug);
        fs::create_dir(&directory)?;
        let science = directory.join("science.json");
        fs::write(
            &science,
            include_str!("../../../../examples/fixture/science.json"),
        )?;
        Ok(Self {
            h,
            admin,
            ana,
            ben,
            slug,
            directory,
            science,
            cli,
        })
    }

    pub fn base(&self) -> String {
        format!("/api/projects/{}", self.slug)
    }

    pub fn bundle(&self, label: &str, slug: &str) -> Result<PathBuf> {
        let destination = self.directory.join(label);
        copy_tree(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/import"),
            &destination,
            &self.ana.email,
            &self.ben.email,
        )?;
        let project = destination.join("project.yaml");
        let content = fs::read_to_string(&project)?
            .replace("slug: retrieval-history", &format!("slug: {slug}"));
        fs::write(project, content)?;
        Ok(destination)
    }

    pub fn run(
        &self,
        bundle: &Path,
        slug: &str,
        science: Option<&Path>,
        flags: &[&str],
        expected: i32,
    ) -> Result<CliOutput> {
        let stdout = self.directory.join("cli.stdout");
        let stderr = self.directory.join("cli.stderr");
        let mut command = Command::new(&self.cli);
        command
            .args(["import", "--bundle"])
            .arg(bundle)
            .args(["--project", slug]);
        if let Some(path) = science {
            command.arg("--science").arg(path);
        }
        command
            .args(flags)
            .stdin(Stdio::null())
            .stdout(fs::File::create(&stdout)?)
            .stderr(fs::File::create(&stderr)?);
        let mut child = command.spawn()?;
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if started.elapsed() > Duration::from_secs(90) {
                child.kill()?;
                child.wait()?;
                return Err("CLI import exceeded 90 seconds".into());
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        // Every expected 2 here denotes an import refusal, never clap usage failure.
        let expected = if expected == 2 && native_cli() {
            1
        } else {
            expected
        };
        if status.code() != Some(expected) {
            // Unexpected database/launcher errors can contain credentials: do not print them.
            return Err(format!(
                "CLI import exit code {:?}, expected {expected}",
                status.code()
            )
            .into());
        }
        Ok(CliOutput {
            stdout: fs::read_to_string(stdout)?,
            stderr: fs::read_to_string(stderr)?,
        })
    }

    pub async fn api(
        &mut self,
        method: Method,
        template: &str,
        path: &str,
        token: &str,
        body: Option<&Value>,
        status: u16,
    ) -> Result<Value> {
        let mut request = self.h.request(method.clone(), path)?.bearer_auth(token);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await?;
        Ok(self
            .h
            .check_response(method, template, response, status)
            .await?
            .body)
    }

    pub async fn get(&mut self, template: &str, path: &str) -> Result<Value> {
        self.api(
            Method::GET,
            template,
            path,
            &self.ana.read_token.clone(),
            None,
            200,
        )
        .await
    }

    pub async fn audit(&mut self, after: u64) -> Result<(u64, Vec<Value>)> {
        let mut cursor = after;
        let mut rows = Vec::new();
        loop {
            let checked = self.h.fetch_audit(&self.admin.token, cursor).await?;
            let page = checked.body["items"]
                .as_array()
                .ok_or("audit items absent")?;
            let next = checked.body["next_after"]
                .as_u64()
                .ok_or("audit cursor absent")?;
            if page.is_empty() {
                break;
            }
            if next <= cursor {
                return Err("audit cursor failed to advance".into());
            }
            rows.extend(page.iter().cloned());
            cursor = next;
        }
        Ok((cursor, rows))
    }

    pub async fn unchanged(
        &mut self,
        project_id: &Value,
        cursor: u64,
        snapshot: &Value,
    ) -> Result<()> {
        assert_eq!(self.snapshot().await?, *snapshot);
        let (_, rows) = self.audit(cursor).await?;
        assert!(!rows.iter().any(|row| {
            row["project_id"] == *project_id
                && row["action"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("import."))
        }));
        Ok(())
    }

    pub async fn snapshot(&mut self) -> Result<Value> {
        let base = self.base();
        let mut documents = Vec::new();
        for (template, path) in [
            (PROJECT, base.clone()),
            (
                "/api/projects/{slug}/members",
                format!("{base}/members?limit=100"),
            ),
            (
                "/api/projects/{slug}/hypotheses",
                format!("{base}/hypotheses?limit=100"),
            ),
            (
                "/api/projects/{slug}/tracks",
                format!("{base}/tracks?limit=100"),
            ),
            (CONFIG, format!("{base}/config/science?limit=100")),
            (
                "/api/projects/{slug}/metrics/query",
                format!("{base}/metrics/query?metric=mrr&authority=imported&all_slices=true"),
            ),
            (
                "/api/projects/{slug}/comparisons",
                format!("{base}/comparisons?origin=all&limit=100"),
            ),
        ] {
            documents.push(self.get(template, &path).await?);
        }
        for number in 1..=7 {
            documents.push(
                self.get(HYPOTHESIS, &format!("{base}/hypotheses/{number}"))
                    .await?,
            );
        }
        documents.push(
            self.get(REPORT, &format!("{base}/hypotheses/1/attempts/1/report"))
                .await?,
        );
        Ok(Value::Array(documents))
    }

    pub fn finish(&self) -> Result<()> {
        let coverage = PathBuf::from(std::env::var("CANNERY_CONFORMANCE_COVERAGE_DIR")?);
        fs::create_dir_all(&coverage)?;
        fs::write(
            coverage.join("imports.json"),
            serde_json::to_vec_pretty(self.h.coverage())?,
        )?;
        Ok(())
    }
}

impl Drop for World {
    fn drop(&mut self) {
        // Only our uniquely created child of the runner-owned temporary directory.
        let _ = fs::remove_dir_all(&self.directory);
    }
}

pub fn edit(path: &Path, old: &str, new: &str) -> Result<()> {
    let text = fs::read_to_string(path)?;
    if !text.contains(old) {
        return Err("fixture edit source absent".into());
    }
    fs::write(path, text.replace(old, new))?;
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path, ana: &str, ben: &str) -> Result<()> {
    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let output = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &output, ana, ben)?;
        } else if entry.file_type()?.is_file() {
            let content = fs::read_to_string(entry.path())?
                .replace("ana@example.org", ana)
                .replace("ben@example.org", ben);
            fs::write(output, content)?;
        } else {
            return Err("unexpected non-regular fixture entry".into());
        }
    }
    Ok(())
}

async fn login(h: &mut Harness, base: &str, subject: &str, email: &str) -> Result<Identity> {
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let redirect = client.get(format!("{base}/auth/login")).send().await?;
    if redirect.status() != 302 {
        return Err("login did not redirect".into());
    }
    let authorization = redirect
        .headers()
        .get("location")
        .ok_or("missing login location")?
        .to_str()?;
    let binding = cookie(redirect.headers(), "cr_login")?;
    let oidc = std::env::var("CANNERY_CONFORMANCE_OIDC_URL")?;
    let control = std::env::var("CANNERY_TEST_OIDC_CONTROL_TOKEN")?;
    let approved: Value = client.post(format!("{oidc}/__conformance/approve")).bearer_auth(control).json(&json!({"authorization_url":authorization,"claims":{"sub":subject,"email":email,"email_verified":true,"name":subject}})).send().await?.error_for_status()?.json().await?;
    let callback = client
        .get(format!("{base}/auth/callback"))
        .header("cookie", binding)
        .query(&[
            ("state", string(&approved["state"])?),
            ("code", string(&approved["code"])?),
        ])
        .send()
        .await?;
    if callback.status() != 302 {
        return Err("login callback did not redirect".into());
    }
    let session = cookie(callback.headers(), "cr_session")?;
    let me = client
        .get(format!("{base}/api/me"))
        .header("cookie", &session)
        .send()
        .await?;
    let checked = h.check_response(Method::GET, "/api/me", me, 200).await?;
    let id = string(&checked.body["user"]["id"])?;
    let csrf = string(&checked.body["csrf_token"])?;
    let mut tokens = Vec::new();
    for scopes in [json!(["read", "write"]), json!(["read"])] {
        let response = client
            .post(format!("{base}/api/tokens"))
            .header("cookie", &session)
            .header("X-CSRF-Token", &csrf)
            .json(&json!({"name":"import-conformance","expires_in_days":1,"scopes":scopes}))
            .send()
            .await?;
        let checked = h
            .check_response(Method::POST, "/api/tokens", response, 201)
            .await?;
        tokens.push(string(&checked.body["token"])?);
    }
    let mut tokens = tokens.into_iter();
    Ok(Identity {
        id,
        token: tokens.next().ok_or("write token absent")?,
        read_token: tokens.next().ok_or("read token absent")?,
        email: email.to_owned(),
    })
}

fn cookie(headers: &HeaderMap, name: &str) -> Result<String> {
    for value in headers.get_all("set-cookie") {
        let cookie = value.to_str()?.split(';').next().ok_or("invalid cookie")?;
        if cookie.starts_with(&format!("{name}=")) {
            return Ok(cookie.to_owned());
        }
    }
    Err("required login cookie missing".into())
}
