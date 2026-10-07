//! Standard serde JSON encoding for dynamic fields.
use super::{Document, Node, NodeId};
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ModelEncodeError {
    #[error("model JSON serialization nesting context exhausted")]
    Recursion,
    #[error("model JSON string cannot be represented as UTF-8")]
    Encoding,
    #[error("model JSON node is absent or incompatible")]
    InvalidNode,
}

#[must_use]
pub fn model_float_text(value: f64) -> String {
    serde_json::Number::from_f64(value).map_or_else(|| "null".into(), |number| number.to_string())
}
/// # Errors
/// Rejects invalid documents or excessive nesting.
pub fn encode_inferred(
    document: &Document,
    root: NodeId,
    budget: usize,
) -> Result<Vec<u8>, ModelEncodeError> {
    let value = super::node_value(document, root, budget).map_err(|error| match error {
        super::EncodeError::Recursion => ModelEncodeError::Recursion,
        _ => ModelEncodeError::InvalidNode,
    })?;
    serde_json::to_vec(&value).map_err(|_| ModelEncodeError::InvalidNode)
}
/// # Errors
/// Rejects a non-object document or invalid nested values.
pub fn encode_model_mapping(
    document: &Document,
    root: NodeId,
    budget: usize,
) -> Result<Vec<u8>, ModelEncodeError> {
    if !matches!(document.node(root), Some(Node::Object(_))) {
        return Err(ModelEncodeError::InvalidNode);
    }
    encode_inferred(document, root, budget)
}
