//! Canonical hashes are a persisted evidence protocol, independent of HTTP JSON.
#![allow(clippy::unwrap_used)]
use cannery_core::json::{self, DocumentBuilder, Node};
#[test]
fn preserves_version_one_numeric_spelling_and_key_order() {
    let mut builder = DocumentBuilder::new();
    let values = [1e-7, 1e20, 1.0, -0.0];
    let children: Vec<_> = values
        .into_iter()
        .map(|v| builder.push(Node::Float(v)).unwrap())
        .collect();
    let array = builder.push(Node::Array(children)).unwrap();
    let text = builder.push(Node::String("😃".into())).unwrap();
    let root = builder
        .push(Node::Object(vec![("é".into(), text), ("a".into(), array)]))
        .unwrap();
    let document = builder.finish(root).unwrap();
    assert_eq!(
        json::canonical::bytes(&document, 128).unwrap(),
        "{\"a\":[1e-07,1e+20,1.0,-0.0],\"é\":\"😃\"}".as_bytes()
    );
}
#[test]
fn model_numbers_always_produce_valid_json() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1.0, -0.0] {
        let text = json::model::model_float_text(value);
        assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok());
        if !value.is_finite() {
            assert_eq!(text, "null");
        }
    }
}
