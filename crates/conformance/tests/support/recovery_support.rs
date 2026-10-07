//! Recovery setup uses public HTTP; only the isolated launcher provisions legacy states.
use super::lifecycle::{ATTEMPT, Actors, Call, JOB, Lease, World, object, sha, string};
use conformance::{Harness, Result};
use reqwest::{Client, Method, header::HeaderMap};
use serde_json::{Value, json};
use std::path::PathBuf;

pub const KEPT: &[u8] = b"verified recovery bytes\n";
pub const CLAIM_KEY: &str = "recovery-upload-claim";

pub fn metadata_path() -> Result<PathBuf> {
    let path = PathBuf::from(std::env::var("CANNERY_CONFORMANCE_RECOVERY_REFERENCE")?);
    if !path.is_absolute() {
        return Err("recovery reference must be absolute".into());
    }
    Ok(path)
}

pub async fn project_id(world: &mut World, actors: &Actors) -> Result<Value> {
    Ok(world
        .api(Call::get(
            "/api/projects/{slug}",
            world.base(),
            &actors.admin,
        ))
        .await?
        .body["id"]
        .clone())
}

pub async fn submit(world: &mut World, actors: &Actors) -> Result<Lease> {
    let number = world.queue(actors, "Recovery fixture").await?;
    let attempt = world.claim(actors, number, false).await?;
    let path = world.attempt_path(&attempt);
    let bytes = include_bytes!("../../../../examples/fixture/candidate.json");
    let grant=world.api(Call::post(&format!("{ATTEMPT}/uploads"),format!("{path}/uploads"),&actors.agent,json!({"role":"candidate","name":"candidate.json","size_bytes":bytes.len(),"sha256":sha(bytes),"media_type":"application/json"}),201).lease(&attempt)?).await?.body;
    let artifact = world.put(&grant, bytes, false).await?;
    let manifest=world.api(Call::post(&format!("{ATTEMPT}/manifest"),format!("{path}/manifest"),&actors.agent,json!({"schema_version":"0.2","attempt_id":attempt.document["id"],"objects":[object(&artifact)]}),201).lease(&attempt)?).await?.body;
    let mut sheet: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/contracts/evidence_envelope/valid/agent.json"
    ))?;
    sheet["attempt_id"] = attempt.document["id"].clone();
    sheet["manifest"] = manifest;
    sheet["artifact_roles"] = json!(["candidate"]);
    sheet["provenance"]["science_revision"] = json!("1");
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
            .lease(&attempt)?,
        )
        .await?;
    Ok(attempt)
}

pub async fn upload(
    world: &mut World,
    actors: &Actors,
    job: &Lease,
    role: &str,
    path: &str,
    bytes: &[u8],
    media: &str,
) -> Result<(Value, Value)> {
    let grant=world.api(Call::post(&format!("{JOB}/uploads"),format!("{}/uploads",world.job_path(job)?),&actors.tester,json!({"role":role,"path":path,"size_bytes":bytes.len(),"sha256":sha(bytes),"media_type":media}),201).lease(job)?).await?.body;
    let artifact = world.put(&grant, bytes, true).await?;
    Ok((grant, artifact))
}

pub async fn complete_tester(world: &mut World, actors: &Actors, job: &Lease) -> Result<Value> {
    let evidence = json!({"schema_version":"0.2","attempt_id":job.document["attempt_id"],"stage":"tester","status":"completed","producer":{"kind":"service","id":"cannery-runner"},"started_at":"2026-09-29T00:00:00Z","finished_at":"2026-09-29T00:01:00Z","provenance":{"source_revision":"4f2a9c1","tester_revision":"runner-fixture-1","dataset_revision":"qrels-r1","control_revision":"fixture-r1","science_revision":"1"},"observations":"Synthetic recovery evidence","measurements":[{"metric":"mrr","value":1.0,"authority":"tester_verified","unit":"ratio","direction":"higher","split":"dev","dimensions":{"language":"en"},"sample_count":2},{"metric":"mrr","value":0.5,"authority":"tester_verified","unit":"ratio","direction":"higher","split":"dev","dimensions":{"language":"fr"},"sample_count":2}],"discrepancies":[],"artifact_roles":["evidence"]});
    let (_, artifact) = upload(
        world,
        actors,
        job,
        "evidence",
        "fixture-scorer/evidence/evidence.json",
        &serde_json::to_vec(&evidence)?,
        "application/json",
    )
    .await?;
    let (_, log) = upload(
        world,
        actors,
        job,
        "step_log",
        "fixture-scorer/step_log/fixture-scorer.log",
        b"scored\n",
        "text/plain",
    )
    .await?;
    Ok(world.api(Call::post(&format!("{JOB}/completion"),format!("{}/completion",world.job_path(job)?),&actors.tester,json!({"schema_version":"0.2","job_id":job.document["job_id"],"evidence":evidence,"manifest":{"schema_version":"0.2","attempt_id":job.document["attempt_id"],"objects":[object(&artifact),object(&log)]}}),200).lease(job)?).await?.body)
}

pub struct Login {
    pub h: Harness,
    pub admin: String,
    session: String,
    csrf: String,
}
fn cookie(headers: &HeaderMap, name: &str) -> Result<String> {
    for header in headers.get_all("set-cookie") {
        let value = header.to_str()?.split(';').next().ok_or("cookie absent")?;
        if value.starts_with(&format!("{name}=")) {
            return Ok(value.to_owned());
        }
    }
    Err("login cookie absent".into())
}
impl Login {
    pub async fn new() -> Result<Self> {
        let base = std::env::var("CANNERY_CONFORMANCE_URL")?;
        let oidc = std::env::var("CANNERY_CONFORMANCE_OIDC_URL")?;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let start = client.get(format!("{base}/auth/login")).send().await?;
        assert_eq!(start.status(), 302);
        let authorization = start
            .headers()
            .get("location")
            .ok_or("login location absent")?
            .to_str()?;
        let approved:Value=client.post(format!("{oidc}/__conformance/approve")).bearer_auth(std::env::var("CANNERY_TEST_OIDC_CONTROL_TOKEN")?).json(&json!({"authorization_url":authorization,"claims":{"sub":"conformance-admin","email":"admin@conformance.test","email_verified":true,"name":"Conformance admin"}})).send().await?.error_for_status()?.json().await?;
        let callback = client
            .get(format!("{base}/auth/callback"))
            .query(&[
                ("state", string(&approved["state"])?),
                ("code", string(&approved["code"])?),
            ])
            .header("cookie", cookie(start.headers(), "cr_login")?)
            .send()
            .await
            .map_err(reqwest::Error::without_url)?;
        assert_eq!(callback.status(), 302);
        let session = cookie(callback.headers(), "cr_session")?;
        let me: Value = client
            .get(format!("{base}/api/me"))
            .header("cookie", &session)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let mut login = Self {
            h: Harness::new(&base)?,
            admin: String::new(),
            session,
            csrf: string(&me["csrf_token"])?,
        };
        login.admin = login
            .mint("/api/tokens", "/api/tokens", "recovery-admin")
            .await?;
        Ok(login)
    }
    pub async fn mint(&mut self, template: &str, path: &str, name: &str) -> Result<String> {
        let response = self
            .h
            .request(Method::POST, path)?
            .header("cookie", &self.session)
            .header("X-CSRF-Token", &self.csrf)
            .json(&json!({"name":name,"expires_in_days":1,"scopes":["read","write"]}))
            .send()
            .await?;
        let response = self
            .h
            .check_response(Method::POST, template, response, 201)
            .await?;
        string(&response.body["token"])
    }
    pub async fn api(&mut self, call: Call<'_>) -> Result<Value> {
        let mut request = self
            .h
            .request(call.method.clone(), &call.path)?
            .bearer_auth(call.token)
            .headers(call.headers);
        if let Some(body) = call.body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        Ok(self
            .h
            .check_response(call.method, &call.template, response, call.status)
            .await?
            .body)
    }
    pub async fn finish(&mut self) -> Result<()> {
        self.h.fetch_audit(&self.admin, 0).await?;
        let directory = PathBuf::from(std::env::var("CANNERY_CONFORMANCE_COVERAGE_DIR")?);
        std::fs::write(
            directory.join("recovery-http.json"),
            serde_json::to_vec_pretty(self.h.coverage())?,
        )?;
        Ok(())
    }
}
