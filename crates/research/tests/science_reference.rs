//! Every actual-source science recipe is compared, including malformed legacy data.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::panic, reason = "Test fixture assertions")]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::{
    json::{self, Document, DocumentBuilder, Node, NodeId},
    text,
};
use cannery_research::science::{
    self, InterfaceSpec, RenderingContext, Scalar, Science, ScienceError,
};
use num_bigint::BigInt;
use serde_json::Value;
use std::cell::RefCell;
const CONTEXT: RenderingContext = RenderingContext {
    nesting_budget: 200,
};
struct Output(RefCell<DocumentBuilder>);
impl Output {
    fn new() -> Self {
        Self(RefCell::new(DocumentBuilder::new()))
    }
    fn push(&self, node: Node) -> NodeId {
        self.0.borrow_mut().push(node).unwrap()
    }
    fn text(&self, text: &str) -> NodeId {
        self.push(Node::String(text.to_owned()))
    }
    fn array(&self, values: Vec<NodeId>) -> NodeId {
        self.push(Node::Array(values))
    }
    fn object(&self, values: Vec<(&str, NodeId)>) -> NodeId {
        self.push(Node::Object(
            values
                .into_iter()
                .map(|(key, id)| (String::from(key), id))
                .collect(),
        ))
    }
    fn raw(&self, document: &Document, id: Option<NodeId>, object_default: bool) -> NodeId {
        id.map_or_else(
            || {
                if object_default {
                    self.object(vec![])
                } else {
                    self.push(Node::Null)
                }
            },
            |id| self.0.borrow_mut().import(document, id).unwrap(),
        )
    }
    fn sorted_texts(&self, mut texts: Vec<String>) -> NodeId {
        texts.sort_by_key(|s| text::repr_string(s).unwrap().codepoints().clone());
        self.array(texts.iter().map(|s| self.text(s)).collect())
    }
    fn scalars(&self, document: &Document, values: &[Scalar]) -> NodeId {
        let mut values: Vec<_> = values
            .iter()
            .map(|s| match s {
                Scalar::Text(s) => (text::repr_string(s).unwrap(), self.text(s)),
                Scalar::Null => (String::from("None"), self.push(Node::Null)),
                Scalar::Bool(value) => (value.to_string(), self.push(Node::Bool(*value))),
                Scalar::Number { original, .. } => (
                    text::str_value(document, *original, CONTEXT.nesting_budget).unwrap(),
                    self.raw(document, Some(*original), false),
                ),
            })
            .collect();
        values.sort_by_key(|a| a.0.codepoints());
        self.array(values.into_iter().map(|(_, id)| id).collect())
    }
    fn finish(self, root: NodeId) -> Document {
        self.0.into_inner().finish(root).unwrap()
    }
}
fn interface(output: &Output, document: &Document, spec: &InterfaceSpec) -> NodeId {
    output.object(vec![
        ("ref", output.text(&spec.reference)),
        (
            "encoding",
            spec.explicit_encoding.map_or_else(
                || output.text(&spec.encoding),
                |id| output.raw(document, Some(id), false),
            ),
        ),
        ("schema", output.raw(document, spec.schema, false)),
        (
            "max_bytes",
            spec.max_bytes.as_ref().map_or_else(
                || output.push(Node::Null),
                |i| output.push(Node::Integer(i.clone())),
            ),
        ),
        ("allow_empty", output.push(Node::Bool(spec.allow_empty))),
        (
            "magic",
            spec.magic
                .as_ref()
                .map_or_else(|| output.push(Node::Null), |s| output.text(s)),
        ),
        ("validate", output.push(Node::Bool(spec.validate))),
        (
            "validator",
            spec.validator
                .as_ref()
                .map_or_else(|| output.push(Node::Null), |s| output.text(s)),
        ),
        (
            "parses_content",
            output.push(Node::Bool(spec.parses_content())),
        ),
    ])
}
#[allow(
    clippy::too_many_lines,
    reason = "one ordered projection matches the source oracle fields"
)]
fn view(output: &Output, science: &Science<'_>) -> Result<NodeId, ScienceError> {
    let document = science.content;
    let metrics = output.array(
        science
            .metrics
            .iter()
            .map(|(name, m)| {
                let metric = output.object(vec![
                    ("key", output.text(&m.key)),
                    ("splits", output.scalars(document, &m.splits)),
                    ("dimensions", output.sorted_texts(m.dimensions.clone())),
                    ("unit", output.text(&m.unit)),
                    ("direction", output.text(&m.direction)),
                    (
                        "dimension_values",
                        output.array(
                            m.dimension_values
                                .iter()
                                .map(|(name, values)| {
                                    output.array(vec![
                                        output.text(name),
                                        values.as_ref().map_or_else(
                                            || output.push(Node::Null),
                                            |v| output.scalars(document, v),
                                        ),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                ]);
                output.array(vec![output.text(name), metric])
            })
            .collect(),
    );
    let pairs = |values: &[(String, String)]| {
        output.array(
            values
                .iter()
                .map(|(a, b)| output.array(vec![output.text(a), output.text(b)]))
                .collect(),
        )
    };
    let mut baselines = science.baselines.clone();
    baselines.sort_by_key(|(a, b)| {
        (
            text::repr_string(a).unwrap().codepoints().clone(),
            text::repr_string(b).unwrap().codepoints().clone(),
        )
    });
    let datasets = output.array(
        science
            .datasets
            .iter()
            .map(|(name, d)| {
                output.array(vec![
                    output.text(name),
                    output.object(vec![
                        ("id", output.text(&d.id)),
                        ("revision", output.text(&d.revision)),
                        (
                            "held_out_labels",
                            output.push(Node::Bool(d.held_out_labels)),
                        ),
                    ]),
                ])
            })
            .collect(),
    );
    let facets = science.hypothesis_facets()?;
    let candidate = science.code_repositories("candidate")?;
    let trusted = science.code_repositories("trusted")?;
    let legacy = science.legacy_problem()?;
    Ok(output.object(vec![
        (
            "revision",
            output.push(Node::Integer(science.revision.clone())),
        ),
        ("metrics", metrics),
        ("baseline_refs", pairs(&science.baseline_refs)),
        ("baselines", pairs(&baselines)),
        ("baseline_ids", output.sorted_texts(science.baseline_ids())),
        (
            "hypothesis_fields",
            output.raw(document, science.hypothesis_fields, false),
        ),
        ("datasets", datasets),
        (
            "interfaces",
            output.sorted_texts(science.interfaces.clone()),
        ),
        (
            "interface_specs",
            output.array(
                science
                    .interface_specs
                    .iter()
                    .map(|(name, spec)| {
                        output.array(vec![output.text(name), interface(output, document, spec)])
                    })
                    .collect(),
            ),
        ),
        (
            "validators",
            output.array(
                science
                    .validators
                    .iter()
                    .map(|(name, id)| {
                        output.array(vec![
                            output.text(name),
                            output.raw(document, Some(*id), false),
                        ])
                    })
                    .collect(),
            ),
        ),
        ("limits", output.raw(document, science.limits, true)),
        ("tester", output.raw(document, science.tester, true)),
        (
            "evaluator",
            output.raw(document, science.evaluator(), false),
        ),
        (
            "legacy_problem",
            legacy
                .as_ref()
                .map_or_else(|| output.push(Node::Null), |s| output.text(s)),
        ),
        ("hypothesis_facets", output.sorted_texts(facets)),
        ("candidate_repositories", output.sorted_texts(candidate)),
        ("trusted_repositories", output.sorted_texts(trusted)),
    ]))
}
fn observed(case: &Value) -> Result<Document, ScienceError> {
    let document = json::decode_str(case["document_json"].as_str().unwrap(), 200).unwrap();
    let context = CONTEXT;
    let secondary = json::decode_str(case["secondary_json"].as_str().unwrap(), 200).unwrap();
    let output = Output::new();
    let root = match case["operation"].as_str().unwrap() {
        "view" => view(&output, &Science::new(BigInt::from(7), &document, context)?)?,
        "check_science" => view(&output, &science::check_science(&document, CONTEXT)?)?,
        "scorer" => output.raw(
            &document,
            Some(Science::new(BigInt::from(7), &document, CONTEXT)?.scorer()?),
            false,
        ),
        "interface" => interface(
            &output,
            &document,
            &InterfaceSpec::from_registration(&document, document.root(), CONTEXT)?,
        ),
        "path" => {
            let Some(Node::String(text)) = document.node(document.root()) else {
                panic!("path fixture")
            };
            output.push(Node::Bool(science::path_segment(text)))
        }
        "dashboard" => {
            science::check_dashboard(
                &Science::new(BigInt::from(7), &document, CONTEXT)?,
                &secondary,
                CONTEXT,
            )?;
            output.push(Node::Null)
        }
        "hypothesis" => {
            science::check_hypothesis(
                &Science::new(BigInt::from(7), &document, CONTEXT)?,
                &secondary,
                CONTEXT,
            )?;
            output.push(Node::Null)
        }
        name => panic!("unknown operation {name}"),
    };
    Ok(output.finish(root))
}
fn assert_native_science_boundary(
    case: &Value,
    contracts: &cannery_core::contracts::ContractValidator,
) -> bool {
    let raw = case["document_json"].as_str().unwrap();
    let secondary = case["secondary_json"].as_str().unwrap();
    if serde_json::from_str::<Value>(raw).is_err()
        || serde_json::from_str::<Value>(secondary).is_err()
    {
        if serde_json::from_str::<Value>(raw).is_err() {
            assert!(json::decode_str(raw, 200).is_err());
        }
        if serde_json::from_str::<Value>(secondary).is_err() {
            assert!(json::decode_str(secondary, 200).is_err());
        }
        return true;
    }
    let document = json::decode_str(raw, 200).unwrap();
    if case["operation"] == "view"
        && case["error"].is_null()
        && !contracts.is_valid(
            cannery_core::contracts::ContractKind::ScienceRevision,
            &document,
        )
    {
        // These legacy constructor recipes are refused by the unchanged
        // published science contract, rather than emulating Python str.
        assert_ne!(
            contracts
                .violations(
                    cannery_core::contracts::ContractKind::ScienceRevision,
                    &document
                )
                .unwrap(),
            []
        );
        return true;
    }
    if case["operation"] == "interface" {
        if let Some(version) = document.field(document.root(), "version")
            && !matches!(document.node(version), Some(Node::Integer(_)))
        {
            assert!(
                !contracts.is_valid(cannery_core::contracts::ContractKind::Interface, &document)
            );
            return true;
        }
        if let Some(value) = document.field(document.root(), "max_bytes")
            && !matches!(document.node(value), Some(Node::Null))
        {
            let expected = match document.node(value) {
                Some(Node::Integer(value)) if value.to_string().parse::<i64>().is_err() => {
                    Some(ScienceError::Overflow)
                }
                Some(Node::Integer(_)) => None,
                _ => Some(ScienceError::Type),
            };
            if let Some(expected) = expected
                && !matches!(case["error"].as_str(), Some("KeyError" | "AttributeError"))
            {
                assert_eq!(observed(case).unwrap_err(), expected, "{}", case["name"]);
                return true;
            }
        }
    }
    false
}

#[test]
fn source_science_decisions_and_native_boundaries_are_asserted() {
    let reference: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/research/tests/fixtures/science_reference.json"
    ))
    .unwrap();
    assert_eq!(reference["runtime"], "3.13.11");
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 231);
    let contracts = cannery_core::contracts::ContractValidator::new().unwrap();
    for case in cases {
        if assert_native_science_boundary(case, &contracts) {
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
                if case["operation"] == "interface" {
                    let input: Value =
                        serde_json::from_str(case["document_json"].as_str().unwrap()).unwrap();
                    for key in ["magic", "validator"] {
                        if let Some(value) =
                            input.get(key).filter(|v| !v.is_null() && !v.is_string())
                        {
                            expected[key] = Value::String(value.to_string());
                        }
                    }
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
                    case["error"].as_str().unwrap(),
                    "error {}",
                    case["name"]
                );
                if let ScienceError::Validation { path, message } = error {
                    assert_eq!(
                        path.as_utf8().unwrap(),
                        case["path"].as_str().unwrap(),
                        "path {}",
                        case["name"]
                    );
                    assert_ne!(message, "");
                }
            }
        }
    }
}

#[test]
fn diagnostics_and_native_scalar_types_are_redacted_and_distinct() {
    let error = ScienceError::Validation {
        path: "private-synthetic-value".into(),
        message: "unknown reference",
    };
    assert_eq!(error.code(), Some("validation_failed"));
    assert!(!format!("{error:?} {error}").contains("private-synthetic-value"));
    for raw in ["NaN", "Infinity", "-Infinity"] {
        assert!(json::decode_str(raw, 64).is_err());
    }
    let document = json::decode_str("1", 64).unwrap();
    let number = Scalar::Number {
        value: num_rational::BigRational::from_integer(1.into()),
        original: document.root(),
    };
    assert_ne!(Scalar::Bool(true), number);
    assert_ne!(Scalar::Bool(false), Scalar::Null);
    assert_eq!(Scalar::Bool(true), Scalar::Bool(true));
    let registry = json::decode_str(
        r#"{"metrics":[{"key":"metric","splits":[true,1,1.0,false,0,0.0]}]}"#,
        64,
    )
    .unwrap();
    let science = Science::new(1.into(), &registry, CONTEXT).unwrap();
    assert_eq!(science.metrics[0].1.splits.len(), 4);
    assert_eq!(science.metrics[0].1.splits[0], Scalar::Bool(true));
    assert_eq!(science.metrics[0].1.splits[2], Scalar::Bool(false));
    assert!(
        !format!("{:?}", Scalar::Text("private-synthetic-value".into()))
            .contains("private-synthetic-value")
    );
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut builder = DocumentBuilder::new();
        let number = builder.push(Node::Float(value)).unwrap();
        let splits = builder.push(Node::Array(vec![number])).unwrap();
        let key = builder.push(Node::String("metric".into())).unwrap();
        let metric = builder
            .push(Node::Object(vec![
                ("key".into(), key),
                ("splits".into(), splits),
            ]))
            .unwrap();
        let metrics = builder.push(Node::Array(vec![metric])).unwrap();
        let root = builder
            .push(Node::Object(vec![("metrics".into(), metrics)]))
            .unwrap();
        let document = builder.finish(root).unwrap();
        assert_eq!(
            Science::new(1.into(), &document, CONTEXT).unwrap_err(),
            ScienceError::Value
        );
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;

#[test]
fn consumed_configuration_integers_are_checked_signed_i64_values() {
    for value in [i64::MIN, -1, 0, 1, i64::MAX] {
        let document = json::decode_str(&value.to_string(), 64).unwrap();
        assert_eq!(
            science::configuration_integer(&document, document.root()).unwrap(),
            BigInt::from(value)
        );
    }
    for raw in [
        "true",
        "false",
        "1.5",
        r#""1""#,
        r#""١٢""#,
        r#""1_000""#,
        "null",
        "[]",
        "{}",
    ] {
        let document = json::decode_str(raw, 64).unwrap();
        assert_eq!(
            science::configuration_integer(&document, document.root()),
            Err(ScienceError::Type),
            "{raw}"
        );
    }
    for raw in ["9223372036854775808", "-9223372036854775809"] {
        let document = json::decode_str(raw, 64).unwrap();
        assert_eq!(
            science::configuration_integer(&document, document.root()),
            Err(ScienceError::Overflow)
        );
    }
}

#[test]
fn consumed_limits_and_interface_sizes_use_the_same_integer_contract() {
    for raw in ["true", "1.5", r#""2""#, "9223372036854775808"] {
        let expected = if raw == "9223372036854775808" {
            ScienceError::Overflow
        } else {
            ScienceError::Type
        };
        let document = json::decode_str(
            &format!(r#"{{"max_auto_retries":{raw},"limits":{{"max_output_bytes":{raw}}}}}"#),
            64,
        )
        .unwrap();
        let science = Science::new(1.into(), &document, CONTEXT).unwrap();
        assert_eq!(science.max_auto_retries(), Err(expected.clone()));
        assert_eq!(science.max_output_bytes(), Err(expected.clone()));
        let document = json::decode_str(
            &format!(r#"{{"name":"output","version":1,"max_bytes":{raw}}}"#),
            64,
        )
        .unwrap();
        assert_eq!(
            InterfaceSpec::from_registration(&document, document.root(), CONTEXT).unwrap_err(),
            expected
        );
    }
    let document = json::decode_str(r#"{"limits":{"max_output_bytes":42},"interfaces":[{"name":"output","version":1,"max_bytes":42}]}"#, 64).unwrap();
    let science = Science::new(1.into(), &document, CONTEXT).unwrap();
    assert_eq!(science.max_auto_retries().unwrap(), 1.into());
    assert_eq!(science.max_output_bytes().unwrap(), 42.into());
    assert_eq!(science.interface_specs[0].1.max_bytes, Some(42.into()));
    assert_eq!(science.interface_specs[0].1.reference, "output/v1");
}

#[test]
fn step_and_setup_deadlines_reject_coercion_and_preserve_defaults() {
    use cannery_research::{job_deadlines, steps};
    let document =
        json::decode_str(r#"{"spec":{"activeDeadlineSeconds":20,"setup":{}}}"#, 64).unwrap();
    assert_eq!(steps::setup_deadline(&document).unwrap(), Some(600.into()));
    assert_eq!(job_deadlines::step_seconds(&document).unwrap(), 620.into());
    for field in ["activeDeadlineSeconds", "setup"] {
        for raw in ["true", "2.5", r#""20""#, "9223372036854775808"] {
            let expected = if raw == "9223372036854775808" {
                ScienceError::Overflow
            } else {
                ScienceError::Type
            };
            let spec = if field == "setup" {
                format!(
                    r#"{{"activeDeadlineSeconds":20,"setup":{{"activeDeadlineSeconds":{raw}}}}}"#
                )
            } else {
                format!(r#"{{"activeDeadlineSeconds":{raw}}}"#)
            };
            let document = json::decode_str(&format!(r#"{{"spec":{spec}}}"#), 64).unwrap();
            assert_eq!(job_deadlines::step_seconds(&document), Err(expected));
        }
    }
}
