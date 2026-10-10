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
/// Whether the caller reads the artifact as an input of the work it holds,
/// for as long as it holds it: a claimed job whose lease and deadline still
/// run (a verify job: an object of its input manifest; a document or decide
/// job: an artifact of the unit's attempts), or an attempt in progress whose
/// plan entry names the artifact as a context item.
/// # Errors
/// Returns a sanitized repository failure.
pub async fn holds_as_input(
    connection: &mut PgConnection,
    principal: &Principal,
    artifact: &Artifact,
) -> Result<bool, AttemptError> {
    let (user, service) = match principal {
        Principal::User(user) => (Some(user.user_id.0.to_string()), None),
        Principal::Service(service) => (None, Some(service.service_account_id.0.to_string())),
    };
    sqlx::query_scalar!(
        r#"SELECT EXISTS (
            SELECT 1 FROM jobs j JOIN attempts ja ON ja.id = j.attempt_id, artifacts ar
            JOIN attempts aa ON aa.id = ar.attempt_id
            WHERE ar.id = $1::text::uuid AND j.project_id = ar.project_id AND j.state = 'claimed'
              AND j.lease_expires_at > now() AND (j.deadline IS NULL OR j.deadline > now())
              AND j.claimed_by_user IS NOT DISTINCT FROM $2::text::uuid
              AND j.claimed_by_service IS NOT DISTINCT FROM $3::text::uuid
              AND ((j.phase = 'verify' AND ar.attempt_id = j.attempt_id AND EXISTS (
                      SELECT 1 FROM manifests m
                      WHERE m.attempt_id = j.attempt_id
                        AND m.id::text = j.spec #>> '{inputs,manifest,ref}'
                        AND m.content -> 'objects' @> jsonb_build_array(jsonb_build_object(
                            'storage', jsonb_build_object('key', ar.key)))))
                   OR (j.phase IN ('document', 'decide') AND aa.unit_id = ja.unit_id))
        ) OR EXISTS (
            SELECT 1 FROM attempts a
            JOIN plan_revisions r ON r.track_id = a.track_id AND r.revision = a.plan_revision
            JOIN plan_units u ON u.plan_revision_id = r.id AND u.unit_id = a.unit_id,
            artifacts ar
            WHERE ar.id = $1::text::uuid AND a.project_id = ar.project_id
              AND (a.state = 'waiting_on_human'
                   OR (a.state IN ('claimed', 'running') AND a.lease_expires_at > now()))
              AND a.claimed_by_user IS NOT DISTINCT FROM $2::text::uuid
              AND a.claimed_by_service IS NOT DISTINCT FROM $3::text::uuid
              AND u.fields -> 'context' @> jsonb_build_array(jsonb_build_object(
                  'kind', 'artifact', 'artifact', ar.id::text))
        ) AS "held!""#,
        artifact.id.0.to_string(),
        user,
        service
    )
    .fetch_one(connection)
    .await
    .map_err(|_| AttemptError::Database { sqlstate: None })
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
        && !holds_as_input(connection, principal, &artifact).await?
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
