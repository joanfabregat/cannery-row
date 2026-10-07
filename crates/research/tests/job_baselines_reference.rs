#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::json::{self, Document, DocumentBuilder, Node};
use cannery_research::{
    job_baselines,
    science::{RenderingContext, Science, ScienceError},
};
use num_bigint::BigInt;
use serde_json::Value;

// Explicit shallow fixture profile, not a production caller budget.
const RENDERING: RenderingContext = RenderingContext {
    nesting_budget: 200,
};
fn texts(mut values: Vec<String>, sort: bool) -> Result<Document, ScienceError> {
    if sort {
        values.sort_by_key(cannery_core::text::TextExt::codepoints);
    }
    let mut builder = DocumentBuilder::new();
    let nodes = values
        .into_iter()
        .map(|value| {
            builder
                .push(Node::String(value))
                .map_err(|_| ScienceError::InvalidNode)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let root = builder
        .push(Node::Array(nodes))
        .map_err(|_| ScienceError::InvalidNode)?;
    builder.finish(root).map_err(|_| ScienceError::InvalidNode)
}
fn observe(case: &Value) -> Result<Option<Document>, ScienceError> {
    let decode = |name: &str| {
        json::decode_str(case[name].as_str().ok_or(ScienceError::Type)?, 200)
            .map_err(|_| ScienceError::Value)
    };
    let input = decode("input_json")?;
    match case["operation"].as_str() {
        Some("inputs") => {
            texts(job_baselines::baseline_inputs(&input, RENDERING)?, false).map(Some)
        }
        Some("roles") => {
            texts(job_baselines::predecessor_roles(&input, RENDERING)?, true).map(Some)
        }
        Some("pinned_control") => job_baselines::pinned_control(&input, RENDERING),
        Some("parameters") => job_baselines::pinned_parameters(
            &String::from(case["stage"].as_str().ok_or(ScienceError::Type)?),
            &input,
        ),
        operation => {
            let registry = decode("science_json")?;
            let science = Science::new(BigInt::from(1), &registry, RENDERING)?;
            let control = decode("control_json")?;
            if operation == Some("conflict") {
                job_baselines::control_conflict(&science, &input, Some(&control), RENDERING)?
                    .map(|text| {
                        let mut builder = DocumentBuilder::new();
                        let root = builder
                            .push(Node::String(text))
                            .map_err(|_| ScienceError::InvalidNode)?;
                        builder.finish(root).map_err(|_| ScienceError::InvalidNode)
                    })
                    .transpose()
            } else {
                job_baselines::staged_baselines(&science, &input, Some(&control), RENDERING)
                    .map(Some)
            }
        }
    }
}
#[test]
fn baselines_preserve_source_decisions_with_native_scalar_projections()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/research/tests/fixtures/job_baselines_reference.json"
    ))?;
    assert_eq!(fixture["python"], "3.13.11");
    let cases = fixture["cases"].as_array().ok_or("cases")?;
    for (index, case) in cases.iter().enumerate() {
        let mut rejected = false;
        for key in ["input_json", "control_json", "science_json"] {
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
        match observe(case) {
            Err(error) => assert_eq!(
                serde_json::json!({"exception":error.class()}),
                case["outcome"],
                "baseline observation {index}"
            ),
            Ok(value) => {
                let mut expected: Value = serde_json::from_str(
                    case["outcome"]["value_json"]
                        .as_str()
                        .ok_or_else(|| format!("expected value at {index}"))?,
                )?;
                if matches!(case["operation"].as_str(), Some("inputs" | "roles"))
                    && serde_json::from_str::<Value>(
                        case["input_json"].as_str().ok_or("reference string")?,
                    )?
                    .is_array()
                {
                    let input: Value = serde_json::from_str(
                        case["input_json"].as_str().ok_or("reference string")?,
                    )?;
                    let role = case["operation"] == "roles";
                    let mut ids = Vec::new();
                    for manifest in input.as_array().ok_or("reference array")? {
                        if let Some(artifacts) = manifest["spec"]["inputs"]["artifacts"].as_array()
                        {
                            for artifact in artifacts {
                                if artifact["from"] == if role { "attempt" } else { "baseline" } {
                                    let raw = if role {
                                        &artifact["name"]
                                    } else {
                                        artifact.get("id").unwrap_or(&artifact["name"])
                                    };
                                    ids.push(
                                        raw.as_str().map_or_else(|| raw.to_string(), str::to_owned),
                                    );
                                }
                            }
                        }
                    }
                    if role {
                        ids.sort();
                        ids.dedup();
                    }
                    expected = serde_json::json!(ids);
                }
                if case["operation"] == "pinned_control" && expected.is_object() {
                    let input: Value = serde_json::from_str(
                        case["input_json"].as_str().ok_or("reference string")?,
                    )?;
                    if input.get("control").is_none() {
                        for key in ["id", "revision"] {
                            let raw = &input["inputs"]["baselines"][0][key];
                            expected[key] = Value::String(
                                raw.as_str().map_or_else(|| raw.to_string(), str::to_owned),
                            );
                        }
                    }
                }
                if case["operation"] == "conflict" && expected.is_string() {
                    let control: Value = serde_json::from_str(
                        case["control_json"].as_str().ok_or("reference string")?,
                    )?;
                    let revision = &control["revision"];
                    expected = Value::String(format!(
                        "a step takes baseline {} as input, but the control's revision {} is not registered",
                        control["id"], revision
                    ));
                }
                let expected = json::decode_str(&expected.to_string(), 200)?;
                let null = json::decode_str("null", 200)?;
                assert_eq!(
                    json::encode_ascii_pretty(value.as_ref().unwrap_or(&null), 200)?,
                    json::encode_ascii_pretty(&expected, 200)?,
                    "baseline observation {index}"
                );
            }
        }
    }
    assert_eq!(cases.len(), 493);
    Ok(())
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;

#[test]
fn pinned_revisions_are_bounded_integers_consumed_in_order()
-> Result<(), Box<dyn std::error::Error>> {
    for raw in [
        "true",
        "2.5",
        "\"2\"",
        "null",
        "9223372036854775808",
        "-9223372036854775809",
    ] {
        let expected = if raw.contains("922337203685477580") {
            ScienceError::Overflow
        } else {
            ScienceError::Type
        };
        let document = json::decode_str(
            &format!(
                r#"{{"steps":[{{"name":"first","revision":1}},{{"name":"second","revision":{raw}}},{{"name":"never","revision":3}}]}}"#
            ),
            64,
        )?;
        let mut references = job_baselines::pinned_references(&document, RENDERING)?;
        assert_eq!(
            references.next().ok_or("first reference")??,
            ("first".into(), 1.into())
        );
        assert_eq!(references.next().ok_or("second reference")?, Err(expected));
    }
    for value in [i64::MIN, 0, i64::MAX] {
        let document = json::decode_str(
            &format!(r#"{{"steps":[{{"name":"control","revision":{value}}}]}}"#),
            64,
        )?;
        assert_eq!(
            job_baselines::pinned_references(&document, RENDERING)?
                .next()
                .ok_or("bounded reference")??,
            ("control".into(), value.into())
        );
    }
    Ok(())
}
