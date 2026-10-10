//! Source-compatible SQL operations; errors never include parameter values.
use cannery_core::{
    ids::{AttemptId, HypothesisId, ProjectId, ReviewCaseId, ServiceAccountId, TrackId, UserId},
    json::{self, DecodeError, Document, EncodeError},
    principal::{Channel, UserPrincipal},
    timestamps::Timestamp,
};
use num_bigint::BigInt;
use sqlx::PgConnection;
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};
use uuid::Uuid;

macro_rules! checked_source {
 (as $model:ident, $sql:literal, ($($args:tt)*), $fetch:ident, $executor:expr) => {sqlx::query_as!($model,$sql,$($args)*).$fetch($executor).await};
 (scalar $_model:ident, $sql:literal, ($($args:tt)*), $fetch:ident, $executor:expr) => {sqlx::query_scalar!($sql,$($args)*).$fetch($executor).await};
 (exec $_model:ident, $sql:literal, ($($args:tt)*), $fetch:ident, $executor:expr) => {sqlx::query!($sql,$($args)*).$fetch($executor).await};
}
macro_rules! checked_query {
 ($kind:ident $model:ident,$sql:literal,($($args:tt)*),$fetch:ident,$connection:expr) => {{
 checked_source!($kind $model,$sql,($($args)*),$fetch,$connection)
 }};
}
/// Callers select both calibrated JSON depth budgets explicitly.
#[derive(Clone, Copy, Debug)]
pub struct JsonContext {
    pub encode_nesting_budget: usize,
    pub decode_nesting_budget: usize,
}
/// The database has no channel CHECK; recovery rows may carry historical labels.
pub enum ViaChannel {
    Known(Channel),
    Other(String),
}
impl ViaChannel {
    #[must_use]
    pub fn as_text(&self) -> String {
        match self {
            Self::Known(value) => String::from(channel(*value)),
            Self::Other(value) => value.clone(),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct DecisionId(pub Uuid);
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct CommentId(pub Uuid);
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct EvidenceId(pub Uuid);
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum HypothesisState {
    Queued,
    Active,
    Documenting,
    Deciding,
    Promoted,
    Rejected,
    Inconclusive,
    Failed,
    Cancelled,
}
impl HypothesisState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Active => "active",
            Self::Documenting => "documenting",
            Self::Deciding => "deciding",
            Self::Promoted => "promoted",
            Self::Rejected => "rejected",
            Self::Inconclusive => "inconclusive",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}
impl TryFrom<&str> for HypothesisState {
    type Error = HypothesisError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "queued" => Ok(Self::Queued),
            "active" => Ok(Self::Active),
            "documenting" => Ok(Self::Documenting),
            "deciding" => Ok(Self::Deciding),
            "promoted" => Ok(Self::Promoted),
            "rejected" => Ok(Self::Rejected),
            "inconclusive" => Ok(Self::Inconclusive),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(HypothesisError::CorruptData),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum TrackMode {
    Agent,
    Workflow,
}
impl TrackMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Workflow => "workflow",
        }
    }
}
impl TryFrom<&str> for TrackMode {
    type Error = HypothesisError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "agent" => Ok(Self::Agent),
            "workflow" => Ok(Self::Workflow),
            _ => Err(HypothesisError::CorruptData),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Origin {
    Live,
    Imported,
}
impl Origin {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Imported => "imported",
        }
    }
}
impl TryFrom<&str> for Origin {
    type Error = HypothesisError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "live" => Ok(Self::Live),
            "imported" => Ok(Self::Imported),
            _ => Err(HypothesisError::CorruptData),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum RelationKind {
    DerivedFrom,
    RelatedTo,
    Supersedes,
}
impl RelationKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DerivedFrom => "derived_from",
            Self::Supersedes => "supersedes",
            Self::RelatedTo => "related_to",
        }
    }
}
impl TryFrom<&str> for RelationKind {
    type Error = HypothesisError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "derived_from" => Ok(Self::DerivedFrom),
            "supersedes" => Ok(Self::Supersedes),
            "related_to" => Ok(Self::RelatedTo),
            _ => Err(HypothesisError::CorruptData),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum CaseKind {
    Decision,
    Failure,
}
impl CaseKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Decision => "decision",
            Self::Failure => "failure",
        }
    }
}
impl TryFrom<&str> for CaseKind {
    type Error = HypothesisError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "decision" => Ok(Self::Decision),
            "failure" => Ok(Self::Failure),
            _ => Err(HypothesisError::CorruptData),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum CaseState {
    Pending,
    Resolved,
}
impl CaseState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Resolved => "resolved",
        }
    }
}
impl TryFrom<&str> for CaseState {
    type Error = HypothesisError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "pending" => Ok(Self::Pending),
            "resolved" => Ok(Self::Resolved),
            _ => Err(HypothesisError::CorruptData),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum DecisionAction {
    Promote,
    Reject,
    Inconclusive,
    Retry,
    Failed,
    Stop,
}
impl DecisionAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Promote => "promote",
            Self::Reject => "reject",
            Self::Inconclusive => "inconclusive",
            Self::Retry => "retry",
            Self::Failed => "failed",
            Self::Stop => "stop",
        }
    }
}
impl TryFrom<&str> for DecisionAction {
    type Error = HypothesisError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "promote" => Ok(Self::Promote),
            "reject" => Ok(Self::Reject),
            "inconclusive" => Ok(Self::Inconclusive),
            "retry" => Ok(Self::Retry),
            "failed" => Ok(Self::Failed),
            "stop" => Ok(Self::Stop),
            _ => Err(HypothesisError::CorruptData),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkKind {
    Relation(RelationKind),
    Mention,
}
impl TryFrom<&str> for LinkKind {
    type Error = HypothesisError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        if s == "mention" {
            Ok(Self::Mention)
        } else {
            RelationKind::try_from(s).map(Self::Relation)
        }
    }
}
/// A mention source carries the identifier of its own entity.
#[derive(Clone, Copy, Debug)]
pub enum MentionSource {
    Hypothesis(HypothesisId),
    Attempt(AttemptId),
    Comment(CommentId),
    Report(EvidenceId),
}
impl MentionSource {
    fn parts(self) -> (&'static str, Uuid) {
        match self {
            Self::Hypothesis(id) => ("hypothesis", id.0),
            Self::Attempt(id) => ("attempt", id.0),
            Self::Comment(id) => ("comment", id.0),
            Self::Report(id) => ("report", id.0),
        }
    }
}
pub struct Hypothesis {
    pub id: HypothesisId,
    pub project_id: ProjectId,
    pub number: i32,
    pub track_id: TrackId,
    pub track_slug: String,
    pub track_mode: TrackMode,
    pub state: HypothesisState,
    pub revision: i32,
    pub approved_revision: Option<i32>,
    pub title: String,
    pub created_by_user: Option<UserId>,
    pub created_by_service: Option<ServiceAccountId>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub approved_at: Option<Timestamp>,
    pub origin: Origin,
    pub source_ref: Option<String>,
    pub external_id: Option<String>,
    pub imported: Option<Document>,
}
impl fmt::Debug for Hypothesis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Hypothesis([redacted])")
    }
}
struct RawRef {
    project_id: ProjectId,
    number: i32,
    id: HypothesisId,
}
struct RawHypothesis {
    id: HypothesisId,
    project_id: ProjectId,
    number: i32,
    track_id: TrackId,
    track_slug: String,
    track_mode: String,
    state: String,
    revision: i32,
    approved_revision: Option<i32>,
    title: String,
    created_by_user: Option<UserId>,
    created_by_service: Option<ServiceAccountId>,
    created_at: Timestamp,
    updated_at: Timestamp,
    approved_at: Option<Timestamp>,
    origin: String,
    source_ref: Option<String>,
    external_id: Option<String>,
    imported: Option<String>,
}
fn decode_hypothesis(
    r: &RawHypothesis,
    context: JsonContext,
) -> Result<Hypothesis, HypothesisError> {
    Ok(Hypothesis {
        id: r.id,
        project_id: r.project_id,
        number: r.number,
        track_id: r.track_id,
        track_slug: String::from(&r.track_slug),
        track_mode: TrackMode::try_from(r.track_mode.as_str())?,
        state: HypothesisState::try_from(r.state.as_str())?,
        revision: r.revision,
        approved_revision: r.approved_revision,
        title: String::from(&r.title),
        created_by_user: r.created_by_user,
        created_by_service: r.created_by_service,
        created_at: r.created_at,
        updated_at: r.updated_at,
        approved_at: r.approved_at,
        origin: Origin::try_from(r.origin.as_str())?,
        source_ref: r.source_ref.as_deref().map(String::from),
        external_id: r.external_id.as_deref().map(String::from),
        imported: decode_optional_json(r.imported.as_deref(), context)?,
    })
}
fn decode_optional_json(
    value: Option<&str>,
    context: JsonContext,
) -> Result<Option<Document>, HypothesisError> {
    value
        .map(|v| json::decode_str(v, context.decode_nesting_budget))
        .transpose()
        .map_err(Into::into)
}
pub struct Revision {
    pub revision: i32,
    pub content: Document,
    pub science_revision: i32,
    pub author_user: Option<UserId>,
    pub author_service: Option<ServiceAccountId>,
    pub via_channel: ViaChannel,
    pub via_client: Option<String>,
    pub created_at: Timestamp,
    pub origin: Origin,
}
impl fmt::Debug for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Revision([redacted])")
    }
}
struct RawRevision {
    revision: i32,
    content: String,
    science_revision: i32,
    author_user: Option<UserId>,
    author_service: Option<ServiceAccountId>,
    via_channel: String,
    via_client: Option<String>,
    created_at: Timestamp,
    origin: String,
}
fn decode_revision(r: &RawRevision, context: JsonContext) -> Result<Revision, HypothesisError> {
    Ok(Revision {
        revision: r.revision,
        content: json::decode_str(&r.content, context.decode_nesting_budget)?,
        science_revision: r.science_revision,
        author_user: r.author_user,
        author_service: r.author_service,
        via_channel: decode_channel(&r.via_channel),
        via_client: r.via_client.as_deref().map(String::from),
        created_at: r.created_at,
        origin: Origin::try_from(r.origin.as_str())?,
    })
}
pub struct Link {
    pub kind: LinkKind,
    pub hypothesis_id: HypothesisId,
    pub project_id: ProjectId,
    pub project_slug: String,
    pub number: i32,
    pub title: String,
    pub state: HypothesisState,
}
impl fmt::Debug for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Link([redacted])")
    }
}
struct RawLink {
    kind: String,
    hypothesis_id: HypothesisId,
    project_id: ProjectId,
    project_slug: String,
    number: i32,
    title: String,
    state: String,
}
fn decode_link(r: &RawLink, _context: JsonContext) -> Result<Link, HypothesisError> {
    Ok(Link {
        kind: LinkKind::try_from(r.kind.as_str())?,
        hypothesis_id: r.hypothesis_id,
        project_id: r.project_id,
        project_slug: String::from(&r.project_slug),
        number: r.number,
        title: String::from(&r.title),
        state: HypothesisState::try_from(r.state.as_str())?,
    })
}
pub struct ReviewCase {
    pub id: ReviewCaseId,
    pub kind: CaseKind,
    pub subject_revision: i32,
    pub state: CaseState,
    pub opened_at: Timestamp,
    pub resolved_at: Option<Timestamp>,
    pub origin: Origin,
    pub source_ref: Option<String>,
}
impl fmt::Debug for ReviewCase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ReviewCase([redacted])")
    }
}
struct RawReviewCase {
    id: ReviewCaseId,
    kind: String,
    subject_revision: i32,
    state: String,
    opened_at: Timestamp,
    resolved_at: Option<Timestamp>,
    origin: String,
    source_ref: Option<String>,
}
fn decode_reviewcase(
    r: &RawReviewCase,
    _context: JsonContext,
) -> Result<ReviewCase, HypothesisError> {
    Ok(ReviewCase {
        id: r.id,
        kind: CaseKind::try_from(r.kind.as_str())?,
        subject_revision: r.subject_revision,
        state: CaseState::try_from(r.state.as_str())?,
        opened_at: r.opened_at,
        resolved_at: r.resolved_at,
        origin: Origin::try_from(r.origin.as_str())?,
        source_ref: r.source_ref.as_deref().map(String::from),
    })
}
pub struct Decision {
    pub id: DecisionId,
    pub review_case_id: ReviewCaseId,
    pub action: DecisionAction,
    pub subject_revision: i32,
    pub reason: String,
    /// The researcher who decided; absent for an automatic decision.
    pub actor_user_id: Option<UserId>,
    /// The decider service account of an automatic decision, and the
    /// revision of its step.
    pub actor_service_id: Option<ServiceAccountId>,
    pub decider_revision: Option<String>,
    pub via_channel: ViaChannel,
    pub via_client: Option<String>,
    pub decided_at: Timestamp,
    pub supersedes: Option<DecisionId>,
    pub origin: Origin,
    pub source_ref: Option<String>,
    /// The decision document of a decision case: its front matter and SHA-256;
    /// the reason is its body. Absent for failure actions and earlier decisions.
    pub document: Option<(Document, String)>,
}
impl fmt::Debug for Decision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Decision([redacted])")
    }
}
struct RawDecision {
    id: DecisionId,
    review_case_id: ReviewCaseId,
    action: String,
    subject_revision: i32,
    reason: String,
    actor_user_id: Option<UserId>,
    actor_service_id: Option<ServiceAccountId>,
    decider_revision: Option<String>,
    via_channel: String,
    via_client: Option<String>,
    decided_at: Timestamp,
    supersedes: Option<DecisionId>,
    origin: String,
    source_ref: Option<String>,
    front_matter: Option<String>,
    sha256: Option<String>,
}
fn decode_decision(r: &RawDecision, context: JsonContext) -> Result<Decision, HypothesisError> {
    Ok(Decision {
        id: r.id,
        review_case_id: r.review_case_id,
        action: DecisionAction::try_from(r.action.as_str())?,
        subject_revision: r.subject_revision,
        reason: String::from(&r.reason),
        actor_user_id: r.actor_user_id,
        actor_service_id: r.actor_service_id,
        decider_revision: r.decider_revision.clone(),
        via_channel: decode_channel(&r.via_channel),
        via_client: r.via_client.as_deref().map(String::from),
        decided_at: r.decided_at,
        supersedes: r.supersedes,
        origin: Origin::try_from(r.origin.as_str())?,
        source_ref: r.source_ref.as_deref().map(String::from),
        document: match (
            decode_optional_json(r.front_matter.as_deref(), context)?,
            &r.sha256,
        ) {
            (Some(front_matter), Some(sha256)) => Some((front_matter, sha256.clone())),
            (None, None) => None,
            _ => return Err(HypothesisError::CorruptData),
        },
    })
}
/// Sanitized failures preserve server SQLSTATE without retaining driver messages.
pub enum HypothesisError {
    Database { sqlstate: Option<String> },
    Encode(EncodeError),
    Decode(DecodeError),
    TextEncoding,
    TextNul,
    IntegerBinding,
    IntegerArrayRendering,
    CorruptData,
    Invariant,
}
impl HypothesisError {
    #[must_use]
    pub fn sqlstate(&self) -> Option<&str> {
        if let Self::Database { sqlstate } = self {
            sqlstate.as_deref()
        } else {
            None
        }
    }
}
impl From<sqlx::Error> for HypothesisError {
    fn from(e: sqlx::Error) -> Self {
        Self::Database {
            sqlstate: e
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .filter(|s| {
                    s.len() == 5
                        && s.bytes()
                            .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
                })
                .map(std::borrow::Cow::into_owned),
        }
    }
}
impl From<EncodeError> for HypothesisError {
    fn from(e: EncodeError) -> Self {
        Self::Encode(e)
    }
}
impl From<DecodeError> for HypothesisError {
    fn from(e: DecodeError) -> Self {
        Self::Decode(e)
    }
}
impl fmt::Display for HypothesisError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Database { .. } => "hypothesis database operation failed",
            Self::Encode(_) => "hypothesis JSON encoding failed",
            Self::Decode(_) => "hypothesis JSON decoding failed",
            Self::TextEncoding => "hypothesis text encoding failed",
            Self::TextNul => "hypothesis text contains NUL",
            Self::IntegerBinding => "hypothesis integer adaptation failed",
            Self::IntegerArrayRendering => "hypothesis integer array rendering failed",
            Self::CorruptData => "invalid persisted hypothesis data",
            Self::Invariant => "required hypothesis row absent",
        })
    }
}
impl fmt::Debug for HypothesisError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl Error for HypothesisError {}
fn text(v: &String) -> Result<String, HypothesisError> {
    let s = v.as_utf8().ok_or(HypothesisError::TextEncoding)?;
    if s.contains('\0') {
        Err(HypothesisError::TextNul)
    } else {
        Ok(s)
    }
}
fn client(v: Option<&str>) -> Result<Option<String>, HypothesisError> {
    v.map(|v| text(&String::from(v))).transpose()
}
fn channel(c: Channel) -> &'static str {
    match c {
        Channel::Ui => "ui",
        Channel::Api => "api",
        Channel::Mcp => "mcp",
        Channel::Cli => "cli",
        Channel::System => "system",
    }
}
fn decode_channel(s: &str) -> ViaChannel {
    let known = match s {
        "ui" => Some(Channel::Ui),
        "api" => Some(Channel::Api),
        "mcp" => Some(Channel::Mcp),
        "cli" => Some(Channel::Cli),
        "system" => Some(Channel::System),
        _ => None,
    };
    known.map_or_else(|| ViaChannel::Other(String::from(s)), ViaChannel::Known)
}
type Integer = i64;
fn integer(value: &BigInt) -> Result<Integer, HypothesisError> {
    i64::try_from(value).map_err(|_| HypothesisError::IntegerBinding)
}
/// All transaction boundaries remain owned by the caller.
pub struct RecordDecision<'a> {
    pub case_id: ReviewCaseId,
    pub action: DecisionAction,
    pub subject_revision: &'a BigInt,
    pub reason: &'a String,
    pub principal: &'a UserPrincipal,
    pub supersedes: Option<DecisionId>,
    /// A decision document's front matter as JSON text, and its SHA-256.
    pub document: Option<(&'a str, &'a str)>,
}
pub struct ListHypotheses<'a> {
    pub states: Option<&'a [HypothesisState]>,
    pub track_slug: Option<&'a String>,
    pub before: Option<&'a BigInt>,
    pub limit: &'a BigInt,
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn next_number(
    connection: &mut PgConnection,
    project: ProjectId,
) -> Result<i32, HypothesisError> {
    let result = next_number_query(Some(project))
        .fetch_optional(connection)
        .await;
    result?.flatten().ok_or(HypothesisError::Invariant)
}
pub(crate) fn next_number_query(
    project: Option<ProjectId>,
) -> sqlx::query::QueryScalar<'static, sqlx::Postgres, Option<i32>, sqlx::postgres::PgArguments> {
    sqlx::query_scalar!(
        "UPDATE projects SET next_hypothesis_number=next_hypothesis_number+1 WHERE id=$1 RETURNING next_hypothesis_number-1",
        project as Option<ProjectId>
    )
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn get_hypothesis(
    connection: &mut PgConnection,
    project: ProjectId,
    number: &BigInt,
    lock: bool,
    context: JsonContext,
) -> Result<Option<Hypothesis>, HypothesisError> {
    let number = integer(number)?;
    if lock {
        let id = checked_query!(scalar Scalar, r#"SELECT id AS "id!: _" FROM hypotheses WHERE project_id=$1 AND number=$2 FOR UPDATE"#, (project as ProjectId,number as _), fetch_optional, &mut *connection)?;
        return match id {
            Some(id) => get_hypothesis_by_id(connection, id, context).await,
            None => Ok(None),
        };
    }
    let row = checked_query!(as RawHypothesis, r#"SELECT h.id AS "id!: _", h.project_id AS "project_id!: _", h.number AS "number!", h.track_id AS "track_id!: _", t.slug AS "track_slug", t.mode AS "track_mode", h.state AS "state!", h.revision AS "revision", h.approved_revision AS "approved_revision", h.title AS "title!", h.created_by_user AS "created_by_user: _", h.created_by_service AS "created_by_service: _", h.created_at AS "created_at!: _", h.updated_at AS "updated_at!: _", h.approved_at AS "approved_at: _", h.origin AS "origin", h.source_ref AS "source_ref", h.external_id AS "external_id", h.imported::text AS "imported" FROM hypotheses h JOIN tracks t ON t.id=h.track_id WHERE h.project_id=$1 AND h.number=$2"#, (project as ProjectId,number as _), fetch_optional, connection)?;
    row.map(|r| decode_hypothesis(&r, context)).transpose()
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn get_hypothesis_by_id(
    connection: &mut PgConnection,
    id: HypothesisId,
    context: JsonContext,
) -> Result<Option<Hypothesis>, HypothesisError> {
    let row = checked_query!(as RawHypothesis, r#"SELECT h.id AS "id!: _", h.project_id AS "project_id!: _", h.number AS "number!", h.track_id AS "track_id!: _", t.slug AS "track_slug", t.mode AS "track_mode", h.state AS "state!", h.revision AS "revision", h.approved_revision AS "approved_revision", h.title AS "title!", h.created_by_user AS "created_by_user: _", h.created_by_service AS "created_by_service: _", h.created_at AS "created_at!: _", h.updated_at AS "updated_at!: _", h.approved_at AS "approved_at: _", h.origin AS "origin", h.source_ref AS "source_ref", h.external_id AS "external_id", h.imported::text AS "imported" FROM hypotheses h JOIN tracks t ON t.id=h.track_id WHERE h.id=$1"#, (id as HypothesisId), fetch_optional, connection)?;
    row.map(|r| decode_hypothesis(&r, context)).transpose()
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn list_hypotheses(
    connection: &mut PgConnection,
    project: ProjectId,
    input: ListHypotheses<'_>,
    context: JsonContext,
) -> Result<Vec<Hypothesis>, HypothesisError> {
    let ListHypotheses {
        states,
        track_slug,
        before,
        limit,
    } = input;
    let states = states.map(|s| s.iter().map(|s| s.as_str().to_owned()).collect::<Vec<_>>());
    let track = track_slug.map(text).transpose()?;
    let before = before.map(integer).transpose()?;
    let limit = integer(limit)?;
    let rows = checked_query!(as RawHypothesis, r#"SELECT h.id AS "id!: _", h.project_id AS "project_id!: _", h.number AS "number!", h.track_id AS "track_id!: _", t.slug AS "track_slug", t.mode AS "track_mode", h.state AS "state!", h.revision AS "revision", h.approved_revision AS "approved_revision", h.title AS "title!", h.created_by_user AS "created_by_user: _", h.created_by_service AS "created_by_service: _", h.created_at AS "created_at!: _", h.updated_at AS "updated_at!: _", h.approved_at AS "approved_at: _", h.origin AS "origin", h.source_ref AS "source_ref", h.external_id AS "external_id", h.imported::text AS "imported" FROM hypotheses h JOIN tracks t ON t.id=h.track_id WHERE h.project_id=$1 AND ($2::text[] IS NULL OR h.state=ANY($2)) AND ($3::text IS NULL OR t.slug=$3) AND ($4::bigint IS NULL OR h.number<$4) ORDER BY h.number DESC LIMIT $5::bigint"#, (project as ProjectId,states.as_deref(),track,before as _,limit as _), fetch_all, connection)?;
    rows.into_iter()
        .map(|r| decode_hypothesis(&r, context))
        .collect()
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn set_state(
    connection: &mut PgConnection,
    id: HypothesisId,
    state: HypothesisState,
    approved_revision: Option<&BigInt>,
) -> Result<(), HypothesisError> {
    let revision = approved_revision.map(integer).transpose()?;
    checked_query!(exec Execute, "UPDATE hypotheses SET state=$1,updated_at=now(),approved_revision=coalesce($2,approved_revision),approved_at=CASE WHEN $2::bigint IS NULL THEN approved_at ELSE now() END WHERE id=$3", (state.as_str(),revision as _,id as HypothesisId), execute, connection)?;
    Ok(())
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn get_revision(
    connection: &mut PgConnection,
    id: HypothesisId,
    revision: &BigInt,
    context: JsonContext,
) -> Result<Option<Revision>, HypothesisError> {
    let revision = integer(revision)?;
    let row = checked_query!(as RawRevision, r#"SELECT revision AS "revision", content::text AS "content!", science_revision AS "science_revision", author_user AS "author_user: _", author_service AS "author_service: _", via_channel AS "via_channel", via_client AS "via_client", created_at AS "created_at!: _", origin AS "origin" FROM hypothesis_revisions WHERE hypothesis_id=$1 AND revision=$2"#, (id as HypothesisId,revision as _), fetch_optional, connection)?;
    row.map(|r| decode_revision(&r, context)).transpose()
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn list_revisions(
    connection: &mut PgConnection,
    id: HypothesisId,
    after: Option<&BigInt>,
    limit: Option<&BigInt>,
    context: JsonContext,
) -> Result<Vec<Revision>, HypothesisError> {
    let after = after.map(integer).transpose()?;
    let limit = limit.map(integer).transpose()?;
    let rows = checked_query!(as RawRevision, r#"SELECT revision AS "revision", content::text AS "content!", science_revision AS "science_revision", author_user AS "author_user: _", author_service AS "author_service: _", via_channel AS "via_channel", via_client AS "via_client", created_at AS "created_at!: _", origin AS "origin" FROM hypothesis_revisions WHERE hypothesis_id=$1 AND ($2::bigint IS NULL OR revision>$2) ORDER BY revision LIMIT $3::bigint"#, (id as HypothesisId,after as _,limit as _), fetch_all, connection)?;
    rows.into_iter()
        .map(|r| decode_revision(&r, context))
        .collect()
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn resolve_refs(
    connection: &mut PgConnection,
    refs: impl IntoIterator<Item = (ProjectId, BigInt)>,
) -> Result<BTreeMap<(ProjectId, i32), HypothesisId>, HypothesisError> {
    let pairs = refs.into_iter().collect::<BTreeSet<_>>();
    if pairs.is_empty() {
        return Ok(BTreeMap::new());
    }
    let projects = pairs.iter().map(|(p, _)| p.0).collect::<Vec<_>>();
    let values = pairs.iter().map(|(_, n)| n).collect::<Vec<_>>();
    let numbers = integer_array(&values)?;
    let rows = checked_query!(as RawRef, r#"SELECT h.project_id AS "project_id!: _",h.number,h.id AS "id!: _" FROM hypotheses h JOIN unnest($1::uuid[],$2::bigint[]) AS r(project_id,number) ON r.project_id=h.project_id AND r.number=h.number"#, (&projects as _,numbers as _), fetch_all, connection)?;
    Ok(rows
        .into_iter()
        .map(|r| ((r.project_id, r.number), r.id))
        .collect())
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn replace_relations(
    connection: &mut PgConnection,
    id: HypothesisId,
    relations: impl IntoIterator<Item = (RelationKind, HypothesisId)>,
) -> Result<(), HypothesisError> {
    checked_query!(exec Execute, "DELETE FROM hypothesis_relations WHERE hypothesis_id=$1", (id as HypothesisId), execute, &mut *connection)?;
    let unique = relations.into_iter().collect::<BTreeSet<_>>();
    if !unique.is_empty() {
        let kinds = unique
            .iter()
            .map(|(k, _)| k.as_str().to_owned())
            .collect::<Vec<_>>();
        let targets = unique.iter().map(|(_, id)| id.0).collect::<Vec<_>>();
        checked_query!(exec Execute, "INSERT INTO hypothesis_relations(hypothesis_id,kind,target_id) SELECT $1,kind,target FROM unnest($2::text[],$3::uuid[]) AS r(kind,target)", (id as HypothesisId,&kinds,&targets as _), execute, connection)?;
    }
    Ok(())
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn replace_mentions(
    connection: &mut PgConnection,
    source: MentionSource,
    targets: impl IntoIterator<Item = HypothesisId>,
) -> Result<(), HypothesisError> {
    let (kind, id) = source.parts();
    checked_query!(exec Execute, "DELETE FROM mentions WHERE source_type=$1 AND source_id=$2", (kind,id as _), execute, &mut *connection)?;
    let unique = targets.into_iter().collect::<BTreeSet<_>>();
    if !unique.is_empty() {
        let targets = unique.into_iter().map(|id| id.0).collect::<Vec<_>>();
        checked_query!(exec Execute, "INSERT INTO mentions(source_type,source_id,target_id) SELECT $1,$2,target FROM unnest($3::uuid[]) AS m(target)", (kind,id as _,&targets as _), execute, connection)?;
    }
    Ok(())
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn outgoing_relations(
    connection: &mut PgConnection,
    id: HypothesisId,
    context: JsonContext,
) -> Result<Vec<Link>, HypothesisError> {
    let rows = checked_query!(as RawLink, r#"SELECT r.kind AS "kind!", h.id AS "hypothesis_id!: _", h.project_id AS "project_id!: _", p.slug AS "project_slug!", h.number AS "number!", h.title AS "title!", h.state AS "state!" FROM hypothesis_relations r JOIN hypotheses h ON h.id=r.target_id JOIN projects p ON p.id=h.project_id WHERE r.hypothesis_id=$1 ORDER BY r.kind,p.slug,h.number"#, (id as HypothesisId), fetch_all, connection)?;
    rows.into_iter().map(|r| decode_link(&r, context)).collect()
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn backlinks(
    connection: &mut PgConnection,
    id: HypothesisId,
    context: JsonContext,
) -> Result<Vec<Link>, HypothesisError> {
    let rows = checked_query!(as RawLink, r#"SELECT r.kind AS "kind!", h.id AS "hypothesis_id!: _", h.project_id AS "project_id!: _", p.slug AS "project_slug!", h.number AS "number!", h.title AS "title!", h.state AS "state!" FROM hypothesis_relations r JOIN hypotheses h ON h.id=r.hypothesis_id JOIN projects p ON p.id=h.project_id WHERE r.target_id=$1 UNION SELECT 'mention' AS "kind!", h.id AS "hypothesis_id!: _", h.project_id AS "project_id!: _", p.slug AS "project_slug!", h.number AS "number!", h.title AS "title!", h.state AS "state!" FROM mentions m JOIN LATERAL (SELECT m.source_id AS hypothesis_id WHERE m.source_type='hypothesis' UNION ALL SELECT c.hypothesis_id FROM comments c WHERE m.source_type='comment' AND c.id=m.source_id UNION ALL SELECT a.hypothesis_id FROM phase_outputs e JOIN attempts a ON a.id=e.attempt_id WHERE m.source_type='report' AND e.id=m.source_id) s ON true JOIN hypotheses h ON h.id=s.hypothesis_id JOIN projects p ON p.id=h.project_id WHERE m.target_id=$1 AND h.id<>$1 ORDER BY 1,4,5"#, (id as HypothesisId), fetch_all, connection)?;
    rows.into_iter().map(|r| decode_link(&r, context)).collect()
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn pending_case(
    connection: &mut PgConnection,
    id: HypothesisId,
    kind: CaseKind,
    context: JsonContext,
) -> Result<Option<ReviewCase>, HypothesisError> {
    let row = checked_query!(as RawReviewCase, r#"SELECT id AS "id!: _", kind AS "kind", subject_revision AS "subject_revision", state AS "state", opened_at AS "opened_at!: _", resolved_at AS "resolved_at: _", origin AS "origin", source_ref AS "source_ref" FROM review_cases WHERE hypothesis_id=$1 AND kind=$2 AND state='pending' FOR UPDATE"#, (id as HypothesisId,kind.as_str()), fetch_optional, connection)?;
    row.map(|r| decode_reviewcase(&r, context)).transpose()
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn record_decision(
    connection: &mut PgConnection,
    input: RecordDecision<'_>,
    context: JsonContext,
) -> Result<Decision, HypothesisError> {
    let revision = integer(input.subject_revision)?;
    let reason = text(input.reason)?;
    let client = client(input.principal.via.client.as_deref())?;
    let (front_matter, sha256) = input.document.unzip();
    let row = checked_query!(as RawDecision, r#"INSERT INTO decisions(review_case_id,action,subject_revision,reason,actor_user_id,via_channel,via_client,supersedes,front_matter,sha256) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9::text::jsonb,$10) RETURNING id AS "id!: _", review_case_id AS "review_case_id!: _", action AS "action", subject_revision AS "subject_revision", reason AS "reason", actor_user_id AS "actor_user_id?: _", actor_service_id AS "actor_service_id?: _", decider_revision, via_channel AS "via_channel", via_client AS "via_client", decided_at AS "decided_at!: _", supersedes AS "supersedes: _", origin AS "origin", source_ref AS "source_ref", front_matter::text AS "front_matter", sha256 AS "sha256""#, (input.case_id as ReviewCaseId,input.action.as_str(),revision as _,reason,input.principal.user_id as UserId,channel(input.principal.via.channel),client,input.supersedes as Option<DecisionId>,front_matter,sha256), fetch_optional, &mut *connection)?;
    let decision = decode_decision(&row.ok_or(HypothesisError::Invariant)?, context)?;
    checked_query!(exec Execute, "UPDATE review_cases SET state='resolved',resolved_at=now() WHERE id=$1 AND state='pending'", (input.case_id as ReviewCaseId), execute, connection)?;
    Ok(decision)
}
/// An automatic decision: the decision document a decider step wrote,
/// recorded on its pending decision case by the decider service account.
pub struct RecordAutomaticDecision<'a> {
    pub case_id: ReviewCaseId,
    pub action: DecisionAction,
    pub subject_revision: &'a BigInt,
    pub reason: &'a String,
    pub decider: ServiceAccountId,
    /// The revision of the decider's step.
    pub revision: &'a str,
    pub via_channel: Channel,
    pub via_client: Option<&'a str>,
    /// The decision document's front matter as JSON text, and its SHA-256.
    pub document: (&'a str, &'a str),
}
/// Record an automatic decision and resolve its case.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn record_automatic_decision(
    connection: &mut PgConnection,
    input: RecordAutomaticDecision<'_>,
    context: JsonContext,
) -> Result<Decision, HypothesisError> {
    let revision = integer(input.subject_revision)?;
    let reason = text(input.reason)?;
    let decider_revision = text(&String::from(input.revision))?;
    let client = client(input.via_client)?;
    let (front_matter, sha256) = input.document;
    let row = checked_query!(as RawDecision, r#"INSERT INTO decisions(review_case_id,action,subject_revision,reason,actor_service_id,decider_revision,via_channel,via_client,front_matter,sha256) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9::text::jsonb,$10) RETURNING id AS "id!: _", review_case_id AS "review_case_id!: _", action AS "action", subject_revision AS "subject_revision", reason AS "reason", actor_user_id AS "actor_user_id?: _", actor_service_id AS "actor_service_id?: _", decider_revision, via_channel AS "via_channel", via_client AS "via_client", decided_at AS "decided_at!: _", supersedes AS "supersedes: _", origin AS "origin", source_ref AS "source_ref", front_matter::text AS "front_matter", sha256 AS "sha256""#, (input.case_id as ReviewCaseId,input.action.as_str(),revision as _,reason,input.decider as ServiceAccountId,decider_revision,channel(input.via_channel),client,front_matter,sha256), fetch_optional, &mut *connection)?;
    let decision = decode_decision(&row.ok_or(HypothesisError::Invariant)?, context)?;
    checked_query!(exec Execute, "UPDATE review_cases SET state='resolved',resolved_at=now() WHERE id=$1 AND state='pending'", (input.case_id as ReviewCaseId), execute, connection)?;
    Ok(decision)
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn list_cases(
    connection: &mut PgConnection,
    id: HypothesisId,
    context: JsonContext,
) -> Result<Vec<ReviewCase>, HypothesisError> {
    let rows = checked_query!(as RawReviewCase, r#"SELECT id AS "id!: _", kind AS "kind", subject_revision AS "subject_revision", state AS "state", opened_at AS "opened_at!: _", resolved_at AS "resolved_at: _", origin AS "origin", source_ref AS "source_ref" FROM review_cases WHERE hypothesis_id=$1 ORDER BY opened_at"#, (id as HypothesisId), fetch_all, connection)?;
    rows.into_iter()
        .map(|r| decode_reviewcase(&r, context))
        .collect()
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn list_decisions(
    connection: &mut PgConnection,
    case_ids: &[ReviewCaseId],
    context: JsonContext,
) -> Result<Vec<Decision>, HypothesisError> {
    if case_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids = case_ids.iter().map(|id| id.0).collect::<Vec<_>>();
    let rows = checked_query!(as RawDecision, r#"SELECT id AS "id!: _", review_case_id AS "review_case_id!: _", action AS "action", subject_revision AS "subject_revision", reason AS "reason", actor_user_id AS "actor_user_id?: _", actor_service_id AS "actor_service_id?: _", decider_revision, via_channel AS "via_channel", via_client AS "via_client", decided_at AS "decided_at!: _", supersedes AS "supersedes: _", origin AS "origin", source_ref AS "source_ref", front_matter::text AS "front_matter", sha256 AS "sha256" FROM decisions WHERE review_case_id=ANY($1) ORDER BY decided_at"#, (&ids as _), fetch_all, connection)?;
    rows.into_iter()
        .map(|r| decode_decision(&r, context))
        .collect()
}

fn integer_array(values: &[&BigInt]) -> Result<Vec<i64>, HypothesisError> {
    values.iter().map(|value| integer(value)).collect()
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optional_json_decode_retains_sql_null_and_json_null() -> Result<(), HypothesisError> {
        let context = JsonContext {
            encode_nesting_budget: 8,
            decode_nesting_budget: 8,
        };
        assert!(decode_optional_json(None, context)?.is_none());
        let value =
            decode_optional_json(Some("null"), context)?.ok_or(HypothesisError::Invariant)?;
        assert!(matches!(value.node(value.root()), Some(json::Node::Null)));
        assert!(matches!(
            decode_optional_json(
                Some("[[]]"),
                JsonContext {
                    decode_nesting_budget: 0,
                    ..context
                }
            ),
            Err(HypothesisError::Decode(_))
        ));
        Ok(())
    }
}
