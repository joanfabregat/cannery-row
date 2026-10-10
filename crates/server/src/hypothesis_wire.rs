//! Hypothesis response models validate mapping roots before transaction exit.
use cannery_core::{
    ids::ProjectId,
    json::{
        Document, Node,
        model::{self, ModelEncodeError},
    },
};
use cannery_hypotheses::repo::{Decision, Hypothesis, Link, ReviewCase, Revision};
use std::collections::BTreeSet;
type Result<T> = std::result::Result<T, ModelEncodeError>;
#[derive(Clone, Copy)]
pub struct ResponseContext {
    pub inferred_nesting_budget: usize,
}
pub(crate) struct Detail {
    pub hypothesis: Hypothesis,
    pub revision: Revision,
    pub project: String,
    pub relations: Vec<Link>,
    pub backlinks: Vec<Link>,
    pub cases: Vec<ReviewCase>,
    pub decisions: Vec<Decision>,
    pub readable: Option<BTreeSet<ProjectId>>,
}
fn mapping(d: &Document, nullable: bool) -> Result<()> {
    if matches!(d.node(d.root()), Some(Node::Object(_)))
        || nullable && matches!(d.node(d.root()), Some(Node::Null))
    {
        Ok(())
    } else {
        Err(ModelEncodeError::InvalidNode)
    }
}
pub(crate) fn validate_summary(h: &Hypothesis) -> Result<()> {
    if h.created_by_user.is_none() && h.created_by_service.is_none() {
        return Err(ModelEncodeError::InvalidNode);
    }
    if let Some(d) = &h.imported {
        mapping(d, true)?;
    }
    Ok(())
}
pub(crate) fn validate_revision(r: &Revision) -> Result<()> {
    if r.author_user.is_none() && r.author_service.is_none() {
        return Err(ModelEncodeError::InvalidNode);
    }
    mapping(&r.content, false)
}
impl Detail {
    pub(crate) fn validate(&self) -> Result<()> {
        validate_summary(&self.hypothesis)?;
        validate_revision(&self.revision)
    }
}

use crate::{
    api_contract::{convert, decode, encode},
    api_models::{
        Author, DecisionOut, HypothesisOut, HypothesisPage, HypothesisSummary, LinkOut,
        Page_RevisionOut_int_, RevisionOut, cannery_row__hypotheses__routes__ReviewCaseOut,
    },
};
fn document<T: serde::de::DeserializeOwned>(d: &Document, p: ResponseContext) -> Result<T> {
    decode(&model::encode_model_mapping(
        d,
        d.root(),
        p.inferred_nesting_budget,
    )?)
}
fn optional(
    d: Option<&Document>,
    p: ResponseContext,
) -> Result<Option<std::collections::BTreeMap<String, serde_json::Value>>> {
    d.filter(|d| !matches!(d.node(d.root()), Some(Node::Null)))
        .map(|d| document(d, p))
        .transpose()
}
fn timestamp(v: cannery_core::timestamps::Timestamp) -> String {
    crate::timestamps::public_timestamp(v)
}
fn author(
    user: Option<cannery_core::ids::UserId>,
    service: Option<cannery_core::ids::ServiceAccountId>,
) -> Result<Author> {
    if let Some(id) = user {
        Ok(Author {
            kind: "user".to_owned(),
            id: id.to_string(),
        })
    } else {
        Ok(Author {
            kind: "service".to_owned(),
            id: service.ok_or(ModelEncodeError::InvalidNode)?.to_string(),
        })
    }
}
fn summary(h: &Hypothesis, p: ResponseContext) -> Result<HypothesisSummary> {
    validate_summary(h)?;
    Ok(HypothesisSummary {
        number: i64::from(h.number),
        r#ref: format!("#{}", h.number),
        title: h.title.as_utf8().ok_or(ModelEncodeError::Encoding)?,
        track: h.track_slug.as_utf8().ok_or(ModelEncodeError::Encoding)?,
        mode: convert(h.track_mode.as_str())?,
        state: convert(h.state.as_str())?,
        revision: i64::from(h.revision),
        approved_revision: h.approved_revision.map(i64::from),
        created_by: author(h.created_by_user, h.created_by_service)?,
        created_at: timestamp(h.created_at),
        updated_at: timestamp(h.updated_at),
        approved_at: h.approved_at.map(timestamp),
        origin: convert(h.origin.as_str())?,
        source_ref: h
            .source_ref
            .as_ref()
            .map(|s| s.as_utf8().ok_or(ModelEncodeError::Encoding))
            .transpose()?,
        external_id: h
            .external_id
            .as_ref()
            .map(|s| s.as_utf8().ok_or(ModelEncodeError::Encoding))
            .transpose()?,
        imported: optional(h.imported.as_ref(), p)?,
    })
}
fn links(rows: &[Link], readable: Option<&BTreeSet<ProjectId>>) -> Result<Vec<LinkOut>> {
    rows.iter()
        .filter(|l| readable.is_none_or(|r| r.contains(&l.project_id)))
        .map(|l| {
            Ok(LinkOut {
                kind: match l.kind {
                    cannery_hypotheses::repo::LinkKind::Mention => "mention",
                    cannery_hypotheses::repo::LinkKind::Relation(k) => k.as_str(),
                }
                .to_owned(),
                project: l.project_slug.as_utf8().ok_or(ModelEncodeError::Encoding)?,
                number: i64::from(l.number),
                r#ref: format!(
                    "{}#{}",
                    l.project_slug.as_utf8().ok_or(ModelEncodeError::Encoding)?,
                    l.number
                ),
                title: l.title.as_utf8().ok_or(ModelEncodeError::Encoding)?,
                state: l.state.as_str().to_owned(),
            })
        })
        .collect()
}
fn decision_model(d: &Decision) -> Result<DecisionOut> {
    Ok(DecisionOut {
        id: d.id.0.to_string(),
        action: d.action.as_str().to_owned(),
        subject_revision: i64::from(d.subject_revision),
        reason: d.reason.as_utf8().ok_or(ModelEncodeError::Encoding)?,
        actor_user_id: d.actor_user_id.map(|id| id.to_string()),
        actor_service_id: d.actor_service_id.map(|id| id.to_string()),
        decider_revision: d.decider_revision.clone(),
        via_channel: d
            .via_channel
            .as_text()
            .as_utf8()
            .ok_or(ModelEncodeError::Encoding)?,
        via_client: d
            .via_client
            .as_ref()
            .map(|s| s.as_utf8().ok_or(ModelEncodeError::Encoding))
            .transpose()?,
        decided_at: timestamp(d.decided_at),
        supersedes: d.supersedes.map(|s| s.0.to_string()),
        origin: convert(d.origin.as_str())?,
        source_ref: d
            .source_ref
            .as_ref()
            .map(|s| s.as_utf8().ok_or(ModelEncodeError::Encoding))
            .transpose()?,
    })
}
pub(crate) fn decision(d: &Decision) -> Result<Vec<u8>> {
    encode(&decision_model(d)?)
}
pub(crate) fn detail(d: &Detail, p: ResponseContext) -> Result<Vec<u8>> {
    d.validate()?;
    let h = summary(&d.hypothesis, p)?;
    let reviews = d
        .cases
        .iter()
        .map(|c| {
            Ok(cannery_row__hypotheses__routes__ReviewCaseOut {
                id: c.id.to_string(),
                kind: convert(c.kind.as_str())?,
                subject_revision: i64::from(c.subject_revision),
                state: convert(c.state.as_str())?,
                opened_at: timestamp(c.opened_at),
                resolved_at: c.resolved_at.map(timestamp),
                origin: convert(c.origin.as_str())?,
                source_ref: c
                    .source_ref
                    .as_ref()
                    .map(|s| s.as_utf8().ok_or(ModelEncodeError::Encoding))
                    .transpose()?,
                decisions: d
                    .decisions
                    .iter()
                    .filter(|decision| decision.review_case_id == c.id)
                    .map(decision_model)
                    .collect::<Result<_>>()?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    encode(&HypothesisOut {
        number: h.number,
        r#ref: h.r#ref,
        title: h.title,
        track: h.track,
        mode: h.mode,
        state: h.state,
        revision: h.revision,
        approved_revision: h.approved_revision,
        created_by: h.created_by,
        created_at: h.created_at,
        updated_at: h.updated_at,
        approved_at: h.approved_at,
        origin: h.origin,
        source_ref: h.source_ref,
        external_id: h.external_id,
        imported: h.imported,
        id: d.hypothesis.id.to_string(),
        project: d.project.clone(),
        document: document(&d.revision.content, p)?,
        science_revision: i64::from(d.revision.science_revision),
        relations: links(&d.relations, d.readable.as_ref())?,
        backlinks: links(&d.backlinks, d.readable.as_ref())?,
        reviews,
    })
}
fn revision_model(r: &Revision, p: ResponseContext) -> Result<RevisionOut> {
    validate_revision(r)?;
    Ok(RevisionOut {
        revision: i64::from(r.revision),
        science_revision: i64::from(r.science_revision),
        author: author(r.author_user, r.author_service)?,
        via_channel: r
            .via_channel
            .as_text()
            .as_utf8()
            .ok_or(ModelEncodeError::Encoding)?,
        via_client: r
            .via_client
            .as_ref()
            .map(|s| s.as_utf8().ok_or(ModelEncodeError::Encoding))
            .transpose()?,
        created_at: timestamp(r.created_at),
        origin: convert(r.origin.as_str())?,
        document: document(&r.content, p)?,
    })
}
pub(crate) fn revision(r: &Revision, p: ResponseContext) -> Result<Vec<u8>> {
    encode(&revision_model(r, p)?)
}
pub(crate) fn summaries(
    rows: &[Hypothesis],
    next: Option<i32>,
    p: ResponseContext,
) -> Result<Vec<u8>> {
    encode(&HypothesisPage {
        items: rows.iter().map(|h| summary(h, p)).collect::<Result<_>>()?,
        next_before: next.map(i64::from),
    })
}
pub(crate) fn revisions(
    rows: &[Revision],
    next: Option<i32>,
    p: ResponseContext,
) -> Result<Vec<u8>> {
    encode(&Page_RevisionOut_int_ {
        items: rows
            .iter()
            .map(|r| revision_model(r, p))
            .collect::<Result<_>>()?,
        next_before: next.map(i64::from),
    })
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
