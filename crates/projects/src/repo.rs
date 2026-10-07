//! Project SQL. Callers own transactions, membership locks and audit writes.

use crate::ProjectError;
use cannery_core::{
    ids::{ProjectId, UserId},
    principal::Role,
    timestamps::Timestamp,
};
use serde::Serialize;
use sqlx::{
    PgConnection, Postgres,
    postgres::{PgArguments, PgRow},
    query::{Map, QueryScalar},
};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Project {
    pub id: ProjectId,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub created_by: UserId,
    pub created_at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProjectWithRole {
    pub id: ProjectId,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub created_by: UserId,
    pub created_at: Timestamp,
    pub role: Option<Role>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Member {
    pub user_id: UserId,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub role: Role,
    pub granted_by: UserId,
    pub granted_at: Timestamp,
}

struct RawProjectWithRole {
    id: ProjectId,
    slug: String,
    title: String,
    description: String,
    created_by: UserId,
    created_at: Timestamp,
    role: Option<String>,
}

struct RawMember {
    user_id: UserId,
    email: Option<String>,
    display_name: Option<String>,
    role: String,
    granted_by: UserId,
    granted_at: Timestamp,
}

fn decode_role(value: &str) -> Result<Role, ProjectError> {
    match value {
        "viewer" => Ok(Role::Viewer),
        "member" => Ok(Role::Member),
        "researcher" => Ok(Role::Researcher),
        _ => Err(ProjectError::CorruptData),
    }
}

const fn role_text(role: Role) -> &'static str {
    match role {
        Role::Viewer => "viewer",
        Role::Member => "member",
        Role::Researcher => "researcher",
    }
}

impl TryFrom<RawProjectWithRole> for ProjectWithRole {
    type Error = ProjectError;
    fn try_from(raw: RawProjectWithRole) -> Result<Self, Self::Error> {
        Ok(Self {
            id: raw.id,
            slug: raw.slug,
            title: raw.title,
            description: raw.description,
            created_by: raw.created_by,
            created_at: raw.created_at,
            role: raw.role.as_deref().map(decode_role).transpose()?,
        })
    }
}

impl TryFrom<RawMember> for Member {
    type Error = ProjectError;
    fn try_from(raw: RawMember) -> Result<Self, Self::Error> {
        Ok(Self {
            user_id: raw.user_id,
            email: raw.email,
            display_name: raw.display_name,
            role: decode_role(&raw.role)?,
            granted_by: raw.granted_by,
            granted_at: raw.granted_at,
        })
    }
}

/// Create a project, returning `None` when the slug is taken.
/// # Errors
/// Returns a sanitized database error on persistence failure.
pub async fn create_project(
    conn: &mut PgConnection,
    slug: &str,
    title: &str,
    description: &str,
    created_by: UserId,
) -> Result<Option<Project>, ProjectError> {
    Ok(sqlx::query_as!(
        Project,
        r#"INSERT INTO projects AS p (slug, title, description, created_by)
        VALUES ($1, $2, $3, $4) ON CONFLICT (slug) DO NOTHING
        RETURNING p.id AS "id!: _", p.slug, p.title, p.description,
        p.created_by AS "created_by!: _", p.created_at AS "created_at!: _""#,
        slug,
        title,
        description,
        created_by as _
    )
    .fetch_optional(conn)
    .await?)
}

/// Find a project by its slug.
/// # Errors
/// Returns a sanitized database error on persistence failure.
pub async fn get_project_by_slug(
    conn: &mut PgConnection,
    slug: &str,
) -> Result<Option<Project>, ProjectError> {
    Ok(project_slug_query(slug).fetch_optional(conn).await?)
}

pub(crate) fn project_slug_query(
    slug: &str,
) -> Map<'_, Postgres, impl FnMut(PgRow) -> Result<Project, sqlx::Error> + Send, PgArguments> {
    sqlx::query_as!(
        Project,
        r#"SELECT p.id AS "id!: _", p.slug, p.title, p.description,
        p.created_by AS "created_by!: _", p.created_at AS "created_at!: _"
        FROM projects p WHERE p.slug = $1"#,
        slug
    )
}

/// Find a project by its identity.
/// # Errors
/// Returns a sanitized database error on persistence failure.
pub async fn get_project(
    conn: &mut PgConnection,
    project_id: ProjectId,
) -> Result<Option<Project>, ProjectError> {
    Ok(project_id_query(Some(project_id))
        .fetch_optional(conn)
        .await?)
}

pub(crate) fn project_id_query(
    project_id: Option<ProjectId>,
) -> Map<'static, Postgres, impl FnMut(PgRow) -> Result<Project, sqlx::Error> + Send, PgArguments> {
    sqlx::query_as!(
        Project,
        r#"SELECT p.id AS "id!: _", p.slug, p.title, p.description,
        p.created_by AS "created_by!: _", p.created_at AS "created_at!: _"
        FROM projects p WHERE p.id = $1"#,
        project_id as _
    )
}

/// List visible projects by slug; administrators can request all projects.
/// A missing limit is unbounded. Cursor and limit validation belongs to callers.
/// # Errors
/// Returns a sanitized database or persisted-role error.
pub async fn list_projects_for_user(
    conn: &mut PgConnection,
    user_id: UserId,
    all_projects: bool,
    after: Option<&str>,
    limit: Option<i64>,
) -> Result<Vec<ProjectWithRole>, ProjectError> {
    sqlx::query_as!(
        RawProjectWithRole,
        r#"SELECT p.id AS "id!: _", p.slug, p.title, p.description,
        p.created_by AS "created_by!: _", p.created_at AS "created_at!: _", m.role AS "role?"
        FROM projects p LEFT JOIN memberships m ON m.project_id = p.id AND m.user_id = $1
        WHERE ($2 OR m.user_id IS NOT NULL) AND ($3::text IS NULL OR p.slug > $3)
        ORDER BY p.slug LIMIT $4"#,
        user_id as _,
        all_projects,
        after,
        limit
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(TryInto::try_into)
    .collect()
}

/// Read the user's role within one project.
/// # Errors
/// Returns a sanitized database or persisted-role error.
pub async fn get_role(
    conn: &mut PgConnection,
    project_id: ProjectId,
    user_id: UserId,
) -> Result<Option<Role>, ProjectError> {
    let role = role_query(Some(project_id), Some(user_id))
        .fetch_optional(conn)
        .await?;
    role.as_deref().map(decode_role).transpose()
}

pub(crate) fn role_query(
    project_id: Option<ProjectId>,
    user_id: Option<UserId>,
) -> QueryScalar<'static, Postgres, String, PgArguments> {
    sqlx::query_scalar!(
        "SELECT role FROM memberships WHERE project_id = $1 AND user_id = $2",
        project_id as _,
        user_id as _
    )
}

/// Serialize membership changes until the caller commits or rolls back.
/// # Errors
/// Returns a sanitized database error on persistence failure.
pub async fn lock_project(
    conn: &mut PgConnection,
    project_id: ProjectId,
) -> Result<bool, ProjectError> {
    Ok(sqlx::query!(
        "SELECT 1 AS present FROM projects WHERE id = $1 FOR UPDATE",
        project_id as _
    )
    .fetch_optional(conn)
    .await?
    .is_some())
}

/// List members by lowercase display name (falling back to email), then user ID.
/// A cursor outside this project's membership matches nothing, preserving privacy.
/// # Errors
/// Returns a sanitized database or persisted-role error.
pub async fn list_members(
    conn: &mut PgConnection,
    project_id: ProjectId,
    after: Option<UserId>,
    limit: Option<i64>,
) -> Result<Vec<Member>, ProjectError> {
    sqlx::query_as!(
        RawMember,
        r#"SELECT m.user_id AS "user_id!: _", u.email, u.display_name, m.role,
        m.granted_by AS "granted_by!: _", m.granted_at AS "granted_at!: _"
        FROM memberships m JOIN users u ON u.id = m.user_id WHERE m.project_id = $1
        AND ($2::uuid IS NULL OR (lower(coalesce(u.display_name, u.email, '')), m.user_id) > (
        SELECT lower(coalesce(u.display_name, u.email, '')), u.id
        FROM memberships c JOIN users u ON u.id = c.user_id
        WHERE c.project_id = $1 AND c.user_id = $2))
        ORDER BY lower(coalesce(u.display_name, u.email, '')), m.user_id LIMIT $3"#,
        project_id as _,
        after as _,
        limit
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(TryInto::try_into)
    .collect()
}

/// Find a member within one project.
/// # Errors
/// Returns a sanitized database or persisted-role error.
pub async fn get_member(
    conn: &mut PgConnection,
    project_id: ProjectId,
    user_id: UserId,
) -> Result<Option<Member>, ProjectError> {
    sqlx::query_as!(
        RawMember,
        r#"SELECT m.user_id AS "user_id!: _", u.email, u.display_name, m.role,
        m.granted_by AS "granted_by!: _", m.granted_at AS "granted_at!: _"
        FROM memberships m JOIN users u ON u.id = m.user_id
        WHERE m.project_id = $1 AND m.user_id = $2"#,
        project_id as _,
        user_id as _
    )
    .fetch_optional(conn)
    .await?
    .map(TryInto::try_into)
    .transpose()
}

/// Grant or change membership and return the previous role.
/// Call inside a transaction after `lock_project` to make the previous role exact.
/// Repeated grants also update grant metadata; HTTP idempotency is caller-owned.
/// # Errors
/// Returns a sanitized database or persisted-role error.
pub async fn set_membership(
    conn: &mut PgConnection,
    project_id: ProjectId,
    user_id: UserId,
    role: Role,
    granted_by: UserId,
) -> Result<Option<Role>, ProjectError> {
    let previous = get_role(conn, project_id, user_id).await?;
    sqlx::query!(
        "INSERT INTO memberships (project_id, user_id, role, granted_by)
        VALUES ($1, $2, $3, $4) ON CONFLICT (project_id, user_id) DO UPDATE SET
        role = EXCLUDED.role, granted_by = EXCLUDED.granted_by, granted_at = now()",
        project_id as _,
        user_id as _,
        role_text(role),
        granted_by as _
    )
    .execute(conn)
    .await?;
    Ok(previous)
}

/// Delete a membership and return its previous role, if any.
/// # Errors
/// Returns a sanitized database or persisted-role error.
pub async fn delete_membership(
    conn: &mut PgConnection,
    project_id: ProjectId,
    user_id: UserId,
) -> Result<Option<Role>, ProjectError> {
    let role = sqlx::query_scalar!(
        "DELETE FROM memberships WHERE project_id = $1 AND user_id = $2 RETURNING role",
        project_id as _,
        user_id as _
    )
    .fetch_optional(conn)
    .await?;
    role.as_deref().map(decode_role).transpose()
}

/// Block configuration revisions until the caller commits or rolls back.
/// Missing projects are deliberately a no-op, matching the source repository.
/// # Errors
/// Returns a sanitized database error on persistence failure.
pub async fn share_lock_project(
    conn: &mut PgConnection,
    project_id: ProjectId,
) -> Result<(), ProjectError> {
    sqlx::query!(
        "SELECT 1 AS present FROM projects WHERE id = $1 FOR SHARE",
        project_id as _
    )
    .fetch_optional(conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::decode_role;
    #[test]
    fn corrupt_roles_are_sanitized() -> Result<(), Box<dyn std::error::Error>> {
        let error = decode_role("synthetic-private-role")
            .err()
            .ok_or("invalid role accepted")?;
        assert_eq!(error.to_string(), "invalid persisted project data");
        assert_eq!(format!("{error:?}"), "CorruptData");
        Ok(())
    }
}
