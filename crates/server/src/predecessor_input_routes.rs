//! Leased predecessor artifacts retain source transaction and delivery order.
use crate::{
    AppState,
    artifact_download::DownloadStore,
    attempt_lease_routes::{self, Failure, domain, failure, internal},
    authentication::authenticate,
    requests::RequestContext,
    validation,
};
use axum::{
    Router,
    body::Body,
    extract::{FromRequestParts, Path, Request, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use cannery_attempts::{
    model::{ArtifactId, StoredJson},
    repo::Repository,
};
use cannery_core::{
    errors::{DomainError, ErrorCode},
    principal::{Principal, ServiceKind},
};
use cannery_research::{job_baselines, science::RenderingContext};
use cannery_storage::{Error as StoreError, ObjectReader};
use sqlx::Acquire;
use std::{collections::BTreeMap, sync::Arc, time::SystemTime};
/// Explicit decoder/rendering/store profiles; default installation remains unbound.
pub struct PredecessorInputContext {
    pub repository: cannery_attempts::model::JsonContext,
    pub rendering: RenderingContext,
    pub manifest_decode_budget: usize,
    pub store: Arc<dyn DownloadStore>,
    pub signing_clock: fn() -> SystemTime,
}
#[derive(Clone)]
pub(crate) struct RouteState {
    app: AppState,
    profile: Arc<PredecessorInputContext>,
}
pub fn routes(app: AppState, profile: Arc<PredecessorInputContext>) -> Router {
    Router::new()
        .route(
            "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/inputs/predecessor/{artifact_id}",
            get(input).head(head).fallback(method),
        )
        .with_state(RouteState { app, profile })
}
async fn head() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [
            (header::ALLOW, "GET"),
            (header::CONTENT_TYPE, "application/json"),
            (header::CONTENT_LENGTH, "31"),
        ],
    )
}
async fn method() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "GET")],
        axum::Json(serde_json::json!({"detail":"Method Not Allowed"})),
    )
}
fn missing(message: &str) -> Failure {
    domain(ErrorCode::NotFound, message)
}
#[allow(
    clippy::too_many_lines,
    reason = "Source transaction and transfer ordering is observable"
)]
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/inputs/predecessor/{artifact_id}",
    operation_id = "predecessor_input_api_projects__slug__hypotheses__number__attempts__sequence__inputs_predecessor__artifact_id__get",
    summary = "Predecessor Input",
    description = "Read a verified artifact of the predecessor attempt, under the lease.\n\nHow an experimenter stages an experiment step's ``from: attempt`` input;\nonly an experimenter may, on its runner-driven attempt, and only its\npredecessor's own verified uploads of a role some pinned step reads\n(listed in its claim's ``workflow.inputs.predecessor``) are readable. With\nan object store that presigns, the answer is a redirect (302) to a\nshort-lived presigned GET: follow it without this request's headers, and\ncheck the bytes against the listed size and SHA-256.",
    params(("slug" = String, Path),
        ("number" = i64, Path),
        ("sequence" = i64, Path),
        ("artifact_id" = String, Path, format = "uuid"),
        ("X-Lease-Token" = Option<String>, Header),
        ("X-Lease-Generation" = Option<i64>, Header)),
    responses((status = 200, description = "Verified object bytes", body = Vec<u8>, content_type = "application/octet-stream"),
        (status = 302, description = "Short-lived presigned download URL"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn input(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(failure)?;
    let paths = Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &state)
        .await
        .map_err(failure)?
        .0;
    let artifact = validation::artifact_uuid(&paths["artifact_id"]);
    let path_errors = artifact
        .as_ref()
        .err()
        .map_or(&[][..], |error| error.problems());
    let parameters = attempt_lease_routes::parameters_with_path_errors(
        &paths,
        &parts.headers,
        &context,
        path_errors,
        &[],
    )?;
    let project = attempt_lease_routes::worker_access(
        &mut auth.connection,
        &auth.principal,
        &paths["slug"],
        &context,
    )
    .await?;
    if !matches!(&auth.principal,Principal::Service(worker) if worker.kind==ServiceKind::Experimenter)
    {
        return Err(domain(
            ErrorCode::Forbidden,
            "only an experimenter reads a predecessor attempt's artifacts",
        ));
    }
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "predecessor transaction"))?;
    let attempt = attempt_lease_routes::leased(
        &mut tx,
        &auth.principal,
        &project,
        &parameters,
        state.profile.repository,
        &context,
    )
    .await?;
    let artifact = Repository::new(&mut tx, state.profile.repository)
        .get_artifact(
            project.id,
            ArtifactId(artifact.map_err(|_| internal(&context, "artifact id"))?),
        )
        .await
        .map_err(|_| internal(&context, "predecessor artifact"))?;
    let roles = match &attempt.workflow {
        StoredJson::SqlNull => vec![],
        StoredJson::Value(workflow) => {
            let manifests = crate::attempt_workflow::pinned_manifests(
                &mut crate::step_binding::PgManifestQuery(&mut tx),
                project.id,
                workflow,
                state.profile.rendering,
                state.profile.manifest_decode_budget,
            )
            .await
            .map_err(|_| internal(&context, "pinned predecessor manifests"))?;
            job_baselines::predecessor_roles(&manifests, state.profile.rendering)
                .map_err(|_| internal(&context, "predecessor roles"))?
        }
    };
    tx.commit()
        .await
        .map_err(|_| internal(&context, "predecessor commit"))?;
    let artifact = artifact
        .filter(|artifact| {
            Some(artifact.attempt_id) == attempt.predecessor_id
                && roles.iter().any(|role| role.equals_utf8(&artifact.role))
                && artifact.job_id.is_none()
                && artifact.backend == state.profile.store.backend()
                && artifact.bucket == state.profile.store.bucket()
        })
        .ok_or_else(|| {
            missing("not a verified artifact of this attempt's predecessor that a step reads")
        })?;
    let head = state
        .profile
        .store
        .head(&artifact.key)
        .await
        .map_err(|error| store_error(error, &context))?
        .ok_or_else(|| missing("the artifact's object is missing from the store"))?;
    if i64::try_from(head.size_bytes).ok() != Some(artifact.size_bytes)
        || (artifact.generation.is_some() && artifact.generation != head.generation)
    {
        return Err(missing(
            "the stored object is no longer the verified artifact",
        ));
    }
    let mut response = if let Some(store) = state.profile.store.presigning() {
        let url = store
            .presign_get(
                &artifact.key,
                artifact.key.rsplit('/').next().unwrap_or_default(),
                &artifact.media_type,
                store
                    .presign_ttl
                    .clone()
                    .min(cannery_storage::DOWNLOAD_URL_SECONDS.into()),
                (state.profile.signing_clock)(),
            )
            .await
            .map_err(|error| store_error(error, &context))?;
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::FOUND;
        response
            .headers_mut()
            .insert(header::LOCATION, header_value(&url, &context)?);
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
        response
    } else {
        let store = state.profile.store.clone();
        let key = artifact.key;
        let chunks = futures_util::stream::try_unfold(
            (store, key, None::<ObjectReader>),
            |(store, key, reader)| async move {
                let mut reader = match reader {
                    Some(reader) => reader,
                    None => store.read(&key).await?,
                };
                Ok::<_, StoreError>(
                    reader
                        .next_chunk()
                        .await?
                        .map(|chunk| (chunk, (store, key, Some(reader)))),
                )
            },
        );
        let mut response = Response::new(Body::from_stream(chunks));
        let mut media = artifact.media_type;
        if media.starts_with("text/") && !media.to_lowercase().contains("charset=") {
            media.push_str("; charset=utf-8");
        }
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, header_value(&media, &context)?);
        response.headers_mut().insert(
            header::CONTENT_LENGTH,
            header_value(&artifact.size_bytes.to_string(), &context)?,
        );
        response
    };
    let body = std::mem::replace(response.body_mut(), Body::empty());
    let chunks = futures_util::stream::try_unfold(
        (auth, body.into_data_stream()),
        |(auth, mut body)| async move {
            use futures_util::StreamExt;
            body.next()
                .await
                .transpose()
                .map(|chunk| chunk.map(|chunk| (chunk, (auth, body))))
        },
    );
    *response.body_mut() = Body::from_stream(chunks);
    Ok(response)
}
fn header_value(value: &str, context: &RequestContext) -> Result<HeaderValue, Failure> {
    let bytes = value
        .chars()
        .map(|c| u8::try_from(u32::from(c)))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| internal(context, "predecessor header"))?;
    HeaderValue::from_bytes(&bytes).map_err(|_| internal(context, "predecessor header"))
}
fn store_error(error: StoreError, context: &RequestContext) -> Failure {
    let message = match error {
        StoreError::Store { operation, code } => format!(
            "the object store is unavailable (object store {operation} failed: {code}); retry later"
        ),
        StoreError::Transport { operation, kind } => format!(
            "the object store is unavailable (object store {operation} failed: {kind}); retry later"
        ),
        _ => return internal(context, "predecessor object store"),
    };
    let mut response =
        crate::errors::ApiError::from(DomainError::new(ErrorCode::StoreUnavailable, message))
            .into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
    failure(response)
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
