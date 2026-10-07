//! Public control pinning, absent controls, and policy-scoped evaluator jobs.
#![allow(clippy::too_many_arguments, clippy::too_many_lines, dead_code)]
include!("support/evaluator_control_semantics.rs");

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn obsolete_opaque_control_survives_science_replacement() -> Result<()> {
    async {
        let mut c = ControlWorld::new().await?;
        let mut science = fixture("examples/fixture/science.json")?;
        science["baselines"][0]["revision"] = json!("fixture-r2");
        let n = c.submit(true, Some(science)).await?;
        c.tester().await?;
        let listed = c.listed(n).await?;
        let tested = listed
            .iter()
            .find(|j| j["stage"] == "tester")
            .ok_or("tester missing")?;
        assert_eq!(tested["science_revision"], 2);
        assert_eq!(
            tested["evidence"]["provenance"]["control_revision"],
            "fixture-r1"
        );
        assert_eq!(c.tester_contract(tested).await?, json!({"control_present":true,"control":{"id":"base-camp","revision":"fixture-r1"},"baselines":[]}));
        c.stock("fixture-policy-1", false).await?;
        let listed = c.listed(n).await?;
        assert_eq!(listed.len(), 2);
        for job in &listed {
            assert_eq!(job["science_revision"], 2);
            assert_eq!(job["state"], "completed");
            assert_eq!(job["run_number"], 1);
        }
        let cases = c.cases().await?;
        assert_eq!(cases.len(), 1);
        assert_eq!(
            cases[0]["evaluation"]["provenance"]["control_revision"],
            "fixture-r1"
        );
        assert_eq!(cases[0]["evaluation"]["assessment"]["verdict"], "pass");
        c.export("opaque").await
    }
    .await
    .map_err(safe)
}
fn camp_science(revision: &str, input: bool) -> Result<Value> {
    let mut science = fixture("examples/fixture/science.json")?;
    science["baselines"]
        .as_array_mut()
        .ok_or("baselines not array")?
        .push(json!({"id":"camp","revision":revision}));
    if input {
        science["scorer"]["spec"]["inputs"]["artifacts"]
            .as_array_mut()
            .ok_or("artifacts not array")?
            .push(json!({"name":"camp","from":"baseline","path":"/cr/inputs/camp"}));
    }
    Ok(science)
}

// Native diagnostics quote identifiers as JSON strings; the refusal is identical.
fn incompatible_control_reason(implementation: &str) -> Result<&'static str> {
    match implementation {
        "python" => Ok(
            "a step takes baseline 'camp' as input, but the control's revision 'camp-r1' is not registered",
        ),
        "rust" => Ok(
            "a step takes baseline \"camp\" as input, but the control's revision \"camp-r1\" is not registered",
        ),
        _ => Err("unknown evaluator implementation".into()),
    }
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn incompatible_control_needed_by_step_refuses_claim_without_attempt() -> Result<()> {
    async {
        let mut c = ControlWorld::new().await?;
        c.science(camp_science("camp-r1", false)?).await?;
        let mut document = ControlWorld::hypothesis(true)?;
        document["control"] = json!({"kind":"baseline","id":"camp","revision":"camp-r1"});
        let n = draft(&mut c.world, &c.base, &c.agent, &c.admin, document).await?;
        c.science(camp_science("camp-r2", true)?).await?;
        let response = c
            .world
            .api(
                Method::POST,
                "/api/projects/{slug}/claims",
                &format!("{}/claims", c.base),
                None,
                Some(&c.agent),
                Some(json!({"hypothesis":n})),
                409,
            )
            .await?;
        let reason = incompatible_control_reason(&std::env::var(
            "CANNERY_CONFORMANCE_SERVER_IMPLEMENTATION",
        )?)?;
        assert_eq!(
            response["error"]["message"],
            format!("the hypothesis cannot be tested under science revision 3: {reason}")
        );
        assert_eq!(
            response["error"]["details"],
            json!([{"path":"/control","message":reason}])
        );
        let detail = c
            .world
            .api(
                Method::GET,
                "/api/projects/{slug}/hypotheses/{number}",
                &format!("{}/hypotheses/{n}", c.base),
                None,
                Some(&c.admin),
                None,
                200,
            )
            .await?;
        assert_eq!(detail["state"], "queued");
        let attempts = c
            .world
            .api(
                Method::GET,
                "/api/projects/{slug}/hypotheses/{number}/attempts",
                &format!("{}/hypotheses/{n}/attempts", c.base),
                None,
                Some(&c.admin),
                None,
                200,
            )
            .await?;
        assert_eq!(attempts["items"], json!([]));
        c.export("incompatible").await
    }
    .await
    .map_err(safe)
}
#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn absent_control_uses_policy_default_without_invented_provenance() -> Result<()> {
    async {
        let mut c = ControlWorld::new().await?;
        let n = c.submit(false, None).await?;
        c.tester().await?;
        let listed = c.listed(n).await?;
        let tester_job = listed
            .iter()
            .find(|j| j["stage"] == "tester")
            .ok_or("tester missing")?;
        assert_eq!(
            c.tester_contract(tester_job).await?,
            json!({"control_present":false,"control":null,"baselines":[]})
        );
        let tested = &tester_job["evidence"];
        assert!(tested["provenance"].get("control_revision").is_none());
        for m in tested["measurements"]
            .as_array()
            .ok_or("measurements missing")?
        {
            assert!(m.get("control_value").is_none());
        }
        c.stock("fixture-policy-1", false).await?;
        let cases = c.cases().await?;
        assert_eq!(cases.len(), 1);
        let record = &cases[0]["evaluation"];
        assert!(record["provenance"].get("control_revision").is_none());
        assert_eq!(record["assessment"]["verdict"], "pass");
        assert!(
            record["assessment"]["gates"][0]["detail"]
                .as_str()
                .ok_or("gate detail missing")?
                .ends_with("control_source: resolved")
        );
        c.export("absent").await
    }
    .await
    .map_err(safe)
}
#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn simultaneous_policy_revisions_only_claim_their_pinned_jobs() -> Result<()> {
    async {
        let mut c = ControlWorld::new().await?;
        let first = c.submit(true, None).await?;
        let mut science = fixture("examples/fixture/science.json")?;
        science["evaluator"]["revision"] = json!("fixture-policy-2");
        c.science(science).await?;
        let second = c.submit(true, None).await?;
        c.tester().await?;
        c.tester().await?;
        c.stock("fixture-policy-2", false).await?;
        c.stock("fixture-policy-2", true).await?;
        let first_jobs = c.listed(first).await?;
        let pending = first_jobs
            .iter()
            .find(|j| j["stage"] == "evaluator")
            .ok_or("old evaluator job missing")?;
        assert_eq!(pending["state"], "pending");
        c.stock("fixture-policy-1", false).await?;
        c.stock("fixture-policy-1", true).await?;
        let cases = c.cases().await?;
        assert_eq!(cases.len(), 2);
        for (number, policy, revision) in [
            (first, "fixture-policy-1", 1),
            (second, "fixture-policy-2", 2),
        ] {
            let case = cases
                .iter()
                .find(|v| v["hypothesis"] == number)
                .ok_or("policy result missing")?;
            assert_eq!(case["evaluation"]["assessment"]["policy_revision"], policy);
            let jobs = c.listed(number).await?;
            assert_eq!(jobs.len(), 2);
            for job in jobs {
                assert_eq!(job["state"], "completed");
                assert_eq!(job["run_number"], 1);
                assert_eq!(job["science_revision"], revision);
                assert_eq!(job["origin"], "submission");
            }
        }
        c.export("policy-pinning").await
    }
    .await
    .map_err(safe)
}
#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn external_evaluator_checks_tested_revisions_and_absent_control_retry() -> Result<()> {
    async {
        // A present control requires the exact tested dataset and control revisions.
        let mut c = ControlWorld::new().await?;
        let mut document = ControlWorld::hypothesis(true)?;
        document["project_fields"] = json!({"top_k":2});
        let present =
            controlled_submit(&mut c.world, &c.base, &c.agent, &c.admin, document, None).await?;
        c.tester().await?;
        let session = c
            .world
            .login("conformance-admin", "admin@conformance.test")
            .await?;
        let misnamed = account(
            &mut c.world,
            &session,
            &c.admin,
            &c.base,
            "evaluator",
            "other-evaluator",
        )
        .await?;
        let refused = c
            .world
            .api(
                Method::POST,
                "/api/projects/{slug}/jobs/claims",
                &format!("{}/jobs/claims", c.base),
                None,
                Some(&misnamed),
                Some(json!({"stage":"evaluator","revision":"fixture-policy-1"})),
                409,
            )
            .await?;
        assert!(
            refused["error"]["message"]
                .as_str()
                .ok_or("claim error message missing")?
                .contains("other-evaluator")
        );
        for (index, path) in ["dataset_revision", "control_revision"]
            .into_iter()
            .enumerate()
        {
            let job = c.claim_evaluation().await?;
            assert_eq!(
                job["control"],
                json!({"id":"base-camp","revision":"fixture-r1"})
            );
            assert_eq!(
                job["inputs"]["baselines"],
                json!([{"id":"base-camp","revision":"fixture-r1"}])
            );
            assert_eq!(job["parameters"], json!({"top_k":2}));
            let tested = c.evidence(&job).await?;
            let mut record = evaluation_record(&job, &tested);
            if index == 0 {
                record["provenance"][path] = json!("qrels-r0");
            } else {
                record["provenance"]
                    .as_object_mut()
                    .ok_or("provenance not object")?
                    .remove(path);
            }
            let response = c.complete(&job, record, 422).await?;
            assert_eq!(
                response["error"]["details"][0]["path"],
                format!("/evidence/provenance/{path}")
            );
        }
        assert_eq!(
            c.cases().await?.len(),
            0,
            "invalid records must publish no result case"
        );
        let jobs = c.listed(present).await?;
        let evaluators: Vec<_> = jobs.iter().filter(|j| j["stage"] == "evaluator").collect();
        assert_eq!(evaluators.len(), 2);
        for j in evaluators {
            assert_eq!(j["state"], "failed");
            assert_eq!(j["error_code"], "invalid_output");
            assert!(j["evidence"].is_null());
        }
        // No control must stay absent. Inventing one fails the first run;
        // the automatic rerun accepts the same tested provenance without it.
        let absent = c.submit(false, None).await?;
        c.tester().await?;
        let job = c.claim_evaluation().await?;
        assert!(job.get("control").is_none());
        assert_eq!(job["inputs"]["baselines"], json!([]));
        assert_eq!(job["parameters"], json!({}));
        let tested = c.evidence(&job).await?;
        assert!(tested["provenance"].get("control_revision").is_none());
        let mut record = evaluation_record(&job, &tested);
        record["provenance"]["control_revision"] = json!("fixture-r1");
        let response = c.complete(&job, record, 422).await?;
        assert_eq!(
            response["error"]["details"][0]["path"],
            "/evidence/provenance/control_revision"
        );
        let retry = c.claim_evaluation().await?;
        assert!(retry.get("control").is_none());
        assert_eq!(retry["inputs"]["baselines"], json!([]));
        let accepted = c
            .complete(&retry, evaluation_record(&retry, &tested), 200)
            .await?;
        assert_eq!(accepted["state"], "completed");
        let replay = c
            .complete(&retry, evaluation_record(&retry, &tested), 200)
            .await?;
        assert_eq!(replay["id"], accepted["id"]);
        let jobs = c.listed(absent).await?;
        let evaluators: Vec<_> = jobs.iter().filter(|j| j["stage"] == "evaluator").collect();
        assert_eq!(evaluators.len(), 2);
        assert_eq!(evaluators[0]["state"], "failed");
        assert_eq!(evaluators[0]["run_number"], 1);
        assert_eq!(evaluators[1]["state"], "completed");
        assert_eq!(evaluators[1]["run_number"], 2);
        assert_eq!(evaluators[1]["origin"], "auto_retry");
        assert_eq!(evaluators[1]["previous_run_id"], evaluators[0]["id"]);
        let cases = c.cases().await?;
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0]["hypothesis"], absent);
        assert!(
            cases[0]["evaluation"]["provenance"]
                .get("control_revision")
                .is_none()
        );
        // An otherwise valid external record cannot claim another policy's
        // result. The rejected record is not published; its rerun retains the
        // original policy and accepts a matching record.
        let policy_number = c.submit(true, None).await?;
        c.tester().await?;
        let job = c.claim_evaluation().await?;
        let tested = c.evidence(&job).await?;
        let mut record = evaluation_record(&job, &tested);
        record["assessment"]["policy_revision"] = json!("policy-r0");
        let rejected = c.complete(&job, record, 422).await?;
        assert_eq!(
            rejected["error"]["details"][0]["path"],
            "/evidence/assessment/policy_revision"
        );
        assert_eq!(
            c.cases().await?.len(),
            1,
            "wrong policy must not publish a result"
        );
        let retry = c.claim_evaluation().await?;
        assert_eq!(retry["evaluator"]["revision"], "fixture-policy-1");
        c.complete(&retry, evaluation_record(&retry, &tested), 200)
            .await?;
        let jobs = c.listed(policy_number).await?;
        let evaluators: Vec<_> = jobs.iter().filter(|j| j["stage"] == "evaluator").collect();
        assert_eq!(evaluators.len(), 2);
        assert_eq!(evaluators[0]["state"], "failed");
        assert_eq!(evaluators[0]["error_code"], "invalid_output");
        assert!(evaluators[0]["evidence"].is_null());
        assert_eq!(evaluators[1]["state"], "completed");
        assert_eq!(evaluators[1]["origin"], "auto_retry");
        assert_eq!(c.cases().await?.len(), 2);
        c.export("external-revisions").await
    }
    .await
    .map_err(safe)
}
