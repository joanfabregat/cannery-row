//! Typed chart responses over complete measurement records; callers supply model limits.
use crate::{aggregation, repo::Point};
use cannery_core::{
    json::{Document, Node, model},
    timestamps::Timestamp,
};
use num_bigint::BigInt;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("metric response fails source model validation")]
    Validation,
    #[error("metric projection has an invalid operation")]
    Value,
    #[error("metric projection exceeds the source numeric range")]
    Overflow,
    #[error("metric projection uses an unsupported source attribute")]
    Attribute,
    #[error("metric projection uses an unhashable or unordered source value")]
    Type,
    #[error("metric projection requires an absent source key")]
    Key,
    #[error("metric source string rendering exceeds its recursion context")]
    Recursion,
    #[error("metric model serialization failed")]
    Model(#[from] model::ModelEncodeError),
}
impl From<aggregation::ProjectionError> for Error {
    fn from(error: aggregation::ProjectionError) -> Self {
        match error {
            aggregation::ProjectionError::Value => Self::Value,
            aggregation::ProjectionError::Overflow => Self::Overflow,
        }
    }
}
/// Explicit inferred-value profile; typed model/dictionary wrappers consume no budget.
#[derive(Clone, Copy, Debug)]
pub struct Context {
    pub inferred_nesting_budget: usize,
}
#[derive(Clone, Copy)]
pub struct Reference<'a> {
    pub value: f64,
    pub label: &'a str,
    pub kind: ReferenceKind,
    pub reference: Option<&'a str>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceKind {
    Paper,
    Benchmark,
    PromotedAttempt,
    Baseline,
    Manual,
    Other,
}
impl ReferenceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Paper => "paper",
            Self::Benchmark => "benchmark",
            Self::PromotedAttempt => "promoted_attempt",
            Self::Baseline => "baseline",
            Self::Manual => "manual",
            Self::Other => "other",
        }
    }
}
impl TryFrom<&str> for ReferenceKind {
    type Error = Error;
    fn try_from(value: &str) -> Result<Self, Error> {
        match value {
            "paper" => Ok(Self::Paper),
            "benchmark" => Ok(Self::Benchmark),
            "promoted_attempt" => Ok(Self::PromotedAttempt),
            "baseline" => Ok(Self::Baseline),
            "manual" => Ok(Self::Manual),
            "other" => Ok(Self::Other),
            _ => Err(Error::Validation),
        }
    }
}
#[derive(Clone, Copy)]
pub struct Uncertainty<'a> {
    pub method: &'a str,
    pub lower: f64,
    pub upper: f64,
}
pub struct PointOut<'a> {
    pub point: &'a Point,
    pub sample_count: Option<BigInt>,
    pub reference: Option<Reference<'a>>,
    pub uncertainty: Option<Uncertainty<'a>>,
}
macro_rules! redacted {($($t:ty),+) => {$(impl std::fmt::Debug for $t {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.write_str("MetricProjection([redacted])")}})+};}
redacted!(Reference<'_>, Uncertainty<'_>, PointOut<'_>);

/// Complete verification reference, independently of whether an overlay is requested.
/// # Errors
/// Rejects an unknown reference kind exactly when all three required fields exist.
pub fn reference(point: &Point) -> Result<Option<Reference<'_>>, Error> {
    let (Some(value), Some(label), Some(kind)) = (
        point.reference_value,
        point.reference_label.as_deref(),
        point.reference_kind.as_deref(),
    ) else {
        return Ok(None);
    };
    let kind = ReferenceKind::try_from(kind)?;
    Ok(Some(Reference {
        value,
        label,
        kind,
        reference: point.reference_ref.as_deref(),
    }))
}
/// Use the verification report's reference rather than the measurement's informational control value.
/// # Errors
/// Reference validation occurs even when no baseline was requested, as in the source.
pub fn overlay<'a>(
    point: &'a Point,
    baseline: Option<&String>,
) -> Result<Option<Reference<'a>>, Error> {
    let found = reference(point)?;
    Ok(match (baseline, found) {
        (Some(baseline), Some(reference))
            if baseline.equals_utf8("control")
                || (reference.kind == ReferenceKind::Baseline
                    && reference
                        .reference
                        .is_some_and(|value| baseline.equals_utf8(value))) =>
        {
            Some(reference)
        }
        _ => None,
    })
}
fn dictionary(document: &Document, nullable: bool) -> Result<(), Error> {
    if matches!(document.node(document.root()), Some(Node::Object(_)))
        || (nullable && matches!(document.node(document.root()), Some(Node::Null)))
    {
        Ok(())
    } else {
        Err(Error::Validation)
    }
}
/// Construct the source model after argument evaluation, preserving Decimal errors first.
/// # Errors
/// Retains numeric exceptions separately from source model validation errors.
pub fn point_out(point: &Point) -> Result<PointOut<'_>, Error> {
    let uncertainty = match (
        point.uncertainty_method.as_deref(),
        point.uncertainty_lower,
        point.uncertainty_upper,
    ) {
        (Some(method), Some(lower), Some(upper)) => Some(Uncertainty {
            method,
            lower,
            upper,
        }),
        _ => None,
    };
    let sample_count = aggregation::sample_count(point.sample_count.as_ref())?;
    let reference = reference(point)?;
    dictionary(&point.dimensions, false)?;
    let Some(Node::Object(dimensions)) = point.dimensions.node(point.dimensions.root()) else {
        return Err(Error::Validation);
    };
    if dimensions
        .iter()
        .any(|(_, id)| !matches!(point.dimensions.node(*id), Some(Node::String(_))))
    {
        return Err(Error::Validation);
    }
    if let Some(control) = &point.control {
        dictionary(control, true)?;
    }
    Ok(PointOut {
        point,
        sample_count,
        reference,
        uncertainty,
    })
}
fn text(value: &str) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(value).map_err(|_| model::ModelEncodeError::InvalidNode.into())
}
fn optional_text(value: Option<&str>) -> Result<Vec<u8>, Error> {
    value.map_or_else(|| Ok(b"null".to_vec()), text)
}
fn float(value: Option<f64>) -> Vec<u8> {
    value.map_or_else(
        || b"null".to_vec(),
        |v| model::model_float_text(v).into_bytes(),
    )
}
fn timestamp(value: Option<Timestamp>) -> Result<Vec<u8>, Error> {
    value.map_or_else(
        || Ok(b"null".to_vec()),
        |value| text(&value.model_isoformat()),
    )
}
fn fields(values: Vec<(&str, Vec<u8>)>) -> Result<Vec<u8>, Error> {
    let mut output = vec![b'{'];
    for (index, (name, value)) in values.into_iter().enumerate() {
        if index > 0 {
            output.push(b',');
        }
        output.extend(text(name)?);
        output.push(b':');
        output.extend(value);
    }
    output.push(b'}');
    Ok(output)
}
impl Reference<'_> {
    /// # Errors
    /// Preserves source model serialization errors without exposing record values.
    pub fn bytes(self) -> Result<Vec<u8>, Error> {
        fields(vec![
            ("value", float(Some(self.value))),
            ("label", text(self.label)?),
            ("kind", text(self.kind.as_str())?),
            ("ref", optional_text(self.reference)?),
        ])
    }
}
impl Uncertainty<'_> {
    /// # Errors
    /// Preserves source model serialization errors without exposing record values.
    pub fn bytes(self) -> Result<Vec<u8>, Error> {
        fields(vec![
            ("method", text(self.method)?),
            ("lower", float(Some(self.lower))),
            ("upper", float(Some(self.upper))),
        ])
    }
}
impl PointOut<'_> {
    /// Exact declaration-order response bytes, keeping typed wrappers outside inferred limits.
    /// # Errors
    /// Preserves immediate string encoding and inferred-value recursion failures.
    #[allow(
        clippy::too_many_lines,
        reason = "Source response declaration order is an observable contract"
    )]
    pub fn bytes(&self, context: Context) -> Result<Vec<u8>, Error> {
        let p = self.point;
        fields(vec![
            ("id", p.id.to_string().into_bytes()),
            (
                "attempt_ref",
                text(&format!("#{}.{}", p.unit_number, p.attempt_sequence))?,
            ),
            ("unit_number", p.unit_number.to_string().into_bytes()),
            ("unit_title", text(&p.unit_title)?),
            ("unit_state", text(&p.unit_state)?),
            ("attempt_state", text(&p.attempt_state)?),
            ("track", text(&p.track_slug)?),
            (
                "science_revision",
                p.science_revision.to_string().into_bytes(),
            ),
            ("metric", text(&p.metric)?),
            ("split", text(&p.split)?),
            (
                "dimensions",
                model::encode_model_mapping(
                    &p.dimensions,
                    p.dimensions.root(),
                    context.inferred_nesting_budget,
                )?,
            ),
            ("value", float(p.value)),
            (
                "missing_reason",
                optional_text(p.missing_reason.as_deref())?,
            ),
            ("unit", text(&p.unit)?),
            ("direction", text(&p.direction)?),
            (
                "sample_count",
                self.sample_count
                    .as_ref()
                    .map_or_else(|| b"null".to_vec(), |v| v.to_string().into_bytes()),
            ),
            ("control_value", float(p.control_value)),
            (
                "control",
                match &p.control {
                    Some(d) if !matches!(d.node(d.root()), Some(Node::Null)) => {
                        model::encode_model_mapping(d, d.root(), context.inferred_nesting_budget)?
                    }
                    _ => b"null".to_vec(),
                },
            ),
            (
                "reference",
                self.reference
                    .map_or_else(|| Ok(b"null".to_vec()), Reference::bytes)?,
            ),
            (
                "uncertainty",
                self.uncertainty
                    .map_or_else(|| Ok(b"null".to_vec()), Uncertainty::bytes)?,
            ),
            ("authority", text(&p.authority)?),
            ("source_ref", optional_text(p.source_ref.as_deref())?),
            ("claimed_at", timestamp(Some(p.claimed_at))?),
            ("finished_at", timestamp(p.finished_at)?),
            ("recorded_at", timestamp(Some(p.recorded_at))?),
        ])
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
