//! Domain scalar ordering with Rust strings and exact integer/float comparisons.
use crate::json::{Document, Node, NodeId};
use num_bigint::BigInt;
use num_traits::FromPrimitive;
use std::cmp::Ordering;
#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("values cannot be compared")]
pub struct ComparisonError;
fn numeric(left: &Node, right: &Node) -> Result<Ordering, ComparisonError> {
    match (left, right) {
        (Node::Integer(a), Node::Integer(b)) => Ok(a.cmp(b)),
        (Node::Float(a), Node::Float(b)) => a.partial_cmp(b).ok_or(ComparisonError),
        (Node::Integer(a), Node::Float(b)) => integer_float(a, *b),
        (Node::Float(a), Node::Integer(b)) => integer_float(b, *a).map(Ordering::reverse),
        _ => Err(ComparisonError),
    }
}
fn integer_float(integer: &BigInt, float: f64) -> Result<Ordering, ComparisonError> {
    if !float.is_finite() {
        return Err(ComparisonError);
    }
    let truncated = BigInt::from_f64(float).ok_or(ComparisonError)?;
    match integer.cmp(&truncated) {
        Ordering::Equal if float.fract() > 0.0 => Ok(Ordering::Less),
        Ordering::Equal if float.fract() < 0.0 => Ok(Ordering::Greater),
        ordering => Ok(ordering),
    }
}
/// # Errors
/// Rejects absent nodes and incomparable scalar types.
pub fn less(
    left: &Document,
    a: NodeId,
    right: &Document,
    b: NodeId,
) -> Result<bool, ComparisonError> {
    let left = left.node(a).ok_or(ComparisonError)?;
    let right = right.node(b).ok_or(ComparisonError)?;
    let ordering = match (left, right) {
        (Node::String(a), Node::String(b)) => a.cmp(b),
        _ => numeric(left, right)?,
    };
    Ok(ordering == Ordering::Less)
}
