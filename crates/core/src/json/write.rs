//! Standard serde JSON serialization.
use super::{Document, NodeId};
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum EncodeError {
    #[error("JSON number is out of range")]
    IntegerLimit,
    #[error("JSON nesting limit exceeded")]
    Recursion,
    #[error("invalid JSON document node")]
    InvalidNode,
}
#[must_use]
pub fn float_text(value: f64) -> String {
    value.to_string()
}
/// # Errors
/// Rejects invalid documents.
pub fn encode_ascii_pretty(document: &Document, budget: usize) -> Result<String, EncodeError> {
    encode_ascii_pretty_node(document, document.root(), budget)
}
/// # Errors
/// Rejects invalid documents.
pub fn encode_ascii_pretty_node(
    document: &Document,
    root: NodeId,
    budget: usize,
) -> Result<String, EncodeError> {
    serde_json::to_string_pretty(&super::node_value(document, root, budget)?)
        .map_err(|_| EncodeError::InvalidNode)
}
/// # Errors
/// Rejects invalid documents.
pub fn encode_ascii_default(document: &Document, budget: usize) -> Result<String, EncodeError> {
    serde_json::to_string(&super::node_value(document, document.root(), budget)?)
        .map_err(|_| EncodeError::InvalidNode)
}
