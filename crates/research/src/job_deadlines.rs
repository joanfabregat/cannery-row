//! Source deadlines shared by experiment workflows and test/evaluation jobs.
use crate::{
    science::{self, RenderingContext, Science, ScienceError, Value},
    steps::{self, Side},
};
use cannery_core::json::{Document, NodeId};
use num_bigint::BigInt;

fn step_seconds_at(document: &Document, root: NodeId) -> Result<BigInt, ScienceError> {
    let spec = science::required(document, &Value::Node(root), "spec")?;
    let deadline = science::required(document, &Value::Node(spec), "activeDeadlineSeconds")?;
    let deadline = science::configuration_integer(document, deadline)?;
    Ok(deadline + steps::setup_deadline_at(document, root)?.unwrap_or_default())
}

/// A step's deadline plus its optional setup deadline, using source integer conversion.
/// # Errors
/// Preserves consumed malformed-value and conversion error categories.
pub fn step_seconds(manifest: &Document) -> Result<BigInt, ScienceError> {
    step_seconds_at(manifest, manifest.root())
}

/// Sum validators selected by output interfaces, including repeated outputs.
/// # Errors
/// Preserves source registry lookup and malformed-output error order.
pub fn validation_seconds(
    science: &Science<'_>,
    manifest: &Document,
    rendering: RenderingContext,
) -> Result<BigInt, ScienceError> {
    let outputs = steps::artifacts(manifest, manifest.root(), Side::Outputs)?;
    let mut total = BigInt::from(0);
    for output in science::items(manifest, outputs)? {
        let reference = science::field(manifest, &output, "interface", false)?.map_or_else(
            || Ok(String::from("None")),
            |value| science::text(manifest, &Value::Node(value), rendering),
        )?;
        if let Some((_, interface)) = science
            .interface_specs
            .iter()
            .find(|(key, _)| key == &reference)
            && let Some(validator) = &interface.validator
        {
            let (_, manifest) = science
                .validators
                .iter()
                .find(|(key, _)| key == validator)
                .ok_or(ScienceError::Key)?;
            total += step_seconds_at(science.content, *manifest)?;
        }
    }
    Ok(total)
}

/// Sum each step and each output's selected validator, retaining duplicate outputs.
/// # Errors
/// Traverses in source order and returns the first consumed failure.
pub fn steps_seconds(
    science: &Science<'_>,
    manifests: &[&Document],
    rendering: RenderingContext,
) -> Result<BigInt, ScienceError> {
    let mut total = BigInt::from(0);
    for manifest in manifests {
        total += step_seconds(manifest)? + validation_seconds(science, manifest, rendering)?;
    }
    Ok(total)
}

/// Add the caller's staging/upload overhead to the workflow's step deadlines.
/// # Errors
/// Preserves the same ordered deadline and validator failures as job creation.
pub fn workflow_seconds(
    science: &Science<'_>,
    manifests: &[&Document],
    overhead: &BigInt,
    rendering: RenderingContext,
) -> Result<BigInt, ScienceError> {
    Ok(overhead + steps_seconds(science, manifests, rendering)?)
}
