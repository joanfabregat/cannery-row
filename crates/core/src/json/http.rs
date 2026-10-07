//! Standard JSON HTTP encoding.
use super::Document;
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum HttpEncodeError {
    #[error("JSON integer exceeds the application limit")]
    IntegerLimit,
    #[error("JSON rendering nesting budget exhausted")]
    Recursion,
    #[error("JSON response contains a nonfinite number")]
    NonFinite,
    #[error("JSON node is absent")]
    InvalidNode,
    #[error("JSON response UTF-8 encoding failed at characters {start} through {end}")]
    Encoding { start: usize, end: usize },
}

/// # Errors
/// Rejects invalid documents, nonfinite numbers and excessive depth.
pub fn encode_http(document: &Document, budget: usize) -> Result<Vec<u8>, HttpEncodeError> {
    let value =
        super::node_value(document, document.root(), budget).map_err(|error| match error {
            super::EncodeError::Recursion => HttpEncodeError::Recursion,
            super::EncodeError::IntegerLimit => HttpEncodeError::NonFinite,
            super::EncodeError::InvalidNode => HttpEncodeError::InvalidNode,
        })?;
    serde_json::to_vec(&value).map_err(|_| HttpEncodeError::InvalidNode)
}
