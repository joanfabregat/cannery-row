//! Actual trusted policy-step inputs and deadline headroom through the CLI.
#![allow(clippy::too_many_arguments, clippy::too_many_lines, dead_code)]
include!("support/policy_step_contracts.rs");

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn declared_policy_inputs_and_parameter_defaults() -> Result<()> {
    async {
        for parameters in [json!({}), json!({"top_k":3})] {
            let mut c = ControlWorld::new().await?;
            let policy = fixture("examples/fixture/policy-step.json")?;
            let mut science = fixture("examples/fixture/science.json")?;
            science["evaluator"] = policy["evaluator"].clone();
            c.science(science).await?;
            install_policy_observer(&c, &policy)?;
            let mut document = ControlWorld::hypothesis(true)?;
            if parameters != json!({}) {
                document["project_fields"] = parameters.clone();
            }
            let n = controlled_submit(&mut c.world, &c.base, &c.agent, &c.admin, document, None)
                .await?;
            c.tester().await?;
            run_policy(&c, "completed").await?;
            let listed = c.listed(n).await?;
            let evaluated = listed
                .iter()
                .find(|j| j["stage"] == "evaluator")
                .ok_or("evaluation missing")?;
            assert_eq!(evaluated["state"], "completed");
            let observed = policy_observation(&mut c, evaluated).await?;
            assert_eq!(observed["role"], "evaluator");
            assert_eq!(observed["evaluator"], policy["evaluator"]);
            assert_eq!(observed["metrics"], json!(["mrr"]));
            assert_eq!(
                observed["control"],
                json!({"id":"base-camp","revision":"fixture-r1"})
            );
            assert_eq!(observed["parameters"], parameters);
            assert_eq!(observed["lease_present"], false);
            assert_eq!(
                observed["evidence"],
                json!([{"keys":["record","ref","sha256"],"stage":"tester"}])
            );
            let roles = observed["manifest_roles"]
                .as_array()
                .ok_or("roles missing")?;
            assert!(
                roles.contains(&json!("evidence")) && roles.contains(&json!("per_query_results"))
            );
            assert_eq!(
                observed["input_files"]["per_query_results"],
                json!(["results.jsonl"])
            );
            assert_eq!(
                observed["input_names"],
                json!(["evidence", "manifest", "per_query_results"])
            );
            c.export("declared-policy-inputs").await?;
        }
        Ok(())
    }
    .await
    .map_err(safe)
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn policy_deadline_headroom_refuses_before_launch() -> Result<()> {
    async {
        let mut c = ControlWorld::new().await?;
        let mut policy = fixture("examples/fixture/policy-step.json")?;
        let mut science = fixture("examples/fixture/science.json")?;
        science["evaluator"] = policy["evaluator"].clone();
        science["max_auto_retries"] = json!(0);
        c.science(science).await?;
        policy["step"]["spec"]["activeDeadlineSeconds"] = json!(590);
        install_policy_observer(&c, &policy)?;
        let n = c.submit(true, None).await?;
        c.tester().await?;
        run_policy(&c, "failed").await?;
        let listed = c.listed(n).await?;
        let failed = listed.iter().find(|j| j["stage"] == "evaluator").ok_or("evaluation missing")?;
        assert_eq!(failed["state"], "failed");
        assert_eq!(failed["error_code"], "evaluator_error");
        assert_eq!(failed["error_reason"], "policy step fixture-policy needs up to 620s (590s for the step, 30s kept by the runner) but an evaluation job of science revision 2 has 600s (max_deadline_seconds)");
        assert_eq!(failed["logs"], json!([]));
        assert_eq!(listed.len(), 2);
        assert!(!c.work.0.join("policy-launched").exists(), "policy executed despite insufficient headroom");
        c.export("policy-headroom").await
    }.await.map_err(safe)
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn policy_dataset_alias_and_baseline_are_staged_by_registered_id() -> Result<()> {
    async {
        let mut c = ControlWorld::new().await?;
        let mut policy = fixture("examples/fixture/policy-step.json")?;
        policy["step"]["spec"]["inputs"]["artifacts"].as_array_mut().ok_or("inputs missing")?.extend([
            json!({"name":"labels","from":"dataset","id":"nanobeir-qrels","path":"/cr/inputs/labels"}),
            json!({"name":"control","from":"baseline","id":"base-camp","path":"/cr/inputs/control"}),
        ]);
        let mut science = fixture("examples/fixture/science.json")?;
        science["evaluator"] = policy["evaluator"].clone();
        science["datasets"].as_array_mut().ok_or("datasets missing")?.push(json!({"id":"nanobeir-qrels","revision":"qrels-r1","held_out_labels":true}));
        c.science(science).await?;
        install_policy_observer(&c, &policy)?;
        let fixture_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/fixture").canonicalize()?;
        let data = c.work.0.join("policy-data");
        copy_policy_data(&fixture_root.join("data"), &data)?;
        copy_policy_data(&fixture_root.join("data/datasets/qrels"), &data.join("datasets/nanobeir-qrels"))?;
        let baseline = data.join("baselines/base-camp/fixture-r1");
        fs::create_dir_all(&baseline)?;
        fs::write(baseline.join("control.json"), b"{\"baseline\":\"base-camp\"}\n")?;
        let config_path = c.work.0.join("observed-policy.toml");
        let configuration = fs::read_to_string(&config_path)?;
        fs::write(config_path, configuration.replace(&toml_string(&fixture_root.join("data").to_string_lossy()), &toml_string(&data.to_string_lossy())))?;
        let n = c.submit(true, None).await?;
        c.tester().await?;
        run_policy(&c, "completed").await?;
        let listed = c.listed(n).await?;
        let evaluated = listed.iter().find(|j| j["stage"] == "evaluator").ok_or("evaluation missing")?;
        assert_eq!(evaluated["state"], "completed");
        let observed = policy_observation(&mut c, evaluated).await?;
        assert_eq!(observed["datasets"], json!([{"name":"labels","id":"nanobeir-qrels","revision":"qrels-r1"}]));
        assert_eq!(observed["input_files"]["labels"], json!(["qrels.json"]));
        assert_eq!(observed["input_files"]["control"], json!(["control.json"]));
        assert_eq!(observed["input_names"], json!(["control","evidence","labels","manifest","per_query_results"]));
        c.export("policy-alias-inputs").await
    }.await.map_err(safe)
}
