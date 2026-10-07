//! Registered interface lookup and shared bounded streaming checks.
use crate::{
    attempt_lease_routes::{Failure, internal},
    job_completion_routes::{JobLifecycleContext, invalid},
    job_lifecycle as flow,
    requests::RequestContext,
};
use cannery_attempts::model::Attempt;
use cannery_research::interfaces::{ContentChecker, Interface, ValidationContext};
use serde_json::Value;
use sqlx::PgConnection;
pub(crate) async fn load(
    c: &mut PgConnection,
    a: &Attempt,
    reference: &str,
    s: &JobLifecycleContext,
    r: &RequestContext,
) -> Result<Option<Interface>, Failure> {
    if reference == "cr-evidence/v0.2" {
        return Ok(None);
    }
    let raw = flow::science(c, a, &s.flow, r).await?;
    let science = cannery_research::science::Science::new(
        a.science_revision.into(),
        &raw.content,
        s.flow.rendering,
    )
    .map_err(|_| internal(r, "job interface science"))?;
    let spec = science
        .interface_specs
        .iter()
        .find(|(key, _)| key.equals_utf8(reference))
        .map(|v| &v.1)
        .ok_or_else(|| {
            invalid(
                "/interface",
                "interface is not registered in the job's science revision",
            )
        })?;
    Interface::from_spec(&raw.content, spec)
        .map(Some)
        .map_err(|_| internal(r, "job interface registration"))
}
pub(crate) async fn check(
    store: &cannery_storage::ObjectStore,
    key: &str,
    size: u64,
    mut interface: Interface,
    cap: usize,
    s: &JobLifecycleContext,
    r: &RequestContext,
) -> Result<(Vec<Value>, bool), Failure> {
    let validated =
        interface.parses_content() && usize::try_from(size).is_ok_and(|size| size <= cap);
    interface.validate &= validated;
    let mut checker = ContentChecker::new(
        interface,
        cap,
        ValidationContext {
            json_decode_budget: s.flow.jobs.decode_nesting_budget,
        },
    )
    .map_err(|_| internal(r, "job output checker"))?;
    let mut reader = store
        .read(key)
        .await
        .map_err(|e| crate::upload_direct::store_error(e, r))?;
    while let Some(chunk) = reader
        .next_chunk()
        .await
        .map_err(|e| crate::upload_direct::store_error(e, r))?
    {
        checker
            .feed(&chunk)
            .map_err(|_| internal(r, "job output check"))?;
        if checker.done() {
            break;
        }
    }
    let issues = checker
        .finish()
        .map_err(|_| internal(r, "job output schema execution"))?;
    let issues = issues
        .into_iter()
        .map(|issue| {
            let mut value = serde_json::json!({"message":issue.message});
            if let Some(path) = issue.pointer {
                value["path"] = serde_json::json!(path);
            }
            if let Some(line) = issue.line {
                value["line"] = serde_json::json!(line);
            }
            value
        })
        .collect();
    Ok((issues, validated))
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
