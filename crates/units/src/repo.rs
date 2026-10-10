//! Source-compatible SQL operations; errors never include parameter values.
use cannery_core::{
    ids::{AttemptId, ProjectId, ReviewCaseId, ServiceAccountId, TrackId, UnitId, UserId},
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
pub enum UnitState {
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
impl UnitState {
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
impl TryFrom<&str> for UnitState {
    type Error = UnitError;
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
            _ => Err(UnitError::CorruptData),
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
    type Error = UnitError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "agent" => Ok(Self::Agent),
            "workflow" => Ok(Self::Workflow),
            _ => Err(UnitError::CorruptData),
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
    type Error = UnitError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "live" => Ok(Self::Live),
            "imported" => Ok(Self::Imported),
            _ => Err(UnitError::CorruptData),
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
    type Error = UnitError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "derived_from" => Ok(Self::DerivedFrom),
            "supersedes" => Ok(Self::Supersedes),
            "related_to" => Ok(Self::RelatedTo),
            _ => Err(UnitError::CorruptData),
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
    type Error = UnitError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "decision" => Ok(Self::Decision),
            "failure" => Ok(Self::Failure),
            _ => Err(UnitError::CorruptData),
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
    type Error = UnitError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "pending" => Ok(Self::Pending),
            "resolved" => Ok(Self::Resolved),
            _ => Err(UnitError::CorruptData),
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
    type Error = UnitError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "promote" => Ok(Self::Promote),
            "reject" => Ok(Self::Reject),
            "inconclusive" => Ok(Self::Inconclusive),
            "retry" => Ok(Self::Retry),
            "failed" => Ok(Self::Failed),
            "stop" => Ok(Self::Stop),
            _ => Err(UnitError::CorruptData),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkKind {
    Relation(RelationKind),
    Mention,
}
impl TryFrom<&str> for LinkKind {
    type Error = UnitError;
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
    Unit(UnitId),
    Attempt(AttemptId),
    Comment(CommentId),
    Report(EvidenceId),
}
impl MentionSource {
    fn parts(self) -> (&'static str, Uuid) {
        match self {
            Self::Unit(id) => ("unit", id.0),
            Self::Attempt(id) => ("attempt", id.0),
            Self::Comment(id) => ("comment", id.0),
            Self::Report(id) => ("report", id.0),
        }
    }
}
pub struct Unit {
    pub id: UnitId,
    pub project_id: ProjectId,
    pub number: i32,
    pub track_id: TrackId,
    pub track_slug: String,
    pub track_mode: TrackMode,
    pub state: UnitState,
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
impl fmt::Debug for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Unit([redacted])")
    }
}
struct RawRef {
    project_id: ProjectId,
    number: i32,
    id: UnitId,
}
struct RawUnit {
    id: UnitId,
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
fn decode_unit(r: &RawUnit, context: JsonContext) -> Result<Unit, UnitError> {
    Ok(Unit {
        id: r.id,
        project_id: r.project_id,
        number: r.number,
        track_id: r.track_id,
        track_slug: String::from(&r.track_slug),
        track_mode: TrackMode::try_from(r.track_mode.as_str())?,
        state: UnitState::try_from(r.state.as_str())?,
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
) -> Result<Option<Document>, UnitError> {
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
fn decode_revision(r: &RawRevision, context: JsonContext) -> Result<Revision, UnitError> {
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
    pub unit_id: UnitId,
    pub project_id: ProjectId,
    pub project_slug: String,
    pub number: i32,
    pub title: String,
    pub state: UnitState,
}
impl fmt::Debug for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Link([redacted])")
    }
}
struct RawLink {
    kind: String,
    unit_id: UnitId,
    project_id: ProjectId,
    project_slug: String,
    number: i32,
    title: String,
    state: String,
}
fn decode_link(r: &RawLink, _context: JsonContext) -> Result<Link, UnitError> {
    Ok(Link {
        kind: LinkKind::try_from(r.kind.as_str())?,
        unit_id: r.unit_id,
        project_id: r.project_id,
        project_slug: String::from(&r.project_slug),
        number: r.number,
        title: String::from(&r.title),
        state: UnitState::try_from(r.state.as_str())?,
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
fn decode_reviewcase(r: &RawReviewCase, _context: JsonContext) -> Result<ReviewCase, UnitError> {
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
fn decode_decision(r: &RawDecision, context: JsonContext) -> Result<Decision, UnitError> {
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
            _ => return Err(UnitError::CorruptData),
        },
    })
}
/// Sanitized failures preserve server SQLSTATE without retaining driver messages.
pub enum UnitError {
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
impl UnitError {
    #[must_use]
    pub fn sqlstate(&self) -> Option<&str> {
        if let Self::Database { sqlstate } = self {
            sqlstate.as_deref()
        } else {
            None
        }
    }
}
impl From<sqlx::Error> for UnitError {
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
impl From<EncodeError> for UnitError {
    fn from(e: EncodeError) -> Self {
        Self::Encode(e)
    }
}
impl From<DecodeError> for UnitError {
    fn from(e: DecodeError) -> Self {
        Self::Decode(e)
    }
}
impl fmt::Display for UnitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Database { .. } => "unit database operation failed",
            Self::Encode(_) => "unit JSON encoding failed",
            Self::Decode(_) => "unit JSON decoding failed",
            Self::TextEncoding => "unit text encoding failed",
            Self::TextNul => "unit text contains NUL",
            Self::IntegerBinding => "unit integer adaptation failed",
            Self::IntegerArrayRendering => "unit integer array rendering failed",
            Self::CorruptData => "invalid persisted unit data",
            Self::Invariant => "required unit row absent",
        })
    }
}
impl fmt::Debug for UnitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl Error for UnitError {}
fn text(v: &String) -> Result<String, UnitError> {
    let s = v.as_utf8().ok_or(UnitError::TextEncoding)?;
    if s.contains('\0') {
        Err(UnitError::TextNul)
    } else {
        Ok(s)
    }
}
fn client(v: Option<&str>) -> Result<Option<String>, UnitError> {
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
fn integer(value: &BigInt) -> Result<Integer, UnitError> {
    i64::try_from(value).map_err(|_| UnitError::IntegerBinding)
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
pub struct ListUnits<'a> {
    pub states: Option<&'a [UnitState]>,
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
) -> Result<i32, UnitError> {
    let result = next_number_query(Some(project))
        .fetch_optional(connection)
        .await;
    result?.flatten().ok_or(UnitError::Invariant)
}
pub(crate) fn next_number_query(
    project: Option<ProjectId>,
) -> sqlx::query::QueryScalar<'static, sqlx::Postgres, Option<i32>, sqlx::postgres::PgArguments> {
    sqlx::query_scalar!(
        "UPDATE projects SET next_unit_number=next_unit_number+1 WHERE id=$1 RETURNING next_unit_number-1",
        project as Option<ProjectId>
    )
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn get_unit(
    connection: &mut PgConnection,
    project: ProjectId,
    number: &BigInt,
    lock: bool,
    context: JsonContext,
) -> Result<Option<Unit>, UnitError> {
    let number = integer(number)?;
    if lock {
        let id = checked_query!(scalar Scalar, r#"SELECT id AS "id!: _" FROM units WHERE project_id=$1 AND number=$2 FOR UPDATE"#, (project as ProjectId,number as _), fetch_optional, &mut *connection)?;
        return match id {
            Some(id) => get_unit_by_id(connection, id, context).await,
            None => Ok(None),
        };
    }
    let row = checked_query!(as RawUnit, r#"SELECT h.id AS "id!: _", h.project_id AS "project_id!: _", h.number AS "number!", h.track_id AS "track_id!: _", t.slug AS "track_slug", t.mode AS "track_mode", h.state AS "state!", h.revision AS "revision", h.approved_revision AS "approved_revision", h.title AS "title!", h.created_by_user AS "created_by_user: _", h.created_by_service AS "created_by_service: _", h.created_at AS "created_at!: _", h.updated_at AS "updated_at!: _", h.approved_at AS "approved_at: _", h.origin AS "origin", h.source_ref AS "source_ref", h.external_id AS "external_id", h.imported::text AS "imported" FROM units h JOIN tracks t ON t.id=h.track_id WHERE h.project_id=$1 AND h.number=$2"#, (project as ProjectId,number as _), fetch_optional, connection)?;
    row.map(|r| decode_unit(&r, context)).transpose()
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn get_unit_by_id(
    connection: &mut PgConnection,
    id: UnitId,
    context: JsonContext,
) -> Result<Option<Unit>, UnitError> {
    let row = checked_query!(as RawUnit, r#"SELECT h.id AS "id!: _", h.project_id AS "project_id!: _", h.number AS "number!", h.track_id AS "track_id!: _", t.slug AS "track_slug", t.mode AS "track_mode", h.state AS "state!", h.revision AS "revision", h.approved_revision AS "approved_revision", h.title AS "title!", h.created_by_user AS "created_by_user: _", h.created_by_service AS "created_by_service: _", h.created_at AS "created_at!: _", h.updated_at AS "updated_at!: _", h.approved_at AS "approved_at: _", h.origin AS "origin", h.source_ref AS "source_ref", h.external_id AS "external_id", h.imported::text AS "imported" FROM units h JOIN tracks t ON t.id=h.track_id WHERE h.id=$1"#, (id as UnitId), fetch_optional, connection)?;
    row.map(|r| decode_unit(&r, context)).transpose()
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn list_units(
    connection: &mut PgConnection,
    project: ProjectId,
    input: ListUnits<'_>,
    context: JsonContext,
) -> Result<Vec<Unit>, UnitError> {
    let ListUnits {
        states,
        track_slug,
        before,
        limit,
    } = input;
    let states = states.map(|s| s.iter().map(|s| s.as_str().to_owned()).collect::<Vec<_>>());
    let track = track_slug.map(text).transpose()?;
    let before = before.map(integer).transpose()?;
    let limit = integer(limit)?;
    let rows = checked_query!(as RawUnit, r#"SELECT h.id AS "id!: _", h.project_id AS "project_id!: _", h.number AS "number!", h.track_id AS "track_id!: _", t.slug AS "track_slug", t.mode AS "track_mode", h.state AS "state!", h.revision AS "revision", h.approved_revision AS "approved_revision", h.title AS "title!", h.created_by_user AS "created_by_user: _", h.created_by_service AS "created_by_service: _", h.created_at AS "created_at!: _", h.updated_at AS "updated_at!: _", h.approved_at AS "approved_at: _", h.origin AS "origin", h.source_ref AS "source_ref", h.external_id AS "external_id", h.imported::text AS "imported" FROM units h JOIN tracks t ON t.id=h.track_id WHERE h.project_id=$1 AND ($2::text[] IS NULL OR h.state=ANY($2)) AND ($3::text IS NULL OR t.slug=$3) AND ($4::bigint IS NULL OR h.number<$4) ORDER BY h.number DESC LIMIT $5::bigint"#, (project as ProjectId,states.as_deref(),track,before as _,limit as _), fetch_all, connection)?;
    rows.into_iter().map(|r| decode_unit(&r, context)).collect()
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn set_state(
    connection: &mut PgConnection,
    id: UnitId,
    state: UnitState,
    approved_revision: Option<&BigInt>,
) -> Result<(), UnitError> {
    let revision = approved_revision.map(integer).transpose()?;
    checked_query!(exec Execute, "UPDATE units SET state=$1,updated_at=now(),approved_revision=coalesce($2,approved_revision),approved_at=CASE WHEN $2::bigint IS NULL THEN approved_at ELSE now() END WHERE id=$3", (state.as_str(),revision as _,id as UnitId), execute, connection)?;
    Ok(())
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn get_revision(
    connection: &mut PgConnection,
    id: UnitId,
    revision: &BigInt,
    context: JsonContext,
) -> Result<Option<Revision>, UnitError> {
    let revision = integer(revision)?;
    let row = checked_query!(as RawRevision, r#"SELECT revision AS "revision", content::text AS "content!", science_revision AS "science_revision", author_user AS "author_user: _", author_service AS "author_service: _", via_channel AS "via_channel", via_client AS "via_client", created_at AS "created_at!: _", origin AS "origin" FROM unit_revisions WHERE unit_id=$1 AND revision=$2"#, (id as UnitId,revision as _), fetch_optional, connection)?;
    row.map(|r| decode_revision(&r, context)).transpose()
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn list_revisions(
    connection: &mut PgConnection,
    id: UnitId,
    after: Option<&BigInt>,
    limit: Option<&BigInt>,
    context: JsonContext,
) -> Result<Vec<Revision>, UnitError> {
    let after = after.map(integer).transpose()?;
    let limit = limit.map(integer).transpose()?;
    let rows = checked_query!(as RawRevision, r#"SELECT revision AS "revision", content::text AS "content!", science_revision AS "science_revision", author_user AS "author_user: _", author_service AS "author_service: _", via_channel AS "via_channel", via_client AS "via_client", created_at AS "created_at!: _", origin AS "origin" FROM unit_revisions WHERE unit_id=$1 AND ($2::bigint IS NULL OR revision>$2) ORDER BY revision LIMIT $3::bigint"#, (id as UnitId,after as _,limit as _), fetch_all, connection)?;
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
) -> Result<BTreeMap<(ProjectId, i32), UnitId>, UnitError> {
    let pairs = refs.into_iter().collect::<BTreeSet<_>>();
    if pairs.is_empty() {
        return Ok(BTreeMap::new());
    }
    let projects = pairs.iter().map(|(p, _)| p.0).collect::<Vec<_>>();
    let values = pairs.iter().map(|(_, n)| n).collect::<Vec<_>>();
    let numbers = integer_array(&values)?;
    let rows = checked_query!(as RawRef, r#"SELECT h.project_id AS "project_id!: _",h.number,h.id AS "id!: _" FROM units h JOIN unnest($1::uuid[],$2::bigint[]) AS r(project_id,number) ON r.project_id=h.project_id AND r.number=h.number"#, (&projects as _,numbers as _), fetch_all, connection)?;
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
    id: UnitId,
    relations: impl IntoIterator<Item = (RelationKind, UnitId)>,
) -> Result<(), UnitError> {
    checked_query!(exec Execute, "DELETE FROM unit_relations WHERE unit_id=$1", (id as UnitId), execute, &mut *connection)?;
    let unique = relations.into_iter().collect::<BTreeSet<_>>();
    if !unique.is_empty() {
        let kinds = unique
            .iter()
            .map(|(k, _)| k.as_str().to_owned())
            .collect::<Vec<_>>();
        let targets = unique.iter().map(|(_, id)| id.0).collect::<Vec<_>>();
        checked_query!(exec Execute, "INSERT INTO unit_relations(unit_id,kind,target_id) SELECT $1,kind,target FROM unnest($2::text[],$3::uuid[]) AS r(kind,target)", (id as UnitId,&kinds,&targets as _), execute, connection)?;
    }
    Ok(())
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn replace_mentions(
    connection: &mut PgConnection,
    source: MentionSource,
    targets: impl IntoIterator<Item = UnitId>,
) -> Result<(), UnitError> {
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
    id: UnitId,
    context: JsonContext,
) -> Result<Vec<Link>, UnitError> {
    let rows = checked_query!(as RawLink, r#"SELECT r.kind AS "kind!", h.id AS "unit_id!: _", h.project_id AS "project_id!: _", p.slug AS "project_slug!", h.number AS "number!", h.title AS "title!", h.state AS "state!" FROM unit_relations r JOIN units h ON h.id=r.target_id JOIN projects p ON p.id=h.project_id WHERE r.unit_id=$1 ORDER BY r.kind,p.slug,h.number"#, (id as UnitId), fetch_all, connection)?;
    rows.into_iter().map(|r| decode_link(&r, context)).collect()
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn backlinks(
    connection: &mut PgConnection,
    id: UnitId,
    context: JsonContext,
) -> Result<Vec<Link>, UnitError> {
    let rows = checked_query!(as RawLink, r#"SELECT r.kind AS "kind!", h.id AS "unit_id!: _", h.project_id AS "project_id!: _", p.slug AS "project_slug!", h.number AS "number!", h.title AS "title!", h.state AS "state!" FROM unit_relations r JOIN units h ON h.id=r.unit_id JOIN projects p ON p.id=h.project_id WHERE r.target_id=$1 UNION SELECT 'mention' AS "kind!", h.id AS "unit_id!: _", h.project_id AS "project_id!: _", p.slug AS "project_slug!", h.number AS "number!", h.title AS "title!", h.state AS "state!" FROM mentions m JOIN LATERAL (SELECT m.source_id AS unit_id WHERE m.source_type='unit' UNION ALL SELECT c.unit_id FROM comments c WHERE m.source_type='comment' AND c.id=m.source_id UNION ALL SELECT a.unit_id FROM phase_outputs e JOIN attempts a ON a.id=e.attempt_id WHERE m.source_type='report' AND e.id=m.source_id) s ON true JOIN units h ON h.id=s.unit_id JOIN projects p ON p.id=h.project_id WHERE m.target_id=$1 AND h.id<>$1 ORDER BY 1,4,5"#, (id as UnitId), fetch_all, connection)?;
    rows.into_iter().map(|r| decode_link(&r, context)).collect()
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn pending_case(
    connection: &mut PgConnection,
    id: UnitId,
    kind: CaseKind,
    context: JsonContext,
) -> Result<Option<ReviewCase>, UnitError> {
    let row = checked_query!(as RawReviewCase, r#"SELECT id AS "id!: _", kind AS "kind", subject_revision AS "subject_revision", state AS "state", opened_at AS "opened_at!: _", resolved_at AS "resolved_at: _", origin AS "origin", source_ref AS "source_ref" FROM review_cases WHERE unit_id=$1 AND kind=$2 AND state='pending' FOR UPDATE"#, (id as UnitId,kind.as_str()), fetch_optional, connection)?;
    row.map(|r| decode_reviewcase(&r, context)).transpose()
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn record_decision(
    connection: &mut PgConnection,
    input: RecordDecision<'_>,
    context: JsonContext,
) -> Result<Decision, UnitError> {
    let revision = integer(input.subject_revision)?;
    let reason = text(input.reason)?;
    let client = client(input.principal.via.client.as_deref())?;
    let (front_matter, sha256) = input.document.unzip();
    let row = checked_query!(as RawDecision, r#"INSERT INTO decisions(review_case_id,action,subject_revision,reason,actor_user_id,via_channel,via_client,supersedes,front_matter,sha256) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9::text::jsonb,$10) RETURNING id AS "id!: _", review_case_id AS "review_case_id!: _", action AS "action", subject_revision AS "subject_revision", reason AS "reason", actor_user_id AS "actor_user_id?: _", actor_service_id AS "actor_service_id?: _", decider_revision, via_channel AS "via_channel", via_client AS "via_client", decided_at AS "decided_at!: _", supersedes AS "supersedes: _", origin AS "origin", source_ref AS "source_ref", front_matter::text AS "front_matter", sha256 AS "sha256""#, (input.case_id as ReviewCaseId,input.action.as_str(),revision as _,reason,input.principal.user_id as UserId,channel(input.principal.via.channel),client,input.supersedes as Option<DecisionId>,front_matter,sha256), fetch_optional, &mut *connection)?;
    let decision = decode_decision(&row.ok_or(UnitError::Invariant)?, context)?;
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
) -> Result<Decision, UnitError> {
    let revision = integer(input.subject_revision)?;
    let reason = text(input.reason)?;
    let decider_revision = text(&String::from(input.revision))?;
    let client = client(input.via_client)?;
    let (front_matter, sha256) = input.document;
    let row = checked_query!(as RawDecision, r#"INSERT INTO decisions(review_case_id,action,subject_revision,reason,actor_service_id,decider_revision,via_channel,via_client,front_matter,sha256) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9::text::jsonb,$10) RETURNING id AS "id!: _", review_case_id AS "review_case_id!: _", action AS "action", subject_revision AS "subject_revision", reason AS "reason", actor_user_id AS "actor_user_id?: _", actor_service_id AS "actor_service_id?: _", decider_revision, via_channel AS "via_channel", via_client AS "via_client", decided_at AS "decided_at!: _", supersedes AS "supersedes: _", origin AS "origin", source_ref AS "source_ref", front_matter::text AS "front_matter", sha256 AS "sha256""#, (input.case_id as ReviewCaseId,input.action.as_str(),revision as _,reason,input.decider as ServiceAccountId,decider_revision,channel(input.via_channel),client,front_matter,sha256), fetch_optional, &mut *connection)?;
    let decision = decode_decision(&row.ok_or(UnitError::Invariant)?, context)?;
    checked_query!(exec Execute, "UPDATE review_cases SET state='resolved',resolved_at=now() WHERE id=$1 AND state='pending'", (input.case_id as ReviewCaseId), execute, connection)?;
    Ok(decision)
}
/// Source-compatible persistence on the caller connection.
/// # Errors
/// Returns sanitized driver, server, JSON or invariant failures.
pub async fn list_cases(
    connection: &mut PgConnection,
    id: UnitId,
    context: JsonContext,
) -> Result<Vec<ReviewCase>, UnitError> {
    let rows = checked_query!(as RawReviewCase, r#"SELECT id AS "id!: _", kind AS "kind", subject_revision AS "subject_revision", state AS "state", opened_at AS "opened_at!: _", resolved_at AS "resolved_at: _", origin AS "origin", source_ref AS "source_ref" FROM review_cases WHERE unit_id=$1 ORDER BY opened_at"#, (id as UnitId), fetch_all, connection)?;
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
) -> Result<Vec<Decision>, UnitError> {
    if case_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids = case_ids.iter().map(|id| id.0).collect::<Vec<_>>();
    let rows = checked_query!(as RawDecision, r#"SELECT id AS "id!: _", review_case_id AS "review_case_id!: _", action AS "action", subject_revision AS "subject_revision", reason AS "reason", actor_user_id AS "actor_user_id?: _", actor_service_id AS "actor_service_id?: _", decider_revision, via_channel AS "via_channel", via_client AS "via_client", decided_at AS "decided_at!: _", supersedes AS "supersedes: _", origin AS "origin", source_ref AS "source_ref", front_matter::text AS "front_matter", sha256 AS "sha256" FROM decisions WHERE review_case_id=ANY($1) ORDER BY decided_at"#, (&ids as _), fetch_all, connection)?;
    rows.into_iter()
        .map(|r| decode_decision(&r, context))
        .collect()
}

fn integer_array(values: &[&BigInt]) -> Result<Vec<i64>, UnitError> {
    values.iter().map(|value| integer(value)).collect()
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optional_json_decode_retains_sql_null_and_json_null() -> Result<(), UnitError> {
        let context = JsonContext {
            encode_nesting_budget: 8,
            decode_nesting_budget: 8,
        };
        assert!(decode_optional_json(None, context)?.is_none());
        let value = decode_optional_json(Some("null"), context)?.ok_or(UnitError::Invariant)?;
        assert!(matches!(value.node(value.root()), Some(json::Node::Null)));
        assert!(matches!(
            decode_optional_json(
                Some("[[]]"),
                JsonContext {
                    decode_nesting_budget: 0,
                    ..context
                }
            ),
            Err(UnitError::Decode(_))
        ));
        Ok(())
    }
}
