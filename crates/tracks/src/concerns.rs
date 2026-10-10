//! Plan concern SQL. Callers own transactions, locks, validation and audit.
//!
//! A concern is raised once on a track's plan and is then immutable but for
//! closing it: a plan revision answers it on approval, or a researcher
//! dismisses it with a reason. While one is open, no new unit of its
//! track is claimed.
use crate::repo::TrackError;
use cannery_core::{
    ids::{
        AttemptId, ConcernId, PlanRevisionId, ProjectId, ServiceAccountId, TrackId, UnitId, UserId,
    },
    timestamps::Timestamp,
};
use sqlx::PgConnection;

/// One concern, with the names of the identities that raised and closed it.
#[derive(Clone, Debug)]
pub struct Concern {
    pub id: ConcernId,
    pub project_id: ProjectId,
    pub track_id: TrackId,
    pub track_slug: String,
    pub kind: String,
    pub unit_id: Option<UnitId>,
    pub unit_number: Option<i32>,
    pub attempt_sequence: Option<i32>,
    /// The front matter as JSON text.
    pub front_matter: String,
    pub body: String,
    pub sha256: String,
    pub raised_by_user: Option<UserId>,
    pub raised_by_user_name: Option<String>,
    pub raised_by_service: Option<ServiceAccountId>,
    pub raised_by_service_name: Option<String>,
    pub via_channel: String,
    pub via_client: Option<String>,
    pub raised_at: Timestamp,
    pub state: String,
    pub answered_by_revision: Option<i32>,
    pub dismissed_by: Option<UserId>,
    pub dismissed_by_name: Option<String>,
    pub dismissal_reason: Option<String>,
    pub closed_at: Option<Timestamp>,
}

/// What a new concern stores.
pub struct NewConcern<'a> {
    pub project_id: ProjectId,
    pub track_id: TrackId,
    pub kind: &'a str,
    pub unit_id: Option<UnitId>,
    pub attempt_id: Option<AttemptId>,
    /// The front matter as JSON text.
    pub front_matter: &'a str,
    pub body: &'a str,
    pub sha256: &'a str,
    pub raised_by_user: Option<UserId>,
    pub raised_by_service: Option<ServiceAccountId>,
    pub via_channel: &'a str,
    pub via_client: Option<&'a str>,
}

/// One entry of a plan revision's answers, with the concern it answers.
#[derive(Clone, Debug)]
pub struct Answer {
    pub concern_id: ConcernId,
    pub kind: String,
    pub state: String,
    pub how: String,
}

/// Which concerns a read selects: of the project, then of one concern, one
/// track or one state when given, raised before the concern `before`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Filter<'a> {
    pub id: Option<ConcernId>,
    pub track: Option<TrackId>,
    pub state: Option<&'a str>,
    pub before: Option<ConcernId>,
}

/// The project's concerns the filter selects, newest first.
/// # Errors
/// Returns a sanitized database error.
pub async fn select(
    conn: &mut PgConnection,
    project: ProjectId,
    filter: Filter<'_>,
    limit: i64,
) -> Result<Vec<Concern>, TrackError> {
    Ok(sqlx::query_as!(
        Concern,
        r#"SELECT c.id AS "id!: _", c.project_id AS "project_id!: _",
        c.track_id AS "track_id!: _", t.slug AS track_slug, c.kind,
        c.unit_id AS "unit_id?: _", h.number AS "unit_number?",
        a.sequence AS "attempt_sequence?", c.front_matter::text AS "front_matter!", c.body,
        c.sha256, c.raised_by_user AS "raised_by_user?: _",
        coalesce(u.display_name, u.email) AS "raised_by_user_name?",
        c.raised_by_service AS "raised_by_service?: _", s.name AS "raised_by_service_name?",
        c.via_channel, c.via_client, c.raised_at AS "raised_at!: _", c.state,
        p.revision AS "answered_by_revision?", c.dismissed_by AS "dismissed_by?: _",
        coalesce(d.display_name, d.email) AS "dismissed_by_name?", c.dismissal_reason,
        c.closed_at AS "closed_at?: _"
        FROM concerns c JOIN tracks t ON t.id = c.track_id
        LEFT JOIN units h ON h.id = c.unit_id
        LEFT JOIN attempts a ON a.id = c.attempt_id
        LEFT JOIN users u ON u.id = c.raised_by_user
        LEFT JOIN service_accounts s ON s.id = c.raised_by_service
        LEFT JOIN plan_revisions p ON p.id = c.answered_by
        LEFT JOIN users d ON d.id = c.dismissed_by
        WHERE c.project_id = $1 AND ($2::uuid IS NULL OR c.id = $2)
          AND ($3::uuid IS NULL OR c.track_id = $3) AND ($4::text IS NULL OR c.state = $4)
          AND ($5::uuid IS NULL OR (c.raised_at, c.id) < (
              SELECT b.raised_at, b.id FROM concerns b WHERE b.id = $5 AND b.project_id = $1))
        ORDER BY c.raised_at DESC, c.id DESC LIMIT $6"#,
        project as ProjectId,
        filter.id as Option<ConcernId>,
        filter.track as Option<TrackId>,
        filter.state,
        filter.before as Option<ConcernId>,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// One concern of the project, locked when asked.
/// # Errors
/// Returns a sanitized database error.
pub async fn get(
    conn: &mut PgConnection,
    project: ProjectId,
    id: ConcernId,
    lock: bool,
) -> Result<Option<Concern>, TrackError> {
    if lock {
        sqlx::query!(
            r#"SELECT id AS "id!: ConcernId" FROM concerns WHERE project_id = $1 AND id = $2 FOR UPDATE"#,
            project as ProjectId,
            id as ConcernId
        )
        .fetch_optional(&mut *conn)
        .await?;
    }
    let filter = Filter {
        id: Some(id),
        ..Filter::default()
    };
    Ok(select(conn, project, filter, 1).await?.pop())
}

/// The track's open concerns, oldest first.
/// # Errors
/// Returns a sanitized database error.
pub async fn open_for_track(
    conn: &mut PgConnection,
    project: ProjectId,
    track: TrackId,
) -> Result<Vec<Concern>, TrackError> {
    let filter = Filter {
        track: Some(track),
        state: Some("open"),
        ..Filter::default()
    };
    let mut open = select(conn, project, filter, i64::MAX).await?;
    open.reverse();
    Ok(open)
}

/// The slugs of the project's tracks that an open concern blocks.
/// # Errors
/// Returns a sanitized database error.
pub async fn blocked_tracks(
    conn: &mut PgConnection,
    project: ProjectId,
) -> Result<Vec<String>, TrackError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT t.slug FROM tracks t WHERE t.project_id = $1
          AND EXISTS (SELECT 1 FROM concerns c WHERE c.track_id = t.id AND c.state = 'open')
          ORDER BY t.slug"#,
        project as ProjectId
    )
    .fetch_all(conn)
    .await?)
}

/// The slugs of the project's active tracks in `mode` whose queued
/// units wait only because an open concern blocks the track: of one
/// track, or holding one unit number, when given.
/// # Errors
/// Returns a sanitized database error.
pub async fn blocked_queued(
    conn: &mut PgConnection,
    project: ProjectId,
    mode: &str,
    number: Option<i32>,
    track: Option<&str>,
) -> Result<Vec<String>, TrackError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT DISTINCT t.slug FROM units h JOIN tracks t ON t.id = h.track_id
        WHERE h.project_id = $1 AND h.state = 'queued' AND t.state = 'active' AND t.mode = $2
          AND ($3::integer IS NULL OR h.number = $3) AND ($4::text IS NULL OR t.slug = $4)
          AND EXISTS (SELECT 1 FROM concerns c WHERE c.track_id = t.id AND c.state = 'open')
        ORDER BY t.slug"#,
        project as ProjectId,
        mode,
        number,
        track
    )
    .fetch_all(conn)
    .await?)
}

/// Whether an open concern blocks the project's track `slug`.
/// # Errors
/// Returns a sanitized database error.
pub async fn blocks(
    conn: &mut PgConnection,
    project: ProjectId,
    slug: &str,
) -> Result<bool, TrackError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM concerns c JOIN tracks t ON t.id = c.track_id
        WHERE t.project_id = $1 AND t.slug = $2 AND c.state = 'open') AS "found!""#,
        project as ProjectId,
        slug
    )
    .fetch_one(conn)
    .await?)
}

/// Store a new open concern.
/// # Errors
/// Returns a sanitized database error.
pub async fn insert(
    conn: &mut PgConnection,
    concern: NewConcern<'_>,
) -> Result<ConcernId, TrackError> {
    Ok(sqlx::query_scalar!(
        r#"INSERT INTO concerns (project_id, track_id, kind, unit_id, attempt_id,
        front_matter, body, sha256, raised_by_user, raised_by_service, via_channel, via_client)
        VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7, $8, $9, $10, $11, $12)
        RETURNING id AS "id!: ConcernId""#,
        concern.project_id as ProjectId,
        concern.track_id as TrackId,
        concern.kind,
        concern.unit_id as Option<UnitId>,
        concern.attempt_id as Option<AttemptId>,
        concern.front_matter,
        concern.body,
        concern.sha256,
        concern.raised_by_user as Option<UserId>,
        concern.raised_by_service as Option<ServiceAccountId>,
        concern.via_channel,
        concern.via_client
    )
    .fetch_one(conn)
    .await?)
}

/// Dismiss an open concern with a reason.
/// # Errors
/// Returns a sanitized database error.
pub async fn dismiss(
    conn: &mut PgConnection,
    id: ConcernId,
    by: UserId,
    reason: &str,
) -> Result<bool, TrackError> {
    Ok(sqlx::query!(
        r#"UPDATE concerns SET state = 'dismissed', dismissed_by = $2, dismissal_reason = $3,
        closed_at = now() WHERE id = $1 AND state = 'open'"#,
        id as ConcernId,
        by as UserId,
        reason
    )
    .execute(conn)
    .await?
    .rows_affected()
        == 1)
}

/// Every answer of a plan revision, oldest concern first.
/// # Errors
/// Returns a sanitized database error.
pub async fn answers(
    conn: &mut PgConnection,
    plan: PlanRevisionId,
) -> Result<Vec<Answer>, TrackError> {
    Ok(sqlx::query_as!(
        Answer,
        r#"SELECT c.id AS "concern_id!: _", c.kind, c.state, a.how
        FROM plan_answers a JOIN concerns c ON c.id = a.concern_id
        WHERE a.plan_revision_id = $1 ORDER BY c.raised_at, c.id"#,
        plan as PlanRevisionId
    )
    .fetch_all(conn)
    .await?)
}

/// Write or replace the draft's answer to a concern.
/// # Errors
/// Returns a sanitized database error.
pub async fn set_answer(
    conn: &mut PgConnection,
    plan: PlanRevisionId,
    concern: ConcernId,
    how: &str,
) -> Result<(), TrackError> {
    sqlx::query!(
        r#"INSERT INTO plan_answers (plan_revision_id, concern_id, how) VALUES ($1, $2, $3)
        ON CONFLICT (plan_revision_id, concern_id) DO UPDATE SET how = EXCLUDED.how"#,
        plan as PlanRevisionId,
        concern as ConcernId,
        how
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Remove the draft's answer to a concern.
/// # Errors
/// Returns a sanitized database error.
pub async fn delete_answer(
    conn: &mut PgConnection,
    plan: PlanRevisionId,
    concern: ConcernId,
) -> Result<bool, TrackError> {
    Ok(sqlx::query!(
        "DELETE FROM plan_answers WHERE plan_revision_id = $1 AND concern_id = $2",
        plan as PlanRevisionId,
        concern as ConcernId
    )
    .execute(conn)
    .await?
    .rows_affected()
        == 1)
}

/// Copy the answers of `from` to concerns still open into the draft `to`.
/// # Errors
/// Returns a sanitized database error.
pub async fn copy_answers(
    conn: &mut PgConnection,
    from: PlanRevisionId,
    to: PlanRevisionId,
) -> Result<u64, TrackError> {
    Ok(sqlx::query!(
        r#"INSERT INTO plan_answers (plan_revision_id, concern_id, how)
        SELECT $2, a.concern_id, a.how FROM plan_answers a JOIN concerns c ON c.id = a.concern_id
        WHERE a.plan_revision_id = $1 AND c.state = 'open'"#,
        from as PlanRevisionId,
        to as PlanRevisionId
    )
    .execute(conn)
    .await?
    .rows_affected())
}

/// Close the open concerns an approved revision answers.
/// # Errors
/// Returns a sanitized database error.
pub async fn answer_open(
    conn: &mut PgConnection,
    plan: PlanRevisionId,
) -> Result<Vec<ConcernId>, TrackError> {
    Ok(sqlx::query_scalar!(
        r#"UPDATE concerns SET state = 'answered', answered_by = $1, closed_at = now()
        WHERE state = 'open' AND id IN (
            SELECT concern_id FROM plan_answers WHERE plan_revision_id = $1)
        RETURNING id AS "id!: ConcernId""#,
        plan as PlanRevisionId
    )
    .fetch_all(conn)
    .await?)
}
