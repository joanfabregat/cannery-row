//! Python datetime serialization keeps six fractional digits when nonzero.

use chrono::{DateTime, Datelike, FixedOffset};
use serde::{Serialize, Serializer};
use sqlx::{
    Decode, Encode, Postgres, Type,
    encode::IsNull,
    error::BoxDynError,
    postgres::{PgArgumentBuffer, PgHasArrayType, PgTypeInfo, PgValueFormat, PgValueRef},
};
use std::{fmt, str::FromStr};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Timestamp(pub DateTime<FixedOffset>);

// Inclusive microseconds from PostgreSQL's 2000-01-01 epoch to Python's
// datetime.min/max. Check before Chrono arithmetic: PostgreSQL infinity and
// sufficiently distant finite dates can overflow the upstream decoder.
const MIN_PYTHON_MICROSECONDS: i64 = -63_082_281_600_000_000;
const MAX_PYTHON_MICROSECONDS: i64 = 252_455_615_999_999_999;

#[derive(Debug, thiserror::Error)]
#[error("timestamp cannot be represented by Python datetime")]
struct TimestampDecodeError;

impl Type<Postgres> for Timestamp {
    fn type_info() -> PgTypeInfo {
        <DateTime<FixedOffset> as Type<Postgres>>::type_info()
    }

    fn compatible(ty: &PgTypeInfo) -> bool {
        <DateTime<FixedOffset> as Type<Postgres>>::compatible(ty)
    }
}

impl PgHasArrayType for Timestamp {
    fn array_type_info() -> PgTypeInfo {
        <DateTime<FixedOffset> as PgHasArrayType>::array_type_info()
    }

    fn array_compatible(ty: &PgTypeInfo) -> bool {
        <DateTime<FixedOffset> as PgHasArrayType>::array_compatible(ty)
    }
}

impl Encode<'_, Postgres> for Timestamp {
    fn encode_by_ref(&self, buffer: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        <DateTime<FixedOffset> as Encode<Postgres>>::encode_by_ref(&self.0, buffer)
    }

    fn produces(&self) -> Option<PgTypeInfo> {
        <DateTime<FixedOffset> as Encode<Postgres>>::produces(&self.0)
    }

    fn size_hint(&self) -> usize {
        <DateTime<FixedOffset> as Encode<Postgres>>::size_hint(&self.0)
    }
}

impl<'r> Decode<'r, Postgres> for Timestamp {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        if value.format() == PgValueFormat::Binary {
            let microseconds = <i64 as Decode<Postgres>>::decode(value.clone())
                .map_err(|_| TimestampDecodeError)?;
            if !(MIN_PYTHON_MICROSECONDS..=MAX_PYTHON_MICROSECONDS).contains(&microseconds) {
                return Err(TimestampDecodeError.into());
            }
        }
        let datetime = <DateTime<FixedOffset> as Decode<Postgres>>::decode(value)
            .map_err(|_| TimestampDecodeError)?;
        if !(1..=9999).contains(&datetime.year()) {
            return Err(TimestampDecodeError.into());
        }
        Ok(Self(datetime))
    }
}

impl Timestamp {
    /// Frozen typed-model timestamp text: UTC uses Z and offset seconds truncate.
    /// Local clock fields and six nonzero fractional digits remain unchanged.
    #[must_use]
    pub fn model_isoformat(self) -> String {
        use std::fmt::Write;
        let mut text = self.0.format("%Y-%m-%dT%H:%M:%S").to_string();
        let micros = self.0.timestamp_subsec_micros();
        if micros != 0 {
            let _ = write!(text, ".{micros:06}");
        }
        let seconds = self.0.offset().local_minus_utc();
        if seconds == 0 {
            text.push('Z');
        } else {
            let sign = if seconds < 0 { '-' } else { '+' };
            let minutes = seconds.unsigned_abs() / 60;
            let _ = write!(text, "{sign}{:02}:{:02}", minutes / 60, minutes % 60);
        }
        text
    }

    #[must_use]
    pub fn isoformat(self) -> String {
        let mut text = self.0.format("%Y-%m-%dT%H:%M:%S").to_string();
        let microseconds = self.0.timestamp_subsec_micros();
        if microseconds != 0 {
            use std::fmt::Write;
            // Writing to a String cannot fail.
            let _ = write!(text, ".{microseconds:06}");
        }
        let offset_format = if self.0.offset().local_minus_utc() % 60 == 0 {
            "%:z"
        } else {
            "%::z"
        };
        text.push_str(&self.0.format(offset_format).to_string());
        text
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.isoformat())
    }
}

impl FromStr for Timestamp {
    type Err = chrono::ParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        DateTime::parse_from_rfc3339(text)
            .or_else(|_| DateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S%.f%#z"))
            .map(Self)
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.isoformat())
    }
}
