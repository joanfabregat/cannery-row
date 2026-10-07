//! Render only the violation already selected by the shared semantic checks.
use super::{BindingContext, Error, Validation};
use cannery_core::{
    json::{Document, Node, NodeId},
    text,
};
use cannery_research::{
    science::{Science, ScienceError},
    steps::{self, Role, Side},
};

pub(super) fn validation(path: String, parts: &[String]) -> Result<Error, ScienceError> {
    let points = parts
        .iter()
        .flat_map(|part| part.chars().map(u32::from))
        .collect();
    Ok(Error::Validation(Validation {
        path,
        message: cannery_core::text::from_codepoints(points).ok_or(ScienceError::InvalidNode)?,
    }))
}
fn literal(value: &str) -> String {
    String::from(value)
}
fn rendered(d: &Document, id: NodeId, c: &BindingContext<'_>) -> Result<String, ScienceError> {
    text::str_value(d, id, c.rendering.nesting_budget).map_err(ScienceError::from)
}
fn repr(value: &str) -> Result<String, ScienceError> {
    text::repr_string(value).map_err(ScienceError::from)
}
fn repr_value(d: &Document, id: NodeId, c: &BindingContext<'_>) -> Result<String, ScienceError> {
    match d.node(id) {
        Some(Node::String(value)) => repr(value),
        _ => rendered(d, id, c),
    }
}
fn required(d: &Document, id: NodeId, name: &str) -> Result<NodeId, ScienceError> {
    super::field(d, id, name).map_err(source)
}
fn source(error: Error) -> ScienceError {
    match error {
        Error::Science(error) => error,
        _ => ScienceError::InvalidNode,
    }
}
fn lookup(d: &Document, pointer: &str) -> Result<NodeId, ScienceError> {
    let mut id = d.root();
    for name in pointer.split('/').filter(|name| !name.is_empty()) {
        id = match d.node(id) {
            Some(Node::Array(values)) => *values
                .get(
                    name.parse::<usize>()
                        .map_err(|_| ScienceError::InvalidNode)?,
                )
                .ok_or(ScienceError::InvalidNode)?,
            _ => required(d, id, &name.replace("~1", "/").replace("~0", "~"))?,
        };
    }
    Ok(id)
}
fn suffix(path: &String) -> Result<String, ScienceError> {
    let text = path.as_utf8().ok_or(ScienceError::InvalidNode)?;
    Ok(text[text.find("/spec").ok_or(ScienceError::InvalidNode)?..].to_owned())
}
#[allow(
    clippy::too_many_lines,
    reason = "Each selected source violation has a distinct public diagnostic"
)]
pub(super) fn step(
    error: ScienceError,
    science: &Science<'_>,
    d: &Document,
    role: Role,
    c: &BindingContext<'_>,
    enabled: bool,
) -> Error {
    if !enabled {
        return error.into();
    }
    let ScienceError::Validation { path, message } = error else {
        return error.into();
    };
    let result = (|| {
        let pointer = suffix(&path)?;
        let parts = match message {
            "role differs" => vec![literal(
                &format!("must be {:?}", role.name()).replace('"', "'"),
            )],
            "environment name is reserved" => vec![literal(
                "NVIDIA_* variables are reserved: the container runtime would act on them",
            )],
            "environment value looks secret" => vec![literal(
                "looks like a secret; step manifests carry plain configuration only",
            )],
            "artifact name is duplicated" => vec![literal("names are unique within a step")],
            "artifact path is duplicated" => vec![literal("paths are unique within a step")],
            "no step precedes a producer" => {
                vec![literal("a producer runs first; no step precedes it")]
            }
            "no resource ceiling" => {
                let name = pointer
                    .rsplit('/')
                    .next()
                    .ok_or(ScienceError::InvalidNode)?
                    .replace("~1", "/")
                    .replace("~0", "~");
                vec![
                    literal("the science revision sets no ceiling for "),
                    repr(&literal(&name))?,
                ]
            }
            "resource ceiling exceeded" => {
                let name = pointer
                    .rsplit('/')
                    .next()
                    .ok_or(ScienceError::InvalidNode)?
                    .replace("~1", "/")
                    .replace("~0", "~");
                let limit = science.limits.ok_or(ScienceError::InvalidNode)?;
                let ceilings = required(science.content, limit, "resource_ceilings")?;
                vec![
                    rendered(d, lookup(d, &pointer)?, c)?,
                    literal(" exceeds the ceiling of "),
                    rendered(
                        science.content,
                        required(science.content, ceilings, &name)?,
                        c,
                    )?,
                ]
            }
            "deadline ceiling exceeded" | "setup deadline ceiling exceeded" => {
                let limit = science.limits.ok_or(ScienceError::InvalidNode)?;
                let maximum = rendered(
                    science.content,
                    required(science.content, limit, "max_deadline_seconds")?,
                    c,
                )?;
                if message == "deadline ceiling exceeded" {
                    vec![
                        literal("exceeds the science revision's max_deadline_seconds ("),
                        maximum,
                        literal(")"),
                    ]
                } else {
                    vec![
                        literal(&format!(
                            "the setup's {}s exceed the science revision's max_deadline_seconds (",
                            steps::setup_deadline(d)?.ok_or(ScienceError::InvalidNode)?
                        )),
                        maximum,
                        literal(")"),
                    ]
                }
            }
            "repository is not allowed" => vec![
                rendered(d, lookup(d, &pointer)?, c)?,
                literal(&format!(
                    " is not in the science revision's code_repositories.{}",
                    role.trust().name()
                )),
            ],
            "input is not registered" | "held-out labels are forbidden" => {
                let base = pointer
                    .split("/artifacts/")
                    .nth(1)
                    .ok_or(ScienceError::InvalidNode)?
                    .split('/')
                    .next()
                    .ok_or(ScienceError::InvalidNode)?;
                let artifact = lookup(d, &format!("/spec/inputs/artifacts/{base}"))?;
                let ident = steps::source_id(d, artifact, c.rendering)?;
                let origin = rendered(d, required(d, artifact, "from")?, c)?;
                if message == "held-out labels are forbidden" {
                    vec![
                        literal("dataset "),
                        repr(&ident)?,
                        literal(
                            " holds held-out labels; only trusted steps that judge (the scorer, an evaluator policy step) may receive them",
                        ),
                    ]
                } else {
                    vec![
                        origin,
                        literal(" "),
                        repr(&ident)?,
                        literal(" is not registered"),
                    ]
                }
            }
            "interface is not registered" | "output interface is not registered" => vec![
                literal("interface "),
                if message == "output interface is not registered" {
                    repr_value(d, lookup(d, &pointer)?, c)?
                } else {
                    repr(&rendered(d, lookup(d, &pointer)?, c)?)?
                },
                literal(" is not registered in the science revision"),
            ],
            _ => return Err(ScienceError::InvalidNode),
        };
        validation(path, &parts)
    })();
    result.unwrap_or_else(Error::from)
}
pub(super) fn pair(
    error: ScienceError,
    science: &Science<'_>,
    d: &Document,
    c: &BindingContext<'_>,
    enabled: bool,
) -> Error {
    if !enabled {
        return error.into();
    }
    let ScienceError::Validation { path, message } = error else {
        return error.into();
    };
    let result = (|| {
        let outputs = super::artifact_items(d, Side::Outputs).map_err(source)?;
        let scorer_inputs = steps::artifacts(science.content, science.scorer()?, Side::Inputs)?;
        for artifact in super::iterable(science.content, scorer_inputs).map_err(source)? {
            let artifact = artifact.id().map_err(source)?;
            let from = required(science.content, artifact, "from")?;
            if !matches!(science.content.node(from), Some(Node::String(v)) if v.equals_utf8("step"))
            {
                continue;
            }
            let name = rendered(
                science.content,
                required(science.content, artifact, "name")?,
                c,
            )?;
            let expected = rendered(
                science.content,
                required(science.content, artifact, "interface")?,
                c,
            )?;
            let mut found = None;
            for (index, output) in outputs.iter().enumerate() {
                let output = output.id().map_err(source)?;
                if rendered(d, required(d, output, "name")?, c)? == name {
                    found = Some((index, d.field(output, "interface")));
                }
            }
            match found {
                None if message == "scorer input lacks a producer output" => {
                    return validation(
                        path,
                        &[
                            literal("the scorer reads "),
                            repr(&name)?,
                            literal(" ("),
                            expected,
                            literal(") from the producer, which lacks it"),
                        ],
                    );
                }
                Some((index, interface))
                    if message == "producer interface differs from scorer"
                        && !interface.is_some_and(|id| matches!(d.node(id), Some(Node::String(value)) if value == &expected))
                        && suffix(&path)?
                            == format!("/spec/outputs/artifacts/{index}/interface") =>
                {
                    let actual =
                        interface.map_or_else(|| Ok(literal("None")), |id| rendered(d, id, c))?;
                    return validation(
                        path,
                        &[
                            literal("the scorer's "),
                            repr(&name)?,
                            literal(" input expects "),
                            expected,
                            literal(", not "),
                            actual,
                        ],
                    );
                }
                _ => {}
            }
        }
        Err(ScienceError::InvalidNode)
    })();
    result.unwrap_or_else(Error::from)
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
