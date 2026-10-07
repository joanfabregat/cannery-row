//! Shared artifact authorization performs no object-store I/O.
use cannery_attempts::{
    model::{Artifact, ArtifactId, JsonContext},
    repo::{AttemptError, Repository},
};
use cannery_core::{
    errors::{DomainError, ErrorCode},
    principal::{Principal, Role},
};
use cannery_projects::{ProjectError, repo::Project};
use sqlx::PgConnection;
#[derive(Debug, thiserror::Error)]
pub enum PermissionError {
    #[error(transparent)]
    Domain(#[from] DomainError),
    #[error(transparent)]
    Project(#[from] ProjectError),
    #[error(transparent)]
    Attempt(#[from] AttemptError),
}
/// Caller-supplied store identity and repository history, with no implicit defaults.
pub struct PermissionContext<'a> {
    pub backend: &'a str,
    pub bucket: &'a str,
    pub repository: JsonContext,
}
/// Members, researchers and installation administrators may read every role.
/// # Errors
/// Returns the existing project repository's sanitized lookup failures.
pub async fn may_download_any_artifact(
    connection: &mut PgConnection,
    principal: &Principal,
    project: &Project,
) -> Result<bool, ProjectError> {
    let Principal::User(user) = principal else {
        return Ok(false);
    };
    if user.is_admin {
        return Ok(true);
    }
    let role = cannery_projects::repo::get_role(connection, project.id, user.user_id).await?;
    Ok(cannery_projects::authz::role_at_least(role, Role::Member))
}
/// Project masking, artifact lookup, role, external reference and store identity in source order.
/// # Errors
/// Returns public source errors or sanitized repository failures; never performs HEAD or transfer.
pub async fn permitted_artifact(
    connection: &mut PgConnection,
    principal: &Principal,
    slug: &str,
    id: ArtifactId,
    context: PermissionContext<'_>,
) -> Result<Artifact, PermissionError> {
    let project = cannery_projects::authz::project_read(connection, principal, slug).await?;
    let artifact = Repository::new(connection, context.repository)
        .get_artifact(project.id, id)
        .await?
        .ok_or_else(|| DomainError::new(ErrorCode::NotFound, "artifact not found"))?;
    if artifact.role != "report_asset"
        && !may_download_any_artifact(connection, principal, &project).await?
    {
        return Err(DomainError::new(
            ErrorCode::Forbidden,
            "downloading this artifact requires the member role",
        )
        .into());
    }
    if artifact.backend == "external" {
        let message = format!(
            "artifact {} is an imported reference to {}, outside the store; fetch it there (its size and SHA-256 are recorded)",
            artifact.id.0,
            artifact.uri.as_deref().unwrap_or("None"),
        );
        return Err(DomainError::new(ErrorCode::Conflict, message).into());
    }
    if artifact.backend != context.backend || artifact.bucket != context.bucket {
        return Err(DomainError::new(ErrorCode::NotFound, "the object is in another store").into());
    }
    Ok(artifact)
}
