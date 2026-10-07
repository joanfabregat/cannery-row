//! Track responses use the public serde DTOs and domain adapters.
use crate::api_models::{
    HistoryEvent, Page_HistoryEvent_int_, Page_TrackOut_str_, TrackMode, TrackOut,
};
use cannery_core::json::{
    Document, DocumentBuilder, Node,
    model::{self, ModelEncodeError},
};
use cannery_tracks::repo::Track;
#[derive(Clone, Copy, Debug)]
pub struct ResponseContext {
    pub inferred_nesting_budget: usize,
}
fn mapping<T: serde::de::DeserializeOwned>(
    value: Option<&Document>,
    context: ResponseContext,
) -> Result<Option<T>, ModelEncodeError> {
    value
        .filter(|d| !matches!(d.node(d.root()), Some(Node::Null)))
        .map(|d| {
            serde_json::from_slice(&model::encode_model_mapping(
                d,
                d.root(),
                context.inferred_nesting_budget,
            )?)
            .map_err(|_| ModelEncodeError::InvalidNode)
        })
        .transpose()
}
fn track(row: &Track, context: ResponseContext) -> Result<TrackOut, ModelEncodeError> {
    Ok(TrackOut {
        id: row.id.to_string(),
        slug: row.slug.as_utf8().ok_or(ModelEncodeError::Encoding)?,
        title: row.title.as_utf8().ok_or(ModelEncodeError::Encoding)?,
        description: row
            .description
            .as_utf8()
            .ok_or(ModelEncodeError::Encoding)?,
        producer: mapping(row.producer.as_ref(), context)?,
        mode: match row.mode.as_str() {
            "agent" => TrackMode::Agent,
            "workflow" => TrackMode::Workflow,
            _ => return Err(ModelEncodeError::InvalidNode),
        },
        workflow: mapping(row.workflow.as_ref(), context)?,
        state: row.state.as_str().to_owned(),
        revision: i64::from(row.revision),
        created_at: crate::timestamps::public_timestamp(row.created_at),
        updated_at: crate::timestamps::public_timestamp(row.updated_at),
    })
}
/// # Errors
/// Rejects stored values that cannot be represented by the response contract.
pub fn track_bytes(row: &Track, context: ResponseContext) -> Result<Vec<u8>, ModelEncodeError> {
    serde_json::to_vec(&track(row, context)?).map_err(|_| ModelEncodeError::Encoding)
}
/// # Errors
/// Returns the first item conversion or serialization error.
pub fn page_bytes(
    rows: &[Track],
    next: Option<&String>,
    context: ResponseContext,
) -> Result<Vec<u8>, ModelEncodeError> {
    serde_json::to_vec(&Page_TrackOut_str_ {
        items: rows
            .iter()
            .map(|row| track(row, context))
            .collect::<Result<_, _>>()?,
        next_before: next
            .map(|value| value.as_utf8().ok_or(ModelEncodeError::Encoding))
            .transpose()?,
    })
    .map_err(|_| ModelEncodeError::Encoding)
}
pub(crate) fn history_bytes(
    rows: &[crate::track_audit::Event],
    next: Option<i64>,
    context: ResponseContext,
) -> Result<Vec<u8>, ModelEncodeError> {
    let items = rows
        .iter()
        .map(|row| {
            Ok(HistoryEvent {
                seq: row.seq,
                occurred_at: crate::timestamps::public_timestamp(row.occurred_at),
                action: row.action.clone(),
                actor_kind: row.actor_kind.clone(),
                actor_user_id: row.actor_user_id.map(|id| id.to_string()),
                actor_service_id: row.actor_service_id.map(|id| id.to_string()),
                via_channel: row.via_channel.clone(),
                via_client: row.via_client.clone(),
                prior_state: serde_json::from_slice(&model::encode_inferred(
                    &row.prior_state,
                    row.prior_state.root(),
                    context.inferred_nesting_budget,
                )?)
                .map_err(|_| ModelEncodeError::InvalidNode)?,
                new_state: serde_json::from_slice(&model::encode_inferred(
                    &row.new_state,
                    row.new_state.root(),
                    context.inferred_nesting_budget,
                )?)
                .map_err(|_| ModelEncodeError::InvalidNode)?,
                reason: row.reason.clone(),
            })
        })
        .collect::<Result<Vec<_>, ModelEncodeError>>()?;
    serde_json::to_vec(&Page_HistoryEvent_int_ {
        items,
        next_before: next,
    })
    .map_err(|_| ModelEncodeError::Encoding)
}
pub(crate) fn snapshot(
    row: &Track,
    fields: &[&str],
) -> Result<Document, cannery_core::json::BuildError> {
    let mut b = DocumentBuilder::new();
    let mut result = vec![];
    for field in fields {
        let id = match *field {
            "title" => b.push(Node::String(row.title.clone())),
            "description" => b.push(Node::String(row.description.clone())),
            "state" => b.push(Node::String(String::from(row.state.as_str()))),
            "mode" => b.push(Node::String(String::from(row.mode.as_str()))),
            "revision" => b.push(Node::Integer(row.revision.into())),
            "producer" => match row.producer.as_ref() {
                Some(d) => b.import(d, d.root()),
                None => b.push(Node::Null),
            },
            "workflow" => match row.workflow.as_ref() {
                Some(d) => b.import(d, d.root()),
                None => b.push(Node::Null),
            },
            _ => b.push(Node::Null),
        }?;
        result.push((String::from(*field), id));
    }
    let root = b.push(Node::Object(result))?;
    b.finish(root)
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
