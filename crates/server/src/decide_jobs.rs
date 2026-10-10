// SPDX-License-Identifier: AGPL-3.0-only
//! Decide jobs: automatic decisions. When the science revision a unit's
//! attempt pinned registers a decider step (`decide.performer: step`), the
//! unit's decision case opens as for a researcher and a decide job is
//! queued on its last attempt beside it. The registered decider service
//! account claims it, runs its decider step (`cannery runner`, kind
//! `decide`) and completes the job with the decision document, recorded on
//! the case with the decider as its actor. A decider promotes only on a
//! `pass` verdict, as a researcher does.
//!
//! While a decide job waits or runs, a researcher cannot decide the case; a
//! researcher corrects the decider's decision once it is recorded, with
//! `supersedes`. A failed or expired decide job is rerun up to the science
//! revision's `max_auto_retries`; after that the case is left to researchers.
#![allow(
    clippy::too_many_arguments,
    clippy::many_single_char_names,
    reason = "Caller-owned transactions keep explicit identity, contexts and diagnostics, named as in the job lifecycle"
)]
use crate::{
    attempt_lease_routes::{Failure, domain, internal},
    job_completion_routes::invalid,
    job_lifecycle as flow,
    requests::RequestContext,
};
use cannery_attempts::{
    model::{Attempt, EvidenceId, StoredJson},
    repo::Repository,
};
use cannery_core::{
    audit::{self, Attribution, Record},
    contracts::phases::{Phase as DocumentPhase, PhaseSchemas},
    errors::ErrorCode,
    front_matter::Limits,
    ids::{JobId, ReviewCaseId},
    principal::Principal,
};
use cannery_jobs::repo::{self as jobs, Job, Performer, Phase};
use cannery_units::repo::{self as units, DecisionAction, UnitState};
use num_bigint::BigInt;
use serde_json::{Value, json};
use sqlx::PgConnection;

/// The time a decide job allows beyond the science's `max_deadline_seconds`.
pub(crate) const OVERHEAD_SECONDS: i64 = 300;

pub(crate) fn output_prefix(a: &Attempt, id: JobId) -> String {
    format!(
        "projects/{}/attempts/{}/decide-runs/{}/",
        a.project_id, a.id, id
    )
}

/// The decider the science revision attempt `a` pinned registers, and the
/// revision of its step; none when researchers decide.
pub(crate) async fn decider(
    c: &mut PgConnection,
    a: &Attempt,
    s: &flow::Context,
    r: &RequestContext,
) -> Result<Option<(String, String, Value)>, Failure> {
    let raw = flow::science(c, a, s, r).await?;
    let registered = flow::value(&raw.content, s, r)?;
    let decide = &registered["decide"];
    if decide["performer"] != "step" {
        return Ok(None);
    }
    let text = |key: &str| {
        decide["decider"][key]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| internal(r, "registered decider"))
    };
    Ok(Some((text("id")?, text("revision")?, registered)))
}

/// Queue a decide job for the decision case `case` of the unit of
/// attempt `a`, its last, when its science revision registers a decider.
/// The caller holds the attempt lock and has opened the case.
pub(crate) async fn create(
    c: &mut PgConnection,
    actor: Attribution<'_>,
    a: &Attempt,
    case: ReviewCaseId,
    origin: jobs::Origin,
    previous: Option<JobId>,
    s: &flow::Context,
    r: &RequestContext,
) -> Result<Option<Job>, Failure> {
    let Some((name, revision, registered)) = decider(c, a, s, r).await? else {
        return Ok(None);
    };
    let limits = &registered["limits"];
    let allowance = limits
        .get("max_deadline_seconds")
        .and_then(Value::as_i64)
        .unwrap_or(3600);
    let output = limits["max_output_bytes"]
        .as_i64()
        .ok_or_else(|| internal(r, "decide job output limit"))?;
    let id = JobId(uuid::Uuid::new_v4());
    let spec = flow::document(
        &json!({"performer":"runner","decider":{"id":name,"revision":revision},
            "track":a.track_slug,"unit":a.unit_number,
            "review_case_id":case.to_string(),"output_prefix":output_prefix(a, id),
            "limits":{"max_output_bytes":output}}),
        s,
        r,
    )?;
    let job = jobs::create_job(
        c,
        jobs::NewJob {
            id,
            project_id: a.project_id,
            attempt_id: a.id,
            phase: Phase::Decide,
            performer: Performer::Runner,
            science_revision: &BigInt::from(a.science_revision),
            verifier_id: Some(&name),
            spec: &spec,
            deadline_seconds: &BigInt::from(OVERHEAD_SECONDS + allowance),
            origin,
            previous_run_id: previous,
        },
        s.jobs,
    )
    .await
    .map_err(|_| internal(r, "decide job creation"))?;
    flow::record_job_created_as(c, actor, a, &job, s, r).await?;
    Ok(Some(job))
}

/// The decision case a decide job resolves.
pub(crate) fn case_id(
    j: &Job,
    s: &flow::Context,
    r: &RequestContext,
) -> Result<ReviewCaseId, Failure> {
    let spec = flow::value(&j.spec, s, r)?;
    spec["review_case_id"]
        .as_str()
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .map(ReviewCaseId)
        .ok_or_else(|| internal(r, "decide job case"))
}

/// A phase output of attempt `a` a decision case cites.
async fn output(
    c: &mut PgConnection,
    a: &Attempt,
    id: Option<cannery_reviews::EvidenceId>,
    s: &flow::Context,
    r: &RequestContext,
) -> Result<Option<(EvidenceId, StoredJson, String)>, Failure> {
    let Some(id) = id.map(|id| EvidenceId(id.0)) else {
        return Ok(None);
    };
    Repository::new(c, s.attempts)
        .get_evidence_by_id(a.id, id)
        .await
        .map_err(|_| internal(r, "decide cited output"))?
        .map(|(stored, sha256)| Some((id, stored, sha256)))
        .ok_or_else(|| internal(r, "decide cited output invariant"))
}

/// What a decide job's decision document cites: the case's verification
/// report and write-up, each `{ref, sha256}` or null.
pub(crate) async fn cited(
    c: &mut PgConnection,
    a: &Attempt,
    case: &cannery_reviews::repo::Case,
    s: &flow::Context,
    r: &RequestContext,
) -> Result<(Value, Value, Option<String>), Failure> {
    let verification = output(c, a, case.evidence_id, s, r).await?;
    let writeup = output(c, a, case.writeup_id, s, r).await?;
    let reference = |found: &Option<(EvidenceId, StoredJson, String)>| {
        found.as_ref().map_or(
            Value::Null,
            |(id, _, sha256)| json!({"ref": id.0.to_string(), "sha256": sha256}),
        )
    };
    let verdict =
        match &verification {
            Some((_, StoredJson::Value(document), _)) => Some(
                cannery_core::json::to_value(document)
                    .map_err(|_| internal(r, "decide verdict"))?["verdict"]
                    .as_str()
                    .ok_or_else(|| internal(r, "decide verdict"))?
                    .to_owned(),
            ),
            Some(_) => return Err(internal(r, "decide verdict type")),
            None => None,
        };
    Ok((reference(&verification), reference(&writeup), verdict))
}

const fn outcome(action: DecisionAction) -> Option<UnitState> {
    match action {
        DecisionAction::Promote => Some(UnitState::Promoted),
        DecisionAction::Reject => Some(UnitState::Rejected),
        DecisionAction::Inconclusive => Some(UnitState::Inconclusive),
        DecisionAction::Failed => Some(UnitState::Failed),
        _ => None,
    }
}

/// Record the decision document `text` of claimed decide job `j` on its
/// decision case, with the decider as its actor, and complete the job. The
/// caller holds the job's lease and the attempt lock.
#[allow(
    clippy::too_many_lines,
    reason = "Decision checks, recording and audit stay in one visible order"
)]
pub(crate) async fn complete(
    c: &mut PgConnection,
    p: &Principal,
    a: &Attempt,
    j: &Job,
    text: &str,
    key: Option<&str>,
    phases: &PhaseSchemas,
    s: &flow::Context,
    r: &RequestContext,
) -> Result<(), Failure> {
    let Principal::Service(decider) = p else {
        return Err(internal(r, "decide job performer"));
    };
    let parsed = phases
        .parse(DocumentPhase::Decision, text, Limits::default())
        .map_err(|error| crate::document_jobs::document_error(&error, "/document", "decision"))?;
    let front_matter = Value::Object(parsed.front_matter);
    let reason = parsed.body.trim().to_owned();
    if reason.is_empty() {
        return Err(invalid(
            "/document",
            "the decision's body is its reason; state it",
        ));
    }
    let action = front_matter["outcome"]
        .as_str()
        .and_then(|outcome| DecisionAction::try_from(outcome).ok())
        .ok_or_else(|| internal(r, "decision outcome"))?;
    let to = outcome(action).ok_or_else(|| internal(r, "decision outcome state"))?;
    let state = sqlx::query_scalar!(
        "SELECT state FROM units WHERE id=$1 FOR UPDATE",
        a.unit_id.0 as _
    )
    .fetch_one(&mut *c)
    .await
    .map_err(|_| internal(r, "decide unit lock"))?;
    if state != "deciding" {
        return Err(domain(
            ErrorCode::Conflict,
            format!(
                "unit #{} is {state}; it no longer awaits a decision",
                a.unit_number
            ),
        ));
    }
    let id = case_id(j, s, r)?;
    let case = cannery_reviews::repo::get_case(c, a.project_id, id, true)
        .await
        .map_err(|_| internal(r, "decide case"))?
        .ok_or_else(|| internal(r, "decide case invariant"))?;
    if case.state == cannery_reviews::CaseState::Resolved {
        return Err(domain(
            ErrorCode::Conflict,
            "this decision case is already decided",
        ));
    }
    let (verification, writeup, verdict) = cited(c, a, &case, s, r).await?;
    if front_matter["verification"] != verification {
        return Err(invalid(
            "/document",
            if verification.is_null() {
                String::from(
                    "front matter /verification: null; the unit was stopped after a failure",
                )
            } else {
                format!(
                    "front matter /verification: cite the verification report {{ref: {}, sha256: {}}}",
                    verification["ref"].as_str().unwrap_or_default(),
                    verification["sha256"].as_str().unwrap_or_default()
                )
            },
        ));
    }
    if front_matter["writeup"] != writeup {
        return Err(invalid(
            "/document",
            if writeup.is_null() {
                String::from("front matter /writeup: null; the write-up was skipped")
            } else {
                format!(
                    "front matter /writeup: cite the write-up {{ref: {}, sha256: {}}}",
                    writeup["ref"].as_str().unwrap_or_default(),
                    writeup["sha256"].as_str().unwrap_or_default()
                )
            },
        ));
    }
    match (&verdict, action) {
        (None, DecisionAction::Failed) => {}
        (None, _) => {
            return Err(invalid(
                "/document",
                "front matter /outcome: a unit stopped after a failure is decided failed",
            ));
        }
        (Some(_), DecisionAction::Failed) => {
            return Err(invalid(
                "/document",
                "front matter /outcome: failed is the outcome of a unit stopped after a failure",
            ));
        }
        (Some(verdict), DecisionAction::Promote) if verdict != "pass" => {
            return Err(invalid(
                "/document",
                format!(
                    "front matter /outcome: a decider promotes only on a pass verdict; this verdict is {verdict}"
                ),
            ));
        }
        _ => {}
    }
    let spec = flow::value(&j.spec, s, r)?;
    let revision = spec["decider"]["revision"]
        .as_str()
        .ok_or_else(|| internal(r, "decide job revision"))?;
    let json =
        serde_json::to_string(&front_matter).map_err(|_| internal(r, "decision encoding"))?;
    let sha256 = format!(
        "{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(text.as_bytes())
    );
    let decision = units::record_automatic_decision(
        c,
        units::RecordAutomaticDecision {
            case_id: case.id,
            action,
            subject_revision: &BigInt::from(case.subject_revision),
            reason: &reason,
            decider: decider.service_account_id,
            revision,
            via_channel: decider.via.channel,
            via_client: decider.via.client.as_deref(),
            document: (&json, &sha256),
        },
        s.units,
    )
    .await
    .map_err(|_| internal(r, "automatic decision"))?;
    let decision_id = decision.id.0.to_string();
    let record = |action: &'static str,
                  subject_type: &'static str,
                  subject: String,
                  prior: Value,
                  new: Value| (action, subject_type, subject, prior, new);
    let events = [
        record(
            match action {
                DecisionAction::Promote => "review.promote",
                DecisionAction::Reject => "review.reject",
                DecisionAction::Inconclusive => "review.inconclusive",
                _ => "review.failed",
            },
            "review_case",
            case.id.to_string(),
            json!({"state":case.state.as_str(),"kind":"decision","decision_id":null}),
            json!({"state":"resolved","decision_id":decision_id,"action":action.as_str(),
                "subject_revision":case.subject_revision,"verdict":verdict,
                "decision_sha256":sha256,"supersedes":null,
                "decider":{"id":decider.name,"revision":revision}}),
        ),
        record(
            "unit.decided",
            "unit",
            a.unit_id.to_string(),
            json!({"state":"deciding"}),
            json!({"state":to.as_str(),"attempt_id":a.id.to_string(),"decision_id":decision_id}),
        ),
    ];
    units::set_state(c, a.unit_id, to, None)
        .await
        .map_err(|_| internal(r, "decided unit transition"))?;
    jobs::complete_decide_job(c, j.id, jobs::DecisionId(decision.id.0), s.jobs)
        .await
        .map_err(|_| internal(r, "decide job completion"))?;
    for (action, subject_type, subject, prior, new) in events {
        audit::record(
            c,
            Attribution::Principal(p),
            Record {
                action,
                subject_type,
                subject_id: &subject,
                project_id: Some(a.project_id),
                prior_state: Some(&prior),
                new_state: Some(&new),
                reason: Some(&reason),
                idempotency_key: key,
            },
        )
        .await
        .map_err(|_| internal(r, "decision audit"))?;
    }
    audit::record(
        c,
        Attribution::Principal(p),
        Record {
            action: "job.completed",
            subject_type: "job",
            subject_id: &j.id.to_string(),
            project_id: Some(a.project_id),
            prior_state: Some(&json!({"state":"claimed"})),
            new_state: Some(&json!({"state":"completed","decision_id":decision_id,
                "decision_sha256":sha256})),
            reason: None,
            idempotency_key: key,
        },
    )
    .await
    .map_err(|_| internal(r, "decide job audit"))?;
    Ok(())
}

/// A claimed decide job failed or its lease expired: it fails and, within
/// the science revision's `max_auto_retries`, runs again; after that the
/// decision case is left to researchers. Returns the rerun, if any.
pub(crate) async fn fail(
    c: &mut PgConnection,
    actor: Attribution<'_>,
    a: &Attempt,
    j: &Job,
    failure: jobs::Failure<'_>,
    s: &flow::Context,
    r: &RequestContext,
) -> Result<Option<Job>, Failure> {
    if j.state != jobs::State::Claimed || j.phase != Phase::Decide {
        return Err(domain(
            ErrorCode::StaleLease,
            "the job is no longer claimed",
        ));
    }
    let code = failure.code;
    let reason = failure.reason;
    let step = failure.step;
    jobs::fail_job(c, j.id, failure, s.jobs)
        .await
        .map_err(|_| internal(r, "decide job failure"))?;
    flow::event_as(
        c,
        actor,
        a,
        "job.failed",
        "job",
        &j.id.to_string(),
        Some(&json!({"state":j.state.as_str()})),
        &json!({"state":"failed","code":code,"step":step,"run_number":j.run_number}),
        Some(reason),
        r,
    )
    .await?;
    let raw = flow::science(c, a, s, r).await?;
    let budget = raw
        .content
        .field(raw.content.root(), "max_auto_retries")
        .map(|id| cannery_research::science::configuration_integer(&raw.content, id))
        .transpose()
        .map_err(|_| internal(r, "decide rerun budget"))?
        .unwrap_or_else(|| 1.into());
    let used = jobs::automatic_reruns(c, a.id, Phase::Decide)
        .await
        .map_err(|_| internal(r, "decide rerun count"))?;
    let still_deciding =
        sqlx::query_scalar!("SELECT state FROM units WHERE id=$1", a.unit_id.0 as _)
            .fetch_one(&mut *c)
            .await
            .map_err(|_| internal(r, "decide unit state"))?
            == "deciding";
    if BigInt::from(used) >= budget || !still_deciding {
        return Ok(None);
    }
    let case = case_id(j, s, r)?;
    create(c, actor, a, case, jobs::Origin::AutoRetry, Some(j.id), s, r).await
}
