//! Checked PostgreSQL integer arguments use `SQLx`'s native BIGINT encoder.
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use sqlx::{
    Encode, Postgres, Type,
    encode::IsNull,
    error::BoxDynError,
    postgres::{PgArgumentBuffer, PgTypeInfo},
};
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum IntegerError {
    #[error("integer is outside PostgreSQL BIGINT range")]
    IntegerEncoding,
}
#[derive(Clone, Copy, Debug)]
pub struct Integer(i64);
impl Integer {
    /// # Errors
    /// Rejects values outside PostgreSQL's signed 64-bit integer range.
    pub fn new(value: &BigInt) -> Result<Self, IntegerError> {
        value
            .to_i64()
            .map(Self)
            .ok_or(IntegerError::IntegerEncoding)
    }
    #[must_use]
    pub fn hint(&self) -> PgTypeInfo {
        i64::type_info()
    }
    #[must_use]
    pub const fn oid(&self) -> u32 {
        20
    }
}
impl Type<Postgres> for Integer {
    fn type_info() -> PgTypeInfo {
        i64::type_info()
    }
}
impl Encode<'_, Postgres> for Integer {
    fn encode_by_ref(&self, buffer: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        self.0.encode_by_ref(buffer)
    }
    fn size_hint(&self) -> usize {
        8
    }
}
pub type PgInteger = Integer;
