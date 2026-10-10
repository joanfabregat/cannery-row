//! Human review decisions; operation contexts and connection history stay explicit.
use crate::{
    AppState,
    authentication::authenticate,
    errors::ApiError,
    requests::RequestContext,
    review_attention_routes::{self, ReviewAttentionContext},
    review_attention_wire,
};
use axum::{
    Router,
    extract::{FromRequestParts, Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use cannery_attempts::{
    model::{Attempt, EvidenceId, StoredJson},
    repo::Repository,
};
use cannery_core::{
    audit::{self, Attribution, Record},
    contracts::{
        ContractKind, ContractValidator,
        phases::{Phase, PhaseSchemas},
    },
    errors::{DomainError, ErrorCode},
    ids::{ProjectId, ReviewCaseId},
    json::{Document, Node},
    principal::{Principal, Role, UserPrincipal},
};
use cannery_jobs::repo as jobs;
use cannery_reviews::{CaseKind, CaseState, Stage, repo};
use cannery_units::repo::{
    self as units, Decision, DecisionAction, DecisionId, RecordDecision, UnitState,
};
use num_bigint::BigInt;
use sqlx::{Acquire, PgConnection};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// Every source frontend/repository profile and bounded preparation factory is caller supplied.
pub struct ReviewDecisionContext {
    pub reads: ReviewAttentionContext,
    pub contracts: ContractValidator,
    pub validation_walk_budget: usize,
    pub repr_budget: usize,

    pub request_hash_budget: usize,
    pub config: cannery_research::config_repo::JsonContext,
    pub science_rendering: cannery_research::science::RenderingContext,
    pub jobs: jobs::JsonContext,
    pub audit_encoding_budget: usize,
    /// Builds the fresh verify job of a retried verification.
    pub lifecycle: Arc<crate::job_lifecycle::Context>,
    /// Checks decision documents against `decision.schema.json`.
    pub phases: PhaseSchemas,
}

#[cfg(test)]
mod tests {
    use super::{domain, rollback_after_error};
    use axum::{body::to_bytes, response::IntoResponse};
    use cannery_core::{db::DatabaseOptions, errors::ErrorCode};
    use sqlx::{Acquire, Connection, PgConnection, postgres::PgPoolOptions};

    #[tokio::test]
    #[ignore = "requires positively selected artifact and guarded fresh PostgreSQL child"]
    async fn cleanup_failure_retains_original_error() -> Result<(), Box<dyn std::error::Error>> {
        let url = std::env::var("CANNERY_REVIEW_DECISIONS_HTTP_DATABASE_URL")?;
        let options = DatabaseOptions::parse(&url)?;
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options.connect_options().clone())
            .await?;
        let mut monitor = PgConnection::connect_with(options.connect_options()).await?;
        let name: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&mut monitor)
            .await?;
        let suffix = name
            .strip_prefix("conformance_")
            .ok_or("owned child required")?;
        if suffix.len() != 24 || !suffix.bytes().all(|v| v.is_ascii_hexdigit()) {
            return Err("owned child required".into());
        }
        let source: serde_json::Value = serde_json::from_str(runtime_reference!(
            "/tests/fixtures/review_decisions_http/reference.json"
        ))?;
        assert_eq!(
            source["cleanup_boundary"]
                .as_array()
                .ok_or("cleanup source")?
                .len(),
            2
        );
        for (code, expected) in [ErrorCode::Conflict, ErrorCode::ValidationFailed]
            .into_iter()
            .zip(
                source["cleanup_boundary"]
                    .as_array()
                    .ok_or("cleanup source")?,
            )
        {
            let mut connection = pool.acquire().await?;
            let mut tx = connection.begin().await?;
            let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(&mut *tx)
                .await?;
            let terminated: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
                .bind(pid)
                .fetch_one(&mut monitor)
                .await?;
            assert!(terminated);
            let (original, failed) =
                rollback_after_error(tx, domain(code, "fixture semantic error")).await;
            assert!(failed);
            connection.close_on_drop();
            drop(connection);
            let response = original.into_response();
            assert_eq!(u64::from(response.status().as_u16()), expected["status"]);
            let body: serde_json::Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 4096).await?)?;
            assert_eq!(body["error"]["code"], expected["code"]);
            assert_eq!(body["error"]["message"], expected["message"]);
            assert_eq!(expected["same_exception"], true);
            let replacement_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(&pool)
                .await?;
            assert_ne!(replacement_pid, pid);
        }
        pool.close().await;
        monitor.close().await?;
        Ok(())
    }
}

/// Compute the source decision envelope digest with an explicit rendering budget.
///
/// # Errors
/// Returns an error when lossless JSON rendering exceeds the supplied profile.
#[doc(hidden)]
pub fn reference_request_hash(
    project: &str,
    decision: &Document,
    budget: usize,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    crate::unit_idempotency::decision_hash(project, decision, budget)
}
#[derive(Clone)]
pub(crate) struct RouteState {
    app: AppState,
    context: Arc<ReviewDecisionContext>,
}
pub(crate) struct Failure(Box<Response>);
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        *self.0
    }
}
fn failure(v: impl IntoResponse) -> Failure {
    Failure(Box::new(v.into_response()))
}
/// A verify job's construction fails with the job lifecycle's responses.
impl From<crate::attempt_lease_routes::Failure> for Failure {
    fn from(v: crate::attempt_lease_routes::Failure) -> Self {
        failure(v)
    }
}
fn internal(r: &RequestContext, op: &'static str) -> Failure {
    failure(r.internal(op))
}
fn domain(code: ErrorCode, message: impl Into<String>) -> Failure {
    failure(ApiError::from(DomainError::new(code, message)))
}
fn violation(path: &str, message: &str) -> Failure {
    failure(ApiError::from(
        DomainError::new(ErrorCode::ValidationFailed, "invalid human decision")
            .with_details(serde_json::json!([{"path":path,"message":message}])),
    ))
}
fn body_error(e: &crate::validation::ValidationErrors, r: &RequestContext) -> Failure {
    e.domain_error().map_or_else(
        |_| internal(r, "review validation"),
        |e| failure(ApiError::from(e)),
    )
}
/// Install the decision operation without any production compatibility defaults.
pub fn routes(app: AppState, context: Arc<ReviewDecisionContext>) -> Router {
    Router::new()
        .route(
            "/api/projects/{slug}/review-cases/{case_id}/decisions",
            post(decide).head(head),
        )
        .with_state(RouteState { app, context })
}
async fn head() -> impl IntoResponse {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST")])
}
fn contract(d: &Document, s: &ReviewDecisionContext, r: &RequestContext) -> Result<(), Failure> {
    let mut stack = vec![(d.root(), 0)];
    while let Some((id, depth)) = stack.pop() {
        if depth >= s.validation_walk_budget {
            return Err(internal(r, "review validation traversal"));
        }
        match d.node(id) {
            Some(Node::Array(v)) => stack.extend(v.iter().map(|v| (*v, depth + 1))),
            Some(Node::Object(v)) => stack.extend(v.iter().map(|(_, v)| (*v, depth + 1))),
            Some(_) => {}
            None => return Err(internal(r, "review invalid node")),
        }
    }
    let errors = s
        .contracts
        .document_violations(ContractKind::HumanDecision, d)
        .map_err(|_| internal(r, "review contract"))?;
    if errors.is_empty() {
        return Ok(());
    }
    let details = errors
        .into_iter()
        .map(|e| {
            e.path
                .as_utf8()
                .map(|path| serde_json::json!({"path":path,"message":e.message}))
                .ok_or_else(|| internal(r, "review validation path"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Err(failure(ApiError::from(
        DomainError::new(ErrorCode::ValidationFailed, "invalid human decision")
            .with_details(serde_json::json!(details)),
    )))
}
struct Input<'a> {
    document: &'a Document,
    action: DecisionAction,
    reason: String,
    /// A decision case's decision document.
    decision: Option<DecisionDocument>,
}
/// A checked decision document: its front matter, as a value and as JSON
/// text, and the SHA-256 of the document's text.
struct DecisionDocument {
    front_matter: serde_json::Value,
    text: String,
    sha256: String,
}
/// Parse a decision document: front matter per `decision.schema.json`, the
/// reason as its non-empty body.
fn decision_document(
    text: &str,
    s: &ReviewDecisionContext,
    r: &RequestContext,
) -> Result<(DecisionAction, String, DecisionDocument), Failure> {
    use sha2::Digest as _;
    let parsed = s
        .phases
        .parse(
            Phase::Decision,
            text,
            cannery_core::front_matter::Limits::default(),
        )
        .map_err(|error| {
            failure(crate::document_jobs::document_error(
                &error,
                "/document",
                "decision",
            ))
        })?;
    let front_matter = serde_json::Value::Object(parsed.front_matter);
    let reason = parsed.body.trim();
    if reason.is_empty() {
        return Err(violation(
            "/document",
            "the decision's body is its reason; state it",
        ));
    }
    let action = front_matter["outcome"]
        .as_str()
        .and_then(|outcome| DecisionAction::try_from(outcome).ok())
        .ok_or_else(|| internal(r, "decision outcome"))?;
    let json =
        serde_json::to_string(&front_matter).map_err(|_| internal(r, "decision encoding"))?;
    Ok((
        action,
        reason.to_owned(),
        DecisionDocument {
            front_matter,
            text: json,
            sha256: format!("{:x}", sha2::Sha256::digest(text.as_bytes())),
        },
    ))
}
impl Input<'_> {
    fn has_supersedes(&self) -> bool {
        self.document
            .field(self.document.root(), "supersedes")
            .is_some()
    }
    fn text(&self, key: &str) -> Option<&String> {
        self.document
            .field(self.document.root(), key)
            .and_then(|v| self.document.node(v))
            .and_then(|v| {
                if let Node::String(v) = v {
                    Some(v)
                } else {
                    None
                }
            })
    }
    fn supersedes(&self) -> Option<String> {
        self.text("supersedes").and_then(String::as_utf8)
    }
    #[allow(
        clippy::float_cmp,
        reason = "Python compares schema integer-valued floats exactly"
    )]
    fn revision_equal(&self, n: i32) -> bool {
        match self
            .document
            .field(self.document.root(), "evidence_revision")
            .and_then(|v| self.document.node(v))
        {
            Some(Node::Integer(v)) => v == &BigInt::from(n),
            Some(Node::Float(v)) => *v == f64::from(n),
            _ => false,
        }
    }
    fn same(&self, d: &Decision, user: &UserPrincipal, decision: bool) -> bool {
        let supersedes = if decision {
            self.supersedes() == d.supersedes.map(|v| v.0.to_string())
        } else {
            !self.has_supersedes()
        };
        // A decision recorded before decision documents has none to compare:
        // its outcome, reason and actor decide whether a document repeats it.
        let subject = match &self.decision {
            Some(document) => d
                .document
                .as_ref()
                .is_none_or(|(_, sha256)| sha256 == &document.sha256),
            None => self.revision_equal(d.subject_revision),
        };
        supersedes
            && d.actor_user_id == Some(user.user_id)
            && d.action == self.action
            && d.reason == self.reason
            && subject
    }
}
async fn load(
    c: &mut PgConnection,
    project: ProjectId,
    id: ReviewCaseId,
    lock: bool,
    _s: &ReviewDecisionContext,
    r: &RequestContext,
) -> Result<repo::Case, Failure> {
    repo::get_case(c, project, id, lock)
        .await
        .map_err(|_| internal(r, "review case"))?
        .ok_or_else(|| domain(ErrorCode::NotFound, "review case not found"))
}
async fn load_invariant(
    c: &mut PgConnection,
    project: ProjectId,
    id: ReviewCaseId,
    lock: bool,
    r: &RequestContext,
) -> Result<repo::Case, Failure> {
    repo::get_case(c, project, id, lock)
        .await
        .map_err(|_| internal(r, "review case"))?
        .ok_or_else(|| internal(r, "review case invariant"))
}

// Psycopg transaction exit reports ordinary rollback/maintenance failures
// without replacing the exception already raised by the body.
async fn rollback_after_error(
    tx: sqlx::Transaction<'_, sqlx::Postgres>,
    original: Failure,
) -> (Failure, bool) {
    let failed = tx.rollback().await.is_err();
    (original, failed)
}
async fn model(
    c: &mut PgConnection,
    case: repo::Case,
    s: &ReviewDecisionContext,
    r: &RequestContext,
) -> Result<review_attention_wire::CaseDetail, Failure> {
    review_attention_routes::case_detail(c, case, &s.reads, r)
        .await
        .map_err(failure)
}
#[allow(
    clippy::too_many_arguments,
    reason = "Explicit transaction and compatibility contexts"
)]
async fn record(
    c: &mut PgConnection,
    case: &repo::Case,
    input: &Input<'_>,
    user: &UserPrincipal,
    supersedes: Option<DecisionId>,
    s: &ReviewDecisionContext,
    r: &RequestContext,
) -> Result<Decision, Failure> {
    units::record_decision(
        c,
        RecordDecision {
            case_id: case.id,
            action: input.action,
            subject_revision: &BigInt::from(case.subject_revision),
            reason: &input.reason,
            principal: user,
            supersedes,
            document: input
                .decision
                .as_ref()
                .map(|document| (document.text.as_str(), document.sha256.as_str())),
        },
        s.reads.units,
    )
    .await
    .map_err(|_| internal(r, "review record decision"))
}
struct Audit<'a> {
    action: &'a str,
    subject_type: &'a str,
    id: String,
    prior: Option<serde_json::Value>,
    new: serde_json::Value,
    reason: Option<&'a str>,
    key: Option<&'a str>,
}
async fn event(
    c: &mut PgConnection,
    principal: &Principal,
    project: ProjectId,
    e: Audit<'_>,
    r: &RequestContext,
) -> Result<(), Failure> {
    audit::record(
        c,
        Attribution::Principal(principal),
        Record {
            action: e.action,
            subject_type: e.subject_type,
            subject_id: &e.id,
            project_id: Some(project),
            prior_state: e.prior.as_ref(),
            new_state: Some(&e.new),
            reason: e.reason,
            idempotency_key: e.key,
        },
    )
    .await
    .map_err(|_| internal(r, "review audit"))?;
    Ok(())
}
fn outcome(action: DecisionAction) -> Option<&'static str> {
    match action {
        DecisionAction::Promote => Some("promoted"),
        DecisionAction::Reject => Some("rejected"),
        DecisionAction::Inconclusive => Some("inconclusive"),
        DecisionAction::Failed => Some("failed"),
        _ => None,
    }
}
/// What a decision document cites: a phase output's id and digest, or null.
async fn cited(
    c: &mut PgConnection,
    attempt: cannery_core::ids::AttemptId,
    id: Option<cannery_reviews::EvidenceId>,
    s: &ReviewDecisionContext,
    r: &RequestContext,
) -> Result<serde_json::Value, Failure> {
    let Some(id) = id else {
        return Ok(serde_json::Value::Null);
    };
    let (_, sha256) = Repository::new(c, s.reads.attempts)
        .get_evidence_by_id(attempt, EvidenceId(id.0))
        .await
        .map_err(|_| internal(r, "review cited output"))?
        .ok_or_else(|| internal(r, "review cited output invariant"))?;
    Ok(serde_json::json!({"ref": id.0.to_string(), "sha256": sha256}))
}
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "Retain the decision order within its caller transaction"
)]
async fn decision(
    c: &mut PgConnection,
    user: &UserPrincipal,
    principal: &Principal,
    project: ProjectId,
    peek: repo::Case,
    input: &Input<'_>,
    key: Option<&str>,
    s: &ReviewDecisionContext,
    r: &RequestContext,
) -> Result<(Decision, bool), Failure> {
    let id = peek
        .attempt_id
        .ok_or_else(|| internal(r, "review decision attempt invariant"))?;
    let attempt = Repository::new(c, s.reads.attempts)
        .get_attempt_by_id(id, true)
        .await
        .map_err(|_| internal(r, "review decision attempt"))?
        .ok_or_else(|| internal(r, "review decision attempt invariant"))?;
    let case = load_invariant(c, project, peek.id, true, r).await?;
    let Some(document) = &input.decision else {
        return Err(violation(
            "/document",
            "a decision case is decided with a decision document",
        ));
    };
    let decisions = units::list_decisions(c, &[case.id], s.reads.units)
        .await
        .map_err(|_| internal(r, "review current decisions"))?;
    let old_ids = decisions
        .iter()
        .filter_map(|v| v.supersedes.map(|v| v.0))
        .collect::<BTreeSet<_>>();
    let current = decisions.iter().rfind(|v| !old_ids.contains(&v.id.0));
    let (supersedes, expected) = if case.state == CaseState::Resolved {
        let current = current.ok_or_else(|| internal(r, "review current invariant"))?;
        if input.same(current, user, true) {
            let id = current.id;
            return Ok((
                decisions
                    .into_iter()
                    .find(|v| v.id == id)
                    .ok_or_else(|| internal(r, "review replay invariant"))?,
                true,
            ));
        }
        if !input.has_supersedes() {
            return Err(domain(
                ErrorCode::Conflict,
                format!(
                    "this decision case is already decided; a correction supersedes {}",
                    current.id.0
                ),
            ));
        }
        if input.supersedes() != Some(current.id.0.to_string()) {
            return Err(domain(
                ErrorCode::Conflict,
                format!(
                    "only the case's current decision {} can be superseded",
                    current.id.0
                ),
            ));
        }
        (
            Some(current.id),
            outcome(current.action).ok_or_else(|| internal(r, "review current outcome"))?,
        )
    } else {
        if input.has_supersedes() {
            return Err(violation(
                "/supersedes",
                "a pending case has no decision to supersede",
            ));
        }
        // A project that registers a decider step decides automatically; a
        // researcher corrects that decision once it is recorded.
        if let Some(job) = jobs::decide_job(c, case.unit_id, s.jobs)
            .await
            .map_err(|_| internal(r, "review decide job"))?
            .filter(|job| matches!(job.state, jobs::State::Pending | jobs::State::Claimed))
        {
            return Err(domain(
                ErrorCode::Conflict,
                format!(
                    "decider {} decides this unit (decide job {} is {}); once its decision is recorded, a researcher may correct it with supersedes",
                    job.verifier_id.as_deref().unwrap_or_default(),
                    job.id,
                    job.state.as_str()
                ),
            ));
        }
        (None, "deciding")
    };
    let to = outcome(input.action).ok_or_else(|| internal(r, "review decision outcome"))?;
    let verification = cited(c, id, case.evidence_id, s, r).await?;
    if document.front_matter["verification"] != verification {
        return Err(violation(
            "/document",
            &if verification.is_null() {
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
    let writeup = cited(c, id, case.writeup_id, s, r).await?;
    if document.front_matter["writeup"] != writeup {
        return Err(violation(
            "/document",
            &if writeup.is_null() {
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
    let verdict = if let Some(evidence) = case.evidence_id {
        let (stored, _) = Repository::new(c, s.reads.attempts)
            .get_evidence_by_id(id, EvidenceId(evidence.0))
            .await
            .map_err(|_| internal(r, "review decision evidence"))?
            .ok_or_else(|| internal(r, "review evidence invariant"))?;
        let StoredJson::Value(d) = stored else {
            return Err(internal(r, "review verdict type"));
        };
        let verdict = d
            .field(d.root(), "verdict")
            .ok_or_else(|| internal(r, "review verdict lookup"))?;
        Some(
            cannery_core::text::str_value(&d, verdict, s.science_rendering.nesting_budget)
                .map_err(|_| internal(r, "review verdict repr"))?
                .as_utf8()
                .ok_or_else(|| internal(r, "review verdict encoding"))?,
        )
    } else {
        None
    };
    match (&verdict, input.action) {
        (None, DecisionAction::Failed) => {}
        (None, _) => {
            return Err(violation(
                "/document",
                "front matter /outcome: a unit stopped after a failure is decided failed",
            ));
        }
        (Some(_), DecisionAction::Failed) => {
            return Err(violation(
                "/document",
                "front matter /outcome: failed is the outcome of a unit stopped after a failure",
            ));
        }
        (Some(verdict), DecisionAction::Promote) if verdict != "pass" => {
            return Err(violation(
                "/document",
                &format!(
                    "front matter /outcome: promotion requires a pass verdict; this verdict is {verdict}"
                ),
            ));
        }
        _ => {}
    }
    if case.unit_state.as_str() != expected {
        return Err(domain(
            ErrorCode::Conflict,
            format!(
                "the unit is {}; nothing awaits this decision",
                case.unit_state.as_str()
            ),
        ));
    }
    let d = record(c, &case, input, user, supersedes, s, r).await?;
    event(c,principal,project,Audit{action:&format!("review.{}",input.action.as_str()),subject_type:"review_case",id:case.id.to_string(),prior:Some(serde_json::json!({"state":case.state.as_str(),"kind":"decision","decision_id":current.map(|v|v.id.0.to_string())})),new:serde_json::json!({"state":"resolved","decision_id":d.id.0.to_string(),"action":input.action.as_str(),"subject_revision":case.subject_revision,"verdict":verdict,"decision_sha256":document.sha256,"supersedes":supersedes.map(|v|v.0.to_string())}),reason:Some(&input.reason),key},r).await?;
    units::set_state(
        c,
        case.unit_id,
        UnitState::try_from(to).map_err(|_| internal(r, "review outcome"))?,
        None,
    )
    .await
    .map_err(|_| internal(r, "review decision state"))?;
    event(c,principal,project,Audit{action:if supersedes.is_some(){"unit.decision_corrected"}else{"unit.decided"},subject_type:"unit",id:case.unit_id.to_string(),prior:Some(serde_json::json!({"state":case.unit_state.as_str()})),new:serde_json::json!({"state":to,"attempt_id":attempt.id.to_string(),"decision_id":d.id.0.to_string()}),reason:Some(&input.reason),key},r).await?;
    Ok((d, false))
}
#[allow(
    clippy::too_many_arguments,
    reason = "Retain pinned-job construction and audit order with explicit contexts"
)]
/// A researcher's retry of a failed verification: a fresh verify job from the attempt's run
/// record, under the science revision the attempt pinned.
async fn rerun(
    c: &mut PgConnection,
    principal: &Principal,
    attempt: &Attempt,
    overhead: &BigInt,
    reason: &str,
    key: Option<&str>,
    s: &ReviewDecisionContext,
    r: &RequestContext,
) -> Result<(), Failure> {
    let run = crate::job_lifecycle::run_inputs(c, attempt, &s.lifecycle, r)
        .await?
        .ok_or_else(|| {
            domain(
                ErrorCode::Conflict,
                "the attempt has no run record to verify; it can only be closed",
            )
        })?;
    let previous = jobs::latest_job(c, attempt.id, jobs::Phase::Verify, s.jobs)
        .await
        .map_err(|_| internal(r, "review previous job"))?;
    Repository::new(c, s.reads.attempts)
        .reopen_attempt(attempt.id, "verifying")
        .await
        .map_err(|_| internal(r, "review reopen"))?;
    let job = crate::job_lifecycle::create_verify_job(
        c,
        attempt,
        &run,
        overhead,
        if previous.is_some() {
            jobs::Origin::HumanRetry
        } else {
            jobs::Origin::Submission
        },
        previous.map(|job| job.id),
        &s.lifecycle,
        r,
    )
    .await?;
    crate::job_lifecycle::record_job_created(c, principal, attempt, &job, &s.lifecycle, r).await?;
    event(c,principal,attempt.project_id,Audit{action:"attempt.retried",subject_type:"attempt",id:attempt.id.to_string(),prior:Some(serde_json::json!({"state":attempt.state.as_str()})),new:serde_json::json!({"state":"verifying","job_id":job.id.to_string(),"run_number":job.run_number}),reason:Some(reason),key},r).await?;
    Ok(())
}
#[allow(
    clippy::too_many_arguments,
    clippy::many_single_char_names,
    reason = "Retain source failure decisions with caller-owned transaction and contexts"
)]
async fn failure_case(
    c: &mut PgConnection,
    user: &UserPrincipal,
    principal: &Principal,
    project: ProjectId,
    peek: repo::Case,
    input: &Input<'_>,
    key: Option<&str>,
    overhead: &BigInt,
    s: &ReviewDecisionContext,
    r: &RequestContext,
) -> Result<(Decision, bool), Failure> {
    let id = peek
        .attempt_id
        .ok_or_else(|| internal(r, "review failed attempt invariant"))?;
    let attempt = Repository::new(c, s.reads.attempts)
        .get_attempt_by_id(id, true)
        .await
        .map_err(|_| internal(r, "review failed attempt"))?
        .ok_or_else(|| internal(r, "review failed attempt invariant"))?;
    let case = load_invariant(c, project, peek.id, true, r).await?;
    if case.state == CaseState::Resolved {
        let mut decisions = units::list_decisions(c, &[case.id], s.reads.units)
            .await
            .map_err(|_| internal(r, "review failure decisions"))?;
        let last=decisions.pop().ok_or_else(||domain(ErrorCode::Conflict,"this failure case is already decided; a failure decision takes effect when recorded and is not superseded"))?;
        if input.same(&last, user, false) {
            return Ok((last, true));
        }
        return Err(domain(
            ErrorCode::Conflict,
            "this failure case is already decided; a failure decision takes effect when recorded and is not superseded",
        ));
    }
    if input.has_supersedes() {
        return Err(violation(
            "/supersedes",
            "a pending case has no decision to supersede",
        ));
    }
    if !matches!(input.action, DecisionAction::Retry | DecisionAction::Stop) {
        return Err(violation(
            "/action",
            "a failure case is decided with retry or stop",
        ));
    }
    if !input.revision_equal(case.subject_revision) {
        return Err(domain(
            ErrorCode::StaleRevision,
            format!(
                "this case is about failure revision {}; decide on it",
                case.subject_revision
            ),
        ));
    }
    if attempt.state.as_str() != "failed" || case.unit_state.as_str() != "active" {
        return Err(domain(
            ErrorCode::Conflict,
            format!(
                "the attempt is {} and the unit {}; nothing awaits this decision",
                attempt.state.as_str(),
                case.unit_state.as_str()
            ),
        ));
    }
    let found = repo::get_failure(
        c,
        case.failure_id
            .ok_or_else(|| internal(r, "review failure invariant"))?,
        s.reads.reviews,
    )
    .await
    .map_err(|_| internal(r, "review failure load"))?
    .ok_or_else(|| internal(r, "review failure invariant"))?;
    let d = record(c, &case, input, user, None, s, r).await?;
    let reason = input
        .reason
        .as_utf8()
        .ok_or_else(|| internal(r, "review reason encoding"))?;
    event(c,principal,project,Audit{action:&format!("review.{}",input.action.as_str()),subject_type:"review_case",id:case.id.to_string(),prior:Some(serde_json::json!({"state":"pending","kind":"failure"})),new:serde_json::json!({"state":"resolved","decision_id":d.id.0.to_string(),"action":input.action.as_str(),"subject_revision":case.subject_revision}),reason:Some(&reason),key},r).await?;
    let code = found
        .code
        .as_utf8()
        .ok_or_else(|| internal(r, "review failure code"))?;
    if input.action == DecisionAction::Stop {
        // A stopped unit is written up, then decided failed.
        let job = crate::document_jobs::stop(c, principal, &attempt, &s.lifecycle, r).await?;
        event(c,principal,project,Audit{action:"unit.stopped",subject_type:"unit",id:case.unit_id.to_string(),prior:Some(serde_json::json!({"state":case.unit_state.as_str()})),new:serde_json::json!({"state":"documenting","attempt_id":attempt.id.to_string(),"job_id":job.id.to_string(),"failure_stage":found.stage.as_str(),"failure_code":code}),reason:Some(&reason),key},r).await?;
        return Ok((d, false));
    }
    let to = if found.stage == Stage::Agent {
        "queued"
    } else {
        rerun(c, principal, &attempt, overhead, &reason, key, s, r).await?;
        "active"
    };
    units::set_state(
        c,
        case.unit_id,
        UnitState::try_from(to).map_err(|_| internal(r, "review retry outcome"))?,
        None,
    )
    .await
    .map_err(|_| internal(r, "review retry state"))?;
    event(c,principal,project,Audit{action:"unit.retried",subject_type:"unit",id:case.unit_id.to_string(),prior:Some(serde_json::json!({"state":case.unit_state.as_str()})),new:serde_json::json!({"state":to,"attempt_id":attempt.id.to_string(),"failure_stage":found.stage.as_str(),"failure_code":code}),reason:Some(&reason),key},r).await?;
    Ok((d, false))
}
#[allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "Preserve source authentication, validation, transaction and model timing"
)]
#[utoipa::path(
    post,
    path = "/api/projects/{slug}/review-cases/{case_id}/decisions",
    operation_id = "decide_api_projects__slug__review_cases__case_id__decisions_post",
    summary = "Decide",
    description = "Record a researcher's decision on a pending review case.\n\nA decision case is decided with a decision document: Markdown with YAML\nfront matter (``decision.schema.json``) naming the outcome (`promote`,\n`reject`, `inconclusive`, or `failed` for a unit stopped after a\nfailure) and citing the case's verification report and write-up, each\nnull when there is none; its body is the reason. A promotion requires a\n`pass` verdict. A decided case is corrected with `supersedes`.\n\nA failure case is decided with `retry` or `stop`, the failure revision and\na reason: `stop` sends the unit to be written up, then decided.",
    params(("slug" = String, Path),
        ("case_id" = String, Path, format = "uuid"),
        ("idempotency-key" = Option<String>, Header)),
    request_body(content = crate::api_models::HumanDecisionRequest, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::cannery_row__reviews__routes__ReviewCaseOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn decide(
    State(s): State<RouteState>,
    axum::Extension(r): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request).await.map_err(failure)?;
    let mut auth = authenticate(&s.app, &r, &parts.headers, &parts.method)
        .await
        .map_err(failure)?;
    let paths = Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &s)
        .await
        .map_err(failure)?
        .0;
    let params = crate::validation::review_attention_parameters(
        paths.get("case_id").map(String::as_str),
        &[],
        false,
        false,
    );
    let input = match &body {
        crate::body::DecodedBody::Missing => crate::validation::BodyInput::Missing,
        crate::body::DecodedBody::RawBytes => crate::validation::BodyInput::RawBytes,
        crate::body::DecodedBody::Json(d) => crate::validation::BodyInput::Json(d),
    };
    let document = crate::validation::validate_document_body(input);
    let (params, d) = match (params, document) {
        (Ok(p), Ok(d)) => (p, d),
        (Err(mut p), Err(d)) => {
            p.append(d);
            return Err(body_error(&p, &r));
        }
        (Err(e), Ok(_)) | (Ok(_), Err(e)) => return Err(body_error(&e, &r)),
    };
    let user = auth
        .principal
        .require_user()
        .map_err(|e| failure(ApiError::from(e)))?;
    let slug = paths
        .get("slug")
        .ok_or_else(|| internal(&r, "review slug"))?;
    let project = cannery_projects::authz::project_access(
        &mut auth.connection,
        &auth.principal,
        slug,
        Some(Role::Researcher),
        &[],
        true,
    )
    .await
    .map_err(|e| failure(r.project_error(e)))?
    .project;
    contract(d, &s.context, &r)?;
    let typed_document =
        crate::api_models::request_document::<crate::api_models::HumanDecisionRequest>(d)
            .map_err(failure)?;
    let d = &typed_document;
    let id = params
        .case_id
        .ok_or_else(|| internal(&r, "review case path"))?;
    if !matches!(d.field(d.root(),"review_case_id").and_then(|v|d.node(v)),Some(Node::String(v)) if v.equals_utf8(&id.to_string()))
    {
        return Err(violation(
            "/review_case_id",
            &format!("this is review case {id}"),
        ));
    }
    let key = crate::request_context::first_header(&parts.headers, "idempotency-key");
    if key
        .as_ref()
        .is_some_and(|v| v.chars().count() == 0 || v.chars().count() > 200)
    {
        return Err(domain(
            ErrorCode::ValidationFailed,
            "Idempotency-Key must be 1 to 200 characters",
        ));
    }
    let hash =
        crate::unit_idempotency::decision_hash(&project.slug, d, s.context.request_hash_budget)
            .map_err(|_| internal(&r, "review request hash"))?;
    let text = |name: &str| {
        d.field(d.root(), name)
            .and_then(|v| d.node(v))
            .and_then(|v| {
                if let Node::String(v) = v {
                    v.as_utf8()
                } else {
                    None
                }
            })
    };
    // A decision case takes a decision document; a failure case an action
    // and a reason. The contract admits exactly one of the two forms.
    let input = if let Some(document) = text("document") {
        let (action, reason, decision) = decision_document(&document, &s.context, &r)?;
        Input {
            document: d,
            action,
            reason,
            decision: Some(decision),
        }
    } else {
        let action = text("action").ok_or_else(|| internal(&r, "review action"))?;
        Input {
            document: d,
            action: DecisionAction::try_from(action.as_str())
                .map_err(|_| internal(&r, "review action enum"))?,
            reason: text("reason").ok_or_else(|| internal(&r, "review reason"))?,
            decision: None,
        }
    };
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&r, "review transaction"))?;
    let actor = format!("user:{}", user.user_id);
    let result = async {
        if let Some(key) = &key
            && let Some((prior, _)) =
                crate::review_decision_idempotency::lookup(&mut tx, &actor, key)
                    .await
                    .map_err(|_| internal(&r, "review replay lookup"))?
        {
            if prior != hash {
                return Err(domain(
                    ErrorCode::Conflict,
                    "this Idempotency-Key was already used with a different request",
                ));
            }
            let case = load_invariant(&mut tx, project.id, id, false, &r).await?;
            return Ok((model(&mut tx, case, &s.context, &r).await?, true));
        }
        let peek = load(&mut tx, project.id, id, false, &s.context, &r).await?;
        let (decision, replayed) = match peek.kind {
            CaseKind::Failure => {
                failure_case(
                    &mut tx,
                    user,
                    &auth.principal,
                    project.id,
                    peek,
                    &input,
                    key.as_deref(),
                    s.app.settings.leases.job_overhead_seconds.as_bigint(),
                    &s.context,
                    &r,
                )
                .await?
            }
            CaseKind::Decision => {
                decision(
                    &mut tx,
                    user,
                    &auth.principal,
                    project.id,
                    peek,
                    &input,
                    key.as_deref(),
                    &s.context,
                    &r,
                )
                .await?
            }
        };
        if let Some(key) = &key {
            crate::review_decision_idempotency::remember(
                &mut tx,
                &actor,
                key,
                &hash,
                &decision.id.0.to_string(),
            )
            .await
            .map_err(|_| internal(&r, "review remember"))?;
        }
        let case = load_invariant(&mut tx, project.id, id, false, &r).await?;
        Ok((model(&mut tx, case, &s.context, &r).await?, replayed))
    }
    .await;
    match result {
        Ok((detail, replayed)) => {
            let Ok(bytes) = review_attention_wire::case(&detail, s.context.reads.response) else {
                let (error, rollback_failed) =
                    rollback_after_error(tx, internal(&r, "review serialization")).await;
                if rollback_failed {
                    auth.connection.close_on_drop();
                }
                return Err(error);
            };
            tx.commit()
                .await
                .map_err(|_| internal(&r, "review commit"))?;
            Ok((
                if replayed {
                    StatusCode::OK
                } else {
                    StatusCode::CREATED
                },
                [(header::CONTENT_TYPE, "application/json")],
                bytes,
            )
                .into_response())
        }
        Err(e) => {
            let (e, rollback_failed) = rollback_after_error(tx, e).await;
            if rollback_failed {
                // Retain the body error, but never return a connection whose
                // rollback or statement cleanup failed to the pool.
                auth.connection.close_on_drop();
            }
            Err(e)
        }
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
