//! Lossless JSONB wire encoding and checked decoding profiles.
use crate::{
    model::{JsonContext, StoredJson},
    repo::AttemptError,
};
use cannery_core::json::{self, Document};
use sqlx::{
    Decode, Encode, Postgres, Type,
    encode::IsNull,
    error::BoxDynError,
    postgres::{PgArgumentBuffer, PgTypeInfo, PgValueFormat, PgValueRef},
};
use std::sync::Arc;
pub(crate) struct JsonbText(pub(crate) String);
impl Type<Postgres> for JsonbText {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_name("jsonb")
    }
}
impl Encode<'_, Postgres> for JsonbText {
    fn encode_by_ref(&self, buffer: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        buffer.push(1);
        buffer.extend_from_slice(self.0.as_bytes());
        Ok(IsNull::No)
    }
}
impl Decode<'_, Postgres> for JsonbText {
    fn decode(value: PgValueRef<'_>) -> Result<Self, BoxDynError> {
        let bytes = value.as_bytes()?;
        let bytes = if value.format() == PgValueFormat::Binary {
            if bytes.first() != Some(&1) {
                return Err("invalid JSONB wire version".into());
            }
            &bytes[1..]
        } else {
            bytes
        };
        let text = std::str::from_utf8(bytes).map_err(|_| "invalid JSONB text encoding")?;
        Ok(Self(text.to_owned()))
    }
}
pub(crate) fn parameter(value: &Document, context: JsonContext) -> Result<JsonbText, AttemptError> {
    json::encode_ascii_pretty(value, context.encode_nesting_budget)
        .map(JsonbText)
        .map_err(|error| match error {
            json::EncodeError::IntegerLimit => AttemptError::SourceIntegerLimit,
            json::EncodeError::Recursion => AttemptError::SourceRecursion,
            json::EncodeError::InvalidNode => AttemptError::CorruptData,
        })
}
pub(crate) fn stored(
    value: Option<&str>,
    context: JsonContext,
) -> Result<StoredJson, AttemptError> {
    value.map_or(Ok(StoredJson::SqlNull), |value| {
        json::decode(value.as_bytes(), context.decode_nesting_budget)
            .map(|value| StoredJson::Value(Arc::new(value)))
            .map_err(|error| match error {
                json::DecodeError::IntegerLimit => AttemptError::SourceIntegerLimit,
                json::DecodeError::Recursion => AttemptError::SourceRecursion,
                json::DecodeError::Syntax { .. } | json::DecodeError::Encoding => {
                    AttemptError::CorruptData
                }
            })
    })
}
impl std::fmt::Debug for JsonbText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JsonbText([redacted])")
    }
}
