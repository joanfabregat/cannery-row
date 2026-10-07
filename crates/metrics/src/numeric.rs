//! PostgreSQL NUMERIC wire codec preserving Decimal's coefficient and scale.
use num_bigint::{BigInt, Sign};
use num_traits::{Signed, Zero};
use sqlx::{
    Decode, Encode, Postgres, Type, TypeInfo,
    encode::IsNull,
    error::BoxDynError,
    postgres::{PgArgumentBuffer, PgTypeInfo, PgValueFormat, PgValueRef},
};
use std::{fmt, str::FromStr};

/// Decimal values retain their scale; PostgreSQL normalizes signed zero.
#[derive(Clone, PartialEq, Eq)]
pub enum PgNumeric {
    Finite { coefficient: BigInt, scale: u16 },
    NaN,
    PositiveInfinity,
    NegativeInfinity,
}
impl fmt::Debug for PgNumeric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PgNumeric([redacted])")
    }
}
#[derive(Debug, thiserror::Error)]
#[error("invalid PostgreSQL numeric representation")]
pub struct NumericError;
impl PgNumeric {
    /// Explicit value projection for callers; Debug never exposes its contents.
    #[must_use]
    pub fn decimal_text(&self) -> String {
        match self {
            Self::NaN => "NaN".into(),
            Self::PositiveInfinity => "Infinity".into(),
            Self::NegativeInfinity => "-Infinity".into(),
            Self::Finite { coefficient, scale } => {
                let mut digits = coefficient.abs().to_string();
                let scale = usize::from(*scale);
                if scale > 0 {
                    if digits.len() <= scale {
                        digits.insert_str(0, &"0".repeat(scale + 1 - digits.len()));
                    }
                    digits.insert(digits.len() - scale, '.');
                }
                if coefficient.sign() == Sign::Minus {
                    digits.insert(0, '-');
                }
                digits
            }
        }
    }
    fn binary(bytes: &[u8]) -> Result<Self, NumericError> {
        let header: &[u8; 8] = bytes
            .get(..8)
            .ok_or(NumericError)?
            .try_into()
            .map_err(|_| NumericError)?;
        let count = usize::from(u16::from_be_bytes([header[0], header[1]]));
        if bytes.len() != 8 + 2 * count {
            return Err(NumericError);
        }
        let weight = i16::from_be_bytes([header[2], header[3]]);
        let sign = u16::from_be_bytes([header[4], header[5]]);
        let scale = u16::from_be_bytes([header[6], header[7]]);
        match sign {
            0xC000 | 0xD000 | 0xF000 if count == 0 => {
                return Ok(match sign {
                    0xC000 => Self::NaN,
                    0xD000 => Self::PositiveInfinity,
                    _ => Self::NegativeInfinity,
                });
            }
            0 | 0x4000 if scale <= 0x3fff => {}
            _ => return Err(NumericError),
        }
        let mut coefficient = BigInt::zero();
        for pair in bytes[8..].as_chunks::<2>().0 {
            let digit = u16::from_be_bytes([pair[0], pair[1]]);
            if digit >= 10000 {
                return Err(NumericError);
            }
            coefficient = coefficient * 10000u16 + digit;
        }
        let count = i32::try_from(count).map_err(|_| NumericError)?;
        let exponent = (i32::from(weight) + 1 - count) * 4 + i32::from(scale);
        if exponent >= 0 {
            coefficient *=
                BigInt::from(10u8).pow(u32::try_from(exponent).map_err(|_| NumericError)?);
        } else {
            let divisor = BigInt::from(10u8).pow(exponent.unsigned_abs());
            if &coefficient % &divisor != BigInt::zero() {
                return Err(NumericError);
            }
            coefficient /= divisor;
        }
        if sign == 0x4000 {
            coefficient = -coefficient;
        }
        Ok(Self::Finite { coefficient, scale })
    }
    fn wire(&self) -> Result<Vec<u8>, NumericError> {
        let (sign, weight, scale, digits) = match self {
            Self::NaN => (0xC000u16, 0i16, 0u16, Vec::new()),
            Self::PositiveInfinity => (0xD000, 0, 0, Vec::new()),
            Self::NegativeInfinity => (0xF000, 0, 0, Vec::new()),
            Self::Finite { coefficient, scale } => {
                if *scale > 0x3fff {
                    return Err(NumericError);
                }
                let sign = if coefficient.sign() == Sign::Minus {
                    0x4000
                } else {
                    0
                };
                if coefficient.is_zero() {
                    (sign, 0, *scale, Vec::new())
                } else {
                    let padding = (4 - usize::from(*scale) % 4) % 4;
                    let mut text = coefficient.abs().to_string();
                    text.push_str(&"0".repeat(padding));
                    let left = (4 - text.len() % 4) % 4;
                    text.insert_str(0, &"0".repeat(left));
                    let mut digits = text
                        .as_bytes()
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|chunk| chunk.iter().fold(0u16, |n, b| n * 10 + u16::from(b - b'0')))
                        .collect::<Vec<_>>();
                    let groups = i32::try_from(digits.len()).map_err(|_| NumericError)?;
                    let fractional = i32::try_from((usize::from(*scale) + padding) / 4)
                        .map_err(|_| NumericError)?;
                    let weight =
                        i16::try_from(groups - fractional - 1).map_err(|_| NumericError)?;
                    while digits.last() == Some(&0) {
                        digits.pop();
                    }
                    (sign, weight, *scale, digits)
                }
            }
        };
        let count = u16::try_from(digits.len()).map_err(|_| NumericError)?;
        let mut bytes = Vec::with_capacity(8 + 2 * digits.len());
        bytes.extend_from_slice(&count.to_be_bytes());
        bytes.extend_from_slice(&weight.to_be_bytes());
        bytes.extend_from_slice(&sign.to_be_bytes());
        bytes.extend_from_slice(&scale.to_be_bytes());
        for digit in digits {
            bytes.extend_from_slice(&digit.to_be_bytes());
        }
        Ok(bytes)
    }
}
impl FromStr for PgNumeric {
    type Err = NumericError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "NaN" => return Ok(Self::NaN),
            "Infinity" => return Ok(Self::PositiveInfinity),
            "-Infinity" => return Ok(Self::NegativeInfinity),
            _ => {}
        }
        let (negative, unsigned) = text.strip_prefix('-').map_or((false, text), |s| (true, s));
        let unsigned = unsigned.strip_prefix('+').unwrap_or(unsigned);
        let mut parts = unsigned.split('.');
        let whole = parts.next().ok_or(NumericError)?;
        let fraction = parts.next().unwrap_or("");
        if parts.next().is_some()
            || whole.is_empty()
            || !whole
                .bytes()
                .chain(fraction.bytes())
                .all(|b| b.is_ascii_digit())
        {
            return Err(NumericError);
        }
        let scale = u16::try_from(fraction.len()).map_err(|_| NumericError)?;
        if scale > 0x3fff {
            return Err(NumericError);
        }
        let mut coefficient =
            BigInt::parse_bytes(format!("{whole}{fraction}").as_bytes(), 10).ok_or(NumericError)?;
        if negative {
            coefficient = -coefficient;
        }
        Ok(Self::Finite { coefficient, scale })
    }
}
impl Type<Postgres> for PgNumeric {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_name("NUMERIC")
    }
    fn compatible(ty: &PgTypeInfo) -> bool {
        ty.name() == "NUMERIC"
    }
}
impl<'r> Decode<'r, Postgres> for PgNumeric {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        match value.format() {
            PgValueFormat::Binary => Self::binary(value.as_bytes()?).map_err(Into::into),
            PgValueFormat::Text => value.as_str()?.parse().map_err(Into::into),
        }
    }
}
impl Encode<'_, Postgres> for PgNumeric {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        buf.extend_from_slice(&self.wire()?);
        Ok(IsNull::No)
    }
}
