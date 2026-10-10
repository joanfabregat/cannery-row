//! Upload grant and transfer phases borrow the caller connection and explicit profiles.
use crate::{
    AppState,
    attempt_lease_routes::{self, Failure, domain, failure, internal},
    authentication::authenticate,
    body::{self, DecodedBody},
    requests::RequestContext,
    upload_direct, upload_request,
    validation::{self, BodyInput},
};
use axum::{
    Router,
    extract::{FromRequestParts, Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{post, put},
};
use cannery_attempts::{
    model::{JsonContext, Transfer, Upload, UploadId, UploadState},
    repo::{AddArtifact, CreateUpload, RecordFailure, Repository},
};
use cannery_core::{
    audit::Record,
    errors::{DomainError, ErrorCode},
    json::{self, Document},
    timestamps::Timestamp,
};
use cannery_identity::secrets;
use cannery_storage::{ObjectStore, StoredObject};
use futures_util::StreamExt;
use num_bigint::BigInt;
use sqlx::{Acquire, PgConnection};
use std::{collections::BTreeMap, sync::Arc, time::SystemTime};

pub struct UploadContext {
    pub cancellation: Arc<CancellationTasks>,
    /// Bound for task-owned cancellation settlement, selected explicitly by caller.
    pub settlement_timeout: std::time::Duration,
    pub repository: JsonContext,
    pub store: Arc<ObjectStore>,
    pub signing_clock: fn() -> SystemTime,
    pub mint:
        fn() -> Result<cannery_core::principal::Secret, cannery_identity::error::IdentityError>,
    pub nonce: fn() -> Result<String, cannery_identity::error::IdentityError>,
}
#[derive(Clone)]
pub(crate) struct RouteState {
    pub(crate) app: AppState,
    pub(crate) profile: Arc<UploadContext>,
    pub(crate) jobs: Option<Arc<crate::job_upload_routes::JobUploadContext>>,
}
pub fn routes(app: AppState, profile: Arc<UploadContext>) -> Router {
    Router::new()
        .route(
            "/api/projects/{slug}/units/{number}/attempts/{sequence}/uploads",
            post(create),
        )
        .route("/api/uploads/{upload_id}", put(rest_put_upload))
        .route(
            "/api/uploads/{upload_id}/presign",
            post(rest_presign_upload),
        )
        .route("/api/uploads/{upload_id}/finish", post(rest_finish_upload))
        .with_state(RouteState {
            app,
            profile,
            jobs: None,
        })
}
pub(crate) fn job_routes(
    app: AppState,
    jobs: Arc<crate::job_upload_routes::JobUploadContext>,
) -> Router {
    let profile = jobs.uploads.clone();
    Router::new()
        .route(
            "/api/projects/{slug}/jobs/{job_id}/uploads",
            post(crate::job_upload_routes::create),
        )
        .route("/api/job-uploads/{upload_id}", put(rest_put_job_upload))
        .route(
            "/api/job-uploads/{upload_id}/presign",
            post(rest_presign_job_upload),
        )
        .route(
            "/api/job-uploads/{upload_id}/finish",
            post(rest_finish_job_upload),
        )
        .with_state(RouteState {
            app,
            profile,
            jobs: Some(jobs),
        })
}
fn input(body: &DecodedBody) -> BodyInput<'_> {
    match body {
        DecodedBody::Missing => BodyInput::Missing,
        DecodedBody::RawBytes => BodyInput::RawBytes,
        DecodedBody::Json(d) => BodyInput::Json(d),
    }
}
fn validation(error: &validation::ValidationErrors, context: &RequestContext) -> Failure {
    error.domain_error().map_or_else(
        |_| internal(context, "upload validation"),
        |e| failure(crate::errors::ApiError::from(e)),
    )
}
async fn now(conn: &mut PgConnection, context: &RequestContext) -> Result<Timestamp, Failure> {
    sqlx::query_scalar("SELECT now()")
        .fetch_one(conn)
        .await
        .map_err(|_| internal(context, "upload database clock"))
}
fn unknown() -> Failure {
    domain(ErrorCode::NotFound, "unknown upload or wrong upload token")
}
async fn granted(
    conn: &mut PgConnection,
    profile: JsonContext,
    id: UploadId,
    token: Option<&str>,
    job: bool,
    context: &RequestContext,
) -> Result<Upload, Failure> {
    Repository::new(conn, profile)
        .get_upload(id, false)
        .await
        .map_err(|_| internal(context, "upload token lookup"))?
        .filter(|u| {
            u.job_id.is_some() == job
                && token.is_some_and(|t| secrets::matches_digest(&u.token_hash, t))
        })
        .ok_or_else(unknown)
}
async fn begin(
    conn: &mut PgConnection,
    profile: JsonContext,
    found: &Upload,
    jobs: Option<&crate::job_upload_routes::JobUploadContext>,
    context: &RequestContext,
) -> Result<Upload, Failure> {
    let mut tx = conn
        .begin()
        .await
        .map_err(|_| internal(context, "upload begin transaction"))?;
    let attempt = Repository::new(&mut tx, profile)
        .get_attempt_by_id(found.attempt_id, true)
        .await
        .map_err(|_| internal(context, "upload attempt lock"))?
        .ok_or_else(|| internal(context, "upload attempt invariant"))?;
    let job = crate::job_upload_routes::locked_job(&mut tx, found, jobs, context).await?;
    let upload = Repository::new(&mut tx, profile)
        .get_upload(found.id, true)
        .await
        .map_err(|_| internal(context, "upload row lock"))?
        .filter(|u| u.token_hash == found.token_hash)
        .ok_or_else(unknown)?;
    if !crate::job_upload_routes::live(
        &attempt,
        job.as_ref(),
        &upload,
        now(&mut tx, context).await?,
    ) {
        return Err(domain(
            ErrorCode::StaleLease,
            if job.is_some() {
                "the job's lease ended"
            } else {
                "the attempt's lease ended"
            },
        ));
    }
    if let Some(jobs) = jobs
        && let Some(reference) = &upload.interface
    {
        crate::job_output_interface::load(&mut tx, &attempt, reference, &jobs.lifecycle, context)
            .await?;
    }
    if !Repository::new(&mut tx, profile)
        .begin_receiving(upload.id)
        .await
        .map_err(|_| internal(context, "upload receiving mutation"))?
    {
        return Err(domain(
            ErrorCode::Conflict,
            "this upload was already received, is in progress, or expired",
        ));
    }
    tx.commit()
        .await
        .map_err(|_| internal(context, "upload receiving commit"))?;
    Ok(upload)
}
fn response(status: StatusCode, bytes: Vec<u8>) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json")], bytes).into_response()
}
#[allow(
    clippy::too_many_lines,
    reason = "Source planning, locked grant and post-commit response order"
)]
#[utoipa::path(
    post,
    path = "/api/projects/{slug}/units/{number}/attempts/{sequence}/uploads",
    operation_id = "create_upload_api_projects__slug__units__number__attempts__sequence__uploads_post",
    summary = "Create Upload",
    description = "Grant a one-time, create-only upload of one file of the attempt.\n\nWith an object store that presigns, the grant also carries ``direct``:\nthe presigned request(s) that send the bytes straight to the bucket, then\na finish request (see ``docs/contracts.md``).",
    params(("slug" = String, Path),
        ("number" = i64, Path),
        ("sequence" = i64, Path),
        ("X-Lease-Token" = Option<String>, Header),
        ("X-Lease-Generation" = Option<i64>, Header)),
    request_body(content = crate::api_models::UploadRequest, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::UploadGrant, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn create(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = body::read_body(request).await.map_err(failure)?;
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(failure)?;
    let body = crate::api_contract::typed_body::<crate::api_models::UploadRequest>(body)
        .map_err(failure)?;
    let paths = Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &state)
        .await
        .map_err(failure)?
        .0;
    let checked = upload_request::upload(input(&body));
    let parameters = attempt_lease_routes::parameters_with_body(
        &paths,
        &parts.headers,
        &context,
        checked.as_ref().err().map_or(&[][..], |e| e.problems()),
    )?;
    let body = checked.map_err(|e| validation(&e, &context))?;
    let project = attempt_lease_routes::worker_access(
        &mut auth.connection,
        &auth.principal,
        &paths["slug"],
        &context,
    )
    .await?;
    if &body.size_bytes > state.app.settings.storage.max_object_bytes.as_bigint() {
        return Err(domain(
            ErrorCode::ValidationFailed,
            format!(
                "objects are limited to {} bytes",
                state.app.settings.storage.max_object_bytes.as_bigint()
            ),
        ));
    }
    let secret = (state.profile.mint)().map_err(|_| internal(&context, "upload mint"))?;
    let number = validation::attempt_path_integer("number", &paths["number"])
        .map_err(|e| validation(&e, &context))?;
    let sequence = validation::attempt_path_integer("sequence", &paths["sequence"])
        .map_err(|e| validation(&e, &context))?;
    let found = Repository::new(&mut auth.connection, state.profile.repository)
        .get_attempt(project.id, &number, &sequence, false)
        .await
        .map_err(|_| internal(&context, "upload unlocked attempt"))?
        .ok_or_else(|| {
            domain(
                ErrorCode::NotFound,
                format!("attempt #{number}.{sequence} not found"),
            )
        })?;
    let slot = format!(
        "projects/{}/attempts/{}/{}/{}",
        project.id.0, found.id.0, body.role, body.name
    );
    let store = &state.profile.store;
    let bucket = store.presigning();
    let (key, plan) = if let Some(bucket) = bucket {
        let nonce = (state.profile.nonce)().map_err(|_| internal(&context, "upload key nonce"))?;
        let (head, name) = slot
            .rsplit_once('/')
            .ok_or_else(|| internal(&context, "upload slot"))?;
        let key = format!("{head}/{nonce}/{name}");
        let plan =
            upload_direct::plan(bucket, &key, &body.size_bytes, &body.media_type, &context).await?;
        (key, plan)
    } else {
        (
            slot.clone(),
            upload_direct::Plan {
                transfer: "stream",
                id: None,
                part_size: None,
            },
        )
    };
    let mut plan_cleanup = PlanCleanup {
        store: state.profile.store.clone(),
        tasks: state.profile.cancellation.clone(),
        key: key.clone(),
        id: plan.id.clone(),
        timeout: state.profile.settlement_timeout,
    };
    let result = async {
        let mut tx = auth
            .connection
            .begin()
            .await
            .map_err(|_| internal(&context, "upload grant transaction"))?;
        let attempt = attempt_lease_routes::leased(
            &mut tx,
            &auth.principal,
            &project,
            &parameters,
            state.profile.repository,
            &context,
        )
        .await?;
        if attempt.id != found.id {
            return Err(internal(&context, "upload attempt identity invariant"));
        }
        let upload = Repository::new(&mut tx, state.profile.repository)
            .create_upload(CreateUpload {
                attempt_id: attempt.id,
                lease_generation: &BigInt::from(attempt.lease_generation),
                token_hash: &secrets::digest(secret.expose()),
                role: &body.role,
                backend: store.backend(),
                bucket: store.bucket(),
                key: &key,
                declared_size: &body.size_bytes,
                declared_sha256: &body.sha256,
                media_type: &body.media_type,
                ttl_minutes: state.app.settings.storage.upload_ttl_minutes.as_bigint(),
                max_stream_seconds: state.app.settings.storage.max_stream_seconds.as_bigint(),
                job_id: None,
                interface: None,
                transfer: plan.transfer,
                multipart_upload_id: plan.id.as_deref(),
                part_size: plan.part_size.as_ref(),
                slot: Some(&slot),
            })
            .await
            .map_err(|_| internal(&context, "upload grant insert"))?
            .ok_or_else(|| {
                domain(
                    ErrorCode::Conflict,
                    format!(
                        "{}/{} was already granted or uploaded",
                        body.role, body.name
                    ),
                )
            })?;
        if Repository::new(&mut tx, state.profile.repository)
            .count_open_uploads(attempt.id, None)
            .await
            .map_err(|_| internal(&context, "upload grant count"))?
            > 32
        {
            return Err(domain(
                ErrorCode::Conflict,
                "an attempt holds at most 32 open upload grants",
            ));
        }
        Repository::new(&mut tx, state.profile.repository)
            .mark_running(attempt.id)
            .await
            .map_err(|_| internal(&context, "upload mark running"))?;
        let direct = if let Some(bucket) = bucket {
            let now = now(&mut tx, &context).await?;
            let lifetime = upload_direct::lifetime(bucket, &upload, now, &context)?;
            let signed = upload_direct::presign(
                bucket,
                &upload,
                None,
                &lifetime,
                now,
                (state.profile.signing_clock)(),
                &context,
            )
            .await?;
            Repository::new(&mut tx, state.profile.repository)
                .record_urls(upload.id, &lifetime)
                .await
                .map_err(|_| internal(&context, "upload grant URLs"))?;
            let base = format!(
                "{}/api/uploads/{}",
                state
                    .app
                    .settings
                    .server
                    .public_base_url
                    .trim_end_matches('/'),
                upload.id.0
            );
            Some(upload_direct::Grant {
                transfer: if upload.transfer == Transfer::Multipart {
                    "multipart"
                } else {
                    "single"
                },
                request: signed.request,
                parts: signed.parts,
                part_size: upload.part_size,
                part_count: upload
                    .part_size
                    .map(|part| {
                        cannery_storage::s3::part_count(
                            &BigInt::from(upload.declared_size),
                            &BigInt::from(part),
                        )
                    })
                    .transpose()
                    .map_err(|_| internal(&context, "upload grant part count"))?
                    .map(|count| {
                        num_traits::ToPrimitive::to_i64(&count)
                            .ok_or_else(|| internal(&context, "upload grant count response"))
                    })
                    .transpose()?,
                presign_url: format!("{base}/presign"),
                finish_url: format!("{base}/finish"),
            })
        } else {
            None
        };
        tx.commit()
            .await
            .map_err(|_| internal(&context, "upload grant commit"))?;
        Ok::<_, Failure>((upload, direct))
    }
    .await;
    let (upload, direct) = match result {
        Ok(value) => value,
        Err(error) => {
            upload_direct::discard(bucket, &key, &plan).await;
            plan_cleanup.id = None;
            return Err(error);
        }
    };
    plan_cleanup.id = None;
    let bytes = serde_json::to_vec(&crate::api_models::UploadGrant {
        upload_url: format!(
            "{}/api/uploads/{}",
            state
                .app
                .settings
                .server
                .public_base_url
                .trim_end_matches('/'),
            upload.id.0
        ),
        method: Some("PUT".to_owned()),
        headers: BTreeMap::from([("X-Upload-Token".to_owned(), secret.expose().to_owned())]),
        expires_at: upload.expires_at.model_isoformat(),
        storage: crate::api_models::StorageRef {
            backend: upload.backend.clone(),
            bucket: upload.bucket.clone(),
            key: upload.key.clone(),
        },
        direct: direct
            .map(crate::api_contract::convert)
            .transpose()
            .map_err(|_| internal(&context, "direct upload contract"))?,
    })
    .map_err(|_| internal(&context, "upload grant response"))?;
    Ok(response(StatusCode::CREATED, bytes))
}
async fn transfer_parameters(
    parts: &mut axum::http::request::Parts,
    state: &RouteState,
    body_errors: &[validation::Problem],
    context: &RequestContext,
) -> Result<(UploadId, Option<String>), Failure> {
    let paths = Path::<BTreeMap<String, String>>::from_request_parts(parts, state)
        .await
        .map_err(failure)?
        .0;
    let checked = validation::upload_uuid(&paths["upload_id"]);
    let mut errors = checked
        .as_ref()
        .err()
        .map_or_else(Vec::new, |e| e.problems().to_vec());
    errors.extend_from_slice(body_errors);
    if !errors.is_empty() {
        return Err(validation(
            &validation::ValidationErrors::from_problems(errors),
            context,
        ));
    }
    Ok((
        UploadId(checked.map_err(|e| validation(&e, context))?),
        crate::request_context::first_header(&parts.headers, "x-upload-token"),
    ))
}
async fn presign(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = body::read_body(request).await.map_err(failure)?;
    let checked = upload_request::presign(input(&body));
    let (id, token) = transfer_parameters(
        &mut parts,
        &state,
        checked.as_ref().err().map_or(&[][..], |e| e.problems()),
        &context,
    )
    .await?;
    let _numbers = checked.map_err(|e| validation(&e, &context))?;
    let mut conn = state
        .app
        .pool
        .acquire()
        .await
        .map_err(|_| internal(&context, "presign connection"))?;
    let found = granted(
        &mut conn,
        state.profile.repository,
        id,
        token.as_deref(),
        state.jobs.is_some(),
        &context,
    )
    .await?;
    let body = crate::api_contract::typed_body::<Option<crate::api_models::PresignRequest>>(body)
        .map_err(failure)?;
    let numbers =
        upload_request::presign(input(&body)).map_err(|error| validation(&error, &context))?;
    let bucket = upload_direct::store(&state.profile.store, &found)?;
    let mut tx = conn
        .begin()
        .await
        .map_err(|_| internal(&context, "presign transaction"))?;
    let attempt = Repository::new(&mut tx, state.profile.repository)
        .get_attempt_by_id(found.attempt_id, true)
        .await
        .map_err(|_| internal(&context, "presign attempt lock"))?
        .ok_or_else(|| internal(&context, "presign attempt invariant"))?;
    let job =
        crate::job_upload_routes::locked_job(&mut tx, &found, state.jobs.as_deref(), &context)
            .await?;
    let upload = Repository::new(&mut tx, state.profile.repository)
        .get_upload(found.id, true)
        .await
        .map_err(|_| internal(&context, "presign upload lock"))?
        .filter(|u| u.token_hash == found.token_hash)
        .ok_or_else(unknown)?;
    let now = now(&mut tx, &context).await?;
    if !crate::job_upload_routes::live(&attempt, job.as_ref(), &upload, now) {
        return Err(domain(
            ErrorCode::StaleLease,
            if job.is_some() {
                "the job's lease ended"
            } else {
                "the attempt's lease ended"
            },
        ));
    }
    if upload.state != UploadState::Pending || upload.expires_at.0 <= now.0 {
        return Err(domain(
            ErrorCode::Conflict,
            "this upload was already received, is in progress, or expired",
        ));
    }
    let lifetime = upload_direct::lifetime(bucket, &upload, now, &context)?;
    let signed = upload_direct::presign(
        bucket,
        &upload,
        numbers,
        &lifetime,
        now,
        (state.profile.signing_clock)(),
        &context,
    )
    .await?;
    Repository::new(&mut tx, state.profile.repository)
        .record_urls(upload.id, &lifetime)
        .await
        .map_err(|_| internal(&context, "presign record URLs"))?;
    tx.commit()
        .await
        .map_err(|_| internal(&context, "presign commit"))?;
    Ok(response(
        StatusCode::OK,
        serde_json::to_vec(
            &crate::api_contract::convert::<crate::api_models::PresignOut>(signed)
                .map_err(|_| internal(&context, "presign contract"))?,
        )
        .map_err(|_| internal(&context, "presign response"))?,
    ))
}
async fn receive(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = request.into_parts();
    let (id, token) = transfer_parameters(&mut parts, &state, &[], &context).await?;
    let mut conn = state
        .app
        .pool
        .acquire()
        .await
        .map_err(|_| internal(&context, "upload connection"))?;
    let found = granted(
        &mut conn,
        state.profile.repository,
        id,
        token.as_deref(),
        state.jobs.is_some(),
        &context,
    )
    .await?;
    if found.transfer == Transfer::Multipart {
        return Err(domain(
            ErrorCode::Conflict,
            "this upload goes to the object store in parts: use its direct URLs",
        ));
    }
    let upload = begin(
        &mut conn,
        state.profile.repository,
        &found,
        state.jobs.as_deref(),
        &context,
    )
    .await?;
    // Obtain is inside the receiving cleanup scope, including the pre-write delete.
    let obtain = async {
        state
            .profile
            .store
            .delete(&upload.key, None)
            .await
            .map_err(|e| upload_direct::store_error(e, &context))?;
        let chunks = body.into_data_stream().map(|chunk| {
            chunk
                .map(|v| v.to_vec())
                .map_err(|_| cannery_storage::Error::Body(std::io::ErrorKind::UnexpectedEof))
        });
        match state
            .profile
            .store
            .write_new_with_cancellation(
                &upload.key,
                chunks,
                BigInt::from(upload.declared_size) + 1,
                state
                    .profile
                    .cancellation
                    .store_sink(state.profile.settlement_timeout),
            )
            .await
        {
            Ok(stored) => Ok(Some(stored)),
            Err(cannery_storage::Error::ObjectTooLarge) => Ok(None),
            Err(cannery_storage::Error::ObjectExists) => Err(domain(
                ErrorCode::Conflict,
                "another upload wrote this key; retry the PUT",
            )),
            Err(e) => Err(upload_direct::store_error(e, &context)),
        }
    };
    Box::pin(settle(
        &state.app.pool,
        &mut conn,
        &state.profile,
        state.jobs.as_deref(),
        &upload,
        obtain,
        &context,
    ))
    .await
}
async fn finish(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _body) = request.into_parts();
    let (id, token) = transfer_parameters(&mut parts, &state, &[], &context).await?;
    let mut conn = state
        .app
        .pool
        .acquire()
        .await
        .map_err(|_| internal(&context, "upload finish connection"))?;
    let found = granted(
        &mut conn,
        state.profile.repository,
        id,
        token.as_deref(),
        state.jobs.is_some(),
        &context,
    )
    .await?;
    let bucket = upload_direct::store(&state.profile.store, &found)?;
    let upload = begin(
        &mut conn,
        state.profile.repository,
        &found,
        state.jobs.as_deref(),
        &context,
    )
    .await?;
    let obtain = async {
        upload_direct::verify(bucket, &upload, &context)
            .await
            .map(Some)
    };
    Box::pin(settle(
        &state.app.pool,
        &mut conn,
        &state.profile,
        state.jobs.as_deref(),
        &upload,
        obtain,
        &context,
    ))
    .await
}
fn details(upload: &Upload, stored: Option<&StoredObject>) -> serde_json::Value {
    serde_json::json!({"key":upload.key,"declared_size":upload.declared_size,"declared_sha256":upload.declared_sha256,"received_size":stored.map(|s|s.size_bytes),"received_sha256":stored.map(|s|&s.sha256)})
}
fn document(
    value: &serde_json::Value,
    profile: JsonContext,
    context: &RequestContext,
) -> Result<Document, Failure> {
    let bytes =
        serde_json::to_vec(value).map_err(|_| internal(context, "upload failure details"))?;
    json::decode(&bytes, profile.encode_nesting_budget)
        .map_err(|_| internal(context, "upload failure details document"))
}
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "Source obtain, transaction, finally cleanup and post-commit error phases"
)]
async fn settle<F>(
    pool: &sqlx::PgPool,
    conn: &mut PgConnection,
    profile: &UploadContext,
    jobs: Option<&crate::job_upload_routes::JobUploadContext>,
    upload: &Upload,
    obtain: F,
    context: &RequestContext,
) -> Result<Response, Failure>
where
    F: std::future::Future<Output = Result<Option<StoredObject>, Failure>>,
{
    let mut cancellation = ReceiveCleanup {
        tasks: profile.cancellation.clone(),
        pool: pool.clone(),
        store: profile.store.clone(),
        id: upload.id,
        key: upload.key.clone(),
        repository: profile.repository,
        timeout: profile.settlement_timeout,
        stored: None,
        settled: false,
        armed: true,
    };
    let mut stored = None;
    let mut artifact = None;
    let mut failed = None;
    let mut refused = Vec::new();
    let mut content_validated = None;
    let mut settled = false;
    let result = async {
        stored = obtain.await?;
        cancellation.stored.clone_from(&stored);
        if let (Some(jobs), Some(reference), Some(object)) = (jobs, upload.interface.as_deref(), stored.as_ref()) {
            let attempt = Repository::new(conn, profile.repository).get_attempt_by_id(upload.attempt_id, false).await.map_err(|_|internal(context,"output check attempt"))?.ok_or_else(||internal(context,"output check attempt missing"))?;
            if let Some(interface) = crate::job_output_interface::load(conn, &attempt, reference, &jobs.lifecycle, context).await? {
                let _slot = tokio::time::timeout(jobs.validation_timeout, jobs.validation_slots.acquire()).await.map_err(|_| domain(ErrorCode::StoreUnavailable,"output validation is busy; retry later"))?.map_err(|_|internal(context,"output validation shutdown"))?;
                let (issues, validated) = tokio::time::timeout(jobs.validation_timeout, crate::job_output_interface::check(&profile.store,&upload.key,object.size_bytes,interface,jobs.json_max_bytes,&jobs.lifecycle,context)).await.map_err(|_| domain(ErrorCode::StoreUnavailable,"output validation exceeded its deadline; retry later"))??;
                refused = issues;
                content_validated = Some(validated);
            } else { content_validated = Some(false); }
        } else if jobs.is_some() { content_validated = Some(false); }
        let matches = stored.as_ref().is_some_and(|s| {
            BigInt::from(s.size_bytes) == BigInt::from(upload.declared_size)
                && s.sha256 == upload.declared_sha256
        });
        let mut tx = conn
            .begin()
            .await
            .map_err(|_| internal(context, "upload settle transaction"))?;
        let attempt = Repository::new(&mut tx, profile.repository)
            .get_attempt_by_id(upload.attempt_id, true)
            .await
            .map_err(|_| internal(context, "upload settle attempt lock"))?
            .ok_or_else(|| internal(context, "upload settle attempt invariant"))?;
        let job = crate::job_upload_routes::locked_job(&mut tx, upload, jobs, context).await?;
        let current = Repository::new(&mut tx, profile.repository)
            .get_upload(upload.id, true)
            .await
            .map_err(|_| internal(context, "upload settle upload lock"))?;
        if !crate::job_upload_routes::live(&attempt, job.as_ref(), upload, now(&mut tx, context).await?) {
            return Err(domain(
                ErrorCode::StaleLease,
                if job.is_some() { "the job's lease ended before the upload completed" } else { "the attempt's lease ended before the upload completed" },
            ));
        }
        if !current
            .is_some_and(|u| u.state == UploadState::Receiving && u.token_hash == upload.token_hash)
        {
            return Err(domain(
                ErrorCode::UploadExpired,
                "the upload grant ended before the upload completed",
            ));
        }
        if let Some(job) = job.as_ref() && !refused.is_empty() {
            let refusal = document(&serde_json::json!(refused), profile.repository, context)?;
            Repository::new(&mut tx, profile.repository).finish_upload(upload.id,"failed",Some(&refusal),stored.is_some()).await.map_err(|_|internal(context,"job output refusal"))?;
            crate::job_upload_routes::event(&mut tx,&attempt,job,"artifact.refused","upload",&upload.id.0.to_string(),&serde_json::json!({"role":upload.role,"key":upload.key,"interface":upload.interface,"job_id":job.id.to_string(),"issues":refused}),Some("output does not match its registered interface"),context).await?;
        } else if matches {
            let stored = stored
                .as_ref()
                .ok_or_else(|| internal(context, "upload stored invariant"))?;
            let value = Repository::new(&mut tx, profile.repository)
                .add_artifact(AddArtifact {
                    project_id: attempt.project_id,
                    upload,
                    size_bytes: &BigInt::from(stored.size_bytes),
                    sha256: &stored.sha256,
                    generation: stored.generation.as_deref(),
                    content_validated,
                })
                .await
                .map_err(|_| internal(context, "upload artifact insert"))?;
            artifact = Some(value);
            cancellation.armed = false;
            let value = artifact
                .as_ref()
                .ok_or_else(|| internal(context, "upload artifact invariant"))?;
            Repository::new(&mut tx, profile.repository)
                .finish_upload(upload.id, "verified", None, false)
                .await
                .map_err(|_| internal(context, "upload verified state"))?;
            if job.is_none() { Repository::new(&mut tx, profile.repository).mark_running(attempt.id).await.map_err(|_| internal(context, "upload running state"))?; }
            let new = serde_json::json!({"role":value.role,"key":value.key,"sha256":value.sha256});
            if let Some(job) = job.as_ref() {
                crate::job_upload_routes::event(&mut tx,&attempt,job,"artifact.verified","artifact",&value.id.0.to_string(),&serde_json::json!({"role":value.role,"key":value.key,"sha256":value.sha256,"job_id":job.id.to_string()}),None,context).await?;
            } else { crate::upload_audit::record(
                &mut tx,
                &attempt,
                crate::upload_audit::actor(&attempt, context)?,
                Record {
                    action: "artifact.verified",
                    subject_type: "artifact",
                    subject_id: &value.id.0.to_string(),
                    project_id: Some(attempt.project_id),
                    prior_state: None,
                    new_state: Some(&new),
                    reason: None,
                    idempotency_key: None,
                },
                context,
            )
            .await
            .map_err(|_| internal(context, "upload verified audit"))?; }
        } else {
            let value = details(upload, stored.as_ref());
            let d = document(&value, profile.repository, context)?;
            if let (Some(job),Some(jobs)) = (job.as_ref(),jobs) {
                crate::job_upload_routes::verification_failed(&mut tx,&attempt,job,&d,jobs,context).await?;
            } else { Repository::new(&mut tx, profile.repository)
                .finish_upload(upload.id, "failed", None, stored.is_some())
                .await
                .map_err(|_| internal(context, "upload failed state"))?;
            let reason = "uploaded bytes do not match the declared size and SHA-256";
            Repository::new(&mut tx, profile.repository)
                .record_failure(RecordFailure {
                    project_id: attempt.project_id,
                    attempt: &attempt,
                    stage: "agent",
                    code: "upload_verification_failed",
                    reason,
                    details: &d,
                    log_refs: None,
                    from_state: None,
                })
                .await
                .map_err(|_| internal(context, "upload failure insert"))?;
            let prior = serde_json::json!({"state":attempt.state.as_str()});
            let new = serde_json::json!({"state":"failed","code":"upload_verification_failed"});
            crate::upload_audit::record(
                &mut tx,
                &attempt,
                crate::upload_audit::actor(&attempt, context)?,
                Record {
                    action: "attempt.failed",
                    subject_type: "attempt",
                    subject_id: &attempt.id.0.to_string(),
                    project_id: Some(attempt.project_id),
                    prior_state: Some(&prior),
                    new_state: Some(&new),
                    reason: Some(reason),
                    idempotency_key: None,
                },
                context,
            )
            .await
            .map_err(|_| internal(context, "upload failure audit"))?; }
            failed = Some(value);
        }
        tx.commit()
            .await
            .map_err(|_| internal(context, "upload settle commit"))?;
        settled = true;
        cancellation.settled = true;
        Ok::<_, Failure>(())
    }
    .await;
    if artifact.is_none() {
        if let Some(stored) = &stored {
            let deleted = profile
                .store
                .delete(&upload.key, stored.generation.as_deref())
                .await;
            if settled {
                if deleted.is_ok() {
                    let _ = Repository::new(conn, profile.repository)
                        .object_deleted(upload.id)
                        .await;
                }
            } else if let Err(error) = deleted {
                cancellation.armed = false;
                return Err(upload_direct::store_error(error, context));
            }
        }
        if !settled {
            let _ = Repository::new(conn, profile.repository)
                .abandon_receiving(upload.id)
                .await;
        }
    }
    cancellation.armed = false;
    result?;
    if !refused.is_empty() {
        return Err(failure(crate::errors::ApiError::from(DomainError::new(ErrorCode::InvalidContent,"output does not match its registered interface; the upload was refused, report the job's failure").with_details(serde_json::json!(refused)))));
    }
    if let Some(details) = failed {
        return Err(failure(crate::errors::ApiError::from(
            DomainError::new(
                ErrorCode::ValidationFailed,
                if jobs.is_some() {
                    "upload verification failed; the job failed"
                } else {
                    "upload verification failed; the attempt failed and awaits review"
                },
            )
            .with_details(details),
        )));
    }
    let artifact =
        artifact.ok_or_else(|| internal(context, "upload artifact response invariant"))?;
    let bytes = crate::attempt_read_wire::artifact(&artifact)
        .map_err(|_| internal(context, "upload artifact model"))?;
    Ok(response(StatusCode::CREATED, bytes))
}

/// Request cancellation jobs belong to the installed context and can be drained at shutdown.
pub struct CancellationTasks {
    tasks: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}
struct StoreSettlement {
    tasks: Arc<CancellationTasks>,
    timeout: std::time::Duration,
}
impl cannery_storage::CancellationSink for StoreSettlement {
    fn submit(&self, work: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>) {
        let timeout = self.timeout;
        self.tasks.track(tokio::spawn(async move {
            let _ = tokio::time::timeout(timeout, work).await;
        }));
    }
}
impl CancellationTasks {
    /// Select a timeout for each tracked official-SDK abort; drain after stopping requests.
    #[must_use]
    pub fn store_sink(
        self: &Arc<Self>,
        timeout: std::time::Duration,
    ) -> Arc<dyn cannery_storage::CancellationSink> {
        Arc::new(StoreSettlement {
            tasks: self.clone(),
            timeout,
        })
    }
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            tasks: std::sync::Mutex::new(vec![]),
        })
    }
    fn track(&self, task: tokio::task::JoinHandle<()>) {
        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.retain(|t| !t.is_finished());
            tasks.push(task);
        } else {
            task.abort();
        }
    }
    /// Drain exactly this context's queued settlement work before closing its pool.
    /// # Errors
    /// Reports poisoned tracking state or a failed cancellation task without payloads.
    pub async fn drain(&self) -> Result<(), &'static str> {
        let tasks = std::mem::take(
            &mut *self
                .tasks
                .lock()
                .map_err(|_| "upload cleanup tracking failed")?,
        );
        for task in tasks {
            task.await.map_err(|_| "upload cleanup task failed")?;
        }
        Ok(())
    }
}
struct ReceiveCleanup {
    pool: sqlx::PgPool,
    store: Arc<ObjectStore>,
    tasks: Arc<CancellationTasks>,
    id: UploadId,
    key: String,
    repository: JsonContext,
    timeout: std::time::Duration,
    stored: Option<StoredObject>,
    settled: bool,
    armed: bool,
}
impl Drop for ReceiveCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let pool = self.pool.clone();
        let store = self.store.clone();
        let id = self.id;
        let key = self.key.clone();
        let repository = self.repository;
        let timeout = self.timeout;
        let stored = self.stored.take();
        let settled = self.settled;
        let task = tokio::spawn(async move {
            let _ = tokio::time::timeout(timeout, async move {
                if let Some(stored) = stored {
                    let deleted = store.delete(&key, stored.generation.as_deref()).await;
                    if !settled && deleted.is_err() {
                        return;
                    }
                    if settled
                        && deleted.is_ok()
                        && let Ok(mut conn) = pool.acquire().await
                    {
                        let _ = Repository::new(&mut conn, repository)
                            .object_deleted(id)
                            .await;
                    }
                }
                if !settled && let Ok(mut conn) = pool.acquire().await {
                    let _ = Repository::new(&mut conn, repository)
                        .abandon_receiving(id)
                        .await;
                }
            })
            .await;
        });
        self.tasks.track(task);
    }
}
pub(crate) struct PlanCleanup {
    store: Arc<ObjectStore>,
    tasks: Arc<CancellationTasks>,
    key: String,
    id: Option<String>,
    timeout: std::time::Duration,
}
impl PlanCleanup {
    pub(crate) fn new(profile: &UploadContext, key: &str, id: Option<String>) -> Self {
        Self {
            store: profile.store.clone(),
            tasks: profile.cancellation.clone(),
            key: key.into(),
            id,
            timeout: profile.settlement_timeout,
        }
    }
    pub(crate) fn disarm(&mut self) {
        self.id = None;
    }
}
impl Drop for PlanCleanup {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        let store = self.store.clone();
        let key = self.key.clone();
        let timeout = self.timeout;
        self.tasks.track(tokio::spawn(async move {
            let _ = tokio::time::timeout(timeout, async move {
                if let Some(store) = store.presigning() {
                    let _ = store.abort_multipart(&key, &id).await;
                }
            })
            .await;
        }));
    }
}

#[utoipa::path(
    put,
    path = "/api/uploads/{upload_id}",
    operation_id = "put_upload_api_uploads__upload_id__put",
    summary = "Put Upload",
    description = "Receive the bytes of a granted upload. The grant's token is the credential.\n\nA grant sent in parts (``direct.transfer = \"multipart\"``) cannot be streamed.",
    params(("upload_id" = String, Path, format = "uuid"),
        ("X-Upload-Token" = Option<String>, Header)),
    responses((status = 201, description = "Successful Response", body = crate::api_models::ArtifactOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_put_upload(
    arg0: State<RouteState>,
    arg1: axum::Extension<RequestContext>,
    arg2: Request,
) -> Result<Response, Failure> {
    receive(arg0, arg1, arg2).await
}

#[utoipa::path(
    post,
    path = "/api/uploads/{upload_id}/presign",
    operation_id = "presign_upload_api_uploads__upload_id__presign_post",
    summary = "Presign Upload",
    description = "Fresh presigned URLs for a direct upload: its single PUT, or the given parts.\n\nEach is valid for the store's presign TTL, and never past the grant.",
    params(("upload_id" = String, Path, format = "uuid"),
        ("X-Upload-Token" = Option<String>, Header)),
    request_body(content = Option<crate::api_models::PresignRequest>, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = crate::api_models::PresignOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_presign_upload(
    arg0: State<RouteState>,
    arg1: axum::Extension<RequestContext>,
    arg2: Request,
) -> Result<Response, Failure> {
    presign(arg0, arg1, arg2).await
}

#[utoipa::path(
    post,
    path = "/api/uploads/{upload_id}/finish",
    operation_id = "finish_upload_api_uploads__upload_id__finish_post",
    summary = "Finish Upload",
    description = "Verify a direct upload's stored object and record the artifact.\n\nThe object must have the declared size and SHA-256, like a streamed\nupload; otherwise it is deleted and the attempt fails for review. While\nbytes are missing (no PUT yet, parts missing) the answer is ``conflict``\nand the grant stays open: send them and finish again.",
    params(("upload_id" = String, Path, format = "uuid"),
        ("X-Upload-Token" = Option<String>, Header)),
    responses((status = 201, description = "Successful Response", body = crate::api_models::ArtifactOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_finish_upload(
    arg0: State<RouteState>,
    arg1: axum::Extension<RequestContext>,
    arg2: Request,
) -> Result<Response, Failure> {
    finish(arg0, arg1, arg2).await
}

#[utoipa::path(
    put,
    path = "/api/job-uploads/{upload_id}",
    operation_id = "put_job_upload_api_job_uploads__upload_id__put",
    summary = "Put Job Upload",
    description = "Receive the bytes of a granted job output. The grant's token is the credential.\n\nBytes that do not match the declared size and SHA-256 fail the job. A\nfile uploaded against an interface is checked against it as it streams\nin: its size and leading bytes, and its content when it is JSON or JSON\nLines no larger than ``storage.validate_json_max_bytes`` (a larger one is\nrecorded as not content-validated). At most\n``storage.max_concurrent_validations`` uploads are content-validated at\nonce; the others wait (503 once they waited ``max_stream_seconds``). A\nfile that does not match is refused with ``invalid_content``, as soon as\na fatal problem shows, without reading the rest: nothing is kept, the\ngrant is spent and records the problems, and the job keeps its lease, so\nthe tester reports the job's failure, naming the step that wrote the\nfile. That record is what blames a producer's output on the agent. A\ngrant sent in parts (``direct.transfer = \"multipart\"``) cannot be streamed.",
    params(("upload_id" = String, Path, format = "uuid"),
        ("X-Upload-Token" = Option<String>, Header)),
    responses((status = 201, description = "Successful Response", body = crate::api_models::ArtifactOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_put_job_upload(
    arg0: State<RouteState>,
    arg1: axum::Extension<RequestContext>,
    arg2: Request,
) -> Result<Response, Failure> {
    receive(arg0, arg1, arg2).await
}

#[utoipa::path(
    post,
    path = "/api/job-uploads/{upload_id}/presign",
    operation_id = "presign_job_upload_api_job_uploads__upload_id__presign_post",
    summary = "Presign Job Upload",
    description = "Fresh presigned URLs for a direct job upload: its single PUT, or the given parts.\n\nEach is valid for the store's presign TTL, and never past the grant.",
    params(("upload_id" = String, Path, format = "uuid"),
        ("X-Upload-Token" = Option<String>, Header)),
    request_body(content = Option<crate::api_models::PresignRequest>, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = crate::api_models::PresignOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_presign_job_upload(
    arg0: State<RouteState>,
    arg1: axum::Extension<RequestContext>,
    arg2: Request,
) -> Result<Response, Failure> {
    presign(arg0, arg1, arg2).await
}

#[utoipa::path(
    post,
    path = "/api/job-uploads/{upload_id}/finish",
    operation_id = "finish_job_upload_api_job_uploads__upload_id__finish_post",
    summary = "Finish Job Upload",
    description = "Verify a direct job upload's stored object and record the artifact.\n\nThe object is checked exactly like a streamed one (see\n`PUT /api/job-uploads/{id}`): its size and SHA-256, and, for a file\nuploaded against an interface, its content, read back from the store.\nBytes that do not match are deleted. While bytes are missing (no PUT\nyet, parts missing) the answer is ``conflict`` and the grant stays open:\nsend them and finish again.",
    params(("upload_id" = String, Path, format = "uuid"),
        ("X-Upload-Token" = Option<String>, Header)),
    responses((status = 201, description = "Successful Response", body = crate::api_models::ArtifactOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_finish_job_upload(
    arg0: State<RouteState>,
    arg1: axum::Extension<RequestContext>,
    arg2: Request,
) -> Result<Response, Failure> {
    finish(arg0, arg1, arg2).await
}
