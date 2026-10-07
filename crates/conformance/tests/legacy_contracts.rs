//! Fixed historical database states, exercised only through production APIs/CLI.
#![allow(clippy::too_many_arguments, clippy::too_many_lines, dead_code)]
include!("support/legacy_contracts.rs");

struct PendingRam {
    path: PathBuf,
    retained: bool,
}
impl Drop for PendingRam {
    fn drop(&mut self) {
        if !self.retained {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn prepare_legacy_contract_fixture() -> Result<()> {
    async {
        let mut cases=json!({});
        let mut no=Legacy::new("legacy-contract-no-evaluator",true).await?;
        let stranded=no.submit(false).await?;no.run_tester().await?;
        cases["sweep_no_evaluator"]=no.metadata(stranded).await?;
        let completion=no.submit(false).await?;cases["legacy_completion"]=no.metadata(completion).await?;
        let failure=no.submit(false).await?;cases["legacy_failure_retry"]=no.metadata(failure).await?;
        let queued=no.draft(false).await?;cases["claim_no_evaluator"]=no.metadata(queued).await?;
        let submission=no.draft(false).await?;
        let mut builtin=Legacy::new("legacy-contract-builtin-gates",true).await?;
        let stranded=builtin.submit(false).await?;builtin.run_tester().await?;
        cases["sweep_builtin"]=builtin.metadata(stranded).await?;
        let queued=builtin.draft(false).await?;cases["claim_builtin"]=builtin.metadata(queued).await?;
        let mut old=Legacy::new("legacy-contract-pre0010-control",true).await?;
        let control=old.draft(false).await?;
        let mut params=Legacy::new("legacy-contract-approved-revision",true).await?;
        let parameters=params.submit(true).await?;params.run_tester().await?;
        cases["approved_revision_changed"]=params.metadata(parameters).await?;
        // Claims have no replay key. Only these two capabilities cross the
        // prepare/exercise process boundary, through private container RAM.
        let directory=PathBuf::from("/tmp").join(unique()?.replace("conformance","legacy-contracts"));
        fs::create_dir(&directory)?;
        let mut ram=PendingRam{path:directory.clone(),retained:false};
        fs::set_permissions(&directory,fs::Permissions::from_mode(0o700))?;
        let claim=no.claim(submission).await?;
        let old_claim=old.claim(control).await?;
        private_file(&directory.join("leases.json"),&serde_json::to_vec(&json!({"legacy_submission":claim,"pre0010_control":old_claim}))?)?;
        cases["legacy_submission"]=no.metadata(submission).await?;
        cases["pre0010_control"]=old.metadata(control).await?;
        assert_eq!(cases.as_object().ok_or("cases not object")?.len(),CASES.len());
        let metadata=json!({"schema_version":1,"ram_basename":directory.file_name().ok_or("RAM directory has no name")?.to_string_lossy(),"cases":cases});
        fs::write(std::env::var("CANNERY_CONFORMANCE_LEGACY_REFERENCE")?,serde_json::to_vec_pretty(&metadata)?)?;
        ram.retained=true;
        Ok(())
    }.await.map_err(safe)
}
fn read_leases(reference: &Value) -> Result<Value> {
    let name = string(reference, "ram_basename")?;
    assert!(
        name.starts_with("legacy-contracts-")
            && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'),
        "invalid private RAM directory name"
    );
    let directory = PathBuf::from("/tmp").join(name);
    let work = Work(directory);
    let path = work.0.join("leases.json");
    assert_eq!(
        fs::symlink_metadata(&work.0)?.permissions().mode() & 0o777,
        0o700,
        "RAM directory permissions"
    );
    assert_eq!(
        fs::symlink_metadata(&path)?.permissions().mode() & 0o777,
        0o600,
        "capability permissions"
    );
    let bytes = fs::read(&path)?;
    fs::remove_file(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}
async fn exercise_claim(l: &mut Legacy, case: &Value) -> Result<()> {
    let stored = l
        .world
        .api(
            Method::GET,
            "/api/projects/{slug}/config/{kind}/{revision}",
            &format!("{}/config/science/2", l.base),
            None,
            Some(&l.admin),
            None,
            200,
        )
        .await?;
    assert!(stored["content"].get("evaluator").is_none());
    if case["project"] == "legacy-contract-builtin-gates" {
        assert!(stored["content"].get("gates").is_some());
    } else {
        assert!(stored["content"].get("gates").is_none());
    }
    let refused = l
        .world
        .api(
            Method::POST,
            "/api/projects/{slug}/claims",
            &format!("{}/claims", l.base),
            None,
            Some(&l.agent),
            Some(json!({"hypothesis":case["hypothesis"]})),
            409,
        )
        .await?;
    assert_eq!(refused["error"]["message"], CLAIM_LEGACY);
    assert_eq!(
        refused["error"]["details"],
        json!([{"path":"/gates","message":CLAIM_LEGACY}])
    );
    let detail = l
        .world
        .api(
            Method::GET,
            "/api/projects/{slug}/hypotheses/{number}",
            &format!("{}/hypotheses/{}", l.base, case["hypothesis"]),
            None,
            Some(&l.admin),
            None,
            200,
        )
        .await?;
    assert_eq!(detail["state"], "queued");
    let attempts = l
        .world
        .api(
            Method::GET,
            "/api/projects/{slug}/hypotheses/{number}/attempts",
            &format!("{}/hypotheses/{}/attempts", l.base, case["hypothesis"]),
            None,
            Some(&l.admin),
            None,
            200,
        )
        .await?;
    assert_eq!(attempts["items"], json!([]));
    Ok(())
}
async fn decision(l: &mut Legacy, case: &Value, action: &str, status: u16) -> Result<Value> {
    l.world.api(Method::POST,"/api/projects/{slug}/review-cases/{case_id}/decisions",&format!("{}/review-cases/{}/decisions",l.base,string(case,"id")?),None,Some(&l.admin),Some(json!({"review_case_id":case["id"],"action":action,"reason":"Historical fixture conformance","evidence_revision":case["subject_revision"]})),status).await
}
#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn exercise_legacy_contracts_http() -> Result<()> {
    async {
        let reference = reference()?;
        assert_eq!(reference["schema_version"], 1);
        let cases = &reference["cases"];
        assert_eq!(
            cases.as_object().ok_or("case metadata absent")?.len(),
            CASES.len()
        );
        for name in CASES {
            assert!(cases.get(name).is_some(), "fixed case metadata missing");
        }
        let leases = read_leases(&reference)?;
        let mut no = Legacy::new("legacy-contract-no-evaluator", false).await?;
        let audit_start = no.world.harness.fetch_audit(&no.admin, 0).await?.body["next_after"]
            .as_u64()
            .ok_or("audit cursor missing")?;
        // Consume the seeded agent lease before any slower CLI work.
        let submitted = no.submit_claim(&leases["legacy_submission"], 2).await?;
        assert_eq!(submitted["state"], "failed");
        let failure = no
            .assert_failure(&cases["legacy_submission"], "tester")
            .await?;
        assert_eq!(failure["failure"]["stage"], "tester");
        assert_eq!(
            no.jobs(
                cases["legacy_submission"]["hypothesis"]
                    .as_u64()
                    .ok_or("number absent")?
            )
            .await?
            .len(),
            0
        );
        let mut old = Legacy::new("legacy-contract-pre0010-control", false).await?;
        let submitted = old.submit_claim(&leases["pre0010_control"], 1).await?;
        assert_eq!(submitted["state"], "testing");
        // Both independent stranded legacy attempts are swept together.
        let sweep = no.world.harness.sweep(&no.admin).await?;
        assert_eq!(sweep["evaluations_refused"], 2);
        assert_eq!(sweep["evaluations_started"], 0);
        assert_eq!(sweep["errors"], 0);
        let again = no.world.harness.sweep(&no.admin).await?;
        assert_eq!(again["evaluations_refused"], 0);
        assert_eq!(again["errors"], 0);
        no.assert_failure(&cases["sweep_no_evaluator"], "evaluator")
            .await?;
        let mut builtin = Legacy::new("legacy-contract-builtin-gates", false).await?;
        builtin
            .assert_failure(&cases["sweep_builtin"], "evaluator")
            .await?;
        for (l, key) in [
            (&mut no, "sweep_no_evaluator"),
            (&mut builtin, "sweep_builtin"),
        ] {
            let jobs = l
                .jobs(cases[key]["hypothesis"].as_u64().ok_or("number absent")?)
                .await?;
            assert_eq!(jobs.len(), 1);
            assert_eq!(jobs[0]["stage"], "tester");
            assert_eq!(jobs[0]["state"], "completed");
        }
        exercise_claim(&mut no, &cases["claim_no_evaluator"]).await?;
        exercise_claim(&mut builtin, &cases["claim_builtin"]).await?;
        // The tester still executes its job's science1, while evaluation
        // refusal consults the attempt's historical science2.
        no.run_tester().await?;
        no.assert_failure(&cases["legacy_completion"], "evaluator")
            .await?;
        let completed = no
            .jobs(
                cases["legacy_completion"]["hypothesis"]
                    .as_u64()
                    .ok_or("number absent")?,
            )
            .await?;
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0]["state"], "completed");
        assert_eq!(completed[0]["science_revision"], 1);
        let job = no.job_claim(false).await?;
        assert_eq!(job["attempt_id"], cases["legacy_failure_retry"]["attempt"]);
        let failed = no.fail(&job, false).await?;
        assert_eq!(failed["state"], "failed");
        assert_eq!(failed["error_code"], "step_crashed");
        let case = no
            .assert_failure(&cases["legacy_failure_retry"], "tester")
            .await?;
        let before = no.detail(&cases["legacy_failure_retry"]).await?;
        let refused = decision(&mut no, &case, "retry", 409).await?;
        assert_eq!(
            refused["error"]["message"],
            format!("{NO_EVALUATOR}: this attempt cannot be retried, only closed")
        );
        assert_eq!(no.detail(&cases["legacy_failure_retry"]).await?, before);
        let pending = no.cases("failure").await?;
        assert_eq!(
            pending
                .iter()
                .find(|c| c["id"] == case["id"])
                .ok_or("pending failure disappeared")?,
            &case
        );
        decision(&mut no, &case, "close_failed", 201).await?;
        let jobs = no
            .jobs(
                cases["legacy_failure_retry"]["hypothesis"]
                    .as_u64()
                    .ok_or("number absent")?,
            )
            .await?;
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0]["state"], "failed");
        assert_eq!(jobs[0]["run_number"], 1);
        assert_eq!(no.cases("result").await?.len(), 0);
        assert_eq!(builtin.cases("result").await?.len(), 0);
        old.run_tester().await?;
        old.run_evaluator().await?;
        let jobs = old
            .jobs(
                cases["pre0010_control"]["hypothesis"]
                    .as_u64()
                    .ok_or("number absent")?,
            )
            .await?;
        assert_eq!(jobs.len(), 2);
        for job in jobs {
            assert_eq!(job["state"], "completed");
            assert_eq!(
                job["evidence"]["provenance"]["control_revision"],
                "fixture-r1"
            );
        }
        let result = old.cases("result").await?;
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["evaluation"]["assessment"]["verdict"], "pass");
        // The current approved revision's top_k5 parameters cannot affect
        // the already pinned revision1 job or its automatically created run2.
        let mut params = Legacy::new("legacy-contract-approved-revision", false).await?;
        let case = &cases["approved_revision_changed"];
        let hypothesis = params
            .world
            .api(
                Method::GET,
                "/api/projects/{slug}/hypotheses/{number}",
                &format!("{}/hypotheses/{}", params.base, case["hypothesis"]),
                None,
                Some(&params.admin),
                None,
                200,
            )
            .await?;
        assert_eq!(hypothesis["revision"], 2);
        assert_eq!(hypothesis["approved_revision"], 2);
        assert_eq!(hypothesis["document"]["project_fields"], json!({"top_k":5}));
        assert_eq!(params.detail(case).await?["hypothesis_revision"], 1);
        let first = params.job_claim(true).await?;
        assert_eq!(first["parameters"], json!({"top_k":2}));
        params.fail(&first, true).await?;
        let second = params.job_claim(true).await?;
        assert_ne!(second["job_id"], first["job_id"]);
        assert_eq!(second["parameters"], json!({"top_k":2}));
        let jobs = params
            .jobs(case["hypothesis"].as_u64().ok_or("number absent")?)
            .await?;
        assert_eq!(jobs.len(), 3);
        for job in jobs {
            if job["stage"] == "tester" {
                assert!(job["parameters"].is_null());
            } else {
                assert_eq!(job["parameters"], json!({"top_k":2}));
            }
        }
        let read = params
            .world
            .api(
                Method::GET,
                "/api/projects/{slug}/jobs/{job_id}",
                &format!("{}/jobs/{}", params.base, string(&second, "job_id")?),
                None,
                Some(&params.admin),
                None,
                200,
            )
            .await?;
        assert_eq!(read["parameters"], json!({"top_k":2}));
        let audit = no.world.harness.fetch_audit(&no.admin, audit_start).await?;
        let rows = audit.body["items"]
            .as_array()
            .ok_or("audit items missing")?;
        for name in [
            "sweep_builtin",
            "sweep_no_evaluator",
            "legacy_submission",
            "legacy_completion",
            "legacy_failure_retry",
        ] {
            let failed: Vec<_> = rows
                .iter()
                .filter(|row| {
                    row["action"] == "attempt.failed" && row["subject_id"] == cases[name]["attempt"]
                })
                .collect();
            assert_eq!(failed.len(), 1, "legacy failure must be audited once");
            assert_eq!(failed[0]["new_state"]["code"], "no_evaluator");
            if name.starts_with("sweep_") {
                assert_eq!(failed[0]["actor_kind"], "system");
            }
            assert_eq!(
                rows.iter()
                    .filter(|row| row["action"] == "job.created"
                        && row["new_state"]["stage"] == "evaluator"
                        && row["new_state"]["attempt_id"] == cases[name]["attempt"])
                    .count(),
                0,
                "legacy attempt must queue no evaluator"
            );
        }
        for (l, name) in [
            (&mut no, "no-evaluator"),
            (&mut builtin, "builtin"),
            (&mut old, "pre0010"),
            (&mut params, "parameters"),
        ] {
            l.export(name).await?;
        }
        Ok(())
    }
    .await
    .map_err(safe)
}
