//! Checked arena construction without a text serialization boundary.
use super::{Document, Node, NodeId};
use std::collections::HashMap;

/// Invalid local node index; diagnostics contain no document values.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BuildError {
    #[error("invalid JSON arena node")]
    InvalidNode,
}

/// Build UTF-8 JSON values and exact integers.
/// Child IDs must refer to nodes already added to this builder. As with document
/// accessors, indices from another arena must not be used as local IDs.
/// Construction has no wire-decoder digit or nesting limit. Callers apply their
/// entry point's serialization policy separately when writing bytes.
#[derive(Debug, Default)]
pub struct DocumentBuilder {
    nodes: Vec<Node>,
}

impl DocumentBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a scalar or a container referencing earlier local nodes.
    /// Object duplicates preserve first insertion position and last value.
    /// # Errors
    /// Refuses invalid child indices before changing the arena.
    pub fn push(&mut self, mut node: Node) -> Result<NodeId, BuildError> {
        let valid = match &node {
            Node::Array(items) => items.iter().all(|id| id.0 < self.nodes.len()),
            Node::Object(items) => items.iter().all(|(_, id)| id.0 < self.nodes.len()),
            _ => true,
        };
        if !valid {
            return Err(BuildError::InvalidNode);
        }
        if let Node::Object(items) = &mut node {
            let mut positions: HashMap<String, usize> = HashMap::new();
            let mut unique: Vec<(String, NodeId)> = Vec::new();
            for (key, id) in std::mem::take(items) {
                if let Some(&position) = positions.get(&key) {
                    unique[position].1 = id;
                } else {
                    positions.insert(key.clone(), unique.len());
                    unique.push((key, id));
                }
            }
            *items = unique;
        }
        let id = NodeId(self.nodes.len());
        self.nodes.push(node);
        Ok(id)
    }

    /// Copy a reachable subtree with an explicit stack, retaining text and scalars.
    /// Repeated references share their copied node; no JSON reparse occurs.
    /// # Errors
    /// Refuses an invalid source node index.
    pub fn import(&mut self, source: &Document, root: NodeId) -> Result<NodeId, BuildError> {
        let mut copied = HashMap::new();
        let mut pending = vec![(root, false)];
        while let Some((id, finish)) = pending.pop() {
            if copied.contains_key(&id.0) {
                continue;
            }
            let node = source.node(id).ok_or(BuildError::InvalidNode)?;
            if !finish {
                pending.push((id, true));
                match node {
                    Node::Array(items) => pending.extend(items.iter().rev().map(|&id| (id, false))),
                    Node::Object(items) => {
                        pending.extend(items.iter().rev().map(|(_, id)| (*id, false)));
                    }
                    _ => {}
                }
                continue;
            }
            let child = |id: &NodeId| copied.get(&id.0).copied().ok_or(BuildError::InvalidNode);
            let node = match node {
                Node::Null => Node::Null,
                Node::Bool(value) => Node::Bool(*value),
                Node::Integer(value) => Node::Integer(value.clone()),
                Node::Float(value) => Node::Float(*value),
                Node::String(value) => Node::String(value.clone()),
                Node::Array(items) => {
                    Node::Array(items.iter().map(child).collect::<Result<_, _>>()?)
                }
                Node::Object(items) => Node::Object(
                    items
                        .iter()
                        .map(|(key, id)| Ok((key.clone(), child(id)?)))
                        .collect::<Result<_, BuildError>>()?,
                ),
            };
            copied.insert(id.0, self.push(node)?);
        }
        copied.get(&root.0).copied().ok_or(BuildError::InvalidNode)
    }

    /// Finish with a valid local root; unused nodes do not change its value.
    /// # Errors
    /// Refuses a root outside this arena.
    pub fn finish(self, root: NodeId) -> Result<Document, BuildError> {
        if root.0 >= self.nodes.len() {
            return Err(BuildError::InvalidNode);
        }
        Ok(Document {
            nodes: self.nodes,
            root,
        })
    }
}
