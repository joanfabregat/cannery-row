//! The verification report of a verify job, independent of HTTP: the stock
//! policy's assessment and the report composed from the scorer's evidence and
//! the policy's verdict.
use crate::{
    gates::{self, Control},
    policy::StockPolicy,
};
use cannery_core::{
    contracts::phases::{Phase, PhaseSchemas},
    front_matter::Limits,
    json::{self, Document},
};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

/// Which input made the report impossible; no input value is quoted.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ReportError {
    #[error("the scorer's evidence has an invalid shape")]
    Evidence,
    #[error("the policy's verdict has an invalid shape")]
    Verdict,
    #[error("the verification report is invalid")]
    Report,
}

/// What the scorer's `evidence` output may hold.
const EVIDENCE_KEYS: [&str; 6] = [
    "provenance",
    "measurements",
    "discrepancies",
    "observations",
    "artifact_roles",
    "extensions",
];
/// What a policy's verdict may hold.
const VERDICT_KEYS: [&str; 4] = ["gates", "comparisons", "verdict", "reason"];

fn document(value: &Value, budget: usize) -> Result<Document, ReportError> {
    let bytes = serde_json::to_vec(value).map_err(|_| ReportError::Evidence)?;
    json::decode(&bytes, budget).map_err(|_| ReportError::Evidence)
}

/// Check the scorer's evidence: an object with known keys, provenance and
/// measurements, never a verdict of its own.
/// # Errors
/// Refuses any other shape.
pub fn check_evidence(evidence: &Value) -> Result<(), ReportError> {
    let fields = evidence.as_object().ok_or(ReportError::Evidence)?;
    if fields
        .keys()
        .any(|key| !EVIDENCE_KEYS.contains(&key.as_str()))
        || !fields.get("provenance").is_some_and(Value::is_object)
        || !fields.get("measurements").is_some_and(Value::is_array)
        || fields
            .get("observations")
            .is_some_and(|text| !text.is_string())
    {
        return Err(ReportError::Evidence);
    }
    Ok(())
}

/// Apply a stock policy's gates to the scorer's verified measurements, with
/// the job's control (or the policy's default control).
/// # Errors
/// Refuses evidence or a metric registry the gates cannot read.
pub fn stock_assessment(
    policy: &StockPolicy,
    science: &Value,
    evidence: &Value,
    control: Option<&Control>,
    budget: usize,
) -> Result<Value, ReportError> {
    check_evidence(evidence)?;
    let metrics = document(&science["metrics"], budget)?;
    let measurements = document(&evidence["measurements"], budget)?;
    let assessment = gates::assess(&policy.context(&metrics, &measurements, control, budget))
        .and_then(|assessment| assessment.document(budget))
        .map_err(|_| ReportError::Evidence)?;
    let bytes = json::canonical::bytes(&assessment, budget).map_err(|_| ReportError::Evidence)?;
    serde_json::from_slice(&bytes).map_err(|_| ReportError::Evidence)
}

/// A Markdown document: its front matter, one JSON value per key (JSON is
/// YAML), then its body unchanged.
/// # Errors
/// Fails only if a value cannot be encoded.
pub fn markdown(
    front_matter: &Map<String, Value>,
    body: &str,
) -> Result<String, serde_json::Error> {
    let mut document = String::from("---\n");
    for (key, value) in front_matter {
        document.push_str(&serde_json::to_string(key)?);
        document.push_str(": ");
        document.push_str(&serde_json::to_string(value)?);
        document.push('\n');
    }
    document.push_str("---\n");
    document.push_str(body);
    Ok(document)
}

/// Compose the verification report of a runner verify job.
///
/// The front matter takes the verdict, reason, gates and comparisons from the
/// policy, the measurements, discrepancies, artifact roles and extensions
/// from the scorer's evidence, `policy_revision` from the registered
/// verifier, and the provenance from the evidence with the job's pinned
/// science revision. The scorer's observations become the body. The report is
/// checked as the API checks it: against the verification schema, the body
/// against `limits.report_max_bytes`, and every `source: tester` comparison
/// against the report's own measurements.
/// # Errors
/// Names the input at fault: the evidence, the verdict, or the report.
pub fn compose(
    job: &Value,
    policy_revision: &str,
    evidence: &Value,
    verdict: &Value,
    science: &Value,
    schemas: &PhaseSchemas,
) -> Result<String, ReportError> {
    check_evidence(evidence)?;
    let assessment = verdict.as_object().ok_or(ReportError::Verdict)?;
    if assessment
        .keys()
        .any(|key| !VERDICT_KEYS.contains(&key.as_str()))
    {
        return Err(ReportError::Verdict);
    }
    let mut gates = BTreeSet::new();
    for gate in assessment
        .get("gates")
        .and_then(Value::as_array)
        .ok_or(ReportError::Verdict)?
    {
        if !gates.insert(gate["id"].as_str().ok_or(ReportError::Verdict)?) {
            return Err(ReportError::Verdict);
        }
    }
    let science_revision = match &job["science_revision"] {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        _ => return Err(ReportError::Report),
    };
    let mut provenance = evidence["provenance"].clone();
    provenance["science_revision"] = Value::String(science_revision);
    let mut front_matter = Map::new();
    for key in ["verdict", "reason"] {
        front_matter.insert(
            key.into(),
            assessment.get(key).cloned().ok_or(ReportError::Verdict)?,
        );
    }
    front_matter.insert("policy_revision".into(), policy_revision.into());
    front_matter.insert("gates".into(), assessment["gates"].clone());
    front_matter.insert("measurements".into(), evidence["measurements"].clone());
    if let Some(discrepancies) = evidence.get("discrepancies") {
        front_matter.insert("discrepancies".into(), discrepancies.clone());
    }
    if let Some(comparisons) = assessment.get("comparisons") {
        front_matter.insert("comparisons".into(), comparisons.clone());
    }
    front_matter.insert("provenance".into(), provenance);
    for key in ["artifact_roles", "extensions"] {
        if let Some(value) = evidence.get(key) {
            front_matter.insert(key.into(), value.clone());
        }
    }
    let body = evidence
        .get("observations")
        .and_then(Value::as_str)
        .map_or_else(String::new, |text| format!("{text}\n"));
    let cap = science["limits"]["report_max_bytes"]
        .as_u64()
        .ok_or(ReportError::Report)?;
    if u64::try_from(body.len()).map_err(|_| ReportError::Report)? > cap {
        return Err(ReportError::Evidence);
    }
    let text = markdown(&front_matter, &body).map_err(|_| ReportError::Report)?;
    let parsed = schemas
        .parse(Phase::Verification, &text, Limits::default())
        .map_err(|_| ReportError::Report)?;
    let parsed = Value::Object(parsed.front_matter);
    cannery_research::comparisons::check(science, &parsed, &parsed)
        .map_err(|_| ReportError::Verdict)?;
    Ok(text)
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
