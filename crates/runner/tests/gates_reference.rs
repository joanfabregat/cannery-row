#![forbid(unsafe_code)]
// Fail-fast assertions are limited to frozen test fixtures and setup.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::json::{self, Document, DocumentBuilder, Node};
use cannery_runner::gates::{self, Control, GateContext};
use serde_json::Value;
const BUDGET: usize = 128;
fn field(doc: &Document, name: &str) -> Document {
    let id = doc.field(doc.root(), name).unwrap();
    let mut builder = DocumentBuilder::new();
    let root = builder.import(doc, id).unwrap();
    builder.finish(root).unwrap()
}
fn unhex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|part| u8::from_str_radix(std::str::from_utf8(part).unwrap(), 16).unwrap())
        .collect()
}
#[test]
fn native_unicode_boundary_rejects_lone_surrogates() {
    let reference: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/runner/tests/fixtures/gates_reference.json"
    ))
    .unwrap();
    let cases = reference["codepoint_cases"].as_array().unwrap();
    assert_eq!(cases.len(), 4);
    for case in cases {
        let raw = unhex(case["raw_json_hex"].as_str().unwrap());
        assert!(serde_json::from_slice::<Value>(&raw).is_err());
        assert!(json::decode(&raw, BUDGET).is_err(), "{}", case["name"]);
    }
}
fn expected_text(doc: &Document) -> String {
    json::encode_ascii_pretty(doc, BUDGET).unwrap()
}
fn semantic_decision(doc: &Document) -> Value {
    fn remove_message(value: &mut Value, key: &str) {
        if let Some(message) = value.as_object_mut().unwrap().remove(key) {
            assert!(message.as_str().is_some_and(|text| !text.is_empty()));
        }
    }
    let mut value: Value = serde_json::from_str(&expected_text(doc)).unwrap();
    // Human descriptions may use native float spelling; decisions and the full
    // evidence remain exact structural comparisons, including dimension keys.
    remove_message(&mut value, "reason");
    if let Some(gate) = value.get_mut("gate") {
        remove_message(gate, "detail");
    }
    if let Some(gates) = value.get_mut("gates") {
        for gate in gates.as_array_mut().unwrap() {
            remove_message(gate, "detail");
        }
    }
    value
}
fn control(doc: &Document) -> Option<Control> {
    match doc.node(doc.root()).unwrap() {
        Node::Null => None,
        Node::Array(items) => {
            let text = |id| match doc.node(id).unwrap() {
                Node::String(text) => text.clone(),
                _ => panic!("fixture control must be text"),
            };
            Some(Control {
                id: text(items[0]),
                revision: text(items[1]),
            })
        }
        _ => panic!("fixture control shape"),
    }
}
#[test]
fn supported_decisions_and_comparisons_match_and_invalid_json_is_rejected() {
    let reference: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/runner/tests/fixtures/gates_reference.json"
    ))
    .unwrap();
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 122);
    let mut original = 0;
    let mut numeric = 0;
    let mut raw = 0;
    let mut gate_count = 0;
    let mut assessment_count = 0;
    for case in cases {
        match case["classification"].as_str().unwrap() {
            "original-source-intent" => original += 1,
            "numeric-helper" => numeric += 1,
            "raw-helper-not-schema-acceptance" => raw += 1,
            _ => panic!("unasserted source class"),
        }
        // R0's portable markers encode actual source nonfinite floats; restore only
        // those frozen marker recipes before lossless production parsing.
        let input = case["json"]
            .as_str()
            .unwrap()
            .replace(r#"{"__non_finite__": "nan"}"#, "NaN")
            .replace(r#"{"__non_finite__": "inf"}"#, "Infinity")
            .replace(r#"{"__non_finite__": "-inf"}"#, "-Infinity");
        let doc = match json::decode(input.as_bytes(), BUDGET) {
            Ok(doc) => doc,
            Err(error) => {
                assert!(
                    serde_json::from_str::<Value>(&input).is_err()
                        || input
                            .as_bytes()
                            .split(|byte| !byte.is_ascii_digit())
                            .any(|literal| literal.len() > 1024),
                    "native JSON rejection: {error:?}"
                );
                continue;
            }
        };
        let metrics = field(&doc, "metrics");
        let policy = field(&doc, "policy");
        let measurements = field(&doc, "measurements");
        let control_doc = field(&doc, "control");
        let control = control(&control_doc);
        let context = GateContext {
            metrics: &metrics,
            policy: &policy,
            measurements: &measurements,
            control: control.as_ref(),
            nesting_budget: BUDGET,
        };
        let mode = doc.field(doc.root(), "mode").unwrap();
        let actual = if matches!(doc.node(mode),Some(Node::String(text)) if text.equals_utf8("gate"))
        {
            gate_count += 1;
            gates::evaluate_gate(&context, &field(&doc, "gate"))
                .unwrap()
                .document(BUDGET)
                .unwrap()
        } else {
            assessment_count += 1;
            gates::assess(&context).unwrap().document(BUDGET).unwrap()
        };
        let name = doc.field(doc.root(), "name").unwrap();
        assert_eq!(
            semantic_decision(&actual),
            semantic_decision(&field(&doc, "expected")),
            "source recipe {:?}",
            doc.node(name)
        );
    }
    assert_eq!((original, numeric, raw), (82, 28, 12));
    assert!(gate_count > 0 && assessment_count > 0);
    assert!(gate_count + assessment_count >= 82);
}
#[test]
fn errors_are_sanitized_and_empty_policy_is_inconclusive() {
    let measurements = json::decode(b"[]", BUDGET).unwrap();
    let metrics = json::decode(b"[]", BUDGET).unwrap();
    let policy = json::decode(
        br#"{"revision":"r","gates":[],"baselines":[],"default_control":null}"#,
        BUDGET,
    )
    .unwrap();
    let context = GateContext {
        metrics: &metrics,
        policy: &policy,
        measurements: &measurements,
        control: None,
        nesting_budget: BUDGET,
    };
    let assessment = gates::assess(&context).unwrap();
    assert_eq!(assessment.verdict, gates::Verdict::Inconclusive);
    assert_eq!(
        assessment.reason.as_utf8().unwrap(),
        "Inconclusive: no gate fails, but 0 of 0 gates are unknown (); unknown never passes."
    );
    let invalid = json::decode(br#"{"private-secret":"do-not-print"}"#, BUDGET).unwrap();
    let error = gates::evaluate_gate(&context, &invalid).err().unwrap();
    assert!(std::error::Error::source(&error).is_none());
    assert!(!format!("{error:?} {error}").contains("do-not-print"));
}

#[test]
fn native_gate_detail_keeps_the_metric_operator_and_reference_source() {
    let metrics = json::decode(
        br#"[{"key":"score","direction":"higher","splits":["test"],"dimensions":[]}]"#,
        BUDGET,
    )
    .unwrap();
    let policy = json::decode(
        br#"{"revision":"r","baselines":[],"default_control":null,"gates":[]}"#,
        BUDGET,
    )
    .unwrap();
    let measurements = json::decode(br#"[{"metric":"score","split":"test","value":2,"control_value":1,"authority":"tester_verified"}]"#, BUDGET).unwrap();
    let gate = json::decode("{\"id\":\"阈值\",\"metric\":\"score\",\"split\":\"test\",\"statistic\":\"value\",\"compare\":\"control\",\"op\":\">=\",\"min_delta\":0}".as_bytes(), BUDGET).unwrap();
    let context = GateContext {
        metrics: &metrics,
        policy: &policy,
        measurements: &measurements,
        control: None,
        nesting_budget: BUDGET,
    };
    let output = gates::evaluate_gate(&context, &gate)
        .unwrap()
        .document(BUDGET)
        .unwrap();
    let value: Value = serde_json::from_str(&expected_text(&output)).unwrap();
    assert_eq!(value["gate"]["id"], "阈值");
    assert_eq!(value["gate"]["result"], "pass");
    let detail = value["gate"]["detail"].as_str().unwrap();
    for required in [
        "score on test",
        "control 1",
        ">= 0",
        "control_source: tester_reported",
    ] {
        assert!(detail.contains(required), "{detail}");
    }
    assert_eq!(value["comparisons"][0]["value"], 2);
    assert_eq!(
        value["comparisons"][0]["reference"]["value"].as_f64(),
        Some(1.0)
    );
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
