//! Project-scoped authorization, independent of authentication and transport.

use crate::{
    ProjectError,
    repo::{self, Project},
};
use cannery_core::{
    errors::{DomainError, ErrorCode},
    principal::{Principal, Role, Scope, ServiceKind},
};
use sqlx::PgConnection;

pub const ALL_SERVICES: [ServiceKind; 4] = [
    ServiceKind::Agent,
    ServiceKind::Experimenter,
    ServiceKind::Tester,
    ServiceKind::Evaluator,
];

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectAccess {
    pub project: Project,
    pub role: Option<Role>,
    pub service_kind: Option<ServiceKind>,
}

#[must_use]
pub fn role_at_least(role: Option<Role>, minimum: Role) -> bool {
    role.is_some_and(|role| role >= minimum)
}

const fn service_name(kind: ServiceKind) -> &'static str {
    match kind {
        ServiceKind::Agent => "agent",
        ServiceKind::Experimenter => "experimenter",
        ServiceKind::Tester => "tester",
        ServiceKind::Evaluator => "evaluator",
    }
}

const fn role_name(role: Role) -> &'static str {
    match role {
        Role::Viewer => "viewer",
        Role::Member => "member",
        Role::Researcher => "researcher",
    }
}

fn not_found() -> ProjectError {
    DomainError::new(ErrorCode::NotFound, "project not found").into()
}

/// Authorize membership or an allowed service kind before checking write scope.
/// Installation administrators receive no bypass here.
/// # Errors
/// Returns `not_found` for missing or inaccessible projects, `forbidden` for
/// insufficient role, service kind or scope, or a sanitized persistence error.
pub async fn project_access(
    conn: &mut PgConnection,
    principal: &Principal,
    slug: &str,
    min_role: Option<Role>,
    services: &[ServiceKind],
    write: bool,
) -> Result<ProjectAccess, ProjectError> {
    let project = repo::get_project_by_slug(conn, slug)
        .await?
        .ok_or_else(not_found)?;
    let role = if let Principal::User(user) = principal {
        repo::get_role(conn, project.id, user.user_id).await?
    } else {
        None
    };
    authorize(
        project,
        principal,
        role.map(role_name),
        min_role,
        services,
        write,
    )
}

fn authorize(
    project: Project,
    principal: &Principal,
    role: Option<&str>,
    min_role: Option<Role>,
    services: &[ServiceKind],
    write: bool,
) -> Result<ProjectAccess, ProjectError> {
    match principal {
        Principal::Service(service) => {
            if service.project_id != project.id {
                return Err(not_found());
            }
            if !services.contains(&service.kind) {
                return Err(DomainError::new(
                    ErrorCode::Forbidden,
                    format!(
                        "a {} service account cannot do this",
                        service_name(service.kind)
                    ),
                )
                .into());
            }
            if write {
                principal.require_scope(Scope::Write)?;
            }
            Ok(ProjectAccess {
                project,
                role: None,
                service_kind: Some(service.kind),
            })
        }
        Principal::User(_) => {
            let role = role.ok_or_else(not_found)?;
            let sufficient = if let Some(minimum) = min_role {
                let rank = match role {
                    "viewer" => Role::Viewer,
                    "member" => Role::Member,
                    "researcher" => Role::Researcher,
                    _ => return Err(ProjectError::CorruptData),
                };
                role_at_least(Some(rank), minimum)
            } else {
                false
            };
            if !sufficient {
                let needed = min_role.map_or_else(
                    || "a service account".to_owned(),
                    |minimum| format!("the {} role", role_name(minimum)),
                );
                return Err(DomainError::new(
                    ErrorCode::Forbidden,
                    format!("this requires {needed}"),
                )
                .into());
            }
            if write {
                principal.require_scope(Scope::Write)?;
            }
            Ok(ProjectAccess {
                project,
                role: Some(match role {
                    "researcher" => Role::Researcher,
                    "member" => Role::Member,
                    _ => Role::Viewer,
                }),
                service_kind: None,
            })
        }
    }
}

/// Read access permits any member, any project service, or an administrator.
/// # Errors
/// Returns authorization or sanitized persistence errors, as in `project_access`.
pub async fn project_read(
    conn: &mut PgConnection,
    principal: &Principal,
    slug: &str,
) -> Result<Project, ProjectError> {
    if matches!(principal, Principal::User(user) if user.is_admin) {
        return repo::get_project_by_slug(conn, slug)
            .await?
            .ok_or_else(not_found);
    }
    Ok(project_access(
        conn,
        principal,
        slug,
        Some(Role::Viewer),
        &ALL_SERVICES,
        false,
    )
    .await?
    .project)
}
