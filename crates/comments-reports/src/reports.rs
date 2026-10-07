//! Latest claimed reports, per-stage evidence, result review IDs and imported history.
use crate::date::SourceDate;
use crate::{RepositoryError, integer::Integer, redacted, source, text};
use cannery_core::{
    ids::{AttemptId, ProjectId, ReviewCaseId, ServiceAccountId, UserId},
    json::{self, Document},
    timestamps::Timestamp,
};
use num_bigint::BigInt;
use sqlx::PgConnection;
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type)]
#[sqlx(transparent)]
pub struct EvidenceId(pub Uuid);
macro_rules! domain {
    ($name:ident {$($variant:ident=>$text:literal),+ $(,)?})=>{
        #[derive(Clone,Copy,Debug,Eq,Ord,PartialEq,PartialOrd)] pub enum $name {$($variant),+}
        impl $name {
            #[must_use] pub const fn as_str(self)-> &'static str { match self {$(Self::$variant=>$text),+} }
            fn parse(value:&str)->Result<Self,RepositoryError>{match value {$($text=>Ok(Self::$variant)),+, _=>Err(RepositoryError::CorruptData)}}
        }
    };
}
domain!(Stage {Agent=>"agent",Evaluator=>"evaluator",Tester=>"tester"});
domain!(Status {Completed=>"completed",Failed=>"failed"});
domain!(Origin {Live=>"live",Imported=>"imported"});
domain!(AttemptState {Claimed=>"claimed",Running=>"running",Submitted=>"submitted",Validating=>"validating",Testing=>"testing",Evaluating=>"evaluating",AwaitingHumanReview=>"awaiting_human_review",Promoted=>"promoted",Rejected=>"rejected",Inconclusive=>"inconclusive",Failed=>"failed",Cancelled=>"cancelled",Unreviewed=>"unreviewed"});
domain!(ImportedKind {Retrospective=>"retrospective"});
/// Calibrated source decoder profile; no universal native default is inferred.
#[derive(Clone, Copy, Debug)]
pub struct JsonContext {
    pub decode_nesting_budget: usize,
}
fn document(value: &str, context: JsonContext) -> Result<Document, RepositoryError> {
    json::decode_str(value, context.decode_nesting_budget).map_err(RepositoryError::JsonDecode)
}
pub struct ReportRow {
    pub id: EvidenceId,
    pub attempt_id: AttemptId,
    pub status: Status,
    /// SQL NULL (for a top-level array containing `report`) remains distinct from JSON null.
    pub report: Option<Document>,
    pub created_at: Timestamp,
    pub producer_user: Option<UserId>,
    pub producer_service: Option<ServiceAccountId>,
    pub hypothesis_number: i32,
    pub hypothesis_title: String,
    pub track_slug: String,
    pub attempt_sequence: i32,
    pub attempt_state: AttemptState,
    pub origin: Origin,
}
struct RawReport {
    id: EvidenceId,
    attempt_id: AttemptId,
    status: String,
    report: Option<String>,
    created_at: Timestamp,
    producer_user: Option<UserId>,
    producer_service: Option<ServiceAccountId>,
    hypothesis_number: i32,
    hypothesis_title: String,
    track_slug: String,
    attempt_sequence: i32,
    attempt_state: String,
    origin: String,
}
impl RawReport {
    fn decode(self, c: JsonContext) -> Result<ReportRow, RepositoryError> {
        Ok(ReportRow {
            id: self.id,
            attempt_id: self.attempt_id,
            status: Status::parse(&self.status)?,
            report: self.report.map(|s| document(&s, c)).transpose()?,
            created_at: self.created_at,
            producer_user: self.producer_user,
            producer_service: self.producer_service,
            hypothesis_number: self.hypothesis_number,
            hypothesis_title: self.hypothesis_title,
            track_slug: self.track_slug,
            attempt_sequence: self.attempt_sequence,
            attempt_state: AttemptState::parse(&self.attempt_state)?,
            origin: Origin::parse(&self.origin)?,
        })
    }
}
pub struct EvidenceRow {
    pub id: EvidenceId,
    pub stage: Stage,
    pub status: Status,
    pub revision: i32,
    pub content: Document,
    pub producer_user: Option<UserId>,
    pub producer_service: Option<ServiceAccountId>,
    pub created_at: Timestamp,
    pub origin: Origin,
    pub source_ref: Option<String>,
}
struct RawEvidence {
    id: EvidenceId,
    stage: String,
    status: String,
    revision: i32,
    content: String,
    producer_user: Option<UserId>,
    producer_service: Option<ServiceAccountId>,
    created_at: Timestamp,
    origin: String,
    source_ref: Option<String>,
}
impl RawEvidence {
    fn decode(self, c: JsonContext) -> Result<EvidenceRow, RepositoryError> {
        Ok(EvidenceRow {
            id: self.id,
            stage: Stage::parse(&self.stage)?,
            status: Status::parse(&self.status)?,
            revision: self.revision,
            content: document(&self.content, c)?,
            producer_user: self.producer_user,
            producer_service: self.producer_service,
            created_at: self.created_at,
            origin: Origin::parse(&self.origin)?,
            source_ref: self.source_ref,
        })
    }
}
pub struct ImportedReportRow {
    pub kind: ImportedKind,
    pub author: String,
    pub written_on: Option<SourceDate>,
    pub written_at: Option<Timestamp>,
    pub body_markdown: String,
    pub source_ref: String,
}
struct RawImported {
    kind: String,
    author: String,
    written_on: Option<SourceDate>,
    written_at: Option<Timestamp>,
    body_markdown: String,
    source_ref: String,
}
redacted!(ReportRow, EvidenceRow, ImportedReportRow);
/// Each attempt's latest agent sheet only when that latest sheet has a report.
/// # Errors
/// Returns sanitized database, argument encoding, domain or JSON decoding failures.
#[allow(clippy::too_many_arguments)]
pub async fn list_reports(
    conn: &mut PgConnection,
    project_id: ProjectId,
    number: Option<&BigInt>,
    track_slug: Option<&str>,
    before: Option<EvidenceId>,
    limit: Option<&BigInt>,
    context: JsonContext,
) -> Result<Vec<ReportRow>, RepositoryError> {
    if let Some(track) = track_slug {
        text(track)?;
    }
    let number = number.map(Integer::new).transpose()?;
    let limit = limit.map(Integer::new).transpose()?;
    source!(
        sqlx::query_file_as!(
            RawReport,
            "src/sql/list_reports.sql",
            project_id as _,
            number as _,
            track_slug,
            before as _,
            limit as _
        ),
        fetch_all,
        conn
    )?
    .into_iter()
    .map(|r| r.decode(context))
    .collect()
}
/// Highest revision for every stored stage, independently of time or status.
/// # Errors
/// Returns sanitized database, domain or JSON decoding failures.
pub async fn latest_evidence(
    conn: &mut PgConnection,
    attempt_id: AttemptId,
    context: JsonContext,
) -> Result<BTreeMap<Stage, EvidenceRow>, RepositoryError> {
    let rows = source!(
        sqlx::query_file_as!(RawEvidence, "src/sql/latest_evidence.sql", attempt_id as _),
        fetch_all,
        conn
    )?;
    rows.into_iter()
        .map(|r| r.decode(context).map(|r| (r.stage, r)))
        .collect()
}
/// Result cases only, ordered by opened instant and UUID.
/// # Errors
/// Returns sanitized database failures.
pub async fn result_case_ids(
    conn: &mut PgConnection,
    attempt_id: AttemptId,
) -> Result<Vec<ReviewCaseId>, RepositoryError> {
    let rows = source!(
        sqlx::query!(
            r#"SELECT id AS "id!: ReviewCaseId" FROM review_cases WHERE attempt_id=$1 AND kind='result' ORDER BY opened_at,id"#,
            attempt_id as _
        ),
        fetch_all,
        conn
    )?;
    Ok(rows.into_iter().map(|r| r.id).collect())
}
/// Immutable imported narrative, preserving whether the bundle stated a day or instant.
/// # Errors
/// Returns sanitized database or domain decoding failures.
pub async fn imported_report(
    conn: &mut PgConnection,
    attempt_id: AttemptId,
) -> Result<Option<ImportedReportRow>, RepositoryError> {
    let row = source!(
        sqlx::query_file_as!(RawImported, "src/sql/imported_report.sql", attempt_id as _),
        fetch_optional,
        conn
    )?;
    row.map(|r| {
        Ok(ImportedReportRow {
            kind: ImportedKind::parse(&r.kind)?,
            author: r.author,
            written_on: r.written_on,
            written_at: r.written_at,
            body_markdown: r.body_markdown,
            source_ref: r.source_ref,
        })
    })
    .transpose()
}
