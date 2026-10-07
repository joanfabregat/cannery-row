//! Report responses adapt domain data to the public serde DTOs.
use crate::{
    api_contract::{convert, decode, encode},
    api_models::{
        EvaluatorReport, Page_ReportSummary_UUID_, Producer, ReportOut, ReportSummary, TesterReport,
    },
};
use cannery_comments_reports::reports::{EvidenceRow, ImportedReportRow, ReportRow};
use cannery_core::json::{
    Document, Node, NodeId,
    model::{self, ModelEncodeError},
};
type Result<T> = std::result::Result<T, ModelEncodeError>;
#[derive(Clone, Copy)]
pub struct ResponseContext {
    pub inferred_nesting_budget: usize,
    pub representation_budget: usize,
}
fn timestamp(v: cannery_core::timestamps::Timestamp) -> String {
    crate::timestamps::public_timestamp(v)
}
fn producer(
    user: Option<cannery_core::ids::UserId>,
    service: Option<cannery_core::ids::ServiceAccountId>,
    imported: bool,
) -> Producer {
    Producer {
        kind: if imported {
            "import"
        } else if user.is_some() {
            "user"
        } else if service.is_some() {
            "service"
        } else {
            "builtin"
        }
        .to_owned(),
        id: if imported {
            None
        } else {
            user.map(|v| v.0)
                .or_else(|| service.map(|v| v.0))
                .map(|v| v.to_string())
        },
    }
}
fn object(d: &Document, id: NodeId) -> Result<()> {
    if matches!(d.node(id), Some(Node::Object(_))) {
        Ok(())
    } else {
        Err(ModelEncodeError::InvalidNode)
    }
}
pub(crate) fn field(d: &Document, key: &str) -> Result<Option<NodeId>> {
    object(d, d.root())?;
    Ok(d.field(d.root(), key))
}
fn mapping<T: serde::de::DeserializeOwned>(
    d: &Document,
    id: NodeId,
    p: ResponseContext,
) -> Result<T> {
    decode(&model::encode_model_mapping(
        d,
        id,
        p.inferred_nesting_budget,
    )?)
}
fn lists<T: serde::de::DeserializeOwned>(
    d: &Document,
    key: &str,
    p: ResponseContext,
) -> Result<Vec<T>> {
    match field(d, key)?.and_then(|id| d.node(id)) {
        Some(Node::Array(ids)) => ids.iter().map(|id| mapping(d, *id, p)).collect(),
        _ => Ok(Vec::new()),
    }
}
fn optional_text(d: &Document, id: Option<NodeId>) -> Result<Option<String>> {
    match id.and_then(|id| d.node(id)) {
        None | Some(Node::Null) => Ok(None),
        Some(Node::String(v)) => v.as_utf8().map(Some).ok_or(ModelEncodeError::Encoding),
        _ => Err(ModelEncodeError::InvalidNode),
    }
}
pub(crate) fn assessment(e: Option<&EvidenceRow>) -> Result<Option<NodeId>> {
    e.map(|e| field(&e.content, "assessment"))
        .transpose()
        .map(Option::flatten)
}
fn summary(row: &ReportRow, p: ResponseContext) -> Result<ReportSummary> {
    let d = row.report.as_ref().ok_or(ModelEncodeError::InvalidNode)?;
    object(d, d.root())?;
    let text = |key| {
        d.field(d.root(), key)
            .map(|id| cannery_core::text::str_value(d, id, p.representation_budget))
            .transpose()
            .map_err(|_| ModelEncodeError::InvalidNode)?
            .map(|text| text.as_utf8().ok_or(ModelEncodeError::Encoding))
            .transpose()
            .map(Option::unwrap_or_default)
    };
    Ok(ReportSummary {
        id: row.id.0.to_string(),
        attempt_ref: format!("#{}.{}", row.hypothesis_number, row.attempt_sequence),
        hypothesis: i64::from(row.hypothesis_number),
        hypothesis_title: convert(&row.hypothesis_title)?,
        track: convert(&row.track_slug)?,
        attempt_state: row.attempt_state.as_str().to_owned(),
        status: row.status.as_str().to_owned(),
        what_was_tried: text("what_was_tried")?,
        findings: text("findings")?,
        submitted_at: timestamp(row.created_at),
        author: producer(row.producer_user, row.producer_service, false),
        origin: convert(row.origin.as_str())?,
    })
}
pub(crate) fn page(rows: &[ReportRow], limit: usize, p: ResponseContext) -> Result<Vec<u8>> {
    encode(&Page_ReportSummary_UUID_ {
        items: rows
            .iter()
            .take(limit)
            .map(|row| summary(row, p))
            .collect::<Result<_>>()?,
        next_before: (rows.len() > limit).then(|| rows[limit - 1].id.0.to_string()),
    })
}
pub(crate) fn tester(row: Option<&EvidenceRow>, p: ResponseContext) -> Result<Vec<u8>> {
    let value = row
        .map(|row| {
            Ok(TesterReport {
                status: row.status.as_str().to_owned(),
                observations: optional_text(&row.content, field(&row.content, "observations")?)?,
                discrepancies: lists(&row.content, "discrepancies", p)?,
                measurements: lists(&row.content, "measurements", p)?,
                published_at: timestamp(row.created_at),
                source_ref: convert(&row.source_ref)?,
            })
        })
        .transpose()?;
    encode(&value)
}
pub(crate) fn evaluator(
    row: Option<&EvidenceRow>,
    assessment: Option<NodeId>,
    p: ResponseContext,
) -> Result<Vec<u8>> {
    let value = row
        .map(|row| {
            let d = &row.content;
            if let Some(id) = assessment {
                object(d, id)?;
            }
            let text = |key| optional_text(d, assessment.and_then(|id| d.field(id, key)));
            Ok(EvaluatorReport {
                status: row.status.as_str().to_owned(),
                verdict: text("verdict")?,
                reason: text("reason")?,
                policy_revision: text("policy_revision")?,
                gates: assessment_lists(d, assessment, "gates", p)?,
                comparisons: assessment_lists(d, assessment, "comparisons", p)?,
                producer: producer(
                    row.producer_user,
                    row.producer_service,
                    row.origin.as_str() == "imported",
                ),
                published_at: timestamp(row.created_at),
                source_ref: convert(&row.source_ref)?,
            })
        })
        .transpose()?;
    encode(&value)
}
#[derive(serde::Serialize)]
struct ImportedReport<'a> {
    kind: &'a str,
    author: &'a str,
    written_at: Option<String>,
    body_markdown: &'a str,
    origin: &'static str,
    source_ref: Option<&'a str>,
}
pub(crate) fn imported(row: Option<&ImportedReportRow>) -> Result<Vec<u8>> {
    match row {
        None => encode(&std::collections::BTreeMap::<String, serde_json::Value>::new()),
        Some(row) => encode(&ImportedReport {
            kind: row.kind.as_str(),
            author: &row.author,
            written_at: row
                .written_on
                .map(|v| v.to_string())
                .or_else(|| row.written_at.map(timestamp)),
            body_markdown: &row.body_markdown,
            origin: "imported",
            source_ref: Some(&row.source_ref),
        }),
    }
}
pub(crate) struct Detail<'a> {
    pub attempt: &'a cannery_attempts::model::Attempt,
    pub title: &'a str,
    pub sheet: Option<&'a EvidenceRow>,
    pub imported: Option<&'a ImportedReportRow>,
    pub tester: Vec<u8>,
    pub evaluator: Vec<u8>,
    pub decisions: &'a [cannery_hypotheses::repo::Decision],
    pub assets: &'a [cannery_attempts::model::Artifact],
}

pub(crate) fn detail(detail: &Detail<'_>, p: ResponseContext) -> Result<Vec<u8>> {
    let a = detail.attempt;
    let report = match detail.sheet {
        Some(sheet) => mapping(
            &sheet.content,
            field(&sheet.content, "report")?.ok_or(ModelEncodeError::InvalidNode)?,
            p,
        )?,
        None => decode(&imported(detail.imported)?)?,
    };
    let claimed_measurements = detail
        .sheet
        .map(|sheet| lists(&sheet.content, "measurements", p))
        .transpose()?
        .unwrap_or_default();
    encode(&ReportOut {
        id: detail.sheet.map_or(a.id.0, |sheet| sheet.id.0).to_string(),
        attempt_ref: format!("#{}.{}", a.hypothesis_number, a.sequence),
        hypothesis: i64::from(a.hypothesis_number),
        hypothesis_title: detail.title.to_owned(),
        track: convert(&a.track_slug)?,
        attempt_state: a.state.as_str().to_owned(),
        science_revision: i64::from(a.science_revision),
        status: detail
            .sheet
            .map_or(
                if a.state.as_str() == "failed" {
                    "failed"
                } else {
                    "completed"
                },
                |sheet| sheet.status.as_str(),
            )
            .to_owned(),
        report,
        claimed_measurements,
        origin: convert(a.origin.as_str())?,
        source_ref: convert(&a.source_ref)?,
        submitted_at: timestamp(detail.sheet.map_or(
            a.finished_at.or(a.started_at).unwrap_or(a.claimed_at),
            |sheet| sheet.created_at,
        )),
        author: detail.sheet.map_or_else(
            || producer(None, None, a.origin.as_str() == "imported"),
            |sheet| producer(sheet.producer_user, sheet.producer_service, false),
        ),
        tester: decode(&detail.tester)?,
        evaluation: decode(&detail.evaluator)?,
        decisions: detail
            .decisions
            .iter()
            .map(|d| decode(&crate::hypothesis_wire::decision(d)?))
            .collect::<Result<_>>()?,
        assets: detail
            .assets
            .iter()
            .map(crate::attempt_read_wire::artifact_model)
            .collect::<Result<_>>()?,
    })
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;

fn assessment_lists<T: serde::de::DeserializeOwned>(
    d: &Document,
    assessment: Option<NodeId>,
    key: &str,
    p: ResponseContext,
) -> Result<Vec<T>> {
    match assessment
        .and_then(|id| d.field(id, key))
        .and_then(|id| d.node(id))
    {
        Some(Node::Array(ids)) => ids.iter().map(|id| mapping(d, *id, p)).collect(),
        _ => Ok(Vec::new()),
    }
}
