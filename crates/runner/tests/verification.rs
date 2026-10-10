#![forbid(unsafe_code)]
use cannery_core::{
    contracts::phases::{Phase, PhaseSchemas},
    front_matter::Limits,
    json::{self, Document},
};
use cannery_runner::{
    cli_depth::PolicyEntryPoint,
    policy::{self, StockPolicy},
    verification::{self, ReportError},
};
use serde_json::{Map, Value, json};
use std::{path::PathBuf, sync::Arc};
type Error = Box<dyn std::error::Error + Send + Sync>;

fn fixture(name: &str) -> Result<Value, Error> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/fixture")
        .join(name);
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}
fn document(value: &Value) -> Result<Document, Error> {
    Ok(json::decode(&serde_json::to_vec(value)?, 256)?)
}
fn stock() -> Result<StockPolicy, Error> {
    Ok(policy::parse_policy(
        Arc::new(document(&fixture("policy.json")?)?),
        PolicyEntryPoint::Evaluator,
        256,
    )?)
}
fn measurement(language: Option<&str>, value: f64) -> Value {
    let dimensions = language.map_or_else(|| json!({}), |language| json!({"language": language}));
    json!({"metric":"mrr","split":"dev","dimensions":dimensions,"value":value,"authority":"tester_verified","unit":"ratio","direction":"higher"})
}
fn evidence(overall: f64) -> Value {
    json!({
        "provenance": {"source_revision": "commit", "dataset_revision": "data"},
        "measurements": [measurement(None, overall), measurement(Some("en"), 0.75), measurement(Some("fr"), 0.5)],
        "artifact_roles": ["evidence"],
        "observations": "Scored every query."
    })
}
fn job() -> Value {
    json!({"job_id": "job", "science_revision": 3, "phase": "verify"})
}

#[test]
fn evidence_is_an_object_of_known_keys_without_a_verdict() {
    assert!(verification::check_evidence(&evidence(0.7)).is_ok());
    let mut invalid = vec![
        json!([]),
        json!({"measurements": []}),
        json!({"provenance": {}}),
    ];
    for (key, value) in [
        ("verdict", json!("pass")),
        ("gates", json!([])),
        ("observations", json!(["not text"])),
        ("measurements", json!({})),
    ] {
        let mut changed = evidence(0.7);
        changed[key] = value;
        invalid.push(changed);
    }
    for evidence in invalid {
        assert_eq!(
            verification::check_evidence(&evidence),
            Err(ReportError::Evidence)
        );
    }
}

#[test]
fn stock_assessment_applies_the_default_control_with_exact_gates() -> Result<(), Error> {
    let policy = stock()?;
    let science = fixture("science.json")?;
    let pass = verification::stock_assessment(&policy, &science, &evidence(0.7), None, 256)?;
    assert_eq!(pass["verdict"], "pass");
    assert_eq!(pass["gates"][0]["id"], "mrr-holds-control");
    assert_eq!(pass["gates"][0]["result"], "pass");
    let equal = verification::stock_assessment(&policy, &science, &evidence(0.625), None, 256)?;
    assert_eq!(equal["verdict"], "pass");
    let fail = verification::stock_assessment(&policy, &science, &evidence(0.6), None, 256)?;
    assert_eq!(fail["verdict"], "fail");
    assert_eq!(fail["gates"][0]["result"], "fail");
    assert_eq!(
        verification::stock_assessment(&policy, &science, &json!({"verdict": "pass"}), None, 256)
            .err(),
        Some(ReportError::Evidence)
    );
    Ok(())
}

#[test]
fn markdown_writes_one_json_value_per_front_matter_key() -> Result<(), Error> {
    let mut front_matter = Map::new();
    front_matter.insert("verdict".into(), json!("pass"));
    front_matter.insert("gates".into(), json!([{"id": "a: b", "result": "pass"}]));
    assert_eq!(
        verification::markdown(&front_matter, "Body.\n")?,
        "---\n\"verdict\": \"pass\"\n\"gates\": [{\"id\":\"a: b\",\"result\":\"pass\"}]\n---\nBody.\n"
    );
    Ok(())
}

#[test]
fn compose_writes_a_valid_report_from_evidence_and_verdict() -> Result<(), Error> {
    let policy = stock()?;
    let science = fixture("science.json")?;
    let schemas = PhaseSchemas::new()?;
    let evidence = evidence(0.7);
    let verdict = verification::stock_assessment(&policy, &science, &evidence, None, 256)?;
    let text = verification::compose(
        &job(),
        &policy.revision,
        &evidence,
        &verdict,
        &science,
        &schemas,
    )?;
    assert!(text.ends_with("---\nScored every query.\n"));
    let parsed = schemas.parse(Phase::Verification, &text, Limits::default())?;
    let report = Value::Object(parsed.front_matter);
    assert_eq!(report["verdict"], "pass");
    assert_eq!(report["policy_revision"], "fixture-policy-1");
    assert_eq!(report["provenance"]["science_revision"], "3");
    assert_eq!(report["provenance"]["source_revision"], "commit");
    assert_eq!(report["measurements"], evidence["measurements"]);
    assert_eq!(report["artifact_roles"], json!(["evidence"]));
    Ok(())
}

#[test]
fn compose_refuses_a_verdict_that_forges_or_contradicts_the_report() -> Result<(), Error> {
    let science = fixture("science.json")?;
    let schemas = PhaseSchemas::new()?;
    let evidence = evidence(0.7);
    let verdict = passing_verdict();
    let compose = |evidence: &Value, verdict: &Value| {
        verification::compose(&job(), "v1", evidence, verdict, &science, &schemas)
    };
    compose(&evidence, &verdict)?;
    let mut cited = verdict.clone();
    cited["comparisons"] = json!([{"metric":"mrr","split":"dev","dimensions":{},"source":"tester","value":0.7,"reference":{"value":0.625,"label":"control","kind":"baseline"}}]);
    compose(&evidence, &cited)?;
    let mut forged = cited;
    forged["comparisons"][0]["value"] = json!(0.71);
    let mut duplicate = verdict.clone();
    duplicate["gates"] = json!([{"id":"quality","result":"pass"},{"id":"quality","result":"pass"}]);
    let mut provenance = verdict.clone();
    provenance["provenance"] = json!({"source_revision": "forged"});
    for invalid in [forged, duplicate, provenance, json!([])] {
        assert_eq!(compose(&evidence, &invalid), Err(ReportError::Verdict));
    }
    let mut contradicted = verdict;
    contradicted["gates"][0]["result"] = json!("fail");
    assert_eq!(compose(&evidence, &contradicted), Err(ReportError::Report));
    let mut long = evidence.clone();
    long["observations"] = json!("x".repeat(70_000));
    assert_eq!(
        compose(&long, &passing_verdict()),
        Err(ReportError::Evidence)
    );
    let mut unverified = evidence;
    unverified["measurements"][0]["authority"] = json!("agent_claim");
    assert_eq!(
        compose(&unverified, &passing_verdict()),
        Err(ReportError::Report)
    );
    Ok(())
}
fn passing_verdict() -> Value {
    json!({"gates":[{"id":"quality","result":"pass"}],"verdict":"pass","reason":"assessed"})
}
