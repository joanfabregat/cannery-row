//! Claimed job documents retain source dictionary updates and pinned inputs.
use crate::{
    job_baselines,
    science::{RenderingContext, ScienceError},
};
use cannery_core::{
    ids::{AttemptId, JobId},
    json::{Document, DocumentBuilder, Node, NodeId},
    text,
    timestamps::Timestamp,
};
use num_bigint::BigInt;

/// Persisted scalar fields are supplied separately from the lossless specification.
pub struct JobDocument<'a> {
    pub id: JobId,
    pub attempt_id: AttemptId,
    pub phase: &'a String,
    pub science_revision: &'a BigInt,
    pub lease_generation: &'a BigInt,
    pub deadline: Option<Timestamp>,
    pub lease_expires_at: Option<Timestamp>,
    pub spec: &'a Document,
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("claimed job timestamps are absent")]
    Invariant,
    #[error(transparent)]
    Science(#[from] ScienceError),
    #[error("job document construction failed")]
    Build,
}
fn set(fields: &mut Vec<(String, NodeId)>, name: String, value: NodeId) {
    if let Some((_, old)) = fields.iter_mut().find(|(key, _)| key == &name) {
        *old = value;
    } else {
        fields.push((name, value));
    }
}
fn string(builder: &mut DocumentBuilder, value: impl AsRef<str>) -> Result<NodeId, Error> {
    builder
        .push(Node::String(String::from(value.as_ref())))
        .map_err(|_| Error::Build)
}
fn field(fields: &mut Vec<(String, NodeId)>, name: &str, value: NodeId) {
    set(fields, String::from(name), value);
}
/// Source job contract projection, before the caller's response serialization.
/// # Errors
/// Requires claimed timestamps, then consumes pinned inputs and specification in source order.
pub fn job_document(
    job: &JobDocument<'_>,
    lease_token: &str,
    rendering: RenderingContext,
) -> Result<Document, Error> {
    let deadline = job.deadline.ok_or(Error::Invariant)?;
    let expires = job.lease_expires_at.ok_or(Error::Invariant)?;
    let control = job_baselines::pinned_control(job.spec, rendering)?;
    let parameters = job_baselines::pinned_parameters(job.spec)?;
    let Some(Node::Object(spec)) = job.spec.node(job.spec.root()) else {
        return Err(ScienceError::Attribute.into());
    };
    let mut builder = DocumentBuilder::new();
    let mut fields = vec![];
    field(&mut fields, "schema_version", string(&mut builder, "0.2")?);
    field(
        &mut fields,
        "job_id",
        string(&mut builder, job.id.to_string())?,
    );
    field(
        &mut fields,
        "phase",
        builder
            .push(Node::String(job.phase.clone()))
            .map_err(|_| Error::Build)?,
    );
    field(
        &mut fields,
        "attempt_id",
        string(&mut builder, job.attempt_id.to_string())?,
    );
    for (key, value) in spec {
        if key.equals_utf8("control") || key.equals_utf8("parameters") {
            continue;
        }
        set(
            &mut fields,
            key.clone(),
            builder.import(job.spec, *value).map_err(|_| Error::Build)?,
        );
    }
    for (name, value) in [("control", control), ("parameters", parameters)] {
        if let Some(value) =
            value.filter(|value| !matches!(value.node(value.root()), Some(Node::Null)))
        {
            field(
                &mut fields,
                name,
                builder
                    .import(&value, value.root())
                    .map_err(|_| Error::Build)?,
            );
        }
    }
    let mut revision = DocumentBuilder::new();
    let root = revision
        .push(Node::Integer(job.science_revision.clone()))
        .map_err(|_| Error::Build)?;
    let revision = revision.finish(root).map_err(|_| Error::Build)?;
    let revision =
        text::str_value(&revision, root, rendering.nesting_budget).map_err(ScienceError::from)?;
    field(
        &mut fields,
        "science_revision",
        builder
            .push(Node::String(revision))
            .map_err(|_| Error::Build)?,
    );
    field(
        &mut fields,
        "deadline",
        string(&mut builder, deadline.isoformat())?,
    );
    let token = builder
        .push(Node::String(lease_token.to_owned()))
        .map_err(|_| Error::Build)?;
    let generation = builder
        .push(Node::Integer(job.lease_generation.clone()))
        .map_err(|_| Error::Build)?;
    let expires = string(&mut builder, expires.isoformat())?;
    let lease = builder
        .push(Node::Object(vec![
            (String::from("token"), token),
            (String::from("generation"), generation),
            (String::from("expires_at"), expires),
        ]))
        .map_err(|_| Error::Build)?;
    field(&mut fields, "lease", lease);
    let root = builder
        .push(Node::Object(fields))
        .map_err(|_| Error::Build)?;
    builder.finish(root).map_err(|_| Error::Build)
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
