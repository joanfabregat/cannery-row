//! Verified stock-evaluator inputs and evidence records, independent of HTTP.
use crate::{
    gates::{self, Control},
    policy::StockPolicy,
};
use cannery_core::json::{self, Document, DocumentBuilder, Node, NodeId};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum EvaluationError {
    #[error("the job does not match the evaluation policy")]
    PolicyMismatch,
    #[error("verified evidence does not match the job")]
    EvidenceMismatch,
    #[error("an evaluation input has an invalid shape")]
    Shape,
    #[error("evaluation record construction failed")]
    Encoding,
}
impl From<json::BuildError> for EvaluationError {
    fn from(_: json::BuildError) -> Self {
        Self::Encoding
    }
}
fn field(d: &Document, id: NodeId, key: &str) -> Result<NodeId, EvaluationError> {
    d.field(id, key).ok_or(EvaluationError::Shape)
}
fn array(d: &Document, id: NodeId) -> Result<&[NodeId], EvaluationError> {
    match d.node(id) {
        Some(Node::Array(items)) => Ok(items),
        _ => Err(EvaluationError::Shape),
    }
}
fn text(d: &Document, id: NodeId) -> Result<&String, EvaluationError> {
    match d.node(id) {
        Some(Node::String(value)) => Ok(value),
        _ => Err(EvaluationError::Shape),
    }
}
fn project(d: &Document, id: NodeId) -> Result<Document, EvaluationError> {
    let mut b = DocumentBuilder::new();
    let root = b.import(d, id)?;
    Ok(b.finish(root)?)
}
fn string(b: &mut DocumentBuilder, value: &str) -> Result<NodeId, EvaluationError> {
    Ok(b.push(Node::String(String::from(value)))?)
}
fn object(b: &mut DocumentBuilder, fields: Vec<(&str, NodeId)>) -> Result<NodeId, EvaluationError> {
    Ok(b.push(Node::Object(
        fields
            .into_iter()
            .map(|(key, id)| (String::from(key), id))
            .collect(),
    ))?)
}

/// Match served evidence to pinned digests in job order, irrespective of HTTP order.
/// # Errors
/// Rejects missing, extra, duplicate or unrepresentable evidence and duplicate digests.
pub fn verified_records(
    refs: &Document,
    served: &Document,
    budget: usize,
) -> Result<Vec<Document>, EvaluationError> {
    let references = array(refs, refs.root())?;
    let items = array(served, served.root())?;
    if references.is_empty() || references.len() != items.len() {
        return Err(EvaluationError::EvidenceMismatch);
    }
    let mut by_digest = BTreeMap::new();
    for &item in items {
        let record = project(served, item)?;
        if !matches!(record.node(record.root()), Some(Node::Object(_))) {
            return Err(EvaluationError::Shape);
        }
        let hash =
            json::canonical::sha256(&record, budget).map_err(|_| EvaluationError::Encoding)?;
        if by_digest.insert(hash, record).is_some() {
            return Err(EvaluationError::EvidenceMismatch);
        }
    }
    let mut records = Vec::with_capacity(references.len());
    for &reference in references {
        let digest = text(refs, field(refs, reference, "sha256")?)?
            .as_utf8()
            .ok_or(EvaluationError::Shape)?;
        records.push(
            by_digest
                .remove(&digest)
                .ok_or(EvaluationError::EvidenceMismatch)?,
        );
    }
    Ok(records)
}

/// Assess genuine verified measurements and wrap them in an evaluator evidence record.
/// `science` is the pinned science content; timestamps come from the worker clock.
/// # Errors
/// Rejects policy mismatches, evidence integrity failures and invalid input shapes.
pub fn stock_record(
    policy: &StockPolicy,
    job: &Document,
    science: &Document,
    served: &Document,
    started: &str,
    finished: &str,
    budget: usize,
) -> Result<Document, EvaluationError> {
    let registration = field(job, job.root(), "evaluator")?;
    if text(job, field(job, registration, "id")?)? != &policy.evaluator_id
        || text(job, field(job, registration, "revision")?)? != &policy.revision
    {
        return Err(EvaluationError::PolicyMismatch);
    }
    let inputs = field(job, job.root(), "inputs")?;
    let refs = project(job, field(job, inputs, "evidence")?)?;
    let records = verified_records(&refs, served, budget)?;
    let first = records.first().ok_or(EvaluationError::EvidenceMismatch)?;
    let metrics = project(science, field(science, science.root(), "metrics")?)?;
    let mut b = DocumentBuilder::new();
    let mut measurements = Vec::new();
    for record in &records {
        if let Some(id) = record.field(record.root(), "measurements") {
            for &measurement in array(record, id)? {
                measurements.push(b.import(record, measurement)?);
            }
        }
    }
    let root = b.push(Node::Array(measurements))?;
    let measurements = b.finish(root)?;
    let control = job
        .field(job.root(), "control")
        .filter(|&id| !matches!(job.node(id), Some(Node::Null)))
        .map(|id| {
            Ok::<_, EvaluationError>(Control {
                id: text(job, field(job, id, "id")?)?.clone(),
                revision: text(job, field(job, id, "revision")?)?.clone(),
            })
        })
        .transpose()?;
    let assessment =
        gates::assess(&policy.context(&metrics, &measurements, control.as_ref(), budget))
            .and_then(|a| a.document(budget))
            .map_err(|_| EvaluationError::Shape)?;
    record(
        (&policy.evaluator_id, &policy.revision),
        job,
        first,
        &refs,
        &assessment,
        started,
        finished,
    )
}

/// Validate a policy step's assessment and bind it to pinned evidence and provenance.
/// `served` contains records fetched under this job's lease; the step supplies only
/// gates, optional comparisons, verdict and reason, never record identity or provenance.
/// # Errors
/// Rejects policy mismatches, malformed assessments, duplicate gates and false citations.
#[allow(
    clippy::too_many_arguments,
    reason = "Bind one immutable policy evaluation"
)]
pub fn policy_record(
    policy: &crate::policy::StepPolicy,
    job: &Document,
    science: &Document,
    served: &Document,
    assessment: &Document,
    started: &str,
    finished: &str,
    budget: usize,
    contracts: &cannery_core::contracts::ContractValidator,
) -> Result<Document, EvaluationError> {
    let registration = field(job, job.root(), "evaluator")?;
    if text(job, field(job, registration, "id")?)? != &policy.evaluator_id
        || text(job, field(job, registration, "revision")?)? != &policy.revision
    {
        return Err(EvaluationError::PolicyMismatch);
    }
    let value = |document: &Document| -> Result<serde_json::Value, EvaluationError> {
        let bytes =
            json::canonical::bytes(document, budget).map_err(|_| EvaluationError::Encoding)?;
        serde_json::from_slice(&bytes).map_err(|_| EvaluationError::Shape)
    };
    let mut assessment = value(assessment)?;
    let fields = assessment.as_object_mut().ok_or(EvaluationError::Shape)?;
    if fields
        .keys()
        .any(|key| !["gates", "comparisons", "verdict", "reason"].contains(&key.as_str()))
    {
        return Err(EvaluationError::Shape);
    }
    fields
        .entry("comparisons")
        .or_insert_with(|| serde_json::json!([]));
    let bytes = serde_json::to_vec(&assessment).map_err(|_| EvaluationError::Encoding)?;
    let assessment_document = json::decode(&bytes, budget).map_err(|_| EvaluationError::Shape)?;
    let inputs = field(job, job.root(), "inputs")?;
    let refs = project(job, field(job, inputs, "evidence")?)?;
    let records = verified_records(&refs, served, budget)?;
    let record = record(
        (&policy.evaluator_id, &policy.revision),
        job,
        records.first().ok_or(EvaluationError::EvidenceMismatch)?,
        &refs,
        &assessment_document,
        started,
        finished,
    )?;
    if !contracts.is_valid(
        cannery_core::contracts::ContractKind::EvidenceEnvelope,
        &record,
    ) {
        return Err(EvaluationError::Shape);
    }
    let mut gates = std::collections::BTreeSet::new();
    for gate in assessment["gates"]
        .as_array()
        .ok_or(EvaluationError::Shape)?
    {
        if !gates.insert(gate["id"].as_str().ok_or(EvaluationError::Shape)?) {
            return Err(EvaluationError::Shape);
        }
    }
    let mut measurements = Vec::new();
    for record in &records {
        if let Some(values) = value(record)?.get("measurements") {
            measurements.extend(
                values
                    .as_array()
                    .ok_or(EvaluationError::Shape)?
                    .iter()
                    .cloned(),
            );
        }
    }
    cannery_research::comparisons::check(
        &value(science)?,
        &assessment,
        &serde_json::json!({"measurements":measurements}),
    )
    .map_err(|_| EvaluationError::Shape)?;
    Ok(record)
}

#[allow(
    clippy::too_many_arguments,
    reason = "One record binds pinned evidence, assessment and worker timestamps"
)]
fn record(
    identity: (&String, &String),
    job: &Document,
    first: &Document,
    refs: &Document,
    assessment: &Document,
    started: &str,
    finished: &str,
) -> Result<Document, EvaluationError> {
    let mut b = DocumentBuilder::new();
    let provenance = field(first, first.root(), "provenance")?;
    let source = b.import(first, field(first, provenance, "source_revision")?)?;
    let science = match job.node(field(job, job.root(), "science_revision")?) {
        Some(Node::Integer(value)) => string(&mut b, &value.to_string())?,
        Some(Node::String(value)) => b.push(Node::String(value.clone()))?,
        _ => return Err(EvaluationError::Shape),
    };
    let mut provenance_fields = vec![("source_revision", source), ("science_revision", science)];
    for key in ["dataset_revision", "control_revision"] {
        if let Some(id) = first.field(provenance, key) {
            provenance_fields.push((key, b.import(first, id)?));
        }
    }
    let provenance = object(&mut b, provenance_fields)?;
    let mut assessment_fields = Vec::new();
    for key in ["gates", "comparisons", "verdict", "reason"] {
        assessment_fields.push((
            key,
            b.import(assessment, field(assessment, assessment.root(), key)?)?,
        ));
    }
    let revision = b.push(Node::String(identity.1.clone()))?;
    assessment_fields.push(("policy_revision", revision));
    let mut evidence = Vec::new();
    for &reference in array(refs, refs.root())? {
        let id = b.import(refs, field(refs, reference, "ref")?)?;
        let digest = b.import(refs, field(refs, reference, "sha256")?)?;
        evidence.push(object(&mut b, vec![("ref", id), ("sha256", digest)])?);
    }
    let evidence = b.push(Node::Array(evidence))?;
    assessment_fields.push(("evidence", evidence));
    let assessment = object(&mut b, assessment_fields)?;
    let service = string(&mut b, "service")?;
    let evaluator = b.push(Node::String(identity.0.clone()))?;
    let producer = object(&mut b, vec![("kind", service), ("id", evaluator)])?;
    let version = string(&mut b, "0.2")?;
    let attempt = b.import(job, field(job, job.root(), "attempt_id")?)?;
    let stage = string(&mut b, "evaluator")?;
    let status = string(&mut b, "completed")?;
    let started = string(&mut b, started)?;
    let finished = string(&mut b, finished)?;
    let root = object(
        &mut b,
        vec![
            ("schema_version", version),
            ("attempt_id", attempt),
            ("stage", stage),
            ("status", status),
            ("producer", producer),
            ("started_at", started),
            ("finished_at", finished),
            ("provenance", provenance),
            ("assessment", assessment),
        ],
    )?;
    Ok(b.finish(root)?)
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
