//! Read-only attention queries; transaction and authorization policy belong to callers.
#![forbid(unsafe_code)]
use cannery_core::{
    ids::{ProjectId, ReviewCaseId},
    timestamps::Timestamp,
};
use cannery_reviews::{
    Action, AttemptState, CaseKind, Error, HypothesisState, Origin, Stage, binding::Integer,
};
use num_bigint::BigInt;
use sqlx::PgConnection;
use std::collections::BTreeMap;
pub struct PendingReview {
    pub case_id: ReviewCaseId,
    pub kind: CaseKind,
    pub subject_revision: i32,
    pub opened_at: Timestamp,
    pub hypothesis_number: i32,
    pub hypothesis_title: String,
    pub track_slug: String,
    pub attempt_sequence: Option<i32>,
    pub verdict: Option<String>,
    pub failure_stage: Option<Stage>,
    pub failure_code: Option<String>,
    pub failure_reason: Option<String>,
    pub origin: Origin,
}
impl std::fmt::Debug for PendingReview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PendingReview([redacted])")
    }
}
struct RawPendingReview {
    case_id: ReviewCaseId,
    kind: String,
    subject_revision: i32,
    opened_at: Timestamp,
    hypothesis_number: i32,
    hypothesis_title: String,
    track_slug: String,
    attempt_sequence: Option<i32>,
    verdict: Option<String>,
    failure_stage: Option<String>,
    failure_code: Option<String>,
    failure_reason: Option<String>,
    origin: String,
}
fn decode_pendingreview(r: &RawPendingReview) -> Result<PendingReview, Error> {
    Ok(PendingReview {
        case_id: r.case_id,
        kind: CaseKind::try_from(r.kind.as_str())?,
        subject_revision: r.subject_revision,
        opened_at: r.opened_at,
        hypothesis_number: r.hypothesis_number,
        hypothesis_title: String::from(&r.hypothesis_title),
        track_slug: String::from(&r.track_slug),
        attempt_sequence: r.attempt_sequence,
        verdict: r.verdict.as_deref().map(String::from),
        failure_stage: r
            .failure_stage
            .as_deref()
            .map(Stage::try_from)
            .transpose()?,
        failure_code: r.failure_code.as_deref().map(String::from),
        failure_reason: r.failure_reason.as_deref().map(String::from),
        origin: Origin::try_from(r.origin.as_str())?,
    })
}
pub struct RunningAttempt {
    pub hypothesis_number: i32,
    pub hypothesis_title: String,
    pub track_slug: String,
    pub attempt_sequence: i32,
    pub state: AttemptState,
    pub claimed_at: Timestamp,
}
impl std::fmt::Debug for RunningAttempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RunningAttempt([redacted])")
    }
}
struct RawRunningAttempt {
    hypothesis_number: i32,
    hypothesis_title: String,
    track_slug: String,
    attempt_sequence: i32,
    state: String,
    claimed_at: Timestamp,
}
fn decode_runningattempt(r: &RawRunningAttempt) -> Result<RunningAttempt, Error> {
    Ok(RunningAttempt {
        hypothesis_number: r.hypothesis_number,
        hypothesis_title: String::from(&r.hypothesis_title),
        track_slug: String::from(&r.track_slug),
        attempt_sequence: r.attempt_sequence,
        state: AttemptState::try_from(r.state.as_str())?,
        claimed_at: r.claimed_at,
    })
}
pub struct Outcome {
    pub hypothesis_number: i32,
    pub hypothesis_title: String,
    pub hypothesis_state: HypothesisState,
    pub track_slug: String,
    pub attempt_sequence: Option<i32>,
    pub action: Action,
    pub reason: String,
    pub decided_at: Timestamp,
    pub origin: Origin,
}
impl std::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Outcome([redacted])")
    }
}
struct RawOutcome {
    hypothesis_number: i32,
    hypothesis_title: String,
    hypothesis_state: String,
    track_slug: String,
    attempt_sequence: Option<i32>,
    action: String,
    reason: String,
    decided_at: Timestamp,
    origin: String,
}
fn decode_outcome(r: &RawOutcome) -> Result<Outcome, Error> {
    Ok(Outcome {
        hypothesis_number: r.hypothesis_number,
        hypothesis_title: String::from(&r.hypothesis_title),
        hypothesis_state: HypothesisState::try_from(r.hypothesis_state.as_str())?,
        track_slug: String::from(&r.track_slug),
        attempt_sequence: r.attempt_sequence,
        action: Action::try_from(r.action.as_str())?,
        reason: String::from(&r.reason),
        decided_at: r.decided_at,
        origin: Origin::try_from(r.origin.as_str())?,
    })
}
pub struct RecentFailure {
    pub hypothesis_number: i32,
    pub hypothesis_title: String,
    pub hypothesis_state: HypothesisState,
    pub track_slug: String,
    pub attempt_sequence: i32,
    pub stage: Stage,
    pub code: String,
    pub reason: String,
    pub created_at: Timestamp,
    pub origin: Origin,
}
impl std::fmt::Debug for RecentFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecentFailure([redacted])")
    }
}
struct RawRecentFailure {
    hypothesis_number: i32,
    hypothesis_title: String,
    hypothesis_state: String,
    track_slug: String,
    attempt_sequence: i32,
    stage: String,
    code: String,
    reason: String,
    created_at: Timestamp,
    origin: String,
}
fn decode_recentfailure(r: &RawRecentFailure) -> Result<RecentFailure, Error> {
    Ok(RecentFailure {
        hypothesis_number: r.hypothesis_number,
        hypothesis_title: String::from(&r.hypothesis_title),
        hypothesis_state: HypothesisState::try_from(r.hypothesis_state.as_str())?,
        track_slug: String::from(&r.track_slug),
        attempt_sequence: r.attempt_sequence,
        stage: Stage::try_from(r.stage.as_str())?,
        code: String::from(&r.code),
        reason: String::from(&r.reason),
        created_at: r.created_at,
        origin: Origin::try_from(r.origin.as_str())?,
    })
}
pub struct StalledEvaluation {
    pub hypothesis_number: i32,
    pub hypothesis_title: String,
    pub track_slug: String,
    pub attempt_sequence: i32,
    pub evaluator: Option<String>,
    pub revision: Option<String>,
    pub waiting_since: Timestamp,
}
impl std::fmt::Debug for StalledEvaluation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StalledEvaluation([redacted])")
    }
}
struct RawStalledEvaluation {
    hypothesis_number: i32,
    hypothesis_title: String,
    track_slug: String,
    attempt_sequence: i32,
    evaluator: Option<String>,
    revision: Option<String>,
    waiting_since: Timestamp,
}
fn decode_stalledevaluation(r: &RawStalledEvaluation) -> StalledEvaluation {
    StalledEvaluation {
        hypothesis_number: r.hypothesis_number,
        hypothesis_title: String::from(&r.hypothesis_title),
        track_slug: String::from(&r.track_slug),
        attempt_sequence: r.attempt_sequence,
        evaluator: r.evaluator.as_deref().map(String::from),
        revision: r.revision.as_deref().map(String::from),
        waiting_since: r.waiting_since,
    }
}
/// Execute the source SQL on the caller-owned connection.
/// # Errors
/// Returns sanitized driver, server, JSON decoding or invariant failures.
pub async fn pending_reviews(
    c: &mut PgConnection,
    project: ProjectId,
    limit: Option<&BigInt>,
) -> Result<Vec<PendingReview>, Error> {
    let limit = limit.map(Integer::new).transpose()?;
    let rows=sqlx::query_as!(RawPendingReview,"\n            SELECT c.id AS \"case_id!: _\", c.kind AS \"kind!\", c.subject_revision AS \"subject_revision!\", c.opened_at AS \"opened_at!: _\", h.number AS \"hypothesis_number!\", h.title AS \"hypothesis_title!\", t.slug AS \"track_slug!\", a.sequence AS \"attempt_sequence?\", e.front_matter -> 'assessment' ->> 'verdict' AS \"verdict?\", f.stage AS \"failure_stage?\", f.code AS \"failure_code?\", f.reason AS \"failure_reason?\", c.origin AS \"origin!\" FROM review_cases c\n            JOIN hypotheses h ON h.id = c.hypothesis_id\n            JOIN tracks t ON t.id = h.track_id\n            LEFT JOIN attempts a ON a.id = c.attempt_id\n            LEFT JOIN phase_outputs e ON e.id = c.evidence_id\n            LEFT JOIN attempt_failures f ON f.id = c.failure_id\n            WHERE c.project_id = $1 AND c.state = 'pending'\n            ORDER BY c.opened_at, c.id\n            LIMIT $2\n            ",project as ProjectId,limit as _).fetch_all(&mut *c).await?;
    rows.iter().map(decode_pendingreview).collect()
}
/// Execute the source SQL on the caller-owned connection.
/// # Errors
/// Returns sanitized driver, server, JSON decoding or invariant failures.
pub async fn recent_outcomes(
    c: &mut PgConnection,
    project: ProjectId,
    limit: Option<&BigInt>,
) -> Result<Vec<Outcome>, Error> {
    let limit = limit.map(Integer::new).transpose()?;
    let rows=sqlx::query_as!(RawOutcome,"\n            SELECT h.number AS \"hypothesis_number!\", h.title AS \"hypothesis_title!\", h.state AS \"hypothesis_state!\", t.slug AS \"track_slug!\", a.sequence AS \"attempt_sequence?\", d.action AS \"action!\", d.reason AS \"reason!\", d.decided_at AS \"decided_at!: _\", d.origin AS \"origin!\" FROM decisions d\n            JOIN review_cases c ON c.id = d.review_case_id\n            JOIN hypotheses h ON h.id = c.hypothesis_id\n            JOIN tracks t ON t.id = h.track_id\n            LEFT JOIN attempts a ON a.id = c.attempt_id\n            WHERE c.project_id = $1 AND c.kind = 'result'\n              AND NOT EXISTS (SELECT 1 FROM decisions s WHERE s.supersedes = d.id)\n            ORDER BY d.decided_at DESC, d.id DESC\n            LIMIT $2\n            ",project as ProjectId,limit as _).fetch_all(&mut *c).await?;
    rows.iter().map(decode_outcome).collect()
}
/// Execute the source SQL on the caller-owned connection.
/// # Errors
/// Returns sanitized driver, server, JSON decoding or invariant failures.
pub async fn recent_failures(
    c: &mut PgConnection,
    project: ProjectId,
    limit: Option<&BigInt>,
) -> Result<Vec<RecentFailure>, Error> {
    let limit = limit.map(Integer::new).transpose()?;
    let rows=sqlx::query_as!(RawRecentFailure,"\n            SELECT h.number AS \"hypothesis_number!\", h.title AS \"hypothesis_title!\", h.state AS \"hypothesis_state!\", t.slug AS \"track_slug!\", a.sequence AS \"attempt_sequence!\", f.stage AS \"stage!\", f.code AS \"code!\", f.reason AS \"reason!\", f.created_at AS \"created_at!: _\", a.origin AS \"origin!\" FROM attempt_failures f\n            JOIN attempts a ON a.id = f.attempt_id\n            JOIN hypotheses h ON h.id = a.hypothesis_id\n            JOIN tracks t ON t.id = a.track_id\n            WHERE a.project_id = $1\n            ORDER BY f.created_at DESC, f.id DESC\n            LIMIT $2\n            ",project as ProjectId,limit as _).fetch_all(&mut *c).await?;
    rows.iter().map(decode_recentfailure).collect()
}
/// Execute the source SQL on the caller-owned connection.
/// # Errors
/// Returns sanitized driver, server, JSON decoding or invariant failures.
pub async fn pending_counts(
    c: &mut PgConnection,
    project: ProjectId,
) -> Result<BTreeMap<CaseKind, i64>, Error> {
    let rows = sqlx::query!(
        "\n            SELECT kind, count(*) AS \"count!\" FROM review_cases\n            WHERE project_id = $1 AND state = 'pending' AND kind <> 'plan' GROUP BY kind\n            ",
        project as ProjectId
    )
    .fetch_all(&mut *c)
    .await?;
    let mut counts = BTreeMap::from([
        (CaseKind::Draft, 0),
        (CaseKind::Result, 0),
        (CaseKind::Failure, 0),
    ]);
    for row in rows {
        counts.insert(CaseKind::try_from(row.kind.as_str())?, row.count);
    }
    Ok(counts)
}
/// Execute the source SQL on the caller-owned connection.
/// # Errors
/// Returns sanitized driver, server, JSON decoding or invariant failures.
pub async fn running_attempts(
    c: &mut PgConnection,
    project: ProjectId,
    limit: Option<&BigInt>,
) -> Result<(i64, Vec<RunningAttempt>), Error> {
    let states = vec![
        "claimed".to_owned(),
        "running".to_owned(),
        "submitted".to_owned(),
        "testing".to_owned(),
        "evaluating".to_owned(),
    ];
    let total = sqlx::query!(
        "SELECT count(*) AS \"count!\" FROM attempts WHERE project_id = $1 AND state = ANY($2)",
        project as ProjectId,
        &states
    )
    .fetch_optional(&mut *c)
    .await?;
    let total = total.map_or(0, |r| r.count);
    let limit = limit.map(Integer::new).transpose()?;
    let rows=sqlx::query_as!(RawRunningAttempt,"\n            SELECT h.number AS \"hypothesis_number!\", h.title AS \"hypothesis_title!\", t.slug AS \"track_slug!\", a.sequence AS \"attempt_sequence!\", a.state AS \"state!\", a.claimed_at AS \"claimed_at!: _\" FROM attempts a\n            JOIN hypotheses h ON h.id = a.hypothesis_id\n            JOIN tracks t ON t.id = a.track_id\n            WHERE a.project_id = $1 AND a.state = ANY($2)\n            ORDER BY a.claimed_at DESC, a.id DESC\n            LIMIT $3\n            ",project as ProjectId,&states,limit as _).fetch_all(&mut *c).await?;
    Ok((
        total,
        rows.iter()
            .map(decode_runningattempt)
            .collect::<Result<Vec<_>, _>>()?,
    ))
}
/// Execute the source SQL on the caller-owned connection.
/// # Errors
/// Returns sanitized driver, server, JSON decoding or invariant failures.
pub async fn stalled_evaluations(
    c: &mut PgConnection,
    project: ProjectId,
    limit: Option<&BigInt>,
    seconds: Option<&BigInt>,
) -> Result<(i64, Vec<StalledEvaluation>), Error> {
    let seconds = seconds.map(Integer::new).transpose()?;
    let total = sqlx::query!(
        "SELECT count(*) AS \"count!\" FROM jobs j WHERE \n        j.project_id = $1 AND j.stage = 'evaluator' AND j.state = 'pending'\n        AND j.created_at < now() - make_interval(secs => $2)\n    ",
        project as ProjectId,
        seconds as _
    )
    .fetch_optional(&mut *c)
    .await?;
    let total = total.map_or(0, |r| r.count);
    let limit = limit.map(Integer::new).transpose()?;
    let rows=sqlx::query_as!(RawStalledEvaluation,"\n            SELECT h.number AS \"hypothesis_number!\", h.title AS \"hypothesis_title!\", t.slug AS \"track_slug!\", a.sequence AS \"attempt_sequence!\", j.spec -> 'evaluator' ->> 'id' AS \"evaluator?\", j.spec -> 'evaluator' ->> 'revision' AS \"revision?\", j.created_at AS \"waiting_since!: _\" FROM jobs j\n            JOIN attempts a ON a.id = j.attempt_id\n            JOIN hypotheses h ON h.id = a.hypothesis_id\n            JOIN tracks t ON t.id = a.track_id\n            WHERE \n        j.project_id = $1 AND j.stage = 'evaluator' AND j.state = 'pending'\n        AND j.created_at < now() - make_interval(secs => $2)\n    \n            ORDER BY j.created_at, j.id\n            LIMIT $3\n            ",project as ProjectId,seconds as _,limit as _).fetch_all(&mut *c).await?;
    Ok((total, rows.iter().map(decode_stalledevaluation).collect()))
}
