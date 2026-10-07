// Reuse signed API/CLI fixtures without changing their implementation.
include!("evaluator_cli.rs");

struct ControlWorld {
    world: World,
    base: String,
    project: String,
    admin: String,
    agent: String,
    evaluator: String,
    work: Work,
}
impl ControlWorld {
    async fn new() -> Result<Self> {
        let mut world = World::new()?;
        let project = unique()?.replace("conformance", "controls");
        let base = format!("/api/projects/{project}");
        let session = world
            .login("conformance-admin", "admin@conformance.test")
            .await?;
        let (admin, _) = world
            .token(&session, "control-admin", &["read", "write"])
            .await?;
        world
            .api(
                Method::POST,
                "/api/projects",
                "/api/projects",
                None,
                Some(&admin),
                Some(json!({"slug":project,"title":"Evaluator controls"})),
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
        let agent = account(
            &mut world,
            &session,
            &admin,
            &base,
            "agent",
            "control-agent",
        )
        .await?;
        let tester = account(
            &mut world,
            &session,
            &admin,
            &base,
            "tester",
            "cannery-runner",
        )
        .await?;
        let evaluator = account(
            &mut world,
            &session,
            &admin,
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
        world.api(Method::POST,"/api/projects/{slug}/tracks",&format!("{base}/tracks"),None,Some(&admin),Some(json!({"slug":"lexical","title":"Control fixture","producer":{"name":"overlap-producer","revision":1}})),201).await?;
        let root = PathBuf::from(std::env::var("CANNERY_CONFORMANCE_WORK_DIR")?).join(&project);
        fs::create_dir(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let work = Work(root);
        private_file(&work.0.join("tester.token"), tester.as_bytes())?;
        private_file(&work.0.join("evaluator.token"), evaluator.as_bytes())?;
        let fixture_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/fixture")
            .canonicalize()?;
        // Observe the public runner's actual token-free /cr/job.json through
        // its normal step-log artifact. The trusted scorer is unchanged apart
        // from printing these three contract fields after scoring completes.
        let steps = work.0.join("steps");
        fs::create_dir(&steps)?;
        for entry in fs::read_dir(fixture_root.join("steps"))? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                fs::copy(entry.path(), steps.join(entry.file_name()))?;
            }
        }
        let scorer = steps.join("score.py");
        let source = fs::read_to_string(&scorer)?;
        let marker = "    print(f\"scored {len(rows)} queries\")";
        assert!(
            source.contains(marker),
            "fixture scorer observation point changed"
        );
        fs::write(&scorer, source.replace(marker, &format!("    print(\"conformance-control-contract:\" + json.dumps({{\"control_present\": \"control\" in details, \"control\": details.get(\"control\"), \"baselines\": details[\"inputs\"][\"baselines\"]}}, sort_keys=True))\n{marker}")))?;
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
            )
            .replace(
                &toml_string(&fixture_root.join("steps").to_string_lossy()),
                &toml_string(&steps.to_string_lossy()),
            ),
        )?;
        Ok(Self {
            world,
            base,
            project,
            admin,
            agent,
            evaluator,
            work,
        })
    }
    async fn science(&mut self, science: Value) -> Result<()> {
        self.world
            .api(
                Method::POST,
                "/api/projects/{slug}/config/{kind}",
                &format!("{}/config/science", self.base),
                None,
                Some(&self.admin),
                Some(science),
                201,
            )
            .await?;
        Ok(())
    }
    fn hypothesis(control: bool) -> Result<Value> {
        let mut doc = fixture("examples/fixture/hypothesis.json")?;
        doc["track"] = json!("lexical");
        if !control {
            doc.as_object_mut()
                .ok_or("hypothesis not object")?
                .remove("control");
        }
        Ok(doc)
    }
    async fn submit(&mut self, control: bool, revised: Option<Value>) -> Result<u64> {
        controlled_submit(
            &mut self.world,
            &self.base,
            &self.agent,
            &self.admin,
            Self::hypothesis(control)?,
            revised,
        )
        .await
    }
    async fn tester(&self) -> Result<()> {
        success(
            &cli(
                vec![
                    "runner".into(),
                    "--config".into(),
                    self.work.0.join("tester.toml").display().to_string(),
                    "--once".into(),
                ],
                None,
            )
            .await?,
            "completed",
        );
        Ok(())
    }
    async fn stock(&self, revision: &str, idle: bool) -> Result<()> {
        let mut policy = fixture("examples/fixture/evaluator.json")?;
        policy["evaluator"]["revision"] = json!(revision);
        let path = self.work.0.join(format!("{revision}.json"));
        fs::write(&path, serde_json::to_vec(&policy)?)?;
        let output = cli(
            vec![
                "evaluator".into(),
                "--api-url".into(),
                self.world.base.clone(),
                "--project".into(),
                self.project.clone(),
                "--config".into(),
                path.display().to_string(),
                "--once".into(),
            ],
            Some((
                "CANNERY_EVALUATOR_TOKEN_FILE",
                &self.work.0.join("evaluator.token"),
            )),
        )
        .await?;
        assert!(
            output.status.success(),
            "stock CLI failed with status {}",
            output.status
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        if idle {
            assert!(
                stdout.contains("no job waiting"),
                "evaluator did not report idle"
            );
        } else {
            assert!(stdout.contains("completed"), "evaluator did not complete");
        }
        Ok(())
    }
    async fn listed(&mut self, number: u64) -> Result<Vec<Value>> {
        jobs(&mut self.world, &self.base, number, &self.admin).await
    }
    async fn tester_contract(&mut self, job: &Value) -> Result<Value> {
        let mut observed = Vec::new();
        for artifact in job["outputs"].as_array().ok_or("job outputs missing")? {
            if artifact["role"] != "step_log" {
                continue;
            }
            let response = self
                .world
                .harness
                .request(
                    Method::GET,
                    &format!("{}/artifacts/{}", self.base, string(artifact, "id")?),
                )?
                .bearer_auth(&self.admin)
                .send()
                .await?;
            let response = self
                .world
                .harness
                .check_response(
                    Method::GET,
                    "/api/projects/{slug}/artifacts/{artifact_id}",
                    response,
                    200,
                )
                .await?;
            let log = std::str::from_utf8(&response.raw_body)?;
            for line in log.lines() {
                if let Some(json) = line.strip_prefix("conformance-control-contract:") {
                    observed.push(serde_json::from_str::<Value>(json)?);
                }
            }
        }
        assert_eq!(
            observed.len(),
            1,
            "scorer must publish one contract observation"
        );
        Ok(observed.remove(0))
    }
    async fn cases(&mut self) -> Result<Vec<Value>> {
        let response = self
            .world
            .api(
                Method::GET,
                "/api/projects/{slug}/review-cases",
                &format!("{}/review-cases?kind=result&state=pending", self.base),
                None,
                Some(&self.admin),
                None,
                200,
            )
            .await?;
        Ok(response["items"]
            .as_array()
            .ok_or("cases omitted items")?
            .clone())
    }
    async fn export(&mut self, name: &str) -> Result<()> {
        self.world.harness.fetch_audit(&self.admin, 0).await?;
        if let Ok(directory) = std::env::var("CANNERY_CONFORMANCE_COVERAGE_DIR") {
            fs::write(
                PathBuf::from(directory).join(format!("evaluator-controls-{name}.json")),
                serde_json::to_vec_pretty(self.world.harness.coverage())?,
            )?;
        }
        Ok(())
    }
    async fn claim_evaluation(&mut self) -> Result<Value> {
        let response = self
            .world
            .api(
                Method::POST,
                "/api/projects/{slug}/jobs/claims",
                &format!("{}/jobs/claims", self.base),
                None,
                Some(&self.evaluator),
                Some(json!({"stage":"evaluator","revision":"fixture-policy-1"})),
                201,
            )
            .await?;
        Ok(response["job"].clone())
    }
    async fn leased(
        &mut self,
        job: &Value,
        suffix: &str,
        method: Method,
        body: Value,
        status: u16,
    ) -> Result<Value> {
        let lease = json!({"lease_token":job["lease"]["token"],"lease_generation":job["lease"]["generation"]});
        lease_api(
            &mut self.world,
            method,
            &format!("/api/projects/{{slug}}/jobs/{{job_id}}/{suffix}"),
            &format!("{}/jobs/{}/{suffix}", self.base, string(job, "job_id")?),
            &self.evaluator,
            &lease,
            body,
            status,
        )
        .await
    }
    async fn evidence(&mut self, job: &Value) -> Result<Value> {
        let response = self
            .leased(job, "inputs/evidence", Method::GET, json!({}), 200)
            .await?;
        Ok(response[0].clone())
    }
    async fn complete(&mut self, job: &Value, record: Value, status: u16) -> Result<Value> {
        self.leased(
            job,
            "completion",
            Method::POST,
            json!({"schema_version":"0.2","job_id":job["job_id"],"evidence":record}),
            status,
        )
        .await
    }
}
fn evaluation_record(job: &Value, tested: &Value) -> Value {
    let mut provenance = json!({"science_revision":job["science_revision"]});
    for field in ["source_revision", "dataset_revision", "control_revision"] {
        if let Some(value) = tested["provenance"].get(field) {
            provenance[field] = value.clone();
        }
    }
    json!({"schema_version":"0.2","attempt_id":job["attempt_id"],"stage":"evaluator","status":"completed","producer":{"kind":"service","id":"stock-evaluator"},"started_at":"2026-09-29T01:00:00Z","finished_at":"2026-09-29T01:00:05Z","provenance":provenance,"assessment":{"policy_revision":"fixture-policy-1","gates":[{"id":"fixture-policy","result":"pass"}],"evidence":job["inputs"]["evidence"],"verdict":"pass","reason":"Public evaluator control fixture"}})
}
fn safe(error: Box<dyn Error + Send + Sync>) -> Box<dyn Error + Send + Sync> {
    match error.downcast::<reqwest::Error>() {
        Ok(error) => Box::new(error.without_url()),
        Err(error) => error,
    }
}
async fn controlled_submit(
    world: &mut World,
    base: &str,
    agent: &str,
    admin: &str,
    document: Value,
    revised: Option<Value>,
) -> Result<u64> {
    let has_control = document.get("control").is_some();
    let number = draft(world, base, agent, admin, document).await?;
    if let Some(revised) = revised {
        world
            .api(
                Method::POST,
                "/api/projects/{slug}/config/{kind}",
                &format!("{base}/config/science"),
                None,
                Some(admin),
                Some(revised),
                201,
            )
            .await?;
    }
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
    evidence["provenance"]["science_revision"] = json!(
        attempt["science_revision"]
            .as_u64()
            .ok_or("attempt science revision absent")?
            .to_string()
    );
    if !has_control {
        evidence["provenance"]
            .as_object_mut()
            .ok_or("provenance not object")?
            .remove("control_revision");
    }
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
