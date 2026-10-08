//! Brief SQL. Callers own transactions, the project lock and audit writes.
//!
//! A brief is a chain of immutable revisions per project; the latest is the
//! project's brief. Parsing and validating the document belongs to callers.

use crate::ProjectError;
use cannery_core::{
    ids::{AttemptId, ProjectId, UserId},
    timestamps::Timestamp,
};
use sqlx::PgConnection;

/// One stored revision, with its author's name for display.
#[derive(Clone, Debug, PartialEq)]
pub struct Brief {
    pub revision: i32,
    pub document: String,
    /// The front matter as stored JSON text.
    pub front_matter: String,
    pub body: String,
    pub sha256: String,
    pub created_by: UserId,
    pub created_by_name: Option<String>,
    pub via_channel: String,
    pub via_client: Option<String>,
    pub created_at: Timestamp,
}

/// The revision an attempt pinned at its claim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedBrief {
    pub revision: i32,
    pub sha256: String,
}

/// What a new revision stores; the revision number is the next one.
pub struct NewBrief<'a> {
    pub project_id: ProjectId,
    pub document: &'a str,
    /// The front matter as JSON text.
    pub front_matter: &'a str,
    pub body: &'a str,
    pub sha256: &'a str,
    pub created_by: UserId,
    pub via_channel: &'a str,
    pub via_client: Option<&'a str>,
}

/// The project's current brief revision number, 0 without a brief.
/// # Errors
/// Returns a sanitized database error on persistence failure.
pub async fn current_revision(
    conn: &mut PgConnection,
    project_id: ProjectId,
) -> Result<i32, ProjectError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT coalesce(max(revision), 0) AS "revision!" FROM briefs WHERE project_id = $1"#,
        project_id as _
    )
    .fetch_one(conn)
    .await?)
}

/// One revision, or the latest when `revision` is `None`.
/// # Errors
/// Returns a sanitized database error on persistence failure.
pub async fn get_brief(
    conn: &mut PgConnection,
    project_id: ProjectId,
    revision: Option<i32>,
) -> Result<Option<Brief>, ProjectError> {
    Ok(sqlx::query_as!(
        Brief,
        r#"SELECT b.revision, b.document, b.front_matter::text AS "front_matter!", b.body,
        b.sha256, b.created_by AS "created_by!: _",
        coalesce(u.display_name, u.email) AS "created_by_name?", b.via_channel, b.via_client,
        b.created_at AS "created_at!: _"
        FROM briefs b JOIN users u ON u.id = b.created_by
        WHERE b.project_id = $1 AND ($2::integer IS NULL OR b.revision = $2)
        ORDER BY b.revision DESC LIMIT 1"#,
        project_id as _,
        revision
    )
    .fetch_optional(conn)
    .await?)
}

/// Revisions newest first, before the `before` revision when given. A missing
/// limit is unbounded.
/// # Errors
/// Returns a sanitized database error on persistence failure.
pub async fn list_briefs(
    conn: &mut PgConnection,
    project_id: ProjectId,
    before: Option<i32>,
    limit: Option<i64>,
) -> Result<Vec<Brief>, ProjectError> {
    Ok(sqlx::query_as!(
        Brief,
        r#"SELECT b.revision, b.document, b.front_matter::text AS "front_matter!", b.body,
        b.sha256, b.created_by AS "created_by!: _",
        coalesce(u.display_name, u.email) AS "created_by_name?", b.via_channel, b.via_client,
        b.created_at AS "created_at!: _"
        FROM briefs b JOIN users u ON u.id = b.created_by
        WHERE b.project_id = $1 AND ($2::integer IS NULL OR b.revision < $2)
        ORDER BY b.revision DESC LIMIT $3"#,
        project_id as _,
        before,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// Store the next revision. Call inside a transaction holding the project
/// row lock, after checking the expected revision.
/// # Errors
/// Returns a sanitized database error on persistence failure.
pub async fn create_brief(
    conn: &mut PgConnection,
    brief: NewBrief<'_>,
) -> Result<i32, ProjectError> {
    Ok(sqlx::query_scalar!(
        r#"INSERT INTO briefs (project_id, revision, document, front_matter, body, sha256,
        created_by, via_channel, via_client)
        SELECT $1, coalesce(max(revision), 0) + 1, $2, $3::text::jsonb, $4, $5, $6, $7, $8
        FROM briefs WHERE project_id = $1
        RETURNING revision"#,
        brief.project_id as _,
        brief.document,
        brief.front_matter,
        brief.body,
        brief.sha256,
        brief.created_by as _,
        brief.via_channel,
        brief.via_client
    )
    .fetch_one(conn)
    .await?)
}

/// The brief revision an attempt pinned, if the project had one at its claim.
/// # Errors
/// Returns a sanitized database error on persistence failure.
pub async fn pinned_brief(
    conn: &mut PgConnection,
    attempt_id: AttemptId,
) -> Result<Option<PinnedBrief>, ProjectError> {
    Ok(sqlx::query_as!(
        PinnedBrief,
        r#"SELECT b.revision, b.sha256 FROM attempts a
        JOIN briefs b ON b.project_id = a.project_id AND b.revision = a.brief_revision
        WHERE a.id = $1"#,
        attempt_id as _
    )
    .fetch_optional(conn)
    .await?)
}
