//! Append-only configuration SQL; callers own project locks and transactions.
use cannery_core::{
    ids::{ProjectId, UserId},
    json::{self, DecodeError, Document, EncodeError},
    timestamps::Timestamp,
};
use num_bigint::BigInt;
use sqlx::PgConnection;

/// The two configuration domains accepted by the public routes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Science,
    Dashboard,
}
impl Kind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Science => "science",
            Self::Dashboard => "dashboard",
        }
    }
}

/// Separate source JSON encoder and psycopg decoder profiles; no inferred default.
#[derive(Clone, Copy, Debug)]
pub struct JsonContext {
    pub encode_nesting_budget: usize,
    pub decode_nesting_budget: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("configuration database operation failed")]
    Database { sqlstate: Option<String> },
    #[error("configuration JSON encoding failed")]
    Encode(#[from] EncodeError),
    #[error("configuration JSON decoding failed")]
    Decode(DecodeError),
}
impl ConfigError {
    fn database(error: &sqlx::Error) -> Self {
        let sqlstate = error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .and_then(|code| {
                (code.len() == 5
                    && code
                        .bytes()
                        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit()))
                .then(|| code.into_owned())
            });
        Self::Database { sqlstate }
    }
}

pub struct ConfigRevision {
    pub project_id: ProjectId,
    pub kind: String,
    pub revision: i32,
    pub content: Document,
    pub science_revision: Option<i32>,
    pub created_by: UserId,
    pub created_at: Timestamp,
}
impl std::fmt::Debug for ConfigRevision {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ConfigRevision([redacted])")
    }
}
struct RawRevision {
    project_id: ProjectId,
    kind: String,
    revision: i32,
    content: String,
    science_revision: Option<i32>,
    created_by: UserId,
    created_at: Timestamp,
}
impl RawRevision {
    fn decode(self, context: JsonContext) -> Result<ConfigRevision, ConfigError> {
        Ok(ConfigRevision {
            project_id: self.project_id,
            kind: self.kind,
            revision: self.revision,
            content: json::decode_str(&self.content, context.decode_nesting_budget)
                .map_err(ConfigError::Decode)?,
            science_revision: self.science_revision,
            created_by: self.created_by,
            created_at: self.created_at,
        })
    }
}
fn integer(value: Option<&BigInt>) -> Result<Option<String>, ConfigError> {
    value
        .map(|value| {
            let text = value.to_string();
            // Psycopg binds large integers as binary NUMERIC, without the
            // JSON digit limit. Its signed base-10000 weight is at most 32767.
            if text.trim_start_matches('-').len() > 131_072 {
                Err(ConfigError::Database { sqlstate: None })
            } else {
                Ok(text)
            }
        })
        .transpose()
}

// Bind JSONB as JSONB, so PostgreSQL validates the input parameter before
// evaluating the statement. A text cast inside SELECT can change error order.
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

/// Append the next revision after the caller locks the project row.
/// # Errors
/// Preserves encoding failures before SQL and decoding failures after INSERT.
/// Database failures are sanitized; the caller must roll back its transaction.
pub async fn create_revision(
    connection: &mut PgConnection,
    project_id: ProjectId,
    kind: Kind,
    content: &Document,
    science_revision: Option<&BigInt>,
    created_by: UserId,
    json_profile: JsonContext,
) -> Result<ConfigRevision, ConfigError> {
    let content = JsonbText(json::encode_ascii_pretty(
        content,
        json_profile.encode_nesting_budget,
    )?);
    let science_revision = integer(science_revision)?;
    let row = sqlx::query_as!(
        RawRevision,
        r#"INSERT INTO config_revisions
            (project_id, kind, revision, content, science_revision, created_by)
        SELECT $1, $2, coalesce(max(revision), 0) + 1, $3,
            $4::text::integer, $5
        FROM config_revisions WHERE project_id = $1 AND kind = $2
        RETURNING project_id AS "project_id!: _", kind, revision,
            content::text AS "content!", science_revision,
            created_by AS "created_by!: _", created_at AS "created_at!: _""#,
        project_id as ProjectId,
        kind.as_str(),
        content as _,
        science_revision,
        created_by as UserId,
    )
    .fetch_one(connection)
    .await
    .map_err(|error| ConfigError::database(&error))?;
    row.decode(json_profile)
}

/// Read a specific revision or the latest, preserving PostgreSQL bigint casts.
/// # Errors
/// Returns sanitized conversion, database or stored JSON failures.
pub async fn get_revision(
    connection: &mut PgConnection,
    project_id: ProjectId,
    kind: Kind,
    revision: Option<&BigInt>,
    context: JsonContext,
) -> Result<Option<ConfigRevision>, ConfigError> {
    let revision = integer(revision)?;
    let row = sqlx::query_as!(
        RawRevision,
        r#"SELECT project_id AS "project_id!: _", kind, revision,
            content::text AS "content!", science_revision,
            created_by AS "created_by!: _", created_at AS "created_at!: _"
        FROM config_revisions
        WHERE project_id = $1 AND kind = $2
            AND ($3::text::bigint IS NULL OR revision = $3::text::bigint)
        ORDER BY revision DESC LIMIT 1"#,
        project_id as ProjectId,
        kind.as_str(),
        revision,
    )
    .fetch_optional(connection)
    .await
    .map_err(|error| ConfigError::database(&error))?;
    row.map(|row| row.decode(context)).transpose()
}

/// Newest first, with an exclusive before cursor and optional PostgreSQL LIMIT.
/// # Errors
/// Returns sanitized conversion, database or stored JSON failures.
pub async fn list_revisions(
    connection: &mut PgConnection,
    project_id: ProjectId,
    kind: Kind,
    before: Option<&BigInt>,
    limit: Option<&BigInt>,
    context: JsonContext,
) -> Result<Vec<ConfigRevision>, ConfigError> {
    let before = integer(before)?;
    let limit = integer(limit)?;
    let rows = sqlx::query_as!(
        RawRevision,
        r#"SELECT project_id AS "project_id!: _", kind, revision,
            content::text AS "content!", science_revision,
            created_by AS "created_by!: _", created_at AS "created_at!: _"
        FROM config_revisions
        WHERE project_id = $1 AND kind = $2
            AND ($3::text::bigint IS NULL OR revision < $3::text::bigint)
        ORDER BY revision DESC LIMIT $4::text::bigint"#,
        project_id as ProjectId,
        kind.as_str(),
        before,
        limit,
    )
    .fetch_all(connection)
    .await
    .map_err(|error| ConfigError::database(&error))?;
    rows.into_iter().map(|row| row.decode(context)).collect()
}
