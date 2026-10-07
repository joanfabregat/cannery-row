//! Source-compatible science projections over prevalidated or legacy documents.
use cannery_core::{
    json::{Document, Node, NodeId},
    text::{self, RenderError},
};
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{ToPrimitive, Zero};

/// Explicit rendering depth for science projections.
#[derive(Clone, Copy, Debug)]
pub struct RenderingContext {
    pub nesting_budget: usize,
}
/// Values are never included in diagnostics. Validation paths use UTF-8 text.
#[derive(Clone, Eq, PartialEq)]
pub enum ScienceError {
    Validation { path: String, message: &'static str },
    Key,
    Type,
    Attribute,
    Value,
    Overflow,
    Recursion,
    InvalidNode,
}
impl std::fmt::Debug for ScienceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Validation { message, .. } => f
                .debug_struct("Validation")
                .field("message", message)
                .finish_non_exhaustive(),
            _ => f.write_str(self.class()),
        }
    }
}
impl std::fmt::Display for ScienceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.class())
    }
}
impl std::error::Error for ScienceError {}
impl ScienceError {
    #[must_use]
    pub const fn code(&self) -> Option<&'static str> {
        if matches!(self, Self::Validation { .. }) {
            Some("validation_failed")
        } else {
            None
        }
    }
    #[must_use]
    pub const fn class(&self) -> &'static str {
        match self {
            Self::Validation { .. } => "Validation",
            Self::Key => "KeyError",
            Self::Type => "TypeError",
            Self::Attribute => "AttributeError",
            Self::Value => "ValueError",
            Self::Overflow => "OverflowError",
            Self::Recursion => "RecursionError",
            Self::InvalidNode => "InvalidNode",
        }
    }
}
impl From<RenderError> for ScienceError {
    fn from(error: RenderError) -> Self {
        match error {
            RenderError::InvalidNode => Self::InvalidNode,
            RenderError::IntegerLimit | RenderError::Encoding => Self::Value,
            RenderError::Recursion => Self::Recursion,
        }
    }
}
fn violation(path: &str, message: &'static str) -> ScienceError {
    ScienceError::Validation {
        path: String::from(path),
        message,
    }
}
#[derive(Clone)]
pub(crate) enum Value {
    Node(NodeId),
    Text(String),
}
pub(crate) fn node(document: &Document, id: NodeId) -> Result<&Node, ScienceError> {
    document.node(id).ok_or(ScienceError::InvalidNode)
}
pub(crate) fn field(
    document: &Document,
    object: &Value,
    key: &str,
    required: bool,
) -> Result<Option<NodeId>, ScienceError> {
    let Value::Node(id) = object else {
        return Err(if required {
            ScienceError::Type
        } else {
            ScienceError::Attribute
        });
    };
    if !matches!(node(document, *id)?, Node::Object(_)) {
        return Err(if required {
            ScienceError::Type
        } else {
            ScienceError::Attribute
        });
    }
    let result = document.field(*id, key);
    if required && result.is_none() {
        return Err(ScienceError::Key);
    }
    Ok(result)
}
pub(crate) fn required(
    document: &Document,
    object: &Value,
    key: &str,
) -> Result<NodeId, ScienceError> {
    field(document, object, key, true)?.ok_or(ScienceError::Key)
}
pub(crate) fn items(document: &Document, value: NodeId) -> Result<Vec<Value>, ScienceError> {
    match node(document, value)? {
        Node::Array(values) => Ok(values.iter().copied().map(Value::Node).collect()),
        Node::Object(values) => Ok(values
            .iter()
            .map(|(key, _)| Value::Text(key.clone()))
            .collect()),
        Node::String(text) => text
            .codepoints()
            .iter()
            .map(|cp| {
                cannery_core::text::from_codepoints(vec![*cp])
                    .map(Value::Text)
                    .ok_or(ScienceError::InvalidNode)
            })
            .collect(),
        _ => Err(ScienceError::Type),
    }
}
pub(crate) fn optional_items(
    document: &Document,
    value: &Value,
    key: &str,
) -> Result<Vec<Value>, ScienceError> {
    field(document, value, key, false)?.map_or_else(|| Ok(vec![]), |id| items(document, id))
}
pub(crate) fn text(
    document: &Document,
    value: &Value,
    rendering: RenderingContext,
) -> Result<String, ScienceError> {
    match value {
        Value::Text(text) => Ok(text.clone()),
        Value::Node(id) => {
            text::str_value(document, *id, rendering.nesting_budget).map_err(Into::into)
        }
    }
}
pub(crate) fn required_text(
    document: &Document,
    value: &Value,
    key: &str,
    rendering: RenderingContext,
) -> Result<String, ScienceError> {
    text(
        document,
        &Value::Node(required(document, value, key)?),
        rendering,
    )
}
fn text_default(
    document: &Document,
    value: &Value,
    key: &str,
    default: &str,
    rendering: RenderingContext,
) -> Result<String, ScienceError> {
    field(document, value, key, false)?.map_or_else(
        || Ok(String::from(default)),
        |id| text(document, &Value::Node(id), rendering),
    )
}
fn truth(document: &Document, id: NodeId) -> Result<bool, ScienceError> {
    Ok(match node(document, id)? {
        Node::Null => false,
        Node::Bool(b) => *b,
        Node::Integer(i) => !i.is_zero(),
        Node::Float(f) => *f != 0.0,
        Node::String(s) => !s.codepoints().is_empty(),
        Node::Array(a) => !a.is_empty(),
        Node::Object(o) => !o.is_empty(),
    })
}
fn append(parts: &[&String]) -> Result<String, ScienceError> {
    cannery_core::text::from_codepoints(
        parts
            .iter()
            .flat_map(|p| p.chars().map(u32::from))
            .collect(),
    )
    .ok_or(ScienceError::InvalidNode)
}
fn unique<T: PartialEq>(values: impl IntoIterator<Item = T>) -> Vec<T> {
    let mut out = vec![];
    for value in values {
        if !out.contains(&value) {
            out.push(value);
        }
    }
    out
}
fn insert<T>(values: &mut Vec<(String, T)>, key: String, value: T) {
    if let Some((_, old)) = values.iter_mut().find(|(existing, _)| existing == &key) {
        *old = value;
    } else {
        values.push((key, value));
    }
}

/// JSON scalars with distinct boolean and numeric values; numbers compare exactly.
#[derive(Clone)]
pub enum Scalar {
    Null,
    Bool(bool),
    Number {
        value: BigRational,
        original: NodeId,
    },
    Text(String),
}
impl PartialEq for Scalar {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Scalar::Null, Scalar::Null) => true,
            (Scalar::Bool(a), Scalar::Bool(b)) => a == b,
            (Scalar::Number { value: a, .. }, Scalar::Number { value: b, .. }) => a == b,
            (Scalar::Text(a), Scalar::Text(b)) => a == b,
            _ => false,
        }
    }
}
impl std::fmt::Debug for Scalar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Scalar([REDACTED])")
    }
}
pub(crate) fn scalar(
    document: &Document,
    value: &Value,
    _rendering: RenderingContext,
) -> Result<Scalar, ScienceError> {
    if let Value::Text(text) = value {
        return Ok(Scalar::Text(text.clone()));
    }
    let Value::Node(id) = value else {
        return Err(ScienceError::InvalidNode);
    };
    Ok(match node(document, *id)? {
        Node::Null => Scalar::Null,
        Node::Bool(value) => Scalar::Bool(*value),
        Node::Integer(value) => Scalar::Number {
            value: BigRational::from_integer(value.clone()),
            original: *id,
        },
        Node::Float(value) => Scalar::Number {
            value: BigRational::from_float(*value).ok_or(ScienceError::Value)?,
            original: *id,
        },
        Node::String(value) => Scalar::Text(value.clone()),
        _ => return Err(ScienceError::Type),
    })
}
fn scalar_set(
    document: &Document,
    value: NodeId,
    rendering: RenderingContext,
) -> Result<Vec<Scalar>, ScienceError> {
    Ok(unique(
        items(document, value)?
            .iter()
            .map(|v| scalar(document, v, rendering))
            .collect::<Result<Vec<_>, _>>()?,
    ))
}

/// Resolved interface defaults; raw explicit encoding remains a lossless node.
pub struct InterfaceSpec {
    pub reference: String,
    pub encoding: String,
    pub explicit_encoding: Option<NodeId>,
    pub schema: Option<NodeId>,
    pub max_bytes: Option<BigInt>,
    pub allow_empty: bool,
    pub magic: Option<String>,
    pub validate: bool,
    pub validator: Option<String>,
}
impl std::fmt::Debug for InterfaceSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InterfaceSpec([REDACTED])")
    }
}
/// Read a consumed configuration integer within the signed 64-bit JSON contract.
/// # Errors
/// Rejects other JSON types and integers outside the supported range.
pub fn configuration_integer(document: &Document, id: NodeId) -> Result<BigInt, ScienceError> {
    match node(document, id)? {
        Node::Integer(value) => value
            .to_i64()
            .map(BigInt::from)
            .ok_or(ScienceError::Overflow),
        _ => Err(ScienceError::Type),
    }
}
impl InterfaceSpec {
    /// Resolve source defaults without validating an interface schema.
    /// # Errors
    /// Preserves source consumed-value and scalar-conversion error categories.
    #[allow(
        clippy::too_many_lines,
        reason = "Preserve source constructor evaluation and coercion order"
    )]
    pub fn from_registration(
        document: &Document,
        id: NodeId,
        rendering: RenderingContext,
    ) -> Result<Self, ScienceError> {
        let raw = Value::Node(id);
        let declared = field(document, &raw, "encoding", false)?
            .filter(|v| !matches!(document.node(*v), Some(Node::Null)));
        let media = if declared.is_none() {
            text_default(document, &raw, "media_type", "", rendering)?
        } else {
            String::new()
        };
        let format = if declared.is_none() {
            text_default(document, &raw, "format", "", rendering)?
        } else {
            String::new()
        };
        let has_schema = field(document, &raw, "schema", false)?;
        let encoding = if let Some(id) = declared {
            if let Node::String(s) = node(document, id)? {
                s.clone()
            } else {
                String::new()
            }
        } else if [
            "application/jsonl",
            "application/jsonlines",
            "application/x-jsonlines",
            "application/ndjson",
            "application/x-ndjson",
        ]
        .iter()
        .any(|v| media.equals_utf8(v))
            || ["jsonl", "jsonlines", "ndjson"]
                .iter()
                .any(|v| format.equals_utf8(v))
        {
            String::from("jsonl")
        } else if media.equals_utf8("application/json")
            || media.codepoints().ends_with(&[43, 106, 115, 111, 110])
            || format.equals_utf8("json")
            || has_schema.is_some()
        {
            String::from("json")
        } else {
            String::from("binary")
        };
        let name = required_text(document, &raw, "name", rendering)?;
        let version = required_text(document, &raw, "version", rendering)?;
        let reference = append(&[&name, &String::from("/v"), &version])?;
        let schema = has_schema.filter(|v| matches!(document.node(*v), Some(Node::Object(_))));
        let max_bytes = field(document, &raw, "max_bytes", false)?
            .filter(|v| !matches!(document.node(*v), Some(Node::Null)))
            .map(|v| configuration_integer(document, v))
            .transpose()?;
        let allow_empty = field(document, &raw, "allow_empty", false)?
            .map_or(Ok(false), |v| truth(document, v))?;
        let magic = field(document, &raw, "magic", false)?
            .filter(|v| !matches!(document.node(*v), Some(Node::Null)));
        let magic = if let Some(id) = magic {
            Some(text(document, &Value::Node(id), rendering)?)
        } else if !encoding.equals_utf8("binary") {
            Some(String::from("json"))
        } else {
            let media = text_default(document, &raw, "media_type", "", rendering)?;
            let format = text_default(document, &raw, "format", "", rendering)?;
            let name = if ["application/gzip", "application/x-gzip"]
                .iter()
                .any(|v| media.equals_utf8(v))
            {
                Some("gzip")
            } else if ["application/zip", "application/x-zip-compressed"]
                .iter()
                .any(|v| media.equals_utf8(v))
            {
                Some("zip")
            } else if ["application/vnd.apache.parquet", "application/x-parquet"]
                .iter()
                .any(|v| media.equals_utf8(v))
            {
                Some("parquet")
            } else {
                ["gzip", "zip", "parquet"]
                    .iter()
                    .copied()
                    .find(|v| format.equals_utf8(v))
            };
            name.map(String::from)
        };
        let validate =
            field(document, &raw, "validate", false)?.map_or(Ok(true), |v| truth(document, v))?;
        let validator = field(document, &raw, "validator", false)?
            .filter(|v| !matches!(document.node(*v), Some(Node::Null)))
            .map(|v| text(document, &Value::Node(v), rendering))
            .transpose()?;
        Ok(Self {
            reference,
            encoding,
            explicit_encoding: declared,
            schema,
            max_bytes,
            allow_empty,
            magic,
            validate,
            validator,
        })
    }
    #[must_use]
    pub fn parses_content(&self) -> bool {
        self.validate && !self.encoding.equals_utf8("binary")
    }
}

pub struct Metric {
    pub key: String,
    pub splits: Vec<Scalar>,
    pub dimensions: Vec<String>,
    pub unit: String,
    pub direction: String,
    pub dimension_values: Vec<(String, Option<Vec<Scalar>>)>,
}
pub struct Dataset {
    pub id: String,
    pub revision: String,
    pub held_out_labels: bool,
}
/// Borrowed registry with registration-order projections and last-value overwrite.
pub struct Science<'a> {
    pub revision: BigInt,
    pub content: &'a Document,
    pub metrics: Vec<(String, Metric)>,
    pub baseline_refs: Vec<(String, String)>,
    pub baselines: Vec<(String, String)>,
    pub hypothesis_fields: Option<NodeId>,
    pub datasets: Vec<(String, Dataset)>,
    pub interfaces: Vec<String>,
    pub interface_specs: Vec<(String, InterfaceSpec)>,
    pub validators: Vec<(String, NodeId)>,
    pub limits: Option<NodeId>,
    pub tester: Option<NodeId>,
    rendering: RenderingContext,
}
macro_rules! redacted_debug {($($t:ty),+)=>{$(impl std::fmt::Debug for $t {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str(concat!(stringify!($t),"([REDACTED])"))}})+};}
redacted_debug!(Metric, Dataset, Science<'_>);
impl<'a> Science<'a> {
    /// Read the bounded JSON output limit at consumption.
    /// # Errors
    /// Preserves missing, malformed and unrepresentable stored limit failures.
    pub fn max_output_bytes(&self) -> Result<BigInt, ScienceError> {
        let limits = self.limits.ok_or(ScienceError::Key)?;
        if !matches!(node(self.content, limits)?, Node::Object(_)) {
            return Err(ScienceError::Type);
        }
        let value = self
            .content
            .field(limits, "max_output_bytes")
            .ok_or(ScienceError::Key)?;
        configuration_integer(self.content, value)
    }
    /// Read the bounded JSON retry limit, defaulting to one.
    /// # Errors
    /// Preserves malformed legacy value conversion failures without diagnostics.
    pub fn max_auto_retries(&self) -> Result<BigInt, ScienceError> {
        self.content
            .field(self.content.root(), "max_auto_retries")
            .map_or_else(
                || Ok(BigInt::from(1)),
                |id| configuration_integer(self.content, id),
            )
    }
    /// Construct the source view; this does not perform publication validation.
    /// # Errors
    /// Reports consumed malformed legacy values without exposing their contents.
    #[allow(
        clippy::too_many_lines,
        reason = "Source registry projections have observable construction order"
    )]
    pub fn new(
        revision: BigInt,
        content: &'a Document,
        rendering: RenderingContext,
    ) -> Result<Self, ScienceError> {
        let root = Value::Node(content.root());
        let mut metrics = vec![];
        for raw in optional_items(content, &root, "metrics")? {
            let dimensions = field(content, &raw, "dimensions", false)?;
            let key = required_text(content, &raw, "key", rendering)?;
            let splits = field(content, &raw, "splits", false)?
                .map_or_else(|| Ok(vec![]), |id| scalar_set(content, id, rendering))?;
            let dimensions = dimensions.map_or_else(|| Ok(vec![]), |id| items(content, id))?;
            let names = unique(
                dimensions
                    .iter()
                    .map(|v| required_text(content, v, "name", rendering))
                    .collect::<Result<Vec<_>, _>>()?,
            );
            let unit = text_default(content, &raw, "unit", "", rendering)?;
            let direction = text_default(content, &raw, "direction", "", rendering)?;
            let mut values = vec![];
            for dimension in &dimensions {
                let name = required_text(content, dimension, "name", rendering)?;
                let registered = field(content, dimension, "values", false)?
                    .map(|id| scalar_set(content, id, rendering))
                    .transpose()?;
                insert(&mut values, name, registered);
            }
            insert(
                &mut metrics,
                key.clone(),
                Metric {
                    key,
                    splits,
                    dimensions: names,
                    unit,
                    direction,
                    dimension_values: values,
                },
            );
        }
        let baseline_refs = optional_items(content, &root, "baselines")?
            .iter()
            .map(|v| {
                Ok((
                    required_text(content, v, "id", rendering)?,
                    required_text(content, v, "revision", rendering)?,
                ))
            })
            .collect::<Result<Vec<_>, ScienceError>>()?;
        let baselines = unique(baseline_refs.clone());
        let hypothesis_fields = field(content, &root, "hypothesis_fields", false)?
            .filter(|id| matches!(content.node(*id), Some(Node::Object(_))));
        let mut datasets = vec![];
        for raw in optional_items(content, &root, "datasets")? {
            let id = required_text(content, &raw, "id", rendering)?;
            let revision = required_text(content, &raw, "revision", rendering)?;
            let held_out_labels = truth(content, required(content, &raw, "held_out_labels")?)?;
            insert(
                &mut datasets,
                id.clone(),
                Dataset {
                    id,
                    revision,
                    held_out_labels,
                },
            );
        }
        let registrations = optional_items(content, &root, "interfaces")?;
        let interfaces = unique(
            registrations
                .iter()
                .map(|v| {
                    append(&[
                        &required_text(content, v, "name", rendering)?,
                        &String::from("/v"),
                        &required_text(content, v, "version", rendering)?,
                    ])
                })
                .collect::<Result<Vec<_>, ScienceError>>()?,
        );
        let mut interface_specs = vec![];
        for raw in &registrations {
            let Value::Node(id) = raw else {
                return Err(ScienceError::Attribute);
            };
            let spec = InterfaceSpec::from_registration(content, *id, rendering)?;
            insert(&mut interface_specs, spec.reference.clone(), spec);
        }
        let mut validators = vec![];
        for raw in optional_items(content, &root, "validators")? {
            let metadata = Value::Node(required(content, &raw, "metadata")?);
            let name = required_text(content, &metadata, "name", rendering)?;
            let Value::Node(id) = raw else {
                return Err(ScienceError::Type);
            };
            insert(&mut validators, name, id);
        }
        let limits = field(content, &root, "limits", false)?;
        let tester = field(content, &root, "tester", false)?;
        Ok(Self {
            revision,
            content,
            metrics,
            baseline_refs,
            baselines,
            hypothesis_fields,
            datasets,
            interfaces,
            interface_specs,
            validators,
            limits,
            tester,
            rendering,
        })
    }
    #[must_use]
    pub fn baseline_ids(&self) -> Vec<String> {
        unique(self.baselines.iter().map(|(id, _)| id.clone()))
    }
    /// # Errors
    /// Reports malformed legacy repository declarations at their consumed phase.
    pub fn code_repositories(&self, trust: &str) -> Result<Vec<String>, ScienceError> {
        let Some(listed) = field(
            self.content,
            &Value::Node(self.content.root()),
            "code_repositories",
            false,
        )?
        else {
            return Ok(vec![]);
        };
        let Some(values) = field(self.content, &Value::Node(listed), trust, false)? else {
            return Ok(vec![]);
        };
        let repositories = items(self.content, values)?
            .iter()
            .map(|v| {
                let Value::Node(id) = v else {
                    if let Value::Text(s) = v {
                        return Ok(s.lowercase());
                    }
                    return Err(ScienceError::InvalidNode);
                };
                if let Node::String(s) = node(self.content, *id)? {
                    Ok(s.lowercase())
                } else {
                    Err(ScienceError::Attribute)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(unique(repositories))
    }
    #[must_use]
    pub fn evaluator(&self) -> Option<NodeId> {
        self.content
            .field(self.content.root(), "evaluator")
            .filter(|v| matches!(self.content.node(*v), Some(Node::Object(_))))
    }
    /// # Errors
    /// Source integer rendering can fail only when a legacy reason is needed.
    pub fn legacy_problem(&self) -> Result<Option<String>, ScienceError> {
        if self.content.field(self.content.root(), "gates").is_none() && self.evaluator().is_some()
        {
            return Ok(None);
        }
        let mut builder = cannery_core::json::DocumentBuilder::new();
        let id = builder
            .push(Node::Integer(self.revision.clone()))
            .map_err(|_| ScienceError::InvalidNode)?;
        let document = builder.finish(id).map_err(|_| ScienceError::InvalidNode)?;
        let revision = text::str_value(&document, id, self.rendering.nesting_budget)?;
        Ok(Some(append(&[
            &String::from("science revision "),
            &revision,
            &String::from(
                " uses built-in gates, which moved to the stock evaluator; register a new revision with an evaluator",
            ),
        ])?))
    }
    /// # Errors
    /// Missing scorer is a source key failure rather than an invented default.
    pub fn scorer(&self) -> Result<NodeId, ScienceError> {
        required(self.content, &Value::Node(self.content.root()), "scorer")
    }
    /// # Errors
    /// Retains malformed project-field properties' iteration failures.
    pub fn hypothesis_facets(&self) -> Result<Vec<String>, ScienceError> {
        let Some(fields) = self.hypothesis_fields else {
            return Ok(vec![]);
        };
        let Some(properties) = field(self.content, &Value::Node(fields), "properties", false)?
        else {
            return Ok(vec![]);
        };
        Ok(unique(
            items(self.content, properties)?
                .iter()
                .map(|value| {
                    append(&[
                        &String::from("hypothesis."),
                        &text(self.content, value, self.rendering)?,
                    ])
                })
                .collect::<Result<Vec<_>, _>>()?,
        ))
    }
}

#[must_use]
pub fn path_segment(value: &String) -> bool {
    let p = value.codepoints();
    !p.is_empty()
        && !p.contains(&47)
        && !p.contains(&92)
        && !p.windows(2).any(|w| w == [46, 46])
        && p.first() != Some(&46)
}
fn repeated(values: Vec<String>) -> bool {
    let mut seen = vec![];
    for value in values {
        if seen.contains(&value) {
            return true;
        }
        seen.push(value);
    }
    false
}
/// Semantic publication checks; schema validation remains the caller's responsibility.
/// # Errors
/// Returns the first source-ordered violation or consumed-value source exception.
pub fn check_science(
    content: &Document,
    rendering: RenderingContext,
) -> Result<Science<'_>, ScienceError> {
    let root = Value::Node(content.root());
    let metrics = optional_items(content, &root, "metrics")?;
    if repeated(
        metrics
            .iter()
            .map(|v| required_text(content, v, "key", rendering))
            .collect::<Result<Vec<_>, _>>()?,
    ) {
        return Err(violation("/metrics", "metric key is declared twice"));
    }
    for kind in ["datasets", "baselines"] {
        for (index, raw) in optional_items(content, &root, kind)?.iter().enumerate() {
            for name in ["id", "revision"] {
                if !path_segment(&required_text(content, raw, name, rendering)?) {
                    return Err(violation(
                        &format!("/{kind}/{index}/{name}"),
                        "must be usable as a directory name",
                    ));
                }
            }
        }
    }
    let baselines = optional_items(content, &root, "baselines")?;
    if repeated(
        baselines
            .iter()
            .map(|v| {
                append(&[
                    &required_text(content, v, "id", rendering)?,
                    &String::from("@"),
                    &required_text(content, v, "revision", rendering)?,
                ])
            })
            .collect::<Result<Vec<_>, ScienceError>>()?,
    ) {
        return Err(violation("/baselines", "baseline is declared twice"));
    }
    let interfaces = optional_items(content, &root, "interfaces")?;
    if repeated(
        interfaces
            .iter()
            .map(|v| {
                append(&[
                    &required_text(content, v, "name", rendering)?,
                    &String::from("/v"),
                    &required_text(content, v, "version", rendering)?,
                ])
            })
            .collect::<Result<Vec<_>, ScienceError>>()?,
    ) {
        return Err(violation("/interfaces", "interface is declared twice"));
    }
    let validators = optional_items(content, &root, "validators")?;
    let names = validators
        .iter()
        .map(|v| {
            required_text(
                content,
                &Value::Node(required(content, v, "metadata")?),
                "name",
                rendering,
            )
        })
        .collect::<Result<Vec<_>, ScienceError>>()?;
    if repeated(names.clone()) {
        return Err(violation("/validators", "validator is declared twice"));
    }
    for (index, interface) in interfaces.iter().enumerate() {
        if let Some(id) = field(content, interface, "validator", false)?
            && !matches!(content.node(id), Some(Node::Null))
        {
            let registered = if let Node::String(s) = node(content, id)? {
                names.contains(s)
            } else {
                false
            };
            if !registered {
                return Err(violation(
                    &format!("/interfaces/{index}/validator"),
                    "validator step is not registered",
                ));
            }
        }
    }
    Science::new(BigInt::zero(), content, rendering)
}
/// # Errors
/// Returns source-ordered dashboard references, without changing stored content.
pub fn check_dashboard(
    science: &Science<'_>,
    document: &Document,
    rendering: RenderingContext,
) -> Result<(), ScienceError> {
    let views = optional_items(document, &Value::Node(document.root()), "views")?;
    if repeated(
        views
            .iter()
            .map(|v| required_text(document, v, "id", rendering))
            .collect::<Result<Vec<_>, _>>()?,
    ) {
        return Err(violation("/views", "view id is declared twice"));
    }
    let mut baselines = science.baseline_ids();
    baselines.push(String::from("control"));
    for (index, view) in views.iter().enumerate() {
        let path = format!("/views/{index}");
        let key = required_text(document, view, "metric", rendering)?;
        let metric = science
            .metrics
            .iter()
            .find(|(name, _)| name == &key)
            .map(|(_, metric)| metric)
            .ok_or_else(|| violation(&format!("{path}/metric"), "unknown metric"))?;
        if let Some(split) = field(document, view, "split", false)?
            && !matches!(document.node(split), Some(Node::Null))
            && !metric
                .splits
                .contains(&scalar(document, &Value::Node(split), rendering)?)
        {
            return Err(violation(
                &format!("{path}/split"),
                "metric is not measured on this split",
            ));
        }
        let mut dimensions = metric.dimensions.clone();
        dimensions.push(String::from("track"));
        dimensions.extend(science.hypothesis_facets()?);
        for (position, name) in optional_items(document, view, "group_by")?
            .iter()
            .enumerate()
        {
            let found = if let Scalar::Text(name) = scalar(document, name, rendering)? {
                dimensions.contains(&name)
            } else {
                false
            };
            if !found {
                return Err(violation(
                    &format!("{path}/group_by/{position}"),
                    "unknown dimension",
                ));
            }
        }
        for name in optional_items(document, view, "filters")? {
            let found = if let Scalar::Text(name) = scalar(document, &name, rendering)? {
                dimensions.contains(&name)
            } else {
                false
            };
            if !found {
                return Err(violation(&format!("{path}/filters"), "unknown dimension"));
            }
        }
        if let Some(baseline) = field(document, view, "baseline", false)?
            && !matches!(document.node(baseline), Some(Node::Null))
        {
            let found =
                if let Scalar::Text(name) = scalar(document, &Value::Node(baseline), rendering)? {
                    baselines.contains(&name)
                } else {
                    false
                };
            if !found {
                return Err(violation(&format!("{path}/baseline"), "unknown baseline"));
            }
        }
    }
    Ok(())
}
/// # Errors
/// Checks the metric, splits and optional exact registered baseline in source order.
pub fn check_hypothesis(
    science: &Science<'_>,
    document: &Document,
    rendering: RenderingContext,
) -> Result<(), ScienceError> {
    let root = Value::Node(document.root());
    let plan = Value::Node(required(document, &root, "plan")?);
    let key = required_text(document, &plan, "primary_metric", rendering)?;
    let metric = science
        .metrics
        .iter()
        .find(|(name, _)| name == &key)
        .map(|(_, metric)| metric)
        .ok_or_else(|| violation("/plan/primary_metric", "unknown metric"))?;
    for name in ["selection_splits", "confirmation_splits"] {
        for (position, split) in optional_items(document, &plan, name)?.iter().enumerate() {
            if !metric.splits.contains(&scalar(document, split, rendering)?) {
                return Err(violation(
                    &format!("/plan/{name}/{position}"),
                    "metric is not measured on this split",
                ));
            }
        }
    }
    if let Some(control) = field(document, &root, "control", false)?
        && !matches!(document.node(control), Some(Node::Null))
    {
        let control = Value::Node(control);
        let baseline = (
            required_text(document, &control, "id", rendering)?,
            required_text(document, &control, "revision", rendering)?,
        );
        if !science.baselines.contains(&baseline) {
            return Err(violation("/control", "baseline revision is not registered"));
        }
    }
    Ok(())
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
