//! Observe actual job documents, including dictionary overwrite positions.
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::{
    ids::{AttemptId, JobId},
    json::{self, Node},
    timestamps::Timestamp,
};
use cannery_research::{
    job_documents::{self, JobDocument},
    science::RenderingContext,
};
use num_bigint::BigInt;
use serde_json::Value;
const RENDERING: RenderingContext = RenderingContext {
    nesting_budget: 200,
};

#[test]
fn job_documents_preserve_source_order_leases_and_native_text()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/research/tests/fixtures/job_documents_reference.json"
    ))?;
    let cases = fixture["cases"].as_array().ok_or("cases")?;
    for (index, case) in cases.iter().enumerate() {
        let mut rejected = false;
        for key in ["spec_json", "token_json"] {
            let raw = case[key].as_str().ok_or("case JSON")?;
            if serde_json::from_str::<Value>(raw).is_err() {
                assert!(json::decode_str(raw, 200).is_err());
                rejected = true;
            }
        }
        if rejected {
            continue;
        }
        let text = |name: &str| case[name].as_str().ok_or("case text");
        let timestamp = |name: &str| -> Result<Option<Timestamp>, Box<dyn std::error::Error>> {
            if case[name].is_null() {
                Ok(None)
            } else {
                Ok(Some(text(name)?.parse()?))
            }
        };
        let spec = json::decode_str(text("spec_json")?, 200)?;
        let token = json::decode_str(text("token_json")?, 200)?;
        let Some(Node::String(token)) = token.node(token.root()) else {
            return Err("token".into());
        };
        let stage = String::from(text("stage")?);
        let revision: BigInt = text("revision")?.parse()?;
        let generation: BigInt = text("generation")?.parse()?;
        let job = JobDocument {
            id: "11111111-1111-4111-8111-111111111111".parse::<JobId>()?,
            attempt_id: "22222222-2222-4222-8222-222222222222".parse::<AttemptId>()?,
            stage: &stage,
            science_revision: &revision,
            lease_generation: &generation,
            deadline: timestamp("deadline")?,
            lease_expires_at: timestamp("expires")?,
            spec: &spec,
        };
        match job_documents::job_document(&job, token, RENDERING) {
            Err(error) => {
                let class = match error {
                    job_documents::Error::Invariant => "AssertionError",
                    job_documents::Error::Science(error) => error.class(),
                    job_documents::Error::Build => "BuildError",
                };
                assert_eq!(case["outcome"]["exception"], class, "job document {index}");
            }
            Ok(value) => {
                let expected = case["outcome"]["value_json"]
                    .as_str()
                    .ok_or("expected value")?;
                let mut expected: Value = serde_json::from_str(expected)?;
                let input: Value = serde_json::from_str(text("spec_json")?)?;
                if input.get("control").is_none() && expected.get("control").is_some() {
                    for key in ["id", "revision"] {
                        let raw = &input["inputs"]["baselines"][0][key];
                        expected["control"][key] = Value::String(
                            raw.as_str().map_or_else(|| raw.to_string(), str::to_owned),
                        );
                    }
                }
                let expected = json::decode_str(&expected.to_string(), 200)?;
                assert_eq!(
                    json::encode_ascii_pretty(&value, 200)?,
                    json::encode_ascii_pretty(&expected, 200)?,
                    "job document {index}"
                );
            }
        }
    }
    assert_eq!(cases.len(), 160);
    Ok(())
}
