//! Default dashboard definitions derived from an already constructed science record.
use crate::projection::Error;
use cannery_core::{
    json::{Document, DocumentBuilder, Node, NodeId},
    text,
};

fn push(builder: &mut DocumentBuilder, node: Node) -> Result<NodeId, Error> {
    builder.push(node).map_err(|_| Error::Value)
}
fn string(builder: &mut DocumentBuilder, value: &str) -> Result<NodeId, Error> {
    push(builder, Node::String(String::from(value)))
}
fn rendered(document: &Document, id: NodeId, budget: usize) -> Result<String, Error> {
    text::str_value(document, id, budget)
        .map_err(|error| match error {
            text::RenderError::Recursion => Error::Recursion,
            _ => Error::Value,
        })?
        .as_utf8()
        .ok_or(Error::Value)
}
fn items(document: &Document, root: NodeId, name: &str) -> Result<Vec<NodeId>, Error> {
    if !matches!(document.node(root), Some(Node::Object(_))) {
        return Err(Error::Attribute);
    }
    match document.field(root, name).and_then(|id| document.node(id)) {
        None => Ok(Vec::new()),
        Some(Node::Array(values)) => Ok(values.clone()),
        _ => Err(Error::Type),
    }
}
fn required(document: &Document, root: NodeId, name: &str) -> Result<NodeId, Error> {
    if !matches!(document.node(root), Some(Node::Object(_))) {
        return Err(Error::Type);
    }
    document.field(root, name).ok_or(Error::Key)
}
fn first_split(content: &Document, metric: NodeId) -> Result<Option<Document>, Error> {
    let mut builder = DocumentBuilder::new();
    let root = match content
        .field(metric, "splits")
        .and_then(|id| content.node(id))
    {
        None | Some(Node::Null | Node::Bool(false)) => return Ok(None),
        Some(Node::Integer(value)) if value == &num_bigint::BigInt::from(0) => return Ok(None),
        Some(Node::Float(value)) if *value == 0.0 => return Ok(None),
        Some(Node::Object(entries)) if entries.is_empty() => return Ok(None),
        Some(Node::Array(values)) => {
            let Some(id) = values.first() else {
                return Ok(None);
            };
            builder.import(content, *id).map_err(|_| Error::Value)?
        }
        Some(Node::String(value)) => {
            let Some(point) = value.chars().next() else {
                return Ok(None);
            };
            push(&mut builder, Node::String(point.to_string()))?
        }
        _ => return Err(Error::Type),
    };
    builder.finish(root).map(Some).map_err(|_| Error::Value)
}
/// Preserve insertion order, source str coercion, and the selected first split.
/// # Errors
/// Reports malformed legacy registry structure and source string-rendering failures.
pub fn derived_views(content: &Document, rendering_budget: usize) -> Result<Document, Error> {
    let mut builder = DocumentBuilder::new();
    let mut views = Vec::new();
    for metric in items(content, content.root(), "metrics")? {
        let key = rendered(content, required(content, metric, "key")?, rendering_budget)?;
        let Some(split_document) = first_split(content, metric)? else {
            continue;
        };
        let split_text = rendered(&split_document, split_document.root(), rendering_budget)?;
        let mut definitions = vec![
            (
                format!("{key}-by-track"),
                format!("{key} ({split_text}) by track"),
                "table",
                None,
                "track".to_owned(),
            ),
            (
                format!("{key}-timeline"),
                format!("{key} ({split_text}) over time"),
                "line",
                Some("attempt.finished_at"),
                "track".to_owned(),
            ),
        ];
        for dimension in items(content, metric, "dimensions")? {
            let name = rendered(
                content,
                required(content, dimension, "name")?,
                rendering_budget,
            )?;
            definitions.push((
                format!("{key}-by-{name}"),
                format!("{key} ({split_text}) by {name}"),
                "bar",
                Some("track.slug"),
                name,
            ));
        }
        for (id, title, chart, x, group) in definitions {
            let mut fields = Vec::new();
            for (name, value) in [
                ("id", id.replace('_', "-")),
                ("title", title),
                ("chart", chart.into()),
                ("metric", key.clone()),
            ] {
                fields.push((String::from(name), string(&mut builder, &value)?));
            }
            let split = builder
                .import(&split_document, split_document.root())
                .map_err(|_| Error::Value)?;
            fields.push((String::from("split"), split));
            if let Some(x) = x {
                fields.push((String::from("x"), string(&mut builder, x)?));
            }
            let group = string(&mut builder, &group)?;
            let group = push(&mut builder, Node::Array(vec![group]))?;
            fields.push((String::from("group_by"), group));
            fields.push((String::from("baseline"), string(&mut builder, "control")?));
            views.push(push(&mut builder, Node::Object(fields))?);
        }
    }
    let root = push(&mut builder, Node::Array(views))?;
    builder.finish(root).map_err(|_| Error::Value)
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
