//! Failure transitions share the caller's transaction; authorization stays outside.
use crate::{
    attempt_lease_routes::{Failure, internal},
    attempt_release_routes::AttemptReleaseContext,
    requests::RequestContext,
};
use cannery_attempts::{
    model::{Attempt, JsonContext},
    repo::{RecordFailure, Repository, RequeueFailed},
};
use cannery_core::{
    audit::{self, Attribution, Record},
    json::Document,
    principal::Principal,
};
use cannery_research::{
    config_repo::{self, Kind},
    science::Science,
};
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use serde_json::json;
use sqlx::PgConnection;

pub(crate) struct Report<'a> {
    pub code: &'a str,
    pub reason: &'a str,
    pub details: &'a Document,
    pub log_refs: Option<&'a Document>,
    pub step: Option<&'a str>,
    pub idempotency_key: Option<&'a str>,
    pub manifest_sha256: Option<&'a str>,
}
pub(crate) fn is_infrastructure(attempt: &Attempt, code: &str) -> bool {
    !attempt.workflow.python_none()
        && (crate::attempt_release_request::CODES.contains(&code) || code == "lease_expired")
}
async fn failed_audit(
    conn: &mut PgConnection,
    actor: Attribution<'_>,
    attempt: &Attempt,
    report: &Report<'_>,
    requeued: bool,
    context: &RequestContext,
) -> Result<(), Failure> {
    let prior = json!({"state":attempt.state.as_str()});
    let mut new = json!({"state":"failed","code":report.code});
    if let Some(step) = report.step {
        new["step"] = json!(step);
    }
    if let Some(sha) = report.manifest_sha256 {
        new["manifest_sha256"] = json!(sha);
    }
    if requeued {
        new["requeued"] = json!(true);
    }
    audit::record(
        conn,
        actor,
        Record {
            action: "attempt.failed",
            subject_type: "attempt",
            subject_id: &attempt.id.0.to_string(),
            project_id: Some(attempt.project_id),
            prior_state: Some(&prior),
            new_state: Some(&new),
            reason: Some(report.reason),
            idempotency_key: report.idempotency_key,
        },
    )
    .await
    .map(|_| ())
    .map_err(|_| internal(context, "attempt failure audit"))
}
pub(crate) async fn agent(
    conn: &mut PgConnection,
    profile: JsonContext,
    principal: &Principal,
    attempt: &Attempt,
    report: &Report<'_>,
    context: &RequestContext,
) -> Result<(), Failure> {
    agent_as(
        conn,
        profile,
        Attribution::Principal(principal),
        attempt,
        report,
        context,
    )
    .await
}
pub(crate) async fn agent_as(
    conn: &mut PgConnection,
    profile: JsonContext,
    actor: Attribution<'_>,
    attempt: &Attempt,
    report: &Report<'_>,
    context: &RequestContext,
) -> Result<(), Failure> {
    Repository::new(conn, profile)
        .record_failure(RecordFailure {
            project_id: attempt.project_id,
            attempt,
            stage: "agent",
            code: report.code,
            reason: report.reason,
            details: report.details,
            log_refs: report.log_refs,
            from_state: None,
        })
        .await
        .map_err(|_| internal(context, "attempt failure transition"))?;
    failed_audit(conn, actor, attempt, report, false, context).await
}
#[allow(
    clippy::too_many_arguments,
    reason = "Explicit caller-owned connection, profiles, attribution and report"
)]
pub(crate) async fn experiment(
    conn: &mut PgConnection,
    profile: JsonContext,
    principal: &Principal,
    attempt: &Attempt,
    report: &Report<'_>,
    release: &AttemptReleaseContext,
    context: &RequestContext,
) -> Result<bool, Failure> {
    experiment_as(
        conn,
        profile,
        Attribution::Principal(principal),
        attempt,
        report,
        release,
        context,
    )
    .await
}
#[allow(
    clippy::too_many_arguments,
    reason = "One failure transaction preserves explicit caller attribution and profiles"
)]
pub(crate) async fn experiment_as(
    conn: &mut PgConnection,
    profile: JsonContext,
    actor: Attribution<'_>,
    attempt: &Attempt,
    report: &Report<'_>,
    release: &AttemptReleaseContext,
    context: &RequestContext,
) -> Result<bool, Failure> {
    if is_infrastructure(attempt, report.code) {
        let used = Repository::new(conn, profile)
            .automatic_requeues(attempt.unit_id)
            .await
            .map_err(|_| internal(context, "automatic retry count"))?;
        let revision = config_repo::get_revision(
            conn,
            attempt.project_id,
            Kind::Science,
            Some(&BigInt::from(attempt.science_revision)),
            release.configuration,
        )
        .await
        .map_err(|_| internal(context, "retry science lookup"))?
        .ok_or_else(|| internal(context, "retry science absent"))?;
        let science = Science::new(
            BigInt::from(revision.revision),
            &revision.content,
            release.science,
        )
        .map_err(|_| internal(context, "retry science construction"))?;
        let allowed = science
            .max_auto_retries()
            .map_err(|_| internal(context, "retry limit conversion"))?;
        if BigInt::from(used) < allowed {
            Repository::new(conn, profile)
                .requeue_failed(RequeueFailed {
                    attempt,
                    code: report.code,
                    reason: report.reason,
                    details: report.details,
                    log_refs: report.log_refs,
                })
                .await
                .map_err(|_| internal(context, "automatic retry transition"))?;
            failed_audit(conn, actor, attempt, report, true, context).await?;
            let allowed = allowed
                .to_i64()
                .ok_or_else(|| internal(context, "retry audit integer"))?;
            let rendered = allowed.to_string();
            let prior = json!({"state":"active"});
            let new = json!({"state":"queued","automatic":true,"failed_attempt":format!("#{}.{}",attempt.unit_number,attempt.sequence),"code":report.code,"retry":used+1,"max_auto_retries":allowed});
            let reason = format!(
                "automatic retry {} of {rendered} after an infrastructure failure",
                used + 1
            );
            audit::record(
                conn,
                actor,
                Record {
                    action: "unit.requeued",
                    subject_type: "unit",
                    subject_id: &attempt.unit_id.0.to_string(),
                    project_id: Some(attempt.project_id),
                    prior_state: Some(&prior),
                    new_state: Some(&new),
                    reason: Some(&reason),
                    idempotency_key: None,
                },
            )
            .await
            .map_err(|_| internal(context, "automatic retry audit"))?;
            return Ok(true);
        }
    }
    agent_as(conn, profile, actor, attempt, report, context).await?;
    Ok(false)
}

/// Release an attempt whose blocking question went unanswered: the unit is
/// queued again whatever its retry budget, and the question, answered or
/// not, reaches its next attempt.
pub(crate) async fn unanswered(
    conn: &mut PgConnection,
    profile: JsonContext,
    actor: Attribution<'_>,
    attempt: &Attempt,
    report: &Report<'_>,
    context: &RequestContext,
) -> Result<(), Failure> {
    Repository::new(conn, profile)
        .requeue_failed(RequeueFailed {
            attempt,
            code: report.code,
            reason: report.reason,
            details: report.details,
            log_refs: report.log_refs,
        })
        .await
        .map_err(|_| internal(context, "unanswered question transition"))?;
    failed_audit(conn, actor, attempt, report, true, context).await?;
    let prior = json!({"state":"active"});
    let new = json!({"state":"queued","failed_attempt":format!("#{}.{}",attempt.unit_number,attempt.sequence),"code":report.code});
    audit::record(
        conn,
        actor,
        Record {
            action: "unit.requeued",
            subject_type: "unit",
            subject_id: &attempt.unit_id.0.to_string(),
            project_id: Some(attempt.project_id),
            prior_state: Some(&prior),
            new_state: Some(&new),
            reason: Some(report.reason),
            idempotency_key: None,
        },
    )
    .await
    .map(|_| ())
    .map_err(|_| internal(context, "unanswered question audit"))
}
