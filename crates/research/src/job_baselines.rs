//! Pure baseline and pinned-input projections shared by jobs and attempt claims.
use crate::{
    science::{self, RenderingContext, Science, ScienceError, Value},
    steps::{self, Side},
};
use cannery_core::{
    json::{Document, DocumentBuilder, Node, NodeId},
    text,
};
use num_bigint::BigInt;

/// Source references are consumed one at a time, interleaved with manifest queries.
pub struct PinnedReferences<'a> {
    document: &'a Document,
    values: std::vec::IntoIter<Value>,
    rendering: RenderingContext,
}
/// # Errors
/// Preserves indexing and iteration failures before the first query.
pub fn pinned_references(
    document: &Document,
    rendering: RenderingContext,
) -> Result<PinnedReferences<'_>, ScienceError> {
    let steps = science::required(document, &Value::Node(document.root()), "steps")?;
    Ok(PinnedReferences {
        document,
        values: science::items(document, steps)?.into_iter(),
        rendering,
    })
}
impl Iterator for PinnedReferences<'_> {
    type Item = Result<(String, BigInt), ScienceError>;
    fn next(&mut self) -> Option<Self::Item> {
        let value = self.values.next()?;
        Some((|| {
            let name = science::required(self.document, &value, "name")?;
            let name = science::text(self.document, &Value::Node(name), self.rendering)?;
            let revision = science::required(self.document, &value, "revision")?;
            Ok((
                name,
                science::configuration_integer(self.document, revision)?,
            ))
        })())
    }
}

fn copy(document: &Document, root: NodeId) -> Result<Document, ScienceError> {
    let mut builder = DocumentBuilder::new();
    let root = builder
        .import(document, root)
        .map_err(|_| ScienceError::InvalidNode)?;
    builder.finish(root).map_err(|_| ScienceError::InvalidNode)
}
fn input_ids(
    document: &Document,
    manifests: &[Value],
    origin: &str,
    rendering: RenderingContext,
) -> Result<Vec<String>, ScienceError> {
    let mut found = vec![];
    for manifest in manifests {
        let Value::Node(manifest) = manifest else {
            return Err(ScienceError::Type);
        };
        let artifacts = steps::artifacts(document, *manifest, Side::Inputs)?;
        for artifact in science::items(document, artifacts)? {
            let from = science::required(document, &artifact, "from")?;
            if matches!(science::node(document, from)?, Node::String(value) if value.equals_utf8(origin))
            {
                let value = if origin == "baseline" {
                    let Value::Node(artifact) = artifact else {
                        return Err(ScienceError::Attribute);
                    };
                    steps::source_id(document, artifact, rendering)?
                } else {
                    science::required_text(document, &artifact, "name", rendering)?
                };
                found.push(value);
            }
        }
    }
    Ok(found)
}
/// Baseline input IDs, retaining duplicates and source traversal order.
/// # Errors
/// Preserves the first consumed malformed manifest/artifact or eager fallback failure.
pub fn baseline_inputs(
    manifests: &Document,
    rendering: RenderingContext,
) -> Result<Vec<String>, ScienceError> {
    input_ids(
        manifests,
        &science::items(manifests, manifests.root())?,
        "baseline",
        rendering,
    )
}
/// Predecessor artifact roles, with source set deduplication and no invented order.
/// # Errors
/// Preserves source iteration, indexing and Python string conversion failures.
pub fn predecessor_roles(
    manifests: &Document,
    rendering: RenderingContext,
) -> Result<Vec<String>, ScienceError> {
    let mut roles = vec![];
    for role in input_ids(
        manifests,
        &science::items(manifests, manifests.root())?,
        "attempt",
        rendering,
    )? {
        if !roles.contains(&role) {
            roles.push(role);
        }
    }
    Ok(roles)
}
fn control_fields(
    control: &Document,
    rendering: RenderingContext,
) -> Result<(NodeId, NodeId), ScienceError> {
    let id = science::required(control, &Value::Node(control.root()), "id")?;
    let revision = science::required(control, &Value::Node(control.root()), "revision")?;
    // Tuple membership in the source set hashes both values even when the registry is empty.
    science::scalar(control, &Value::Node(id), rendering)?;
    science::scalar(control, &Value::Node(revision), rendering)?;
    Ok((id, revision))
}
fn registered(science: &Science<'_>, control: &Document, fields: (NodeId, NodeId)) -> bool {
    let (Some(Node::String(id)), Some(Node::String(revision))) =
        (control.node(fields.0), control.node(fields.1))
    else {
        return false;
    };
    science
        .baselines
        .iter()
        .any(|(i, r)| i == id && r == revision)
}
/// Explain a consumed unregistered control without normalizing opaque legacy values.
/// # Errors
/// Preserves control lookup/hash failures before traversing manifests.
pub fn control_conflict(
    science: &Science<'_>,
    manifests: &Document,
    control: Option<&Document>,
    rendering: RenderingContext,
) -> Result<Option<String>, ScienceError> {
    let Some(control) =
        control.filter(|value| !matches!(value.node(value.root()), Some(Node::Null)))
    else {
        return Ok(None);
    };
    let fields = control_fields(control, rendering)?;
    if registered(science, control, fields) {
        return Ok(None);
    }
    let wanted = baseline_inputs(manifests, rendering)?;
    let Some(Node::String(id)) = control.node(fields.0) else {
        return Ok(None);
    };
    if !wanted.contains(id) {
        return Ok(None);
    }
    let id_repr = text::repr_string(id)?;
    // A conflict is possible only for a string id; revision remains an arbitrary hashable value.
    let revision = match science::node(control, fields.1)? {
        Node::String(value) => text::repr_string(value)?,
        _ => science::text(control, &Value::Node(fields.1), rendering)?,
    };
    let pieces = [
        String::from("a step takes baseline "),
        id_repr,
        String::from(" as input, but the control's revision "),
        revision,
        String::from(" is not registered"),
    ];
    Ok(Some(
        cannery_core::text::from_codepoints(
            pieces
                .iter()
                .flat_map(|value| value.chars().map(u32::from))
                .collect(),
        )
        .ok_or(ScienceError::InvalidNode)?,
    ))
}
/// Registered control first, then each requested baseline's first registered revision.
/// # Errors
/// Preserves source control membership, complete manifest-list construction and input order.
pub fn staged_baselines(
    science: &Science<'_>,
    steps: &Document,
    control: Option<&Document>,
    rendering: RenderingContext,
) -> Result<Document, ScienceError> {
    let control = control.filter(|value| !matches!(value.node(value.root()), Some(Node::Null)));
    let registered_control = control
        .map(|value| {
            control_fields(value, rendering).map(|fields| registered(science, value, fields))
        })
        .transpose()?
        .unwrap_or(false);
    let manifests = science::items(steps, steps.root())?
        .iter()
        .map(|step| science::required(steps, step, "manifest").map(Value::Node))
        .collect::<Result<Vec<_>, _>>()?;
    let wanted = input_ids(steps, &manifests, "baseline", rendering)?;
    let mut builder = DocumentBuilder::new();
    let mut refs = vec![];
    let mut pinned_ids = vec![];
    if registered_control && let Some(control) = control {
        refs.push(
            builder
                .import(control, control.root())
                .map_err(|_| ScienceError::InvalidNode)?,
        );
        let Some(Node::String(id)) = control.node(science::required(
            control,
            &Value::Node(control.root()),
            "id",
        )?) else {
            return Err(ScienceError::InvalidNode);
        };
        pinned_ids.push(id.clone());
    }
    for id in wanted {
        if pinned_ids.contains(&id) {
            continue;
        }
        if let Some((_, revision)) = science.baseline_refs.iter().find(|(name, _)| name == &id) {
            let id_node = builder
                .push(Node::String(id.clone()))
                .map_err(|_| ScienceError::InvalidNode)?;
            let revision_node = builder
                .push(Node::String(revision.clone()))
                .map_err(|_| ScienceError::InvalidNode)?;
            refs.push(
                builder
                    .push(Node::Object(vec![
                        (String::from("id"), id_node),
                        (String::from("revision"), revision_node),
                    ]))
                    .map_err(|_| ScienceError::InvalidNode)?,
            );
            pinned_ids.push(id);
        }
    }
    let root = builder
        .push(Node::Array(refs))
        .map_err(|_| ScienceError::InvalidNode)?;
    builder.finish(root).map_err(|_| ScienceError::InvalidNode)
}
/// Preserve explicit control values; normalize only the pre-0010 first-baseline fallback.
/// # Errors
/// Reports malformed consumed legacy inputs in source order.
pub fn pinned_control(
    spec: &Document,
    rendering: RenderingContext,
) -> Result<Option<Document>, ScienceError> {
    if let Some(control) = science::field(spec, &Value::Node(spec.root()), "control", true)
        .or_else(|error| {
            if error == ScienceError::Key {
                Ok(None)
            } else {
                Err(error)
            }
        })?
    {
        return if matches!(science::node(spec, control)?, Node::Null) {
            Ok(None)
        } else {
            copy(spec, control).map(Some)
        };
    }
    let inputs = science::required(spec, &Value::Node(spec.root()), "inputs")?;
    let Some(baselines) = science::field(spec, &Value::Node(inputs), "baselines", false)? else {
        return Ok(None);
    };
    let first = match science::node(spec, baselines)? {
        Node::Array(values) if values.is_empty() => return Ok(None),
        Node::Array(values) => values[0],
        Node::Null | Node::Bool(false) => return Ok(None),
        Node::Integer(value) if *value == 0.into() => return Ok(None),
        Node::Float(value) if *value == 0.0 => return Ok(None),
        Node::String(value) if value.codepoints().is_empty() => return Ok(None),
        Node::Object(values) if values.is_empty() => return Ok(None),
        Node::Object(_) => return Err(ScienceError::Key),
        _ => return Err(ScienceError::Type),
    };
    let id = science::required_text(spec, &Value::Node(first), "id", rendering)?;
    let revision = science::required_text(spec, &Value::Node(first), "revision", rendering)?;
    let mut builder = DocumentBuilder::new();
    let id = builder
        .push(Node::String(id))
        .map_err(|_| ScienceError::InvalidNode)?;
    let revision = builder
        .push(Node::String(revision))
        .map_err(|_| ScienceError::InvalidNode)?;
    let root = builder
        .push(Node::Object(vec![
            (String::from("id"), id),
            (String::from("revision"), revision),
        ]))
        .map_err(|_| ScienceError::InvalidNode)?;
    builder
        .finish(root)
        .map_err(|_| ScienceError::InvalidNode)
        .map(Some)
}
/// A job's pinned unit parameters, including explicit null/scalar legacy values.
/// # Errors
/// A spec without parameters (a job created before verify jobs pinned them) has none.
pub fn pinned_parameters(spec: &Document) -> Result<Option<Document>, ScienceError> {
    science::field(spec, &Value::Node(spec.root()), "parameters", false)?
        .map(|root| copy(spec, root))
        .transpose()
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
