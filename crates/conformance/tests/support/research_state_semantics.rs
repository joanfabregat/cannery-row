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
            .await
            .map_err(reqwest::Error::without_url)?;
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
                PathBuf::from(directory).join("research-state-semantics.json"),
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
fn assert_violation_path(value: &Value, path: &str) {
    assert!(
        value
            .pointer("/error/details")
            .and_then(Value::as_array)
            .is_some_and(|details| details.iter().any(|detail| detail["path"] == path)),
        "missing violation path {path}"
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

fn without_url(error: Box<dyn Error + Send + Sync>) -> Box<dyn Error + Send + Sync> {
    match error.downcast::<reqwest::Error>() {
        Ok(error) => Box::new(error.without_url()),
        Err(error) => error,
    }
}
impl World {
    async fn rest(
        &mut self,
        method: Method,
        template: &str,
        path: &str,
        token: &str,
        body: Option<Value>,
        status: u16,
    ) -> Result<Value> {
        self.api(method, template, path, None, Some(token), body, status)
            .await
    }
    async fn keyed(
        &mut self,
        template: &str,
        path: &str,
        token: &str,
        body: Value,
        key: &str,
        status: u16,
    ) -> Result<Value> {
        let response = self
            .harness
            .request(Method::POST, path)?
            .bearer_auth(token)
            .header("Idempotency-Key", key)
            .json(&body)
            .send()
            .await?;
        Ok(self
            .harness
            .check_response(Method::POST, template, response, status)
            .await?
            .body)
    }
}
async fn project(world: &mut World, slug: &str, admin: &Session, token: &str) -> Result<String> {
    world
        .rest(
            Method::POST,
            "/api/projects",
            "/api/projects",
            token,
            Some(json!({"slug":slug,"title":"Research state semantics"})),
            201,
        )
        .await?;
    let base = format!("/api/projects/{slug}");
    member(world, &base, token, &admin.id, "researcher").await?;
    let mut science =
        fixture("tests/fixtures/contracts/science_revision/valid/stock_evaluator.json")?;
    science["default_producer"] = json!({"name":"sparse-producer","revision":1});
    world
        .rest(
            Method::POST,
            "/api/projects/{slug}/config/{kind}",
            &format!("{base}/config/science"),
            token,
            Some(science),
            201,
        )
        .await?;
    world
        .rest(
            Method::POST,
            "/api/projects/{slug}/producers",
            &format!("{base}/producers"),
            token,
            Some(fixture(
                "tests/fixtures/contracts/step_manifest/valid/producer.json",
            )?),
            201,
        )
        .await?;
    Ok(base)
}
async fn member(world: &mut World, base: &str, token: &str, id: &str, role: &str) -> Result<()> {
    world
        .rest(
            Method::PUT,
            "/api/projects/{slug}/members/{user_id}",
            &format!("{base}/members/{id}"),
            token,
            Some(json!({"role":role})),
            200,
        )
        .await?;
    Ok(())
}
async fn agent(
    world: &mut World,
    base: &str,
    admin: &Session,
    token: &str,
    name: &str,
) -> Result<String> {
    world
        .rest(
            Method::POST,
            "/api/projects/{slug}/service-accounts",
            &format!("{base}/service-accounts"),
            token,
            Some(json!({"kind":"agent","name":name})),
            201,
        )
        .await?;
    let minted = world
        .api(
            Method::POST,
            "/api/projects/{slug}/service-accounts/{name}/tokens",
            &format!("{base}/service-accounts/{name}/tokens"),
            Some(admin),
            None,
            Some(json!({"name":"state","expires_in_days":1,"scopes":["read","write"]})),
            201,
        )
        .await?;
    Ok(string(&minted, "token")?.into())
}
async fn track(world: &mut World, base: &str, token: &str, slug: &str) -> Result<Value> {
    world
        .rest(
            Method::POST,
            "/api/projects/{slug}/tracks",
            &format!("{base}/tracks"),
            token,
            Some(json!({"slug":slug,"title":slug})),
            201,
        )
        .await
}
fn document(track: &str, title: &str) -> Result<Value> {
    let mut document = fixture("tests/fixtures/contracts/hypothesis/valid/minimal.json")?;
    document["track"] = json!(track);
    document["title"] = json!(title);
    document["project_fields"] = json!({"architecture":"hybrid"});
    Ok(document)
}
async fn create(world: &mut World, base: &str, token: &str, document: Value) -> Result<Value> {
    world
        .rest(
            Method::POST,
            "/api/projects/{slug}/hypotheses",
            &format!("{base}/hypotheses"),
            token,
            Some(document),
            201,
        )
        .await
}
fn number(body: &Value) -> Result<u64> {
    body["number"]
        .as_u64()
        .ok_or_else(|| "hypothesis number absent".into())
}
async fn revise(
    world: &mut World,
    base: &str,
    number: u64,
    token: &str,
    revision: u64,
    document: Value,
    status: u16,
) -> Result<Value> {
    world
        .rest(
            Method::PUT,
            "/api/projects/{slug}/hypotheses/{number}",
            &format!("{base}/hypotheses/{number}"),
            token,
            Some(json!({"expected_revision":revision,"document":document})),
            status,
        )
        .await
}
async fn review(
    world: &mut World,
    base: &str,
    number: u64,
    token: &str,
    revision: u64,
    action: &str,
    status: u16,
) -> Result<Value> {
    world
        .rest(
            Method::POST,
            "/api/projects/{slug}/hypotheses/{number}/draft-review",
            &format!("{base}/hypotheses/{number}/draft-review"),
            token,
            Some(review_body(revision, action)),
            status,
        )
        .await
}
fn review_body(revision: u64, action: &str) -> Value {
    json!({"draft_revision":revision,"action":action,"reason":"Research semantic review"})
}
async fn detail(world: &mut World, base: &str, n: u64, token: &str) -> Result<Value> {
    world
        .rest(
            Method::GET,
            "/api/projects/{slug}/hypotheses/{number}",
            &format!("{base}/hypotheses/{n}"),
            token,
            None,
            200,
        )
        .await
}
async fn move_track(
    world: &mut World,
    base: &str,
    slug: &str,
    token: &str,
    revision: u64,
    state: &str,
    reason: &str,
    status: u16,
) -> Result<Value> {
    world
        .rest(
            Method::POST,
            "/api/projects/{slug}/tracks/{track_slug}/transitions",
            &format!("{base}/tracks/{slug}/transitions"),
            token,
            Some(json!({"to_state":state,"expected_revision":revision,"reason":reason})),
            status,
        )
        .await
}
fn rows(value: &Value) -> Result<&Vec<Value>> {
    value["items"]
        .as_array()
        .ok_or_else(|| "page items absent".into())
}
fn nums(value: &Value) -> Result<Vec<u64>> {
    rows(value)?.iter().map(number).collect()
}

// Reuse the reviewed HTTP submission and local-worker fixtures for new review decisions.
#[allow(dead_code)]
mod result_review {
    include!("evaluator_cli.rs");
    pub async fn exercise() -> Result<conformance::Coverage> {
        let mut world = World::new()?;
        let project = unique()?.replace("conformance", "research-result");
        let base = format!("/api/projects/{project}");
        let admin = world
            .login("conformance-admin", "admin@conformance.test")
            .await?;
        let (token, _) = world
            .token(&admin, "result-review-state", &["read", "write"])
            .await?;
        world
            .api(
                Method::POST,
                "/api/projects",
                "/api/projects",
                None,
                Some(&token),
                Some(json!({"slug":project,"title":"Result review state semantics"})),
                201,
            )
            .await?;
        world
            .api(
                Method::PUT,
                "/api/projects/{slug}/members/{user_id}",
                &format!("{base}/members/{}", admin.id),
                None,
                Some(&token),
                Some(json!({"role":"researcher"})),
                200,
            )
            .await?;
        let agent = account(&mut world, &admin, &token, &base, "agent", "review-agent").await?;
        let tester = account(
            &mut world,
            &admin,
            &token,
            &base,
            "tester",
            "cannery-runner",
        )
        .await?;
        let evaluator = account(
            &mut world,
            &admin,
            &token,
            &base,
            "evaluator",
            "stock-evaluator",
        )
        .await?;
        world
            .api(
                Method::POST,
                "/api/projects/{slug}/config/{kind}",
                &format!("{base}/config/science"),
                None,
                Some(&token),
                Some(fixture("examples/fixture/science.json")?),
                201,
            )
            .await?;
        world
            .api(
                Method::POST,
                "/api/projects/{slug}/producers",
                &format!("{base}/producers"),
                None,
                Some(&token),
                Some(fixture("examples/fixture/producers/overlap-producer.json")?),
                201,
            )
            .await?;
        world.api(Method::POST,"/api/projects/{slug}/tracks",&format!("{base}/tracks"),None,Some(&token),Some(json!({"slug":"lexical","title":"Review lexical","producer":{"name":"overlap-producer","revision":1}})),201).await?;
        let root = PathBuf::from(std::env::var("CANNERY_CONFORMANCE_WORK_DIR")?).join(&project);
        fs::create_dir(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let work = Work(root);
        private_file(&work.0.join("tester.token"), tester.as_bytes())?;
        private_file(&work.0.join("evaluator.token"), evaluator.as_bytes())?;
        let fixture_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/fixture")
            .canonicalize()?;
        fs::write(
            work.0.join("tester.toml"),
            config(
                &work.0,
                &world.base,
                &project,
                &fixture_root,
                "test",
                "tester.token",
                None,
            ),
        )?;
        fs::write(
            work.0.join("evaluator.toml"),
            config(
                &work.0,
                &world.base,
                &project,
                &fixture_root,
                "eval",
                "evaluator.token",
                Some("policy.json"),
            ),
        )?;
        for (verdict, action, tag) in [
            ("fail", "reject", "fail"),
            ("inconclusive", "reject", "inconclusive"),
            ("pass", "promote", "pass"),
            ("pass", "reject", "pass-reject"),
            ("pass", "inconclusive", "pass-inconclusive"),
        ] {
            let mut policy = fixture("examples/fixture/evaluator.json")?;
            if verdict == "fail" {
                policy["gates"][0]["min_delta"] = json!(10.0);
            } else if verdict == "inconclusive" {
                policy["gates"][0]["statistic"] = json!("uncertainty.lower");
            }
            fs::write(work.0.join("policy.json"), serde_json::to_vec(&policy)?)?;
            let n = submit(&mut world, &base, &agent, &token, "lexical").await?;
            for file in ["tester.toml", "evaluator.toml"] {
                success(
                    &cli(
                        vec![
                            "runner".into(),
                            "--config".into(),
                            work.0.join(file).display().to_string(),
                            "--once".into(),
                        ],
                        None,
                    )
                    .await?,
                    "completed",
                );
            }
            let pending = world
                .api(
                    Method::GET,
                    "/api/projects/{slug}/review-cases",
                    &format!("{base}/review-cases?kind=result&state=pending"),
                    None,
                    Some(&token),
                    None,
                    200,
                )
                .await?;
            let case = pending["items"]
                .as_array()
                .ok_or("cases absent")?
                .iter()
                .find(|case| case["hypothesis"] == n)
                .ok_or("new result case absent")?;
            assert_eq!(case["evaluation"]["assessment"]["verdict"], verdict);
            let id = string(case, "id")?.to_owned();
            let path = format!("{base}/review-cases/{id}/decisions");
            let template = "/api/projects/{slug}/review-cases/{case_id}/decisions";
            let decision = json!({"review_case_id":id,"evidence_revision":case["subject_revision"],"action":action,"reason":"Research state decision"});
            if verdict != "pass" {
                let mut invalid = decision.clone();
                invalid["action"] = json!("promote");
                let invalid = world
                    .api(
                        Method::POST,
                        template,
                        &path,
                        None,
                        Some(&token),
                        Some(invalid),
                        422,
                    )
                    .await?;
                assert_eq!(invalid["error"]["details"][0]["path"], "/action");
                let after = world
                    .api(
                        Method::GET,
                        "/api/projects/{slug}/review-cases/{case_id}",
                        &format!("{base}/review-cases/{id}"),
                        None,
                        Some(&token),
                        None,
                        200,
                    )
                    .await?;
                assert_eq!(after["state"], "pending");
                assert_eq!(after["decisions"], json!([]));
            }
            let first = keyed(
                &mut world,
                template,
                &path,
                &token,
                decision.clone(),
                &format!("initial-{tag}"),
                201,
            )
            .await?;
            let first_id = first["decisions"][0]["id"].clone();
            if verdict == "pass" && action != "promote" {
                let expected = if action == "reject" {
                    "rejected"
                } else {
                    "inconclusive"
                };
                assert_eq!(first["hypothesis_state"], expected);
                assert_eq!(first["attempt_state"], expected);
                assert_eq!(first["evaluation"], case["evaluation"]);
                continue;
            }
            if verdict != "pass" {
                let correction = json!({"review_case_id":id,"evidence_revision":first["subject_revision"],"action":"promote","reason":"Invalid correction cannot overrule gates","supersedes":first_id});
                let rejected = world
                    .api(
                        Method::POST,
                        template,
                        &path,
                        None,
                        Some(&token),
                        Some(correction),
                        422,
                    )
                    .await?;
                assert_eq!(rejected["error"]["details"][0]["path"], "/action");
                let unchanged = world
                    .api(
                        Method::GET,
                        "/api/projects/{slug}/review-cases/{case_id}",
                        &format!("{base}/review-cases/{id}"),
                        None,
                        Some(&token),
                        None,
                        200,
                    )
                    .await?;
                assert_eq!(unchanged, first);
                continue;
            }
            let correction = json!({"review_case_id":id,"evidence_revision":first["subject_revision"],"action":"inconclusive","reason":"Reconsider selection leakage","supersedes":first_id});
            let corrected = keyed(
                &mut world,
                template,
                &path,
                &token,
                correction.clone(),
                "correction-once",
                201,
            )
            .await?;
            assert_eq!(corrected["attempt_state"], "inconclusive");
            assert_eq!(corrected["hypothesis_state"], "inconclusive");
            assert_eq!(
                corrected["evaluation"], case["evaluation"],
                "corrections preserve the evaluator record"
            );
            let replay = keyed(
                &mut world,
                template,
                &path,
                &token,
                decision.clone(),
                "initial-pass",
                200,
            )
            .await?;
            assert_eq!(
                replay, corrected,
                "key replay returns current corrected case"
            );
            assert_ne!(replay, first);
            let replay = keyed(
                &mut world,
                template,
                &path,
                &token,
                correction.clone(),
                "correction-once",
                200,
            )
            .await?;
            assert_eq!(replay, corrected);
            let repeated = world
                .api(
                    Method::POST,
                    template,
                    &path,
                    None,
                    Some(&token),
                    Some(correction),
                    200,
                )
                .await?;
            assert_eq!(repeated, corrected);
            let mut again = decision;
            again["action"] = json!("reject");
            again["supersedes"] = first_id;
            world
                .api(
                    Method::POST,
                    template,
                    &path,
                    None,
                    Some(&token),
                    Some(again),
                    409,
                )
                .await?;
            let audit = world.harness.fetch_audit(&token, 0).await?;
            for action in ["review.promote", "review.inconclusive"] {
                assert_eq!(
                    audit.body["items"]
                        .as_array()
                        .ok_or("audit items absent")?
                        .iter()
                        .filter(|event| event["subject_id"] == id && event["action"] == action)
                        .count(),
                    1,
                    "review key replays must audit once"
                );
            }
        }
        // Public voluntary release supplies a failure without seeding expired leases.
        let mut document = fixture("examples/fixture/hypothesis.json")?;
        document["track"] = json!("lexical");
        let n = draft(&mut world, &base, &agent, &token, document).await?;
        let claim = world
            .api(
                Method::POST,
                "/api/projects/{slug}/claims",
                &format!("{base}/claims"),
                None,
                Some(&agent),
                Some(json!({"hypothesis":n})),
                201,
            )
            .await?;
        let attempt_path = format!("{base}/hypotheses/{n}/attempts/1");
        lease_api(
            &mut world,
            Method::POST,
            "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/release",
            &format!("{attempt_path}/release"),
            &agent,
            &claim,
            json!({"reason":"Review semantics voluntary release"}),
            200,
        )
        .await?;
        let pending = world
            .api(
                Method::GET,
                "/api/projects/{slug}/review-cases",
                &format!("{base}/review-cases?kind=failure&state=pending"),
                None,
                Some(&token),
                None,
                200,
            )
            .await?;
        let case = pending["items"]
            .as_array()
            .ok_or("failure cases absent")?
            .iter()
            .find(|case| case["hypothesis"] == n)
            .ok_or("released attempt failure case absent")?;
        let id = string(case, "id")?.to_owned();
        world.api(Method::POST, "/api/projects/{slug}/tracks/{track_slug}/transitions", &format!("{base}/tracks/lexical/transitions"), None, Some(&token), Some(json!({"to_state":"paused","expected_revision":1,"reason":"Retry waits for paused track"})), 200).await?;
        let body = json!({"review_case_id":id,"evidence_revision":case["subject_revision"],"action":"retry","reason":"Retry the released work"});
        let template = "/api/projects/{slug}/review-cases/{case_id}/decisions";
        let path = format!("{base}/review-cases/{id}/decisions");
        let first = keyed(
            &mut world,
            template,
            &path,
            &token,
            body.clone(),
            "retry-once",
            201,
        )
        .await?;
        assert_eq!(first["hypothesis_state"], "queued");
        assert_eq!(first["attempt_state"], "failed");
        world
            .api(
                Method::POST,
                "/api/projects/{slug}/claims",
                &format!("{base}/claims"),
                None,
                Some(&agent),
                Some(json!({"hypothesis":n})),
                409,
            )
            .await?;
        world.api(Method::POST, "/api/projects/{slug}/tracks/{track_slug}/transitions", &format!("{base}/tracks/lexical/transitions"), None, Some(&token), Some(json!({"to_state":"active","expected_revision":2,"reason":"Resume queued retry"})), 200).await?;
        let next = world
            .api(
                Method::POST,
                "/api/projects/{slug}/claims",
                &format!("{base}/claims"),
                None,
                Some(&agent),
                Some(json!({"hypothesis":n})),
                201,
            )
            .await?;
        assert_eq!(next["attempt"]["sequence"], 2);
        assert_ne!(next["attempt"]["id"], claim["attempt"]["id"]);
        let replay = keyed(&mut world, template, &path, &token, body, "retry-once", 200).await?;
        assert_eq!(replay["hypothesis_state"], "active");
        assert_eq!(replay["attempt_state"], "failed");
        assert_eq!(replay["decisions"], first["decisions"]);
        assert_ne!(
            replay, first,
            "retired case replay reflects the current hypothesis state"
        );
        world.api(Method::POST, template, &path, None, Some(&token), Some(json!({"review_case_id":id,"evidence_revision":case["subject_revision"],"action":"close_failed","reason":"Cannot change an old failure decision"})), 409).await?;
        let audit = world.harness.fetch_audit(&token, 0).await?;
        assert_eq!(
            audit.body["items"]
                .as_array()
                .ok_or("audit items absent")?
                .iter()
                .filter(|event| event["subject_id"] == id && event["action"] == "review.retry")
                .count(),
            1
        );
        for retries in [0_u64, 2] {
            let mut science = fixture("examples/fixture/science.json")?;
            science["max_auto_retries"] = json!(retries);
            world
                .api(
                    Method::POST,
                    "/api/projects/{slug}/config/{kind}",
                    &format!("{base}/config/science"),
                    None,
                    Some(&token),
                    Some(science),
                    201,
                )
                .await?;
            let n = submit(&mut world, &base, &agent, &token, "lexical").await?;
            for _ in 0..=retries {
                let claimed = world
                    .api(
                        Method::POST,
                        "/api/projects/{slug}/jobs/claims",
                        &format!("{base}/jobs/claims"),
                        None,
                        Some(&tester),
                        Some(json!({})),
                        201,
                    )
                    .await?;
                let job = &claimed["job"];
                assert_eq!(
                    job["inputs"]["baselines"],
                    json!([{"id":"base-camp","revision":"fixture-r1"}]),
                    "tester receives the pinned baseline control"
                );
                let lease = json!({"lease_token":job["lease"]["token"],"lease_generation":job["lease"]["generation"]});
                lease_api(&mut world, Method::POST, "/api/projects/{slug}/jobs/{job_id}/failure", &format!("{base}/jobs/{}/failure", string(job, "job_id")?), &tester, &lease, json!({"schema_version":"0.2","job_id":job["job_id"],"error_code":"step_failed","reason":"Bounded retry fixture failure","logs":[]}), 200).await?;
            }
            let listed = jobs(&mut world, &base, n, &token).await?;
            assert_eq!(listed.len() as u64, retries + 1);
            for (index, job) in listed.iter().enumerate() {
                assert_eq!(job["run_number"], index + 1);
                assert_eq!(job["state"], "failed");
                assert_eq!(
                    job["origin"],
                    if index == 0 {
                        "submission"
                    } else {
                        "auto_retry"
                    }
                );
            }
            let attempt = world
                .api(
                    Method::GET,
                    "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}",
                    &format!("{base}/hypotheses/{n}/attempts/1"),
                    None,
                    Some(&token),
                    None,
                    200,
                )
                .await?;
            assert_eq!(attempt["state"], "failed");
            let pending = world
                .api(
                    Method::GET,
                    "/api/projects/{slug}/review-cases",
                    &format!("{base}/review-cases?kind=failure&state=pending"),
                    None,
                    Some(&token),
                    None,
                    200,
                )
                .await?;
            let case = pending["items"]
                .as_array()
                .ok_or("failure cases absent")?
                .iter()
                .find(|case| case["hypothesis"] == n)
                .ok_or("exhausted run case absent")?;
            assert_eq!(
                case["failure"]["details"]["runs"]
                    .as_array()
                    .ok_or("failure runs absent")?
                    .len() as u64,
                retries + 1
            );
            let id = string(case, "id")?;
            let closed = world.api(Method::POST, template, &format!("{base}/review-cases/{id}/decisions"), None, Some(&token), Some(json!({"review_case_id":id,"evidence_revision":case["subject_revision"],"action":"close_failed","reason":"Close exhausted fixture runs"})), 201).await?;
            assert_eq!(closed["hypothesis_state"], "failed");
            assert_eq!(closed["attempt_state"], "failed");
        }
        world.harness.fetch_audit(&token, 0).await?;
        Ok(world.harness.coverage().clone())
    }
    async fn keyed(
        world: &mut World,
        template: &str,
        path: &str,
        token: &str,
        body: Value,
        key: &str,
        status: u16,
    ) -> Result<Value> {
        let response = world
            .harness
            .request(Method::POST, path)?
            .bearer_auth(token)
            .header("Idempotency-Key", key)
            .json(&body)
            .send()
            .await?;
        Ok(world
            .harness
            .check_response(Method::POST, template, response, status)
            .await?
            .body)
    }
}
