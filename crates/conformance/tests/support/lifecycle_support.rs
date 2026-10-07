use conformance::{CheckedResponse, Harness, Result};
use reqwest::{
    Client, Method,
    header::{HeaderMap, HeaderValue},
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

pub const ATTEMPT: &str = "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}";
pub const JOB: &str = "/api/projects/{slug}/jobs/{job_id}";

pub struct Actors {
    pub admin: String,
    pub agent: String,
    pub other_agent: String,
    pub tester: String,
    pub evaluator: String,
}
pub struct World {
    pub h: Harness,
    pub base_url: String,
    pub project: String,
    session: String,
    csrf: String,
}
pub struct Call<'a> {
    pub method: Method,
    pub template: String,
    pub path: String,
    pub token: &'a str,
    pub body: Option<Value>,
    pub status: u16,
    pub headers: HeaderMap,
}
impl<'a> Call<'a> {
    pub fn get(template: &str, path: String, token: &'a str) -> Self {
        Self {
            method: Method::GET,
            template: template.to_owned(),
            path,
            token,
            body: None,
            status: 200,
            headers: HeaderMap::new(),
        }
    }
    pub fn post(template: &str, path: String, token: &'a str, body: Value, status: u16) -> Self {
        Self {
            method: Method::POST,
            template: template.to_owned(),
            path,
            token,
            body: Some(body),
            status,
            headers: HeaderMap::new(),
        }
    }
    pub fn lease(mut self, lease: &Lease) -> Result<Self> {
        self.headers
            .insert("X-Lease-Token", HeaderValue::from_str(&lease.token)?);
        self.headers.insert(
            "X-Lease-Generation",
            HeaderValue::from_str(&lease.generation.to_string())?,
        );
        Ok(self)
    }
    pub fn key(mut self, key: &str) -> Result<Self> {
        self.headers
            .insert("Idempotency-Key", HeaderValue::from_str(key)?);
        Ok(self)
    }
}
pub struct Lease {
    pub document: Value,
    pub token: String,
    pub generation: u64,
}
impl Lease {
    pub fn attempt(body: &Value) -> Result<Self> {
        Ok(Self {
            document: body["attempt"].clone(),
            token: string(&body["lease_token"])?,
            generation: body["lease_generation"]
                .as_u64()
                .ok_or("missing attempt generation")?,
        })
    }
    pub fn job(body: &Value) -> Result<Self> {
        let job = &body["job"];
        Ok(Self {
            document: job.clone(),
            token: string(&job["lease"]["token"])?,
            generation: job["lease"]["generation"]
                .as_u64()
                .ok_or("missing job generation")?,
        })
    }
    pub fn attempt_args(&self, project: &str) -> Value {
        json!({"project":project,"number":self.document["number"],"sequence":self.document["sequence"],"lease_token":self.token,"lease_generation":self.generation})
    }
    pub fn job_args(&self, project: &str) -> Value {
        json!({"project":project,"job_id":self.document["job_id"],"lease_token":self.token,"lease_generation":self.generation})
    }
}
pub fn string(value: &Value) -> Result<String> {
    Ok(value.as_str().ok_or("expected response string")?.to_owned())
}
pub fn sha(data: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    ring::digest::digest(&ring::digest::SHA256, data)
        .as_ref()
        .iter()
        .flat_map(|byte| {
            [
                char::from(HEX[usize::from(byte >> 4)]),
                char::from(HEX[usize::from(byte & 15)]),
            ]
        })
        .collect()
}
pub fn add(mut base: Value, name: &str, value: Value) -> Value {
    base[name] = value;
    base
}
pub fn object(artifact: &Value) -> Value {
    json!({"role":artifact["role"],"storage":artifact["storage"],"size_bytes":artifact["size_bytes"],"sha256":artifact["sha256"],"media_type":artifact["media_type"]})
}

impl World {
    pub async fn new(label: &str) -> Result<(Self, Actors)> {
        Self::with_deadline(label, None).await
    }
    #[allow(
        clippy::too_many_lines,
        reason = "Keep the HTTP-only bootstrap and fixture registration in sequence"
    )]
    pub async fn with_deadline(label: &str, deadline: Option<u64>) -> Result<(Self, Actors)> {
        let base_url = std::env::var("CANNERY_CONFORMANCE_URL")?;
        let oidc = std::env::var("CANNERY_CONFORMANCE_OIDC_URL")?;
        let control = std::env::var("CANNERY_TEST_OIDC_CONTROL_TOKEN")?;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let login = client.get(format!("{base_url}/auth/login")).send().await?;
        if login.status() != 302 {
            return Err("login did not redirect".into());
        }
        let authorization = login
            .headers()
            .get("location")
            .ok_or("login location missing")?
            .to_str()?;
        let binding = cookie(login.headers(), "cr_login")?;
        let approved:Value=client.post(format!("{oidc}/__conformance/approve")).bearer_auth(control).json(&json!({"authorization_url":authorization,"claims":{"sub":"conformance-admin","email":"admin@conformance.test","email_verified":true,"name":"Conformance admin"}})).send().await?.error_for_status()?.json().await?;
        let callback = client
            .get(format!("{base_url}/auth/callback"))
            .query(&[
                ("state", string(&approved["state"])?),
                ("code", string(&approved["code"])?),
            ])
            .header("cookie", binding)
            .send()
            .await?;
        if callback.status() != 302 {
            return Err("OIDC callback did not redirect".into());
        }
        let session = cookie(callback.headers(), "cr_session")?;
        let me: Value = client
            .get(format!("{base_url}/api/me"))
            .header("cookie", &session)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let csrf = string(&me["csrf_token"])?;
        let user_id = string(&me["user"]["id"])?;
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let mut world = Self {
            h: Harness::new(&base_url)?,
            base_url,
            project: format!("life-{label}-{nonce}"),
            session,
            csrf,
        };
        let admin = world
            .mint("/api/tokens", "/api/tokens", "lifecycle-admin")
            .await?;
        world
            .api(Call::post(
                "/api/projects",
                "/api/projects".into(),
                &admin,
                json!({"slug":world.project,"title":"Lifecycle conformance"}),
                201,
            ))
            .await?;
        let mut membership = Call::post(
            "/api/projects/{slug}/members/{user_id}",
            format!("{}/members/{user_id}", world.base()),
            &admin,
            json!({"role":"researcher"}),
            200,
        );
        membership.method = Method::PUT;
        world.api(membership).await?;
        let mut service_tokens = Vec::new();
        for (kind, name) in [
            ("agent", "agent"),
            ("agent", "other-agent"),
            ("tester", "cannery-runner"),
            ("evaluator", "stock-evaluator"),
        ] {
            world
                .api(Call::post(
                    "/api/projects/{slug}/service-accounts",
                    format!("{}/service-accounts", world.base()),
                    &admin,
                    json!({"kind":kind,"name":name}),
                    201,
                ))
                .await?;
            let token = world
                .mint(
                    "/api/projects/{slug}/service-accounts/{name}/tokens",
                    &format!("{}/service-accounts/{name}/tokens", world.base()),
                    name,
                )
                .await?;
            service_tokens.push(token);
        }
        let mut tokens = service_tokens.into_iter();
        let actors = Actors {
            admin,
            agent: tokens.next().ok_or("agent missing")?,
            other_agent: tokens.next().ok_or("other agent missing")?,
            tester: tokens.next().ok_or("tester missing")?,
            evaluator: tokens.next().ok_or("evaluator missing")?,
        };
        let mut science: Value =
            serde_json::from_str(include_str!("../../../../examples/fixture/science.json"))?;
        if let Some(seconds) = deadline {
            science["scorer"]["spec"]["activeDeadlineSeconds"] = json!(seconds);
            for validator in science["validators"]
                .as_array_mut()
                .ok_or("validators absent")?
            {
                validator["spec"]["activeDeadlineSeconds"] = json!(seconds);
            }
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
        let mut producer: Value = serde_json::from_str(include_str!(
            "../../../../examples/fixture/producers/overlap-producer.json"
        ))?;
        if let Some(seconds) = deadline {
            producer["spec"]["activeDeadlineSeconds"] = json!(seconds);
        }
        world
            .api(Call::post(
                "/api/projects/{slug}/producers",
                format!("{}/producers", world.base()),
                &actors.admin,
                producer,
                201,
            ))
            .await?;
        world.api(Call::post("/api/projects/{slug}/tracks",format!("{}/tracks",world.base()),&actors.admin,json!({"slug":"lexical","title":"Lifecycle","producer":{"name":"overlap-producer","revision":1}}),201)).await?;
        Ok((world, actors))
    }
    pub fn base(&self) -> String {
        format!("/api/projects/{}", self.project)
    }
    pub fn attempt_path(&self, lease: &Lease) -> String {
        format!(
            "{}/hypotheses/{}/attempts/{}",
            self.base(),
            lease.document["number"],
            lease.document["sequence"]
        )
    }
    pub fn job_path(&self, lease: &Lease) -> Result<String> {
        Ok(format!(
            "{}/jobs/{}",
            self.base(),
            string(&lease.document["job_id"])?
        ))
    }
    pub async fn api(&mut self, call: Call<'_>) -> Result<CheckedResponse> {
        let mut request = self
            .h
            .request(call.method.clone(), &call.path)?
            .headers(call.headers);
        if !call.token.is_empty() {
            request = request.bearer_auth(call.token);
        }
        if let Some(body) = call.body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        self.h
            .check_response(call.method, &call.template, response, call.status)
            .await
    }
    async fn mint(&mut self, template: &str, path: &str, name: &str) -> Result<String> {
        let response = self
            .h
            .request(Method::POST, path)?
            .header("cookie", &self.session)
            .header("X-CSRF-Token", &self.csrf)
            .json(&json!({"name":name,"expires_in_days":1,"scopes":["read","write"]}))
            .send()
            .await?;
        let checked = self
            .h
            .check_response(Method::POST, template, response, 201)
            .await?;
        string(&checked.body["token"])
    }
    #[allow(
        dead_code,
        reason = "Used by the workflow extension integration binary"
    )]
    pub async fn experimenter(&mut self, admin: &str) -> Result<String> {
        self.api(Call::post(
            "/api/projects/{slug}/service-accounts",
            format!("{}/service-accounts", self.base()),
            admin,
            json!({"kind":"experimenter","name":"workflow-runner"}),
            201,
        ))
        .await?;
        self.mint(
            "/api/projects/{slug}/service-accounts/{name}/tokens",
            &format!("{}/service-accounts/workflow-runner/tokens", self.base()),
            "workflow-runner",
        )
        .await
    }
    pub async fn queue(&mut self, actors: &Actors, title: &str) -> Result<i64> {
        let mut hypothesis: Value =
            serde_json::from_str(include_str!("../../../../examples/fixture/hypothesis.json"))?;
        hypothesis["title"] = json!(title);
        let draft = self
            .api(Call::post(
                "/api/projects/{slug}/hypotheses",
                format!("{}/hypotheses", self.base()),
                &actors.agent,
                hypothesis,
                201,
            ))
            .await?
            .body;
        let number = draft["number"]
            .as_i64()
            .ok_or("hypothesis number missing")?;
        self.api(Call::post(
            "/api/projects/{slug}/hypotheses/{number}/draft-review",
            format!("{}/hypotheses/{number}/draft-review", self.base()),
            &actors.admin,
            json!({"draft_revision":1,"action":"approve","reason":"Conformance hypothesis"}),
            200,
        ))
        .await?;
        Ok(number)
    }
    pub async fn claim(&mut self, actors: &Actors, number: i64, mcp: bool) -> Result<Lease> {
        let body = if mcp {
            self.h
                .call_tool(
                    &actors.agent,
                    "claim_hypothesis",
                    json!({"project":self.project,"hypothesis":number}),
                )
                .await?
        } else {
            self.api(Call::post(
                "/api/projects/{slug}/claims",
                format!("{}/claims", self.base()),
                &actors.agent,
                json!({"hypothesis":number}),
                201,
            ))
            .await?
            .body
        };
        Lease::attempt(&body)
    }
    pub async fn claim_job(
        &mut self,
        actors: &Actors,
        evaluation: bool,
        mcp: bool,
    ) -> Result<Lease> {
        let token = if evaluation {
            &actors.evaluator
        } else {
            &actors.tester
        };
        let args = if evaluation {
            json!({"stage":"evaluator","revision":"fixture-policy-1"})
        } else {
            json!({})
        };
        let body = if mcp {
            self.h
                .call_tool(
                    token,
                    "claim_job",
                    add(args, "project", json!(self.project)),
                )
                .await?
        } else {
            self.api(Call::post(
                "/api/projects/{slug}/jobs/claims",
                format!("{}/jobs/claims", self.base()),
                token,
                args,
                201,
            ))
            .await?
            .body
        };
        Lease::job(&body)
    }
    pub async fn put(&mut self, grant: &Value, bytes: &[u8], job: bool) -> Result<Value> {
        let url = reqwest::Url::parse(grant["upload_url"].as_str().ok_or("upload URL missing")?)?;
        let mut request = self
            .h
            .request(Method::PUT, url.path())?
            .body(bytes.to_vec());
        for (name, value) in grant["headers"]
            .as_object()
            .ok_or("upload headers missing")?
        {
            request = request.header(name, string(value)?);
        }
        let response = request.send().await?;
        let template = if job {
            "/api/job-uploads/{upload_id}"
        } else {
            "/api/uploads/{upload_id}"
        };
        Ok(self
            .h
            .check_response(Method::PUT, template, response, 201)
            .await?
            .body)
    }
    pub async fn finish_coverage(&mut self, admin: &str, label: &str) -> Result<()> {
        self.h.fetch_audit(admin, 0).await?;
        let directory = PathBuf::from(std::env::var("CANNERY_CONFORMANCE_COVERAGE_DIR")?);
        std::fs::create_dir_all(&directory)?;
        std::fs::write(
            directory.join(format!("lifecycle-{label}.json")),
            serde_json::to_vec_pretty(self.h.coverage())?,
        )?;
        Ok(())
    }
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
