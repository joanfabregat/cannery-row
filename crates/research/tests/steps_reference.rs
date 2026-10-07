//! Source registration decisions and helper projections, without schema narrowing.
#![forbid(unsafe_code)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "Source fixture assertions"
)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::json::{self, Document, DocumentBuilder, Node};
use cannery_research::{
    science::{RenderingContext, Science, ScienceError},
    steps::{self, Role},
};
use num_bigint::BigInt;
use serde_json::Value;
const CONTEXT: RenderingContext = RenderingContext {
    nesting_budget: 200,
};
fn string(document: &Document) -> Result<&String, ScienceError> {
    if let Some(Node::String(s)) = document.node(document.root()) {
        Ok(s)
    } else {
        Err(ScienceError::Type)
    }
}
fn observed(case: &Value) -> Result<Document, ScienceError> {
    let document = json::decode_str(case["document_json"].as_str().unwrap(), 200).unwrap();
    let content = json::decode_str(case["science_json"].as_str().unwrap(), 200).unwrap();
    let path = json::decode_str(case["path_json"].as_str().unwrap(), 200).unwrap();
    let path = string(&path)?;
    let mut output = DocumentBuilder::new();
    let root = match case["operation"].as_str().unwrap() {
        "trust" => output
            .push(Node::String(String::from(
                steps::trust_class(string(&document)?).name(),
            )))
            .unwrap(),
        "artifacts" => output
            .import(
                &document,
                steps::artifacts(&document, document.root(), steps::Side::Inputs)?,
            )
            .unwrap(),
        "quantity" => {
            let quantity = steps::quantity(string(&document)?)?;
            let numerator = output
                .push(Node::String(String::from(&quantity.numer().to_string())))
                .unwrap();
            let denominator = output
                .push(Node::String(String::from(&quantity.denom().to_string())))
                .unwrap();
            output
                .push(Node::Array(vec![numerator, denominator]))
                .unwrap()
        }
        "secret" => output
            .push(Node::Bool(steps::looks_secret(string(&document)?)))
            .unwrap(),
        "reserved" => {
            let Ok(value) = string(&document) else {
                return Err(ScienceError::Attribute);
            };
            output.push(Node::Bool(steps::reserved_env(value))).unwrap()
        }
        "source_id" => output
            .push(Node::String(steps::source_id(
                &document,
                document.root(),
                CONTEXT,
            )?))
            .unwrap(),
        "source_pointer" => output
            .push(Node::String(steps::source_pointer(
                &document,
                document.root(),
                path,
            )?))
            .unwrap(),
        "setup" => output
            .push(steps::setup_deadline(&document)?.map_or(Node::Null, Node::Integer))
            .unwrap(),
        "step" => {
            let role = match case["role"].as_str().unwrap() {
                "producer" => Role::Producer,
                "scorer" => Role::Scorer,
                "validator" => Role::Validator,
                "experiment" => Role::Experiment,
                "evaluator" => Role::Evaluator,
                _ => panic!("role"),
            };
            steps::check_step(
                &Science::new(BigInt::from(1), &content, CONTEXT)?,
                &document,
                role,
                path,
                CONTEXT,
            )?;
            output.push(Node::Null).unwrap()
        }
        "pair" => {
            steps::check_pair(
                &Science::new(BigInt::from(1), &content, CONTEXT)?,
                &document,
                path,
                CONTEXT,
            )?;
            output.push(Node::Null).unwrap()
        }
        "validators" => {
            steps::check_validators(&Science::new(BigInt::from(1), &content, CONTEXT)?, CONTEXT)?;
            output.push(Node::Null).unwrap()
        }
        _ => panic!("operation"),
    };
    Ok(output.finish(root).unwrap())
}
fn assert_native_registration_boundary(
    case: &Value,
    contracts: &cannery_core::contracts::ContractValidator,
) -> bool {
    let mut rejected = false;
    for key in ["document_json", "science_json", "path_json"] {
        let raw = case[key].as_str().unwrap();
        if serde_json::from_str::<Value>(raw).is_err() {
            assert!(json::decode_str(raw, 200).is_err());
            rejected = true;
        }
    }
    if rejected {
        return true;
    }
    let input: Value = serde_json::from_str(case["document_json"].as_str().unwrap()).unwrap();
    let registry: Value = serde_json::from_str(case["science_json"].as_str().unwrap()).unwrap();
    if ["max_deadline_seconds", "max_setup_deadline_seconds"]
        .iter()
        .any(|key| registry["limits"][key].is_boolean())
    {
        let document = json::decode_str(case["science_json"].as_str().unwrap(), 200).unwrap();
        assert!(!contracts.is_valid(
            cannery_core::contracts::ContractKind::ScienceRevision,
            &document
        ));
        return true;
    }
    if case["operation"] == "step"
        && let Some(raw) = input["spec"].get("activeDeadlineSeconds")
        && (!raw.is_number() || raw.to_string().contains(['.', 'e', 'E']))
    {
        let document = json::decode_str(case["document_json"].as_str().unwrap(), 200).unwrap();
        assert!(!contracts.is_valid(
            cannery_core::contracts::ContractKind::StepManifest,
            &document
        ));
        return true;
    }
    if case["operation"] == "quantity"
        && input
            .as_str()
            .is_some_and(|s| !s.is_ascii() || s.len() > 1024 || s.ends_with('\n'))
    {
        assert_eq!(
            observed(case).unwrap_err(),
            ScienceError::Value,
            "{}",
            case["name"]
        );
        return true;
    }
    if matches!(case["operation"].as_str(), Some("setup" | "step"))
        && let Some(value) = input["spec"]["setup"].get("activeDeadlineSeconds")
    {
        let expected = if value.is_i64() {
            None
        } else if value.is_number() && !value.to_string().contains(['.', 'e', 'E']) {
            Some(ScienceError::Overflow)
        } else {
            Some(ScienceError::Type)
        };
        if let Some(expected) = expected {
            assert_eq!(observed(case).unwrap_err(), expected, "{}", case["name"]);
            return true;
        }
    }
    false
}

#[test]
fn registration_preserves_source_decisions_and_rejects_private_coercions() {
    let fixture: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/research/tests/fixtures/steps_reference.json"
    ))
    .unwrap();
    assert_eq!(fixture["runtime"], "3.13.11");
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 471);
    let contracts = cannery_core::contracts::ContractValidator::new().unwrap();
    for case in cases {
        if assert_native_registration_boundary(case, &contracts) {
            continue;
        }
        match observed(case) {
            Ok(actual) => {
                assert!(
                    case["error"].is_null(),
                    "unexpected success {}",
                    case["name"]
                );
                let mut expected: Value =
                    serde_json::from_str(case["result_json"].as_str().unwrap()).unwrap();
                if case["operation"] == "source_id" {
                    let input: Value =
                        serde_json::from_str(case["document_json"].as_str().unwrap()).unwrap();
                    let value = input.get("id").unwrap_or(&input["name"]);
                    expected = Value::String(
                        value
                            .as_str()
                            .map_or_else(|| value.to_string(), str::to_owned),
                    );
                }
                let expected = json::decode_str(&expected.to_string(), 200).unwrap();
                assert_eq!(
                    json::encode_ascii_pretty(&actual, 200).unwrap(),
                    json::encode_ascii_pretty(&expected, 200).unwrap(),
                    "projection {}",
                    case["name"]
                );
            }
            Err(error) => {
                assert_eq!(
                    error.class(),
                    case["error"].as_str().unwrap_or("native unexpected error"),
                    "error {}",
                    case["name"]
                );
                if let ScienceError::Validation { path, message } = error {
                    let expected =
                        json::decode_str(case["error_path_json"].as_str().unwrap(), 200).unwrap();
                    assert_eq!(&path, string(&expected).unwrap(), "path {}", case["name"]);
                    assert_ne!(message, "");
                }
            }
        }
    }
}

#[test]
fn native_quantities_are_exact_ascii_and_bounded() {
    use num_rational::BigRational;
    for (raw, numerator, denominator) in [
        ("0", 0, 1),
        ("1.5", 3, 2),
        ("250m", 1, 4),
        ("1.5Ki", 1536, 1),
        ("2M", 2_000_000, 1),
    ] {
        assert_eq!(
            steps::quantity(&raw.into()).unwrap(),
            BigRational::new(numerator.into(), denominator.into())
        );
    }
    let accepted = "9".repeat(1024);
    assert_eq!(
        steps::quantity(&accepted).unwrap(),
        BigRational::from_integer(accepted.parse::<BigInt>().unwrap())
    );
    for raw in [
        "1\n", "1Ki\n", "١٢", "１２Mi", "1_000", " 1", "+1", "-1", ".5", "1.", "1e3", "1K", "NaN",
        "Infinity",
    ] {
        assert_eq!(
            steps::quantity(&raw.into()),
            Err(ScienceError::Value),
            "{raw}"
        );
    }
    assert_eq!(steps::quantity(&"9".repeat(1025)), Err(ScienceError::Value));
    assert_eq!(
        steps::quantity(&format!("{}Ki", "9".repeat(1023))),
        Err(ScienceError::Value)
    );
}
