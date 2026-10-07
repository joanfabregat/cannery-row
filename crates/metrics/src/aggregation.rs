//! Metric aggregation and exact Decimal-to-integer response projections.
use crate::numeric::PgNumeric;
use num_bigint::BigInt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Aggregation {
    Median,
    Sum,
    Min,
    Max,
    Count,
    Mean,
}
impl Aggregation {
    /// The source uses its mean branch for every unrecognized name.
    #[must_use]
    pub fn from_name(name: &String) -> Self {
        for (text, kind) in [
            ("median", Self::Median),
            ("sum", Self::Sum),
            ("min", Self::Min),
            ("max", Self::Max),
            ("count", Self::Count),
        ] {
            if name.equals_utf8(text) {
                return kind;
            }
        }
        Self::Mean
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ProjectionError {
    #[error("metric projection has an invalid numeric operation")]
    Value,
    #[error("metric projection exceeds the source numeric range")]
    Overflow,
}

/// Preserve Decimal integer truncation without float conversion or digit limits.
/// # Errors
/// NaN is Value; either infinity is Overflow, as in Python's int(Decimal).
pub fn sample_count(value: Option<&PgNumeric>) -> Result<Option<BigInt>, ProjectionError> {
    match value {
        None => Ok(None),
        Some(PgNumeric::Finite { coefficient, scale }) => Ok(Some(
            coefficient / BigInt::from(10u8).pow(u32::from(*scale)),
        )),
        Some(PgNumeric::NaN) => Err(ProjectionError::Value),
        Some(PgNumeric::PositiveInfinity | PgNumeric::NegativeInfinity) => {
            Err(ProjectionError::Overflow)
        }
    }
}

/// Aggregate finite samples using native Rust arithmetic and ordering.
/// # Errors
/// Rejects nonfinite samples and overflowing results.
#[allow(clippy::cast_precision_loss)]
pub fn aggregate(values: &[f64], method: Aggregation) -> Result<Option<f64>, ProjectionError> {
    if values.is_empty() {
        return Ok(None);
    }
    if values.iter().any(|value| !value.is_finite()) {
        return Err(ProjectionError::Value);
    }
    let count = values.len() as f64;
    let result = match method {
        Aggregation::Count => count,
        Aggregation::Sum => values.iter().sum(),
        Aggregation::Mean => values.iter().map(|value| value / count).sum(),
        Aggregation::Min => values.iter().copied().fold(f64::INFINITY, f64::min),
        Aggregation::Max => values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        Aggregation::Median => {
            let mut sorted = values.to_vec();
            sorted.sort_by(f64::total_cmp);
            let middle = sorted.len() / 2;
            if sorted.len().is_multiple_of(2) {
                sorted[middle - 1].midpoint(sorted[middle])
            } else {
                sorted[middle]
            }
        }
    };
    if result.is_finite() {
        Ok(Some(result))
    } else {
        Err(ProjectionError::Overflow)
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use serde_json::{Value, json};
    #[test]
    fn native_statistics_cover_finite_samples_and_bounds() {
        let values = [1.0, 4.0, 2.0, 3.0];
        for (method, expected) in [
            (Aggregation::Sum, 10.0),
            (Aggregation::Mean, 2.5),
            (Aggregation::Median, 2.5),
            (Aggregation::Min, 1.0),
            (Aggregation::Max, 4.0),
            (Aggregation::Count, 4.0),
        ] {
            assert_eq!(aggregate(&values, method), Ok(Some(expected)));
        }
        assert_eq!(aggregate(&[], Aggregation::Mean), Ok(None));
        assert_eq!(
            aggregate(&[f64::NAN], Aggregation::Sum),
            Err(ProjectionError::Value)
        );
        assert_eq!(
            aggregate(&[f64::MAX, f64::MAX], Aggregation::Sum),
            Err(ProjectionError::Overflow)
        );
        assert_eq!(
            aggregate(&[f64::MAX, f64::MAX], Aggregation::Mean),
            Ok(Some(f64::MAX))
        );
        assert_eq!(
            aggregate(&[f64::MAX, f64::MAX], Aggregation::Median),
            Ok(Some(f64::MAX))
        );
    }
    #[test]
    fn actual_decimal_sample_counts_preserve_integer_truncation() {
        let fixture: Value = serde_json::from_str(runtime_reference!(
            "/tests/fixtures/aggregation_reference.json"
        ))
        .expect("fixture JSON");
        let cases = fixture["sample_counts"].as_array().expect("cases");
        assert_eq!(cases.len(), 38);
        for (index, case) in cases.iter().enumerate() {
            let input = &case["native_input"];
            let value = if input.is_null() {
                None
            } else if let Some(text) = input["special"].as_str() {
                Some(
                    text.parse::<PgNumeric>()
                        .expect("PG-compatible special value"),
                )
            } else {
                Some(PgNumeric::Finite {
                    coefficient: BigInt::parse_bytes(
                        input["coefficient_hex"]
                            .as_str()
                            .expect("coefficient")
                            .as_bytes(),
                        16,
                    )
                    .expect("coefficient hex"),
                    scale: u16::try_from(input["scale"].as_u64().expect("scale"))
                        .expect("PG-compatible scale"),
                })
            };
            let actual = match sample_count(value.as_ref()) {
                Ok(None) => json!({"null":true}),
                Ok(Some(value)) => json!({"integer_hex":value.to_str_radix(16)}),
                Err(ProjectionError::Value) => json!({"error":"ValueError"}),
                Err(ProjectionError::Overflow) => json!({"error":"OverflowError"}),
            };
            assert_eq!(actual, case["output"], "sample count {index}");
        }
    }
}
