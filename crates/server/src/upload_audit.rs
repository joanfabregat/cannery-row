//! Capability audit retains the stored channel until PostgreSQL's actual CHECK.
//! The checked INSERT is identical to `core::audit::record`; parent can consolidate ownership.
use crate::{
    attempt_lease_routes::{Failure, internal},
    requests::RequestContext,
};
use cannery_attempts::model::Attempt;
use cannery_core::audit::Record;
use sqlx::PgConnection;

pub(crate) fn actor(attempt: &Attempt, context: &RequestContext) -> Result<&'static str, Failure> {
    match (attempt.claimed_by_user, attempt.claimed_by_service) {
        (Some(_), None) => Ok("user"),
        (None, Some(_)) => Ok("service"),
        _ => Err(internal(context, "upload actor identity")),
    }
}
pub(crate) async fn record(
    connection: &mut PgConnection,
    attempt: &Attempt,
    kind: &str,
    event: Record<'_>,
    context: &RequestContext,
) -> Result<(), Failure> {
    let prior = event.prior_state.filter(|v| !v.is_null());
    let new = event.new_state.filter(|v| !v.is_null());
    sqlx::query!(
        r#"INSERT INTO audit_events (
              project_id, actor_kind, actor_user_id, actor_service_id, via_channel,
              via_client, action, subject_type, subject_id, prior_state, new_state,
              reason, idempotency_key)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
           RETURNING seq"#,
        event.project_id as _,
        kind,
        attempt.claimed_by_user as _,
        attempt.claimed_by_service as _,
        &attempt.via_channel,
        None::<&str>,
        event.action,
        event.subject_type,
        event.subject_id,
        prior as _,
        new as _,
        event.reason,
        event.idempotency_key,
    )
    .fetch_one(connection)
    .await
    .map_err(|_| internal(context, "upload capability audit"))?;
    Ok(())
}
