//! Shared binary integer adaptation, retaining this domain's null type hint.
use crate::repo::SearchError;
use num_bigint::BigInt;
use sqlx::{
    Encode, Postgres, Type,
    encode::IsNull,
    error::BoxDynError,
    postgres::{PgArgumentBuffer, PgTypeInfo},
};
pub(crate) struct Integer(cannery_core::pg_integer::Integer);
impl Integer {
    pub(crate) fn new(value: &BigInt) -> Result<Self, SearchError> {
        cannery_core::pg_integer::Integer::new(value)
            .map(Self)
            .map_err(|_| SearchError::Database { sqlstate: None })
    }
}
impl Type<Postgres> for Integer {
    fn type_info() -> PgTypeInfo {
        i64::type_info()
    }
}
impl Encode<'_, Postgres> for Integer {
    fn produces(&self) -> Option<PgTypeInfo> {
        Some(self.0.hint())
    }
    fn encode_by_ref(&self, buffer: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        self.0.encode_by_ref(buffer)
    }
    fn size_hint(&self) -> usize {
        self.0.size_hint()
    }
}
pub(crate) fn argument(value: Option<&BigInt>) -> Result<Option<Integer>, SearchError> {
    value.map(Integer::new).transpose()
}
