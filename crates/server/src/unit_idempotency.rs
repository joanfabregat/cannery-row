//! Canonical request hashes for idempotency keys.
use cannery_core::json::{Document, DocumentBuilder, Node};
use sha2::{Digest, Sha256};
pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub(crate) fn canonical_hash(document: &Document, budget: usize) -> Result<Vec<u8>> {
    Ok(Sha256::digest(cannery_core::json::canonical::bytes(document, budget)?).to_vec())
}
pub(crate) fn decision_hash(project: &str, decision: &Document, budget: usize) -> Result<Vec<u8>> {
    envelope_hash(project, None, "decision", decision, budget)
}
pub(crate) fn envelope_hash(
    project: &str,
    number: Option<&num_bigint::BigInt>,
    name: &str,
    review: &Document,
    budget: usize,
) -> Result<Vec<u8>> {
    enum CopyPart {
        Visit(cannery_core::json::NodeId),
        Finish(cannery_core::json::NodeId),
    }
    let mut b = DocumentBuilder::new();
    let mut translated = std::collections::BTreeMap::new();
    let mut pending = vec![CopyPart::Visit(review.root())];
    while let Some(part) = pending.pop() {
        let original = match part {
            CopyPart::Visit(id) => {
                let original = review.node(id).ok_or("invalid hash node")?;
                if translated.contains_key(&(std::ptr::from_ref(original) as usize)) {
                    continue;
                }
                pending.push(CopyPart::Finish(id));
                match original {
                    Node::Object(values) => {
                        pending.extend(values.iter().rev().map(|(_, id)| CopyPart::Visit(*id)));
                    }
                    Node::Array(values) => {
                        pending.extend(values.iter().rev().map(|id| CopyPart::Visit(*id)));
                    }
                    _ => {}
                }
                continue;
            }
            CopyPart::Finish(id) => review.node(id).ok_or("invalid hash node")?,
        };
        let child = |id| -> Result<_> {
            let node = review.node(id).ok_or("invalid hash node")?;
            translated
                .get(&(std::ptr::from_ref(node) as usize))
                .copied()
                .ok_or_else(|| "invalid hash child".into())
        };
        let node = match original {
            Node::Object(v) => {
                let mut v = v.clone();
                v.sort_by_key(|(a, _)| a.codepoints());
                Node::Object(
                    v.into_iter()
                        .map(|(k, v)| Ok((k, child(v)?)))
                        .collect::<Result<_>>()?,
                )
            }
            Node::Array(v) => Node::Array(v.iter().map(|id| child(*id)).collect::<Result<_>>()?),
            Node::Null => Node::Null,
            Node::Bool(v) => Node::Bool(*v),
            Node::Integer(v) => Node::Integer(v.clone()),
            Node::Float(v) => Node::Float(*v),
            Node::String(v) => Node::String(v.clone()),
        };
        let id = b.push(node)?;
        translated.insert(std::ptr::from_ref(original) as usize, id);
    }
    let review = *translated
        .get(&(std::ptr::from_ref(review.node(review.root()).ok_or("missing hash root")?) as usize))
        .ok_or("missing translated root")?;
    let project = b.push(Node::String(String::from(project)))?;
    let mut fields = vec![
        (String::from("project"), project),
        (String::from(name), review),
    ];
    if let Some(number) = number {
        let unit = b.push(Node::Integer(number.clone()))?;
        fields.push((String::from("unit"), unit));
    }
    fields.sort_by_key(|(a, _)| a.codepoints());
    let root = b.push(Node::Object(fields))?;
    let d = b.finish(root)?;
    Ok(Sha256::digest(cannery_core::json::encode_http(&d, budget)?).to_vec())
}
#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
