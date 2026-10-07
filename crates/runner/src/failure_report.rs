//! Ordered job failure reports, before the HTTP encoding and response boundaries.
use cannery_core::json::{BuildError, Document, DocumentBuilder, Node};
use num_traits::Zero;

pub struct Failure<'a> {
    pub job_id: &'a String,
    pub code: &'a String,
    pub reason: &'a String,
    pub step: Option<&'a String>,
    /// The constructor's `logs or []` coercion applies to this lossless value.
    pub logs: Option<&'a Document>,
}
impl std::fmt::Debug for Failure<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Failure([redacted])")
    }
}

fn truth(node: &Node) -> bool {
    match node {
        Node::Null => false,
        Node::Bool(value) => *value,
        Node::Integer(value) => !value.is_zero(),
        Node::Float(value) => *value != 0.0,
        Node::String(value) => !value.codepoints().is_empty(),
        Node::Array(values) => !values.is_empty(),
        Node::Object(values) => !values.is_empty(),
    }
}

fn report(
    failure: &Failure<'_>,
    logs: Option<&Document>,
    bare: bool,
) -> Result<Document, BuildError> {
    let mut builder = DocumentBuilder::new();
    let mut fields = Vec::new();
    let reason = cannery_core::text::from_codepoints(
        failure
            .reason
            .codepoints()
            .iter()
            .take(6000)
            .copied()
            .collect(),
    )
    .ok_or(BuildError::InvalidNode)?;
    for (name, value) in [
        ("schema_version", String::from("0.2")),
        ("job_id", failure.job_id.clone()),
        ("error_code", failure.code.clone()),
        ("reason", reason),
    ] {
        fields.push((String::from(name), builder.push(Node::String(value))?));
    }
    let logs = match logs.filter(|_| !bare) {
        Some(logs) => builder.import(logs, logs.root())?,
        None => builder.push(Node::Array(Vec::new()))?,
    };
    fields.push((String::from("logs"), logs));
    if !bare && let Some(step) = failure.step {
        fields.push((
            String::from("step"),
            builder.push(Node::String(step.clone()))?,
        ));
    }
    let root = builder.push(Node::Object(fields))?;
    builder.finish(root)
}

/// Construct the detailed report and, only when distinct, its bare fallback.
/// The caller sends the second report only after a first HTTP 422 response.
/// An unexpected transport/response failure does not consume the fallback.
/// # Errors
/// Returns a sanitized arena error; source values never enter diagnostics.
pub fn reports(failure: &Failure<'_>) -> Result<Vec<Document>, BuildError> {
    let logs = failure
        .logs
        .filter(|logs| logs.node(logs.root()).is_some_and(truth));
    let mut reports = vec![report(failure, logs, false)?];
    if failure.step.is_some() || logs.is_some() {
        reports.push(report(failure, None, true)?);
    }
    Ok(reports)
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
