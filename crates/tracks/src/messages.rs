//! Questions, answers and steering notes SQL. Callers own transactions,
//! locks, validation and audit.
//!
//! A performer asks a question about its unit; a blocking one stops its
//! lease clock until a researcher answers or escalates it. A researcher
//! answers a question, escalates it into a plan concern, or posts a
//! steering note to a running attempt. Messages are written once; a
//! question only closes, and an answer or a steering note is only
//! acknowledged by the performer it was meant for.
use crate::repo::TrackError;
use cannery_core::{
    ids::{
        AttemptId, ConcernId, JobId, MessageId, ProjectId, ServiceAccountId, TrackId, UnitId,
        UserId,
    },
    timestamps::Timestamp,
};
use sqlx::PgConnection;

/// One message, with where it belongs, who wrote it, and the answer of a
/// question once given.
#[derive(Clone, Debug)]
pub struct Message {
    pub id: MessageId,
    pub project_id: ProjectId,
    pub track_slug: String,
    pub unit_id: UnitId,
    pub unit_number: i32,
    pub attempt_id: AttemptId,
    pub attempt_sequence: i32,
    pub job_id: Option<JobId>,
    pub job_phase: Option<String>,
    pub kind: String,
    pub blocking: Option<bool>,
    pub default_text: Option<String>,
    pub body: String,
    pub sha256: String,
    pub question_id: Option<MessageId>,
    pub author_user: Option<UserId>,
    pub author_service: Option<ServiceAccountId>,
    pub author_name: Option<String>,
    pub via_channel: String,
    pub via_client: Option<String>,
    pub created_at: Timestamp,
    pub state: Option<String>,
    pub closed_at: Option<Timestamp>,
    pub concern_id: Option<ConcernId>,
    pub released_at: Option<Timestamp>,
    pub acknowledged_at: Option<Timestamp>,
    pub answer_id: Option<MessageId>,
    pub answer_body: Option<String>,
    pub answer_author: Option<UserId>,
    pub answer_author_name: Option<String>,
    pub answer_created_at: Option<Timestamp>,
    pub answer_acknowledged_at: Option<Timestamp>,
}

/// What a new message stores.
pub struct NewMessage<'a> {
    pub project_id: ProjectId,
    pub unit_id: UnitId,
    pub attempt_id: AttemptId,
    pub job_id: Option<JobId>,
    pub kind: &'a str,
    pub blocking: Option<bool>,
    pub default_text: Option<&'a str>,
    pub body: &'a str,
    pub sha256: &'a str,
    pub question_id: Option<MessageId>,
    pub author_user: Option<UserId>,
    pub author_service: Option<ServiceAccountId>,
    pub via_channel: &'a str,
    pub via_client: Option<&'a str>,
}

/// Which messages a read selects: of the project, then of one message, one
/// track, one unit, one attempt, one kind or one question state when given,
/// written before the message `before`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Filter<'a> {
    pub id: Option<MessageId>,
    pub track: Option<&'a str>,
    pub unit: Option<UnitId>,
    pub attempt: Option<AttemptId>,
    pub kind: Option<&'a str>,
    pub state: Option<&'a str>,
    pub before: Option<MessageId>,
}

/// The project's messages the filter selects, newest first.
/// # Errors
/// Returns a sanitized database error.
pub async fn select(
    conn: &mut PgConnection,
    project: ProjectId,
    filter: Filter<'_>,
    limit: i64,
) -> Result<Vec<Message>, TrackError> {
    Ok(sqlx::query_as!(
        Message,
        r#"SELECT m.id AS "id!: _", m.project_id AS "project_id!: _", t.slug AS track_slug,
        m.unit_id AS "unit_id!: _", h.number AS unit_number,
        m.attempt_id AS "attempt_id!: _", a.sequence AS attempt_sequence,
        m.job_id AS "job_id?: _", j.phase AS "job_phase?", m.kind, m.blocking AS "blocking?",
        m.default_text AS "default_text?", m.body, m.sha256,
        m.question_id AS "question_id?: _", m.author_user AS "author_user?: _",
        m.author_service AS "author_service?: _",
        coalesce(u.display_name, u.email, s.name) AS "author_name?",
        m.via_channel, m.via_client AS "via_client?", m.created_at AS "created_at!: _",
        m.state AS "state?", m.closed_at AS "closed_at?: _", m.concern_id AS "concern_id?: _",
        m.released_at AS "released_at?: _", m.acknowledged_at AS "acknowledged_at?: _",
        r.id AS "answer_id?: _", r.body AS "answer_body?", r.author_user AS "answer_author?: _",
        coalesce(ru.display_name, ru.email) AS "answer_author_name?",
        r.created_at AS "answer_created_at?: _", r.acknowledged_at AS "answer_acknowledged_at?: _"
        FROM messages m JOIN units h ON h.id = m.unit_id JOIN tracks t ON t.id = h.track_id
        JOIN attempts a ON a.id = m.attempt_id
        LEFT JOIN jobs j ON j.id = m.job_id
        LEFT JOIN users u ON u.id = m.author_user
        LEFT JOIN service_accounts s ON s.id = m.author_service
        LEFT JOIN messages r ON r.question_id = m.id AND r.kind = 'answer'
        LEFT JOIN users ru ON ru.id = r.author_user
        WHERE m.project_id = $1 AND ($2::uuid IS NULL OR m.id = $2)
          AND ($3::text IS NULL OR t.slug = $3) AND ($4::uuid IS NULL OR m.unit_id = $4)
          AND ($5::uuid IS NULL OR m.attempt_id = $5) AND ($6::text IS NULL OR m.kind = $6)
          AND ($7::text IS NULL OR m.state = $7)
          AND ($8::uuid IS NULL OR (m.created_at, m.id) < (
              SELECT b.created_at, b.id FROM messages b WHERE b.id = $8 AND b.project_id = $1))
        ORDER BY m.created_at DESC, m.id DESC LIMIT $9"#,
        project as ProjectId,
        filter.id as Option<MessageId>,
        filter.track,
        filter.unit as Option<UnitId>,
        filter.attempt as Option<AttemptId>,
        filter.kind,
        filter.state,
        filter.before as Option<MessageId>,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// One message of the project, locked when asked.
/// # Errors
/// Returns a sanitized database error.
pub async fn get(
    conn: &mut PgConnection,
    project: ProjectId,
    id: MessageId,
    lock: bool,
) -> Result<Option<Message>, TrackError> {
    if lock {
        sqlx::query!(
            r#"SELECT id AS "id!: MessageId" FROM messages WHERE project_id = $1 AND id = $2 FOR UPDATE"#,
            project as ProjectId,
            id as MessageId
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

/// Store a new message; a question starts open.
/// # Errors
/// Returns a sanitized database error.
pub async fn insert(
    conn: &mut PgConnection,
    message: NewMessage<'_>,
) -> Result<MessageId, TrackError> {
    Ok(sqlx::query_scalar!(
        r#"INSERT INTO messages (project_id, unit_id, attempt_id, job_id, kind, blocking,
        default_text, body, sha256, question_id, author_user, author_service, via_channel,
        via_client, state)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
                CASE WHEN $5 = 'question' THEN 'open' END)
        RETURNING id AS "id!: MessageId""#,
        message.project_id as ProjectId,
        message.unit_id as UnitId,
        message.attempt_id as AttemptId,
        message.job_id as Option<JobId>,
        message.kind,
        message.blocking,
        message.default_text,
        message.body,
        message.sha256,
        message.question_id as Option<MessageId>,
        message.author_user as Option<UserId>,
        message.author_service as Option<ServiceAccountId>,
        message.via_channel,
        message.via_client
    )
    .fetch_one(conn)
    .await?)
}

/// Close an open question as `answered`, or as `escalated` into `concern`.
/// # Errors
/// Returns a sanitized database error.
pub async fn close(
    conn: &mut PgConnection,
    id: MessageId,
    concern: Option<ConcernId>,
) -> Result<bool, TrackError> {
    Ok(sqlx::query!(
        r#"UPDATE messages SET closed_at = now(), concern_id = $2,
        state = CASE WHEN $2::uuid IS NULL THEN 'answered' ELSE 'escalated' END
        WHERE id = $1 AND kind = 'question' AND state = 'open'"#,
        id as MessageId,
        concern as Option<ConcernId>
    )
    .execute(conn)
    .await?
    .rows_affected()
        == 1)
}

/// What the performer of an attempt (`job` none) or of a job has not
/// acknowledged yet, oldest first: answers to its questions; for an
/// attempt, also the steering notes posted to it and the answers to
/// questions of earlier attempts of its unit that were released unanswered.
/// # Errors
/// Returns a sanitized database error.
pub async fn pending(
    conn: &mut PgConnection,
    project: ProjectId,
    attempt: AttemptId,
    job: Option<JobId>,
) -> Result<Vec<Message>, TrackError> {
    let ids = sqlx::query_scalar!(
        r#"SELECT m.id AS "id!: MessageId" FROM messages m
        LEFT JOIN messages q ON q.id = m.question_id
        WHERE m.acknowledged_at IS NULL AND (
            (m.kind = 'steer' AND m.attempt_id = $1 AND $2::uuid IS NULL)
            OR (m.kind = 'answer' AND q.attempt_id = $1 AND q.job_id IS NOT DISTINCT FROM $2)
            OR (m.kind = 'answer' AND $2::uuid IS NULL AND q.job_id IS NULL
                AND q.released_at IS NOT NULL AND q.attempt_id <> $1
                AND q.unit_id = (SELECT unit_id FROM attempts WHERE id = $1)))
        ORDER BY m.created_at, m.id"#,
        attempt as AttemptId,
        job as Option<JobId>
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut found = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(message) = get(conn, project, id, false).await? {
            found.push(message);
        }
    }
    Ok(found)
}

/// Acknowledge, as the identity `user` or `service`, those of `ids` meant
/// for it: a steering note posted to an attempt it claimed, an answer to a
/// question it asked, or an answer to a released question of a unit whose
/// live attempt it holds. Returns the ones newly acknowledged.
/// # Errors
/// Returns a sanitized database error.
pub async fn acknowledge(
    conn: &mut PgConnection,
    project: ProjectId,
    ids: &[uuid::Uuid],
    user: Option<UserId>,
    service: Option<ServiceAccountId>,
) -> Result<Vec<MessageId>, TrackError> {
    Ok(sqlx::query_scalar!(
        r#"UPDATE messages m SET acknowledged_at = now()
        WHERE m.project_id = $1 AND m.id = ANY($2::uuid[]) AND m.acknowledged_at IS NULL AND (
            (m.kind = 'steer' AND EXISTS (SELECT 1 FROM attempts a WHERE a.id = m.attempt_id
                AND a.claimed_by_user IS NOT DISTINCT FROM $3
                AND a.claimed_by_service IS NOT DISTINCT FROM $4))
            OR (m.kind = 'answer' AND EXISTS (SELECT 1 FROM messages q WHERE q.id = m.question_id
                AND ((q.author_user IS NOT DISTINCT FROM $3
                      AND q.author_service IS NOT DISTINCT FROM $4)
                     OR (q.released_at IS NOT NULL AND EXISTS (
                         SELECT 1 FROM attempts a WHERE a.unit_id = q.unit_id
                           AND a.state IN ('claimed', 'running', 'waiting_on_human')
                           AND a.claimed_by_user IS NOT DISTINCT FROM $3
                           AND a.claimed_by_service IS NOT DISTINCT FROM $4))))))
        RETURNING m.id AS "id!: MessageId""#,
        project as ProjectId,
        ids as _,
        user as Option<UserId>,
        service as Option<ServiceAccountId>
    )
    .fetch_all(conn)
    .await?)
}

/// Those of `ids` that are messages of the project, with whether each was
/// already acknowledged.
/// # Errors
/// Returns a sanitized database error.
pub async fn acknowledged(
    conn: &mut PgConnection,
    project: ProjectId,
    ids: &[uuid::Uuid],
) -> Result<Vec<(MessageId, bool)>, TrackError> {
    Ok(sqlx::query!(
        r#"SELECT id AS "id!: MessageId", acknowledged_at IS NOT NULL AS "done!"
        FROM messages WHERE project_id = $1 AND id = ANY($2::uuid[])"#,
        project as ProjectId,
        ids as _
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| (row.id, row.done))
    .collect())
}

/// Whether a blocking question asked under the attempt's lease (`job`
/// none) or the job's is still open and not released.
/// # Errors
/// Returns a sanitized database error.
pub async fn waiting(
    conn: &mut PgConnection,
    attempt: AttemptId,
    job: Option<JobId>,
) -> Result<bool, TrackError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM messages WHERE attempt_id = $1
          AND job_id IS NOT DISTINCT FROM $2 AND kind = 'question' AND blocking
          AND state = 'open' AND released_at IS NULL) AS "found!""#,
        attempt as AttemptId,
        job as Option<JobId>
    )
    .fetch_one(conn)
    .await?)
}

/// Stop an attempt's lease clock: it waits on a person.
/// # Errors
/// Returns a sanitized database error.
pub async fn pause_attempt(conn: &mut PgConnection, attempt: AttemptId) -> Result<(), TrackError> {
    let paused = sqlx::query!(
        r#"UPDATE attempts SET state = 'waiting_on_human', started_at = coalesce(started_at, now())
        WHERE id = $1 AND state IN ('claimed', 'running', 'waiting_on_human')"#,
        attempt as AttemptId
    )
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if paused == 1 {
        sqlx::query!(
            r#"INSERT INTO lease_pauses (attempt_id) VALUES ($1)
            ON CONFLICT (attempt_id) WHERE job_id IS NULL DO NOTHING"#,
            attempt as AttemptId
        )
        .execute(conn)
        .await?;
    }
    Ok(())
}

/// Restart a waiting attempt's lease clock once no blocking question of it
/// is open: a fresh lease of `ttl` seconds, and its deadline moved by the
/// time it waited. Returns whether it resumed.
/// # Errors
/// Returns a sanitized database error.
pub async fn resume_attempt(
    conn: &mut PgConnection,
    attempt: AttemptId,
    ttl: f64,
) -> Result<bool, TrackError> {
    Ok(sqlx::query!(
        r#"WITH resumed AS (
            DELETE FROM lease_pauses p WHERE p.attempt_id = $1 AND p.job_id IS NULL
              AND NOT EXISTS (
                SELECT 1 FROM messages WHERE attempt_id = $1 AND job_id IS NULL
                  AND kind = 'question' AND blocking AND state = 'open' AND released_at IS NULL)
            RETURNING p.paused_at)
        UPDATE attempts a SET state = 'running',
            lease_expires_at = now() + make_interval(secs => $2),
            deadline = a.deadline + (now() - r.paused_at)
        FROM resumed r
        WHERE a.id = $1 AND a.state = 'waiting_on_human'"#,
        attempt as AttemptId,
        ttl
    )
    .execute(conn)
    .await?
    .rows_affected()
        == 1)
}

/// Stop a claimed job's lease clock.
/// # Errors
/// Returns a sanitized database error.
pub async fn pause_job(conn: &mut PgConnection, job: JobId) -> Result<(), TrackError> {
    sqlx::query!(
        r#"INSERT INTO lease_pauses (attempt_id, job_id)
        SELECT attempt_id, id FROM jobs WHERE id = $1 AND state = 'claimed'
        ON CONFLICT (job_id) WHERE job_id IS NOT NULL DO NOTHING"#,
        job as JobId
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Restart a paused job's lease clock once no blocking question of it is
/// open, as `resume_attempt` does. Returns whether it resumed.
/// # Errors
/// Returns a sanitized database error.
pub async fn resume_job(conn: &mut PgConnection, job: JobId, ttl: f64) -> Result<bool, TrackError> {
    Ok(sqlx::query!(
        r#"WITH resumed AS (
            DELETE FROM lease_pauses p WHERE p.job_id = $1
              AND NOT EXISTS (
                SELECT 1 FROM messages WHERE job_id = $1 AND kind = 'question'
                  AND blocking AND state = 'open' AND released_at IS NULL)
            RETURNING p.paused_at)
        UPDATE jobs j SET lease_expires_at = now() + make_interval(secs => $2),
            deadline = j.deadline + (now() - r.paused_at)
        FROM resumed r
        WHERE j.id = $1 AND j.state = 'claimed'"#,
        job as JobId,
        ttl
    )
    .execute(conn)
    .await?
    .rows_affected()
        == 1)
}

/// When the job's lease clock stopped, if it is paused.
/// # Errors
/// Returns a sanitized database error.
pub async fn job_paused_at(
    conn: &mut PgConnection,
    job: JobId,
) -> Result<Option<Timestamp>, TrackError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT paused_at AS "paused_at?: Timestamp" FROM lease_pauses WHERE job_id = $1"#,
        job as JobId
    )
    .fetch_optional(conn)
    .await?
    .flatten())
}

/// A blocking question that waited longer than its project allows, with
/// the attempt or job it holds.
#[derive(Clone, Debug)]
pub struct Overdue {
    pub id: MessageId,
    pub project_id: ProjectId,
    pub attempt_id: AttemptId,
    pub job_id: Option<JobId>,
}

/// Up to `limit` overdue blocking questions whose attempt or job still
/// waits on them, oldest first, skipping `seen`.
/// # Errors
/// Returns a sanitized database error.
pub async fn overdue(
    conn: &mut PgConnection,
    seen: &[uuid::Uuid],
    limit: i64,
) -> Result<Vec<Overdue>, TrackError> {
    Ok(sqlx::query_as!(
        Overdue,
        r#"SELECT m.id AS "id!: _", m.project_id AS "project_id!: _",
        m.attempt_id AS "attempt_id!: _", m.job_id AS "job_id?: _"
        FROM messages m
        LEFT JOIN project_limits l ON l.project_id = m.project_id
        JOIN attempts a ON a.id = m.attempt_id
        LEFT JOIN jobs j ON j.id = m.job_id
        WHERE m.kind = 'question' AND m.blocking AND m.state = 'open' AND m.released_at IS NULL
          AND m.created_at + make_interval(secs => coalesce(l.question_wait_seconds, 86400))
              <= now()
          AND ((m.job_id IS NULL AND a.state = 'waiting_on_human')
               OR (j.state = 'claimed' AND EXISTS (
                   SELECT 1 FROM lease_pauses p WHERE p.job_id = j.id)))
          AND m.id <> ALL($1::uuid[])
        ORDER BY m.created_at, m.id LIMIT $2"#,
        seen as _,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// Mark the open blocking questions of an attempt's lease (`job` none) or
/// of a job as released; returns their ids.
/// # Errors
/// Returns a sanitized database error.
pub async fn release(
    conn: &mut PgConnection,
    attempt: AttemptId,
    job: Option<JobId>,
) -> Result<Vec<MessageId>, TrackError> {
    sqlx::query!(
        "DELETE FROM lease_pauses WHERE attempt_id = $1 AND job_id IS NOT DISTINCT FROM $2",
        attempt as AttemptId,
        job as Option<JobId>
    )
    .execute(&mut *conn)
    .await?;
    Ok(sqlx::query_scalar!(
        r#"UPDATE messages SET released_at = now()
        WHERE attempt_id = $1 AND job_id IS NOT DISTINCT FROM $2 AND kind = 'question'
          AND blocking AND state = 'open' AND released_at IS NULL
        RETURNING id AS "id!: MessageId""#,
        attempt as AttemptId,
        job as Option<JobId>
    )
    .fetch_all(conn)
    .await?)
}

/// The questions asked during the unit's attempts before `sequence`,
/// oldest first, with their answers.
/// # Errors
/// Returns a sanitized database error.
pub async fn earlier_questions(
    conn: &mut PgConnection,
    project: ProjectId,
    unit: UnitId,
    sequence: i32,
) -> Result<Vec<Message>, TrackError> {
    let filter = Filter {
        unit: Some(unit),
        kind: Some("question"),
        ..Filter::default()
    };
    let mut found = select(conn, project, filter, i64::MAX).await?;
    found.retain(|message| message.attempt_sequence < sequence);
    found.reverse();
    Ok(found)
}

/// The track a unit belongs to: its id, slug and state.
/// # Errors
/// Returns a sanitized database error.
pub async fn unit_track(
    conn: &mut PgConnection,
    unit: UnitId,
) -> Result<(TrackId, String, String), TrackError> {
    let row = sqlx::query!(
        r#"SELECT t.id AS "id!: TrackId", t.slug, t.state FROM units h
        JOIN tracks t ON t.id = h.track_id WHERE h.id = $1"#,
        unit as UnitId
    )
    .fetch_one(conn)
    .await?;
    Ok((row.id, row.slug, row.state))
}

/// Every message of an attempt, oldest first.
/// # Errors
/// Returns a sanitized database error.
pub async fn of_attempt(
    conn: &mut PgConnection,
    project: ProjectId,
    attempt: AttemptId,
) -> Result<Vec<Message>, TrackError> {
    let filter = Filter {
        attempt: Some(attempt),
        ..Filter::default()
    };
    let mut found = select(conn, project, filter, i64::MAX).await?;
    found.reverse();
    Ok(found)
}
