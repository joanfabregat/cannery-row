//! Authored application policy and resource-bound regression cases.
#![allow(clippy::unwrap_used)]
use cannery_core::{
    contracts::{instance::ProjectValidator, project_schema_violations},
    json,
};
use num_bigint::BigInt;
use serde_json::{Value, json};
use std::sync::Arc;
fn schema(value: Value) -> Arc<json::Document> {
    Arc::new(json::from_value(value).unwrap())
}
fn paths(value: Value) -> Vec<String> {
    project_schema_violations(&schema(value))
        .unwrap()
        .into_iter()
        .map(|error| error.path)
        .collect()
}
#[test]
fn project_policy_rejects_external_references_dialects_and_unknown_formats() {
    assert_eq!(paths(json!(true)), [""]);
    assert_eq!(
        paths(json!({"$schema":"http://json-schema.org/draft-07/schema#"})),
        ["/$schema"]
    );
    assert_eq!(
        paths(json!({"properties":{"a/b~c":{"$dynamicRef":"https://example.invalid/schema"}}})),
        ["/properties/a~1b~0c/$dynamicRef"]
    );
    assert_eq!(
        paths(json!({"$defs":{"x":{"format":"email"}}})),
        ["/$defs/x/format"]
    );
    assert_ne!(
        paths(json!({"$ref":"#/$defs/missing"})),
        [] as [std::string::String; 0]
    );
    assert!(ProjectValidator::new(&schema(json!({"$ref":"file:///etc/passwd"}))).is_err());
}
#[test]
fn policy_checks_schema_positions_without_interpreting_instance_data() {
    assert_eq!(
        paths(
            json!({"$defs":{"x":{"type":"string"}},"$ref":"#/$defs/x","const":{"$ref":"https://example.invalid","format":"email"},"default":{"$schema":"unsupported"},"examples":[{"$dynamicRef":"https://example.invalid"}]})
        ),
        [] as [std::string::String; 0]
    );
    for format in ["date", "date-time", "uri", "uuid"] {
        assert_eq!(
            paths(json!({"format":format})),
            [] as [std::string::String; 0]
        );
    }
    assert_eq!(
        paths(
            json!({"if":{"properties":{"x":{"format":"email"}}},"then":{"items":{"$ref":"https://example.invalid"}}})
        ),
        ["/if/properties/x/format", "/then/items/$ref"]
    );
}
#[test]
fn instance_paths_choose_deepest_combination_child_and_hide_values() {
    let document = json::from_value(json!({"secret":["PRIVATE_VALUE"]})).unwrap();
    for keyword in ["anyOf", "oneOf"] {
        let validator = ProjectValidator::new(&schema(json!({keyword:[{"type":"null"},{"type":"object","properties":{"secret":{"type":"array","items":{"type":"integer"}}}}]}))).unwrap();
        let errors = validator.violations(&document, &BigInt::from(10)).unwrap();
        assert_eq!(errors[0].path, "/secret/0");
        assert!(!format!("{errors:?}").contains("PRIVATE_VALUE"));
    }
}
#[test]
fn duplicate_paths_do_not_exhaust_instance_limit_and_zero_is_empty() {
    let validator = ProjectValidator::new(&schema(
        json!({"type":"object","required":["a","b"],"properties":{"x":{"type":"integer"}}}),
    ))
    .unwrap();
    let document = json::from_value(json!({"x":"private"})).unwrap();
    let errors = validator.violations(&document, &BigInt::from(2)).unwrap();
    assert_eq!(
        errors
            .iter()
            .map(|error| error.path.as_str())
            .collect::<Vec<_>>(),
        ["", "/x"]
    );
    assert_eq!(
        validator.violations(&document, &BigInt::from(0)).unwrap(),
        [] as [cannery_core::contracts::instance::ProjectViolation; 0]
    );
}
#[test]
fn project_and_published_date_time_policy_is_shared() {
    let validator = ProjectValidator::new(&schema(json!({"format":"date-time"}))).unwrap();
    for (timestamp, valid) in [
        ("2024-02-29T01:02:03+01:75", true),
        ("2023-02-29T01:02:03Z", false),
    ] {
        let document = json::from_value(json!(timestamp)).unwrap();
        assert_eq!(
            validator
                .violations(&document, &BigInt::from(10))
                .unwrap()
                .is_empty(),
            valid
        );
    }
}
#[test]
fn native_numeric_literals_and_arena_conversions_are_bounded() {
    assert!(json::decode_str(&"9".repeat(1024), 128).is_ok());
    assert_eq!(
        json::decode_str(&"9".repeat(1025), 128).unwrap_err(),
        json::DecodeError::IntegerLimit
    );
    let number: serde_json::Number = "9".repeat(1025).parse().unwrap();
    assert_eq!(
        json::from_value(Value::Number(number)).unwrap_err(),
        json::DecodeError::IntegerLimit
    );
    let mut builder = json::DocumentBuilder::new();
    let root = builder
        .push(json::Node::Integer(BigInt::from(10).pow(1024)))
        .unwrap();
    assert!(matches!(
        json::to_value(&builder.finish(root).unwrap()),
        Err(json::EncodeError::IntegerLimit)
    ));
    let mut builder = json::DocumentBuilder::new();
    let mut root = builder.push(json::Node::Null).unwrap();
    for _ in 0..129 {
        root = builder.push(json::Node::Array(vec![root])).unwrap();
    }
    let document = builder.finish(root).unwrap();
    assert!(matches!(
        json::node_value(&document, document.root(), usize::MAX),
        Err(json::EncodeError::Recursion)
    ));
    let mut value = Value::Null;
    for _ in 0..129 {
        value = Value::Array(vec![value]);
    }
    assert_eq!(
        json::from_value(value).unwrap_err(),
        json::DecodeError::Recursion
    );
}
