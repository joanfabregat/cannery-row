//! `cannery evaluator`: the stock policy applied offline to a scorer's evidence.
#![forbid(unsafe_code)]
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
type Error = Box<dyn std::error::Error + Send + Sync>;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/fixture")
        .join(name)
}
fn evidence(overall: f64) -> Value {
    let measurement = |dimensions: Value, value: f64| json!({"metric":"mrr","split":"dev","dimensions":dimensions,"value":value,"authority":"tester_verified","unit":"ratio","direction":"higher"});
    json!({
        "provenance": {"source_revision": "commit"},
        "measurements": [
            measurement(json!({}), overall),
            measurement(json!({"language": "en"}), 0.75),
            measurement(json!({"language": "fr"}), 0.5)
        ]
    })
}
fn evaluate(
    binary: &std::ffi::OsStr,
    config: &Path,
    science: &Path,
    evidence: &Path,
) -> Result<std::process::Output, Error> {
    Ok(std::process::Command::new(binary)
        .arg("evaluator")
        .arg("--config")
        .arg(config)
        .arg("--science")
        .arg(science)
        .arg("--evidence")
        .arg(evidence)
        .env_remove("CANNERY_DATABASE_URL")
        .env_remove("CANNERY_SETTINGS")
        .output()?)
}

#[test]
#[ignore = "Requires the actual installed cannery CLI"]
fn stock_evaluator_applies_policy_gates_offline() -> Result<(), Error> {
    let binary = std::env::var_os("CANNERY_EVALUATOR_BINARY").ok_or("CLI binary required")?;
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce)?;
    let directory = std::env::temp_dir().join(format!(
        "cr-evaluator-cli-{}",
        uuid::Uuid::from_bytes(nonce).simple()
    ));
    std::fs::create_dir(&directory)?;
    let result = (|| {
        let science: Value = serde_json::from_slice(&std::fs::read(fixture("science.json"))?)?;
        let response = directory.join("science.json");
        std::fs::write(
            &response,
            serde_json::to_vec(&json!({"revision": 1, "content": science}))?,
        )?;
        let policy = fixture("policy.json");
        for (overall, verdict) in [(0.7, "pass"), (0.5, "fail")] {
            let path = directory.join("evidence.json");
            std::fs::write(&path, serde_json::to_vec(&evidence(overall))?)?;
            let output = evaluate(&binary, &policy, &response, &path)?;
            assert!(output.status.success(), "stock evaluator failed");
            let printed: Value = serde_json::from_slice(&output.stdout)?;
            assert_eq!(printed["verdict"], verdict);
            assert_eq!(printed["policy_revision"], "fixture-policy-1");
            assert_eq!(printed["gates"][0]["id"], "mrr-holds-control");
            assert_eq!(printed["gates"][0]["result"], verdict);
        }
        let judged = directory.join("judged.json");
        let mut invalid = evidence(0.7);
        invalid["verdict"] = json!("pass");
        std::fs::write(&judged, serde_json::to_vec(&invalid)?)?;
        let refused = evaluate(&binary, &policy, &response, &judged)?;
        assert_eq!(refused.status.code(), Some(1));
        assert_eq!(refused.stdout, b"");
        let step = evaluate(&binary, &fixture("policy-step.json"), &response, &judged)?;
        assert_eq!(step.status.code(), Some(2));
        let missing = evaluate(&binary, &policy, &directory.join("absent.json"), &judged)?;
        assert_eq!(missing.status.code(), Some(2));
        Ok::<(), Error>(())
    })();
    std::fs::remove_dir_all(directory)?;
    result
}
