//! Authored arena integrity and native JSON boundary regression cases.
#![allow(clippy::unwrap_used)]
use cannery_core::json::{self, BuildError, DocumentBuilder, EncodeError, Node};
use num_bigint::BigInt;
use serde_json::json;

#[test]
fn duplicate_utf8_keys_keep_first_position_and_last_value() {
    let mut builder = DocumentBuilder::new();
    let first = builder.push(Node::String("première".into())).unwrap();
    let replacement = builder.push(Node::String("😃".into())).unwrap();
    let root = builder
        .push(Node::Object(vec![
            ("clé".into(), first),
            ("𐀀".into(), first),
            ("clé".into(), replacement),
        ]))
        .unwrap();
    let document = builder.finish(root).unwrap();
    let Some(Node::Object(items)) = document.node(root) else {
        unreachable!()
    };
    assert_eq!(
        items
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        ["clé", "𐀀"]
    );
    assert_eq!(
        json::to_value(&document).unwrap(),
        json!({"clé":"😃", "𐀀":"première"})
    );
}

#[test]
fn import_roundtrip_preserves_nested_values_and_shared_children() {
    let mut shared = DocumentBuilder::new();
    let child = shared.push(Node::String("shared UTF-8 😃".into())).unwrap();
    let root = shared.push(Node::Array(vec![child, child])).unwrap();
    let shared = shared.finish(root).unwrap();
    let mut imported = DocumentBuilder::new();
    imported.push(Node::Null).unwrap();
    let root = imported.import(&shared, shared.root()).unwrap();
    let imported = imported.finish(root).unwrap();
    let Some(Node::Array(children)) = imported.node(root) else {
        unreachable!()
    };
    assert_eq!(children[0], children[1]);
    assert_eq!(imported.nodes().len(), 3);

    let source = json::from_value(json!({"nested":[null,true,42,1.25,"UTF-8 😃"]})).unwrap();
    let mut builder = DocumentBuilder::new();
    let unrelated = builder.push(Node::Bool(false)).unwrap();
    let imported = builder.import(&source, source.root()).unwrap();
    let root = builder.push(Node::Array(vec![imported, imported])).unwrap();
    let copy = builder.finish(root).unwrap();
    assert_eq!(
        json::to_value(&copy).unwrap(),
        json!([
            {"nested":[null,true,42,1.25,"UTF-8 😃"]},
            {"nested":[null,true,42,1.25,"UTF-8 😃"]}
        ])
    );
    assert!(matches!(copy.node(unrelated), Some(Node::Bool(false))));
    let Some(Node::Array(items)) = copy.node(root) else {
        unreachable!()
    };
    assert_eq!(items[0], items[1]);
    let serialized = serde_json::to_vec(&json::to_value(&copy).unwrap()).unwrap();
    let decoded = json::decode(&serialized, json::MAX_DEPTH).unwrap();
    assert_eq!(
        json::to_value(&decoded).unwrap(),
        json::to_value(&copy).unwrap()
    );
}

#[test]
fn internal_nodes_must_pass_native_json_boundary_checks() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut builder = DocumentBuilder::new();
        let root = builder.push(Node::Float(value)).unwrap();
        let document = builder.finish(root).unwrap();
        assert!(matches!(
            json::to_value(&document),
            Err(EncodeError::IntegerLimit)
        ));
    }
    let mut builder = DocumentBuilder::new();
    let root = builder
        .push(Node::Integer(BigInt::from(10_u8).pow(1024)))
        .unwrap();
    assert!(matches!(
        json::to_value(&builder.finish(root).unwrap()),
        Err(EncodeError::IntegerLimit)
    ));
    let mut builder = DocumentBuilder::new();
    let mut root = builder.push(Node::Null).unwrap();
    for _ in 0..=json::MAX_DEPTH {
        root = builder.push(Node::Array(vec![root])).unwrap();
    }
    assert!(matches!(
        json::to_value(&builder.finish(root).unwrap()),
        Err(EncodeError::Recursion)
    ));
}

#[test]
fn rejects_invalid_indices_without_appending_a_partial_container() {
    let mut other = DocumentBuilder::new();
    other.push(Node::Null).unwrap();
    let outside = other.push(Node::Null).unwrap();
    let source = other.finish(outside).unwrap();
    let mut builder = DocumentBuilder::new();
    assert_eq!(
        builder.push(Node::Array(vec![outside])),
        Err(BuildError::InvalidNode)
    );
    assert_eq!(
        builder.push(Node::Object(vec![("key".into(), outside)])),
        Err(BuildError::InvalidNode)
    );
    let local = builder.push(Node::Bool(true)).unwrap();
    let document = builder.finish(local).unwrap();
    assert_eq!(document.nodes().len(), 1);
    assert!(matches!(document.node(local), Some(Node::Bool(true))));
    assert!(matches!(
        DocumentBuilder::new().finish(outside),
        Err(BuildError::InvalidNode)
    ));
    assert_eq!(DocumentBuilder::new().import(&source, local), Ok(local));
    assert_eq!(
        DocumentBuilder::new().import(&document, outside),
        Err(BuildError::InvalidNode)
    );
}
