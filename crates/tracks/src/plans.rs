//! Track plan SQL. Callers own transactions, locks, validation and audit.
//!
//! A plan is a chain of revisions per track. A revision is a draft until it
//! is submitted; its entries (one per unit) and its alignment entries change
//! only while it is a draft. Approval writes the units it lists.
use crate::repo::TrackError;
use cannery_core::{
    ids::{AttemptId, PlanRevisionId, ProjectId, ReviewCaseId, TrackId, UnitId, UserId},
    timestamps::Timestamp,
};
use sqlx::PgConnection;
use uuid::Uuid;

/// Unit states of a unit that is running or waiting for its decision.
pub const IN_FLIGHT: [&str; 3] = ["active", "documenting", "deciding"];
/// Unit states of a decided unit.
pub const DONE: [&str; 4] = ["promoted", "rejected", "inconclusive", "failed"];

/// One plan revision, with the names of the people who wrote and reviewed it.
#[derive(Clone, Debug)]
pub struct PlanRevision {
    pub id: PlanRevisionId,
    pub project_id: ProjectId,
    pub track_id: TrackId,
    pub revision: i32,
    pub state: String,
    pub based_on: Option<i32>,
    pub approach: String,
    pub created_by: UserId,
    pub created_by_name: Option<String>,
    pub via_channel: String,
    pub via_client: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub submitted_by: Option<UserId>,
    pub submitted_at: Option<Timestamp>,
    pub review_case_id: Option<ReviewCaseId>,
    pub reviewed_by: Option<UserId>,
    pub reviewed_by_name: Option<String>,
    pub review_reason: Option<String>,
    pub reviewed_at: Option<Timestamp>,
    pub units: i64,
}

/// One entry of a revision: a unit it adds, keeps or revises.
#[derive(Clone, Debug)]
pub struct PlanUnit {
    pub key: String,
    pub position: i32,
    pub unit_id: Option<UnitId>,
    pub number: Option<i32>,
    pub state: Option<String>,
    pub redo_of: Option<UnitId>,
    pub redo_of_number: Option<i32>,
    /// The structured fields as JSON text.
    pub fields: String,
    pub brief: String,
    pub science_revision: i32,
    pub unit_revision: Option<i32>,
}

/// One alignment entry, with the unit it concerns.
#[derive(Clone, Debug)]
pub struct Alignment {
    pub unit_id: UnitId,
    pub number: i32,
    pub title: String,
    pub state: String,
    pub decision: String,
    pub reason: String,
}

/// A unit of a track, as plans and context index see it.
#[derive(Clone, Debug)]
pub struct TrackUnit {
    pub unit_id: UnitId,
    pub number: i32,
    pub title: String,
    pub state: String,
    pub key: Option<String>,
    pub obsolete: bool,
    pub revision: i32,
    pub approved_revision: Option<i32>,
}

/// A unit revision as plans and context bundles read it.
#[derive(Clone, Debug)]
pub struct UnitRevision {
    pub revision: i32,
    /// The unit document as JSON text.
    pub content: String,
    pub brief: Option<String>,
    pub science_revision: i32,
    pub created_at: Timestamp,
    /// The plan revision that wrote it, when a plan did.
    pub plan_revision: Option<i32>,
}

/// What a new draft stores.
pub struct NewPlan<'a> {
    pub project_id: ProjectId,
    pub track_id: TrackId,
    pub based_on: Option<i32>,
    pub approach: &'a str,
    pub created_by: UserId,
    pub via_channel: &'a str,
    pub via_client: Option<&'a str>,
}

/// What an entry stores.
pub struct UnitEntry<'a> {
    pub key: &'a str,
    pub unit_id: Option<UnitId>,
    pub redo_of: Option<UnitId>,
    /// The structured fields as JSON text.
    pub fields: &'a str,
    pub brief: &'a str,
    pub science_revision: i32,
}

/// One revision of a track's plan.
/// # Errors
/// Returns a sanitized database error.
pub async fn get(
    conn: &mut PgConnection,
    track: TrackId,
    revision: i32,
) -> Result<Option<PlanRevision>, TrackError> {
    Ok(sqlx::query_as!(
        PlanRevision,
        r#"SELECT p.id AS "id!: _", p.project_id AS "project_id!: _", p.track_id AS "track_id!: _",
        p.revision, p.state, p.based_on, p.approach, p.created_by AS "created_by!: _",
        coalesce(c.display_name, c.email) AS "created_by_name?", p.via_channel, p.via_client,
        p.created_at AS "created_at!: _", p.updated_at AS "updated_at!: _",
        p.submitted_by AS "submitted_by?: _", p.submitted_at AS "submitted_at?: _",
        p.review_case_id AS "review_case_id?: _", p.reviewed_by AS "reviewed_by?: _",
        coalesce(r.display_name, r.email) AS "reviewed_by_name?", p.review_reason,
        p.reviewed_at AS "reviewed_at?: _",
        (SELECT count(*) FROM plan_units u WHERE u.plan_revision_id = p.id) AS "units!"
        FROM plan_revisions p JOIN users c ON c.id = p.created_by
        LEFT JOIN users r ON r.id = p.reviewed_by
        WHERE p.track_id = $1 AND p.revision = $2"#,
        track as TrackId,
        revision
    )
    .fetch_optional(conn)
    .await?)
}

/// The track's open revision (a draft or one under review), locked when asked.
/// # Errors
/// Returns a sanitized database error.
pub async fn open(
    conn: &mut PgConnection,
    track: TrackId,
    lock: bool,
) -> Result<Option<PlanRevision>, TrackError> {
    let revision = if lock {
        sqlx::query_scalar!(
            "SELECT revision FROM plan_revisions WHERE track_id = $1 AND state IN ('draft', 'submitted') FOR UPDATE",
            track as TrackId
        )
        .fetch_optional(&mut *conn)
        .await?
    } else {
        sqlx::query_scalar!(
            "SELECT revision FROM plan_revisions WHERE track_id = $1 AND state IN ('draft', 'submitted')",
            track as TrackId
        )
        .fetch_optional(&mut *conn)
        .await?
    };
    match revision {
        Some(revision) => get(conn, track, revision).await,
        None => Ok(None),
    }
}

/// The track's current plan: its latest approved revision number.
/// # Errors
/// Returns a sanitized database error.
pub async fn approved_revision(
    conn: &mut PgConnection,
    track: TrackId,
) -> Result<Option<i32>, TrackError> {
    Ok(sqlx::query_scalar!(
        "SELECT max(revision) FROM plan_revisions WHERE track_id = $1 AND state = 'approved'",
        track as TrackId
    )
    .fetch_one(conn)
    .await?)
}

/// The track's newest revision number, whatever its state.
/// # Errors
/// Returns a sanitized database error.
pub async fn latest_revision(
    conn: &mut PgConnection,
    track: TrackId,
) -> Result<Option<i32>, TrackError> {
    Ok(sqlx::query_scalar!(
        "SELECT max(revision) FROM plan_revisions WHERE track_id = $1",
        track as TrackId
    )
    .fetch_one(conn)
    .await?)
}

/// Whether any track of the project has an approved plan.
/// # Errors
/// Returns a sanitized database error.
pub async fn project_has_approved_plan(
    conn: &mut PgConnection,
    project: ProjectId,
) -> Result<bool, TrackError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM plan_revisions WHERE project_id = $1 AND state = 'approved') AS "found!""#,
        project as ProjectId
    )
    .fetch_one(conn)
    .await?)
}

/// Revisions newest first, before `before` when given.
/// # Errors
/// Returns a sanitized database error.
pub async fn list(
    conn: &mut PgConnection,
    track: TrackId,
    before: Option<i32>,
    limit: i64,
) -> Result<Vec<PlanRevision>, TrackError> {
    Ok(sqlx::query_as!(
        PlanRevision,
        r#"SELECT p.id AS "id!: _", p.project_id AS "project_id!: _", p.track_id AS "track_id!: _",
        p.revision, p.state, p.based_on, p.approach, p.created_by AS "created_by!: _",
        coalesce(c.display_name, c.email) AS "created_by_name?", p.via_channel, p.via_client,
        p.created_at AS "created_at!: _", p.updated_at AS "updated_at!: _",
        p.submitted_by AS "submitted_by?: _", p.submitted_at AS "submitted_at?: _",
        p.review_case_id AS "review_case_id?: _", p.reviewed_by AS "reviewed_by?: _",
        coalesce(r.display_name, r.email) AS "reviewed_by_name?", p.review_reason,
        p.reviewed_at AS "reviewed_at?: _",
        (SELECT count(*) FROM plan_units u WHERE u.plan_revision_id = p.id) AS "units!"
        FROM plan_revisions p JOIN users c ON c.id = p.created_by
        LEFT JOIN users r ON r.id = p.reviewed_by
        WHERE p.track_id = $1 AND ($2::integer IS NULL OR p.revision < $2)
        ORDER BY p.revision DESC LIMIT $3"#,
        track as TrackId,
        before,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// Open the next revision as a draft. Call holding the track row lock.
/// # Errors
/// Returns a sanitized database error; a second open revision violates
/// `plan_revisions_one_open_idx`.
pub async fn create(conn: &mut PgConnection, plan: NewPlan<'_>) -> Result<i32, TrackError> {
    Ok(sqlx::query_scalar!(
        r#"INSERT INTO plan_revisions (project_id, track_id, revision, based_on, approach,
        created_by, via_channel, via_client)
        SELECT $1, $2, coalesce(max(revision), 0) + 1, $3, $4, $5, $6, $7
        FROM plan_revisions WHERE track_id = $2
        RETURNING revision"#,
        plan.project_id as ProjectId,
        plan.track_id as TrackId,
        plan.based_on,
        plan.approach,
        plan.created_by as UserId,
        plan.via_channel,
        plan.via_client
    )
    .fetch_one(conn)
    .await?)
}

/// Copy the entries of `from` into the draft `to`: every new unit, and every
/// existing one that is still queued. Unit revisions are not copied.
/// # Errors
/// Returns a sanitized database error.
pub async fn copy_units(
    conn: &mut PgConnection,
    from: PlanRevisionId,
    to: PlanRevisionId,
) -> Result<u64, TrackError> {
    Ok(sqlx::query!(
        r#"INSERT INTO plan_units (plan_revision_id, key, position, unit_id, redo_of,
        fields, brief, science_revision)
        SELECT $2, u.key, u.position, u.unit_id,
        CASE WHEN u.unit_id IS NULL THEN u.redo_of END, u.fields, u.brief, u.science_revision
        FROM plan_units u LEFT JOIN units h ON h.id = u.unit_id
        WHERE u.plan_revision_id = $1 AND (u.unit_id IS NULL OR h.state = 'queued')"#,
        from as PlanRevisionId,
        to as PlanRevisionId
    )
    .execute(conn)
    .await?
    .rows_affected())
}

/// Replace the draft's approach.
/// # Errors
/// Returns a sanitized database error.
pub async fn set_approach(
    conn: &mut PgConnection,
    plan: PlanRevisionId,
    text: &str,
) -> Result<(), TrackError> {
    sqlx::query!(
        "UPDATE plan_revisions SET approach = $2, updated_at = now() WHERE id = $1 AND state = 'draft'",
        plan as PlanRevisionId,
        text
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Mark the draft as edited now.
/// # Errors
/// Returns a sanitized database error.
pub async fn touch(conn: &mut PgConnection, plan: PlanRevisionId) -> Result<(), TrackError> {
    sqlx::query!(
        "UPDATE plan_revisions SET updated_at = now() WHERE id = $1",
        plan as PlanRevisionId
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Every entry of a revision, in plan order.
/// # Errors
/// Returns a sanitized database error.
pub async fn units(
    conn: &mut PgConnection,
    plan: PlanRevisionId,
) -> Result<Vec<PlanUnit>, TrackError> {
    Ok(sqlx::query_as!(
        PlanUnit,
        r#"SELECT u.key, u.position, u.unit_id AS "unit_id?: _", h.number AS "number?",
        h.state AS "state?", u.redo_of AS "redo_of?: _", r.number AS "redo_of_number?",
        u.fields::text AS "fields!", u.brief, u.science_revision, u.unit_revision
        FROM plan_units u LEFT JOIN units h ON h.id = u.unit_id
        LEFT JOIN units r ON r.id = u.redo_of
        WHERE u.plan_revision_id = $1 ORDER BY u.position, u.key"#,
        plan as PlanRevisionId
    )
    .fetch_all(conn)
    .await?)
}

/// One entry of a revision, by key.
/// # Errors
/// Returns a sanitized database error.
pub async fn unit(
    conn: &mut PgConnection,
    plan: PlanRevisionId,
    key: &str,
) -> Result<Option<PlanUnit>, TrackError> {
    Ok(sqlx::query_as!(
        PlanUnit,
        r#"SELECT u.key, u.position, u.unit_id AS "unit_id?: _", h.number AS "number?",
        h.state AS "state?", u.redo_of AS "redo_of?: _", r.number AS "redo_of_number?",
        u.fields::text AS "fields!", u.brief, u.science_revision, u.unit_revision
        FROM plan_units u LEFT JOIN units h ON h.id = u.unit_id
        LEFT JOIN units r ON r.id = u.redo_of
        WHERE u.plan_revision_id = $1 AND u.key = $2"#,
        plan as PlanRevisionId,
        key
    )
    .fetch_optional(conn)
    .await?)
}

/// Add an entry at the end of the draft.
/// # Errors
/// Returns a sanitized database error; a duplicate key is a unique violation.
pub async fn insert_unit(
    conn: &mut PgConnection,
    plan: PlanRevisionId,
    entry: UnitEntry<'_>,
) -> Result<(), TrackError> {
    sqlx::query!(
        r#"INSERT INTO plan_units (plan_revision_id, key, position, unit_id, redo_of, fields,
        brief, science_revision)
        SELECT $1, $2, coalesce(max(position), 0) + 1, $3, $4, $5::text::jsonb, $6, $7
        FROM plan_units WHERE plan_revision_id = $1"#,
        plan as PlanRevisionId,
        entry.key,
        entry.unit_id as Option<UnitId>,
        entry.redo_of as Option<UnitId>,
        entry.fields,
        entry.brief,
        entry.science_revision
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Replace an entry's fields and brief.
/// # Errors
/// Returns a sanitized database error.
pub async fn update_unit(
    conn: &mut PgConnection,
    plan: PlanRevisionId,
    entry: UnitEntry<'_>,
) -> Result<bool, TrackError> {
    Ok(sqlx::query!(
        r#"UPDATE plan_units SET fields = $3::text::jsonb, brief = $4, science_revision = $5
        WHERE plan_revision_id = $1 AND key = $2"#,
        plan as PlanRevisionId,
        entry.key,
        entry.fields,
        entry.brief,
        entry.science_revision
    )
    .execute(conn)
    .await?
    .rows_affected()
        == 1)
}

/// Remove an entry from the draft.
/// # Errors
/// Returns a sanitized database error.
pub async fn delete_unit(
    conn: &mut PgConnection,
    plan: PlanRevisionId,
    key: &str,
) -> Result<bool, TrackError> {
    Ok(sqlx::query!(
        "DELETE FROM plan_units WHERE plan_revision_id = $1 AND key = $2",
        plan as PlanRevisionId,
        key
    )
    .execute(conn)
    .await?
    .rows_affected()
        == 1)
}

/// Remove the entries a `redo` alignment entry added.
/// # Errors
/// Returns a sanitized database error.
pub async fn delete_redo_units(
    conn: &mut PgConnection,
    plan: PlanRevisionId,
    redo_of: UnitId,
) -> Result<u64, TrackError> {
    Ok(sqlx::query!(
        "DELETE FROM plan_units WHERE plan_revision_id = $1 AND redo_of = $2",
        plan as PlanRevisionId,
        redo_of as UnitId
    )
    .execute(conn)
    .await?
    .rows_affected())
}

/// The key of every unit an approved plan of the track wrote, with the
/// unit number it names; a key reused later names its latest unit.
/// Later revisions keep only queued units as entries, so their entries name
/// the others by these keys.
/// # Errors
/// Returns a sanitized database error.
pub async fn known_keys(
    conn: &mut PgConnection,
    track: TrackId,
) -> Result<Vec<(String, UnitId, i32)>, TrackError> {
    Ok(sqlx::query!(
        r#"SELECT DISTINCT ON (u.key) u.key, h.id AS "unit_id!: UnitId", h.number
        FROM plan_units u JOIN plan_revisions p ON p.id = u.plan_revision_id
        JOIN units h ON h.id = u.unit_id
        WHERE p.track_id = $1 AND p.state = 'approved' ORDER BY u.key, p.revision DESC"#,
        track as TrackId
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| (row.key, row.unit_id, row.number))
    .collect())
}

/// Every alignment entry of a revision.
/// # Errors
/// Returns a sanitized database error.
pub async fn alignments(
    conn: &mut PgConnection,
    plan: PlanRevisionId,
) -> Result<Vec<Alignment>, TrackError> {
    Ok(sqlx::query_as!(
        Alignment,
        r#"SELECT a.unit_id AS "unit_id!: _", h.number, h.title, h.state, a.decision,
        a.reason FROM plan_alignments a JOIN units h ON h.id = a.unit_id
        WHERE a.plan_revision_id = $1 ORDER BY h.number"#,
        plan as PlanRevisionId
    )
    .fetch_all(conn)
    .await?)
}

/// Write or replace one alignment entry.
/// # Errors
/// Returns a sanitized database error.
pub async fn set_alignment(
    conn: &mut PgConnection,
    plan: PlanRevisionId,
    unit: UnitId,
    decision: &str,
    reason: &str,
) -> Result<(), TrackError> {
    sqlx::query!(
        r#"INSERT INTO plan_alignments (plan_revision_id, unit_id, decision, reason)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (plan_revision_id, unit_id)
        DO UPDATE SET decision = EXCLUDED.decision, reason = EXCLUDED.reason"#,
        plan as PlanRevisionId,
        unit as UnitId,
        decision,
        reason
    )
    .execute(conn)
    .await?;
    Ok(())
}

macro_rules! track_units {
    ($conn:expr, $track:expr, $states:expr, $before:expr, $limit:expr) => {
        sqlx::query_as!(
            TrackUnit,
            r#"SELECT h.id AS "unit_id!: _", h.number, h.title, h.state,
            (SELECT u.key FROM plan_units u JOIN plan_revisions p ON p.id = u.plan_revision_id
             WHERE u.unit_id = h.id AND p.state = 'approved'
             ORDER BY p.revision DESC LIMIT 1) AS "key?",
            EXISTS (SELECT 1 FROM plan_alignments a JOIN plan_revisions p ON p.id = a.plan_revision_id
                    WHERE a.unit_id = h.id AND p.state = 'approved'
                      AND a.decision IN ('obsolete', 'redo')) AS "obsolete!",
            h.revision, h.approved_revision
            FROM units h
            WHERE h.track_id = $1 AND ($2::text[] IS NULL OR h.state = ANY($2))
              AND ($3::integer IS NULL OR h.number < $3)
            ORDER BY h.number DESC LIMIT $4"#,
            $track as TrackId,
            $states,
            $before,
            $limit
        )
        .fetch_all($conn)
    };
}

/// The track's units, newest first, with their plan key and obsolete mark.
/// # Errors
/// Returns a sanitized database error.
pub async fn track_units(
    conn: &mut PgConnection,
    track: TrackId,
    states: Option<&[String]>,
    before: Option<i32>,
    limit: i64,
) -> Result<Vec<TrackUnit>, TrackError> {
    Ok(track_units!(conn, track, states, before, limit).await?)
}

/// The track's units that are done or in flight and not yet obsolete:
/// each needs an alignment entry in every new revision.
/// # Errors
/// Returns a sanitized database error.
pub async fn needing_alignment(
    conn: &mut PgConnection,
    track: TrackId,
) -> Result<Vec<TrackUnit>, TrackError> {
    let states: Vec<String> = IN_FLIGHT
        .iter()
        .chain(DONE.iter())
        .map(|state| (*state).to_owned())
        .collect();
    let mut units =
        track_units!(conn, track, Some(states.as_slice()), None::<i32>, i64::MAX).await?;
    units.retain(|unit| !unit.obsolete);
    units.reverse();
    Ok(units)
}

/// One unit of the project as a unit, by number.
/// # Errors
/// Returns a sanitized database error.
pub async fn project_unit(
    conn: &mut PgConnection,
    project: ProjectId,
    number: i32,
) -> Result<Option<(TrackId, TrackUnit)>, TrackError> {
    let row = sqlx::query!(
        r#"SELECT h.track_id AS "track_id!: TrackId", h.id AS "unit_id!: UnitId",
        h.number, h.title, h.state,
        (SELECT u.key FROM plan_units u JOIN plan_revisions p ON p.id = u.plan_revision_id
         WHERE u.unit_id = h.id AND p.state = 'approved'
         ORDER BY p.revision DESC LIMIT 1) AS "key?",
        EXISTS (SELECT 1 FROM plan_alignments a JOIN plan_revisions p ON p.id = a.plan_revision_id
                WHERE a.unit_id = h.id AND p.state = 'approved'
                  AND a.decision IN ('obsolete', 'redo')) AS "obsolete!",
        h.revision, h.approved_revision
        FROM units h WHERE h.project_id = $1 AND h.number = $2"#,
        project as ProjectId,
        number
    )
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|row| {
        (
            row.track_id,
            TrackUnit {
                unit_id: row.unit_id,
                number: row.number,
                title: row.title,
                state: row.state,
                key: row.key,
                obsolete: row.obsolete,
                revision: row.revision,
                approved_revision: row.approved_revision,
            },
        )
    }))
}

/// Every revision of a unit, oldest first, with the plan revision that
/// wrote each one.
/// # Errors
/// Returns a sanitized database error.
pub async fn unit_revisions(
    conn: &mut PgConnection,
    unit: UnitId,
) -> Result<Vec<UnitRevision>, TrackError> {
    Ok(sqlx::query_as!(
        UnitRevision,
        r#"SELECT r.revision, r.content::text AS "content!", r.brief, r.science_revision,
        r.created_at AS "created_at!: _",
        (SELECT p.revision FROM plan_units u JOIN plan_revisions p ON p.id = u.plan_revision_id
         WHERE u.unit_id = r.unit_id AND u.unit_revision = r.revision
           AND p.state = 'approved' LIMIT 1) AS "plan_revision?"
        FROM unit_revisions r WHERE r.unit_id = $1 ORDER BY r.revision"#,
        unit as UnitId
    )
    .fetch_all(conn)
    .await?)
}

/// One unit revision.
/// # Errors
/// Returns a sanitized database error.
pub async fn unit_revision(
    conn: &mut PgConnection,
    unit: UnitId,
    revision: i32,
) -> Result<Option<UnitRevision>, TrackError> {
    Ok(sqlx::query_as!(
        UnitRevision,
        r#"SELECT r.revision, r.content::text AS "content!", r.brief, r.science_revision,
        r.created_at AS "created_at!: _",
        (SELECT p.revision FROM plan_units u JOIN plan_revisions p ON p.id = u.plan_revision_id
         WHERE u.unit_id = r.unit_id AND u.unit_revision = r.revision
           AND p.state = 'approved' LIMIT 1) AS "plan_revision?"
        FROM unit_revisions r WHERE r.unit_id = $1 AND r.revision = $2"#,
        unit as UnitId,
        revision
    )
    .fetch_optional(conn)
    .await?)
}

/// Every alignment entry about a unit in an approved revision, oldest first.
/// # Errors
/// Returns a sanitized database error.
pub async fn unit_alignments(
    conn: &mut PgConnection,
    unit: UnitId,
) -> Result<Vec<(i32, String, String)>, TrackError> {
    Ok(sqlx::query!(
        r#"SELECT p.revision, a.decision, a.reason FROM plan_alignments a
        JOIN plan_revisions p ON p.id = a.plan_revision_id
        WHERE a.unit_id = $1 AND p.state = 'approved' ORDER BY p.revision"#,
        unit as UnitId
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| (row.revision, row.decision, row.reason))
    .collect())
}

/// Freeze the draft and attach its newly opened `plan` review case.
/// # Errors
/// Returns a sanitized database error.
pub async fn submit(
    conn: &mut PgConnection,
    plan: PlanRevisionId,
    project: ProjectId,
    revision: i32,
    by: UserId,
) -> Result<ReviewCaseId, TrackError> {
    let case = sqlx::query_scalar!(
        r#"INSERT INTO review_cases (project_id, unit_id, kind, subject_revision)
        VALUES ($1, NULL, 'plan', $2) RETURNING id AS "id!: ReviewCaseId""#,
        project as ProjectId,
        revision
    )
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query!(
        r#"UPDATE plan_revisions SET state = 'submitted', submitted_by = $2, submitted_at = now(),
        review_case_id = $3, updated_at = now() WHERE id = $1 AND state = 'draft'"#,
        plan as PlanRevisionId,
        by as UserId,
        case as ReviewCaseId
    )
    .execute(conn)
    .await?;
    Ok(case)
}

/// The review of a submitted revision.
pub struct Review<'a> {
    pub plan: PlanRevisionId,
    pub case: ReviewCaseId,
    pub revision: i32,
    /// `approve`, `send_back` or `decline`.
    pub action: &'a str,
    pub reason: &'a str,
    pub by: UserId,
    pub via_channel: &'a str,
    pub via_client: Option<&'a str>,
}

/// Record the decision on the plan case, resolve it and set the revision's state.
/// # Errors
/// Returns a sanitized database error.
pub async fn review(conn: &mut PgConnection, review: Review<'_>) -> Result<(), TrackError> {
    let state = match review.action {
        "approve" => "approved",
        "send_back" => "sent_back",
        _ => "declined",
    };
    sqlx::query!(
        r#"INSERT INTO decisions (review_case_id, action, subject_revision, reason, actor_user_id,
        via_channel, via_client) VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
        review.case as ReviewCaseId,
        review.action,
        review.revision,
        review.reason,
        review.by as UserId,
        review.via_channel,
        review.via_client
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "UPDATE review_cases SET state = 'resolved', resolved_at = now() WHERE id = $1 AND state = 'pending'",
        review.case as ReviewCaseId
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        r#"UPDATE plan_revisions SET state = $2, reviewed_by = $3, review_reason = $4,
        reviewed_at = now(), updated_at = now() WHERE id = $1 AND state = 'submitted'"#,
        review.plan as PlanRevisionId,
        state,
        review.by as UserId,
        review.reason
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// A queued unit created by an approved plan, at revision 1.
pub struct NewUnit<'a> {
    pub project_id: ProjectId,
    pub number: i32,
    pub track_id: TrackId,
    pub title: &'a str,
    pub created_by: UserId,
}

/// Create the unit of a new unit, queued at its approved revision 1.
/// # Errors
/// Returns a sanitized database error.
pub async fn create_unit(conn: &mut PgConnection, unit: NewUnit<'_>) -> Result<UnitId, TrackError> {
    Ok(sqlx::query_scalar!(
        r#"INSERT INTO units (project_id, number, track_id, state, revision, approved_revision,
        title, created_by_user, approved_at) VALUES ($1, $2, $3, 'queued', 1, 1, $4, $5, now())
        RETURNING id AS "id!: UnitId""#,
        unit.project_id as ProjectId,
        unit.number,
        unit.track_id as TrackId,
        unit.title,
        unit.created_by as UserId
    )
    .fetch_one(conn)
    .await?)
}

/// Move a queued unit to its next revision, approved at once.
/// # Errors
/// Returns a sanitized database error.
pub async fn revise_unit(
    conn: &mut PgConnection,
    unit: UnitId,
    title: &str,
) -> Result<Option<i32>, TrackError> {
    Ok(sqlx::query_scalar!(
        r#"UPDATE units SET revision = revision + 1, approved_revision = revision + 1,
        title = $2, approved_at = now(), updated_at = now()
        WHERE id = $1 AND state = 'queued' RETURNING revision"#,
        unit as UnitId,
        title
    )
    .fetch_optional(conn)
    .await?)
}

/// A unit revision written by a plan.
pub struct NewUnitRevision<'a> {
    pub unit_id: UnitId,
    pub revision: i32,
    /// The unit document as JSON text.
    pub content: &'a str,
    pub brief: &'a str,
    pub science_revision: i32,
    pub author: UserId,
    pub via_channel: &'a str,
    pub via_client: Option<&'a str>,
}

/// Store a unit revision written by a plan.
/// # Errors
/// Returns a sanitized database error.
pub async fn add_unit_revision(
    conn: &mut PgConnection,
    revision: NewUnitRevision<'_>,
) -> Result<(), TrackError> {
    sqlx::query!(
        r#"INSERT INTO unit_revisions (unit_id, revision, content, science_revision,
        author_user, via_channel, via_client, brief)
        VALUES ($1, $2, $3::text::jsonb, $4, $5, $6, $7, $8)"#,
        revision.unit_id as UnitId,
        revision.revision,
        revision.content,
        revision.science_revision,
        revision.author as UserId,
        revision.via_channel,
        revision.via_client,
        revision.brief
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Record on an approved entry the unit it names and the revision it wrote.
/// # Errors
/// Returns a sanitized database error.
pub async fn applied(
    conn: &mut PgConnection,
    plan: PlanRevisionId,
    key: &str,
    unit: UnitId,
    revision: Option<i32>,
) -> Result<(), TrackError> {
    sqlx::query!(
        "UPDATE plan_units SET unit_id = $3, unit_revision = $4 WHERE plan_revision_id = $1 AND key = $2",
        plan as PlanRevisionId,
        key,
        unit as UnitId,
        revision
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Cancel a queued unit the plan dropped.
/// # Errors
/// Returns a sanitized database error.
pub async fn cancel_queued(conn: &mut PgConnection, unit: UnitId) -> Result<bool, TrackError> {
    Ok(sqlx::query!(
        "UPDATE units SET state = 'cancelled', updated_at = now() WHERE id = $1 AND state = 'queued'",
        unit as UnitId
    )
    .execute(conn)
    .await?
    .rows_affected()
        == 1)
}

/// What cancelling an in-flight unit did.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Cancelled {
    pub attempts: Vec<AttemptId>,
    pub jobs: u64,
    pub cases: u64,
}

/// Cancel an in-flight unit and its open attempt, which is kept: its
/// lease ends, its claimed jobs and claimed document job fail as
/// `attempt_cancelled`, its pending review cases are resolved. Pending jobs
/// of a cancelled attempt, and a pending document job of a unit no
/// longer documenting, are never claimed.
/// # Errors
/// Returns a sanitized database error.
pub async fn cancel_in_flight(
    conn: &mut PgConnection,
    unit: UnitId,
) -> Result<Cancelled, TrackError> {
    let attempts = sqlx::query_scalar!(
        r#"UPDATE attempts SET state = 'cancelled', lease_token_hash = NULL, lease_expires_at = NULL,
        finished_at = coalesce(finished_at, now())
        WHERE unit_id = $1 AND state IN ('claimed', 'running', 'waiting_on_human', 'verifying')
        RETURNING id AS "id!: AttemptId""#,
        unit as UnitId
    )
    .fetch_all(&mut *conn)
    .await?;
    let jobs = sqlx::query!(
        r#"UPDATE jobs SET state = 'failed', lease_token_hash = NULL, lease_expires_at = NULL,
        finished_at = now(), error_code = 'attempt_cancelled',
        error_reason = 'the plan made this unit obsolete'
        WHERE state = 'claimed' AND (attempt_id = ANY($1) OR (phase = 'document' AND attempt_id IN (
            SELECT id FROM attempts WHERE unit_id = $2)))"#,
        &attempts as &[AttemptId],
        unit as UnitId
    )
    .execute(&mut *conn)
    .await?
    .rows_affected();
    let cases = sqlx::query!(
        r#"UPDATE review_cases SET state = 'resolved', resolved_at = now()
        WHERE unit_id = $1 AND state = 'pending'"#,
        unit as UnitId
    )
    .execute(&mut *conn)
    .await?
    .rows_affected();
    sqlx::query!(
        "UPDATE units SET state = 'cancelled', updated_at = now() WHERE id = $1",
        unit as UnitId
    )
    .execute(conn)
    .await?;
    Ok(Cancelled {
        attempts,
        jobs,
        cases,
    })
}

/// Move a planning track to active, on its first approved plan.
/// # Errors
/// Returns a sanitized database error.
pub async fn activate(conn: &mut PgConnection, track: TrackId) -> Result<bool, TrackError> {
    Ok(sqlx::query!(
        r#"UPDATE tracks SET state = 'active', revision = revision + 1, updated_at = now()
        WHERE id = $1 AND state = 'planning'"#,
        track as TrackId
    )
    .execute(conn)
    .await?
    .rows_affected()
        == 1)
}

/// The project's size limits; the defaults when none were set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub brief_max_bytes: i32,
    pub plan_approach_max_bytes: i32,
    pub unit_brief_max_bytes: i32,
    pub context_items_max: i32,
    pub index_line_max_bytes: i32,
    pub context_summary_max_bytes: i32,
    /// How long a blocking question waits for an answer before its
    /// attempt or job is released.
    pub question_wait_seconds: i32,
    /// The largest transcript of an attempt.
    pub transcript_max_bytes: i64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            brief_max_bytes: 65_536,
            plan_approach_max_bytes: 65_536,
            unit_brief_max_bytes: 32_768,
            context_items_max: 64,
            index_line_max_bytes: 100,
            context_summary_max_bytes: 300,
            question_wait_seconds: 86_400,
            transcript_max_bytes: 67_108_864,
        }
    }
}

/// The project's limits.
/// # Errors
/// Returns a sanitized database error.
pub async fn limits(conn: &mut PgConnection, project: ProjectId) -> Result<Limits, TrackError> {
    Ok(sqlx::query_as!(
        Limits,
        r#"SELECT brief_max_bytes, plan_approach_max_bytes, unit_brief_max_bytes,
        context_items_max, index_line_max_bytes, context_summary_max_bytes,
        question_wait_seconds, transcript_max_bytes
        FROM project_limits WHERE project_id = $1"#,
        project as ProjectId
    )
    .fetch_optional(conn)
    .await?
    .unwrap_or_default())
}

/// Replace the project's limits.
/// # Errors
/// Returns a sanitized database error; out-of-range values violate the checks.
pub async fn set_limits(
    conn: &mut PgConnection,
    project: ProjectId,
    limits: Limits,
    by: UserId,
) -> Result<(), TrackError> {
    sqlx::query!(
        r#"INSERT INTO project_limits (project_id, brief_max_bytes, plan_approach_max_bytes,
        unit_brief_max_bytes, context_items_max, index_line_max_bytes, context_summary_max_bytes,
        question_wait_seconds, transcript_max_bytes, updated_by)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        ON CONFLICT (project_id) DO UPDATE SET brief_max_bytes = EXCLUDED.brief_max_bytes,
        plan_approach_max_bytes = EXCLUDED.plan_approach_max_bytes,
        unit_brief_max_bytes = EXCLUDED.unit_brief_max_bytes,
        context_items_max = EXCLUDED.context_items_max,
        index_line_max_bytes = EXCLUDED.index_line_max_bytes,
        context_summary_max_bytes = EXCLUDED.context_summary_max_bytes,
        question_wait_seconds = EXCLUDED.question_wait_seconds,
        transcript_max_bytes = EXCLUDED.transcript_max_bytes,
        updated_by = EXCLUDED.updated_by, updated_at = now()"#,
        project as ProjectId,
        limits.brief_max_bytes,
        limits.plan_approach_max_bytes,
        limits.unit_brief_max_bytes,
        limits.context_items_max,
        limits.index_line_max_bytes,
        limits.context_summary_max_bytes,
        limits.question_wait_seconds,
        limits.transcript_max_bytes,
        by as UserId
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// What an attempt pinned at its claim, as the context bundle reads it.
#[derive(Clone, Debug)]
pub struct AttemptPins {
    pub attempt_id: AttemptId,
    pub sequence: i32,
    pub unit_id: UnitId,
    pub number: i32,
    pub title: String,
    pub track_id: TrackId,
    pub track_slug: String,
    pub track_title: String,
    pub unit_revision: i32,
    pub brief_revision: Option<i32>,
    pub plan_revision: Option<i32>,
}

/// An attempt's pins, by unit number and attempt sequence.
/// # Errors
/// Returns a sanitized database error.
pub async fn attempt_pins(
    conn: &mut PgConnection,
    project: ProjectId,
    number: i32,
    sequence: i32,
) -> Result<Option<AttemptPins>, TrackError> {
    Ok(sqlx::query_as!(
        AttemptPins,
        r#"SELECT a.id AS "attempt_id!: _", a.sequence, h.id AS "unit_id!: _", h.number,
        h.title, t.id AS "track_id!: _", t.slug AS track_slug, t.title AS track_title,
        a.unit_revision, a.brief_revision, a.plan_revision
        FROM attempts a JOIN units h ON h.id = a.unit_id
        JOIN tracks t ON t.id = a.track_id
        WHERE h.project_id = $1 AND h.number = $2 AND a.sequence = $3"#,
        project as ProjectId,
        number,
        sequence
    )
    .fetch_optional(conn)
    .await?)
}

/// An attempt's pins, by attempt id.
/// # Errors
/// Returns a sanitized database error.
pub async fn attempt_pins_by_id(
    conn: &mut PgConnection,
    attempt: AttemptId,
) -> Result<Option<AttemptPins>, TrackError> {
    Ok(sqlx::query_as!(
        AttemptPins,
        r#"SELECT a.id AS "attempt_id!: _", a.sequence, h.id AS "unit_id!: _", h.number,
        h.title, t.id AS "track_id!: _", t.slug AS track_slug, t.title AS track_title,
        a.unit_revision, a.brief_revision, a.plan_revision
        FROM attempts a JOIN units h ON h.id = a.unit_id
        JOIN tracks t ON t.id = a.track_id
        WHERE a.id = $1"#,
        attempt as AttemptId
    )
    .fetch_optional(conn)
    .await?)
}

/// The entry naming a unit in one revision.
/// # Errors
/// Returns a sanitized database error.
pub async fn entry_for(
    conn: &mut PgConnection,
    plan: PlanRevisionId,
    unit: UnitId,
) -> Result<Option<PlanUnit>, TrackError> {
    Ok(sqlx::query_as!(
        PlanUnit,
        r#"SELECT u.key, u.position, u.unit_id AS "unit_id?: _", h.number AS "number?",
        h.state AS "state?", u.redo_of AS "redo_of?: _", r.number AS "redo_of_number?",
        u.fields::text AS "fields!", u.brief, u.science_revision, u.unit_revision
        FROM plan_units u LEFT JOIN units h ON h.id = u.unit_id
        LEFT JOIN units r ON r.id = u.redo_of
        WHERE u.plan_revision_id = $1 AND u.unit_id = $2"#,
        plan as PlanRevisionId,
        unit as UnitId
    )
    .fetch_optional(conn)
    .await?)
}

/// The entry naming a unit in the newest approved revision listing it.
/// # Errors
/// Returns a sanitized database error.
pub async fn latest_entry(
    conn: &mut PgConnection,
    unit: UnitId,
) -> Result<Option<(i32, PlanUnit)>, TrackError> {
    let plan = sqlx::query!(
        r#"SELECT p.id AS "id!: PlanRevisionId", p.revision FROM plan_units u
        JOIN plan_revisions p ON p.id = u.plan_revision_id
        WHERE u.unit_id = $1 AND p.state = 'approved' ORDER BY p.revision DESC LIMIT 1"#,
        unit as UnitId
    )
    .fetch_optional(&mut *conn)
    .await?;
    let Some(plan) = plan else {
        return Ok(None);
    };
    Ok(entry_for(conn, plan.id, unit)
        .await?
        .map(|entry| (plan.revision, entry)))
}

/// The text an attempt's output says about itself: its write-up's body, or
/// the findings of its agent report.
/// # Errors
/// Returns a sanitized database error.
pub async fn attempt_report(
    conn: &mut PgConnection,
    attempt: AttemptId,
) -> Result<Option<String>, TrackError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT coalesce(nullif(o.body, ''), o.front_matter #>> '{report,findings}',
                          o.front_matter #>> '{report,what_was_tried}') AS "text?"
        FROM phase_outputs o
        WHERE o.attempt_id = $1 AND o.stage IN ('writeup', 'agent')
        ORDER BY (o.stage = 'writeup') DESC, o.created_at DESC LIMIT 1"#,
        attempt as AttemptId
    )
    .fetch_optional(conn)
    .await?
    .flatten())
}

/// The newest attempt of a unit that left an output: its sequence,
/// state and that output's text.
/// # Errors
/// Returns a sanitized database error.
pub async fn latest_output(
    conn: &mut PgConnection,
    unit: UnitId,
) -> Result<Option<(i32, String, Option<String>)>, TrackError> {
    let row = sqlx::query!(
        r#"SELECT a.id AS "id!: AttemptId", a.sequence, a.state FROM attempts a
        WHERE a.unit_id = $1
          AND EXISTS (SELECT 1 FROM phase_outputs o WHERE o.attempt_id = a.id
                      AND o.stage IN ('writeup', 'agent'))
        ORDER BY a.sequence DESC LIMIT 1"#,
        unit as UnitId
    )
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let text = attempt_report(conn, row.id).await?;
    Ok(Some((row.sequence, row.state, text)))
}

/// An attempt of the project, by unit number and sequence.
/// # Errors
/// Returns a sanitized database error.
pub async fn attempt_id(
    conn: &mut PgConnection,
    project: ProjectId,
    number: i32,
    sequence: i32,
) -> Result<Option<AttemptId>, TrackError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT a.id AS "id!: AttemptId" FROM attempts a
        JOIN units h ON h.id = a.unit_id
        WHERE h.project_id = $1 AND h.number = $2 AND a.sequence = $3"#,
        project as ProjectId,
        number,
        sequence
    )
    .fetch_optional(conn)
    .await?)
}

/// An artifact of the project, described for a context line.
#[derive(Clone, Debug)]
pub struct ArtifactLine {
    pub role: String,
    pub media_type: String,
    pub size_bytes: i64,
    pub number: i32,
    pub sequence: i32,
}

/// One artifact of the project.
/// # Errors
/// Returns a sanitized database error.
pub async fn artifact(
    conn: &mut PgConnection,
    project: ProjectId,
    artifact: Uuid,
) -> Result<Option<ArtifactLine>, TrackError> {
    Ok(sqlx::query_as!(
        ArtifactLine,
        r#"SELECT f.role, f.media_type, f.size_bytes, h.number, a.sequence
        FROM artifacts f JOIN attempts a ON a.id = f.attempt_id
        JOIN units h ON h.id = a.unit_id
        WHERE f.project_id = $1 AND f.id = $2"#,
        project as ProjectId,
        artifact as _
    )
    .fetch_optional(conn)
    .await?)
}

/// The brief of the unit revision a plan wrote, if any.
/// # Errors
/// Returns a sanitized database error.
pub async fn current_unit_brief(
    conn: &mut PgConnection,
    unit: UnitId,
) -> Result<Option<String>, TrackError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT r.brief FROM unit_revisions r JOIN units h ON h.id = r.unit_id
        WHERE r.unit_id = $1 AND r.revision = coalesce(h.approved_revision, h.revision)"#,
        unit as UnitId
    )
    .fetch_optional(conn)
    .await?
    .flatten())
}
