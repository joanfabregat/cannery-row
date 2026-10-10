//! Source-compatible PostgreSQL track operations with sanitized failures.
use cannery_core::{
    ids::{ProjectId, TrackId, UserId},
    json::{self, DecodeError, Document, EncodeError, Node},
    timestamps::Timestamp,
};
use num_bigint::BigInt;
use sqlx::PgConnection;
use std::{error::Error, fmt};

/// Explicit caller-selected JSON profiles; no universal production depth default.
#[derive(Clone, Copy, Debug)]
pub struct JsonContext {
    pub encode_nesting_budget: usize,
    pub decode_nesting_budget: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrackState {
    /// Waiting for its first approved plan; nothing in it can be claimed.
    Planning,
    Active,
    Paused,
    Archived,
}
impl TrackState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Archived => "archived",
        }
    }
}
impl TryFrom<&str> for TrackState {
    type Error = TrackError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "planning" => Ok(Self::Planning),
            "active" => Ok(Self::Active),
            "paused" => Ok(Self::Paused),
            "archived" => Ok(Self::Archived),
            _ => Err(TrackError::CorruptData),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
    type Error = TrackError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "agent" => Ok(Self::Agent),
            "workflow" => Ok(Self::Workflow),
            _ => Err(TrackError::CorruptData),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RowLock {
    Share,
    Update,
}

/// Sensitive strings and JSON are deliberately excluded from Debug.
pub struct Track {
    pub id: TrackId,
    pub project_id: ProjectId,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub producer: Option<Document>,
    pub mode: TrackMode,
    pub workflow: Option<Document>,
    pub state: TrackState,
    pub revision: i32,
    pub created_by: UserId,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
impl fmt::Debug for Track {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Track([redacted])")
    }
}

pub struct CreateTrack<'a> {
    pub project_id: ProjectId,
    pub slug: &'a String,
    pub title: &'a String,
    pub description: &'a String,
    pub producer: Option<&'a Document>,
    pub mode: TrackMode,
    pub workflow: Option<&'a Document>,
    pub created_by: UserId,
}
pub struct UpdateTrack<'a> {
    pub expected_revision: &'a BigInt,
    pub title: &'a String,
    pub description: &'a String,
    pub producer: Option<&'a Document>,
    pub state: TrackState,
    pub mode: TrackMode,
    pub workflow: Option<&'a Document>,
}

pub enum TrackError {
    Database { sqlstate: Option<String> },
    Encode(EncodeError),
    Decode(DecodeError),
    TextEncoding,
    TextNul,
    IntegerBinding,
    CorruptData,
}
impl TrackError {
    #[must_use]
    pub fn sqlstate(&self) -> Option<&str> {
        if let Self::Database { sqlstate } = self {
            sqlstate.as_deref()
        } else {
            None
        }
    }
}
impl From<sqlx::Error> for TrackError {
    fn from(error: sqlx::Error) -> Self {
        let sqlstate = error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .filter(|code| {
                code.len() == 5
                    && code
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
            })
            .map(std::borrow::Cow::into_owned);
        Self::Database { sqlstate }
    }
}
impl From<EncodeError> for TrackError {
    fn from(error: EncodeError) -> Self {
        Self::Encode(error)
    }
}
impl From<DecodeError> for TrackError {
    fn from(error: DecodeError) -> Self {
        Self::Decode(error)
    }
}
impl fmt::Display for TrackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Database { .. } => "track database operation failed",
            Self::Encode(_) => "track JSON encoding failed",
            Self::Decode(_) => "track JSON decoding failed",
            Self::TextEncoding => "track text encoding failed",
            Self::TextNul => "track text contains NUL",
            Self::IntegerBinding => "track integer cannot be adapted to PostgreSQL numeric",
            Self::CorruptData => "invalid persisted track data",
        })
    }
}
impl fmt::Debug for TrackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl Error for TrackError {}

struct RawTrack {
    id: TrackId,
    project_id: ProjectId,
    slug: String,
    title: String,
    description: String,
    producer: Option<String>,
    mode: String,
    workflow: Option<String>,
    state: String,
    revision: i32,
    created_by: UserId,
    created_at: Timestamp,
    updated_at: Timestamp,
}
fn decode(raw: &RawTrack, context: JsonContext) -> Result<Track, TrackError> {
    Ok(Track {
        id: raw.id,
        project_id: raw.project_id,
        slug: String::from(&raw.slug),
        title: String::from(&raw.title),
        description: String::from(&raw.description),
        producer: raw
            .producer
            .as_deref()
            .map(|s| json::decode_str(s, context.decode_nesting_budget))
            .transpose()?,
        mode: TrackMode::try_from(raw.mode.as_str())?,
        workflow: raw
            .workflow
            .as_deref()
            .map(|s| json::decode_str(s, context.decode_nesting_budget))
            .transpose()?,
        state: TrackState::try_from(raw.state.as_str())?,
        revision: raw.revision,
        created_by: raw.created_by,
        created_at: raw.created_at,
        updated_at: raw.updated_at,
    })
}
fn optional(raw: Option<RawTrack>, context: JsonContext) -> Result<Option<Track>, TrackError> {
    raw.map(|r| decode(&r, context)).transpose()
}
fn text(value: &String) -> Result<String, TrackError> {
    let value = value.as_utf8().ok_or(TrackError::TextEncoding)?;
    if value.contains('\0') {
        Err(TrackError::TextNul)
    } else {
        Ok(value)
    }
}
// Bind JSONB itself, not text cast in an UPDATE expression. PostgreSQL parses
// source psycopg Jsonb parameters before matching rows, including stale writes.
struct JsonbText(String);
impl sqlx::Type<sqlx::Postgres> for JsonbText {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_name("jsonb")
    }
}
impl sqlx::Encode<'_, sqlx::Postgres> for JsonbText {
    fn encode_by_ref(
        &self,
        buffer: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        buffer.push(1);
        buffer.extend_from_slice(self.0.as_bytes());
        Ok(sqlx::encode::IsNull::No)
    }
    fn size_hint(&self) -> usize {
        self.0.len() + 1
    }
}

fn json_parameter(
    value: Option<&Document>,
    context: JsonContext,
) -> Result<Option<JsonbText>, TrackError> {
    value
        .filter(|d| !matches!(d.node(d.root()), Some(Node::Null)))
        .map(|d| json::encode_ascii_pretty(d, context.encode_nesting_budget))
        .transpose()
        .map(|value| value.map(JsonbText))
        .map_err(Into::into)
}
fn integer(value: Option<&BigInt>) -> Result<Option<String>, TrackError> {
    value
        .map(|v| {
            let s = v.to_string();
            // psycopg's actual binary NUMERIC adapter stores the base-10000
            // weight in a signed int16. This is its measured wire bound, not
            // Python's separate JSON/string decimal-rendering limit.
            if s.trim_start_matches('-').len() > 131_072 {
                Err(TrackError::IntegerBinding)
            } else {
                Ok(s)
            }
        })
        .transpose()
}

/// Insert in `planning`, returning `None` only for an existing project/slug pair.
/// # Errors
/// Returns sanitized database, text or JSON failures. No transaction is started.
pub async fn create_track(
    connection: &mut PgConnection,
    input: CreateTrack<'_>,
    context: JsonContext,
) -> Result<Option<Track>, TrackError> {
    let slug = text(input.slug)?;
    let title = text(input.title)?;
    let description = text(input.description)?;
    let producer = json_parameter(input.producer, context)?;
    let workflow = json_parameter(input.workflow, context)?;
    let raw=sqlx::query_as!(RawTrack,r#"INSERT INTO tracks(project_id,slug,title,description,producer,mode,workflow,created_by,state)
VALUES($1,$2,$3,$4,$5,$6,$7,$8,'planning')
ON CONFLICT(project_id,slug) DO NOTHING RETURNING
id AS "id!: _",project_id AS "project_id!: _",slug,title,description,producer::text AS producer,mode,workflow::text AS workflow,state,revision,created_by AS "created_by!: _",created_at AS "created_at!: _",updated_at AS "updated_at!: _""#,input.project_id as ProjectId,slug,title,description,producer as _,input.mode.as_str(),workflow as _,input.created_by as UserId).fetch_optional(connection).await?;
    optional(raw, context)
}

/// Project-scoped lookup; row locks remain held by the caller's transaction.
/// # Errors
/// Returns sanitized database, text or persisted JSON failures.
pub async fn get_track(
    connection: &mut PgConnection,
    project: ProjectId,
    slug: &String,
    lock: Option<RowLock>,
    context: JsonContext,
) -> Result<Option<Track>, TrackError> {
    let slug = text(slug)?;
    let raw=match lock {
None=>sqlx::query_as!(RawTrack,r#"SELECT id AS "id!: _",project_id AS "project_id!: _",slug,title,description,producer::text AS producer,mode,workflow::text AS workflow,state,revision,created_by AS "created_by!: _",created_at AS "created_at!: _",updated_at AS "updated_at!: _" FROM tracks WHERE project_id=$1 AND slug=$2"#,project as ProjectId,slug).fetch_optional(&mut *connection).await?,
Some(RowLock::Share)=>sqlx::query_as!(RawTrack,r#"SELECT id AS "id!: _",project_id AS "project_id!: _",slug,title,description,producer::text AS producer,mode,workflow::text AS workflow,state,revision,created_by AS "created_by!: _",created_at AS "created_at!: _",updated_at AS "updated_at!: _" FROM tracks WHERE project_id=$1 AND slug=$2 FOR SHARE"#,project as ProjectId,slug).fetch_optional(&mut *connection).await?,
Some(RowLock::Update)=>sqlx::query_as!(RawTrack,r#"SELECT id AS "id!: _",project_id AS "project_id!: _",slug,title,description,producer::text AS producer,mode,workflow::text AS workflow,state,revision,created_by AS "created_by!: _",created_at AS "created_at!: _",updated_at AS "updated_at!: _" FROM tracks WHERE project_id=$1 AND slug=$2 FOR UPDATE"#,project as ProjectId,slug).fetch_optional(&mut *connection).await?,
};
    optional(raw, context)
}

/// # Errors
/// Returns sanitized database or persisted JSON failures.
pub async fn get_track_by_id(
    connection: &mut PgConnection,
    id: TrackId,
    context: JsonContext,
) -> Result<Option<Track>, TrackError> {
    let raw=sqlx::query_as!(RawTrack,r#"SELECT id AS "id!: _",project_id AS "project_id!: _",slug,title,description,producer::text AS producer,mode,workflow::text AS workflow,state,revision,created_by AS "created_by!: _",created_at AS "created_at!: _",updated_at AS "updated_at!: _" FROM tracks WHERE id=$1"#,id as TrackId).fetch_optional(connection).await?;
    optional(raw, context)
}

/// Raw state filters preserve source unknown-string behavior. Pagination is by
/// PostgreSQL slug collation; `None` LIMIT is unbounded, not an HTTP default.
/// # Errors
/// Returns sanitized database, encoding, integer or persisted JSON failures.
pub async fn list_tracks(
    connection: &mut PgConnection,
    project: ProjectId,
    state: Option<&String>,
    after: Option<&String>,
    limit: Option<&BigInt>,
    context: JsonContext,
) -> Result<Vec<Track>, TrackError> {
    let state = state.map(text).transpose()?;
    let after = after.map(text).transpose()?;
    let limit = integer(limit)?;
    let rows=sqlx::query_as!(RawTrack,r#"SELECT id AS "id!: _",project_id AS "project_id!: _",slug,title,description,producer::text AS producer,mode,workflow::text AS workflow,state,revision,created_by AS "created_by!: _",created_at AS "created_at!: _",updated_at AS "updated_at!: _" FROM tracks WHERE project_id=$1 AND ($2::text IS NULL OR state=$2) AND ($3::text IS NULL OR slug>$3) ORDER BY slug LIMIT $4::text::bigint"#,project as ProjectId,state,after,limit).fetch_all(connection).await?;
    rows.into_iter().map(|r| decode(&r, context)).collect()
}

/// Compare-and-replace. Every matched update increments revision, even if all
/// fields are unchanged. Wide integer revisions compare numerically without
/// prematurely narrowing to PostgreSQL int4/int8.
/// # Errors
/// Returns sanitized database, encoding, integer or persisted JSON failures.
pub async fn update_track(
    connection: &mut PgConnection,
    id: TrackId,
    input: UpdateTrack<'_>,
    context: JsonContext,
) -> Result<Option<Track>, TrackError> {
    let title = text(input.title)?;
    let description = text(input.description)?;
    let producer = json_parameter(input.producer, context)?;
    let workflow = json_parameter(input.workflow, context)?;
    let expected = integer(Some(input.expected_revision))?;
    let raw=sqlx::query_as!(RawTrack,r#"UPDATE tracks SET title=$1,description=$2,producer=$3,mode=$4,workflow=$5,state=$6,revision=revision+1,updated_at=now() WHERE id=$7 AND revision=$8::text::numeric RETURNING id AS "id!: _",project_id AS "project_id!: _",slug,title,description,producer::text AS producer,mode,workflow::text AS workflow,state,revision,created_by AS "created_by!: _",created_at AS "created_at!: _",updated_at AS "updated_at!: _""#,title,description,producer as _,input.mode.as_str(),workflow as _,input.state.as_str(),id as TrackId,expected).fetch_optional(connection).await?;
    optional(raw, context)
}

/// Only queued, active and awaiting-human-review hypotheses are open.
/// # Errors
/// Returns a sanitized database failure. This function takes no implicit lock.
pub async fn count_open_hypotheses(
    connection: &mut PgConnection,
    id: TrackId,
) -> Result<i64, TrackError> {
    Ok(sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM hypotheses WHERE track_id=$1 AND state=ANY(ARRAY['queued','active','documenting','deciding']::text[])"#,id as TrackId).fetch_one(connection).await?)
}

/// Every hypothesis of the track, whatever its state.
/// # Errors
/// Returns a sanitized database failure.
pub async fn count_hypotheses(
    connection: &mut PgConnection,
    id: TrackId,
) -> Result<i64, TrackError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM hypotheses WHERE track_id = $1"#,
        id as TrackId
    )
    .fetch_one(connection)
    .await?)
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
