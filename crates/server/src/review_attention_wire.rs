//! Ordered read response models; dynamic documents remain lossless.
use crate::{
    api_contract::{convert, decode, encode},
    api_models::{
        AttentionFailure, AttentionOut, AttentionOutcome, AttentionReview, AttentionRunning,
        AttentionStalledVerification, ReviewCasePage, VerificationDocument,
        cannery_row__reviews__routes__FailureOut, cannery_row__reviews__routes__ReviewCaseOut,
    },
};
use cannery_attention::{
    Outcome, PendingReview, RecentFailure, RunningAttempt, StalledVerification,
};
use cannery_core::json::{
    Document, Node,
    model::{self, ModelEncodeError},
};
use cannery_hypotheses::repo::Decision;
use cannery_reviews::repo::{Case, Failure};
use std::collections::BTreeMap;
type Result<T> = std::result::Result<T, ModelEncodeError>;
#[derive(Clone, Copy)]
pub struct ResponseContext {
    pub inferred_nesting_budget: usize,
}
fn timestamp(v: cannery_core::timestamps::Timestamp) -> String {
    crate::timestamps::public_timestamp(v)
}
fn array(values: impl Iterator<Item = Result<Vec<u8>>>) -> Result<Vec<u8>> {
    let values = values
        .map(|v| decode::<serde_json::Value>(&v?))
        .collect::<Result<Vec<_>>>()?;
    encode(&values)
}
fn mapping(d: &Document, p: ResponseContext) -> Result<Vec<u8>> {
    if !matches!(d.node(d.root()), Some(Node::Object(_))) {
        return Err(ModelEncodeError::InvalidNode);
    }
    model::encode_model_mapping(d, d.root(), p.inferred_nesting_budget)
}
pub(crate) struct CaseDetail {
    pub case: Case,
    pub failure: Option<Failure>,
    pub verification: Option<(std::sync::Arc<Document>, String)>,
    pub decisions: Vec<Decision>,
}
impl CaseDetail {
    pub(crate) fn validate_failure(f: &Failure) -> Result<()> {
        if !matches!(f.details.node(f.details.root()), Some(Node::Object(_))) {
            return Err(ModelEncodeError::InvalidNode);
        }
        match f.log_refs.node(f.log_refs.root()) {
            Some(Node::Array(v))
                if v.iter()
                    .all(|id| matches!(f.log_refs.node(*id), Some(Node::Object(_)))) =>
            {
                Ok(())
            }
            _ => Err(ModelEncodeError::InvalidNode),
        }
    }
    pub(crate) fn validate(&self) -> Result<()> {
        if let Some(f) = &self.failure {
            Self::validate_failure(f)?;
        }
        if let Some((d, _)) = &self.verification
            && !matches!(d.node(d.root()), Some(Node::Object(_) | Node::Null))
        {
            return Err(ModelEncodeError::InvalidNode);
        }
        Ok(())
    }
}
pub(crate) fn case(d: &CaseDetail, p: ResponseContext) -> Result<Vec<u8>> {
    d.validate()?;
    let c = &d.case;

    encode(&cannery_row__reviews__routes__ReviewCaseOut {
        id: convert(c.id)?,
        kind: convert(c.kind.as_str())?,
        state: convert(c.state.as_str())?,
        subject_revision: convert(c.subject_revision)?,
        hypothesis: convert(c.hypothesis_number)?,
        hypothesis_ref: convert(format!("#{}", c.hypothesis_number))?,
        hypothesis_state: convert(c.hypothesis_state.as_str())?,
        attempt_ref: convert(attempt_ref(c.hypothesis_number, c.attempt_sequence))?,
        attempt_state: convert(c.attempt_state.map(cannery_reviews::AttemptState::as_str))?,
        opened_at: convert(timestamp(c.opened_at))?,
        resolved_at: convert(c.resolved_at.map(timestamp))?,
        origin: convert(c.origin.as_str())?,
        source_ref: (c.source_ref.as_ref())
            .map(|v| v.as_utf8().ok_or(ModelEncodeError::Encoding))
            .transpose()?,
        failure: decode(&d.failure.as_ref().map_or_else(
            || Ok(b"null".to_vec()),
            |f| {
                encode(&cannery_row__reviews__routes__FailureOut {
                    stage: convert(f.stage.as_str())?,
                    code: f.code.as_utf8().ok_or(ModelEncodeError::Encoding)?,
                    reason: f.reason.as_utf8().ok_or(ModelEncodeError::Encoding)?,
                    details: decode(&mapping(&f.details, p)?)?,
                    log_refs: decode(&log_refs(&f.log_refs, p)?)?,
                    created_at: convert(timestamp(f.created_at))?,
                })
            },
        )?)?,
        verification: d
            .verification
            .as_ref()
            .filter(|(d, _)| !matches!(d.node(d.root()), Some(Node::Null)))
            .map(|(d, body)| {
                Ok(VerificationDocument {
                    front_matter: decode(&mapping(d, p)?)?,
                    body_markdown: body.clone(),
                })
            })
            .transpose()?,
        decisions: decode(&array(
            d.decisions.iter().map(crate::hypothesis_wire::decision),
        )?)?,
    })
}
pub(crate) fn page(
    values: &[CaseDetail],
    next: Option<cannery_core::ids::ReviewCaseId>,
    p: ResponseContext,
) -> Result<Vec<u8>> {
    encode(&ReviewCasePage {
        items: decode(&array(values.iter().map(|v| case(v, p)))?)?,
        next_before: convert(next)?,
    })
}
fn attempt_ref(n: i32, s: Option<i32>) -> Option<String> {
    s.map(|s| format!("#{n}.{s}"))
}
pub(crate) struct AttentionDetail {
    pub counts: BTreeMap<String, i64>,
    pub reviews: Vec<PendingReview>,
    pub running_count: i64,
    pub running: Vec<RunningAttempt>,
    pub outcomes: Vec<Outcome>,
    pub failures: Vec<RecentFailure>,
    pub stalled_count: i64,
    pub stalled: Vec<StalledVerification>,
}
#[allow(
    clippy::too_many_lines,
    reason = "Preserve all six source models in declared field order"
)]
pub(crate) fn attention(d: &AttentionDetail) -> Result<Vec<u8>> {
    let counts = ["result", "failure"]
        .into_iter()
        .map(|kind| (kind.to_owned(), d.counts.get(kind).copied().unwrap_or(0)))
        .collect::<BTreeMap<_, _>>();

    encode(&AttentionOut {
        pending_counts: decode(&encode(&counts)?)?,
        pending_reviews: decode(&array(d.reviews.iter().map(|v| {
            encode(&AttentionReview {
                case_id: convert(v.case_id)?,
                kind: convert(v.kind.as_str())?,
                subject_revision: convert(v.subject_revision)?,
                opened_at: convert(timestamp(v.opened_at))?,
                hypothesis: convert(v.hypothesis_number)?,
                hypothesis_ref: convert(format!("#{}", v.hypothesis_number))?,
                title: v
                    .hypothesis_title
                    .as_utf8()
                    .ok_or(ModelEncodeError::Encoding)?,
                track: v.track_slug.as_utf8().ok_or(ModelEncodeError::Encoding)?,
                attempt_ref: convert(attempt_ref(v.hypothesis_number, v.attempt_sequence))?,
                verdict: (v.verdict.as_ref())
                    .map(|v| v.as_utf8().ok_or(ModelEncodeError::Encoding))
                    .transpose()?,
                failure_stage: convert(v.failure_stage.map(cannery_reviews::Stage::as_str))?,
                failure_code: (v.failure_code.as_ref())
                    .map(|v| v.as_utf8().ok_or(ModelEncodeError::Encoding))
                    .transpose()?,
                failure_reason: (v.failure_reason.as_ref())
                    .map(|v| v.as_utf8().ok_or(ModelEncodeError::Encoding))
                    .transpose()?,
                origin: convert(v.origin.as_str())?,
            })
        }))?)?,
        running_count: convert(d.running_count)?,
        running: decode(&array(d.running.iter().map(|v| {
            encode(&AttentionRunning {
                hypothesis: convert(v.hypothesis_number)?,
                hypothesis_ref: convert(format!("#{}", v.hypothesis_number))?,
                title: v
                    .hypothesis_title
                    .as_utf8()
                    .ok_or(ModelEncodeError::Encoding)?,
                track: v.track_slug.as_utf8().ok_or(ModelEncodeError::Encoding)?,
                attempt_ref: convert(format!("#{}.{}", v.hypothesis_number, v.attempt_sequence))?,
                state: convert(v.state.as_str())?,
                claimed_at: convert(timestamp(v.claimed_at))?,
            })
        }))?)?,
        recent_outcomes: decode(&array(d.outcomes.iter().map(|v| {
            encode(&AttentionOutcome {
                hypothesis: convert(v.hypothesis_number)?,
                hypothesis_ref: convert(format!("#{}", v.hypothesis_number))?,
                title: v
                    .hypothesis_title
                    .as_utf8()
                    .ok_or(ModelEncodeError::Encoding)?,
                hypothesis_state: convert(v.hypothesis_state.as_str())?,
                track: v.track_slug.as_utf8().ok_or(ModelEncodeError::Encoding)?,
                attempt_ref: convert(attempt_ref(v.hypothesis_number, v.attempt_sequence))?,
                action: convert(v.action.as_str())?,
                reason: v.reason.as_utf8().ok_or(ModelEncodeError::Encoding)?,
                decided_at: convert(timestamp(v.decided_at))?,
                origin: convert(v.origin.as_str())?,
            })
        }))?)?,
        recent_failures: decode(&array(d.failures.iter().map(|v| {
            encode(&AttentionFailure {
                hypothesis: convert(v.hypothesis_number)?,
                hypothesis_ref: convert(format!("#{}", v.hypothesis_number))?,
                title: v
                    .hypothesis_title
                    .as_utf8()
                    .ok_or(ModelEncodeError::Encoding)?,
                hypothesis_state: convert(v.hypothesis_state.as_str())?,
                track: v.track_slug.as_utf8().ok_or(ModelEncodeError::Encoding)?,
                attempt_ref: convert(format!("#{}.{}", v.hypothesis_number, v.attempt_sequence))?,
                stage: convert(v.stage.as_str())?,
                code: v.code.as_utf8().ok_or(ModelEncodeError::Encoding)?,
                reason: v.reason.as_utf8().ok_or(ModelEncodeError::Encoding)?,
                created_at: convert(timestamp(v.created_at))?,
                origin: convert(v.origin.as_str())?,
            })
        }))?)?,
        stalled_verification_count: convert(d.stalled_count)?,
        stalled_verifications: decode(&array(d.stalled.iter().map(|v| {
            let text = |value: &Option<String>| {
                value
                    .as_ref()
                    .map(|v| v.as_utf8().ok_or(ModelEncodeError::Encoding))
                    .transpose()
            };
            let verifier = text(&v.verifier)?;
            let revision = text(&v.revision)?;
            let waiting_for = match (&verifier, &revision) {
                (Some(verifier), Some(revision)) => {
                    format!("verifier {verifier} revision {revision}")
                }
                _ => String::from("an agent or a researcher"),
            };
            encode(&AttentionStalledVerification {
                hypothesis: convert(v.hypothesis_number)?,
                hypothesis_ref: convert(format!("#{}", v.hypothesis_number))?,
                title: v
                    .hypothesis_title
                    .as_utf8()
                    .ok_or(ModelEncodeError::Encoding)?,
                track: v.track_slug.as_utf8().ok_or(ModelEncodeError::Encoding)?,
                attempt_ref: convert(format!("#{}.{}", v.hypothesis_number, v.attempt_sequence))?,
                performer: v.performer.as_utf8().ok_or(ModelEncodeError::Encoding)?,
                waiting_since: convert(timestamp(v.waiting_since))?,
                message: convert(format!(
                    "verification of #{}.{} waits for {waiting_for}",
                    v.hypothesis_number, v.attempt_sequence
                ))?,
                verifier,
                revision,
            })
        }))?)?,
    })
}
fn log_refs(d: &Document, p: ResponseContext) -> Result<Vec<u8>> {
    let Some(Node::Array(v)) = d.node(d.root()) else {
        return Err(ModelEncodeError::InvalidNode);
    };
    array(
        v.iter()
            .map(|id| model::encode_model_mapping(d, *id, p.inferred_nesting_budget)),
    )
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
