//! Leased job input reads preserve authorization, lookup and transfer order.
use crate::{
    AppState, artifact_download::DownloadStore, authentication::authenticate,
    requests::RequestContext, validation,
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
    model::{EvidenceId, ManifestId, StoredJson},
    repo::Repository,
};
use cannery_core::{
    errors::{DomainError, ErrorCode},
    ids::JobId,
    json::{Document, Node, NodeId, model},
    principal::{Principal, ServiceKind},
    text,
    timestamps::Timestamp,
};
use cannery_jobs::repo::{self, Job, State as JobState};
use cannery_storage::{Error as StoreError, ObjectReader};
use num_bigint::BigInt;
use std::{collections::BTreeMap, sync::Arc, time::SystemTime};
pub struct JobInputContext {
    pub jobs: repo::JsonContext,
    pub attempts: cannery_attempts::model::JsonContext,
    pub inferred_nesting_budget: usize,
    pub representation_budget: usize,
    pub store: Arc<dyn DownloadStore>,
    pub signing_clock: fn() -> SystemTime,
}
#[derive(Clone)]
pub(crate) struct RouteState {
    app: AppState,
    profile: Arc<JobInputContext>,
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
fn internal(context: &RequestContext) -> Failure {
    failure(context.internal("job input"))
}
fn domain(code: ErrorCode, message: impl Into<String>) -> Failure {
    failure(crate::errors::ApiError::from(DomainError::new(
        code, message,
    )))
}
fn not_found(message: &str) -> Failure {
    domain(ErrorCode::NotFound, message)
}
#[derive(Clone, Copy)]
enum Operation {
    Sheet,
    Evidence,
    Manifest,
    Object,
}
pub fn routes(app: AppState, profile: Arc<JobInputContext>) -> Router {
    Router::new()
        .route(
            "/api/projects/{slug}/jobs/{job_id}/inputs/claimed-sheet",
            get(sheet).head(head).fallback(method),
        )
        .route(
            "/api/projects/{slug}/jobs/{job_id}/inputs/evidence",
            get(evidence).head(head).fallback(method),
        )
        .route(
            "/api/projects/{slug}/jobs/{job_id}/inputs/manifest",
            get(manifest).head(head).fallback(method),
        )
        .route(
            "/api/projects/{slug}/jobs/{job_id}/inputs/object",
            get(object).head(head).fallback(method),
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
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/jobs/{job_id}/inputs/claimed-sheet",
    operation_id = "claimed_sheet_api_projects__slug__jobs__job_id__inputs_claimed_sheet_get",
    summary = "Claimed Sheet",
    description = "The frozen claimed result sheet the job tests (test jobs only).",
    params(("slug" = String, Path),
        ("job_id" = String, Path, format = "uuid"),
        ("X-Lease-Token" = Option<String>, Header),
        ("X-Lease-Generation" = Option<i64>, Header)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::EvidenceEnvelopeRequest, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn sheet(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    read(state, context, request, Operation::Sheet).await
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/jobs/{job_id}/inputs/evidence",
    operation_id = "input_evidence_api_projects__slug__jobs__job_id__inputs_evidence_get",
    summary = "Input Evidence",
    description = "The tester-verified evidence records an evaluation job assesses, in its input order.",
    params(("slug" = String, Path),
        ("job_id" = String, Path, format = "uuid"),
        ("X-Lease-Token" = Option<String>, Header),
        ("X-Lease-Generation" = Option<i64>, Header)),
    responses((status = 200, description = "Successful Response", body = Vec<crate::api_models::EvidenceEnvelopeRequest>, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn evidence(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    read(state, context, request, Operation::Evidence).await
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/jobs/{job_id}/inputs/manifest",
    operation_id = "input_manifest_api_projects__slug__jobs__job_id__inputs_manifest_get",
    summary = "Input Manifest",
    description = "The verified artifact manifest the job reads: the submission's for a test job,\nthe tested outputs' for an evaluation job.",
    params(("slug" = String, Path),
        ("job_id" = String, Path, format = "uuid"),
        ("X-Lease-Token" = Option<String>, Header),
        ("X-Lease-Generation" = Option<i64>, Header)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::ArtifactManifestRequest, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn manifest(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    read(state, context, request, Operation::Manifest).await
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/jobs/{job_id}/inputs/object",
    operation_id = "input_object_api_projects__slug__jobs__job_id__inputs_object_get",
    summary = "Input Object",
    description = "Stream one object listed in the job's input manifest; nothing else is readable.\n\nWith an object store that presigns, the answer is a redirect (302) to a\nshort-lived presigned GET instead: follow it without this request's\nheaders (the URL is the credential), and check the bytes against the\nmanifest's size and SHA-256 as always. Either way the stored object is\nchecked first (a HEAD): one that is gone, or no longer the verified\nartifact (another size or generation), is ``not_found``.",
    params(("slug" = String, Path),
        ("job_id" = String, Path, format = "uuid"),
        ("key" = String, Query, max_length = 1024),
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
pub(crate) async fn object(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    read(state, context, request, Operation::Object).await
}
fn field(
    document: &Document,
    id: NodeId,
    name: &str,
    context: &RequestContext,
) -> Result<NodeId, Failure> {
    if !matches!(document.node(id), Some(Node::Object(_))) {
        return Err(internal(context));
    }
    document.field(id, name).ok_or_else(|| internal(context))
}
fn string(
    document: &Document,
    id: NodeId,
    profile: &JobInputContext,
    context: &RequestContext,
) -> Result<String, Failure> {
    text::str_value(document, id, profile.representation_budget)
        .map_err(|_| internal(context))?
        .as_utf8()
        .ok_or_else(|| internal(context))
}
fn document(value: &StoredJson, context: &RequestContext) -> Result<Arc<Document>, Failure> {
    match value {
        StoredJson::Value(value) => Ok(value.clone()),
        StoredJson::SqlNull => Err(internal(context)),
    }
}
fn uuid(
    document: &Document,
    id: NodeId,
    profile: &JobInputContext,
    context: &RequestContext,
) -> Result<uuid::Uuid, Failure> {
    uuid::Uuid::parse_str(&string(document, id, profile, context)?).map_err(|_| internal(context))
}
fn mapping(
    document: &Document,
    id: NodeId,
    profile: &JobInputContext,
    context: &RequestContext,
) -> Result<Vec<u8>, Failure> {
    model::encode_model_mapping(document, id, profile.inferred_nesting_budget)
        .map_err(|_| internal(context))
}
async fn input_manifest(
    repository: &mut Repository<'_>,
    job: &Job,
    profile: &JobInputContext,
    context: &RequestContext,
) -> Result<Arc<Document>, Failure> {
    let inputs = field(&job.spec, job.spec.root(), "inputs", context)?;
    let reference = field(&job.spec, inputs, "manifest", context)?;
    let id = uuid(
        &job.spec,
        field(&job.spec, reference, "ref", context)?,
        profile,
        context,
    )?;
    let found = repository
        .get_manifest(job.attempt_id, ManifestId(id))
        .await
        .map_err(|_| internal(context))?
        .ok_or_else(|| internal(context))?;
    document(&found.content, context)
}
#[allow(
    clippy::too_many_lines,
    reason = "Retain source dependency, lease and response construction order"
)]
async fn read(
    state: RouteState,
    context: RequestContext,
    request: Request,
    operation: Operation,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(failure)?;
    let paths = Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &state)
        .await
        .map_err(failure)?
        .0;
    let query =
        crate::request_context::QueryParams::parse(parts.uri.query().unwrap_or("").as_bytes());
    let mut errors = vec![];
    let id = match validation::job_uuid(&paths["job_id"]) {
        Ok(value) => Some(JobId(value)),
        Err(error) => {
            errors.extend(error.problems().iter().cloned());
            None
        }
    };
    let key = if matches!(operation, Operation::Object) {
        match validation::job_input_key(query.get("key")) {
            Ok(value) => Some(value),
            Err(error) => {
                errors.extend(error.problems().iter().cloned());
                None
            }
        }
    } else {
        None
    };
    let generation = crate::request_context::first_header(&parts.headers, "x-lease-generation")
        .and_then(
            |raw| match validation::attempt_header_integer("X-Lease-Generation", &raw) {
                Ok(value) => Some(value),
                Err(error) => {
                    errors.extend(error.problems().iter().cloned());
                    None
                }
            },
        );
    if !errors.is_empty() {
        return Err(validation::ValidationErrors::from_problems(errors)
            .domain_error()
            .map_or_else(
                |_| internal(&context),
                |error| failure(crate::errors::ApiError::from(error)),
            ));
    }
    let project = cannery_projects::authz::project_access(
        &mut auth.connection,
        &auth.principal,
        &paths["slug"],
        None,
        &[ServiceKind::Tester, ServiceKind::Evaluator],
        false,
    )
    .await
    .map_err(|error| failure(context.project_error(error)))?
    .project;
    let Principal::Service(worker) = &auth.principal else {
        return Err(internal(&context));
    };
    let kind = if worker.kind == ServiceKind::Tester {
        "tester"
    } else {
        "evaluator"
    };
    let job = repo::get_job(
        &mut auth.connection,
        id.ok_or_else(|| internal(&context))?,
        false,
        state.profile.jobs,
    )
    .await
    .map_err(|_| internal(&context))?
    .filter(|job| job.project_id == project.id)
    .ok_or_else(|| not_found("job not found"))?;
    if job.stage.as_str() != kind {
        return Err(domain(
            ErrorCode::Forbidden,
            format!(
                "a {kind} service account cannot work on {} jobs",
                job.stage.as_str()
            ),
        ));
    }
    if job.claimed_by_service != Some(worker.service_account_id) {
        return Err(domain(
            ErrorCode::Forbidden,
            format!(
                "only the {} that claimed this job can work on it",
                job.stage.as_str()
            ),
        ));
    }
    let token = crate::request_context::first_header(&parts.headers, "x-lease-token");
    let (Some(token), Some(generation)) = (token, generation) else {
        return Err(domain(
            ErrorCode::StaleLease,
            "send X-Lease-Token and X-Lease-Generation",
        ));
    };
    if job.state != JobState::Claimed
        || !job
            .lease_token_hash
            .as_ref()
            .is_some_and(|held| cannery_identity::secrets::matches_digest(held, &token))
        || generation != BigInt::from(job.lease_generation)
    {
        return Err(domain(
            ErrorCode::StaleLease,
            "this lease is no longer valid for the job",
        ));
    }
    let now: Timestamp = sqlx::query_scalar("SELECT now()")
        .fetch_one(&mut *auth.connection)
        .await
        .map_err(|_| internal(&context))?;
    if job.deadline.is_none_or(|value| value.0 <= now.0) {
        return Err(domain(ErrorCode::StaleLease, "the job's deadline passed"));
    }
    if job.lease_expires_at.is_none_or(|value| value.0 <= now.0) {
        return Err(domain(ErrorCode::StaleLease, "the lease expired"));
    }
    let mut repository = Repository::new(&mut auth.connection, state.profile.attempts);
    let bytes = match operation {
        Operation::Sheet | Operation::Evidence => {
            let inputs = field(&job.spec, job.spec.root(), "inputs", &context)?;
            if !matches!(job.spec.node(inputs), Some(Node::Object(_))) {
                return Err(internal(&context));
            }
            let name = if matches!(operation, Operation::Sheet) {
                "claimed_sheet"
            } else {
                "evidence"
            };
            let reference = job
                .spec
                .field(inputs, name)
                .filter(|id| !matches!(job.spec.node(*id), Some(Node::Null)))
                .ok_or_else(|| {
                    not_found(if matches!(operation, Operation::Sheet) {
                        "an evaluation job reads verified evidence, not the claimed sheet"
                    } else {
                        "a test job has no verified evidence input"
                    })
                })?;
            let references = if matches!(operation, Operation::Sheet) {
                vec![reference]
            } else {
                match job.spec.node(reference) {
                    Some(Node::Array(values)) => values.clone(),
                    Some(Node::Object(values)) if values.is_empty() => vec![],
                    Some(Node::String(value)) if value.codepoints().is_empty() => vec![],
                    _ => return Err(internal(&context)),
                }
            };
            let mut records = vec![];
            for reference in references {
                let id = uuid(
                    &job.spec,
                    field(&job.spec, reference, "ref", &context)?,
                    &state.profile,
                    &context,
                )?;
                let found = repository
                    .get_evidence_by_id(job.attempt_id, EvidenceId(id))
                    .await
                    .map_err(|_| internal(&context))?
                    .ok_or_else(|| internal(&context))?;
                records.push(document(&found.0, &context)?);
            }
            if matches!(operation, Operation::Sheet) {
                let record = records.first().ok_or_else(|| internal(&context))?;
                serde_json::to_vec(
                    &crate::api_contract::decode::<crate::api_models::EvidenceEnvelopeRequest>(
                        &mapping(record, record.root(), &state.profile, &context)?,
                    )
                    .map_err(|_| internal(&context))?,
                )
                .map_err(|_| internal(&context))?
            } else {
                let documents = records
                    .iter()
                    .map(|record| {
                        let bytes = mapping(record, record.root(), &state.profile, &context)?;
                        crate::api_contract::decode::<crate::api_models::EvidenceEnvelopeRequest>(
                            &bytes,
                        )
                        .map_err(|_| internal(&context))
                    })
                    .collect::<Result<Vec<_>, Failure>>()?;
                serde_json::to_vec(&documents).map_err(|_| internal(&context))?
            }
        }
        Operation::Manifest => {
            let value = input_manifest(&mut repository, &job, &state.profile, &context).await?;
            serde_json::to_vec(
                &crate::api_contract::decode::<crate::api_models::ArtifactManifestRequest>(
                    &mapping(&value, value.root(), &state.profile, &context)?,
                )
                .map_err(|_| internal(&context))?,
            )
            .map_err(|_| internal(&context))?
        }
        Operation::Object => {
            let value = input_manifest(&mut repository, &job, &state.profile, &context).await?;
            let response = transfer(
                &mut repository,
                value,
                &key.ok_or_else(|| internal(&context))?
                    .as_utf8()
                    .ok_or_else(|| internal(&context))?,
                project.id,
                &state.profile,
                &context,
            )
            .await?;
            let (parts, body) = response.into_parts();
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
            return Ok(Response::from_parts(parts, Body::from_stream(chunks)));
        }
    };
    Ok(([(header::CONTENT_TYPE, "application/json")], bytes).into_response())
}
fn header_value(value: &str, context: &RequestContext) -> Result<HeaderValue, Failure> {
    let bytes = value
        .chars()
        .map(|value| u8::try_from(u32::from(value)).map_err(|_| internal(context)))
        .collect::<Result<Vec<_>, _>>()?;
    HeaderValue::from_bytes(&bytes).map_err(|_| internal(context))
}
fn store_error(error: StoreError, context: &RequestContext) -> Failure {
    match error {
        StoreError::Store { operation, code } => {
            let message = format!(
                "the object store is unavailable (object store {operation} failed: {code}); retry later"
            );
            let mut response = crate::errors::ApiError::from(DomainError::new(
                ErrorCode::StoreUnavailable,
                message,
            ))
            .into_response();
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
            failure(response)
        }
        StoreError::Transport { operation, kind } => {
            let message = format!(
                "the object store is unavailable (object store {operation} failed: {kind}); retry later"
            );
            let mut response = crate::errors::ApiError::from(DomainError::new(
                ErrorCode::StoreUnavailable,
                message,
            ))
            .into_response();
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
            failure(response)
        }
        _ => internal(context),
    }
}
#[allow(
    clippy::too_many_lines,
    reason = "Preserve source manifest, database, HEAD and lazy transfer order"
)]
async fn transfer(
    repository: &mut Repository<'_>,
    value: Arc<Document>,
    key: &str,
    project: cannery_core::ids::ProjectId,
    profile: &JobInputContext,
    context: &RequestContext,
) -> Result<Response, Failure> {
    let objects = field(&value, value.root(), "objects", context)?;
    let objects = match value.node(objects) {
        Some(Node::Array(values)) => values.clone(),
        Some(Node::Object(values)) if values.is_empty() => vec![],
        Some(Node::String(value)) if value.codepoints().is_empty() => vec![],
        _ => return Err(internal(context)),
    };
    let mut listed = None;
    for object in objects {
        let storage = field(&value, object, "storage", context)?;
        let candidate = field(&value, storage, "key", context)?;
        if matches!(value.node(candidate),Some(Node::String(value))if value.equals_utf8(key)) {
            listed = Some(object);
            break;
        }
    }
    let listed = listed.ok_or_else(|| not_found("not an object of this job's inputs"))?;
    let storage = field(&value, listed, "storage", context)?;
    let backend = field(&value, storage, "backend", context)?;
    let bucket = field(&value, storage, "bucket", context)?;
    if !matches!(value.node(backend),Some(Node::String(value))if value.equals_utf8(profile.store.backend()))
        || !matches!(value.node(bucket),Some(Node::String(value))if value.equals_utf8(profile.store.bucket()))
    {
        return Err(not_found("the object is in another store"));
    }
    let media = match value.node(field(&value, listed, "media_type", context)?) {
        Some(Node::String(media)) => media.clone(),
        _ => return Err(internal(context)),
    };
    let verified = repository
        .get_artifact_by_key(
            project,
            profile.store.backend(),
            profile.store.bucket(),
            key,
        )
        .await
        .map_err(|_| internal(context))?;
    let head = profile
        .store
        .head(key)
        .await
        .map_err(|error| store_error(error, context))?
        .ok_or_else(|| not_found("the input object is missing from the store"))?;
    let size = field(&value, listed, "size_bytes", context)?;
    let expected = cannery_research::science::configuration_integer(&value, size)
        .map_err(|_| internal(context))?;
    if BigInt::from(head.size_bytes) != expected
        || verified.as_ref().is_some_and(|artifact| {
            artifact.generation.is_some() && artifact.generation != head.generation
        })
    {
        return Err(not_found(
            "the stored object is no longer the verified artifact",
        ));
    }
    if let Some(store) = profile.store.presigning() {
        let expiry = store
            .presign_ttl
            .clone()
            .min(cannery_storage::DOWNLOAD_URL_SECONDS.into());
        let url = store
            .presign_get(
                key,
                key.rsplit('/').next().unwrap_or_default(),
                &media,
                expiry,
                (profile.signing_clock)(),
            )
            .await
            .map_err(|error| store_error(error, context))?;
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::FOUND;
        response
            .headers_mut()
            .insert(header::LOCATION, header_value(&url, context)?);
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
        return Ok(response);
    }
    let media = if media.starts_with("text/") && !media.to_ascii_lowercase().contains("charset=") {
        format!("{media}; charset=utf-8")
    } else {
        media
    };
    let size = string(&value, size, profile, context)?;
    let store = profile.store.clone();
    let key = key.to_owned();
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
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, header_value(&media, context)?);
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, header_value(&size, context)?);
    Ok(response)
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
