//! The six frozen comments functions. Callers own transactions, row locks and preparation history.
use crate::{RepositoryError, integer::Integer, redacted, source, text};
use cannery_core::{
    ids::{AttemptId, ProjectId, UnitId, UserId},
    principal::{Channel, UserPrincipal},
    timestamps::Timestamp,
};
use num_bigint::BigInt;
use sqlx::PgConnection;
use uuid::Uuid;
#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type)]
#[sqlx(transparent)]
pub struct CommentId(pub Uuid);
pub struct Comment {
    pub id: CommentId,
    pub project_id: ProjectId,
    pub unit_id: UnitId,
    pub unit_number: i32,
    pub attempt_id: Option<AttemptId>,
    pub attempt_sequence: Option<i32>,
    pub author_user: UserId,
    pub body_markdown: String,
    pub revision: i32,
    pub created_at: Timestamp,
    pub edited_at: Option<Timestamp>,
}
pub struct CommentRevision {
    pub revision: i32,
    pub body_markdown: String,
    pub via_channel: String,
    pub via_client: Option<String>,
    pub created_at: Timestamp,
}
redacted!(Comment, CommentRevision);
const fn channel(value: Channel) -> &'static str {
    match value {
        Channel::Ui => "ui",
        Channel::Api => "api",
        Channel::Mcp => "mcp",
        Channel::Cli => "cli",
        Channel::System => "system",
    }
}
/// Insert a comment and its initial revision in the caller's transaction.
/// # Errors
/// Returns sanitized database/encoding errors or the source's missing RETURNING invariant.
pub async fn create_comment(
    conn: &mut PgConnection,
    project_id: ProjectId,
    unit_id: UnitId,
    attempt_id: Option<AttemptId>,
    body: &str,
    author: &UserPrincipal,
) -> Result<CommentId, RepositoryError> {
    text(body)?;
    let row=source!(sqlx::query!(r#"INSERT INTO comments(project_id,unit_id,attempt_id,author_user,body_markdown) VALUES($1,$2,$3,$4,$5) RETURNING id AS "id!: CommentId""#,project_id as _,unit_id as _,attempt_id as _,author.user_id as _,body),fetch_optional,&mut *conn)?.ok_or(RepositoryError::Invariant)?;
    add_revision(conn, row.id, &BigInt::from(1), body, author).await?;
    Ok(row.id)
}
/// Append a revision; deliberately does not compare the comment's current revision or body.
/// # Errors
/// Returns sanitized database or argument-encoding failures.
pub async fn add_revision(
    conn: &mut PgConnection,
    comment_id: CommentId,
    revision: &BigInt,
    body: &str,
    author: &UserPrincipal,
) -> Result<(), RepositoryError> {
    text(body)?;
    if let Some(client) = &author.via.client {
        text(client)?;
    }
    let revision = Integer::new(revision)?;
    source!(
        sqlx::query!(
            "INSERT INTO comment_revisions(comment_id,revision,body_markdown,via_channel,via_client) VALUES($1,$2,$3,$4,$5)",
            comment_id as _,
            revision as _,
            body,
            channel(author.via.channel),
            author.via.client.as_deref()
        ),
        execute,
        conn
    )?;
    Ok(())
}
/// Replace the body as the next revision. Caller must already hold its row lock.
/// # Errors
/// Returns sanitized database/encoding failures or the source's missing-row assertion failure.
pub async fn edit_comment(
    conn: &mut PgConnection,
    comment_id: CommentId,
    body: &str,
) -> Result<i32, RepositoryError> {
    text(body)?;
    let row=source!(sqlx::query!("UPDATE comments SET body_markdown=$1,revision=revision+1,edited_at=now() WHERE id=$2 RETURNING revision",body,comment_id as _),fetch_optional,conn)?.ok_or(RepositoryError::Invariant)?;
    Ok(row.revision)
}
/// Optionally lock the exact row before executing the separate joined read.
/// # Errors
/// Returns sanitized database failures.
pub async fn get_comment(
    conn: &mut PgConnection,
    project_id: ProjectId,
    comment_id: CommentId,
    lock: bool,
) -> Result<Option<Comment>, RepositoryError> {
    if lock {
        source!(
            sqlx::query!(
                "SELECT 1 AS ignored FROM comments WHERE id=$1 AND project_id=$2 FOR UPDATE",
                comment_id as _,
                project_id as _
            ),
            fetch_all,
            &mut *conn
        )?;
    }
    source!(
        sqlx::query_file_as!(
            Comment,
            "src/sql/get_comment.sql",
            comment_id as _,
            project_id as _
        ),
        fetch_optional,
        conn
    )
}
/// Newest first; missing/foreign-unit cursors yield no rows.
/// # Errors
/// Returns sanitized database or integer-encoding failures.
pub async fn list_comments(
    conn: &mut PgConnection,
    unit_id: UnitId,
    attempt_id: Option<AttemptId>,
    before: Option<CommentId>,
    limit: Option<&BigInt>,
) -> Result<Vec<Comment>, RepositoryError> {
    let limit = limit.map(Integer::new).transpose()?;
    source!(
        sqlx::query_file_as!(
            Comment,
            "src/sql/list_comments.sql",
            unit_id as _,
            attempt_id as _,
            before as _,
            limit as _
        ),
        fetch_all,
        conn
    )
}
/// Oldest revision first, preserving unconstrained historical channel labels.
/// # Errors
/// Returns sanitized database or integer-encoding failures.
pub async fn list_revisions(
    conn: &mut PgConnection,
    comment_id: CommentId,
    after: Option<&BigInt>,
    limit: Option<&BigInt>,
) -> Result<Vec<CommentRevision>, RepositoryError> {
    let after = after.map(Integer::new).transpose()?;
    let limit = limit.map(Integer::new).transpose()?;
    source!(
        sqlx::query_file_as!(
            CommentRevision,
            "src/sql/list_revisions.sql",
            comment_id as _,
            after as _,
            limit as _
        ),
        fetch_all,
        conn
    )
}
