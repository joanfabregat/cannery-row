//! Helpers for Rust UTF-8 text and dynamic JSON projections.
use crate::json::{Document, Node, NodeId};
/// UTF-8 strings cannot contain lone surrogate code points.
#[must_use]
pub fn from_codepoints(points: Vec<u32>) -> Option<String> {
    points.into_iter().map(char::from_u32).collect()
}
/// Temporary view helpers for scalar-oriented domain algorithms.
pub trait TextExt {
    fn codepoints(&self) -> Vec<u32>;
    fn as_utf8(&self) -> Option<String>;
    fn equals_utf8(&self, value: &str) -> bool;
    fn lowercase(&self) -> String;
}
impl TextExt for str {
    fn codepoints(&self) -> Vec<u32> {
        self.chars().map(u32::from).collect()
    }
    fn as_utf8(&self) -> Option<String> {
        Some(self.to_owned())
    }
    fn equals_utf8(&self, value: &str) -> bool {
        self == value
    }
    fn lowercase(&self) -> String {
        self.to_lowercase()
    }
}
impl TextExt for String {
    fn codepoints(&self) -> Vec<u32> {
        self.as_str().codepoints()
    }
    fn as_utf8(&self) -> Option<String> {
        Some(self.clone())
    }
    fn equals_utf8(&self, value: &str) -> bool {
        self == value
    }
    fn lowercase(&self) -> String {
        self.to_lowercase()
    }
}
#[must_use]
pub fn decimal_digit(point: u32) -> Option<u32> {
    char::from_u32(point)?.to_digit(10)
}
#[must_use]
pub fn whitespace(point: u32) -> bool {
    char::from_u32(point).is_some_and(char::is_whitespace)
}
#[must_use]
pub fn printable(point: u32) -> bool {
    char::from_u32(point).is_some_and(|c| {
        !c.is_control()
            && !matches!(point, 0x061c | 0x200e..=0x200f | 0x202a..=0x202e | 0x2066..=0x2069)
    })
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RenderError {
    #[error("invalid JSON node")]
    InvalidNode,
    #[error("invalid JSON number")]
    IntegerLimit,
    #[error("JSON nesting limit exceeded")]
    Recursion,
    #[error("JSON encoding failed")]
    Encoding,
}
/// # Errors
/// Rejects an encoding failure.
pub fn repr_string(text: &str) -> Result<String, RenderError> {
    serde_json::to_string(text).map_err(|_| RenderError::Encoding)
}
/// # Errors
/// Rejects an invalid dynamic JSON value.
pub fn str_value(document: &Document, id: NodeId, budget: usize) -> Result<String, RenderError> {
    if let Some(Node::String(value)) = document.node(id) {
        return Ok(value.clone());
    }
    let value = crate::json::node_value(document, id, budget).map_err(|error| match error {
        crate::json::EncodeError::Recursion => RenderError::Recursion,
        crate::json::EncodeError::IntegerLimit => RenderError::IntegerLimit,
        crate::json::EncodeError::InvalidNode => RenderError::InvalidNode,
    })?;
    serde_json::to_string(&value).map_err(|_| RenderError::Encoding)
}
