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
    contracts::{ContractKind, ContractValidator},
    errors::{DomainError, ErrorCode},
    ids::{JobId, ProjectId, ReviewCaseId},
    json::{self, Document, Node},
    principal::{Principal, Role, UserPrincipal},
};
use cannery_hypotheses::repo::{
    self as hypotheses, Decision, DecisionAction, DecisionId, HypothesisState, RecordDecision,
};
use cannery_jobs::repo as jobs;
use cannery_reviews::{CaseKind, CaseState, Stage, repo};
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
    crate::hypothesis_idempotency::decision_hash(project, decision, budget)
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
    fn same(&self, d: &Decision, user: &UserPrincipal, result: bool) -> bool {
        let supersedes = if result {
            self.supersedes() == d.supersedes.map(|v| v.0.to_string())
        } else {
            !self.has_supersedes()
        };
        supersedes
            && d.actor_user_id == user.user_id
            && d.action == self.action
            && d.reason == self.reason
            && self.revision_equal(d.subject_revision)
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
    hypotheses::record_decision(
        c,
        RecordDecision {
            case_id: case.id,
            action: input.action,
            subject_revision: &BigInt::from(case.subject_revision),
            reason: &input.reason,
            principal: user,
            supersedes,
        },
        s.reads.hypotheses,
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
        _ => None,
    }
}
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "Retain the source result decision order within its caller transaction"
)]
async fn result(
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
        .ok_or_else(|| internal(r, "review result attempt invariant"))?;
    let attempt = Repository::new(c, s.reads.attempts)
        .get_attempt_by_id(id, true)
        .await
        .map_err(|_| internal(r, "review result attempt"))?
        .ok_or_else(|| internal(r, "review result attempt invariant"))?;
    let case = load_invariant(c, project, peek.id, true, r).await?;
    let decisions = hypotheses::list_decisions(c, &[case.id], s.reads.hypotheses)
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
                    "this result case is already decided; a correction supersedes {}",
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
            outcome(current.action).ok_or_else(|| internal(r, "review legacy current outcome"))?,
        )
    } else {
        if input.has_supersedes() {
            return Err(violation(
                "/supersedes",
                "a pending case has no decision to supersede",
            ));
        }
        (None, "awaiting_human_review")
    };
    let to = outcome(input.action).ok_or_else(|| {
        violation(
            "/action",
            "a result case is decided with promote, reject or inconclusive",
        )
    })?;
    if !input.revision_equal(case.subject_revision) {
        return Err(domain(
            ErrorCode::StaleRevision,
            format!(
                "this case is about evaluator record revision {}; decide on it",
                case.subject_revision
            ),
        ));
    }
    let evidence = peek
        .evidence_id
        .ok_or_else(|| internal(r, "review result evidence invariant"))?;
    let (document, _) = Repository::new(c, s.reads.attempts)
        .get_evidence_by_id(id, EvidenceId(evidence.0))
        .await
        .map_err(|_| internal(r, "review result evidence"))?
        .ok_or_else(|| internal(r, "review evidence invariant"))?;
    let StoredJson::Value(d) = document else {
        return Err(internal(r, "review verdict type"));
    };
    let verdict = d
        .field(d.root(), "assessment")
        .and_then(|v| d.field(v, "verdict"))
        .ok_or_else(|| internal(r, "review verdict lookup"))?;
    let verdict = cannery_core::text::str_value(&d, verdict, s.science_rendering.nesting_budget)
        .map_err(|_| internal(r, "review verdict repr"))?
        .as_utf8()
        .ok_or_else(|| internal(r, "review verdict encoding"))?;
    if input.action == DecisionAction::Promote && verdict != "pass" {
        return Err(violation(
            "/action",
            &format!("promotion requires an evaluator pass; this verdict is {verdict}"),
        ));
    }
    if attempt.state.as_str() != expected || case.hypothesis_state.as_str() != expected {
        return Err(domain(
            ErrorCode::Conflict,
            format!(
                "the attempt is {} and the hypothesis {}; nothing awaits this decision",
                attempt.state.as_str(),
                case.hypothesis_state.as_str()
            ),
        ));
    }
    let d = record(c, &case, input, user, supersedes, s, r).await?;
    let reason = input
        .reason
        .as_utf8()
        .ok_or_else(|| internal(r, "review reason encoding"))?;
    event(c,principal,project,Audit{action:&format!("review.{}",input.action.as_str()),subject_type:"review_case",id:case.id.to_string(),prior:Some(serde_json::json!({"state":case.state.as_str(),"kind":"result","decision_id":current.map(|v|v.id.0.to_string())})),new:serde_json::json!({"state":"resolved","decision_id":d.id.0.to_string(),"action":input.action.as_str(),"subject_revision":case.subject_revision,"verdict":verdict,"supersedes":supersedes.map(|v|v.0.to_string())}),reason:Some(&reason),key},r).await?;
    Repository::new(c, s.reads.attempts)
        .move_attempt(id, expected, to)
        .await
        .map_err(|_| internal(r, "review result move"))?;
    hypotheses::set_state(
        c,
        case.hypothesis_id,
        HypothesisState::try_from(to).map_err(|_| internal(r, "review outcome"))?,
        None,
    )
    .await
    .map_err(|_| internal(r, "review result state"))?;
    event(c,principal,project,Audit{action:if supersedes.is_some(){"attempt.decision_corrected"}else{"attempt.decided"},subject_type:"attempt",id:id.to_string(),prior:Some(serde_json::json!({"state":attempt.state.as_str(),"hypothesis_state":case.hypothesis_state.as_str()})),new:serde_json::json!({"state":to,"hypothesis_state":to,"decision_id":d.id.0.to_string()}),reason:Some(&reason),key},r).await?;
    Ok((d, false))
}
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "Retain pinned-job construction and audit order with explicit contexts"
)]
async fn rerun(
    c: &mut PgConnection,
    principal: &Principal,
    attempt: &Attempt,
    stage: Stage,
    reason: &str,
    key: Option<&str>,
    s: &ReviewDecisionContext,
    r: &RequestContext,
) -> Result<(), Failure> {
    let science = cannery_research::config_repo::get_revision(
        c,
        attempt.project_id,
        cannery_research::config_repo::Kind::Science,
        Some(&BigInt::from(attempt.science_revision)),
        s.config,
    )
    .await
    .map_err(|_| internal(r, "review science"))?
    .ok_or_else(|| internal(r, "review science invariant"))?;
    let loaded = cannery_research::science::Science::new(
        BigInt::from(science.revision),
        &science.content,
        s.science_rendering,
    )
    .map_err(|_| internal(r, "review science construction"))?;
    if loaded
        .legacy_problem()
        .map_err(|_| internal(r, "review legacy science"))?
        .is_some()
    {
        return Err(domain(
            ErrorCode::Conflict,
            format!(
                "science revision {} has no evaluator; built-in gates moved to the stock evaluator: this attempt cannot be retried, only closed",
                science.revision
            ),
        ));
    }
    let job_stage = if stage == Stage::Tester {
        jobs::Stage::Tester
    } else {
        jobs::Stage::Evaluator
    };
    let previous = jobs::latest_job(c, attempt.id, job_stage, s.jobs)
        .await
        .map_err(|_| internal(r, "review previous job"))?
        .ok_or_else(|| {
            domain(
                ErrorCode::Conflict,
                format!("the attempt has no {} run to retry", stage.as_str()),
            )
        })?;
    let to = if stage == Stage::Tester {
        "testing"
    } else {
        "evaluating"
    };
    Repository::new(c, s.reads.attempts)
        .reopen_attempt(attempt.id, to)
        .await
        .map_err(|_| internal(r, "review reopen"))?;
    let id = JobId(uuid::Uuid::new_v4());
    // Source rerun changes only output_prefix in the existing lossless specification.
    let prefix = format!(
        "projects/{}/attempts/{}/{}/{}/",
        previous.project_id,
        previous.attempt_id,
        if stage == Stage::Tester {
            "test-runs"
        } else {
            "evaluation-runs"
        },
        id
    );
    let spec =
        replace_prefix(&previous.spec, &prefix).map_err(|_| internal(r, "review job prefix"))?;
    let job = jobs::create_job(
        c,
        jobs::NewJob {
            id,
            project_id: previous.project_id,
            attempt_id: previous.attempt_id,
            stage: previous.stage,
            science_revision: &BigInt::from(previous.science_revision),
            tester_id: &previous.tester_id,
            spec: &spec,
            deadline_seconds: &BigInt::from(previous.deadline_seconds),
            origin: jobs::Origin::HumanRetry,
            previous_run_id: Some(previous.id),
        },
        s.jobs,
    )
    .await
    .map_err(|_| internal(r, "review rerun"))?;
    let mut steps = vec![];
    if let Some(v) = job.spec.field(job.spec.root(), "steps") {
        let Some(Node::Array(v)) = job.spec.node(v) else {
            return Err(internal(r, "review job audit steps"));
        };
        for v in v {
            let name = job
                .spec
                .field(*v, "name")
                .ok_or_else(|| internal(r, "review job audit name"))?;
            let revision = job
                .spec
                .field(*v, "revision")
                .ok_or_else(|| internal(r, "review job audit revision"))?;
            steps.push(serde_json::json!({"name":subtree(&job.spec,name,s.audit_encoding_budget).map_err(|_|internal(r,"review step name"))?,"revision":subtree(&job.spec,revision,s.audit_encoding_budget).map_err(|_|internal(r,"review step revision"))?}));
        }
    }
    event(c,principal,attempt.project_id,Audit{action:"job.created",subject_type:"job",id:job.id.to_string(),prior:None,new:serde_json::json!({"state":job.state.as_str(),"stage":job.stage.as_str(),"run_number":job.run_number,"attempt_id":job.attempt_id.to_string(),"service":job.tester_id,"origin":job.origin.as_str(),"previous_run_id":job.previous_run_id.map(|v|v.to_string()),"steps":steps}),reason:None,key:None},r).await?;
    event(c,principal,attempt.project_id,Audit{action:"attempt.retried",subject_type:"attempt",id:attempt.id.to_string(),prior:Some(serde_json::json!({"state":attempt.state.as_str()})),new:serde_json::json!({"state":to,"job_id":job.id.to_string(),"run_number":job.run_number}),reason:Some(reason),key},r).await?;
    Ok(())
}
fn subtree(
    d: &Document,
    id: json::NodeId,
    budget: usize,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let text = json::encode_ascii_pretty_node(d, id, budget)?;
    Ok(serde_json::from_str(&text)?)
}
fn replace_prefix(d: &Document, prefix: &str) -> Result<Document, json::BuildError> {
    let Some(Node::Object(fields)) = d.node(d.root()) else {
        return Err(json::BuildError::InvalidNode);
    };
    let mut builder = json::DocumentBuilder::new();
    let replacement = builder.push(Node::String(String::from(prefix)))?;
    let mut copied = Vec::with_capacity(fields.len() + 1);
    let mut replaced = false;
    for (name, value) in fields {
        let value = if name.equals_utf8("output_prefix") {
            replaced = true;
            replacement
        } else {
            builder.import(d, *value)?
        };
        copied.push((name.clone(), value));
    }
    if !replaced {
        copied.push((String::from("output_prefix"), replacement));
    }
    let root = builder.push(Node::Object(copied))?;
    builder.finish(root)
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
        let mut decisions = hypotheses::list_decisions(c, &[case.id], s.reads.hypotheses)
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
    if !matches!(
        input.action,
        DecisionAction::Retry | DecisionAction::CloseFailed
    ) {
        return Err(violation(
            "/action",
            "a failure case is decided with retry or close_failed",
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
    if attempt.state.as_str() != "failed"
        || case.hypothesis_state.as_str() != "awaiting_human_review"
    {
        return Err(domain(
            ErrorCode::Conflict,
            format!(
                "the attempt is {} and the hypothesis {}; nothing awaits this decision",
                attempt.state.as_str(),
                case.hypothesis_state.as_str()
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
    let to = if input.action == DecisionAction::CloseFailed {
        "failed"
    } else if found.stage == Stage::Agent {
        "queued"
    } else {
        rerun(c, principal, &attempt, found.stage, &reason, key, s, r).await?;
        "active"
    };
    hypotheses::set_state(
        c,
        case.hypothesis_id,
        HypothesisState::try_from(to).map_err(|_| internal(r, "review retry outcome"))?,
        None,
    )
    .await
    .map_err(|_| internal(r, "review retry state"))?;
    event(c,principal,project,Audit{action:if input.action==DecisionAction::CloseFailed{"hypothesis.failure_closed"}else{"hypothesis.retried"},subject_type:"hypothesis",id:case.hypothesis_id.to_string(),prior:Some(serde_json::json!({"state":case.hypothesis_state.as_str()})),new:serde_json::json!({"state":to,"attempt_id":attempt.id.to_string(),"failure_stage":found.stage.as_str(),"failure_code":found.code.as_utf8().ok_or_else(||internal(r,"review failure code"))?}),reason:Some(&reason),key},r).await?;
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
    description = "Record a researcher's reasoned decision on a pending review case.",
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
    let hash = crate::hypothesis_idempotency::decision_hash(
        &project.slug,
        d,
        s.context.request_hash_budget,
    )
    .map_err(|_| internal(&r, "review request hash"))?;
    let action = d
        .field(d.root(), "action")
        .and_then(|v| d.node(v))
        .and_then(|v| {
            if let Node::String(v) = v {
                v.as_utf8()
            } else {
                None
            }
        })
        .ok_or_else(|| internal(&r, "review action"))?;
    let reason = d
        .field(d.root(), "reason")
        .and_then(|v| d.node(v))
        .and_then(|v| {
            if let Node::String(v) = v {
                Some(v.clone())
            } else {
                None
            }
        })
        .ok_or_else(|| internal(&r, "review reason"))?;
    let input = Input {
        document: d,
        action: DecisionAction::try_from(action.as_str())
            .map_err(|_| internal(&r, "review action enum"))?,
        reason,
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
                    &s.context,
                    &r,
                )
                .await?
            }
            CaseKind::Result => {
                result(
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
            CaseKind::Draft => {
                return Err(domain(
                    ErrorCode::Conflict,
                    "draft cases are decided through the hypothesis's draft-review",
                ));
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
