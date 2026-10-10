//! Claim-time workflow projection over the caller's existing attempt repository.
use cannery_attempts::{
    model::{Attempt, StoredJson},
    repo::{AttemptError, Repository},
};
use cannery_core::{
    json::{Document, DocumentBuilder, Node, NodeId},
    text,
};
use cannery_research::{
    job_baselines,
    science::{RenderingContext, Science, ScienceError},
};
use num_bigint::BigInt;

/// Already resolved and checked step; the caller retains its manifest owner.
pub struct WorkflowStep<'a> {
    pub name: &'a String,
    pub revision: &'a BigInt,
    pub manifest: &'a Document,
}

/// Source pinned-manifest reads remain interleaved with each reference conversion.
/// # Errors
/// Preserves source malformed-reference, query, decode and missing-manifest failures.
pub async fn pinned_manifests(
    query: &mut dyn crate::step_binding::ManifestQuery,
    project: cannery_core::ids::ProjectId,
    workflow: &Document,
    rendering: RenderingContext,
    decode_budget: usize,
) -> Result<Document, crate::step_binding::Error> {
    let mut builder = DocumentBuilder::new();
    let mut nodes = vec![];
    for reference in job_baselines::pinned_references(workflow, rendering)? {
        let (name, revision) = reference?;
        let name = name.as_utf8().ok_or(crate::step_binding::Error::Store)?;
        let manifest = query
            .manifest(project, &name, &revision, true, decode_budget)
            .await?
            .ok_or(crate::step_binding::Error::Store)?;
        nodes.push(
            builder
                .import(&manifest, manifest.root())
                .map_err(|_| crate::step_binding::Error::Store)?,
        );
    }
    let root = builder
        .push(Node::Array(nodes))
        .map_err(|_| crate::step_binding::Error::Store)?;
    builder
        .finish(root)
        .map_err(|_| crate::step_binding::Error::Store)
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Database(#[from] AttemptError),
    #[error(transparent)]
    Science(#[from] ScienceError),
    #[error("workflow persistence invariant failed")]
    Invariant,
    #[error("workflow document construction failed")]
    Build,
}
fn push(builder: &mut DocumentBuilder, node: Node) -> Result<NodeId, Error> {
    builder.push(node).map_err(|_| Error::Build)
}
fn text(builder: &mut DocumentBuilder, value: impl AsRef<str>) -> Result<NodeId, Error> {
    push(builder, Node::String(String::from(value.as_ref())))
}
fn object(builder: &mut DocumentBuilder, fields: Vec<(&str, NodeId)>) -> Result<NodeId, Error> {
    push(
        builder,
        Node::Object(
            fields
                .into_iter()
                .map(|(name, value)| (String::from(name), value))
                .collect(),
        ),
    )
}
fn copy(builder: &mut DocumentBuilder, document: &Document) -> Result<NodeId, Error> {
    builder
        .import(document, document.root())
        .map_err(|_| Error::Build)
}
fn manifests(steps: &[WorkflowStep<'_>]) -> Result<Document, Error> {
    let mut builder = DocumentBuilder::new();
    let nodes = steps
        .iter()
        .map(|step| copy(&mut builder, step.manifest))
        .collect::<Result<Vec<_>, _>>()?;
    let root = push(&mut builder, Node::Array(nodes))?;
    builder.finish(root).map_err(|_| Error::Build)
}
fn resolved(steps: &[WorkflowStep<'_>]) -> Result<Document, Error> {
    let mut builder = DocumentBuilder::new();
    let nodes = steps
        .iter()
        .map(|step| {
            let name = push(&mut builder, Node::String(step.name.clone()))?;
            let revision = push(&mut builder, Node::Integer(step.revision.clone()))?;
            let manifest = copy(&mut builder, step.manifest)?;
            object(
                &mut builder,
                vec![
                    ("name", name),
                    ("revision", revision),
                    ("manifest", manifest),
                ],
            )
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let root = push(&mut builder, Node::Array(nodes))?;
    builder.finish(root).map_err(|_| Error::Build)
}
/// Source runner specification, including source-ordered consumed repository reads.
/// # Errors
/// Preserves predecessor assertions, repository failures and consumed science errors.
#[allow(
    clippy::too_many_lines,
    reason = "Source construction and query order is observable"
)]
pub async fn run_spec(
    repository: &mut Repository<'_>,
    attempt: &Attempt,
    science: &Science<'_>,
    steps: &[WorkflowStep<'_>],
    control: Option<&Document>,
    rendering: RenderingContext,
) -> Result<Document, Error> {
    let deadline = attempt.deadline.ok_or(Error::Invariant)?;
    let resolved = resolved(steps)?;
    let wanted = job_baselines::predecessor_roles(&manifests(steps)?, rendering)?;
    let mut builder = DocumentBuilder::new();
    let predecessor = if let Some(id) = attempt.predecessor_id {
        let previous = repository
            .get_attempt_by_id(id, false)
            .await?
            .ok_or(Error::Invariant)?;
        let found = repository.list_artifacts(previous.id).await?;
        let previous_id = text(&mut builder, previous.id.to_string())?;
        let reference = text(
            &mut builder,
            format!("#{}.{}", previous.unit_number, previous.sequence),
        )?;
        let state = text(&mut builder, previous.state.as_str())?;
        let failure = repository.last_failure_code(previous.id).await?;
        let failure = match failure {
            Some(value) => text(&mut builder, value)?,
            None => push(&mut builder, Node::Null)?,
        };
        let mut artifacts = vec![];
        for artifact in found {
            if !wanted.contains(&String::from(&artifact.role))
                || artifact.backend == "external"
                || artifact.job_id.is_some()
            {
                continue;
            }
            let id = text(&mut builder, artifact.id.0.to_string())?;
            let role = text(&mut builder, artifact.role)?;
            let backend = text(&mut builder, artifact.backend)?;
            let bucket = text(&mut builder, artifact.bucket)?;
            let key = text(&mut builder, artifact.key)?;
            let storage = object(
                &mut builder,
                vec![("backend", backend), ("bucket", bucket), ("key", key)],
            )?;
            let size = push(&mut builder, Node::Integer(artifact.size_bytes.into()))?;
            let sha256 = text(&mut builder, artifact.sha256)?;
            let media_type = text(&mut builder, artifact.media_type)?;
            artifacts.push(object(
                &mut builder,
                vec![
                    ("id", id),
                    ("role", role),
                    ("storage", storage),
                    ("size_bytes", size),
                    ("sha256", sha256),
                    ("media_type", media_type),
                ],
            )?);
        }
        let artifacts = push(&mut builder, Node::Array(artifacts))?;
        object(
            &mut builder,
            vec![
                ("attempt_id", previous_id),
                ("ref", reference),
                ("state", state),
                ("failure_code", failure),
                ("artifacts", artifacts),
            ],
        )?
    } else {
        push(&mut builder, Node::Null)?
    };
    let attempt_id = text(&mut builder, attempt.id.to_string())?;
    let reference = text(
        &mut builder,
        format!("#{}.{}", attempt.unit_number, attempt.sequence),
    )?;
    let track = text(&mut builder, &attempt.track_slug)?;
    let mut revision_builder = DocumentBuilder::new();
    let revision = push(
        &mut revision_builder,
        Node::Integer(science.revision.clone()),
    )?;
    let revision_document = revision_builder
        .finish(revision)
        .map_err(|_| Error::Build)?;
    let revision = text::str_value(&revision_document, revision, rendering.nesting_budget)
        .map_err(ScienceError::from)?;
    let revision = push(&mut builder, Node::String(revision))?;
    let steps = copy(&mut builder, &resolved)?;
    let parameters = repository.approved_project_fields(attempt.unit_id).await?;
    let parameters = match parameters {
        StoredJson::Value(value) => copy(&mut builder, &value)?,
        StoredJson::SqlNull => return Err(Error::Invariant),
    };
    let mut datasets = vec![];
    for (_, dataset) in &science.datasets {
        if dataset.held_out_labels {
            continue;
        }
        let id = push(&mut builder, Node::String(dataset.id.clone()))?;
        let revision = push(&mut builder, Node::String(dataset.revision.clone()))?;
        datasets.push(object(
            &mut builder,
            vec![("id", id), ("revision", revision)],
        )?);
    }
    let datasets = push(&mut builder, Node::Array(datasets))?;
    let baselines = job_baselines::staged_baselines(science, &resolved, control, rendering)?;
    let baselines = copy(&mut builder, &baselines)?;
    let inputs = object(
        &mut builder,
        vec![
            ("datasets", datasets),
            ("baselines", baselines),
            ("predecessor", predecessor),
        ],
    )?;
    let max_bytes = push(&mut builder, Node::Integer(science.max_output_bytes()?))?;
    let limits = object(&mut builder, vec![("max_output_bytes", max_bytes)])?;
    let deadline = text(&mut builder, deadline.isoformat())?;
    let mut fields = vec![
        ("attempt_id", attempt_id),
        ("attempt_ref", reference),
        ("track", track),
        ("science_revision", revision),
        ("steps", steps),
        ("parameters", parameters),
        ("inputs", inputs),
        ("limits", limits),
        ("deadline", deadline),
    ];
    if let Some(control) = control {
        fields.push(("control", copy(&mut builder, control)?));
    }
    let root = object(&mut builder, fields)?;
    builder.finish(root).map_err(|_| Error::Build)
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
