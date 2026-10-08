//! Application JSON Schema validation using the maintained jsonschema engine.
pub mod comparison;
mod formats;
pub mod instance;
pub mod phases;
mod policy;
use crate::json::{self, Document};
use jsonschema::{Registry, Validator};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct ContractViolation {
    pub path: String,
    pub message: &'static str,
}
/// Published contracts needed by runner policy and research configuration loading.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ContractKind {
    Track,
    TrackTransition,
    Hypothesis,
    DraftReview,
    HumanDecision,
    ArtifactManifest,
    EvidenceEnvelope,
    Job,
    JobCompletion,
    JobFailure,
    Interface,
    Gates,
    ImportBundle,
    EvaluatorConfig,
    StepManifest,
    ScienceRevision,
    DashboardViews,
}
impl ContractKind {
    /// Resolve an unchanged published schema name; unknown names remain absent.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "track" => Self::Track,
            "track_transition" => Self::TrackTransition,
            "hypothesis" => Self::Hypothesis,
            "draft_review" => Self::DraftReview,
            "human_decision" => Self::HumanDecision,
            "artifact_manifest" => Self::ArtifactManifest,
            "evidence_envelope" => Self::EvidenceEnvelope,
            "job" => Self::Job,
            "job_completion" => Self::JobCompletion,
            "job_failure" => Self::JobFailure,
            "step_manifest" => Self::StepManifest,
            "interface" => Self::Interface,
            "gates" => Self::Gates,
            "evaluator_config" => Self::EvaluatorConfig,
            "dashboard_views" => Self::DashboardViews,
            "science_revision" => Self::ScienceRevision,
            "import_bundle" => Self::ImportBundle,
            _ => return None,
        })
    }
}
/// Static compilation failures; no schema or instance values are disclosed.
#[derive(Clone, Copy, Debug, thiserror::Error, Eq, PartialEq)]
pub enum ContractError {
    #[error("published contract schema could not be compiled")]
    Schema,
}

pub struct ContractValidator {
    validators: BTreeMap<ContractKind, Validator>,
}
impl ContractValidator {
    /// # Errors
    /// Rejects an invalid published schema.
    pub fn new() -> Result<Self, ContractError> {
        let schemas: Vec<(&str, Value)> = PUBLISHED_SOURCES
            .iter()
            .map(|(name, source)| {
                Ok((
                    *name,
                    serde_json::from_str(source).map_err(|_| ContractError::Schema)?,
                ))
            })
            .collect::<Result<_, ContractError>>()?;
        let mut registry = Registry::new();
        for (name, schema) in &schemas {
            registry = registry
                .add(
                    format!("https://cannery-row.invalid/schemas/0.2/{name}.schema.json"),
                    schema,
                )
                .map_err(|_| ContractError::Schema)?;
        }
        let registry = registry.prepare().map_err(|_| ContractError::Schema)?;
        let mut validators = BTreeMap::new();
        for (name, schema) in &schemas {
            if let Some(kind) = ContractKind::from_name(name) {
                let validator = formats::options()
                    .with_registry(&registry)
                    .build(schema)
                    .map_err(|_| ContractError::Schema)?;
                validators.insert(kind, validator);
            }
        }
        Ok(Self { validators })
    }
    #[must_use]
    pub fn is_valid(&self, kind: ContractKind, document: &Document) -> bool {
        json::to_value(document).is_ok_and(|value| self.validators[&kind].is_valid(&value))
    }
    /// # Errors
    /// Rejects an invalid document representation.
    pub fn violations(
        &self,
        kind: ContractKind,
        document: &Document,
    ) -> Result<Vec<ContractViolation>, ContractError> {
        let value = json::to_value(document).map_err(|_| ContractError::Schema)?;
        Ok(schema_errors(&self.validators[&kind], &value))
    }
    /// # Errors
    /// Rejects an invalid document representation.
    pub fn violation_paths(
        &self,
        kind: ContractKind,
        document: &Document,
    ) -> Result<Vec<String>, ContractError> {
        let mut seen = BTreeSet::new();
        Ok(self
            .violations(kind, document)?
            .into_iter()
            .map(|error| error.path)
            .filter(|path| seen.insert(path.clone()))
            .collect())
    }
    /// # Errors
    /// Rejects invalid document representation or embedded schemas.
    pub fn document_violations(
        &self,
        kind: ContractKind,
        document: &Document,
    ) -> Result<Vec<ContractViolation>, ContractError> {
        let mut errors = self.violations(kind, document)?;
        let value = json::to_value(document).map_err(|_| ContractError::Schema)?;
        embedded_schemas(kind, &value, &mut errors);
        sort_errors(&value, &mut errors);
        Ok(errors)
    }
    /// # Errors
    /// Rejects invalid document representation.
    pub fn document_is_valid(
        &self,
        kind: ContractKind,
        document: &Document,
    ) -> Result<bool, ContractError> {
        Ok(self.document_violations(kind, document)?.is_empty())
    }
}
pub(crate) fn schema_errors(validator: &Validator, value: &Value) -> Vec<ContractViolation> {
    let mut errors: Vec<_> = validator
        .iter_errors(value)
        .map(|error| ContractViolation {
            path: useful_path(&error),
            message: "value does not satisfy the schema",
        })
        .collect();
    sort_errors(value, &mut errors);
    errors
}

/// Order pointer segments using their actual parent container: array positions
/// are numeric, while object keys (including numeric keys) remain lexical.
fn pointer_order(value: &Value, left: &str, right: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut left = left.split('/').skip(1);
    let mut right = right.split('/').skip(1);
    let mut parent = Some(value);
    loop {
        let (left, right) = match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left), Some(right)) => (
                left.replace("~1", "/").replace("~0", "~"),
                right.replace("~1", "/").replace("~0", "~"),
            ),
        };
        let order = if matches!(parent, Some(Value::Array(_))) {
            match (left.parse::<usize>(), right.parse::<usize>()) {
                (Ok(left_index), Ok(right_index)) => {
                    left_index.cmp(&right_index).then_with(|| left.cmp(&right))
                }
                (Ok(_), Err(_)) => Ordering::Less,
                (Err(_), Ok(_)) => Ordering::Greater,
                (Err(_), Err(_)) => left.cmp(&right),
            }
        } else {
            left.cmp(&right)
        };
        if order != Ordering::Equal {
            return order;
        }
        parent = match parent {
            Some(Value::Array(items)) => left
                .parse::<usize>()
                .ok()
                .and_then(|index| items.get(index)),
            Some(Value::Object(fields)) => fields.get(&left),
            _ => None,
        };
    }
}

fn sort_errors(value: &Value, errors: &mut Vec<ContractViolation>) {
    errors.sort_by(|left, right| {
        pointer_order(value, &left.path, &right.path).then_with(|| left.message.cmp(right.message))
    });
    errors.dedup();
}
/// # Errors
/// Returns sanitized schema-validation failures without schema contents.
pub fn project_schema_violations(
    document: &Document,
) -> Result<Vec<ContractViolation>, ContractError> {
    let value = json::to_value(document).map_err(|_| ContractError::Schema)?;
    Ok(project_schema_errors(&value))
}
pub(crate) fn project_schema_errors(value: &Value) -> Vec<ContractViolation> {
    let policy = policy::errors(value);
    if !policy.is_empty() {
        return policy;
    }
    match jsonschema::draft202012::meta::validate(value) {
        Ok(()) => match formats::options().build(value) {
            Ok(_) => Vec::new(),
            Err(error) => vec![ContractViolation {
                path: error.instance_path().to_string(),
                message: "schema cannot be compiled without external retrieval",
            }],
        },
        Err(error) => vec![ContractViolation {
            path: error.instance_path().to_string(),
            message: "invalid JSON Schema",
        }],
    }
}
fn useful_path(error: &jsonschema::ValidationError<'_>) -> String {
    use jsonschema::error::ValidationErrorKind;
    let mut best = error.instance_path().to_string();
    let mut pending = vec![error];
    while let Some(error) = pending.pop() {
        let path = error.instance_path().to_string();
        if path.bytes().filter(|byte| *byte == b'/').count()
            > best.bytes().filter(|byte| *byte == b'/').count()
        {
            best = path;
        }
        match error.kind() {
            ValidationErrorKind::AnyOf { context }
            | ValidationErrorKind::OneOfNotValid { context } => {
                pending.extend(context.iter().rev().flat_map(|branch| branch.iter().rev()));
            }
            _ => {}
        }
    }
    best
}
fn embedded_schemas(kind: ContractKind, value: &Value, errors: &mut Vec<ContractViolation>) {
    let mut validate = |schema: &Value, prefix: String| {
        for mut error in project_schema_errors(schema) {
            error.path = format!("{prefix}{}", error.path);
            errors.push(error);
        }
    };
    match kind {
        ContractKind::Interface => {
            if let Some(schema) = value.get("schema") {
                validate(schema, "/schema".into());
            }
        }
        ContractKind::ScienceRevision => {
            for field in ["hypothesis_fields", "result_extensions"] {
                if let Some(schema) = value.get(field) {
                    validate(schema, format!("/{field}"));
                }
            }
            if let Some(interfaces) = value.get("interfaces").and_then(Value::as_array) {
                for (index, interface) in interfaces.iter().enumerate() {
                    if let Some(schema) = interface.get("schema") {
                        validate(schema, format!("/interfaces/{index}/schema"));
                    }
                }
            }
        }
        _ => {}
    }
}
const PUBLISHED_SOURCES: [(&str, &str); 22] = [
    (
        "track",
        include_str!("../../../contracts/schemas/track.schema.json"),
    ),
    (
        "track_transition",
        include_str!("../../../contracts/schemas/track_transition.schema.json"),
    ),
    (
        "hypothesis",
        include_str!("../../../contracts/schemas/hypothesis.schema.json"),
    ),
    (
        "draft_review",
        include_str!("../../../contracts/schemas/draft_review.schema.json"),
    ),
    (
        "human_decision",
        include_str!("../../../contracts/schemas/human_decision.schema.json"),
    ),
    (
        "artifact_manifest",
        include_str!("../../../contracts/schemas/artifact_manifest.schema.json"),
    ),
    (
        "evidence_envelope",
        include_str!("../../../contracts/schemas/evidence_envelope.schema.json"),
    ),
    (
        "job",
        include_str!("../../../contracts/schemas/job.schema.json"),
    ),
    (
        "job_completion",
        include_str!("../../../contracts/schemas/job_completion.schema.json"),
    ),
    (
        "job_failure",
        include_str!("../../../contracts/schemas/job_failure.schema.json"),
    ),
    (
        "import_bundle",
        include_str!("../../../contracts/schemas/import_bundle.schema.json"),
    ),
    (
        "common",
        include_str!("../../../contracts/schemas/common.schema.json"),
    ),
    (
        "gates",
        include_str!("../../../contracts/schemas/gates.schema.json"),
    ),
    (
        "evaluator_config",
        include_str!("../../../contracts/schemas/evaluator_config.schema.json"),
    ),
    (
        "step_manifest",
        include_str!("../../../contracts/schemas/step_manifest.schema.json"),
    ),
    (
        "interface",
        include_str!("../../../contracts/schemas/interface.schema.json"),
    ),
    (
        "science_revision",
        include_str!("../../../contracts/schemas/science_revision.schema.json"),
    ),
    (
        "dashboard_views",
        include_str!("../../../contracts/schemas/dashboard_views.schema.json"),
    ),
    (
        "brief",
        include_str!("../../../contracts/schemas/brief.schema.json"),
    ),
    (
        "run",
        include_str!("../../../contracts/schemas/run.schema.json"),
    ),
    (
        "verification",
        include_str!("../../../contracts/schemas/verification.schema.json"),
    ),
    (
        "writeup",
        include_str!("../../../contracts/schemas/writeup.schema.json"),
    ),
];
