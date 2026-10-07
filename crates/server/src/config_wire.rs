//! Configuration responses serialize the same typed DTOs that define `OpenAPI`.
use crate::api_models::{ConfigOut, Page_ConfigOut_int_};
use cannery_core::json::model::{ModelEncodeError, encode_model_mapping};
use cannery_research::config_repo::ConfigRevision;

#[derive(Clone, Copy, Debug)]
pub struct ResponseContext {
    pub inferred_nesting_budget: usize,
}

fn config(row: &ConfigRevision, context: ResponseContext) -> Result<ConfigOut, ModelEncodeError> {
    Ok(ConfigOut {
        kind: row.kind.as_str().to_owned(),
        revision: i64::from(row.revision),
        science_revision: row.science_revision.map(i64::from),
        content: serde_json::from_slice(&encode_model_mapping(
            &row.content,
            row.content.root(),
            context.inferred_nesting_budget,
        )?)
        .map_err(|_| ModelEncodeError::InvalidNode)?,
        created_by: row.created_by.to_string(),
        created_at: crate::timestamps::public_timestamp(row.created_at),
    })
}

/// # Errors
/// Rejects stored documents that cannot be represented by the response contract.
pub fn config_bytes(
    row: &ConfigRevision,
    context: ResponseContext,
) -> Result<Vec<u8>, ModelEncodeError> {
    serde_json::to_vec(&config(row, context)?).map_err(|_| ModelEncodeError::Encoding)
}

/// # Errors
/// Returns the first item conversion or serialization error.
pub fn page_bytes(
    rows: &[ConfigRevision],
    next_before: Option<i32>,
    context: ResponseContext,
) -> Result<Vec<u8>, ModelEncodeError> {
    serde_json::to_vec(&Page_ConfigOut_int_ {
        items: rows
            .iter()
            .map(|row| config(row, context))
            .collect::<Result<_, _>>()?,
        next_before: next_before.map(i64::from),
    })
    .map_err(|_| ModelEncodeError::Encoding)
}
