// SPDX-License-Identifier: AGPL-3.0-only
//! Document jobs: one per unit, queued on its last attempt when that
//! attempt is verified or a researcher stops the unit after a failure.
//! An agent service account or a researcher writes it up; a researcher may
//! skip it with a reason. Either moves the unit from `documenting` to
//! `deciding` and opens its decision case. A failed or expired document job
//! is queued again: nothing skips it automatically.
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
    model::{Attempt, State as AttemptState},
    repo::{AddEvidence, Repository},
};
use cannery_core::{
    audit::{self, Attribution, Record},
    contracts::phases::{Phase as DocumentPhase, PhaseDocumentError, PhaseSchemas},
    errors::ErrorCode,
    front_matter::{FrontMatterError, Limits},
    ids::{JobId, ReviewCaseId},
    principal::Principal,
};
use cannery_jobs::repo::{self as jobs, Job, Performer, Phase};
use cannery_units::repo::{self as units, UnitState};
use num_bigint::BigInt;
use serde_json::{Value, json};
use sqlx::PgConnection;

/// The time a document job allows beyond the science's `max_deadline_seconds`.
pub(crate) const OVERHEAD_SECONDS: i64 = 300;

pub(crate) fn output_prefix(a: &Attempt, id: JobId) -> String {
    format!(
        "projects/{}/attempts/{}/document-runs/{}/",
        a.project_id, a.id, id
    )
}

/// Queue a document job on the unit's last attempt. The caller holds
/// the attempt lock and has moved the unit to `documenting`.
pub(crate) async fn create(
    c: &mut PgConnection,
    actor: Attribution<'_>,
    a: &Attempt,
    origin: jobs::Origin,
    previous: Option<JobId>,
    s: &flow::Context,
    r: &RequestContext,
) -> Result<Job, Failure> {
    let raw = flow::science(c, a, s, r).await?;
    let registered = flow::value(&raw.content, s, r)?;
    let allowance = registered["limits"]
        .get("max_deadline_seconds")
        .and_then(Value::as_i64)
        .unwrap_or(3600);
    let id = JobId(uuid::Uuid::new_v4());
    let spec = flow::document(
        &json!({"performer":"agent","track":a.track_slug,"unit":a.unit_number,
            "output_prefix":output_prefix(a, id)}),
        s,
        r,
    )?;
    let job = jobs::create_job(
        c,
        jobs::NewJob {
            id,
            project_id: a.project_id,
            attempt_id: a.id,
            phase: Phase::Document,
            performer: Performer::Agent,
            science_revision: &BigInt::from(a.science_revision),
            verifier_id: None,
            spec: &spec,
            deadline_seconds: &BigInt::from(OVERHEAD_SECONDS + allowance),
            origin,
            previous_run_id: previous,
        },
        s.jobs,
    )
    .await
    .map_err(|_| internal(r, "document job creation"))?;
    flow::record_job_created_as(c, actor, a, &job, s, r).await?;
    Ok(job)
}

/// The verification report a write-up and a decision cite.
#[derive(Clone)]
pub(crate) struct Cited {
    pub id: uuid::Uuid,
    pub sha256: String,
    pub revision: i32,
}

/// What a unit's write-up covers and cites: the sequence numbers of
/// every attempt, and the last attempt's verification report, absent when
/// the unit was stopped after a failure.
pub(crate) struct Inputs {
    pub attempts: Vec<i64>,
    pub verification: Option<Cited>,
}

impl Inputs {
    /// The `verification` a write-up and a decision document cite.
    pub(crate) fn cited(&self) -> Value {
        self.verification.as_ref().map_or(
            Value::Null,
            |cited| json!({"ref": cited.id.to_string(), "sha256": cited.sha256}),
        )
    }
}

/// The inputs of the document job on attempt `a`, the unit's last.
pub(crate) async fn inputs(
    c: &mut PgConnection,
    a: &Attempt,
    r: &RequestContext,
) -> Result<Inputs, Failure> {
    let attempts = sqlx::query_scalar!(
        "SELECT sequence FROM attempts WHERE unit_id=$1 ORDER BY sequence",
        a.unit_id.0 as _
    )
    .fetch_all(&mut *c)
    .await
    .map_err(|_| internal(r, "document job attempts"))?
    .into_iter()
    .map(i64::from)
    .collect();
    let verification = if a.state == AttemptState::Verified {
        sqlx::query!(
            "SELECT id AS \"id: uuid::Uuid\", sha256 AS \"sha256!\", revision FROM phase_outputs WHERE attempt_id=$1 AND stage='verification' AND status='completed' ORDER BY revision DESC LIMIT 1",
            a.id.0 as _
        )
        .fetch_optional(&mut *c)
        .await
        .map_err(|_| internal(r, "document job verification"))?
        .map(|row| Cited {
            id: row.id,
            sha256: row.sha256,
            revision: row.revision,
        })
    } else {
        None
    };
    Ok(Inputs {
        attempts,
        verification,
    })
}

/// A checked write-up: its front matter, body and the digest of its text.
pub(crate) struct Writeup {
    pub front_matter: Value,
    pub body: String,
    pub sha256: String,
}

/// Refusal details of a phase document, reported at `path`.
pub(crate) fn document_error(error: &PhaseDocumentError, path: &str, what: &str) -> Failure {
    match error {
        PhaseDocumentError::FrontMatter(FrontMatterError::TooLarge) => {
            invalid(path, format!("the {what} exceeds 1 MiB"))
        }
        PhaseDocumentError::FrontMatter(error) => invalid(path, error.to_string()),
        PhaseDocumentError::Invalid(violations) => {
            let message = violations
                .iter()
                .map(|violation| {
                    let pointer = if violation.path.is_empty() {
                        "/"
                    } else {
                        violation.path.as_str()
                    };
                    format!("front matter {pointer}: {}", violation.message)
                })
                .collect::<Vec<_>>()
                .join("; ");
            invalid(path, message)
        }
    }
}

/// Check a write-up against `writeup.schema.json` and the job's inputs: it
/// covers every attempt, cites the verification report (null for a stopped
/// unit) and has a body.
pub(crate) fn check(
    text: &str,
    inputs: &Inputs,
    phases: &PhaseSchemas,
    path: &str,
) -> Result<Writeup, Failure> {
    let parsed = phases
        .parse(DocumentPhase::Writeup, text, Limits::default())
        .map_err(|error| document_error(&error, path, "write-up"))?;
    let front_matter = Value::Object(parsed.front_matter);
    if front_matter.get("summary").is_none() {
        return Err(invalid(
            path,
            "front matter: a write-up states its summary, the attempts it covers and the verification report it cites",
        ));
    }
    let mut attempts: Vec<i64> = front_matter["attempts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_i64)
        .collect();
    attempts.sort_unstable();
    if attempts != inputs.attempts {
        let expected = inputs
            .attempts
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(invalid(
            path,
            format!(
                "front matter /attempts: the write-up covers every attempt of the unit: [{expected}]"
            ),
        ));
    }
    if front_matter["verification"] != inputs.cited() {
        return Err(invalid(
            path,
            match &inputs.verification {
                Some(cited) => format!(
                    "front matter /verification: cite the verification report {{ref: {}, sha256: {}}}",
                    cited.id, cited.sha256
                ),
                None => String::from(
                    "front matter /verification: null; the unit was stopped after a failure",
                ),
            },
        ));
    }
    if parsed.body.trim().is_empty() {
        return Err(invalid(
            path,
            "the write-up's body is empty: write what was tried, what was found and what it means",
        ));
    }
    Ok(Writeup {
        front_matter,
        body: parsed.body,
        sha256: format!(
            "{:x}",
            <sha2::Sha256 as sha2::Digest>::digest(text.as_bytes())
        ),
    })
}

/// The unit of attempt `a`, locked, must wait for its write-up.
async fn require_documenting(
    c: &mut PgConnection,
    a: &Attempt,
    r: &RequestContext,
) -> Result<(), Failure> {
    let state = sqlx::query_scalar!(
        "SELECT state FROM units WHERE id=$1 FOR UPDATE",
        a.unit_id.0 as _
    )
    .fetch_one(&mut *c)
    .await
    .map_err(|_| internal(r, "document unit lock"))?;
    if state != "documenting" {
        return Err(domain(
            ErrorCode::Conflict,
            format!(
                "unit #{} is {state}; it no longer waits for a write-up",
                a.unit_number
            ),
        ));
    }
    Ok(())
}

/// Move the unit to `deciding` and open its decision case.
async fn deciding(
    c: &mut PgConnection,
    actor: Attribution<'_>,
    a: &Attempt,
    j: &Job,
    inputs: &Inputs,
    writeup: Option<uuid::Uuid>,
    reason: Option<&str>,
    key: Option<&str>,
    s: &flow::Context,
    r: &RequestContext,
) -> Result<ReviewCaseId, Failure> {
    units::set_state(c, a.unit_id, UnitState::Deciding, None)
        .await
        .map_err(|_| internal(r, "deciding unit transition"))?;
    let case = cannery_reviews::repo::open_decision_case(
        c,
        cannery_reviews::repo::OpenDecisionCase {
            project_id: a.project_id,
            unit_id: a.unit_id,
            attempt_id: a.id,
            evidence_id: inputs
                .verification
                .as_ref()
                .map(|cited| cannery_reviews::EvidenceId(cited.id)),
            writeup_id: writeup.map(cannery_reviews::EvidenceId),
            subject_revision: &BigInt::from(
                inputs
                    .verification
                    .as_ref()
                    .map_or(1, |cited| cited.revision),
            ),
        },
    )
    .await
    .map_err(|_| internal(r, "decision case"))?;
    audit::record(
        c,
        actor,
        Record {
            action: "unit.deciding",
            subject_type: "unit",
            subject_id: &a.unit_id.to_string(),
            project_id: Some(a.project_id),
            prior_state: Some(&json!({"state":"documenting"})),
            new_state: Some(
                &json!({"state":"deciding","review_case_id":case.to_string(),
                "job_id":j.id.to_string(),"writeup_id":writeup.map(|id| id.to_string())}),
            ),
            reason,
            idempotency_key: key,
        },
    )
    .await
    .map_err(|_| internal(r, "deciding unit audit"))?;
    // A registered decider step decides it; otherwise a researcher does.
    crate::decide_jobs::create(c, actor, a, case, jobs::Origin::Submission, None, s, r).await?;
    Ok(case)
}

/// Publish the write-up of claimed document job `j`: the unit then
/// awaits its decision. The caller holds the job's lease and the attempt lock.
pub(crate) async fn publish(
    c: &mut PgConnection,
    p: &Principal,
    a: &Attempt,
    j: &Job,
    inputs: &Inputs,
    writeup: &Writeup,
    key: Option<&str>,
    s: &flow::Context,
    r: &RequestContext,
) -> Result<ReviewCaseId, Failure> {
    require_documenting(c, a, r).await?;
    let content = flow::document(&writeup.front_matter, s, r)?;
    let id = Repository::new(c, s.attempts)
        .add_evidence(AddEvidence {
            project_id: a.project_id,
            attempt_id: a.id,
            stage: "writeup",
            status: "completed",
            content: &content,
            body: &writeup.body,
            sha256: &writeup.sha256,
            manifest_id: None,
            principal: p,
        })
        .await
        .map_err(|_| internal(r, "write-up publication"))?;
    jobs::complete_job(c, j.id, jobs::EvidenceId(id.0), None, s.jobs)
        .await
        .map_err(|_| internal(r, "document job completion"))?;
    audit::record(
        c,
        Attribution::Principal(p),
        Record {
            action: "job.completed",
            subject_type: "job",
            subject_id: &j.id.to_string(),
            project_id: Some(a.project_id),
            prior_state: Some(&json!({"state":"claimed"})),
            new_state: Some(&json!({"state":"completed","writeup_id":id.0.to_string(),
                "writeup_sha256":writeup.sha256})),
            reason: None,
            idempotency_key: key,
        },
    )
    .await
    .map_err(|_| internal(r, "document job audit"))?;
    deciding(
        c,
        Attribution::Principal(p),
        a,
        j,
        inputs,
        Some(id.0),
        writeup.front_matter["summary"].as_str(),
        key,
        s,
        r,
    )
    .await
}

/// A researcher skips document job `j`, pending or claimed, with a reason:
/// the unit then awaits its decision without a write-up.
pub(crate) async fn skip(
    c: &mut PgConnection,
    p: &Principal,
    a: &Attempt,
    j: &Job,
    inputs: &Inputs,
    reason: &str,
    key: Option<&str>,
    s: &flow::Context,
    r: &RequestContext,
) -> Result<(Job, ReviewCaseId), Failure> {
    require_documenting(c, a, r).await?;
    let skipped = jobs::skip_job(c, j.id, p, reason, s.jobs)
        .await
        .map_err(|_| {
            domain(
                ErrorCode::Conflict,
                "the document job is no longer waiting or claimed",
            )
        })?;
    audit::record(
        c,
        Attribution::Principal(p),
        Record {
            action: "job.skipped",
            subject_type: "job",
            subject_id: &j.id.to_string(),
            project_id: Some(a.project_id),
            prior_state: Some(&json!({"state":j.state.as_str()})),
            new_state: Some(&json!({"state":"skipped"})),
            reason: Some(reason),
            idempotency_key: key,
        },
    )
    .await
    .map_err(|_| internal(r, "document job skip audit"))?;
    let case = deciding(
        c,
        Attribution::Principal(p),
        a,
        j,
        inputs,
        None,
        Some(reason),
        key,
        s,
        r,
    )
    .await?;
    Ok((skipped, case))
}

/// A claimed document job failed or its lease expired: it fails, and the
/// same write-up is queued again, since nothing skips it but a researcher.
pub(crate) async fn requeue(
    c: &mut PgConnection,
    actor: Attribution<'_>,
    a: &Attempt,
    j: &Job,
    failure: jobs::Failure<'_>,
    s: &flow::Context,
    r: &RequestContext,
) -> Result<(Job, Job), Failure> {
    if j.state != jobs::State::Claimed || j.phase != Phase::Document {
        return Err(domain(
            ErrorCode::StaleLease,
            "the job is no longer claimed",
        ));
    }
    let code = failure.code;
    let reason = failure.reason;
    let step = failure.step;
    let failed = jobs::fail_job(c, j.id, failure, s.jobs)
        .await
        .map_err(|_| internal(r, "document job failure"))?;
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
    let next = create(c, actor, a, jobs::Origin::AutoRetry, Some(j.id), s, r).await?;
    Ok((failed, next))
}

/// A stopped unit is written up too: the researcher's `stop` moves it
/// to `documenting` and queues its document job on the failed attempt.
pub(crate) async fn stop(
    c: &mut PgConnection,
    p: &Principal,
    a: &Attempt,
    s: &flow::Context,
    r: &RequestContext,
) -> Result<Job, Failure> {
    units::set_state(c, a.unit_id, UnitState::Documenting, None)
        .await
        .map_err(|_| internal(r, "stopped unit transition"))?;
    let previous = jobs::latest_job(c, a.id, Phase::Document, s.jobs)
        .await
        .map_err(|_| internal(r, "stopped unit document job"))?;
    create(
        c,
        Attribution::Principal(p),
        a,
        if previous.is_some() {
            jobs::Origin::HumanRetry
        } else {
            jobs::Origin::Submission
        },
        previous.map(|job| job.id),
        s,
        r,
    )
    .await
}
