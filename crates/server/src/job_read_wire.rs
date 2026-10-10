//! Ordinary job models preserve Python projection and Pydantic construction order.
use crate::{
    api_contract::{convert, decode, encode},
    api_models::{
        JobOut, Page_JobOut_UUID_, StepRef, StepRefRevisionValue, VerificationDocument,
        WriteupDocument,
    },
};
use cannery_attempts::model::{Artifact, StoredJson};
use cannery_core::{
    json::{
        Document, Node, NodeId,
        model::{self, ModelEncodeError},
    },
    text,
};
use cannery_jobs::repo::Job;
#[derive(Clone, Copy)]
pub struct ResponseContext {
    pub inferred_nesting_budget: usize,
    pub representation_budget: usize,
}
type Result<T> = std::result::Result<T, ModelEncodeError>;
fn required(document: &Document, id: NodeId, k: &str) -> Result<NodeId> {
    match document.node(id) {
        Some(Node::Object(_)) => document.field(id, k).ok_or(ModelEncodeError::InvalidNode),
        _ => Err(ModelEncodeError::InvalidNode),
    }
}
fn str_value(document: &Document, id: NodeId, profile: ResponseContext) -> Result<String> {
    text::str_value(document, id, profile.representation_budget)
        .map_err(|_| ModelEncodeError::InvalidNode)
}
/// Intermediate values constructed before the source queries output artifacts.
pub(crate) struct Projection {
    track: String,
    steps: Vec<(String, Node)>,
    parameters: Option<Document>,
    output_prefix: String,
}
/// # Errors
/// Retains eager per-step model validation before parameters and artifact lookup.
pub(crate) fn prepare(job: &Job, profile: ResponseContext) -> Result<Projection> {
    let document = &job.spec;
    let track = str_value(
        document,
        required(document, document.root(), "track")?,
        profile,
    )?;
    let values = match document
        .field(document.root(), "steps")
        .and_then(|id| document.node(id))
    {
        None => vec![],
        Some(Node::Array(value)) => value.clone(),
        Some(Node::Object(value)) if value.is_empty() => vec![],
        Some(Node::String(value)) if value.codepoints().is_empty() => vec![],
        _ => return Err(ModelEncodeError::InvalidNode),
    };
    let steps = values
        .into_iter()
        .map(|id| {
            let name = str_value(document, required(document, id, "name")?, profile)?;
            let revision = required(document, id, "revision")?;
            let node = document
                .node(revision)
                .ok_or(ModelEncodeError::InvalidNode)?;
            let revision = if matches!(node, Node::String(_)) {
                Node::String(match node {
                    Node::String(value) => value.clone(),
                    _ => return Err(ModelEncodeError::InvalidNode),
                })
            } else {
                let integer =
                    crate::validation::model_integer(node).ok_or(ModelEncodeError::InvalidNode)?;
                Node::Integer(integer)
            };
            Ok((name, revision))
        })
        .collect::<Result<Vec<_>>>()?;
    let parameters = cannery_research::job_baselines::pinned_parameters(document)
        .map_err(|_| ModelEncodeError::InvalidNode)?;
    let output_prefix = str_value(
        document,
        required(document, document.root(), "output_prefix")?,
        profile,
    )?;
    Ok(Projection {
        track,
        steps,
        parameters,
        output_prefix,
    })
}

/// # Errors
/// Rejects stored job fields that cannot be represented by the response contract.
#[allow(clippy::too_many_lines)] // One wire projection of every stored job field.
pub(crate) fn job(
    job: &Job,
    value: Projection,
    verification: Option<(&StoredJson, &str)>,
    outputs: &[Artifact],
    profile: ResponseContext,
) -> Result<Vec<u8>> {
    use num_traits::ToPrimitive;
    let steps = value
        .steps
        .into_iter()
        .map(|(name, revision)| {
            Ok(StepRef {
                name: name.as_utf8().ok_or(ModelEncodeError::Encoding)?,
                revision: match revision {
                    Node::String(text) => StepRefRevisionValue::Variant1(
                        text.as_utf8().ok_or(ModelEncodeError::Encoding)?,
                    ),
                    Node::Integer(number) => StepRefRevisionValue::Variant0(
                        number.to_i64().ok_or(ModelEncodeError::InvalidNode)?,
                    ),
                    _ => return Err(ModelEncodeError::InvalidNode),
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let parameters = value
        .parameters
        .as_ref()
        .filter(|d| !matches!(d.node(d.root()), Some(Node::Null)))
        .map(|d| {
            decode(&model::encode_model_mapping(
                d,
                d.root(),
                profile.inferred_nesting_budget,
            )?)
        })
        .transpose()?
        .unwrap_or_default();
    let logs = match job.logs.node(job.logs.root()) {
        Some(Node::Array(ids)) => ids
            .iter()
            .map(|id| {
                decode(&model::encode_model_mapping(
                    &job.logs,
                    *id,
                    profile.inferred_nesting_budget,
                )?)
            })
            .collect::<Result<Vec<_>>>()?,
        _ => return Err(ModelEncodeError::InvalidNode),
    };
    let verification = match verification {
        None | Some((StoredJson::SqlNull, _)) => None,
        Some((StoredJson::Value(d), _)) if matches!(d.node(d.root()), Some(Node::Null)) => None,
        Some((StoredJson::Value(d), body)) => Some(VerificationDocument {
            front_matter: decode(&model::encode_model_mapping(
                d,
                d.root(),
                profile.inferred_nesting_budget,
            )?)?,
            body_markdown: body.to_owned(),
        }),
    };
    encode(&JobOut {
        id: job.id.to_string(),
        attempt_id: job.attempt_id.to_string(),
        phase: job.phase.as_str().to_owned(),
        performer: job.performer.as_str().to_owned(),
        run_number: i64::from(job.run_number),
        origin: job.origin.as_str().to_owned(),
        previous_run_id: job.previous_run_id.map(|v| v.to_string()),
        state: job.state.as_str().to_owned(),
        science_revision: i64::from(job.science_revision),
        verifier: convert(&job.verifier_id)?,
        track: value.track.as_utf8().ok_or(ModelEncodeError::Encoding)?,
        steps,
        parameters,
        output_prefix: value
            .output_prefix
            .as_utf8()
            .ok_or(ModelEncodeError::Encoding)?,
        created_at: job.created_at.model_isoformat(),
        claimed_at: job
            .claimed_at
            .map(cannery_core::timestamps::Timestamp::model_isoformat),
        claimed_by: job.claimed_by_service.map(|v| v.to_string()),
        claimed_by_user: job.claimed_by_user.map(|v| v.to_string()),
        via_client: convert(&job.via_client)?,
        deadline: job
            .deadline
            .map(cannery_core::timestamps::Timestamp::model_isoformat),
        finished_at: job
            .finished_at
            .map(cannery_core::timestamps::Timestamp::model_isoformat),
        lease_generation: i64::from(job.lease_generation),
        lease_expires_at: job
            .lease_expires_at
            .map(cannery_core::timestamps::Timestamp::model_isoformat),
        error_step: convert(&job.error_step)?,
        error_code: convert(&job.error_code)?,
        error_reason: convert(&job.error_reason)?,
        logs,
        // A document job's output is its write-up.
        writeup: if job.phase == cannery_jobs::repo::Phase::Document {
            verification.clone().map(|document| WriteupDocument {
                front_matter: document.front_matter,
                body_markdown: document.body_markdown,
            })
        } else {
            None
        },
        verification: if job.phase == cannery_jobs::repo::Phase::Document {
            None
        } else {
            verification
        },
        outputs: outputs
            .iter()
            .map(crate::attempt_read_wire::artifact_model)
            .collect::<Result<_>>()?,
    })
}
pub(crate) fn page(items: Vec<Vec<u8>>, next: Option<cannery_core::ids::JobId>) -> Result<Vec<u8>> {
    encode(&Page_JobOut_UUID_ {
        items: items
            .into_iter()
            .map(|bytes| decode(&bytes))
            .collect::<Result<_>>()?,
        next_before: next.map(|v| v.to_string()),
    })
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
