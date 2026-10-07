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
fn published_review_schema_validates_the_application_shape() {
    let contracts = ContractValidator::new().unwrap();
    let valid =
        json::from_value(json!({"draft_revision":1,"action":"approve","reason":"ready"})).unwrap();
    assert!(contracts.is_valid(ContractKind::DraftReview, &valid));
    let invalid =
        json::from_value(json!({"draft_revision":0,"action":"approve","reason":"ready"})).unwrap();
    let paths = contracts
        .violation_paths(ContractKind::DraftReview, &invalid)
        .unwrap();
    assert_eq!(paths, ["/draft_revision"]);
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
