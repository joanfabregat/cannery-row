//! Pure stock-evaluator decisions. Callers validate policy/evidence schemas separately.
use cannery_core::{
    json::{self, Document, DocumentBuilder, Node, NodeId},
    text,
};
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{Signed, Zero};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GateResult {
    Pass,
    Fail,
    Unknown,
}
impl GateResult {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Unknown => "unknown",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verdict {
    Pass,
    Fail,
    Inconclusive,
}
impl Verdict {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Inconclusive => "inconclusive",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operator {
    Greater,
    GreaterEqual,
    Less,
    LessEqual,
}
impl Operator {
    fn parse(text: &String) -> Option<Self> {
        [
            (">", Self::Greater),
            (">=", Self::GreaterEqual),
            ("<", Self::Less),
            ("<=", Self::LessEqual),
        ]
        .into_iter()
        .find_map(|(name, value)| text.equals_utf8(name).then_some(value))
    }
    fn compare(self, left: &BigRational, right: &BigRational) -> bool {
        match self {
            Self::Greater => left > right,
            Self::GreaterEqual => left >= right,
            Self::Less => left < right,
            Self::LessEqual => left <= right,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Statistic {
    Value,
    Lower,
    Upper,
}
impl Statistic {
    fn parse(text: &String) -> Option<Self> {
        [
            ("value", Self::Value),
            ("uncertainty.lower", Self::Lower),
            ("uncertainty.upper", Self::Upper),
        ]
        .into_iter()
        .find_map(|(name, value)| text.equals_utf8(name).then_some(value))
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Higher,
    Lower,
}
impl Direction {
    fn parse(text: &String) -> Option<Self> {
        if text.equals_utf8("higher") {
            Some(Self::Higher)
        } else if text.equals_utf8("lower") {
            Some(Self::Lower)
        } else {
            None
        }
    }
}
#[derive(Clone, Eq, PartialEq)]
pub struct Control {
    pub id: String,
    pub revision: String,
}
/// Borrowed projections of already validated configuration, registry and evidence.
/// Policy uses the reference's flattened baseline measurements, not the config file grammar.
pub struct GateContext<'a> {
    pub metrics: &'a Document,
    pub policy: &'a Document,
    pub measurements: &'a Document,
    pub control: Option<&'a Control>,
    pub nesting_budget: usize,
}
pub struct GateOutcome {
    pub id: String,
    pub result: GateResult,
    pub detail: String,
}
pub struct GateEvaluation {
    pub gate: GateOutcome,
    pub comparisons: Document,
}
pub struct Assessment {
    pub gates: Vec<GateOutcome>,
    pub comparisons: Document,
    pub verdict: Verdict,
    pub reason: String,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum GateError {
    #[error("a gate input field is missing or has an incompatible shape")]
    Shape,
    #[error("gate text or numeric rendering failed")]
    Rendering,
    #[error("the gate policy has no finite minimum delta")]
    InvalidPolicy,
}

#[derive(Clone, Copy)]
struct Value<'a> {
    doc: &'a Document,
    id: NodeId,
    budget: usize,
}
impl<'a> Value<'a> {
    fn root(doc: &'a Document, budget: usize) -> Self {
        Self {
            doc,
            id: doc.root(),
            budget,
        }
    }
    fn node(self) -> Result<&'a Node, GateError> {
        self.doc.node(self.id).ok_or(GateError::Shape)
    }
    fn opt(self, key: &str) -> Result<Option<Self>, GateError> {
        if !matches!(self.node()?, Node::Object(_)) {
            return Err(GateError::Shape);
        }
        Ok(self.doc.field(self.id, key).map(|id| Self { id, ..self }))
    }
    fn get(self, key: &str) -> Result<Self, GateError> {
        self.opt(key)?.ok_or(GateError::Shape)
    }
    fn array(self) -> Result<Vec<Self>, GateError> {
        if let Node::Array(items) = self.node()? {
            Ok(items.iter().map(|&id| Self { id, ..self }).collect())
        } else {
            Err(GateError::Shape)
        }
    }
    fn text(self) -> Result<String, GateError> {
        text::str_value(self.doc, self.id, self.budget).map_err(|_| GateError::Rendering)
    }
    fn render(self) -> Result<String, GateError> {
        json::encode_ascii_pretty_node(self.doc, self.id, self.budget)
            .map_err(|_| GateError::Rendering)
    }
    fn is(self, text: &str) -> bool {
        matches!(self.node(),Ok(Node::String(value)) if value.equals_utf8(text))
    }
}
fn py(value: &str) -> String {
    String::from(value)
}
fn compose(pattern: &str, args: &[String]) -> String {
    let mut points = Vec::new();
    let mut args = args.iter();
    for (index, part) in pattern.split("{}").enumerate() {
        if index > 0
            && let Some(value) = args.next()
        {
            points.extend_from_slice(&value.codepoints());
        }
        points.extend(part.chars().map(u32::from));
    }
    // Inputs consist only of constructor-checked Python code points.
    cannery_core::text::from_codepoints(points).unwrap_or_else(|| py(""))
}
fn joined(parts: &[String], separator: &str) -> String {
    let mut points = Vec::new();
    let separator = py(separator);
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            points.extend_from_slice(&separator.codepoints());
        }
        points.extend_from_slice(&part.codepoints());
    }
    cannery_core::text::from_codepoints(points).unwrap_or_else(|| py(""))
}
fn push(builder: &mut DocumentBuilder, node: Node) -> Result<NodeId, GateError> {
    builder.push(node).map_err(|_| GateError::Rendering)
}
fn object(builder: &mut DocumentBuilder, fields: &[(&str, NodeId)]) -> Result<NodeId, GateError> {
    push(
        builder,
        Node::Object(fields.iter().map(|(name, id)| (py(name), *id)).collect()),
    )
}
fn import(builder: &mut DocumentBuilder, value: Value<'_>) -> Result<NodeId, GateError> {
    builder
        .import(value.doc, value.id)
        .map_err(|_| GateError::Rendering)
}
fn finish(builder: DocumentBuilder, root: NodeId) -> Result<Document, GateError> {
    builder.finish(root).map_err(|_| GateError::Rendering)
}
fn documents(items: &[Document]) -> Result<Document, GateError> {
    let mut builder = DocumentBuilder::new();
    let nodes = items
        .iter()
        .map(|doc| {
            builder
                .import(doc, doc.root())
                .map_err(|_| GateError::Rendering)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let root = push(&mut builder, Node::Array(nodes))?;
    finish(builder, root)
}
fn gate_document(builder: &mut DocumentBuilder, gate: &GateOutcome) -> Result<NodeId, GateError> {
    let id = push(builder, Node::String(gate.id.clone()))?;
    let result = push(builder, Node::String(py(gate.result.as_str())))?;
    let detail = push(builder, Node::String(gate.detail.clone()))?;
    object(
        builder,
        &[("id", id), ("result", result), ("detail", detail)],
    )
}
impl GateEvaluation {
    /// # Errors
    /// Returns a sanitized arena construction failure. Output construction,
    /// like Python's dict projection, does not apply wire serialization limits.
    pub fn document(&self, _budget: usize) -> Result<Document, GateError> {
        let mut builder = DocumentBuilder::new();
        let gate = gate_document(&mut builder, &self.gate)?;
        let comparisons = builder
            .import(&self.comparisons, self.comparisons.root())
            .map_err(|_| GateError::Rendering)?;
        let root = object(
            &mut builder,
            &[("gate", gate), ("comparisons", comparisons)],
        )?;
        finish(builder, root)
    }
}
impl Assessment {
    /// # Errors
    /// Returns a sanitized arena construction failure; wire limits are caller-owned.
    pub fn document(&self, _budget: usize) -> Result<Document, GateError> {
        let mut builder = DocumentBuilder::new();
        let gates = self
            .gates
            .iter()
            .map(|gate| gate_document(&mut builder, gate))
            .collect::<Result<Vec<_>, _>>()?;
        let gates = push(&mut builder, Node::Array(gates))?;
        let comparisons = builder
            .import(&self.comparisons, self.comparisons.root())
            .map_err(|_| GateError::Rendering)?;
        let verdict = push(&mut builder, Node::String(py(self.verdict.as_str())))?;
        let reason = push(&mut builder, Node::String(self.reason.clone()))?;
        let root = object(
            &mut builder,
            &[
                ("gates", gates),
                ("comparisons", comparisons),
                ("verdict", verdict),
                ("reason", reason),
            ],
        )?;
        finish(builder, root)
    }
}
#[derive(Clone)]
struct Decimal {
    coefficient: BigInt,
    exponent: i32,
    negative_zero: bool,
}
impl Decimal {
    fn from(value: Value<'_>) -> Result<Option<Self>, GateError> {
        let text = match value.node()? {
            Node::Integer(_) => value.render()?,
            Node::Float(number) if number.is_finite() => json::float_text(*number),
            _ => return Ok(None),
        };
        let (mantissa, exponent) = text
            .split_once(['e', 'E'])
            .map_or((text.as_str(), Ok(0)), |(m, e)| (m, e.parse::<i32>()));
        let exponent = exponent.map_err(|_| GateError::Rendering)?;
        let scale = mantissa.split_once('.').map_or(0, |(_, part)| part.len());
        let coefficient = mantissa
            .replace('.', "")
            .parse::<BigInt>()
            .map_err(|_| GateError::Rendering)?;
        Ok(Some(Self {
            negative_zero: coefficient.is_zero() && text.starts_with('-'),
            coefficient,
            exponent: exponent - i32::try_from(scale).map_err(|_| GateError::Rendering)?,
        }))
    }
    fn rational(&self) -> BigRational {
        if self.exponent < 0 {
            BigRational::new(
                self.coefficient.clone(),
                BigInt::from(10u8).pow(self.exponent.unsigned_abs()),
            )
        } else {
            BigRational::from_integer(
                &self.coefficient * BigInt::from(10u8).pow(self.exponent.unsigned_abs()),
            )
        }
    }
    fn shown(&self) -> Result<String, GateError> {
        let negative = self.coefficient.is_negative() || self.negative_zero;
        let digits = self.coefficient.abs().to_string();
        let adjusted =
            self.exponent + i32::try_from(digits.len()).map_err(|_| GateError::Rendering)? - 1;
        let body = if self.exponent > 0 || adjusted < -6 {
            let mut chars = digits.chars();
            let first = chars.next().ok_or(GateError::Rendering)?;
            let rest = chars.collect::<String>();
            format!(
                "{first}{}E{adjusted:+}",
                if rest.is_empty() {
                    String::new()
                } else {
                    format!(".{rest}")
                }
            )
        } else if self.exponent == 0 {
            digits
        } else {
            let scale = usize::try_from(-self.exponent).map_err(|_| GateError::Rendering)?;
            if scale >= digits.len() {
                format!("0.{}{digits}", "0".repeat(scale - digits.len()))
            } else {
                let split = digits.len() - scale;
                format!("{}.{}", &digits[..split], &digits[split..])
            }
        };
        Ok(if negative { format!("-{body}") } else { body })
    }
    fn subtract(&self, other: &Self) -> Result<Self, GateError> {
        let exponent = self.exponent.min(other.exponent);
        let left = &self.coefficient
            * BigInt::from(10u8)
                .pow(u32::try_from(self.exponent - exponent).map_err(|_| GateError::Rendering)?);
        let right = &other.coefficient
            * BigInt::from(10u8)
                .pow(u32::try_from(other.exponent - exponent).map_err(|_| GateError::Rendering)?);
        let mut coefficient = left - right;
        let mut exponent = exponent;
        let negative_zero = coefficient.is_zero() && self.negative_zero && !other.negative_zero;
        let digits = coefficient.abs().to_string().len();
        if digits > 800 {
            let drop = digits - 800;
            let factor =
                BigInt::from(10u8).pow(u32::try_from(drop).map_err(|_| GateError::Rendering)?);
            let absolute = coefficient.abs();
            let mut rounded = &absolute / &factor;
            let remainder = &absolute % &factor;
            let twice = &remainder * 2u8;
            if twice > factor || (twice == factor && (&rounded % 2u8) != BigInt::zero()) {
                rounded += 1u8;
            }
            exponent += i32::try_from(drop).map_err(|_| GateError::Rendering)?;
            if rounded.to_string().len() > 800 {
                rounded /= 10u8;
                exponent += 1;
            }
            coefficient = if coefficient.is_negative() {
                -rounded
            } else {
                rounded
            };
        }
        Ok(Self {
            coefficient,
            exponent,
            negative_zero,
        })
    }
}
type Dimensions = Vec<(String, String)>;
fn dimensions(value: Value<'_>) -> Result<Dimensions, GateError> {
    let Some(dims) = value.opt("dimensions")? else {
        return Ok(Vec::new());
    };
    match dims.node()? {
        Node::Null => Ok(Vec::new()),
        Node::Object(fields) => {
            let mut result = fields
                .iter()
                .map(|(name, id)| Ok((name.clone(), Value { id: *id, ..dims }.text()?)))
                .collect::<Result<Vec<_>, GateError>>()?;
            result.sort_by_key(|a| a.0.codepoints());
            Ok(result)
        }
        _ => Err(GateError::Shape),
    }
}
fn same_slice(
    value: Value<'_>,
    metric: &String,
    split: &String,
    dims: &Dimensions,
) -> Result<bool, GateError> {
    Ok(value.get("metric")?.text()? == *metric
        && value.get("split")?.text()? == *split
        && dimensions(value)? == *dims)
}
fn control_from(value: Value<'_>) -> Result<Option<Control>, GateError> {
    if matches!(value.node()?, Node::Null) {
        return Ok(None);
    }
    let items = value.array()?;
    if items.len() != 2 {
        return Err(GateError::Shape);
    }
    Ok(Some(Control {
        id: items[0].text()?,
        revision: items[1].text()?,
    }))
}
struct Pinned<'a> {
    control: Option<&'a Control>,
    baseline: Option<Value<'a>>,
    registered: Option<Decimal>,
}
impl Pinned<'_> {
    fn source(&self) -> &'static str {
        if self.registered.is_some() {
            "resolved"
        } else {
            "tester_reported"
        }
    }
}
fn baseline<'a>(
    policy: Value<'a>,
    control: Option<&Control>,
) -> Result<Option<Value<'a>>, GateError> {
    let Some(control) = control else {
        return Ok(None);
    };
    for base in policy.get("baselines")?.array()?.into_iter().rev() {
        if base.get("id")?.text()? == control.id
            && base.get("revision")?.text()? == control.revision
        {
            return Ok(Some(base));
        }
    }
    Ok(None)
}
fn pinned<'a>(
    policy: Value<'a>,
    control: Option<&'a Control>,
    metric: &String,
    split: &String,
    dims: &Dimensions,
) -> Result<Pinned<'a>, GateError> {
    let base = baseline(policy, control)?;
    let mut registered = None;
    if let Some(base) = base {
        for measurement in base.get("measurements")?.array()?.into_iter().rev() {
            if same_slice(measurement, metric, split, dims)? {
                registered = Decimal::from(measurement.get("value")?)?;
                break;
            }
        }
    }
    Ok(Pinned {
        control,
        baseline: base,
        registered,
    })
}
fn reference(
    measurement: Value<'_>,
    pinned: &Pinned<'_>,
) -> Result<Result<Decimal, String>, GateError> {
    let raw = measurement.opt("control_value")?;
    let reported = raw.map(Decimal::from).transpose()?.flatten();
    let Some(registered) = pinned.registered.as_ref() else {
        return Ok(reported.ok_or_else(|| py("no finite control_value")));
    };
    if raw.is_none()
        || reported
            .as_ref()
            .is_some_and(|value| value.rational() == registered.rational())
    {
        return Ok(Ok(registered.clone()));
    }
    let shown = if let Some(reported) = reported {
        py(&reported.shown()?)
    } else {
        raw.ok_or(GateError::Shape)?.text()?
    };
    let base = pinned.baseline.ok_or(GateError::Shape)?;
    Ok(Err(compose(
        "control_mismatch: the tester's control_value {} is not {}, the value registered for baseline {} revision {}",
        &[
            shown,
            py(&registered.shown()?),
            base.get("id")?.text()?,
            base.get("revision")?.text()?,
        ],
    )))
}
fn comparison(
    measurement: Value<'_>,
    control: &Decimal,
    pinned: &Pinned<'_>,
) -> Result<Option<Document>, GateError> {
    let Some(value) = measurement.opt("value")? else {
        return Ok(None);
    };
    if Decimal::from(value)?.is_none() {
        return Ok(None);
    }
    let number = control
        .shown()?
        .parse::<f64>()
        .map_err(|_| GateError::Rendering)?;
    let mut builder = DocumentBuilder::new();
    let numeric = push(&mut builder, Node::Float(number))?;
    let mut reference = vec![("value", numeric)];
    if let Some(base) = pinned.baseline.filter(|_| pinned.registered.is_some()) {
        let label = import(&mut builder, base.get("label")?)?;
        let kind = push(&mut builder, Node::String(py("baseline")))?;
        let id = import(&mut builder, base.get("id")?)?;
        reference.extend([("label", label), ("kind", kind), ("ref", id)]);
    } else {
        let named = pinned.control.map_or_else(
            || py(""),
            |value| compose(" for {} {}", &[value.id.clone(), value.revision.clone()]),
        );
        let label = push(
            &mut builder,
            Node::String(compose("control value reported by the tester{}", &[named])),
        )?;
        let kind = push(&mut builder, Node::String(py("other")))?;
        reference.extend([("label", label), ("kind", kind)]);
    }
    let reference = object(&mut builder, &reference)?;
    let metric = import(&mut builder, measurement.get("metric")?)?;
    let split = import(&mut builder, measurement.get("split")?)?;
    let dimensions = match measurement
        .opt("dimensions")?
        .filter(|value| matches!(value.node(),Ok(Node::Object(fields)) if !fields.is_empty()))
    {
        Some(value) => import(&mut builder, value)?,
        None => push(&mut builder, Node::Object(Vec::new()))?,
    };
    let value = import(&mut builder, value)?;
    let source = push(&mut builder, Node::String(py("tester")))?;
    let root = object(
        &mut builder,
        &[
            ("metric", metric),
            ("split", split),
            ("dimensions", dimensions),
            ("value", value),
            ("source", source),
            ("reference", reference),
        ],
    )?;
    Ok(Some(finish(builder, root)?))
}
fn compare(
    measurements: &[Value<'_>],
    gate: Value<'_>,
    pinned: &Pinned<'_>,
) -> Result<(GateResult, String, Option<Document>), GateError> {
    if measurements.is_empty() {
        return Ok((GateResult::Unknown, py("no verified measurement"), None));
    }
    if measurements.len() > 1 {
        return Ok((
            GateResult::Unknown,
            py(&format!(
                "{} verified measurements of the same slice",
                measurements.len()
            )),
            None,
        ));
    }
    let measured = measurements[0];
    if let Some(reason) = measured.opt("missing_reason")? {
        return Ok((
            GateResult::Unknown,
            compose("reported missing: {}", &[reason.text()?]),
            None,
        ));
    }
    let reference = match reference(measured, pinned)? {
        Ok(value) => value,
        Err(reason) => return Ok((GateResult::Unknown, reason, None)),
    };
    let statistic = gate.get("statistic")?.text()?;
    let observed = match Statistic::parse(&statistic) {
        Some(Statistic::Value) => measured.opt("value")?,
        Some(Statistic::Lower | Statistic::Upper) => match measured.opt("uncertainty")? {
            Some(value) if matches!(value.node()?, Node::Object(_)) => {
                value.opt(if statistic.equals_utf8("uncertainty.lower") {
                    "lower"
                } else {
                    "upper"
                })?
            }
            _ => None,
        },
        None => None,
    };
    let Some(observed) = observed.map(Decimal::from).transpose()?.flatten() else {
        return Ok((
            GateResult::Unknown,
            compose("no finite {}", &[statistic]),
            None,
        ));
    };
    let minimum = Decimal::from(gate.get("min_delta")?)?.ok_or(GateError::InvalidPolicy)?;
    let op = gate.get("op")?.text()?;
    let operator = Operator::parse(&op).ok_or(GateError::InvalidPolicy)?;
    let passed = operator.compare(
        &(observed.rational() - reference.rational()),
        &minimum.rational(),
    );
    let detail = compose(
        if passed {
            "{} {} - control {} = {} {} {}"
        } else {
            "{} {} - control {} = {}, not {} {}"
        },
        &[
            statistic,
            py(&observed.shown()?),
            py(&reference.shown()?),
            py(&observed.subtract(&reference)?.shown()?),
            op,
            py(&minimum.shown()?),
        ],
    );
    Ok((
        if passed {
            GateResult::Pass
        } else {
            GateResult::Fail
        },
        detail,
        comparison(measured, &reference, pinned)?,
    ))
}
fn combined(results: &[GateResult]) -> GateResult {
    if results.contains(&GateResult::Fail) {
        GateResult::Fail
    } else if results.is_empty() || results.contains(&GateResult::Unknown) {
        GateResult::Unknown
    } else {
        GateResult::Pass
    }
}
fn direction_problem(metric: Value<'_>, gate: Value<'_>) -> Result<Option<String>, GateError> {
    let key = metric.get("key")?.text()?;
    let shown = text::repr_string(&key).map_err(|_| GateError::Rendering)?;
    let direction = metric.get("direction")?.text()?;
    let Some(parsed) = Direction::parse(&direction) else {
        return Ok(Some(compose("metric {} has no known direction", &[shown])));
    };
    let op = gate.get("op")?.text()?;
    let allowed = match parsed {
        Direction::Higher => matches!(
            Operator::parse(&op),
            Some(Operator::Greater | Operator::GreaterEqual)
        ),
        Direction::Lower => matches!(
            Operator::parse(&op),
            Some(Operator::Less | Operator::LessEqual)
        ),
    };
    if !allowed {
        return Ok(Some(compose(
            "metric {} is {}-is-better: op must be {} or {}, not {}",
            &[
                shown,
                direction,
                py(if parsed == Direction::Higher {
                    ">"
                } else {
                    "<"
                }),
                py(if parsed == Direction::Higher {
                    ">="
                } else {
                    "<="
                }),
                text::repr_string(&op).map_err(|_| GateError::Rendering)?,
            ],
        )));
    }
    let statistic = gate.get("statistic")?.text()?;
    let accepted = match parsed {
        Direction::Higher => matches!(
            Statistic::parse(&statistic),
            Some(Statistic::Value | Statistic::Lower)
        ),
        Direction::Lower => matches!(
            Statistic::parse(&statistic),
            Some(Statistic::Value | Statistic::Upper)
        ),
    };
    if !accepted {
        return Ok(Some(compose(
            "metric {} is {}-is-better: the conservative statistic is value or {}, not {}",
            &[
                shown,
                direction,
                py(if parsed == Direction::Higher {
                    "uncertainty.lower"
                } else {
                    "uncertainty.upper"
                }),
                text::repr_string(&statistic).map_err(|_| GateError::Rendering)?,
            ],
        )));
    }
    Ok(None)
}
// Keep the source's ordered early decisions and per-dimension combination together.
#[allow(clippy::too_many_lines)]
fn evaluate(
    context: &GateContext<'_>,
    gate: Value<'_>,
    control: Option<&Control>,
) -> Result<(GateOutcome, Vec<Document>), GateError> {
    let id = gate.get("id")?.text()?;
    let key = gate.get("metric")?.text()?;
    let split = gate.get("split")?.text()?;
    let location = compose("{} on {}", &[key.clone(), split.clone()]);
    let unknown = |detail| {
        Ok((
            GateOutcome {
                id: id.clone(),
                result: GateResult::Unknown,
                detail,
            },
            Vec::new(),
        ))
    };
    let metrics = Value::root(context.metrics, context.nesting_budget);
    let mut metric = None;
    for entry in metrics.array()?.into_iter().rev() {
        if entry.get("key")?.text()? == key {
            metric = Some(entry);
            break;
        }
    }
    let Some(metric) = metric else {
        return unknown(compose(
            "{}: metric not registered in the pinned science revision",
            &[location],
        ));
    };
    if let Some(problem) = direction_problem(metric, gate)? {
        return unknown(compose("{}: invalid gate, {}", &[location, problem]));
    }
    let policy = Value::root(context.policy, context.nesting_budget);
    if let Some(control) = control
        && !policy.get("baselines")?.array()?.is_empty()
        && baseline(policy, Some(control))?.is_none()
    {
        return unknown(compose(
            "{}: every slice unknown, the pinned control, baseline {} revision {}, is not listed in evaluator configuration {}",
            &[
                location,
                control.id.clone(),
                control.revision.clone(),
                policy.get("revision")?.text()?,
            ],
        ));
    }
    let mut relevant = Vec::new();
    for measurement in Value::root(context.measurements, context.nesting_budget).array()? {
        if measurement
            .opt("authority")?
            .is_some_and(|value| value.is("tester_verified"))
            && measurement.get("metric")?.text()? == key
            && measurement.get("split")?.text()? == split
        {
            relevant.push(measurement);
        }
    }
    let dimension = gate
        .opt("per_dimension")?
        .filter(|value| !matches!(value.node(), Ok(Node::Null)));
    let Some(dimension) = dimension else {
        let pin = pinned(policy, control, &key, &split, &Vec::new())?;
        let mut overall = Vec::new();
        for measurement in relevant {
            if dimensions(measurement)?.is_empty() {
                overall.push(measurement);
            }
        }
        let (result, detail, comparison) = compare(&overall, gate, &pin)?;
        return Ok((
            GateOutcome {
                id,
                result,
                detail: compose(
                    "{}: {}; control_source: {}",
                    &[location, detail, py(pin.source())],
                ),
            },
            comparison.into_iter().collect(),
        ));
    };
    let name = dimension.text()?;
    let mut by_value: BTreeMap<Vec<u32>, Vec<Value<'_>>> = BTreeMap::new();
    for measurement in relevant {
        let dims = dimensions(measurement)?;
        if dims.len() == 1 && dims[0].0 == name {
            by_value
                .entry(dims[0].1.codepoints().clone())
                .or_default()
                .push(measurement);
        }
    }
    let mut registered = None;
    for item in metric.get("dimensions")?.array()?.into_iter().rev() {
        if item.get("name")?.text()? == name {
            if let Some(values) = item
                .opt("values")?
                .filter(|value| !matches!(value.node(), Ok(Node::Null)))
            {
                registered = Some(
                    values
                        .array()?
                        .iter()
                        .map(|value| Ok(value.text()?.codepoints().clone()))
                        .collect::<Result<BTreeSet<_>, GateError>>()?,
                );
            }
            break;
        }
    }
    let values = registered.unwrap_or_else(|| by_value.keys().cloned().collect());
    if values.is_empty() {
        return unknown(compose(
            "{}: no verified measurement per {}",
            &[location, name],
        ));
    }
    let mut results = Vec::new();
    let mut parts = Vec::new();
    let mut comparisons = Vec::new();
    for value in values {
        let text =
            cannery_core::text::from_codepoints(value.clone()).ok_or(GateError::Rendering)?;
        let dims = vec![(name.clone(), text.clone())];
        let pin = pinned(policy, control, &key, &split, &dims)?;
        let measured = by_value.get(&value).map_or(&[][..], Vec::as_slice);
        let (result, detail, comparison) = compare(measured, gate, &pin)?;
        results.push(result);
        parts.push(compose(
            "{}={} {}: {}; control_source: {}",
            &[
                name.clone(),
                text,
                py(result.as_str()),
                detail,
                py(pin.source()),
            ],
        ));
        if let Some(comparison) = comparison {
            comparisons.push(comparison);
        }
    }
    Ok((
        GateOutcome {
            id,
            result: combined(&results),
            detail: compose("{}; {}", &[location, joined(&parts, "; ")]),
        },
        comparisons,
    ))
}
/// Evaluate one gate with the explicitly supplied control (no default-control fallback).
/// # Errors
/// Returns sanitized input/policy/rendering failures; callers validate schemas separately.
pub fn evaluate_gate(
    context: &GateContext<'_>,
    gate: &Document,
) -> Result<GateEvaluation, GateError> {
    let (gate, comparisons) = evaluate(
        context,
        Value::root(gate, context.nesting_budget),
        context.control,
    )?;
    Ok(GateEvaluation {
        gate,
        comparisons: documents(&comparisons)?,
    })
}
/// Assess all configured gates, applying default control and first-comparison deduplication.
/// # Errors
/// Returns sanitized input/policy/rendering failures; this is not a config parser or schema validator.
pub fn assess(context: &GateContext<'_>) -> Result<Assessment, GateError> {
    let policy = Value::root(context.policy, context.nesting_budget);
    let default = policy
        .opt("default_control")?
        .map(control_from)
        .transpose()?
        .flatten();
    let control = context.control.or(default.as_ref());
    let mut gates = Vec::new();
    let mut comparisons = Vec::new();
    let mut seen = BTreeSet::new();
    for gate in policy.get("gates")?.array()? {
        let (outcome, compared) = evaluate(context, gate, control)?;
        gates.push(outcome);
        for item in compared {
            let value = Value::root(&item, context.nesting_budget);
            let metric = value.get("metric")?.text()?;
            let split = value.get("split")?.text()?;
            let dims = dimensions(value)?;
            let key = (
                metric.codepoints().clone(),
                split.codepoints().clone(),
                dims.iter()
                    .map(|(key, value)| (key.codepoints().clone(), value.codepoints().clone()))
                    .collect::<Vec<_>>(),
            );
            if seen.insert(key) {
                comparisons.push(item);
            }
        }
    }
    let (verdict, summary) = summarize(&gates);
    let details = joined(
        &gates
            .iter()
            .map(|gate| {
                compose(
                    "[{}: {}; {}]",
                    &[
                        gate.id.clone(),
                        py(gate.result.as_str()),
                        gate.detail.clone(),
                    ],
                )
            })
            .collect::<Vec<_>>(),
        " ",
    );
    let reason = if details.codepoints().is_empty() {
        summary
    } else {
        compose("{} {}", &[summary, details])
    };
    Ok(Assessment {
        gates,
        comparisons: documents(&comparisons)?,
        verdict,
        reason,
    })
}
fn summarize(gates: &[GateOutcome]) -> (Verdict, String) {
    let failed = gates
        .iter()
        .filter(|gate| gate.result == GateResult::Fail)
        .map(|gate| gate.id.clone())
        .collect::<Vec<_>>();
    let unknown = gates
        .iter()
        .filter(|gate| gate.result == GateResult::Unknown)
        .map(|gate| gate.id.clone())
        .collect::<Vec<_>>();
    if !failed.is_empty() {
        (
            Verdict::Fail,
            compose(
                "Fail: {} of {} gates fail ({}).",
                &[
                    py(&failed.len().to_string()),
                    py(&gates.len().to_string()),
                    joined(&failed, ", "),
                ],
            ),
        )
    } else if !unknown.is_empty() || gates.is_empty() {
        (
            Verdict::Inconclusive,
            compose(
                "Inconclusive: no gate fails, but {} of {} gates are unknown ({}); unknown never passes.",
                &[
                    py(&unknown.len().to_string()),
                    py(&gates.len().to_string()),
                    joined(&unknown, ", "),
                ],
            ),
        )
    } else {
        (
            Verdict::Pass,
            py(&format!(
                "Pass: all {} gates pass on tester-verified measurements.",
                gates.len()
            )),
        )
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
