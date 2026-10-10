//! Ordered registration checks, independent of schema validation and storage.
use crate::science::{
    self, RenderingContext, Scalar, Science, ScienceError, Value, field, items, node,
    optional_items, required, required_text, scalar, text,
};
use cannery_core::{
    contracts::comparison,
    json::{Document, Node, NodeId},
    text, unicode,
};
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::One;

pub const EVIDENCE_INTERFACE: &str = "cr-evidence/v0.2";
/// The run document an experiment workflow's last step outputs as `run`.
pub const RUN_INTERFACE: &str = "cr-run/v0.2";
pub const DEFAULT_SETUP_DEADLINE_SECONDS: u32 = 600;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Producer,
    Scorer,
    Validator,
    Experiment,
    Policy,
    Decider,
}
impl Role {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Producer => "producer",
            Self::Scorer => "scorer",
            Self::Validator => "validator",
            Self::Experiment => "experiment",
            Self::Policy => "policy",
            Self::Decider => "decider",
        }
    }
    #[must_use]
    pub const fn trust(self) -> TrustClass {
        if matches!(self, Self::Producer | Self::Experiment) {
            TrustClass::Candidate
        } else {
            TrustClass::Trusted
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustClass {
    Candidate,
    Trusted,
}
/// Source helper fallback: every name outside the two blind roles is trusted.
#[must_use]
pub fn trust_class(role: &String) -> TrustClass {
    if role.equals_utf8("producer") || role.equals_utf8("experiment") {
        TrustClass::Candidate
    } else {
        TrustClass::Trusted
    }
}
impl TrustClass {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Trusted => "trusted",
        }
    }
}
fn violation(path: &String, suffix: &str, message: &'static str) -> ScienceError {
    let mut points = path.codepoints().clone();
    points.extend(suffix.chars().map(u32::from));
    match cannery_core::text::from_codepoints(points) {
        Some(path) => ScienceError::Validation { path, message },
        None => ScienceError::InvalidNode,
    }
}
fn matches_text(document: &Document, id: NodeId, wanted: &str) -> bool {
    matches!(document.node(id),Some(Node::String(t)) if t.equals_utf8(wanted))
}
fn raw_string(
    document: &Document,
    id: NodeId,
    error: ScienceError,
) -> Result<&String, ScienceError> {
    if let Node::String(s) = node(document, id)? {
        Ok(s)
    } else {
        Err(error)
    }
}
/// Exact resource quantity with an ASCII decimal, a known suffix, and at most 1024 bytes.
/// # Errors
/// Rejects invalid syntax and oversized quantities.
pub fn quantity(value: &String) -> Result<BigRational, ScienceError> {
    if value.len() > 1024 || !value.is_ascii() {
        return Err(ScienceError::Value);
    }
    let p = value.codepoints();
    let mut i = 0;
    while i < p.len() && (48..=57).contains(&p[i]) {
        i += 1;
    }
    if i == 0 {
        return Err(ScienceError::Value);
    }
    let mut digits = p[..i]
        .iter()
        .map(|n| u8::try_from(*n).map_err(|_| ScienceError::Value))
        .collect::<Result<Vec<_>, _>>()?;
    let mut places = 0;
    if p.get(i) == Some(&46) {
        i += 1;
        let start = i;
        while i < p.len() && (48..=57).contains(&p[i]) {
            digits.push(u8::try_from(p[i]).map_err(|_| ScienceError::Value)?);
            i += 1;
        }
        places = i - start;
        if places == 0 {
            return Err(ScienceError::Value);
        }
    }
    let suffix = p[i..]
        .iter()
        .map(|n| char::from_u32(*n).ok_or(ScienceError::Value))
        .collect::<Result<String, _>>()?;
    let multiplier = match suffix.as_str() {
        "" => BigRational::one(),
        "m" => BigRational::new(BigInt::one(), BigInt::from(1000)),
        "k" => BigRational::from_integer(BigInt::from(1000)),
        "M" => BigRational::from_integer(BigInt::from(10u32).pow(6)),
        "G" => BigRational::from_integer(BigInt::from(10u32).pow(9)),
        "T" => BigRational::from_integer(BigInt::from(10u32).pow(12)),
        "P" => BigRational::from_integer(BigInt::from(10u32).pow(15)),
        "Ki" => BigRational::from_integer(BigInt::from(2u32).pow(10)),
        "Mi" => BigRational::from_integer(BigInt::from(2u32).pow(20)),
        "Gi" => BigRational::from_integer(BigInt::from(2u32).pow(30)),
        "Ti" => BigRational::from_integer(BigInt::from(2u32).pow(40)),
        "Pi" => BigRational::from_integer(BigInt::from(2u32).pow(50)),
        _ => return Err(ScienceError::Value),
    };
    let numerator = BigInt::parse_bytes(&digits, 10).ok_or(ScienceError::Value)?;
    let places = u32::try_from(places).map_err(|_| ScienceError::Value)?;
    Ok(BigRational::new(numerator, BigInt::from(10u32).pow(places)) * multiplier)
}
#[must_use]
pub fn reserved_env(name: &str) -> bool {
    unicode::nvidia_prefix(name)
}
fn prefix(p: &[u32], s: &str) -> bool {
    p.iter().copied().take(s.len()).eq(s.chars().map(u32::from))
}
fn ascii_alnum(p: u32) -> bool {
    matches!(p,48..=57|65..=90|97..=122)
}
fn count(p: &[u32], accept: impl Fn(u32) -> bool) -> usize {
    p.iter().take_while(|&&c| accept(c)).count()
}
/// Fixed credential patterns with Rust Unicode word and whitespace classification.
#[must_use]
pub fn looks_secret(value: &String) -> bool {
    let p = value.codepoints();
    for i in 0..p.len() {
        let tail = &p[i..];
        if [
            "cr_pat_",
            "cr_svc_",
            "cr_ses_",
            "cr_lease_",
            "cr_upl_",
            "cr_job_",
        ]
        .iter()
        .any(|s| prefix(tail, s))
        {
            return true;
        }
        if prefix(tail, "-----BEGIN ") {
            let rest = &tail[11..];
            let n = count(rest, |c| matches!(c, 65..=90 | 32));
            if (0..=n).any(|j| prefix(&rest[j..], "PRIVATE KEY")) {
                return true;
            }
        }
        let boundary = i == 0 || !unicode::word(p[i - 1]);
        if boundary {
            if prefix(tail, "AKIA")
                && tail.len() >= 20
                && tail[4..20].iter().all(|&c| matches!(c,48..=57|65..=90))
                && (tail.len() == 20 || !unicode::word(tail[20]))
            {
                return true;
            }
            if tail.len() >= 4
                && prefix(tail, "gh")
                && matches!(tail[2], 112 | 111 | 117 | 115 | 114)
                && tail[3] == 95
                && count(&tail[4..], ascii_alnum) >= 20
            {
                return true;
            }
            if prefix(tail, "github_pat_")
                && count(&tail[11..], |c| ascii_alnum(c) || c == 95) >= 20
            {
                return true;
            }
            if tail.len() >= 5
                && prefix(tail, "xox")
                && matches!(tail[3], 97 | 98 | 112 | 114 | 115)
                && tail[4] == 45
                && count(&tail[5..], |c| ascii_alnum(c) || c == 45) >= 10
            {
                return true;
            }
            if prefix(tail, "sk-")
                && count(&tail[3..], |c| ascii_alnum(c) || matches!(c, 95 | 45)) >= 20
            {
                return true;
            }
            if prefix(tail, "AIza")
                && tail.len() >= 39
                && tail[4..39]
                    .iter()
                    .all(|&c| ascii_alnum(c) || matches!(c, 95 | 45))
                && (tail.len() == 39 || !unicode::word(tail[39]))
            {
                return true;
            }
        }
        if prefix(tail, "://") {
            let rest = &tail[3..];
            let n = count(rest, |c| !matches!(c, 47 | 58 | 64) && !text::whitespace(c));
            if n > 0 && rest.get(n) == Some(&58) {
                let rest = &rest[n + 1..];
                let n = count(rest, |c| !matches!(c, 47 | 64) && !text::whitespace(c));
                if n > 0 && rest.get(n) == Some(&64) {
                    return true;
                }
            }
        }
    }
    false
}
/// # Errors
/// Preserves missing-spec and checked setup integer failures.
pub fn setup_deadline(manifest: &Document) -> Result<Option<BigInt>, ScienceError> {
    setup_deadline_at(manifest, manifest.root())
}
pub(crate) fn setup_deadline_at(
    document: &Document,
    manifest: NodeId,
) -> Result<Option<BigInt>, ScienceError> {
    let spec = required(document, &Value::Node(manifest), "spec")?;
    let setup = field(document, &Value::Node(spec), "setup", false)?;
    match setup {
        None => Ok(None),
        Some(id) if matches!(document.node(id), Some(Node::Null)) => Ok(None),
        Some(id) => match field(document, &Value::Node(id), "activeDeadlineSeconds", false)? {
            Some(v) => science::configuration_integer(document, v).map(Some),
            None => Ok(Some(BigInt::from(DEFAULT_SETUP_DEADLINE_SECONDS))),
        },
    }
}
#[derive(Clone, Copy, Debug)]
pub enum Side {
    Inputs,
    Outputs,
}
impl Side {
    const fn name(self) -> &'static str {
        match self {
            Self::Inputs => "inputs",
            Self::Outputs => "outputs",
        }
    }
}
/// # Errors
/// Exposes raw artifact iteration without adding manifest validation.
pub fn artifacts(
    document: &Document,
    manifest: NodeId,
    side: Side,
) -> Result<NodeId, ScienceError> {
    let spec = required(document, &Value::Node(manifest), "spec")?;
    let side = required(document, &Value::Node(spec), side.name())?;
    required(document, &Value::Node(side), "artifacts")
}
/// # Errors
/// The fallback name is evaluated even when id exists, as Python dict.get does.
pub fn source_id(
    document: &Document,
    artifact: NodeId,
    rendering: RenderingContext,
) -> Result<String, ScienceError> {
    // Python resolves the bound get method before evaluating its eager fallback.
    if !matches!(node(document, artifact)?, Node::Object(_)) {
        return Err(ScienceError::Attribute);
    }
    let fallback = required(document, &Value::Node(artifact), "name")?;
    let id = field(document, &Value::Node(artifact), "id", false)?.unwrap_or(fallback);
    text(document, &Value::Node(id), rendering)
}
/// # Errors
/// Retains Python membership behavior for consumed malformed iterables.
pub fn source_pointer(
    document: &Document,
    artifact: NodeId,
    where_: &String,
) -> Result<String, ScienceError> {
    let present = match node(document, artifact)? {
        Node::Object(fields) => fields.iter().any(|(key, _)| key.equals_utf8("id")),
        Node::Array(values) => values.iter().any(|id| matches_text(document, *id, "id")),
        Node::String(value) => value.codepoints().windows(2).any(|s| s == [105, 100]),
        _ => return Err(ScienceError::Type),
    };
    let suffix = if present { "/id" } else { "/name" };
    if let ScienceError::Validation { path, .. } = violation(where_, suffix, "unused") {
        Ok(path)
    } else {
        Err(ScienceError::InvalidNode)
    }
}
fn amount(document: &Document, id: NodeId) -> Result<BigRational, ScienceError> {
    quantity(raw_string(document, id, ScienceError::Type)?)
}
fn greater(
    document: &Document,
    id: NodeId,
    other: &Document,
    rhs: NodeId,
) -> Result<bool, ScienceError> {
    comparison::less(other, rhs, document, id).map_err(|_| ScienceError::Type)
}
fn scalar_text_member(
    document: &Document,
    value: &Value,
    set: &[String],
    rendering: RenderingContext,
) -> Result<bool, ScienceError> {
    Ok(
        if let Scalar::Text(s) = scalar(document, value, rendering)? {
            set.contains(&s)
        } else {
            false
        },
    )
}
fn mapping_entries(document: &Document, id: NodeId) -> Result<&[(String, NodeId)], ScienceError> {
    if let Node::Object(values) = node(document, id)? {
        Ok(values)
    } else {
        Err(ScienceError::Attribute)
    }
}
fn ceiling(science: &Science<'_>) -> Result<Option<NodeId>, ScienceError> {
    science
        .limits
        .map(|id| {
            field(
                science.content,
                &Value::Node(id),
                "resource_ceilings",
                false,
            )
        })
        .transpose()
        .map(Option::flatten)
}
fn check_resources(
    science: &Science<'_>,
    document: &Document,
    resources: NodeId,
    path: &String,
) -> Result<(), ScienceError> {
    let ceilings = ceiling(science)?;
    for kind in ["limits", "requests"] {
        if let Some(values) = field(document, &Value::Node(resources), kind, false)? {
            for (name, amount_id) in mapping_entries(document, values)? {
                let limit = ceilings
                    .map(|id| {
                        if !matches!(science.content.node(id), Some(Node::Object(_))) {
                            return Err(ScienceError::Attribute);
                        }
                        Ok(mapping_entries(science.content, id)?
                            .iter()
                            .find(|(key, _)| key == name)
                            .map(|(_, value)| *value))
                    })
                    .transpose()?
                    .flatten();
                let escaped = name
                    .codepoints()
                    .iter()
                    .flat_map(|&p| match p {
                        126 => vec![126, 48],
                        47 => vec![126, 49],
                        _ => vec![p],
                    })
                    .collect::<Vec<_>>();
                let prefix = format!("/spec/container/resources/{kind}/");
                let mut where_ = path.codepoints().clone();
                where_.extend(prefix.chars().map(u32::from));
                where_.extend(escaped);
                let where_ =
                    cannery_core::text::from_codepoints(where_).ok_or(ScienceError::InvalidNode)?;
                let Some(limit) =
                    limit.filter(|id| !matches!(science.content.node(*id), Some(Node::Null)))
                else {
                    return Err(violation(&where_, "", "no resource ceiling"));
                };
                if amount(document, *amount_id)? > amount(science.content, limit)? {
                    return Err(violation(&where_, "", "resource ceiling exceeded"));
                }
            }
        }
    }
    Ok(())
}
fn check_input(
    science: &Science<'_>,
    document: &Document,
    artifact: &Value,
    role: Role,
    where_: &String,
    rendering: RenderingContext,
) -> Result<(), ScienceError> {
    let from = required(document, artifact, "from")?;
    if matches_text(document, from, "dataset") || matches_text(document, from, "baseline") {
        let Value::Node(id) = artifact else {
            return Err(ScienceError::Type);
        };
        let ident = source_id(document, *id, rendering)?;
        let missing = if matches_text(document, from, "dataset") {
            !science.datasets.iter().any(|(name, _)| name == &ident)
        } else {
            !science.baseline_ids().contains(&ident)
        };
        if missing {
            let p = source_pointer(document, *id, where_)?;
            return Err(violation(&p, "", "input is not registered"));
        }
        if matches_text(document, from, "dataset")
            && role.trust() == TrustClass::Candidate
            && science
                .datasets
                .iter()
                .any(|(name, d)| name == &ident && d.held_out_labels)
        {
            return Err(violation(where_, "", "held-out labels are forbidden"));
        }
    } else if matches_text(document, from, "step") {
        if role == Role::Producer {
            return Err(violation(where_, "/from", "no step precedes a producer"));
        }
        let interface = required_text(document, artifact, "interface", rendering)?;
        // A policy step may read the scorer's verified measurements.
        let readable = science.interfaces.contains(&interface)
            || (role == Role::Policy && interface.equals_utf8(EVIDENCE_INTERFACE));
        if !readable {
            return Err(violation(
                where_,
                "/interface",
                "interface is not registered",
            ));
        }
    }
    Ok(())
}
/// Check schema-valid registration semantics; consumed legacy errors remain distinct.
/// # Errors
/// Returns the first ordered violation or value-free source exception category.
#[allow(
    clippy::too_many_lines,
    reason = "Source registration order determines the first failure"
)]
pub fn check_step(
    science: &Science<'_>,
    document: &Document,
    role: Role,
    path: &String,
    rendering: RenderingContext,
) -> Result<(), ScienceError> {
    check_step_at(science, document, document.root(), role, path, rendering)
}
#[allow(
    clippy::too_many_lines,
    reason = "Source registration order determines the first failure"
)]
fn check_step_at(
    science: &Science<'_>,
    document: &Document,
    manifest: NodeId,
    role: Role,
    path: &String,
    rendering: RenderingContext,
) -> Result<(), ScienceError> {
    let spec = required(document, &Value::Node(manifest), "spec")?;
    let spec = Value::Node(spec);
    if !matches_text(document, required(document, &spec, "role")?, role.name()) {
        return Err(violation(path, "/spec/role", "role differs"));
    }
    let container = required(document, &spec, "container")?;
    let container = Value::Node(container);
    for (index, variable) in optional_items(document, &container, "env")?
        .iter()
        .enumerate()
    {
        if reserved_env(raw_string(
            document,
            required(document, variable, "name")?,
            ScienceError::Attribute,
        )?) {
            return Err(violation(
                path,
                &format!("/spec/container/env/{index}/name"),
                "environment name is reserved",
            ));
        }
        if looks_secret(raw_string(
            document,
            required(document, variable, "value")?,
            ScienceError::Type,
        )?) {
            return Err(violation(
                path,
                &format!("/spec/container/env/{index}/value"),
                "environment value looks secret",
            ));
        }
    }
    check_resources(
        science,
        document,
        required(document, &container, "resources")?,
        path,
    )?;
    let maximum = science
        .limits
        .map(|id| {
            field(
                science.content,
                &Value::Node(id),
                "max_deadline_seconds",
                false,
            )
        })
        .transpose()?
        .flatten()
        .filter(|id| !matches!(science.content.node(*id), Some(Node::Null)));
    if let Some(maximum) = maximum
        && greater(
            document,
            required(document, &spec, "activeDeadlineSeconds")?,
            science.content,
            maximum,
        )?
    {
        return Err(violation(
            path,
            "/spec/activeDeadlineSeconds",
            "deadline ceiling exceeded",
        ));
    }
    let setup = setup_deadline_at(document, manifest)?;
    if let (Some(maximum), Some(seconds)) = (maximum, setup) {
        let mut builder = cannery_core::json::DocumentBuilder::new();
        let root = builder
            .push(Node::Integer(seconds))
            .map_err(|_| ScienceError::InvalidNode)?;
        let d = builder
            .finish(root)
            .map_err(|_| ScienceError::InvalidNode)?;
        if greater(&d, d.root(), science.content, maximum)? {
            return Err(violation(
                path,
                "/spec/setup/activeDeadlineSeconds",
                "setup deadline ceiling exceeded",
            ));
        }
    }
    if let Some(code) = field(document, &spec, "code", false)?
        .filter(|id| !matches!(document.node(*id), Some(Node::Null)))
    {
        let repo = required_text(document, &Value::Node(code), "repo", rendering)?;
        if !science
            .code_repositories(role.trust().name())?
            .contains(&repo.lowercase())
        {
            return Err(violation(
                path,
                "/spec/code/repo",
                "repository is not allowed",
            ));
        }
    }
    let mut names = Vec::new();
    let mut resumed = Vec::new();
    let mut paths = Vec::new();
    for side in [Side::Inputs, Side::Outputs] {
        for (index, artifact) in items(document, artifacts(document, manifest, side)?)?
            .iter()
            .enumerate()
        {
            let where_ = format!("/spec/{}/artifacts/{index}", side.name());
            let name = required_text(document, artifact, "name", rendering)?;
            if names.contains(&name) || (matches!(side, Side::Inputs) && resumed.contains(&name)) {
                return Err(violation(
                    path,
                    &format!("{where_}/name"),
                    "artifact name is duplicated",
                ));
            }
            let artifact_path = scalar(
                document,
                &Value::Node(required(document, artifact, "path")?),
                rendering,
            )?;
            if paths.contains(&artifact_path) {
                return Err(violation(
                    path,
                    &format!("{where_}/path"),
                    "artifact path is duplicated",
                ));
            }
            if matches!(side, Side::Inputs)
                && role == Role::Experiment
                && field(document, artifact, "from", false)?
                    .is_some_and(|id| matches_text(document, id, "attempt"))
            {
                resumed.push(name);
            } else {
                names.push(name);
            }
            paths.push(artifact_path);
        }
    }
    for (index, artifact) in items(document, artifacts(document, manifest, Side::Inputs)?)?
        .iter()
        .enumerate()
    {
        let where_ = violation(path, &format!("/spec/inputs/artifacts/{index}"), "unused");
        let ScienceError::Validation { path: where_, .. } = where_ else {
            return Err(ScienceError::InvalidNode);
        };
        check_input(science, document, artifact, role, &where_, rendering)?;
    }
    for (index, artifact) in items(document, artifacts(document, manifest, Side::Outputs)?)?
        .iter()
        .enumerate()
    {
        if let Some(interface) = field(document, artifact, "interface", false)?
            .filter(|id| !matches!(document.node(*id), Some(Node::Null)))
            && !matches_text(document, interface, EVIDENCE_INTERFACE)
            && !matches_text(document, interface, RUN_INTERFACE)
            && !scalar_text_member(
                document,
                &Value::Node(interface),
                &science.interfaces,
                rendering,
            )?
        {
            return Err(violation(
                path,
                &format!("/spec/outputs/artifacts/{index}/interface"),
                "output interface is not registered",
            ));
        }
    }
    Ok(())
}
/// # Errors
/// Preserves missing scorer, iteration and first mismatched scorer input errors.
pub fn check_pair(
    science: &Science<'_>,
    producer: &Document,
    path: &String,
    rendering: RenderingContext,
) -> Result<(), ScienceError> {
    let mut outputs: Vec<(String, (usize, Option<NodeId>))> = Vec::new();
    for (i, artifact) in items(
        producer,
        artifacts(producer, producer.root(), Side::Outputs)?,
    )?
    .iter()
    .enumerate()
    {
        let name = required_text(producer, artifact, "name", rendering)?;
        let value = (i, field(producer, artifact, "interface", false)?);
        if let Some((_, old)) = outputs.iter_mut().find(|(key, _)| key == &name) {
            *old = value;
        } else {
            outputs.push((name, value));
        }
    }
    for artifact in items(
        science.content,
        artifacts(science.content, science.scorer()?, Side::Inputs)?,
    )? {
        if !matches_text(
            science.content,
            required(science.content, &artifact, "from")?,
            "step",
        ) {
            continue;
        }
        let name = required_text(science.content, &artifact, "name", rendering)?;
        let expected = required_text(science.content, &artifact, "interface", rendering)?;
        let Some((_, (index, interface))) = outputs.iter().find(|(key, _)| key == &name) else {
            return Err(violation(
                path,
                "/spec/outputs/artifacts",
                "scorer input lacks a producer output",
            ));
        };
        if !interface.is_some_and(
            |id| matches!(producer.node(id),Some(Node::String(value)) if value==&expected),
        ) {
            return Err(violation(
                path,
                &format!("/spec/outputs/artifacts/{index}/interface"),
                "producer interface differs from scorer",
            ));
        }
    }
    Ok(())
}
/// # Errors
/// All validator steps are checked before any interface-to-validator binding.
pub fn check_validators(
    science: &Science<'_>,
    rendering: RenderingContext,
) -> Result<(), ScienceError> {
    for (index, manifest) in optional_items(
        science.content,
        &Value::Node(science.content.root()),
        "validators",
    )?
    .iter()
    .enumerate()
    {
        let Value::Node(id) = manifest else {
            return Err(ScienceError::Type);
        };
        check_step_at(
            science,
            science.content,
            *id,
            Role::Validator,
            &String::from(&format!("/validators/{index}")),
            rendering,
        )?;
    }
    for (index, registration) in optional_items(
        science.content,
        &Value::Node(science.content.root()),
        "interfaces",
    )?
    .iter()
    .enumerate()
    {
        let Some(name) = field(science.content, registration, "validator", false)?
            .filter(|id| !matches!(science.content.node(*id), Some(Node::Null)))
        else {
            continue;
        };
        let reference = InterfaceSpecRef::registration(science.content, registration, rendering)?;
        let name = text(science.content, &Value::Node(name), rendering)?;
        let manifest = science
            .validators
            .iter()
            .find(|(key, _)| key == &name)
            .map(|(_, id)| *id)
            .ok_or(ScienceError::Key)?;
        let inputs = items(
            science.content,
            artifacts(science.content, manifest, Side::Inputs)?,
        )?;
        if inputs.len() != 1 {
            return Err(ScienceError::Value);
        }
        let interface = required(science.content, &inputs[0], "interface")?;
        if !matches!(science.content.node(interface),Some(Node::String(value)) if value==&reference)
        {
            return Err(violation(
                &String::from(&format!("/interfaces/{index}/validator")),
                "",
                "validator interface differs",
            ));
        }
    }
    Ok(())
}
struct InterfaceSpecRef;
impl InterfaceSpecRef {
    fn registration(
        document: &Document,
        value: &Value,
        rendering: RenderingContext,
    ) -> Result<String, ScienceError> {
        let name = required_text(document, value, "name", rendering)?;
        let version = required_text(document, value, "version", rendering)?;
        let mut p = name.codepoints().clone();
        p.extend([47, 118]);
        p.extend(version.codepoints());
        cannery_core::text::from_codepoints(p).ok_or(ScienceError::InvalidNode)
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
