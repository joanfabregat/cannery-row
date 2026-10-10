use crate::{
    AttemptState, CaseKind, CaseState, Error, EvidenceId, FailureId, HypothesisState, JsonContext,
    Origin, Stage, binding::Integer, document, text,
};
use cannery_core::{
    ids::{AttemptId, HypothesisId, ProjectId, ReviewCaseId},
    json::Document,
    timestamps::Timestamp,
};
use num_bigint::BigInt;
use sqlx::PgConnection;
pub struct Case {
    pub id: ReviewCaseId,
    pub project_id: ProjectId,
    pub hypothesis_id: HypothesisId,
    pub hypothesis_number: i32,
    pub hypothesis_state: HypothesisState,
    pub attempt_id: Option<AttemptId>,
    pub attempt_sequence: Option<i32>,
    pub attempt_state: Option<AttemptState>,
    pub kind: CaseKind,
    pub subject_revision: i32,
    pub state: CaseState,
    pub opened_at: Timestamp,
    pub resolved_at: Option<Timestamp>,
    pub failure_id: Option<FailureId>,
    pub evidence_id: Option<EvidenceId>,
    /// The write-up a decision case cites; absent when it was skipped.
    pub writeup_id: Option<EvidenceId>,
    pub origin: Origin,
    pub source_ref: Option<String>,
}
impl std::fmt::Debug for Case {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Case([redacted])")
    }
}
struct RawCase {
    id: ReviewCaseId,
    project_id: ProjectId,
    hypothesis_id: HypothesisId,
    hypothesis_number: i32,
    hypothesis_state: String,
    attempt_id: Option<AttemptId>,
    attempt_sequence: Option<i32>,
    attempt_state: Option<String>,
    kind: String,
    subject_revision: i32,
    state: String,
    opened_at: Timestamp,
    resolved_at: Option<Timestamp>,
    failure_id: Option<FailureId>,
    evidence_id: Option<EvidenceId>,
    writeup_id: Option<EvidenceId>,
    origin: String,
    source_ref: Option<String>,
}
fn decode_case(r: &RawCase) -> Result<Case, Error> {
    Ok(Case {
        id: r.id,
        project_id: r.project_id,
        hypothesis_id: r.hypothesis_id,
        hypothesis_number: r.hypothesis_number,
        hypothesis_state: HypothesisState::try_from(r.hypothesis_state.as_str())?,
        attempt_id: r.attempt_id,
        attempt_sequence: r.attempt_sequence,
        attempt_state: r
            .attempt_state
            .as_deref()
            .map(AttemptState::try_from)
            .transpose()?,
        kind: CaseKind::try_from(r.kind.as_str())?,
        subject_revision: r.subject_revision,
        state: CaseState::try_from(r.state.as_str())?,
        opened_at: r.opened_at,
        resolved_at: r.resolved_at,
        failure_id: r.failure_id,
        evidence_id: r.evidence_id,
        writeup_id: r.writeup_id,
        origin: Origin::try_from(r.origin.as_str())?,
        source_ref: r.source_ref.as_deref().map(String::from),
    })
}
pub struct Failure {
    pub id: FailureId,
    pub attempt_id: AttemptId,
    pub stage: Stage,
    pub code: String,
    pub reason: String,
    pub details: Document,
    pub log_refs: Document,
    pub created_at: Timestamp,
}
impl std::fmt::Debug for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Failure([redacted])")
    }
}
struct RawFailure {
    id: FailureId,
    attempt_id: AttemptId,
    stage: String,
    code: String,
    reason: String,
    details: String,
    log_refs: String,
    created_at: Timestamp,
}
fn decode_failure(r: &RawFailure, json: JsonContext) -> Result<Failure, Error> {
    Ok(Failure {
        id: r.id,
        attempt_id: r.attempt_id,
        stage: Stage::try_from(r.stage.as_str())?,
        code: String::from(&r.code),
        reason: String::from(&r.reason),
        details: document(&r.details, json)?,
        log_refs: document(&r.log_refs, json)?,
        created_at: r.created_at,
    })
}
/// Execute the source SQL on the caller-owned connection.
/// # Errors
/// Returns sanitized driver, server, JSON decoding or invariant failures.
pub async fn get_case(
    c: &mut PgConnection,
    project: ProjectId,
    id: ReviewCaseId,
    lock: bool,
) -> Result<Option<Case>, Error> {
    if lock {
        sqlx::query!(
            "SELECT 1 AS locked FROM review_cases WHERE id = $1 AND project_id = $2 FOR UPDATE",
            id as ReviewCaseId,
            project as ProjectId
        )
        .fetch_optional(&mut *c)
        .await?;
    }
    let row=sqlx::query_as!(RawCase,"SELECT c.id AS \"id!: _\", c.project_id AS \"project_id!: _\", c.hypothesis_id AS \"hypothesis_id!: _\", h.number AS \"hypothesis_number!\", h.state AS \"hypothesis_state!\", c.attempt_id AS \"attempt_id?: _\", a.sequence AS \"attempt_sequence?\", a.state AS \"attempt_state?\", c.kind AS \"kind!\", c.subject_revision AS \"subject_revision!\", c.state AS \"state!\", c.opened_at AS \"opened_at!: _\", c.resolved_at AS \"resolved_at?: _\", c.failure_id AS \"failure_id?: _\", c.evidence_id AS \"evidence_id?: _\", c.writeup_id AS \"writeup_id?: _\", c.origin AS \"origin!\", c.source_ref AS \"source_ref?\" FROM \n    review_cases c JOIN hypotheses h ON h.id = c.hypothesis_id\n    LEFT JOIN attempts a ON a.id = c.attempt_id\n WHERE c.id = $1 AND c.project_id = $2",id as ReviewCaseId,project as ProjectId).fetch_optional(&mut *c).await?;
    row.as_ref().map(decode_case).transpose()
}
pub struct ListCases<'a> {
    pub kind: Option<&'a String>,
    pub state: Option<&'a String>,
    pub before: Option<ReviewCaseId>,
    pub limit: Option<&'a BigInt>,
}
/// Execute the source SQL on the caller-owned connection.
/// # Errors
/// Returns sanitized driver, server, JSON decoding or invariant failures.
pub async fn list_cases(
    c: &mut PgConnection,
    project: ProjectId,
    input: ListCases<'_>,
) -> Result<Vec<Case>, Error> {
    let kind = input.kind.map(text).transpose()?;
    let state = input.state.map(text).transpose()?;
    let before = input.before;
    let limit = input.limit.map(Integer::new).transpose()?;
    let rows=sqlx::query_as!(RawCase,"\n            SELECT c.id AS \"id!: _\", c.project_id AS \"project_id!: _\", c.hypothesis_id AS \"hypothesis_id!: _\", h.number AS \"hypothesis_number!\", h.state AS \"hypothesis_state!\", c.attempt_id AS \"attempt_id?: _\", a.sequence AS \"attempt_sequence?\", a.state AS \"attempt_state?\", c.kind AS \"kind!\", c.subject_revision AS \"subject_revision!\", c.state AS \"state!\", c.opened_at AS \"opened_at!: _\", c.resolved_at AS \"resolved_at?: _\", c.failure_id AS \"failure_id?: _\", c.evidence_id AS \"evidence_id?: _\", c.writeup_id AS \"writeup_id?: _\", c.origin AS \"origin!\", c.source_ref AS \"source_ref?\" FROM \n    review_cases c JOIN hypotheses h ON h.id = c.hypothesis_id\n    LEFT JOIN attempts a ON a.id = c.attempt_id\n\n            WHERE c.project_id = $1\n              AND ($2::text IS NULL OR c.kind = $2)\n              AND ($3::text IS NULL OR c.state = $3)\n              AND ($4::uuid IS NULL OR (c.opened_at, c.id) < (\n                  SELECT opened_at, id FROM review_cases\n                  WHERE id = $4 AND project_id = $1))\n            ORDER BY c.opened_at DESC, c.id DESC\n            LIMIT $5\n            ",project as ProjectId,kind,state,before as Option<ReviewCaseId>,limit as _).fetch_all(&mut *c).await?;
    rows.iter().map(decode_case).collect()
}
/// Execute the source SQL on the caller-owned connection.
/// # Errors
/// Returns sanitized driver, server, JSON decoding or invariant failures.
pub async fn get_failure(
    c: &mut PgConnection,
    id: FailureId,
    json: JsonContext,
) -> Result<Option<Failure>, Error> {
    let row=sqlx::query_as!(RawFailure,"\n            SELECT id AS \"id!: _\", attempt_id AS \"attempt_id!: _\", stage AS \"stage!\", code AS \"code!\", reason AS \"reason!\", details::text AS \"details!\", log_refs::text AS \"log_refs!\", created_at AS \"created_at!: _\" FROM attempt_failures WHERE id = $1\n            ",id as FailureId).fetch_optional(&mut *c).await?;
    row.as_ref().map(|r| decode_failure(r, json)).transpose()
}
/// A hypothesis's decision case, opened once it is written up or its
/// write-up is skipped: it cites the verification report decided on (none
/// for a stopped hypothesis, whose revision is then 1) and the write-up
/// (none when it was skipped).
pub struct OpenDecisionCase<'a> {
    pub project_id: ProjectId,
    pub hypothesis_id: HypothesisId,
    pub attempt_id: AttemptId,
    pub evidence_id: Option<EvidenceId>,
    pub writeup_id: Option<EvidenceId>,
    pub subject_revision: &'a BigInt,
}
/// Execute the source SQL on the caller-owned connection.
/// # Errors
/// Returns sanitized driver, server, JSON decoding or invariant failures.
pub async fn open_decision_case(
    c: &mut PgConnection,
    input: OpenDecisionCase<'_>,
) -> Result<ReviewCaseId, Error> {
    let revision = Integer::new(input.subject_revision)?;
    let row: Option<ReviewCaseId> = sqlx::query_scalar!(
        "\n            INSERT INTO review_cases (project_id, hypothesis_id, attempt_id, kind,\n                                      subject_revision, evidence_id, writeup_id)\n            VALUES ($1, $2, $3, 'decision', $4, $5, $6)\n            RETURNING id AS \"id!: _\"\n            ",
        input.project_id as ProjectId,
        input.hypothesis_id as HypothesisId,
        input.attempt_id as AttemptId,
        revision as _,
        input.evidence_id as Option<EvidenceId>,
        input.writeup_id as Option<EvidenceId>
    )
    .fetch_optional(&mut *c)
    .await?;
    row.ok_or(Error::Invariant)
}
