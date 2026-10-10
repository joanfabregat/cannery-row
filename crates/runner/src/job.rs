//! Lossless step-facing job contracts, separate from staging and execution.
use cannery_core::json::{self, Document, Node, NodeId};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

/// The session projections: a verify job's steps (producer, scorer,
/// validators and the policy step), an experiment's workflow steps, and a
/// decide job's decider step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobKind {
    Verify,
    Experiment,
    Decide,
}

/// Borrowed public claim and selected step; no transport credentials are projected.
/// `metrics`, the science revision's metric registry, is projected for a policy step only.
/// `nesting_budget` is explicit and has not been calibrated against the final runner entry point.
pub struct JobContext<'a> {
    pub kind: JobKind,
    pub claim: &'a Document,
    pub step: &'a Document,
    pub metrics: Option<&'a Document>,
    pub nesting_budget: usize,
}

/// Sanitized failures, without native errors, paths, or source document values.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum JobError {
    #[error("a required job field is missing")]
    MissingField,
    #[error("a job field has an incompatible type")]
    WrongType,
    #[error("job string rendering recursion limit exceeded")]
    Recursion,
    #[error("job JSON rendering failed")]
    Encoding,
    #[error("a contract parent directory is missing")]
    NotFound,
    #[error("the contract file is a directory")]
    IsDirectory,
    #[error("an output path already exists as a file")]
    AlreadyExists,
    #[error("contract filesystem operation failed")]
    Filesystem,
}
impl JobError {
    /// Reference exception category, without human-readable exception payloads.
    #[must_use]
    pub const fn python_exception(self) -> &'static str {
        match self {
            Self::MissingField => "KeyError",
            Self::WrongType => "TypeError",
            Self::Recursion => "RecursionError",
            Self::Encoding => "ValueError",
            Self::NotFound => "FileNotFoundError",
            Self::IsDirectory => "IsADirectoryError",
            Self::AlreadyExists => "FileExistsError",
            Self::Filesystem => "OSError",
        }
    }
}
impl From<std::io::Error> for JobError {
    fn from(error: std::io::Error) -> Self {
        match error.kind() {
            std::io::ErrorKind::NotFound => Self::NotFound,
            std::io::ErrorKind::IsADirectory => Self::IsDirectory,
            std::io::ErrorKind::AlreadyExists => Self::AlreadyExists,
            _ => Self::Filesystem,
        }
    }
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
    fn node(self) -> Result<&'a Node, JobError> {
        self.doc.node(self.id).ok_or(JobError::WrongType)
    }
    fn object(self) -> Result<&'a [(String, NodeId)], JobError> {
        if let Node::Object(entries) = self.node()? {
            Ok(entries)
        } else {
            Err(JobError::WrongType)
        }
    }
    fn optional(self, key: &str) -> Result<Option<Self>, JobError> {
        self.object()?;
        Ok(self.doc.field(self.id, key).map(|id| Self { id, ..self }))
    }
    fn get(self, key: &str) -> Result<Self, JobError> {
        self.optional(key)?.ok_or(JobError::MissingField)
    }
    fn array(self) -> Result<Vec<Self>, JobError> {
        if let Node::Array(items) = self.node()? {
            Ok(items.iter().map(|&id| Self { id, ..self }).collect())
        } else {
            Err(JobError::WrongType)
        }
    }
    fn render(self) -> Result<String, JobError> {
        json::encode_ascii_pretty_node(self.doc, self.id, self.budget)
            .map_err(|_| JobError::Encoding)
    }
    fn text(self) -> Result<String, JobError> {
        use cannery_core::text::{self, RenderError};
        text::str_value(self.doc, self.id, self.budget).map_err(|error| match error {
            RenderError::InvalidNode => JobError::WrongType,
            RenderError::Recursion => JobError::Recursion,
            RenderError::IntegerLimit | RenderError::Encoding => JobError::Encoding,
        })
    }
}
// Intermediate JSON quoting preserves all Python Unicode code points. The core renderer
// supplies the final source-compatible escaping/indentation, including surrogate pairs.
fn quote(text: &String) -> String {
    use std::fmt::Write;
    let mut out = String::from("\"");
    for point in text.codepoints() {
        if point <= 0xffff {
            let _ = write!(out, "\\u{point:04x}");
        } else {
            let shifted = point - 0x10000;
            let _ = write!(
                out,
                "\\u{:04x}\\u{:04x}",
                0xd800 + (shifted >> 10),
                0xdc00 + (shifted & 0x3ff)
            );
        }
    }
    out.push('"');
    out
}
fn object(fields: &[(String, String)]) -> String {
    format!(
        "{{{}}}",
        fields
            .iter()
            .map(|(key, value)| format!("{}:{value}", quote(&String::from(key))))
            .collect::<Vec<_>>()
            .join(",")
    )
}
fn fields(value: Value<'_>, names: &[&str]) -> Result<String, JobError> {
    Ok(object(
        &names
            .iter()
            .map(|&name| Ok((name.to_owned(), value.get(name)?.render()?)))
            .collect::<Result<Vec<_>, JobError>>()?,
    ))
}
fn document(text: &str, budget: usize) -> Result<Document, JobError> {
    json::decode(text.as_bytes(), budget).map_err(|_| JobError::Encoding)
}
fn datasets(step: Value<'_>, pinned: Value<'_>) -> Result<String, JobError> {
    let mut by_id = HashMap::new();
    for item in pinned.array()? {
        let key = item.get("id")?.text()?;
        by_id.insert(key, item);
    }
    let artifacts = step
        .get("manifest")?
        .get("spec")?
        .get("inputs")?
        .get("artifacts")?;
    let mut result = Vec::new();
    for artifact in artifacts.array()? {
        if !matches!(artifact.get("from")?.node()?, Node::String(text) if text.equals_utf8("dataset"))
        {
            continue;
        }
        // Python dict.get evaluates its name fallback even when id is present.
        let name = artifact.get("name")?;
        let key = artifact.optional("id")?.unwrap_or(name).text()?;
        if let Some(pinned) = by_id.get(&key) {
            let mut output = vec![(String::from("name"), quote(&name.text()?))];
            for (key, id) in pinned.object()? {
                let rendered = Value { id: *id, ..*pinned }.render()?;
                if key.equals_utf8("name") {
                    output[0].1 = rendered;
                } else {
                    output.push((key.clone(), rendered));
                }
            }
            result.push(format!(
                "{{{}}}",
                output
                    .iter()
                    .map(|(key, value)| format!("{}:{value}", quote(key)))
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
    }
    Ok(format!("[{}]", result.join(",")))
}

/// Dataset declaration order, last-wins pins, name insertion and pinned-field overwrite.
/// Missing pins are omitted here; staging must reject unavailable required inputs separately.
/// # Errors
/// Returns sanitized field/type/rendering failures.
pub fn declared_datasets(
    step: &Document,
    pinned: &Document,
    nesting_budget: usize,
) -> Result<Document, JobError> {
    document(
        &datasets(
            Value::root(step, nesting_budget),
            Value::root(pinned, nesting_budget),
        )?,
        nesting_budget,
    )
}

/// Project the selected step's contract, preserving lossless source fields and field order.
/// # Errors
/// Returns sanitized field/type/rendering failures. No claim/registration validation is implied.
pub fn project(context: &JobContext<'_>) -> Result<Document, JobError> {
    if context.kind == JobKind::Decide {
        return project_decide(context);
    }
    let claim = Value::root(context.claim, context.nesting_budget);
    let step = Value::root(context.step, context.nesting_budget);
    let experiment = context.kind == JobKind::Experiment;
    let job = claim.get(if experiment { "workflow" } else { "job" })?;
    let inputs = job.get("inputs")?;
    let manifest = step.get("manifest")?;
    let declared = datasets(step, inputs.get("datasets")?)?;
    let mut out = Vec::new();
    let mut add = |name: &str, value: String| {
        out.push((name.to_owned(), value));
    };
    if experiment {
        let attempt = claim.get("attempt")?;
        add("attempt_id", quote(&attempt.get("id")?.text()?));
        add("attempt_ref", attempt.get("ref")?.render()?);
    } else {
        add("job_id", quote(&job.get("job_id")?.text()?));
        add("attempt_id", job.get("attempt_id")?.render()?);
        add("attempt_ref", claim.get("attempt_ref")?.render()?);
    }
    add("track", job.get("track")?.render()?);
    add("science_revision", job.get("science_revision")?.render()?);
    match context.kind {
        JobKind::Verify => {
            let producer = job
                .get("steps")?
                .array()?
                .first()
                .copied()
                .ok_or(JobError::WrongType)?;
            add("producer", fields(producer, &["name", "revision"])?);
            add("verifier", job.get("verifier")?.render()?);
        }
        JobKind::Experiment => {
            let workflow = job
                .get("steps")?
                .array()?
                .into_iter()
                .map(|entry| fields(entry, &["name", "revision"]))
                .collect::<Result<Vec<_>, _>>()?;
            add("workflow", format!("[{}]", workflow.join(",")));
        }
        JobKind::Decide => return project_decide(context),
    }
    add("step", step.get("name")?.render()?);
    add("role", manifest.get("spec")?.get("role")?.render()?);
    add("parameters", job.get("parameters")?.render()?);
    let mut projected_inputs = Vec::new();
    projected_inputs.push(("datasets".to_owned(), declared));
    projected_inputs.push(("baselines".to_owned(), inputs.get("baselines")?.render()?));
    if experiment {
        let predecessor = match inputs.optional("predecessor")? {
            None => "null".to_owned(),
            Some(value) if matches!(value.node()?, Node::Null) => "null".to_owned(),
            Some(value) => fields(value, &["attempt_id", "ref", "state", "failure_code"])?,
        };
        projected_inputs.push(("predecessor".to_owned(), predecessor));
    }
    add("inputs", object(&projected_inputs));
    if let Some(control) = job.optional("control")? {
        add("control", control.render()?);
    }
    if let Some(metrics) = context.metrics {
        add(
            "metrics",
            Value::root(metrics, context.nesting_budget).render()?,
        );
    }
    add("manifest", manifest.render()?);
    document(&object(&out), context.nesting_budget)
}

/// A decider step's contract: the decide job, its decision case, the
/// registered decider and what the decision cites (null when there is none).
fn project_decide(context: &JobContext<'_>) -> Result<Document, JobError> {
    let claim = Value::root(context.claim, context.nesting_budget);
    let step = Value::root(context.step, context.nesting_budget);
    let job = claim.get("job")?;
    let inputs = job.get("inputs")?;
    let manifest = step.get("manifest")?;
    let cited = |name: &str| {
        Ok::<_, JobError>(match inputs.optional(name)? {
            Some(value) => value.render()?,
            None => "null".to_owned(),
        })
    };
    let out = vec![
        ("job_id".to_owned(), quote(&job.get("job_id")?.text()?)),
        ("attempt_id".to_owned(), job.get("attempt_id")?.render()?),
        (
            "attempt_ref".to_owned(),
            claim.get("attempt_ref")?.render()?,
        ),
        ("track".to_owned(), job.get("track")?.render()?),
        ("hypothesis".to_owned(), job.get("hypothesis")?.render()?),
        (
            "science_revision".to_owned(),
            job.get("science_revision")?.render()?,
        ),
        ("decider".to_owned(), job.get("decider")?.render()?),
        (
            "review_case_id".to_owned(),
            job.get("review_case_id")?.render()?,
        ),
        ("step".to_owned(), step.get("name")?.render()?),
        (
            "role".to_owned(),
            manifest.get("spec")?.get("role")?.render()?,
        ),
        (
            "inputs".to_owned(),
            object(&[
                ("verification".to_owned(), cited("verification")?),
                ("writeup".to_owned(), cited("writeup")?),
            ]),
        ),
        ("manifest".to_owned(), manifest.render()?),
    ];
    document(&object(&out), context.nesting_budget)
}

/// Render first, write job.json without a newline, then create output directories in order.
/// Paths are trusted, owned caller paths; this helper does not implement launcher isolation.
/// # Errors
/// Returns sanitized projection/rendering/filesystem failures, retaining earlier side effects.
pub fn write_contract(
    context: &JobContext<'_>,
    root: &Path,
    outputs: &[PathBuf],
) -> Result<(), JobError> {
    let projection = project(context)?;
    let text = json::encode_ascii_pretty(&projection, context.nesting_budget)
        .map_err(|_| JobError::Encoding)?;
    fs::write(root.join("job.json"), text)?;
    for directory in outputs {
        fs::create_dir_all(directory)?;
    }
    Ok(())
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
