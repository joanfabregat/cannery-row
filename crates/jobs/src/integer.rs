//! Shared binary NUMERIC payload with the existing jobs argument type hint.
use crate::repo::JobError;
use num_bigint::BigInt;
use sqlx::{
    Encode, Postgres, Type,
    encode::IsNull,
    error::BoxDynError,
    postgres::{PgArgumentBuffer, PgTypeInfo},
};
pub(crate) struct PgInteger(cannery_core::pg_integer::PgInteger);
impl PgInteger {
    pub(crate) fn new(value: &BigInt) -> Result<Self, JobError> {
        cannery_core::pg_integer::PgInteger::new(value)
            .map(Self)
            .map_err(|_| JobError::Database { sqlstate: None })
    }
}
impl Type<Postgres> for PgInteger {
    fn type_info() -> PgTypeInfo {
        cannery_core::pg_integer::PgInteger::type_info()
    }
}
impl Encode<'_, Postgres> for PgInteger {
    fn encode_by_ref(&self, buffer: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        self.0.encode_by_ref(buffer)
    }
    fn size_hint(&self) -> usize {
        self.0.size_hint()
    }
}
