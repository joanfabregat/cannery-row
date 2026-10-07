//! Bounded JSON documents using Rust UTF-8 strings and serde JSON syntax.
mod build;
pub mod canonical;
mod http;
pub mod model;
mod write;
pub use build::{BuildError, DocumentBuilder};
pub use http::{HttpEncodeError, encode_http};
use num_bigint::BigInt;
use serde_json::Value;
pub use write::{
    EncodeError, encode_ascii_default, encode_ascii_pretty, encode_ascii_pretty_node, float_text,
};
/// Maximum nesting accepted by native application JSON conversions.
pub const MAX_DEPTH: usize = 128;
/// Maximum numeric literal length before arbitrary-precision conversion.
pub const MAX_NUMBER_LENGTH: usize = 1024;
/// A JSON node's index in its owning document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeId(usize);

#[derive(Debug)]
pub enum Node {
    Null,
    Bool(bool),
    Integer(BigInt),
    Float(f64),
    String(String),
    Array(Vec<NodeId>),
    /// First key insertion order, with the last value for each duplicate key.
    Object(Vec<(String, NodeId)>),
}

/// Nodes contain indices rather than recursive values; traversal and drop need no recursion.
#[derive(Debug)]
pub struct Document {
    nodes: Vec<Node>,
    root: NodeId,
}
impl Document {
    #[must_use]
    pub fn root(&self) -> NodeId {
        self.root
    }
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.0)
    }
    #[must_use]
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }
    #[must_use]
    pub fn field(&self, object: NodeId, key: &str) -> Option<NodeId> {
        match self.node(object)? {
            Node::Object(entries) => entries
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, id)| *id),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DecodeError {
    #[error("invalid JSON at character {position}")]
    Syntax { position: usize },
    #[error("invalid JSON text encoding")]
    Encoding,
    #[error("JSON number is out of range")]
    IntegerLimit,
    #[error("JSON nesting limit exceeded")]
    Recursion,
}
/// Decode standard UTF-8 JSON. Lone surrogates and nonfinite literals are rejected.
/// # Errors
/// Returns a sanitized encoding, syntax or nesting error.
pub fn decode(bytes: &[u8], nesting_budget: usize) -> Result<Document, DecodeError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|error| DecodeError::Syntax {
        position: error.column(),
    })?;
    from_value_bounded(value, nesting_budget)
}
/// # Errors
/// Returns a sanitized JSON parse failure.
pub fn decode_str(text: &str, nesting_budget: usize) -> Result<Document, DecodeError> {
    decode(text.as_bytes(), nesting_budget)
}
/// # Errors
/// Returns a sanitized JSON parse failure.
pub fn decode_text(text: &str, nesting_budget: usize) -> Result<Document, DecodeError> {
    decode_str(text, nesting_budget)
}
/// # Errors
/// Rejects JSON beyond the application nesting limit.
pub fn from_value(value: Value) -> Result<Document, DecodeError> {
    from_value_bounded(value, MAX_DEPTH)
}
fn from_value_bounded(value: Value, budget: usize) -> Result<Document, DecodeError> {
    fn append(
        value: Value,
        nodes: &mut Vec<Node>,
        depth: usize,
        budget: usize,
    ) -> Result<NodeId, DecodeError> {
        if depth > budget {
            return Err(DecodeError::Recursion);
        }
        let node = match value {
            Value::Null => Node::Null,
            Value::Bool(value) => Node::Bool(value),
            Value::String(value) => Node::String(value),
            Value::Number(value) => {
                let spelling = value.to_string();
                if spelling.len() > MAX_NUMBER_LENGTH {
                    return Err(DecodeError::IntegerLimit);
                }
                if spelling.contains(['.', 'e', 'E']) {
                    Node::Float(
                        value
                            .as_f64()
                            .filter(|number| number.is_finite())
                            .ok_or(DecodeError::IntegerLimit)?,
                    )
                } else {
                    Node::Integer(
                        spelling
                            .parse::<BigInt>()
                            .map_err(|_| DecodeError::IntegerLimit)?,
                    )
                }
            }
            Value::Array(items) => Node::Array(
                items
                    .into_iter()
                    .map(|value| append(value, nodes, depth + 1, budget))
                    .collect::<Result<_, _>>()?,
            ),
            Value::Object(items) => Node::Object(
                items
                    .into_iter()
                    .map(|(key, value)| Ok((key, append(value, nodes, depth + 1, budget)?)))
                    .collect::<Result<_, DecodeError>>()?,
            ),
        };
        let id = NodeId(nodes.len());
        nodes.push(node);
        Ok(id)
    }
    let budget = budget.min(MAX_DEPTH);
    let mut nodes = Vec::new();
    let root = append(value, &mut nodes, 0, budget)?;
    Ok(Document { nodes, root })
}
/// Convert a document to standard serde JSON, checking depth and numeric validity.
/// # Errors
/// Rejects missing nodes, nonfinite numbers and excessive depth.
pub fn to_value(document: &Document) -> Result<Value, EncodeError> {
    node_value(document, document.root(), MAX_DEPTH)
}
/// # Errors
/// Rejects missing nodes, nonfinite numbers and excessive depth.
pub fn node_value(document: &Document, root: NodeId, budget: usize) -> Result<Value, EncodeError> {
    fn convert(
        document: &Document,
        id: NodeId,
        depth: usize,
        budget: usize,
    ) -> Result<Value, EncodeError> {
        if depth > budget {
            return Err(EncodeError::Recursion);
        }
        Ok(match document.node(id).ok_or(EncodeError::InvalidNode)? {
            Node::Null => Value::Null,
            Node::Bool(value) => Value::Bool(*value),
            Node::String(value) => Value::String(value.clone()),
            Node::Integer(value) => {
                let spelling = value.to_string();
                if spelling.len() > MAX_NUMBER_LENGTH {
                    return Err(EncodeError::IntegerLimit);
                }
                Value::Number(spelling.parse().map_err(|_| EncodeError::IntegerLimit)?)
            }
            Node::Float(value) => Value::Number(
                serde_json::Number::from_f64(*value).ok_or(EncodeError::IntegerLimit)?,
            ),
            Node::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|id| convert(document, *id, depth + 1, budget))
                    .collect::<Result<_, _>>()?,
            ),
            Node::Object(items) => Value::Object(
                items
                    .iter()
                    .map(|(key, id)| Ok((key.clone(), convert(document, *id, depth + 1, budget)?)))
                    .collect::<Result<_, EncodeError>>()?,
            ),
        })
    }
    convert(document, root, 0, budget.min(MAX_DEPTH))
}
