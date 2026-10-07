//! Python date boundary checked before Chrono's PostgreSQL epoch arithmetic.
use chrono::{Datelike, NaiveDate};
use sqlx::{
    Decode, Postgres, Type,
    error::BoxDynError,
    postgres::{PgTypeInfo, PgValueFormat, PgValueRef},
};
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct SourceDate(pub NaiveDate);
impl std::fmt::Debug for SourceDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceDate([redacted])")
    }
}
impl std::fmt::Display for SourceDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
#[derive(Debug, thiserror::Error)]
#[error("date cannot be represented by Python date")]
struct DateDecodeError;
impl Type<Postgres> for SourceDate {
    fn type_info() -> PgTypeInfo {
        NaiveDate::type_info()
    }
    fn compatible(ty: &PgTypeInfo) -> bool {
        NaiveDate::compatible(ty)
    }
}
impl<'r> Decode<'r, Postgres> for SourceDate {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        if value.format() == PgValueFormat::Binary {
            let days =
                <i32 as Decode<Postgres>>::decode(value.clone()).map_err(|_| DateDecodeError)?;
            if !(-730_119..=2_921_939).contains(&days) {
                return Err(DateDecodeError.into());
            }
        }
        let date = <NaiveDate as Decode<Postgres>>::decode(value).map_err(|_| DateDecodeError)?;
        if !(1..=9999).contains(&date.year()) {
            return Err(DateDecodeError.into());
        }
        Ok(Self(date))
    }
}
