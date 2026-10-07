//! Native document depth limits before worker startup.
use cannery_core::json::{Document, Node};
use std::collections::HashMap;

/// Installed `cannery` JSON file parsing, counting every array or object.
pub const JSON_CONTAINERS: usize = 128;

/// Policy callers share the same native validation depth limit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyEntryPoint {
    Evaluator,
    RunnerEvalKind,
}
impl PolicyEntryPoint {
    /// Maximum edges from document root accepted by native validation.
    #[must_use]
    pub const fn validation_edges(self) -> usize {
        JSON_CONTAINERS
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DepthError {
    #[error("document depth limit exceeded")]
    Recursion,
    #[error("invalid native document reference")]
    InvalidNode,
}

/// Check document depth before schema validation.
/// # Errors
/// Returns a depth limit or native arena invariant error.
pub fn check_validation_walk(document: &Document, edges: usize) -> Result<(), DepthError> {
    let mut pending = vec![(document.root(), 0usize)];
    let mut deepest = HashMap::new();
    while let Some((id, depth)) = pending.pop() {
        if depth > edges {
            return Err(DepthError::Recursion);
        }
        let node = document.node(id).ok_or(DepthError::InvalidNode)?;
        // Revisited shared subtrees cannot change the existence of a deeper
        // path unless reached at a greater depth. Never serialize these keys.
        let identity = std::ptr::from_ref(node) as usize;
        if deepest.get(&identity).is_some_and(|&seen| seen >= depth) {
            continue;
        }
        deepest.insert(identity, depth);
        let next = depth.checked_add(1).ok_or(DepthError::Recursion)?;
        match node {
            Node::Array(children) => {
                pending.extend(children.iter().rev().map(|&id| (id, next)));
            }
            Node::Object(children) => {
                pending.extend(children.iter().rev().map(|(_, id)| (*id, next)));
            }
            _ => {}
        }
    }
    Ok(())
}
