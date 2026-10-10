//! Transcript chunk SQL. Callers own transactions, locks, object storage,
//! validation and audit.
//!
//! An agent appends its transcript in chunks while its attempt runs; each
//! chunk is one stored object of JSON Lines. Submitting the attempt seals
//! the chunks into one `transcript` artifact.
use crate::repo::TrackError;
use cannery_core::{
    ids::{AttemptId, ProjectId, ServiceAccountId, UserId},
    timestamps::Timestamp,
};
use sqlx::PgConnection;
use uuid::Uuid;

/// One stored chunk of a transcript.
#[derive(Clone, Debug)]
pub struct Chunk {
    pub sequence: i32,
    pub first_event: i32,
    pub events: i32,
    pub backend: String,
    pub bucket: String,
    pub key: String,
    pub size_bytes: i64,
    pub sha256: String,
    pub created_at: Timestamp,
}

/// What a new chunk stores.
pub struct NewChunk<'a> {
    pub attempt_id: AttemptId,
    pub sequence: i32,
    pub first_event: i32,
    pub events: i32,
    pub backend: &'a str,
    pub bucket: &'a str,
    pub key: &'a str,
    pub size_bytes: i64,
    pub sha256: &'a str,
    pub author_user: Option<UserId>,
    pub author_service: Option<ServiceAccountId>,
    pub via_channel: &'a str,
    pub via_client: Option<&'a str>,
}

/// The sealed transcript artifact of an attempt.
#[derive(Clone, Debug)]
pub struct Sealed {
    pub id: Uuid,
    pub size_bytes: i64,
    pub sha256: String,
}

/// The attempt's chunks, in order.
/// # Errors
/// Returns a sanitized database error.
pub async fn chunks(conn: &mut PgConnection, attempt: AttemptId) -> Result<Vec<Chunk>, TrackError> {
    Ok(sqlx::query_as!(
        Chunk,
        r#"SELECT sequence, first_event, events, backend, bucket, key, size_bytes, sha256,
        created_at AS "created_at!: _"
        FROM transcript_chunks WHERE attempt_id = $1 ORDER BY sequence"#,
        attempt as AttemptId
    )
    .fetch_all(conn)
    .await?)
}

/// Store a chunk's row once its object is written.
/// # Errors
/// Returns a sanitized database error; a concurrent append of the same
/// sequence is a unique violation.
pub async fn insert(conn: &mut PgConnection, chunk: NewChunk<'_>) -> Result<(), TrackError> {
    sqlx::query!(
        r#"INSERT INTO transcript_chunks (attempt_id, sequence, first_event, events, backend,
        bucket, key, size_bytes, sha256, author_user, author_service, via_channel, via_client)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)"#,
        chunk.attempt_id as AttemptId,
        chunk.sequence,
        chunk.first_event,
        chunk.events,
        chunk.backend,
        chunk.bucket,
        chunk.key,
        chunk.size_bytes,
        chunk.sha256,
        chunk.author_user as Option<UserId>,
        chunk.author_service as Option<ServiceAccountId>,
        chunk.via_channel,
        chunk.via_client
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The attempt's sealed transcript artifact, if it has one.
/// # Errors
/// Returns a sanitized database error.
pub async fn sealed(
    conn: &mut PgConnection,
    attempt: AttemptId,
) -> Result<Option<Sealed>, TrackError> {
    Ok(sqlx::query_as!(
        Sealed,
        r#"SELECT id AS "id!: Uuid", size_bytes, sha256 FROM artifacts
        WHERE attempt_id = $1 AND role = 'transcript' AND job_id IS NULL
        ORDER BY verified_at LIMIT 1"#,
        attempt as AttemptId
    )
    .fetch_optional(conn)
    .await?)
}

/// Record the sealed transcript as the attempt's `transcript` artifact.
/// # Errors
/// Returns a sanitized database error.
#[allow(
    clippy::too_many_arguments,
    reason = "One artifact row: where it is stored and what it holds"
)]
pub async fn seal(
    conn: &mut PgConnection,
    project: ProjectId,
    attempt: AttemptId,
    backend: &str,
    bucket: &str,
    key: &str,
    generation: Option<&str>,
    size_bytes: i64,
    sha256: &str,
) -> Result<Uuid, TrackError> {
    Ok(sqlx::query_scalar!(
        r#"INSERT INTO artifacts (project_id, attempt_id, role, backend, bucket, key, generation,
        size_bytes, sha256, media_type)
        VALUES ($1, $2, 'transcript', $3, $4, $5, $6, $7, $8, 'application/jsonl')
        RETURNING id AS "id!: Uuid""#,
        project as ProjectId,
        attempt as AttemptId,
        backend,
        bucket,
        key,
        generation,
        size_bytes,
        sha256
    )
    .fetch_one(conn)
    .await?)
}
