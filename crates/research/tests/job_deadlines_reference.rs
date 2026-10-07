#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::json::{self, Document, DocumentBuilder, Node};
use cannery_research::{
    job_deadlines,
    science::{RenderingContext, Science, ScienceError},
};
use num_bigint::BigInt;
use serde_json::Value;

const RENDERING: RenderingContext = RenderingContext {
    nesting_budget: 200,
};

fn observe(case: &Value) -> Result<BigInt, ScienceError> {
    let input = json::decode_str(
        case["manifests_json"].as_str().ok_or(ScienceError::Type)?,
        200,
    )
    .map_err(|_| ScienceError::Value)?;
    let Some(Node::Array(nodes)) = input.node(input.root()) else {
        return Err(ScienceError::Type);
    };
    let manifests: Vec<Document> = nodes
        .iter()
        .map(|node| {
            let mut builder = DocumentBuilder::new();
            let root = builder
                .import(&input, *node)
                .map_err(|_| ScienceError::InvalidNode)?;
            builder.finish(root).map_err(|_| ScienceError::InvalidNode)
        })
        .collect::<Result<_, _>>()?;
    if case["operation"] == "step" {
        return job_deadlines::step_seconds(manifests.first().ok_or(ScienceError::Type)?);
    }
    let content = json::decode_str(
        case["science_json"].as_str().ok_or(ScienceError::Type)?,
        200,
    )
    .map_err(|_| ScienceError::Value)?;
    let science = Science::new(BigInt::from(1), &content, RENDERING)?;
    let manifests: Vec<&Document> = manifests.iter().collect();
    match case["operation"].as_str() {
        Some("validation") => job_deadlines::validation_seconds(
            &science,
            manifests.first().ok_or(ScienceError::Type)?,
            RENDERING,
        ),
        Some("steps") => job_deadlines::steps_seconds(&science, &manifests, RENDERING),
        Some("workflow") => job_deadlines::workflow_seconds(
            &science,
            &manifests,
            &case["overhead"]
                .as_str()
                .ok_or(ScienceError::Type)?
                .parse()
                .map_err(|_| ScienceError::Value)?,
            RENDERING,
        ),
        _ => Err(ScienceError::Value),
    }
}

#[test]
fn deadlines_preserve_source_traversal_with_checked_native_integers()
-> Result<(), Box<dyn std::error::Error>> {
    let reference: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/research/tests/fixtures/job_deadlines_reference.json"
    ))?;
    assert_eq!(reference["python"], "3.13.11");
    let cases = reference["cases"].as_array().ok_or("missing cases")?;
    for (index, case) in cases.iter().enumerate() {
        let mut rejected = false;
        for key in ["manifests_json", "science_json"] {
            if let Some(raw) = case[key].as_str()
                && serde_json::from_str::<Value>(raw).is_err()
            {
                assert!(json::decode_str(raw, 200).is_err());
                rejected = true;
            }
        }
        if rejected {
            continue;
        }
        let input: Value =
            serde_json::from_str(case["manifests_json"].as_str().ok_or("reference string")?)?;
        let mut expected = case["outcome"].clone();
        let registry: Value =
            serde_json::from_str(case["science_json"].as_str().ok_or("reference string")?)?;
        let integer_failure = |manifest: &Value| {
            let spec = &manifest["spec"];
            for raw in [
                spec.get("activeDeadlineSeconds"),
                spec["setup"].get("activeDeadlineSeconds"),
            ]
            .into_iter()
            .flatten()
            {
                if raw.is_i64() {
                    continue;
                }
                return Some(
                    if raw.is_number() && !raw.to_string().contains(['.', 'e', 'E']) {
                        "OverflowError"
                    } else {
                        "TypeError"
                    },
                );
            }
            None
        };
        'manifests: for manifest in input.as_array().ok_or("reference array")? {
            if case["operation"] != "validation"
                && let Some(error) = integer_failure(manifest)
            {
                expected = serde_json::json!({"exception":error});
                break;
            }
            if case["operation"] == "step" {
                break;
            }
            if let Some(outputs) = manifest["spec"]["outputs"]["artifacts"].as_array() {
                for output in outputs {
                    if let Some(interfaces) = registry["interfaces"].as_array() {
                        for interface in interfaces {
                            let version = interface["version"]
                                .as_str()
                                .map_or_else(|| interface["version"].to_string(), str::to_owned);
                            let reference = format!(
                                "{}/v{version}",
                                interface["name"].as_str().ok_or("reference string")?
                            );
                            if output["interface"] == reference
                                && let Some(validators) = registry["validators"].as_array()
                            {
                                for validator in validators {
                                    if validator["metadata"]["name"] == interface["validator"]
                                        && let Some(error) = integer_failure(validator)
                                    {
                                        expected = serde_json::json!({"exception":error});
                                        break 'manifests;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        let outcome = match observe(case) {
            Ok(value) => serde_json::json!({"value":value.to_string()}),
            Err(error) => serde_json::json!({"exception":error.class()}),
        };
        assert_eq!(outcome, expected, "deadline observation {index}");
    }
    assert_eq!(cases.len(), 164);
    Ok(())
}
