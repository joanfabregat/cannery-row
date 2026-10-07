//! Track callers preserve producer-first order while sharing checked resolution.
pub(crate) use crate::step_binding::Error;
use cannery_core::{ids::ProjectId, json::Document};
use cannery_research::science::{RenderingContext, Science, ScienceError};
use sqlx::PgConnection;
fn fail(path: &str, message: &'static str) -> Error {
    ScienceError::Validation {
        path: String::from(path),
        message,
    }
    .into()
}
pub(crate) async fn check(
    conn: &mut PgConnection,
    project: ProjectId,
    science: Option<&Science<'_>>,
    binding: Option<&Document>,
    workflow: Option<&Document>,
    rendering: RenderingContext,
    budget: usize,
) -> Result<(), Error> {
    let mut conn = crate::step_binding::PgManifestQuery(conn);
    let equality = crate::step_binding::CheckedOutputEquality;
    let context = crate::step_binding::BindingContext {
        rendering,
        decode_budget: budget,
        equality: &equality,
    };
    if binding.is_some() {
        let science = science.ok_or_else(|| {
            fail(
                "/producer",
                "binding a producer needs a science revision to check it",
            )
        })?;
        crate::step_binding::resolve_producer(
            &mut conn,
            project,
            science,
            binding,
            &String::from("/producer"),
            &context,
        )
        .await?;
    }
    let Some(workflow) = workflow else {
        return Ok(());
    };
    let science = science.ok_or_else(|| {
        fail(
            "/workflow",
            "a workflow needs a science revision to check it",
        )
    })?;
    let bound = crate::step_binding::resolve_producer(
        &mut conn,
        project,
        science,
        binding,
        &String::from("/producer"),
        &context,
    )
    .await?;
    crate::step_binding::resolve_workflow(
        &mut conn,
        project,
        science,
        workflow,
        &String::from("/workflow"),
        Some(&bound.manifest),
        &context,
    )
    .await?;
    Ok(())
}
