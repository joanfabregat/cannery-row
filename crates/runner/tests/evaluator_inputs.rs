#![forbid(unsafe_code)]
use cannery_core::json::{self, Document};
use cannery_runner::{
    cli_depth::PolicyEntryPoint,
    evaluator_inputs::{self, EvaluationError},
    policy,
};
use serde_json::{Value, json};
use std::sync::Arc;
type Error = Box<dyn std::error::Error + Send + Sync>;
fn document(value: &Value) -> Result<Document, Error> {
    Ok(json::decode(&serde_json::to_vec(value)?, 256)?)
}
fn digest(value: &Value) -> Result<String, Error> {
    Ok(json::canonical::sha256(&document(value)?, 256)?)
}
#[test]
fn verified_records_match_pins_in_job_order_and_reject_ambiguous_evidence() -> Result<(), Error> {
    let one = json!({"stage":"tester","measurements":[],"provenance":{"source_revision":"one"}});
    let two = json!({"stage":"tester","measurements":[],"provenance":{"source_revision":"two"}});
    let refs = document(
        &json!([{"ref":"one","sha256":digest(&one)?},{"ref":"two","sha256":digest(&two)?}]),
    )?;
    let ordered = evaluator_inputs::verified_records(
        &refs,
        &document(&json!([two.clone(), one.clone()]))?,
        256,
    )?;
    assert_eq!(json::canonical::sha256(&ordered[0], 256)?, digest(&one)?);
    assert_eq!(json::canonical::sha256(&ordered[1], 256)?, digest(&two)?);
    for evidence in [
        json!([one.clone()]),
        json!([one.clone(), one.clone()]),
        json!([one.clone(), two.clone(), one.clone()]),
        json!([one.clone(),{"stage":"tester","tampered":true}]),
    ] {
        assert!(matches!(
            evaluator_inputs::verified_records(&refs, &document(&evidence)?, 256),
            Err(EvaluationError::EvidenceMismatch)
        ));
    }
    let duplicate = document(
        &json!([{"ref":"one","sha256":digest(&one)?},{"ref":"duplicate","sha256":digest(&one)?}]),
    )?;
    assert!(matches!(
        evaluator_inputs::verified_records(&duplicate, &document(&json!([one, two]))?, 256),
        Err(EvaluationError::EvidenceMismatch)
    ));
    Ok(())
}
#[test]
fn stock_record_preserves_pins_and_uses_exact_decimal_gate_arithmetic() -> Result<(), Error> {
    let policy = policy::parse_policy(
        Arc::new(document(
            &json!({"schema_version":"0.2","evaluator":{"id":"stock-evaluator","revision":"v1"},"gates":[{"id":"improvement","metric":"mrr","split":"dev","statistic":"value","compare":"control","op":">=","min_delta":0.02}],"baselines":[]}),
        )?),
        PolicyEntryPoint::Evaluator,
        256,
    )?;
    let evidence = json!({"stage":"tester","provenance":{"source_revision":"commit","dataset_revision":"data","control_revision":"base"},"measurements":[{"metric":"mrr","split":"dev","value":0.42,"control_value":0.4,"authority":"tester_verified"}]});
    let mut job = json!({"attempt_id":"attempt","science_revision":1,"evaluator":{"id":"stock-evaluator","revision":"v1"},"inputs":{"evidence":[{"ref":"evidence-id","sha256":digest(&evidence)?}]}});
    let science = document(
        &json!({"metrics":[{"key":"mrr","direction":"higher","splits":["dev"],"dimensions":[]}]}),
    )?;
    let served = document(&json!([evidence]))?;
    let record = evaluator_inputs::stock_record(
        &policy,
        &document(&job)?,
        &science,
        &served,
        "2026-10-04T00:00:00Z",
        "2026-10-04T00:00:01Z",
        256,
    )?;
    let value: Value = serde_json::from_slice(&json::encode_http(&record, 256)?)?;
    assert_eq!(value["assessment"]["verdict"], "pass");
    assert_eq!(value["assessment"]["gates"][0]["result"], "pass");
    assert_eq!(value["provenance"]["science_revision"], "1");
    assert_eq!(value["provenance"]["dataset_revision"], "data");
    assert_eq!(value["assessment"]["evidence"], job["inputs"]["evidence"]);
    job["evaluator"]["revision"] = json!("different");
    assert!(matches!(
        evaluator_inputs::stock_record(
            &policy,
            &document(&job)?,
            &science,
            &served,
            "start",
            "end",
            256
        ),
        Err(EvaluationError::PolicyMismatch)
    ));
    Ok(())
}

#[test]
fn policy_verdict_cannot_forge_provenance_gates_or_verified_values() -> Result<(), Error> {
    let policy = policy::StepPolicy {
        evaluator_id: String::from("policy-evaluator"),
        revision: String::from("v1"),
        document: Arc::new(document(&json!(null))?),
        envelope: Arc::new(document(&json!(null))?),
        name: String::from("policy"),
        needs_data_root: false,
    };
    let evidence = json!({"provenance":{"source_revision":"commit","dataset_revision":"data"},"measurements":[{"metric":"mrr","split":"dev","dimensions":{},"authority":"tester_verified","value":0.42}]});
    let job = document(
        &json!({"attempt_id":"attempt","science_revision":1,"evaluator":{"id":"policy-evaluator","revision":"v1"},"inputs":{"evidence":[{"ref":"record","sha256":digest(&evidence)?}]}}),
    )?;
    let science = document(&json!({"metrics":[{"key":"mrr","splits":["dev"],"dimensions":[]}]}))?;
    let served = document(&json!([evidence]))?;
    let contracts = cannery_core::contracts::ContractValidator::new()?;
    let verdict =
        json!({"gates":[{"id":"quality","result":"pass"}],"verdict":"pass","reason":"assessed"});
    let evaluate = |verdict: &Value| -> Result<Document, Error> {
        Ok(evaluator_inputs::policy_record(
            &policy,
            &job,
            &science,
            &served,
            &document(verdict)?,
            "2026-10-04T00:00:00Z",
            "2026-10-04T00:00:01Z",
            256,
            &contracts,
        )?)
    };
    let record: Value = serde_json::from_slice(&json::encode_http(&evaluate(&verdict)?, 256)?)?;
    assert_eq!(record["provenance"]["source_revision"], "commit");
    assert_eq!(record["provenance"]["dataset_revision"], "data");
    assert_eq!(record["assessment"]["comparisons"], json!([]));
    let mut valid_citation = verdict.clone();
    valid_citation["comparisons"] = json!([{"metric":"mrr","split":"dev","dimensions":{},"source":"tester","value":0.42,"reference":{"value":0.4,"label":"baseline","kind":"baseline"}}]);
    evaluate(&valid_citation)?;
    let mut forged = valid_citation;
    forged["comparisons"][0]["value"] = json!(0.43);
    let mut duplicate = verdict.clone();
    duplicate["gates"] = json!([{"id":"quality","result":"pass"},{"id":"quality","result":"pass"}]);
    let mut contradicted = verdict.clone();
    contradicted["gates"][0]["result"] = json!("fail");
    let mut provenance = verdict.clone();
    provenance["provenance"] = json!({"source_revision":"forged"});
    for invalid in [forged, duplicate, contradicted, provenance] {
        assert!(evaluate(&invalid).is_err());
    }
    Ok(())
}
