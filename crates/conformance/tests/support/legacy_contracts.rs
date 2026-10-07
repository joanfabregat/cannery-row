include!("evaluator_cli.rs");

const CASES: [&str; 9] = [
    "claim_builtin",
    "claim_no_evaluator",
    "sweep_builtin",
    "sweep_no_evaluator",
    "legacy_submission",
    "legacy_completion",
    "legacy_failure_retry",
    "pre0010_control",
    "approved_revision_changed",
];
const NO_EVALUATOR: &str =
    "science revision 2 has no evaluator; built-in gates moved to the stock evaluator";
const CLAIM_LEGACY: &str = "science revision 2 uses built-in gates, which moved to the stock evaluator; register a new revision with an evaluator";
struct Legacy {
    world: World,
    base: String,
    project: String,
    admin: String,
    agent: String,
    tester: String,
    evaluator: String,
    work: Work,
}
impl Legacy {
    async fn new(project: &str, create: bool) -> Result<Self> {
        let mut world = World::new()?;
        let session = world
            .login("conformance-admin", "admin@conformance.test")
            .await?;
        let (admin, _) = world
            .token(&session, "legacy-contracts-admin", &["read", "write"])
            .await?;
        let base = format!("/api/projects/{project}");
        if create {
            world
                .api(
                    Method::POST,
                    "/api/projects",
                    "/api/projects",
                    None,
                    Some(&admin),
                    Some(json!({"slug":project,"title":"Fixed legacy contract fixture"})),
                    201,
                )
                .await?;
            world
                .api(
                    Method::PUT,
                    "/api/projects/{slug}/members/{user_id}",
                    &format!("{base}/members/{}", session.id),
                    None,
                    Some(&admin),
                    Some(json!({"role":"researcher"})),
                    200,
                )
                .await?;
        }
        let mut tokens = Vec::new();
        for (kind, name) in [
            ("agent", "legacy-agent"),
            ("tester", "cannery-runner"),
            ("evaluator", "stock-evaluator"),
        ] {
            let token = if create {
                account(&mut world, &session, &admin, &base, kind, name).await?
            } else {
                let value=world.api(Method::POST,"/api/projects/{slug}/service-accounts/{name}/tokens",&format!("{base}/service-accounts/{name}/tokens"),Some(&session),None,Some(json!({"name":"legacy-exercise","expires_in_days":1,"scopes":["read","write"]})),201).await?;
                string(&value, "token")?.to_owned()
            };
            tokens.push(token);
        }
        if create {
            world
                .api(
                    Method::POST,
                    "/api/projects/{slug}/config/{kind}",
                    &format!("{base}/config/science"),
                    None,
                    Some(&admin),
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
                    Some(&admin),
                    Some(fixture("examples/fixture/producers/overlap-producer.json")?),
                    201,
                )
                .await?;
            world.api(Method::POST,"/api/projects/{slug}/tracks",&format!("{base}/tracks"),None,Some(&admin),Some(json!({"slug":"lexical","title":"Legacy fixture","producer":{"name":"overlap-producer","revision":1}})),201).await?;
        }
        let root = PathBuf::from(std::env::var("CANNERY_CONFORMANCE_WORK_DIR")?).join(project);
        fs::create_dir(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let work = Work(root);
        let agent = tokens.remove(0);
        let tester = tokens.remove(0);
        let evaluator = tokens.remove(0);
        private_file(&work.0.join("tester.token"), tester.as_bytes())?;
        private_file(&work.0.join("evaluator.token"), evaluator.as_bytes())?;
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/fixture")
            .canonicalize()?;
        fs::write(
            work.0.join("runner.toml"),
            config(
                &work.0,
                &world.base,
                project,
                &fixtures,
                "test",
                "tester.token",
                None,
            ),
        )?;
        fs::write(
            work.0.join("policy.json"),
            serde_json::to_vec(&fixture("examples/fixture/evaluator.json")?)?,
        )?;
        Ok(Self {
            world,
            base,
            project: project.into(),
            admin,
            agent,
            tester,
            evaluator,
            work,
        })
    }
    async fn draft(&mut self, parameters: bool) -> Result<u64> {
        let mut doc = fixture("examples/fixture/hypothesis.json")?;
        if parameters {
            doc["project_fields"] = json!({"top_k":2});
        }
        draft(&mut self.world, &self.base, &self.agent, &self.admin, doc).await
    }
    async fn claim(&mut self, number: u64) -> Result<Value> {
        self.world
            .api(
                Method::POST,
                "/api/projects/{slug}/claims",
                &format!("{}/claims", self.base),
                None,
                Some(&self.agent),
                Some(json!({"hypothesis":number})),
                201,
            )
            .await
    }
    async fn submit(&mut self, parameters: bool) -> Result<u64> {
        let number = self.draft(parameters).await?;
        let claim = self.claim(number).await?;
        self.submit_claim(&claim, 1).await?;
        Ok(number)
    }
    async fn submit_claim(&mut self, claim: &Value, revision: u64) -> Result<Value> {
        agent_submission(&mut self.world, &self.base, &self.agent, claim, revision).await
    }
    async fn run_tester(&self) -> Result<()> {
        success(
            &cli(
                vec![
                    "runner".into(),
                    "--config".into(),
                    self.work.0.join("runner.toml").display().to_string(),
                    "--once".into(),
                ],
                None,
            )
            .await?,
            "completed",
        );
        Ok(())
    }
    async fn run_evaluator(&self) -> Result<()> {
        success(
            &cli(
                vec![
                    "evaluator".into(),
                    "--api-url".into(),
                    self.world.base.clone(),
                    "--project".into(),
                    self.project.clone(),
                    "--config".into(),
                    self.work.0.join("policy.json").display().to_string(),
                    "--once".into(),
                ],
                Some((
                    "CANNERY_EVALUATOR_TOKEN_FILE",
                    &self.work.0.join("evaluator.token"),
                )),
            )
            .await?,
            "completed",
        );
        Ok(())
    }
    async fn jobs(&mut self, number: u64) -> Result<Vec<Value>> {
        jobs(&mut self.world, &self.base, number, &self.admin).await
    }
    async fn detail(&mut self, case: &Value) -> Result<Value> {
        self.world
            .api(
                Method::GET,
                "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}",
                &format!("{}/hypotheses/{}/attempts/1", self.base, case["hypothesis"]),
                None,
                Some(&self.admin),
                None,
                200,
            )
            .await
    }
    async fn cases(&mut self, kind: &str) -> Result<Vec<Value>> {
        let v = self
            .world
            .api(
                Method::GET,
                "/api/projects/{slug}/review-cases",
                &format!("{}/review-cases?kind={kind}&state=pending", self.base),
                None,
                Some(&self.admin),
                None,
                200,
            )
            .await?;
        Ok(v["items"].as_array().ok_or("review cases absent")?.clone())
    }
    async fn job_claim(&mut self, evaluation: bool) -> Result<Value> {
        let token = if evaluation {
            &self.evaluator
        } else {
            &self.tester
        };
        let body = if evaluation {
            json!({"stage":"evaluator","revision":"fixture-policy-1"})
        } else {
            json!({})
        };
        let v = self
            .world
            .api(
                Method::POST,
                "/api/projects/{slug}/jobs/claims",
                &format!("{}/jobs/claims", self.base),
                None,
                Some(token),
                Some(body),
                201,
            )
            .await?;
        Ok(v["job"].clone())
    }
    async fn fail(&mut self, job: &Value, evaluation: bool) -> Result<Value> {
        let token = if evaluation {
            &self.evaluator
        } else {
            &self.tester
        };
        let lease = json!({"lease_token":job["lease"]["token"],"lease_generation":job["lease"]["generation"]});
        lease_api(&mut self.world,Method::POST,"/api/projects/{slug}/jobs/{job_id}/failure",&format!("{}/jobs/{}/failure",self.base,string(job,"job_id")?),token,&lease,json!({"schema_version":"0.2","job_id":job["job_id"],"error_code":if evaluation {"evaluator_crashed"}else{"step_crashed"},"reason":"Fixed recovery conformance crash","logs":[]}),200).await
    }
    async fn metadata(&mut self, number: u64) -> Result<Value> {
        let detail = self
            .world
            .api(
                Method::GET,
                "/api/projects/{slug}/hypotheses/{number}",
                &format!("{}/hypotheses/{number}", self.base),
                None,
                Some(&self.admin),
                None,
                200,
            )
            .await?;
        let attempts = self
            .world
            .api(
                Method::GET,
                "/api/projects/{slug}/hypotheses/{number}/attempts",
                &format!("{}/hypotheses/{number}/attempts", self.base),
                None,
                Some(&self.admin),
                None,
                200,
            )
            .await?;
        let rows = attempts["items"].as_array().ok_or("attempt items absent")?;
        assert!(rows.len() <= 1, "fixture must have at most one attempt");
        let mut job_ids = json!({"tester":null,"evaluator":null});
        if !rows.is_empty() {
            for job in self.jobs(number).await? {
                let stage = string(&job, "stage")?;
                assert!(
                    job_ids[stage].is_null(),
                    "fixture must have one job per stage"
                );
                job_ids[stage] = job["id"].clone();
            }
        }
        Ok(
            json!({"project":self.project,"hypothesis":number,"hypothesis_id":detail["id"],"attempt":rows.first().map(|a|a["id"].clone()),"tester_job":job_ids["tester"],"evaluator_job":job_ids["evaluator"]}),
        )
    }
    async fn assert_failure(&mut self, case: &Value, stage: &str) -> Result<Value> {
        let attempt = self.detail(case).await?;
        assert_eq!(attempt["state"], "failed");
        let failures = attempt["failures"].as_array().ok_or("failures absent")?;
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0]["stage"], stage);
        assert_eq!(failures[0]["code"], "no_evaluator");
        assert_eq!(failures[0]["reason"], NO_EVALUATOR);
        let cases = self.cases("failure").await?;
        let case = cases
            .into_iter()
            .find(|v| v["hypothesis"] == case["hypothesis"])
            .ok_or("failure case missing")?;
        assert_eq!(case["state"], "pending");
        assert_eq!(case["hypothesis_state"], "awaiting_human_review");
        assert_eq!(case["failure"]["stage"], stage);
        assert_eq!(case["failure"]["code"], "no_evaluator");
        assert_eq!(case["failure"]["reason"], NO_EVALUATOR);
        Ok(case)
    }
    async fn export(&mut self, name: &str) -> Result<()> {
        self.world.harness.fetch_audit(&self.admin, 0).await?;
        if let Ok(dir) = std::env::var("CANNERY_CONFORMANCE_COVERAGE_DIR") {
            fs::write(
                PathBuf::from(dir).join(format!("legacy-contracts-{name}.json")),
                serde_json::to_vec_pretty(self.world.harness.coverage())?,
            )?;
        }
        Ok(())
    }
}
fn safe(error: Box<dyn Error + Send + Sync>) -> Box<dyn Error + Send + Sync> {
    match error.downcast::<reqwest::Error>() {
        Ok(error) => Box::new(error.without_url()),
        Err(error) => error,
    }
}
fn reference() -> Result<Value> {
    Ok(serde_json::from_str(&fs::read_to_string(std::env::var(
        "CANNERY_CONFORMANCE_LEGACY_REFERENCE",
    )?)?)?)
}
async fn agent_submission(
    world: &mut World,
    base: &str,
    agent: &str,
    claim: &Value,
    science_revision: u64,
) -> Result<Value> {
    let number = claim["attempt"]["number"]
        .as_u64()
        .ok_or("claim omitted number")?;
    let attempt = &claim["attempt"];
    let path = format!(
        "{base}/hypotheses/{number}/attempts/{}",
        attempt["sequence"]
    );
    let bytes = fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/fixture/candidate.json"),
    )?;
    let grant=lease_api(world,Method::POST,"/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/uploads",&format!("{path}/uploads"),agent,claim,json!({"role":"candidate","name":"candidate.json","size_bytes":bytes.len(),"sha256":sha(&bytes),"media_type":"application/json"}),201).await?;
    let upload_url = reqwest::Url::parse(string(&grant, "upload_url")?)?;
    let base_url = reqwest::Url::parse(&world.base)?;
    assert!(
        upload_url.origin() == base_url.origin(),
        "upload capability must stay on the API origin"
    );
    let mut upload_path = upload_url.path().to_owned();
    if let Some(query) = upload_url.query() {
        upload_path.push('?');
        upload_path.push_str(query);
    }
    let mut request = world.harness.request(Method::PUT, &upload_path)?;
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
        claim,
        json!({"schema_version":"0.2","attempt_id":attempt["id"],"objects":[object]}),
        201,
    )
    .await?;
    let mut evidence = fixture("tests/fixtures/contracts/evidence_envelope/valid/agent.json")?;
    evidence["attempt_id"] = attempt["id"].clone();
    evidence["manifest"] = manifest;
    evidence["artifact_roles"] = json!(["candidate"]);
    evidence["measurements"] = json!([{"metric":"mrr","value":0.9,"authority":"agent_claim","unit":"ratio","direction":"higher","split":"dev","dimensions":{"language":"en"}}]);
    evidence["provenance"]["science_revision"] = json!(science_revision.to_string());
    let submitted = lease_api(
        world,
        Method::POST,
        "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/submission",
        &format!("{path}/submission"),
        agent,
        claim,
        evidence,
        201,
    )
    .await?;
    Ok(submitted)
}
