//! Verified manifests belong to a live, claimant-owned attempt lease.
use crate::{
    AppState,
    attempt_lease_routes::{self, Failure, domain, failure, internal},
    authentication::authenticate,
    body::{self, DecodedBody},
    requests::RequestContext,
};
use axum::{
    Json, Router,
    extract::{FromRequestParts, Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use cannery_attempts::{
    model::{Artifact, JsonContext},
    repo::Repository,
};
use cannery_core::{
    audit::{self, Attribution, Record},
    contracts::{ContractKind, ContractValidator},
    errors::{DomainError, ErrorCode},
    json::{self, Document, Node, NodeId},
};
use serde_json::json;
use sqlx::Acquire;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub struct ManifestContext {
    pub repository: JsonContext,
    pub contracts: ContractValidator,
    pub nesting_budget: usize,
}
#[derive(Clone)]
pub(crate) struct RouteState {
    app: AppState,
    profile: Arc<ManifestContext>,
}
pub fn routes(app: AppState, profile: Arc<ManifestContext>) -> Router {
    Router::new()
        .route(
            "/api/projects/{slug}/units/{number}/attempts/{sequence}/manifest",
            post(create),
        )
        .with_state(RouteState { app, profile })
}
pub(crate) fn violation(path: &str, message: &str) -> Failure {
    failure(crate::errors::ApiError::from(
        DomainError::new(ErrorCode::ValidationFailed, message)
            .with_details(json!([{"path":path,"message":message}])),
    ))
}
fn field<'a>(document: &'a Document, root: NodeId, key: &str) -> Option<&'a Node> {
    document.field(root, key).and_then(|id| document.node(id))
}
fn text(document: &Document, root: NodeId, key: &str) -> Option<String> {
    match field(document, root, key) {
        Some(Node::String(value)) => value.as_utf8(),
        _ => None,
    }
}
/// Match verified identity and content, rejecting duplicates and cross-owner objects.
pub(crate) fn check_objects(document: &Document, artifacts: &[Artifact]) -> Result<(), Failure> {
    let Some(Node::Array(objects)) = field(document, document.root(), "objects") else {
        return Err(violation("/objects", "objects must be an array"));
    };
    let mut seen = BTreeSet::new();
    for (index, object) in objects.iter().enumerate() {
        let path = format!("/objects/{index}");
        let storage = document
            .field(*object, "storage")
            .ok_or_else(|| violation(&path, "storage is required"))?;
        let identity = (
            text(document, storage, "backend"),
            text(document, storage, "bucket"),
            text(document, storage, "key"),
        );
        if !seen.insert(identity.clone()) {
            return Err(violation(&path, "object listed twice"));
        }
        let artifact = artifacts
            .iter()
            .find(|artifact| {
                identity
                    == (
                        Some(artifact.backend.clone()),
                        Some(artifact.bucket.clone()),
                        Some(artifact.key.clone()),
                    )
            })
            .ok_or_else(|| violation(&path, "not a verified upload of this owner"))?;
        for (name, expected) in [
            ("role", &artifact.role),
            ("sha256", &artifact.sha256),
            ("media_type", &artifact.media_type),
        ] {
            if text(document, *object, name).as_ref() != Some(expected) {
                return Err(violation(
                    &format!("{path}/{name}"),
                    "value differs from the verified object",
                ));
            }
        }
        if !matches!(field(document, *object, "size_bytes"), Some(Node::Integer(value)) if value == &artifact.size_bytes.into())
        {
            return Err(violation(
                &format!("{path}/size_bytes"),
                "size differs from the verified object",
            ));
        }
    }
    Ok(())
}

/// Sorted compact UTF-8 hashing retains the arena's integer and floating-point representation.
pub(crate) fn canonical_sha256(document: &Document, budget: usize) -> Result<String, &'static str> {
    json::canonical::sha256(document, budget)
}

pub(crate) fn canonical_bytes(document: &Document, budget: usize) -> Result<Vec<u8>, &'static str> {
    json::canonical::bytes(document, budget)
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/units/{number}/attempts/{sequence}/manifest",
    operation_id = "post_manifest_api_projects__slug__units__number__attempts__sequence__manifest_post",
    summary = "Post Manifest",
    description = "Verify an artifact manifest against this attempt's verified uploads.",
    params(("slug" = String, Path),
        ("number" = i64, Path),
        ("sequence" = i64, Path),
        ("X-Lease-Token" = Option<String>, Header),
        ("X-Lease-Generation" = Option<i64>, Header)),
    request_body(content = crate::api_models::ArtifactManifestRequest, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::ManifestRef, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
#[allow(
    clippy::too_many_lines,
    reason = "Keep authorization, lease fencing and the manifest commit together"
)]
pub(crate) async fn create(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let mut auth = authenticate(&state.app, &context, request.headers(), request.method())
        .await
        .map_err(failure)?;
    let (mut parts, body) = body::read_body(request).await.map_err(failure)?;
    let paths = Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &state)
        .await
        .map_err(failure)?
        .0;
    let parameters = attempt_lease_routes::parameters(&paths, &parts.headers, &context)?;
    let slug = paths
        .get("slug")
        .ok_or_else(|| internal(&context, "manifest project path"))?;
    let project =
        attempt_lease_routes::worker_access(&mut auth.connection, &auth.principal, slug, &context)
            .await?;
    let DecodedBody::Json(document) = body else {
        return Err(domain(
            ErrorCode::ValidationFailed,
            "manifest must be a JSON object",
        ));
    };
    let violations = state
        .profile
        .contracts
        .violations(ContractKind::ArtifactManifest, &document)
        .map_err(|_| internal(&context, "manifest contract"))?;
    if !violations.is_empty() {
        let details: Vec<_> = violations
            .iter()
            .map(|value| json!({"path":value.path.as_utf8(),"message":value.message}))
            .collect();
        return Err(failure(crate::errors::ApiError::from(
            DomainError::new(ErrorCode::ValidationFailed, "invalid artifact manifest")
                .with_details(json!(details)),
        )));
    }
    let document =
        crate::api_models::request_document::<crate::api_models::ArtifactManifestRequest>(
            &document,
        )
        .map_err(failure)?;
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "manifest transaction"))?;
    let attempt = attempt_lease_routes::leased(
        &mut tx,
        &auth.principal,
        &project,
        &parameters,
        state.profile.repository,
        &context,
    )
    .await?;
    if text(&document, document.root(), "attempt_id") != Some(attempt.id.0.to_string()) {
        return Err(violation(
            "/attempt_id",
            "manifest belongs to a different attempt",
        ));
    }
    let artifacts = Repository::new(&mut tx, state.profile.repository)
        .list_artifacts(attempt.id)
        .await
        .map_err(|_| internal(&context, "manifest artifacts"))?;
    check_objects(&document, &artifacts)?;
    let sha256 = canonical_sha256(&document, state.profile.nesting_budget)
        .map_err(|_| internal(&context, "manifest hash"))?;
    let stored = Repository::new(&mut tx, state.profile.repository)
        .add_manifest(attempt.id, "agent", &document, &sha256)
        .await
        .map_err(|_| internal(&context, "manifest storage"))?;
    let new_state = json!({"attempt_id":attempt.id.0.to_string(),"sha256":sha256});
    audit::record(
        &mut tx,
        Attribution::Principal(&auth.principal),
        Record {
            action: "manifest.verified",
            subject_type: "manifest",
            subject_id: &stored.id.0.to_string(),
            project_id: Some(project.id),
            prior_state: None,
            new_state: Some(&new_state),
            reason: None,
            idempotency_key: None,
        },
    )
    .await
    .map_err(|_| internal(&context, "manifest audit"))?;
    tx.commit()
        .await
        .map_err(|_| internal(&context, "manifest commit"))?;
    Ok((
        StatusCode::CREATED,
        Json(crate::api_models::ManifestRef {
            r#ref: stored.id.0.to_string(),
            sha256,
        }),
    )
        .into_response())
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_hash_sorts_keys_and_preserves_numeric_and_unicode_values()
    -> Result<(), json::DecodeError> {
        let first = json::decode("{\"z\":-0.0,\"a\":[1.0,\"é\"]}".as_bytes(), 16)?;
        let second = json::decode("{\"a\":[1.0,\"é\"],\"z\":-0.0}".as_bytes(), 16)?;
        assert_eq!(
            canonical_sha256(&first, 16),
            Ok("81c7d2daa0bb7d7844e9bbbc85f95759e81f117a22c5dac9bac1d96888e0e606".to_owned())
        );
        assert_eq!(canonical_sha256(&first, 16), canonical_sha256(&second, 16));
        assert!(canonical_sha256(&first, 1).is_err());
        Ok(())
    }
}
