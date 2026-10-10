//! Shared source step resolution; storage and raw equality are explicit boundaries.
mod diagnostic;
use cannery_core::{
    ids::ProjectId,
    json::{self, Document, DocumentBuilder, Node, NodeId},
};
use cannery_research::{
    science::{RenderingContext, Science, ScienceError},
    steps::{self, Role, Side},
};
use futures_util::future::BoxFuture;
use num_bigint::BigInt;
use sqlx::PgConnection;
use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Science(#[from] ScienceError),
    #[error("registered manifest load failed")]
    Store,
    #[error("step binding validation failed")]
    Validation(Validation),
}
/// Public source diagnostic, with value-free Debug and Display output.
pub struct Validation {
    pub path: String,
    pub message: String,
}
impl std::fmt::Debug for Validation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Validation([redacted])")
    }
}
/// Ordinary Python equality, including its caller-profiled recursion and identity.
/// Implementations must distinguish direct NaN equality from container shortcuts.
pub trait RawEquality: Send + Sync {
    /// # Errors
    /// Preserve source comparison errors under the supplied caller profile.
    fn equal(
        &self,
        left: Option<(&Document, NodeId)>,
        right: Option<(&Document, NodeId)>,
        rendering: RenderingContext,
    ) -> Result<bool, ScienceError>;
}
/// Required decode/render/equality profiles; no production default is inferred.
pub struct BindingContext<'a> {
    pub rendering: RenderingContext,
    pub decode_budget: usize,
    pub equality: &'a dyn RawEquality,
}
/// Required registered-manifest storage; its connection owner retains preparation history.
pub trait ManifestQuery: Send {
    /// # Errors
    /// Preserve real storage and row-decoding failures, without disclosing values.
    fn manifest<'a>(
        &'a mut self,
        project: ProjectId,
        name: &'a str,
        revision: &'a BigInt,
        experiment: bool,
        decode_budget: usize,
    ) -> BoxFuture<'a, Result<Option<Arc<Document>>, Error>>;
}
/// Existing track connection boundary, not a source-preparation parity claim.
pub struct PgManifestQuery<'a>(pub &'a mut PgConnection);
impl ManifestQuery for PgManifestQuery<'_> {
    fn manifest<'a>(
        &'a mut self,
        project: ProjectId,
        name: &'a str,
        revision: &'a BigInt,
        experiment: bool,
        decode_budget: usize,
    ) -> BoxFuture<'a, Result<Option<Arc<Document>>, Error>> {
        Box::pin(async move {
            let argument =
                crate::track_audit::PgInteger::new(revision).map_err(|_| Error::Store)?;
            let raw = if experiment {
                sqlx::query_scalar!(
                    r#"SELECT content::text AS "content!" FROM experiment_manifests WHERE project_id=$1 AND name=$2 AND revision=$3"#,
                    project as ProjectId,
                    name,
                    argument as _
                )
                .fetch_optional(&mut *self.0)
                .await
            } else {
                sqlx::query_scalar!(
                    r#"SELECT content::text AS "content!" FROM producer_manifests WHERE project_id=$1 AND name=$2 AND revision=$3"#,
                    project as ProjectId,
                    name,
                    argument as _
                )
                .fetch_optional(&mut *self.0)
                .await
            };
            raw.map_err(|_| Error::Store)?
                .map(|raw| {
                    json::decode_str(&raw, decode_budget)
                        .map(Arc::new)
                        .map_err(|_| Error::Store)
                })
                .transpose()
        })
    }
}

pub struct ResolvedStep {
    pub name: String,
    pub revision: BigInt,
    pub manifest: Arc<Document>,
}
impl std::fmt::Debug for ResolvedStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ResolvedStep([REDACTED])")
    }
}
impl ResolvedStep {
    /// Source `{name, revision}` projection, retaining arbitrary integers/text.
    /// # Errors
    /// Returns a sanitized arena construction failure.
    pub fn reference(&self) -> Result<Document, json::BuildError> {
        let mut b = DocumentBuilder::new();
        let name = b.push(Node::String(self.name.clone()))?;
        let revision = b.push(Node::Integer(self.revision.clone()))?;
        let root = b.push(Node::Object(vec![
            (String::from("name"), name),
            (String::from("revision"), revision),
        ]))?;
        b.finish(root)
    }
}
fn fail(path: &String, suffix: &str, message: &'static str) -> Error {
    let mut p = path.codepoints().clone();
    p.extend(suffix.chars().map(u32::from));
    match cannery_core::text::from_codepoints(p) {
        Some(path) => ScienceError::Validation { path, message }.into(),
        None => ScienceError::InvalidNode.into(),
    }
}
fn field(d: &Document, id: NodeId, key: &str) -> Result<NodeId, Error> {
    if !matches!(d.node(id), Some(Node::Object(_))) {
        return Err(ScienceError::Type.into());
    }
    d.field(id, key).ok_or_else(|| ScienceError::Key.into())
}
fn text(d: &Document, id: NodeId, c: &BindingContext<'_>) -> Result<String, Error> {
    cannery_core::text::str_value(d, id, c.rendering.nesting_budget)
        .map_err(ScienceError::from)
        .map_err(Error::from)
}
fn is(d: &Document, id: NodeId, wanted: &str) -> bool {
    matches!(d.node(id),Some(Node::String(v))if v.equals_utf8(wanted))
}
#[allow(
    clippy::too_many_arguments,
    reason = "Explicit query, document, role and diagnostic mode boundaries"
)]
async fn load(
    conn: &mut dyn ManifestQuery,
    project: ProjectId,
    d: &Document,
    id: NodeId,
    experiment: bool,
    path: &String,
    c: &BindingContext<'_>,
    diagnostics: bool,
) -> Result<ResolvedStep, Error> {
    let name = text(d, field(d, id, "name")?, c)?;
    let revision = cannery_research::science::configuration_integer(d, field(d, id, "revision")?)?;
    let utf8 = name.as_utf8().ok_or(Error::Store)?;
    let raw = conn
        .manifest(project, &utf8, &revision, experiment, c.decode_budget)
        .await?;
    let Some(manifest) = raw else {
        // Source evaluates name repr and integer interpolation before violation.
        let name_repr = cannery_core::text::repr_string(&name).map_err(ScienceError::from)?;
        if diagnostics {
            return Err(diagnostic::validation(
                path.clone(),
                &[
                    String::from(if experiment {
                        "experiment step "
                    } else {
                        "producer "
                    }),
                    name_repr,
                    String::from(&format!(" revision {revision} is not registered")),
                ],
            )?);
        }
        return Err(fail(path, "", "step is not registered"));
    };
    Ok(ResolvedStep {
        name,
        revision,
        manifest,
    })
}
/// Resolve/check the selected producer, or the science default, in source order.
/// # Errors
/// Preserves source consumption failures, validation paths and storage errors.
pub async fn resolve_producer(
    conn: &mut dyn ManifestQuery,
    project: ProjectId,
    science: &Science<'_>,
    binding: Option<&Document>,
    path: &String,
    c: &BindingContext<'_>,
) -> Result<ResolvedStep, Error> {
    producer(conn, project, science, binding, path, c, false).await
}
/// Resolve a producer with its complete source public validation diagnostic.
/// # Errors
/// Preserves validation selection and diagnostic rendering failures.
pub async fn resolve_producer_diagnostic(
    conn: &mut dyn ManifestQuery,
    project: ProjectId,
    science: &Science<'_>,
    binding: Option<&Document>,
    path: &String,
    c: &BindingContext<'_>,
) -> Result<ResolvedStep, Error> {
    producer(conn, project, science, binding, path, c, true).await
}
async fn producer(
    conn: &mut dyn ManifestQuery,
    project: ProjectId,
    science: &Science<'_>,
    binding: Option<&Document>,
    path: &String,
    c: &BindingContext<'_>,
    diagnostics: bool,
) -> Result<ResolvedStep, Error> {
    let (d, id) = match binding {
        Some(d) => (d, d.root()),
        None => (
            science.content,
            field(science.content, science.content.root(), "default_producer")?,
        ),
    };
    let found = load(conn, project, d, id, false, path, c, diagnostics).await?;
    steps::check_step(science, &found.manifest, Role::Producer, path, c.rendering).map_err(
        |e| diagnostic::step(e, science, &found.manifest, Role::Producer, c, diagnostics),
    )?;
    steps::check_pair(science, &found.manifest, path, c.rendering)
        .map_err(|e| diagnostic::pair(e, science, &found.manifest, c, diagnostics))?;
    Ok(found)
}

/// Exact ordinary equality under the checked-output string/null precondition.
/// It deliberately rejects a violated precondition instead of claiming a general
/// recursive Python equality implementation.
pub struct CheckedOutputEquality;
impl RawEquality for CheckedOutputEquality {
    fn equal(
        &self,
        left: Option<(&Document, NodeId)>,
        right: Option<(&Document, NodeId)>,
        _rendering: RenderingContext,
    ) -> Result<bool, ScienceError> {
        let left = left.and_then(|(d, id)| d.node(id));
        let right = right.and_then(|(d, id)| d.node(id));
        match left {
            None | Some(Node::Null) => Ok(matches!(right, None | Some(Node::Null))),
            Some(Node::String(a)) => Ok(matches!(right,Some(Node::String(b))if a==b)),
            _ => Err(ScienceError::InvalidNode),
        }
    }
}
enum Item {
    Node(NodeId),
    Text(String),
}
fn iterable(d: &Document, id: NodeId) -> Result<Vec<Item>, Error> {
    match d.node(id) {
        Some(Node::Array(v)) => Ok(v.iter().copied().map(Item::Node).collect()),
        Some(Node::Object(v)) => Ok(v.iter().map(|(key, _)| Item::Text(key.clone())).collect()),
        Some(Node::String(v)) => v
            .codepoints()
            .iter()
            .map(|p| {
                cannery_core::text::from_codepoints(vec![*p])
                    .map(Item::Text)
                    .ok_or_else(|| ScienceError::InvalidNode.into())
            })
            .collect(),
        _ => Err(ScienceError::Type.into()),
    }
}
impl Item {
    fn id(&self) -> Result<NodeId, Error> {
        match self {
            Self::Node(id) => Ok(*id),
            Self::Text(_) => Err(ScienceError::Type.into()),
        }
    }
    fn text(&self, d: &Document, c: &BindingContext<'_>) -> Result<String, Error> {
        match self {
            Self::Node(id) => text(d, *id, c),
            Self::Text(v) => Ok(v.clone()),
        }
    }
}
fn artifact_items(d: &Document, side: Side) -> Result<Vec<Item>, Error> {
    iterable(d, steps::artifacts(d, d.root(), side)?)
}
fn at(path: &String, suffix: &str) -> Result<String, Error> {
    let mut points = path.codepoints().clone();
    points.extend(suffix.chars().map(u32::from));
    cannery_core::text::from_codepoints(points).ok_or_else(|| ScienceError::InvalidNode.into())
}
/// Resolve all workflow refs and their checked manifests before producer inputs.
/// # Errors
/// Preserves source consumption failures, validation paths and storage errors.
#[allow(
    clippy::too_many_lines,
    reason = "First source failure follows the ordered workflow walk"
)]
pub async fn resolve_workflow(
    conn: &mut dyn ManifestQuery,
    project: ProjectId,
    science: &Science<'_>,
    workflow: &Document,
    path: &String,
    producer: Option<&Document>,
    c: &BindingContext<'_>,
) -> Result<Vec<ResolvedStep>, Error> {
    workflow_resolution(conn, project, science, workflow, path, producer, c, false).await
}
/// Resolve a workflow with complete source public validation diagnostics.
/// # Errors
/// Preserves the ordered checks and diagnostic rendering failures.
pub async fn resolve_workflow_diagnostic(
    conn: &mut dyn ManifestQuery,
    project: ProjectId,
    science: &Science<'_>,
    workflow: &Document,
    path: &String,
    producer: Option<&Document>,
    c: &BindingContext<'_>,
) -> Result<Vec<ResolvedStep>, Error> {
    workflow_resolution(conn, project, science, workflow, path, producer, c, true).await
}
#[allow(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    reason = "Ordered source workflow walk with explicit diagnostic mode"
)]
async fn workflow_resolution(
    conn: &mut dyn ManifestQuery,
    project: ProjectId,
    science: &Science<'_>,
    workflow: &Document,
    path: &String,
    producer: Option<&Document>,
    c: &BindingContext<'_>,
    diagnostics: bool,
) -> Result<Vec<ResolvedStep>, Error> {
    let refs = iterable(workflow, field(workflow, workflow.root(), "steps")?)?;
    let mut resolved = Vec::new();
    let mut names = HashSet::new();
    let mut outputs = HashSet::new();
    let mut interfaces: BTreeMap<Vec<u32>, (Arc<Document>, Option<NodeId>)> = BTreeMap::new();
    for (index, reference) in refs.iter().enumerate() {
        let where_ = at(path, &format!("/steps/{index}"))?;
        let found = load(
            conn,
            project,
            workflow,
            reference.id()?,
            true,
            &where_,
            c,
            diagnostics,
        )
        .await?;
        if !names.insert(found.name.clone()) {
            if diagnostics {
                return Err(diagnostic::validation(
                    at(&where_, "/name")?,
                    &[
                        String::from("step "),
                        cannery_core::text::repr_string(&found.name).map_err(ScienceError::from)?,
                        String::from(" appears twice in the workflow"),
                    ],
                )?);
            }
            return Err(fail(&where_, "/name", "step appears twice in the workflow"));
        }
        steps::check_step(
            science,
            &found.manifest,
            Role::Experiment,
            &where_,
            c.rendering,
        )
        .map_err(|e| {
            diagnostic::step(
                e,
                science,
                &found.manifest,
                Role::Experiment,
                c,
                diagnostics,
            )
        })?;
        for (i, artifact) in artifact_items(&found.manifest, Side::Inputs)?
            .iter()
            .enumerate()
        {
            let id = artifact.id()?;
            let from = field(&found.manifest, id, "from")?;
            // Source short-circuit: name is untouched for non-attempt/non-step.
            if is(&found.manifest, from, "attempt")
                && is(
                    &found.manifest,
                    field(&found.manifest, id, "name")?,
                    "claimed_sheet",
                )
            {
                if diagnostics {
                    return Err(diagnostic::validation(
                        at(&where_, &format!("/spec/inputs/artifacts/{i}/name"))?,
                        &[String::from(
                            "an experiment reads its predecessor's artifacts, not a claimed sheet",
                        )],
                    )?);
                }
                return Err(fail(
                    &where_,
                    &format!("/spec/inputs/artifacts/{i}/name"),
                    "an experiment reads predecessor artifacts, not a claimed sheet",
                ));
            }
            if !is(&found.manifest, from, "step") {
                continue;
            }
            let name = text(&found.manifest, field(&found.manifest, id, "name")?, c)?;
            let output = interfaces.get(&name.codepoints()).ok_or_else(|| {
                (|| {
                    if diagnostics {
                        return diagnostic::validation(
                            at(&where_, &format!("/spec/inputs/artifacts/{i}"))
                                .map_err(|_| ScienceError::InvalidNode)?,
                            &[
                                String::from("no earlier step of the workflow outputs "),
                                cannery_core::text::repr_string(&name)
                                    .map_err(ScienceError::from)?,
                            ],
                        );
                    }
                    Ok(fail(
                        &where_,
                        &format!("/spec/inputs/artifacts/{i}"),
                        "no earlier step outputs this artifact",
                    ))
                })()
                .unwrap_or_else(Error::from)
            })?;
            let interface = field(&found.manifest, id, "interface")?;
            if !c.equality.equal(
                output.1.map(|id| (&*output.0, id)),
                Some((&found.manifest, interface)),
                c.rendering,
            )? {
                if diagnostics {
                    let actual = output
                        .1
                        .map_or_else(|| Ok(String::from("None")), |id| text(&output.0, id, c))?;
                    return Err(diagnostic::validation(
                        at(&where_, &format!("/spec/inputs/artifacts/{i}/interface"))?,
                        &[
                            String::from("the workflow's "),
                            cannery_core::text::repr_string(&name).map_err(ScienceError::from)?,
                            String::from(" output is "),
                            actual,
                            String::from(", not "),
                            text(&found.manifest, interface, c)?,
                        ],
                    )?);
                }
                return Err(fail(
                    &where_,
                    &format!("/spec/inputs/artifacts/{i}/interface"),
                    "workflow artifact interfaces differ",
                ));
            }
        }
        let last = index + 1 == refs.len();
        for (i, artifact) in artifact_items(&found.manifest, Side::Outputs)?
            .iter()
            .enumerate()
        {
            let id = artifact.id()?;
            let name = text(&found.manifest, field(&found.manifest, id, "name")?, c)?;
            let suffix = format!("/spec/outputs/artifacts/{i}");
            if ["step_log", "setup_log", "validator_log"]
                .iter()
                .any(|v| name.equals_utf8(v))
            {
                if diagnostics {
                    return Err(diagnostic::validation(
                        at(&where_, &format!("{suffix}/name"))?,
                        &[
                            cannery_core::text::repr_string(&name).map_err(ScienceError::from)?,
                            String::from(" is the role of the runner's logs"),
                        ],
                    )?);
                }
                return Err(fail(
                    &where_,
                    &format!("{suffix}/name"),
                    "output is a runner log role",
                ));
            }
            if outputs.contains(&name) {
                if diagnostics {
                    return Err(diagnostic::validation(
                        at(&where_, &format!("{suffix}/name"))?,
                        &[
                            String::from("another step of the workflow outputs "),
                            cannery_core::text::repr_string(&name).map_err(ScienceError::from)?,
                        ],
                    )?);
                }
                return Err(fail(
                    &where_,
                    &format!("{suffix}/name"),
                    "another step outputs this artifact",
                ));
            }
            let sheet = name.equals_utf8("run");
            if sheet && !last {
                if diagnostics {
                    return Err(diagnostic::validation(
                        at(&where_, &format!("{suffix}/name"))?,
                        &[String::from(
                            "only the workflow's last step outputs the run document",
                        )],
                    )?);
                }
                return Err(fail(
                    &where_,
                    &format!("{suffix}/name"),
                    "only the workflow's last step outputs the run document",
                ));
            }
            let interface = found.manifest.field(id, "interface");
            if sheet && !interface.is_some_and(|id| is(&found.manifest, id, steps::RUN_INTERFACE)) {
                if diagnostics {
                    return Err(diagnostic::validation(
                        at(&where_, &format!("{suffix}/interface"))?,
                        &[String::from("the run document is cr-run/v0.2")],
                    )?);
                }
                return Err(fail(
                    &where_,
                    &format!("{suffix}/interface"),
                    "the run document is cr-run/v0.2",
                ));
            }
            interfaces.insert(
                name.codepoints().clone(),
                (found.manifest.clone(), interface),
            );
            outputs.insert(name);
        }
        resolved.push(found);
    }
    if !outputs.contains(&String::from("run")) {
        let last = BigInt::from(refs.len()) - 1;
        if diagnostics {
            return Err(diagnostic::validation(
                at(path, &format!("/steps/{last}"))?,
                &[String::from(
                    "the last step must output 'run' (cr-run/v0.2)",
                )],
            )?);
        }
        return Err(fail(
            path,
            &format!("/steps/{last}"),
            "the last step must output run",
        ));
    }
    let required = field(
        science.content,
        science.content.root(),
        "required_artifact_roles",
    )?;
    let required = field(science.content, required, "attempt")?;
    let mut needed = BTreeMap::new();
    for role in iterable(science.content, required)? {
        // A role is its name, or `{role, description}`.
        let role = match role {
            Item::Node(id) if matches!(science.content.node(id), Some(Node::Object(_))) => {
                text(science.content, field(science.content, id, "role")?, c)?
            }
            other => other.text(science.content, c)?,
        };
        needed.insert(
            role.codepoints().clone(),
            (role, "the science revision requires it"),
        );
    }
    if let Some(producer) = producer {
        for artifact in artifact_items(producer, Side::Inputs)? {
            let id = artifact.id()?;
            if is(producer, field(producer, id, "from")?, "attempt")
                && !is(producer, field(producer, id, "name")?, "claimed_sheet")
            {
                let name = text(producer, field(producer, id, "name")?, c)?;
                needed
                    .entry(name.codepoints().clone())
                    .or_insert((name, "the track's producer reads it"));
            }
        }
    }
    for (name, why) in needed.into_values() {
        if !outputs.contains(&name) {
            if diagnostics {
                return Err(diagnostic::validation(
                    at(path, "/steps")?,
                    &[
                        String::from("no step outputs the attempt artifact "),
                        cannery_core::text::repr_string(&name).map_err(ScienceError::from)?,
                        String::from(&format!(": {why}")),
                    ],
                )?);
            }
            return Err(fail(
                path,
                "/steps",
                "no step outputs a required attempt artifact",
            ));
        }
    }
    Ok(resolved)
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
