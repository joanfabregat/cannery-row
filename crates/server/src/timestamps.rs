//! Datetime formatting for fields projected through Python Pydantic HTTP models.
use cannery_core::timestamps::Timestamp;

/// Match frozen Pydantic's typed datetime serialization.
/// Zero UTC offset becomes `Z`; nonzero offsets keep their sign and truncate
/// offset seconds to minutes without changing the displayed local clock.
/// Use core `Timestamp::isoformat` for plain JSON datetime strings and audit state.
#[must_use]
pub fn public_timestamp(value: Timestamp) -> String {
    value.model_isoformat()
}

/// Preserve nullability of source HTTP datetime fields.
#[must_use]
pub fn optional_timestamp(value: Option<Timestamp>) -> Option<String> {
    value.map(public_timestamp)
}
