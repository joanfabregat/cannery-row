//! Authored regression cases for native JSON and published application contracts.
#![allow(clippy::unwrap_used)]
use cannery_core::{
    contracts::{ContractKind, ContractValidator},
    json,
};
use serde_json::json;

#[test]
fn utf8_json_rejects_surrogates_and_nonfinite_literals() {
    for source in [r#""\ud800""#, "NaN", "Infinity", "-Infinity"] {
        assert!(json::decode(source.as_bytes(), 128).is_err());
    }
    let document = json::decode(br#"{"text":"\ud83d\ude03"}"#, 128).unwrap();
    assert_eq!(json::to_value(&document).unwrap(), json!({"text":"😃"}));
}

#[test]
fn published_unit_schema_validates_the_plan_entry_shape() {
    let contracts = ContractValidator::new().unwrap();
    let mut unit = json!({
        "key": "lower-rate",
        "title": "Lower the learning rate",
        "question": "Does a lower rate help?",
        "intervention": "Halve it.",
        "acceptance": {
            "selection_splits": ["dev"],
            "confirmation_splits": ["test"],
            "primary_metric": "accuracy",
            "required_slices": [],
            "success_criteria": "Accuracy rises.",
            "falsification_criteria": "Accuracy does not rise.",
            "regression_gates": [],
            "compute_budget": {"gpu_hours_max": 1}
        },
        "relations": [{"kind": "derived_from", "unit": "baseline"}]
    });
    let valid = json::from_value(unit.clone()).unwrap();
    assert!(contracts.is_valid(ContractKind::Unit, &valid));
    unit["question"] = json!(" ");
    let invalid = json::from_value(unit).unwrap();
    let paths = contracts
        .violation_paths(ContractKind::Unit, &invalid)
        .unwrap();
    assert_eq!(paths, ["/question"]);
}

#[test]
fn application_depth_bound_is_enforced() {
    assert!(json::decode(br"[[[1]]]", 2).is_err());
    assert!(json::decode(br"[[1]]", 2).is_ok());
}

#[test]
fn attribution_text_refuses_directional_controls_and_accepts_unicode_names() {
    for character in [
        '\u{061c}', '\u{200e}', '\u{200f}', '\u{202e}', '\u{2066}', '\u{2069}', '\n',
    ] {
        assert!(!cannery_core::text::printable(u32::from(character)));
    }
    assert!(
        "Joan 日本語 مرحبا"
            .chars()
            .all(|character| cannery_core::text::printable(u32::from(character)))
    );
}
