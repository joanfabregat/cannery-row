//! Source chart buckets retain Python numeric equality and stable non-total ordering.
use crate::{
    aggregation,
    projection::{self, Context, Error, Uncertainty},
    repo::Point,
};
use cannery_core::{
    json::{Document, Node, NodeId, model},
    sorting,
    timestamps::Timestamp,
};
use num_bigint::BigInt;
use num_traits::FromPrimitive;
use std::cmp::Ordering;

#[derive(Clone)]
pub enum Scalar {
    Null,
    Bool(bool),
    Integer(BigInt),
    Float(f64),
    Text(String),
    Timestamp(Timestamp),
    /// Deferred rejection: source evaluates x before hashing its bucket key.
    Unhashable,
}
impl std::fmt::Debug for Scalar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChartScalar([redacted])")
    }
}
impl Scalar {
    fn from_node(document: &Document, id: NodeId) -> Result<Self, Error> {
        Ok(match document.node(id).ok_or(Error::Value)? {
            Node::Null => Self::Null,
            Node::Bool(v) => Self::Bool(*v),
            Node::Integer(v) => Self::Integer(v.clone()),
            Node::Float(v) => Self::Float(*v),
            Node::String(v) => Self::Text(v.clone()),
            Node::Array(_) | Node::Object(_) => Self::Unhashable,
        })
    }
    fn equal(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Text(a), Self::Text(b)) => a == b,
            (Self::Timestamp(a), Self::Timestamp(b)) => a == b,
            _ => self.numeric(other) == Some(Ordering::Equal),
        }
    }
    fn integer(&self) -> Option<BigInt> {
        match self {
            Self::Bool(v) => Some(BigInt::from(u8::from(*v))),
            Self::Integer(v) => Some(v.clone()),
            _ => None,
        }
    }
    fn numeric(&self, other: &Self) -> Option<Ordering> {
        match (self.integer(), other.integer(), self, other) {
            (Some(a), Some(b), _, _) => Some(a.cmp(&b)),
            (Some(a), _, _, Self::Float(b)) => integer_float(&a, *b),
            (_, Some(b), Self::Float(a), _) => integer_float(&b, *a).map(Ordering::reverse),
            (_, _, Self::Float(a), Self::Float(b)) => a.partial_cmp(b),
            _ => None,
        }
    }
    fn category(&self) -> u8 {
        match self {
            Self::Null => 2,
            Self::Text(_) => 1,
            _ => 0,
        }
    }
    fn less(&self, other: &Self) -> Result<bool, Error> {
        if self.category() != other.category() {
            return Ok(self.category() < other.category());
        }
        match (self, other) {
            (Self::Null, Self::Null) => Ok(false),
            (Self::Text(a), Self::Text(b)) => Ok(a.codepoints() < b.codepoints()),
            (Self::Timestamp(a), Self::Timestamp(b)) => Ok(a.0 < b.0),
            (Self::Timestamp(_), _) | (_, Self::Timestamp(_)) => Err(Error::Type),
            _ => Ok(self.numeric(other) == Some(Ordering::Less)),
        }
    }
    /// Exact inferred scalar bytes, without a decimal digit limit.
    /// # Errors
    /// Preserves source UTF-8 failures.
    pub fn bytes(&self) -> Result<Vec<u8>, Error> {
        Ok(match self {
            Self::Null => b"null".to_vec(),
            Self::Bool(v) => v.to_string().into_bytes(),
            Self::Integer(v) => v.to_string().into_bytes(),
            Self::Float(v) => model::model_float_text(*v).into_bytes(),
            Self::Text(v) => {
                serde_json::to_vec(&v.as_utf8().ok_or(model::ModelEncodeError::Encoding)?)
                    .map_err(|_| Error::Value)?
            }
            Self::Timestamp(v) => {
                serde_json::to_vec(&v.model_isoformat()).map_err(|_| Error::Value)?
            }
            Self::Unhashable => return Err(Error::Type),
        })
    }
}
fn integer_float(integer: &BigInt, float: f64) -> Option<Ordering> {
    if float.is_nan() {
        return None;
    }
    if float.is_infinite() {
        return Some(if float.is_sign_negative() {
            Ordering::Greater
        } else {
            Ordering::Less
        });
    }
    let truncated = BigInt::from_f64(float)?;
    Some(match integer.cmp(&truncated) {
        Ordering::Equal if float.fract() > 0.0 => Ordering::Less,
        Ordering::Equal if float.fract() < 0.0 => Ordering::Greater,
        value => value,
    })
}
fn text(value: &str) -> Scalar {
    Scalar::Text(String::from(value))
}
fn lookup(document: &Document, name: &str) -> Result<Scalar, Error> {
    if !matches!(document.node(document.root()), Some(Node::Object(_))) {
        return Err(Error::Attribute);
    }
    document
        .field(document.root(), name)
        .map_or(Ok(Scalar::Null), |id| Scalar::from_node(document, id))
}
fn truthy(document: &Document) -> bool {
    match document.node(document.root()) {
        Some(Node::Null) | None => false,
        Some(Node::Bool(v)) => *v,
        Some(Node::Integer(v)) => v != &BigInt::from(0),
        Some(Node::Float(v)) => *v != 0.0,
        Some(Node::String(v)) => !v.codepoints().is_empty(),
        Some(Node::Array(v)) => !v.is_empty(),
        Some(Node::Object(v)) => !v.is_empty(),
    }
}
fn group_value(point: &Point, name: &String) -> Result<Scalar, Error> {
    let name = name.as_utf8().ok_or(model::ModelEncodeError::Encoding)?;
    if name == "track" {
        return Ok(text(&point.track_slug));
    }
    if let Some(name) = name.strip_prefix("hypothesis.") {
        return point
            .project_fields
            .as_ref()
            .filter(|doc| truthy(doc))
            .map_or(Ok(Scalar::Null), |doc| lookup(doc, name));
    }
    lookup(&point.dimensions, &name)
}
fn x_value(point: &Point, field: Option<&String>) -> Result<(Scalar, bool), Error> {
    let Some(field) = field else {
        return Ok((Scalar::Null, true));
    };
    let field = field.as_utf8().ok_or(model::ModelEncodeError::Encoding)?;
    let datetime = |value: Option<Timestamp>| value.map_or(Scalar::Null, Scalar::Timestamp);
    let value = match field.as_str() {
        "attempt.finished_at" => datetime(point.finished_at),
        "attempt.claimed_at" => datetime(Some(point.claimed_at)),
        "attempt.submitted_at" => datetime(point.submitted_at),
        "attempt.recorded_at" => datetime(Some(point.recorded_at)),
        "attempt.sequence" => Scalar::Integer(point.attempt_sequence.into()),
        "attempt.ref" => text(&format!(
            "#{}.{}",
            point.hypothesis_number, point.attempt_sequence
        )),
        "attempt.state" => text(&point.attempt_state),
        "hypothesis.number" => Scalar::Integer(point.hypothesis_number.into()),
        "hypothesis.title" => text(&point.hypothesis_title),
        "hypothesis.state" => text(&point.hypothesis_state),
        "track.slug" => text(&point.track_slug),
        "track.title" => text(&point.track_title),
        _ => {
            let (entity, name) = field.split_once('.').unwrap_or((&field, ""));
            return Ok((
                if entity == "hypothesis" {
                    point
                        .project_fields
                        .as_ref()
                        .map_or(Ok(Scalar::Null), |doc| lookup(doc, name))?
                } else {
                    Scalar::Null
                },
                entity == "hypothesis",
            ));
        }
    };
    Ok((value, true))
}
#[derive(Debug)]
pub struct SeriesPoint<'a> {
    pub x: Scalar,
    pub value: Option<f64>,
    pub count: usize,
    pub attempt_refs: Vec<String>,
    pub control_value: Option<f64>,
    pub reference_label: Option<&'a str>,
    pub uncertainty: Option<Uncertainty<'a>>,
    pub sample_count: Option<BigInt>,
    pub missing_reasons: Vec<&'a str>,
}
#[derive(Debug)]
pub struct Series<'a> {
    pub science_revision: i32,
    pub group: Vec<(String, Scalar)>,
    pub points: Vec<SeriesPoint<'a>>,
}
#[derive(Clone)]
struct Bucket<'a> {
    revision: i32,
    group: Vec<Scalar>,
    members: Vec<(Scalar, i64, Vec<&'a Point>)>,
}
fn groups_equal(a: &[Scalar], b: &[Scalar]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.equal(b))
}
fn group_less(a: &Bucket<'_>, b: &Bucket<'_>) -> Result<bool, Error> {
    if a.revision != b.revision {
        return Ok(a.revision < b.revision);
    }
    for (a, b) in a.group.iter().zip(&b.group) {
        if !a.equal(b) {
            return a.less(b);
        }
    }
    Ok(a.group.len() < b.group.len())
}
/// Resolve measurement buckets in source evaluation order; revisions never combine.
/// # Errors
/// Preserves legacy attribute/hash/model/numeric failures.
#[allow(
    clippy::too_many_lines,
    reason = "Source bucket evaluation and model construction order"
)]
#[allow(
    clippy::float_cmp,
    reason = "Python reference sets use exact floating equality"
)]
pub fn series<'a>(
    view: &Document,
    rows: &'a [Point],
    method: Option<&String>,
) -> Result<(Vec<Series<'a>>, Vec<String>), Error> {
    if !matches!(view.node(view.root()), Some(Node::Object(_))) {
        return Err(Error::Attribute);
    }
    let mut ordered: Vec<_> = rows.iter().collect();
    ordered.sort_by_key(|p| p.id);
    let mut buckets: Vec<Bucket<'a>> = Vec::new();
    let mut group_names = Vec::new();
    let mut warnings = Vec::new();
    let mut unknown = false;
    let mut mixed = false;
    let mut x_name = None;
    for row in ordered {
        group_names = match view
            .field(view.root(), "group_by")
            .and_then(|id| view.node(id))
        {
            None => Vec::new(),
            Some(Node::Array(ids)) => ids
                .iter()
                .map(|id| match view.node(*id) {
                    Some(Node::String(v)) => Ok(v.clone()),
                    _ => Err(Error::Attribute),
                })
                .collect::<Result<_, _>>()?,
            Some(Node::Object(entries)) => entries.iter().map(|(name, _)| name.clone()).collect(),
            Some(Node::String(v)) => v
                .codepoints()
                .iter()
                .map(|cp| cannery_core::text::from_codepoints(vec![*cp]).ok_or(Error::Value))
                .collect::<Result<_, _>>()?,
            _ => return Err(Error::Type),
        };
        let group = group_names
            .iter()
            .map(|name| group_value(row, name))
            .collect::<Result<Vec<_>, _>>()?;
        x_name = match view.field(view.root(), "x").and_then(|id| view.node(id)) {
            None | Some(Node::Null) => None,
            Some(Node::String(v)) => Some(v),
            _ => return Err(Error::Attribute),
        };
        let (x, known) = x_value(row, x_name)?;
        unknown |= !known;
        if group
            .iter()
            .chain(std::iter::once(&x))
            .any(|value| matches!(value, Scalar::Unhashable))
        {
            return Err(Error::Type);
        }
        let index = buckets
            .iter()
            .position(|b| b.revision == row.science_revision && groups_equal(&b.group, &group));
        let bucket = if let Some(index) = index {
            &mut buckets[index]
        } else {
            buckets.push(Bucket {
                revision: row.science_revision,
                group,
                members: Vec::new(),
            });
            buckets.last_mut().ok_or(Error::Value)?
        };
        if let Some((_, _, members)) = bucket
            .members
            .iter_mut()
            .find(|(value, id, _)| value.equal(&x) && (method.is_some() || *id == row.id))
        {
            members.push(row);
        } else {
            bucket.members.push((x, row.id, vec![row]));
        }
    }
    if unknown {
        let name = x_name.ok_or(Error::Value)?;
        let repr = cannery_core::text::repr_string(name)
            .map_err(|_| Error::Value)?
            .as_utf8()
            .ok_or(model::ModelEncodeError::Encoding)?;
        warnings.push(format!("x field {repr} is not supported; x is null"));
    }
    sorting::sort(&mut buckets, group_less)?;
    let baseline = match view
        .field(view.root(), "baseline")
        .and_then(|id| view.node(id))
    {
        Some(Node::String(v)) => Some(v),
        _ => None,
    };
    let aggregation = method.map_or(
        aggregation::Aggregation::Mean,
        aggregation::Aggregation::from_name,
    );
    let mut result = Vec::new();
    for bucket in buckets {
        let mut points = Vec::new();
        for (x, _, members) in bucket.members {
            let values = members.iter().filter_map(|p| p.value).collect::<Vec<_>>();
            let mut references = Vec::new();
            for member in &members {
                if let Some(reference) = projection::overlay(member, baseline)?
                    && !references.iter().any(|(v, label): &(f64, &str)| {
                        *v == reference.value && *label == reference.label
                    })
                {
                    references.push((reference.value, reference.label));
                }
            }
            mixed |= references.len() > 1;
            let reference = (references.len() == 1).then(|| references[0]);
            let value = aggregation::aggregate(&values, aggregation)?;
            let single = if members.len() == 1 {
                Some(projection::point_out(members[0])?)
            } else {
                None
            };
            let mut attempt_refs = members
                .iter()
                .map(|p| format!("#{}.{}", p.hypothesis_number, p.attempt_sequence))
                .collect::<Vec<_>>();
            attempt_refs.sort();
            attempt_refs.dedup();
            let mut missing_reasons = members
                .iter()
                .filter_map(|p| p.missing_reason.as_deref())
                .collect::<Vec<_>>();
            missing_reasons.sort_unstable();
            missing_reasons.dedup();
            points.push(SeriesPoint {
                x,
                value,
                count: members.len(),
                attempt_refs,
                control_value: reference.map(|v| v.0),
                reference_label: reference.map(|v| v.1),
                uncertainty: single.as_ref().and_then(|v| v.uncertainty),
                sample_count: single.and_then(|v| v.sample_count),
                missing_reasons,
            });
        }
        let mut indices: Vec<usize> = (0..points.len()).collect();
        sorting::sort(&mut indices, |a, b| points[*a].x.less(&points[*b].x))?;
        let mut slots: Vec<_> = points.into_iter().map(Some).collect();
        let points = indices
            .into_iter()
            .map(|index| slots[index].take().ok_or(Error::Value))
            .collect::<Result<_, _>>()?;
        let mut group: Vec<(String, Scalar)> = Vec::new();
        for (name, value) in group_names.iter().cloned().zip(bucket.group) {
            if let Some((_, existing)) = group.iter_mut().find(|(key, _)| *key == name) {
                *existing = value;
            } else {
                group.push((name, value));
            }
        }
        result.push(Series {
            science_revision: bucket.revision,
            group,
            points,
        });
    }
    if mixed {
        warnings.push(
            "the verification reports of one point gave different references; it shows none".into(),
        );
    }
    Ok((result, warnings))
}
fn record(values: Vec<(&str, Vec<u8>)>) -> Result<Vec<u8>, Error> {
    let mut bytes = vec![b'{'];
    for (index, (key, value)) in values.into_iter().enumerate() {
        if index != 0 {
            bytes.push(b',');
        }
        bytes.extend(serde_json::to_vec(key).map_err(|_| Error::Value)?);
        bytes.push(b':');
        bytes.extend(value);
    }
    bytes.push(b'}');
    Ok(bytes)
}
fn array(values: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
    let mut bytes = vec![b'['];
    for (index, value) in values.into_iter().enumerate() {
        if index != 0 {
            bytes.push(b',');
        }
        bytes.extend(value);
    }
    bytes.push(b']');
    bytes
}
impl SeriesPoint<'_> {
    /// Exact typed-model declaration order.
    /// # Errors
    /// Preserves inferred string and model serialization failures.
    pub fn bytes(&self, _context: Context) -> Result<Vec<u8>, Error> {
        record(vec![
            ("x", self.x.bytes()?),
            (
                "value",
                self.value.map_or_else(
                    || b"null".to_vec(),
                    |v| model::model_float_text(v).into_bytes(),
                ),
            ),
            ("count", self.count.to_string().into_bytes()),
            (
                "attempt_refs",
                serde_json::to_vec(&self.attempt_refs).map_err(|_| Error::Value)?,
            ),
            (
                "control_value",
                self.control_value.map_or_else(
                    || b"null".to_vec(),
                    |v| model::model_float_text(v).into_bytes(),
                ),
            ),
            (
                "reference_label",
                serde_json::to_vec(&self.reference_label).map_err(|_| Error::Value)?,
            ),
            (
                "uncertainty",
                self.uncertainty
                    .map_or_else(|| Ok(b"null".to_vec()), projection::Uncertainty::bytes)?,
            ),
            (
                "sample_count",
                self.sample_count
                    .as_ref()
                    .map_or_else(|| b"null".to_vec(), |v| v.to_string().into_bytes()),
            ),
            (
                "missing_reasons",
                serde_json::to_vec(&self.missing_reasons).map_err(|_| Error::Value)?,
            ),
        ])
    }
}
impl Series<'_> {
    /// Exact source dictionary order and nested model bytes.
    /// # Errors
    /// Preserves inferred string and nested model serialization failures.
    pub fn bytes(&self, context: Context) -> Result<Vec<u8>, Error> {
        let mut group = vec![b'{'];
        for (index, (name, value)) in self.group.iter().enumerate() {
            if index != 0 {
                group.push(b',');
            }
            group.extend(Scalar::Text(name.clone()).bytes()?);
            group.push(b':');
            group.extend(value.bytes()?);
        }
        group.push(b'}');
        record(vec![
            (
                "science_revision",
                self.science_revision.to_string().into_bytes(),
            ),
            ("group", group),
            (
                "points",
                array(
                    self.points
                        .iter()
                        .map(|v| v.bytes(context))
                        .collect::<Result<Vec<_>, _>>()?,
                ),
            ),
        ])
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
