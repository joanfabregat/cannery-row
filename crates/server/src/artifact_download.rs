//! Lazy artifact delivery releases authentication's connection before store I/O.
use crate::{
    AppState,
    artifact_permission::{self, PermissionContext, PermissionError},
    errors::ApiError,
    requests::RequestContext,
};
use axum::{
    Router,
    body::Body,
    extract::{FromRequestParts, Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use cannery_attempts::model::{ArtifactId, JsonContext};
use cannery_core::errors::{DomainError, ErrorCode};
use cannery_storage::{Error as StoreError, ObjectHead, ObjectReader, ObjectStore, s3::S3Store};
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc, time::SystemTime};
pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, StoreError>> + Send + 'a>>;
/// An injectable store protocol; the production implementation delegates to `ObjectStore`.
pub trait DownloadStore: Send + Sync {
    fn backend(&self) -> &str;
    fn bucket(&self) -> &str;
    fn head<'a>(&'a self, key: &'a str) -> StoreFuture<'a, Option<ObjectHead>>;
    fn read<'a>(&'a self, key: &'a str) -> StoreFuture<'a, ObjectReader>;
    fn presigning(&self) -> Option<&S3Store>;
}
impl DownloadStore for ObjectStore {
    fn backend(&self) -> &str {
        self.backend()
    }
    fn bucket(&self) -> &str {
        self.bucket()
    }
    fn head<'a>(&'a self, key: &'a str) -> StoreFuture<'a, Option<ObjectHead>> {
        Box::pin(self.head(key))
    }
    fn read<'a>(&'a self, key: &'a str) -> StoreFuture<'a, ObjectReader> {
        Box::pin(self.read(key))
    }
    fn presigning(&self) -> Option<&S3Store> {
        self.presigning()
    }
}
/// Explicit store, signing clock and repository profiles; no production default is installed.
pub struct DownloadContext {
    pub store: Arc<dyn DownloadStore>,
    pub signing_clock: fn() -> SystemTime,
    pub repository: JsonContext,
}
#[derive(Clone)]
pub(crate) struct RouteState {
    app: AppState,
    context: Arc<DownloadContext>,
}
pub(crate) struct Failure(Box<Response>);
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        *self.0
    }
}
fn failure(value: impl IntoResponse) -> Failure {
    Failure(Box::new(value.into_response()))
}
fn error(code: ErrorCode, message: impl Into<String>) -> Failure {
    failure(ApiError::from(DomainError::new(code, message)))
}
fn internal(context: &RequestContext) -> Failure {
    failure(context.internal("artifact download"))
}
fn passive(stored: &str) -> &str {
    match stored {
        "image/png"
        | "image/jpeg"
        | "image/gif"
        | "image/webp"
        | "image/avif"
        | "text/plain"
        | "text/csv"
        | "application/json"
        | "application/jsonl"
        | "application/x-ndjson"
        | "application/octet-stream" => stored,
        _ => "application/octet-stream",
    }
}
fn filename(key: &str) -> String {
    let name = key
        .rsplit('/')
        .next()
        .unwrap_or("")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let name = name.trim_start_matches('.');
    if name.is_empty() {
        "artifact".into()
    } else {
        name.into()
    }
}
fn etag_matches(value: Option<&str>, etag: &str) -> bool {
    value.is_some_and(|value| {
        value.split(',').any(|tag| {
            let tag = tag.trim_matches(|c| cannery_core::text::whitespace(u32::from(c)));
            tag == "*" || tag.strip_prefix("W/").unwrap_or(tag) == etag
        })
    })
}
fn header_value(value: &str, context: &RequestContext) -> Result<HeaderValue, Failure> {
    HeaderValue::from_str(value).map_err(|_| internal(context))
}
/// Mount only the artifact GET operation; HEAD follows the source method contract.
pub fn routes(app: AppState, context: Arc<DownloadContext>) -> Router {
    Router::new()
        .route(
            "/api/projects/{slug}/artifacts/{artifact_id}",
            get(download).head(head),
        )
        .with_state(RouteState { app, context })
}
async fn head() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "GET")],
        axum::Json(serde_json::json!({"detail": "Method Not Allowed"})),
    )
}
#[allow(
    clippy::too_many_lines,
    reason = "Preserve path/auth, permission, conditional, HEAD and transfer phases"
)]
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/artifacts/{artifact_id}",
    operation_id = "download_artifact_api_projects__slug__artifacts__artifact_id__get",
    summary = "Download Artifact",
    description = "Stream one verified artifact as an attachment.\n\n``report_asset`` objects are readable by everyone who reads the project;\nevery other role needs ``member`` or above (``forbidden`` otherwise), except\nfor the holder of the work that takes the artifact as an input, while its\nlease runs: a claimed verify job (an object of its input manifest), a\nclaimed document or decide job (an artifact of the unit's attempts; its\n``inputs/artifacts`` lists them), or an attempt in progress whose plan\nentry names the artifact as a context item. The\nETag is the artifact's SHA-256; ``If-None-Match`` with it answers 304. A\nstored object that is gone or no longer the verified one is\n``not_found``; an unreachable object store is ``store_unavailable``. With an S3\nstore, the answer is a redirect (302) to a presigned GET that names the\nfilename and the served media type, valid for a few minutes.",
    params(("slug" = String, Path),
        ("artifact_id" = String, Path, format = "uuid")),
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
pub(crate) async fn download(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let paths = Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &state)
        .await
        .map_err(failure)?
        .0;
    let raw = paths.get("artifact_id").ok_or_else(|| internal(&context))?;
    let id = crate::validation::artifact_uuid(raw).map_err(|errors| {
        errors.domain_error().map_or_else(
            |_| internal(&context),
            |error| failure(ApiError::from(error)),
        )
    })?;
    let slug = paths.get("slug").ok_or_else(|| internal(&context))?;
    let artifact = {
        let mut auth = crate::authentication::authenticate(
            &state.app,
            &context,
            &parts.headers,
            &parts.method,
        )
        .await
        .map_err(failure)?;
        artifact_permission::permitted_artifact(
            &mut auth.connection,
            &auth.principal,
            slug,
            ArtifactId(id),
            PermissionContext {
                backend: state.context.store.backend(),
                bucket: state.context.store.bucket(),
                repository: state.context.repository,
            },
        )
        .await
        .map_err(|error| match error {
            PermissionError::Domain(error) => failure(ApiError::from(error)),
            PermissionError::Project(error) => failure(context.project_error(error)),
            PermissionError::Attempt(_) => internal(&context),
        })?
    };
    let etag = format!("\"{}\"", artifact.sha256);
    let mut headers = HeaderMap::from_iter([
        (
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ),
        (
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'none'; sandbox"),
        ),
        (
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-cache"),
        ),
        (header::ETAG, header_value(&etag, &context)?),
    ]);
    let conditional = crate::request_context::first_header(&parts.headers, "if-none-match");
    if etag_matches(conditional.as_deref(), &etag) {
        let empty = futures_util::stream::empty::<Result<Vec<u8>, StoreError>>();
        let mut response = Response::new(Body::from_stream(empty));
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        *response.headers_mut() = headers;
        return Ok(response);
    }
    let head = state
        .context
        .store
        .head(&artifact.key)
        .await
        .map_err(|error| match error {
            StoreError::Io(_)
            | StoreError::Body(_)
            | StoreError::Store { .. }
            | StoreError::Transport { .. } => error_response(),
            _ => internal(&context),
        })?
        .ok_or_else(|| {
            error(
                ErrorCode::NotFound,
                "the artifact's object is missing from the store",
            )
        })?;
    if u64::try_from(artifact.size_bytes).ok() != Some(head.size_bytes)
        || (artifact.generation.is_some() && artifact.generation != head.generation)
    {
        return Err(error(
            ErrorCode::NotFound,
            "the stored object is no longer the verified artifact",
        ));
    }
    let name = filename(&artifact.key);
    let media = passive(&artifact.media_type);
    headers.insert(
        header::CONTENT_DISPOSITION,
        header_value(&format!("attachment; filename=\"{name}\""), &context)?,
    );
    if let Some(store) = state.context.store.presigning() {
        let expiry = store
            .presign_ttl
            .clone()
            .min(cannery_storage::DOWNLOAD_URL_SECONDS.into());
        let url = store
            .presign_get(
                &artifact.key,
                &name,
                media,
                expiry,
                (state.context.signing_clock)(),
            )
            .await
            .map_err(|_| internal(&context))?;
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::FOUND;
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
            .headers_mut()
            .insert(header::ETAG, header_value(&etag, &context)?);
        response
            .headers_mut()
            .insert(header::LOCATION, header_value(&url, &context)?);
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
        return Ok(response);
    }
    headers.insert(
        header::CONTENT_LENGTH,
        header_value(&artifact.size_bytes.to_string(), &context)?,
    );
    let media = if media.starts_with("text/") {
        format!("{media}; charset=utf-8")
    } else {
        media.to_owned()
    };
    headers.insert(header::CONTENT_TYPE, header_value(&media, &context)?);
    let store = state.context.store.clone();
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
    *response.headers_mut() = headers;
    Ok(response)
}
fn error_response() -> Failure {
    error(
        ErrorCode::StoreUnavailable,
        "the object store is unavailable",
    )
}
