//! Source scalar OIDs and binary integer adaptation, including trailing zero groups.
use crate::repo::AttemptError;
use num_bigint::BigInt;
use sqlx::{
    Encode, Postgres, Type,
    encode::IsNull,
    error::BoxDynError,
    postgres::{PgArgumentBuffer, PgTypeInfo},
};
use uuid::Uuid;
pub(crate) enum Value {
    Uuid(Option<Uuid>),
    Text(Option<String>),
    Integer(Option<BigInt>),
    Json(Option<JsonParameter>),
    Bytes(Option<Vec<u8>>),
    Bool(Option<bool>),
    TextList(Option<Vec<String>>),
    UuidList(Option<Vec<Uuid>>),
}
impl Value {
    pub(crate) fn oid(&self) -> u32 {
        match self {
            Self::Uuid(Some(_)) => 2950,
            Self::Integer(_) => 20,
            Self::Json(Some(_)) => 3802,
            Self::Bytes(Some(_)) => 17,
            Self::Bool(Some(_)) => 16,
            Self::TextList(Some(v)) if !v.is_empty() => 1009,
            Self::UuidList(Some(v)) if !v.is_empty() => 2951,
            _ => 0,
        }
    }

    pub(crate) fn integer(value: Option<&BigInt>) -> Self {
        Self::Integer(value.cloned())
    }
    pub(crate) fn text(value: Option<&str>) -> Self {
        Self::Text(value.map(str::to_owned))
    }
    pub(crate) fn uuid(value: Uuid) -> Self {
        Self::Uuid(Some(value))
    }
}

pub(crate) struct JsonParameter {
    pub document: std::sync::Arc<cannery_core::json::Document>,
    pub context: crate::model::JsonContext,
}
pub(crate) struct Argument {
    oid: u32,
    bytes: Option<Vec<u8>>,
}
impl Argument {
    pub(crate) fn new(value: &Value) -> Result<Self, AttemptError> {
        let oid = value.oid();
        let mut buffer = PgArgumentBuffer::default();
        macro_rules! encode {
            ($value:expr) => {
                $value
                    .encode_by_ref(&mut buffer)
                    .map_err(|_| AttemptError::Encoding)?
            };
        }
        let null = match value {
            Value::Uuid(v) => encode!(v),
            Value::Text(v) => {
                if v.as_ref().is_some_and(|value| value.contains('\0')) {
                    return Err(AttemptError::Database { sqlstate: None });
                }
                encode!(v)
            }
            Value::Integer(v) => encode!(
                v.as_ref()
                    .map(|v| cannery_core::pg_integer::Integer::new(v)
                        .map_err(|_| AttemptError::Database { sqlstate: None }))
                    .transpose()?
            ),
            Value::Json(v) => encode!(
                v.as_ref()
                    .map(|v| crate::wire::parameter(&v.document, v.context))
                    .transpose()?
            ),
            Value::Bytes(v) => encode!(v),
            Value::Bool(v) => encode!(v),
            Value::TextList(v) => {
                if v.as_ref()
                    .is_some_and(|v| v.iter().any(|v| v.contains('\0')))
                {
                    return Err(AttemptError::Database { sqlstate: None });
                }
                encode!(v)
            }
            Value::UuidList(v) => encode!(v),
        };
        Ok(Self {
            oid,
            bytes: if matches!(null, IsNull::Yes) {
                None
            } else {
                Some(buffer.to_vec())
            },
        })
    }
    pub(crate) fn hint(&self) -> PgTypeInfo {
        PgTypeInfo::with_oid(sqlx::postgres::types::Oid(self.oid))
    }
}
impl Type<Postgres> for Argument {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_oid(sqlx::postgres::types::Oid(0))
    }
}
impl Encode<'_, Postgres> for Argument {
    fn produces(&self) -> Option<PgTypeInfo> {
        Some(self.hint())
    }
    fn encode_by_ref(&self, b: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        if let Some(bytes) = &self.bytes {
            b.extend_from_slice(bytes);
            Ok(IsNull::No)
        } else {
            Ok(IsNull::Yes)
        }
    }
}
