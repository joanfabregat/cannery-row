//! Lossless stock and step policy parsing with source-ordered schema diagnostics.
use crate::{
    cli_depth::{self, PolicyEntryPoint},
    config::{EvaluationPolicy, PolicyLoadError, PolicyLoader},
    gates::{Control, GateContext},
    launcher::PosixPath,
};
use cannery_core::{
    contracts::{ContractError, ContractKind, ContractValidator, ContractViolation, comparison},
    json::{self, Document, DocumentBuilder, Node, NodeId},
    text,
};
use std::{
    collections::{BTreeSet, HashSet},
    fs,
    sync::Arc,
};

/// Static diagnostic category; no input contents are formatted in errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Configuration,
    Value,
    Recursion,
    Encoding,
    Invariant,
}
/// Schema diagnostics retain source multiplicity and lossless prefixed paths.
/// Public messages use sanitized Rust wording; unique paths remain a compatibility view.
pub struct PolicyError {
    pub kind: ErrorKind,
    pub paths: Vec<String>,
    pub violations: Vec<ContractViolation>,
    display_limit: Option<usize>,
}
impl PolicyError {
    /// Stock errors report all schema violations; step errors display the first five.
    /// Manual parsing and filesystem errors have no schema violation entries.
    #[must_use]
    pub fn displayed_violations(&self) -> &[ContractViolation] {
        &self.violations[..self
            .display_limit
            .unwrap_or(self.violations.len())
            .min(self.violations.len())]
    }
}
impl std::fmt::Debug for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolicyError")
            .field("kind", &self.kind)
            .field("paths", &"[redacted]")
            .field("violations", &"[redacted]")
            .field("display_limit", &self.display_limit)
            .finish()
    }
}
impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("evaluation policy was refused")
    }
}
impl std::error::Error for PolicyError {}
fn fail(kind: ErrorKind, path: &str) -> PolicyError {
    PolicyError {
        kind,
        paths: if path.is_empty() {
            vec![]
        } else {
            vec![String::from(path)]
        },
        violations: Vec::new(),
        display_limit: None,
    }
}
fn invariant() -> PolicyError {
    fail(ErrorKind::Invariant, "")
}
impl From<json::BuildError> for PolicyError {
    fn from(_: json::BuildError) -> Self {
        invariant()
    }
}
impl From<ContractError> for PolicyError {
    fn from(e: ContractError) -> Self {
        fail(
            match e {
                // Embedded project-schema failures belong to research
                // contracts; neither runner policy kind evaluates that path.
                ContractError::Schema => ErrorKind::Invariant,
            },
            "",
        )
    }
}

/// Original stock document plus the source's flattened baseline projection.
pub struct StockPolicy {
    pub evaluator_id: String,
    pub revision: String,
    pub default_control: Option<Control>,
    pub document: Arc<Document>,
    pub gate_document: Document,
}
impl StockPolicy {
    #[must_use]
    pub fn context<'a>(
        &'a self,
        metrics: &'a Document,
        measurements: &'a Document,
        control: Option<&'a Control>,
        nesting_budget: usize,
    ) -> GateContext<'a> {
        GateContext {
            metrics,
            policy: &self.gate_document,
            measurements,
            control,
            nesting_budget,
        }
    }
}
/// `document` is the step manifest, not its registration envelope.
pub struct StepPolicy {
    pub evaluator_id: String,
    pub revision: String,
    pub document: Arc<Document>,
    pub envelope: Arc<Document>,
    pub name: String,
    pub needs_data_root: bool,
}
pub enum Policy {
    Stock(StockPolicy),
    Step(StepPolicy),
}

fn field(doc: &Document, id: NodeId, key: &str) -> Result<NodeId, PolicyError> {
    doc.field(id, key).ok_or_else(invariant)
}
fn text(doc: &Document, id: NodeId) -> Result<String, PolicyError> {
    if let Some(Node::String(s)) = doc.node(id) {
        Ok(s.clone())
    } else {
        Err(invariant())
    }
}
fn array(doc: &Document, id: NodeId) -> Result<&[NodeId], PolicyError> {
    if let Some(Node::Array(a)) = doc.node(id) {
        Ok(a)
    } else {
        Err(invariant())
    }
}
fn object(doc: &Document, id: NodeId) -> Result<&[(String, NodeId)], PolicyError> {
    if let Some(Node::Object(a)) = doc.node(id) {
        Ok(a)
    } else {
        Err(invariant())
    }
}
fn joined(parts: &[&String]) -> String {
    cannery_core::text::from_codepoints(
        parts
            .iter()
            .flat_map(|p| p.chars().map(u32::from))
            .collect(),
    )
    .unwrap_or_default()
}
fn validate(
    doc: &Document,
    kind: ContractKind,
    point: PolicyEntryPoint,
    _budget: usize,
    prefix: &str,
) -> Result<(), PolicyError> {
    cli_depth::check_validation_walk(doc, point.validation_edges())
        .map_err(|_| fail(ErrorKind::Recursion, ""))?;
    let mut violations = ContractValidator::new()?.violations(kind, doc)?;
    if violations.is_empty() {
        Ok(())
    } else {
        let p = String::from(prefix);
        for violation in &mut violations {
            violation.path = joined(&[&p, &violation.path]);
        }
        let paths: BTreeSet<_> = violations
            .iter()
            .map(|v| v.path.codepoints().clone())
            .collect();
        let paths = paths
            .into_iter()
            .map(|path| cannery_core::text::from_codepoints(path).ok_or_else(invariant))
            .collect::<Result<Vec<_>, _>>()?;
        Err(PolicyError {
            kind: ErrorKind::Configuration,
            paths,
            violations,
            display_limit: if kind == ContractKind::StepManifest {
                Some(5)
            } else {
                None
            },
        })
    }
}
fn subtree(doc: &Document, id: NodeId) -> Result<Document, PolicyError> {
    let mut b = DocumentBuilder::new();
    let root = b.import(doc, id)?;
    Ok(b.finish(root)?)
}
fn less(doc: &Document, a: NodeId, b: NodeId) -> Result<bool, PolicyError> {
    comparison::less(doc, a, doc, b).map_err(|_| invariant())
}
fn render_interval(doc: &Document, ids: &[NodeId], budget: usize) -> Result<(), PolicyError> {
    for &id in ids {
        text::str_value(doc, id, budget).map_err(|e| {
            fail(
                match e {
                    text::RenderError::IntegerLimit => ErrorKind::Value,
                    text::RenderError::Recursion => ErrorKind::Recursion,
                    text::RenderError::Encoding => ErrorKind::Encoding,
                    text::RenderError::InvalidNode => ErrorKind::Invariant,
                },
                "",
            )
        })?;
    }
    Ok(())
}

/// Schema validation precedes duplicate and interval semantics.
/// # Errors
/// Source configuration, diagnostic value/recursion, or arena invariant failure.
pub fn parse_policy(
    document: Arc<Document>,
    point: PolicyEntryPoint,
    budget: usize,
) -> Result<StockPolicy, PolicyError> {
    let d = &*document;
    let root = d.root();
    validate(d, ContractKind::EvaluatorConfig, point, budget, "")?;
    let gates = field(d, root, "gates")?;
    let mut seen = HashSet::new();
    for &g in array(d, gates)? {
        if !seen.insert(text(d, field(d, g, "id")?)?) {
            return Err(fail(ErrorKind::Configuration, "/gates"));
        }
    }
    let bases = array(d, field(d, root, "baselines")?)?;
    let mut seen = HashSet::new();
    let at = String::from("@");
    for &base in bases {
        let id = text(d, field(d, base, "id")?)?;
        let rev = text(d, field(d, base, "revision")?)?;
        if !seen.insert(joined(&[&id, &at, &rev])) {
            return Err(fail(ErrorKind::Configuration, "/baselines"));
        }
    }
    let mut b = DocumentBuilder::new();
    let gates = b.import(d, gates)?;
    let mut flattened = Vec::new();
    for (index, &base) in bases.iter().enumerate() {
        flattened.push(flatten_baseline(d, &mut b, base, index, budget)?);
    }
    let evaluator = field(d, root, "evaluator")?;
    let evaluator_id = text(d, field(d, evaluator, "id")?)?;
    let revision = text(d, field(d, evaluator, "revision")?)?;
    let default_control = d
        .field(root, "default_control")
        .map(|v| {
            Ok::<Control, PolicyError>(Control {
                id: text(d, field(d, v, "id")?)?,
                revision: text(d, field(d, v, "revision")?)?,
            })
        })
        .transpose()?;
    let default = if let Some(c) = &default_control {
        let i = b.push(Node::String(c.id.clone()))?;
        let r = b.push(Node::String(c.revision.clone()))?;
        b.push(Node::Array(vec![i, r]))?
    } else {
        b.push(Node::Null)?
    };
    let id = b.push(Node::String(evaluator_id.clone()))?;
    let revision_node = b.push(Node::String(revision.clone()))?;
    let bases = b.push(Node::Array(flattened))?;
    let root = b.push(Node::Object(vec![
        (String::from("evaluator_id"), id),
        (String::from("revision"), revision_node),
        (String::from("gates"), gates),
        (String::from("default_control"), default),
        (String::from("baselines"), bases),
    ]))?;
    Ok(StockPolicy {
        evaluator_id,
        revision,
        default_control,
        document,
        gate_document: b.finish(root)?,
    })
}
fn identifier(s: &String) -> bool {
    let owned_points = s.codepoints();
    let mut p = owned_points.as_slice();
    if p.last() == Some(&10) {
        p = &p[..p.len() - 1];
    }
    (1..=256).contains(&p.len())
        && u8::try_from(p[0]).is_ok_and(|c| c.is_ascii_alphanumeric())
        && p.iter().all(|&c| {
            u8::try_from(c).is_ok_and(|c| c.is_ascii_alphanumeric() || b"._:/+@-".contains(&c))
        })
}
/// Manual envelope checks precede whole manifest validation and input semantics.
/// # Errors
/// Source configuration, diagnostic value/recursion, or arena invariant failure.
pub fn parse_step_policy(
    envelope: Arc<Document>,
    point: PolicyEntryPoint,
    budget: usize,
) -> Result<StepPolicy, PolicyError> {
    let d = &*envelope;
    let root = d.root();
    let Some(Node::Object(entries)) = d.node(root) else {
        return Err(fail(ErrorKind::Configuration, ""));
    };
    if let Some((key, _)) = entries
        .iter()
        .filter(|(k, _)| {
            !["schema_version", "evaluator", "step"]
                .iter()
                .any(|s| k.equals_utf8(s))
        })
        .min_by(|a, b| a.0.codepoints().cmp(&b.0.codepoints()))
    {
        return Err(PolicyError {
            kind: ErrorKind::Configuration,
            paths: vec![joined(&[&String::from("/"), key])],
            violations: Vec::new(),
            display_limit: None,
        });
    }
    if !d
        .field(root, "schema_version")
        .is_some_and(|v| matches!(d.node(v),Some(Node::String(s)) if s.equals_utf8("0.2")))
    {
        return Err(fail(ErrorKind::Configuration, "/schema_version"));
    }
    let evaluator = d
        .field(root, "evaluator")
        .ok_or_else(|| fail(ErrorKind::Configuration, "/evaluator"))?;
    let fields = object(d, evaluator).map_err(|_| fail(ErrorKind::Configuration, "/evaluator"))?;
    if fields.len() != 2
        || d.field(evaluator, "id").is_none()
        || d.field(evaluator, "revision").is_none()
    {
        return Err(fail(ErrorKind::Configuration, "/evaluator"));
    }
    let mut registration = Vec::new();
    for key in ["id", "revision"] {
        let s = text(d, field(d, evaluator, key)?)
            .map_err(|_| fail(ErrorKind::Configuration, &format!("/evaluator/{key}")))?;
        if !identifier(&s) {
            return Err(fail(ErrorKind::Configuration, &format!("/evaluator/{key}")));
        }
        registration.push(s);
    }
    let document = Arc::new(if let Some(step) = d.field(root, "step") {
        subtree(d, step)?
    } else {
        let mut b = DocumentBuilder::new();
        let root = b.push(Node::Null)?;
        b.finish(root)?
    });
    validate(
        &document,
        ContractKind::StepManifest,
        point,
        budget,
        "/step",
    )?;
    let root = document.root();
    let spec = field(&document, root, "spec")?;
    if !text(&document, field(&document, spec, "role")?)?.equals_utf8("evaluator") {
        return Err(fail(ErrorKind::Configuration, "/step/spec/role"));
    }
    let inputs = field(&document, field(&document, spec, "inputs")?, "artifacts")?;
    let mut names = HashSet::new();
    let mut needs_data_root = false;
    for (index, &a) in array(&document, inputs)?.iter().enumerate() {
        let name = text(&document, field(&document, a, "name")?)?;
        let source = text(&document, field(&document, a, "from")?)?;
        let where_ = format!("/step/spec/inputs/artifacts/{index}");
        if !names.insert(name.clone()) {
            return Err(fail(ErrorKind::Configuration, &format!("{where_}/name")));
        }
        if source.equals_utf8("step") {
            return Err(fail(ErrorKind::Configuration, &format!("{where_}/from")));
        }
        if name.equals_utf8("claimed_sheet") && source.equals_utf8("attempt") {
            return Err(fail(ErrorKind::Configuration, &format!("{where_}/name")));
        }
        if (name.equals_utf8("evidence") || name.equals_utf8("manifest"))
            && !source.equals_utf8("attempt")
        {
            return Err(fail(ErrorKind::Configuration, &format!("{where_}/from")));
        }
        needs_data_root |= source.equals_utf8("baseline") || source.equals_utf8("dataset");
    }
    let name = text(
        &document,
        field(&document, field(&document, root, "metadata")?, "name")?,
    )?;
    Ok(StepPolicy {
        evaluator_id: registration.remove(0),
        revision: registration.remove(0),
        document,
        envelope,
        name,
        needs_data_root,
    })
}
/// Dispatch uses presence of `step`, including a null value.
/// # Errors
/// Preserves parsing and semantic refusal categories.
pub fn parse(
    document: Arc<Document>,
    point: PolicyEntryPoint,
    budget: usize,
) -> Result<Policy, PolicyError> {
    if document.field(document.root(), "step").is_some() {
        parse_step_policy(document, point, budget).map(Policy::Step)
    } else {
        parse_policy(document, point, budget).map(Policy::Stock)
    }
}

/// Real filesystem loader. The explicit profile is the source invocation context.
pub struct FilePolicyLoader {
    pub entry_point: PolicyEntryPoint,
    pub repr_nesting_budget: usize,
}
impl FilePolicyLoader {
    /// Strict UTF8, universal newlines and JSON text parsing, then source dispatch.
    /// # Errors
    /// Configuration errors differ from uncaught encoding/value/recursion failures.
    pub fn load_policy(&self, path: &PosixPath) -> Result<Policy, PolicyError> {
        let native = native_path(&path.text())?;
        let bytes = fs::read(native).map_err(|_| fail(ErrorKind::Configuration, ""))?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| fail(ErrorKind::Configuration, ""))?
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        let d = json::decode_str(&text, cli_depth::JSON_CONTAINERS).map_err(|e| {
            fail(
                match e {
                    json::DecodeError::IntegerLimit => ErrorKind::Value,
                    json::DecodeError::Recursion => ErrorKind::Recursion,
                    json::DecodeError::Syntax { .. } | json::DecodeError::Encoding => {
                        ErrorKind::Configuration
                    }
                },
                "",
            )
        })?;
        parse(Arc::new(d), self.entry_point, self.repr_nesting_budget)
    }
}
impl PolicyLoader for FilePolicyLoader {
    fn load(&self, path: &PosixPath) -> Result<EvaluationPolicy, PolicyLoadError> {
        self.load_policy(path)
            .map(|p| match p {
                Policy::Stock(p) => EvaluationPolicy::Stock(p.document),
                Policy::Step(p) => EvaluationPolicy::Step {
                    document: p.envelope,
                    needs_data_root: p.needs_data_root,
                },
            })
            .map_err(|e| match e.kind {
                ErrorKind::Configuration | ErrorKind::Invariant => PolicyLoadError::Configuration,
                ErrorKind::Encoding => PolicyLoadError::Encoding,
                ErrorKind::Value => PolicyLoadError::Value,
                ErrorKind::Recursion => PolicyLoadError::Recursion,
            })
    }
}
fn native_path(text: &String) -> Result<std::path::PathBuf, PolicyError> {
    use std::os::unix::ffi::OsStringExt;
    let mut bytes = Vec::new();
    for point in text.codepoints() {
        if (0xdc80..=0xdcff).contains(&point) {
            bytes.push(u8::try_from(point - 0xdc00).map_err(|_| invariant())?);
        } else {
            let c = char::from_u32(point).ok_or_else(|| fail(ErrorKind::Encoding, ""))?;
            bytes.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
        }
    }
    if bytes.contains(&0) {
        return Err(fail(ErrorKind::Value, ""));
    }
    Ok(std::path::PathBuf::from(std::ffi::OsString::from_vec(
        bytes,
    )))
}
fn flatten_baseline(
    d: &Document,
    b: &mut DocumentBuilder,
    base: NodeId,
    index: usize,
    budget: usize,
) -> Result<NodeId, PolicyError> {
    let id = text(d, field(d, base, "id")?)?;
    let rev = text(d, field(d, base, "revision")?)?;
    let label = if let Some(label) = d.field(base, "label") {
        text(d, label)?
    } else {
        joined(&[&id, &String::from(" "), &rev])
    };
    let mut slices = HashSet::new();
    let mut measurements = Vec::new();
    for (position, &m) in array(d, field(d, base, "measurements")?)?
        .iter()
        .enumerate()
    {
        let metric = text(d, field(d, m, "metric")?)?;
        let split = text(d, field(d, m, "split")?)?;
        let mut dims = if let Some(dim) = d.field(m, "dimensions") {
            object(d, dim)?
                .iter()
                .map(|(name, value)| Ok((name.clone(), text(d, *value)?)))
                .collect::<Result<Vec<_>, PolicyError>>()?
        } else {
            vec![]
        };
        dims.sort_by_key(|a| a.0.codepoints());
        let where_ = format!("/baselines/{index}/measurements/{position}");
        if !slices.insert((metric.clone(), split.clone(), dims.clone())) {
            return Err(fail(ErrorKind::Configuration, &where_));
        }
        let value = field(d, m, "value")?;
        if let Some(u) = d.field(m, "uncertainty") {
            let low = field(d, u, "lower")?;
            let high = field(d, u, "upper")?;
            if less(d, value, low)? || less(d, high, value)? {
                render_interval(d, &[low, high, value], budget)?;
                return Err(fail(
                    ErrorKind::Configuration,
                    &format!("{where_}/uncertainty"),
                ));
            }
        }
        let metric = b.push(Node::String(metric))?;
        let split = b.push(Node::String(split))?;
        let value = b.import(d, value)?;
        let dim_fields = dims
            .into_iter()
            .map(|(k, v)| Ok((k, b.push(Node::String(v))?)))
            .collect::<Result<Vec<_>, PolicyError>>()?;
        let dims = b.push(Node::Object(dim_fields))?;
        measurements.push(b.push(Node::Object(vec![
            (String::from("metric"), metric),
            (String::from("split"), split),
            (String::from("dimensions"), dims),
            (String::from("value"), value),
        ]))?);
    }
    let id = b.push(Node::String(id))?;
    let rev = b.push(Node::String(rev))?;
    let label = b.push(Node::String(label))?;
    let measurements = b.push(Node::Array(measurements))?;
    Ok(b.push(Node::Object(vec![
        (String::from("id"), id),
        (String::from("revision"), rev),
        (String::from("label"), label),
        (String::from("measurements"), measurements),
    ]))?)
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
