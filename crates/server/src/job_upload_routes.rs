//! Job upload grants use worker leases; transfers use only their upload capability.
#![allow(
    clippy::many_single_char_names,
    reason = "Explicit borrowed transaction and capability contexts"
)]
use crate::{
    AppState,
    attempt_lease_routes::{Failure, domain, failure, internal},
    authentication::authenticate,
    job_completion_routes::{self as jobs_http, JobLifecycleContext},
    job_lifecycle as flow,
    request_context::first_header,
    requests::RequestContext,
    upload_direct,
    upload_routes::{self, RouteState, UploadContext},
    validation,
};
use axum::{
    Router,
    extract::{FromRequestParts, Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use cannery_attempts::{
    model::{Attempt, Transfer, Upload},
    repo::{CreateUpload, Repository},
};
use cannery_core::{
    audit::{Actor, Attribution},
    errors::ErrorCode,
    ids::JobId,
    json::Document,
    principal::Via,
    timestamps::Timestamp,
};
use cannery_jobs::repo::{self as jobs, Job};
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use serde_json::{Value, json};
use sqlx::{Acquire, PgConnection};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

pub struct JobUploadContext {
    pub lifecycle: Arc<JobLifecycleContext>,
    pub uploads: Arc<UploadContext>,
    pub json_max_bytes: usize,
    pub validation_slots: Arc<tokio::sync::Semaphore>,
    pub validation_timeout: Duration,
}
pub fn routes(app: AppState, context: Arc<JobUploadContext>) -> Router {
    upload_routes::job_routes(app, context)
}
pub(crate) async fn locked_job(
    c: &mut PgConnection,
    u: &Upload,
    s: Option<&JobUploadContext>,
    r: &RequestContext,
) -> Result<Option<Job>, Failure> {
    match (u.job_id, s) {
        (None, None) => Ok(None),
        (Some(id), Some(s)) => jobs::get_job(c, id, true, s.lifecycle.flow.jobs)
            .await
            .map_err(|_| internal(r, "upload job lock"))?
            .filter(|j| j.attempt_id == u.attempt_id)
            .map(Some)
            .ok_or_else(|| internal(r, "upload job identity")),
        _ => Err(internal(r, "upload route capability kind")),
    }
}
pub(crate) fn live(a: &Attempt, j: Option<&Job>, u: &Upload, now: Timestamp) -> bool {
    j.map_or_else(
        || {
            matches!(
                a.state,
                cannery_attempts::model::State::Claimed | cannery_attempts::model::State::Running
            ) && a.lease_generation == u.lease_generation
                && a.lease_expires_at.is_some_and(|v| v.0 > now.0)
        },
        |j| {
            a.state == cannery_attempts::model::State::Verifying
                && j.state == jobs::State::Claimed
                && j.lease_generation == u.lease_generation
                && j.lease_expires_at.is_some_and(|v| v.0 > now.0)
                && j.deadline.is_some_and(|v| v.0 > now.0)
        },
    )
}
fn actor(j: &Job, r: &RequestContext) -> Result<Actor, Failure> {
    let channel = serde_json::from_value(json!(j.via_channel.as_deref().unwrap_or("api")))
        .map_err(|_| internal(r, "job capability channel"))?;
    let via = Via {
        channel,
        client: j.via_client.clone(),
    };
    match j.claimant() {
        Some(jobs::Claimant::Service(id)) => Ok(Actor::Service { id, via }),
        Some(jobs::Claimant::User(id)) => Ok(Actor::User { id, via }),
        None => Err(internal(r, "job capability actor")),
    }
}
#[allow(
    clippy::too_many_arguments,
    reason = "Capability audit must retain stored job attribution"
)]
pub(crate) async fn event(
    c: &mut PgConnection,
    a: &Attempt,
    j: &Job,
    action: &str,
    subject: &str,
    id: &str,
    new: &Value,
    reason: Option<&str>,
    r: &RequestContext,
) -> Result<(), Failure> {
    let actor = actor(j, r)?;
    flow::event_as(
        c,
        Attribution::Capability(&actor),
        a,
        action,
        subject,
        id,
        None,
        new,
        reason,
        r,
    )
    .await
}
pub(crate) async fn verification_failed(
    c: &mut PgConnection,
    a: &Attempt,
    j: &Job,
    details: &Document,
    s: &JobUploadContext,
    r: &RequestContext,
) -> Result<(), Failure> {
    let actor = actor(j, r)?;
    let logs = flow::document(&json!([]), &s.lifecycle.flow, r)?;
    let result = flow::fail_job_run_as(
        c,
        Attribution::Capability(&actor),
        a,
        j,
        jobs::Failure {
            step: None,
            code: "upload_verification_failed",
            reason: "uploaded bytes do not match the declared size and SHA-256",
            logs: &logs,
        },
        details,
        false,
        &s.lifecycle.flow,
        r,
    )
    .await;
    result.map(|_| ())
}
fn validation_error(e: &validation::ValidationErrors, r: &RequestContext) -> Failure {
    e.domain_error().map_or_else(
        |_| internal(r, "job upload validation"),
        |e| failure(crate::errors::ApiError::from(e)),
    )
}
#[allow(
    clippy::too_many_lines,
    reason = "Grant planning, lease locks and multipart cleanup remain explicit"
)]
#[utoipa::path(
    post,
    path = "/api/projects/{slug}/jobs/{job_id}/uploads",
    operation_id = "create_job_upload_api_projects__slug__jobs__job_id__uploads_post",
    summary = "Create Job Upload",
    description = "Grant a one-time, create-only upload under the job's output prefix.\n\nAn ``interface`` must be registered in the job's science revision. The\nbytes are checked against it when they stream in (or, sent directly to\nthe object store, when the upload finishes), never on the declared size\nalone: only bytes the API received or read back are evidence against a\nstep.",
    params(("slug" = String, Path),
        ("job_id" = String, Path, format = "uuid"),
        ("X-Lease-Token" = Option<String>, Header),
        ("X-Lease-Generation" = Option<i64>, Header)),
    request_body(content = crate::api_models::JobUploadRequest, content_type = "application/json"),
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
    axum::Extension(r): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let s = state
        .jobs
        .as_ref()
        .ok_or_else(|| internal(&r, "job upload installation"))?;
    let (mut parts, body) = crate::body::read_body(request).await.map_err(failure)?;
    let mut auth = authenticate(&state.app, &r, &parts.headers, &parts.method)
        .await
        .map_err(failure)?;
    let body = crate::api_contract::typed_body::<crate::api_models::JobUploadRequest>(body)
        .map_err(failure)?;
    let paths = Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &state)
        .await
        .map_err(failure)?
        .0;
    let mut errors = vec![];
    let id = validation::job_uuid(&paths["job_id"])
        .map_err(|e| errors.extend(e.problems().iter().cloned()))
        .ok();
    let generation = first_header(&parts.headers, "x-lease-generation")
        .map(|v| validation::attempt_header_integer("X-Lease-Generation", &v))
        .transpose()
        .map_err(|e| errors.extend(e.problems().iter().cloned()))
        .ok()
        .flatten();
    let input = match &body {
        crate::body::DecodedBody::Json(d) => validation::BodyInput::Json(d),
        crate::body::DecodedBody::Missing => validation::BodyInput::Missing,
        crate::body::DecodedBody::RawBytes => validation::BodyInput::RawBytes,
    };
    let body = validation::validate_job_upload(input)
        .map_err(|e| errors.extend(e.problems().iter().cloned()))
        .ok();
    if !errors.is_empty() {
        return Err(validation_error(
            &validation::ValidationErrors::from_problems(errors),
            &r,
        ));
    }
    let body = body.ok_or_else(|| internal(&r, "job upload body"))?;
    let id = JobId(id.ok_or_else(|| internal(&r, "job upload identity"))?);
    let project =
        crate::job_workers::worker(&mut auth.connection, &auth.principal, &paths["slug"], &r)
            .await?;
    if &body.size_bytes > state.app.settings.storage.max_object_bytes.as_bigint() {
        return Err(domain(
            ErrorCode::ValidationFailed,
            "object exceeds the configured maximum size",
        ));
    }
    let secret = (state.profile.mint)().map_err(|_| internal(&r, "job upload grant secret"))?;
    let found = jobs::get_job(&mut auth.connection, id, false, s.lifecycle.flow.jobs)
        .await
        .map_err(|_| internal(&r, "job upload prefix lookup"))?
        .filter(|job| job.project_id == project.id)
        .ok_or_else(|| domain(ErrorCode::NotFound, "job not found"))?;
    let spec = flow::value(&found.spec, &s.lifecycle.flow, &r)?;
    let prefix = spec["output_prefix"]
        .as_str()
        .ok_or_else(|| internal(&r, "job upload prefix"))?;
    let slot = format!("{prefix}{}", body.path);
    let store = &state.profile.store;
    let bucket = store.presigning();
    let (key, plan) = if let Some(bucket) = bucket {
        let nonce = (state.profile.nonce)().map_err(|_| internal(&r, "job upload object nonce"))?;
        let (head, name) = slot
            .rsplit_once('/')
            .ok_or_else(|| internal(&r, "job upload path"))?;
        let key = format!("{head}/{nonce}/{name}");
        let plan =
            upload_direct::plan(bucket, &key, &body.size_bytes, &body.media_type, &r).await?;
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
    let mut cleanup = upload_routes::PlanCleanup::new(&state.profile, &key, plan.id.clone());
    let token = first_header(&parts.headers, "x-lease-token");
    let result = async {
        let mut tx = auth
            .connection
            .begin()
            .await
            .map_err(|_| internal(&r, "job upload transaction"))?;
        let attempt =
            jobs_http::locked_attempt(&mut tx, id, project.id, &s.lifecycle.flow, &r).await?;
        let job = jobs_http::leased(
            &mut tx,
            &auth.principal,
            id,
            project.id,
            token.as_deref(),
            generation.as_ref(),
            &s.lifecycle.flow,
            &r,
        )
        .await?;
        jobs_http::require_verifying(&attempt)?;
        if let Some(reference) = &body.interface {
            crate::job_output_interface::load(&mut tx, &attempt, reference, &s.lifecycle, &r)
                .await?;
        }
        let limit = spec["limits"]["max_output_bytes"]
            .as_u64()
            .ok_or_else(|| internal(&r, "job output limit"))?;
        let used = jobs::committed_output_bytes(&mut tx, job.id)
            .await
            .map_err(|_| internal(&r, "job output byte count"))?;
        if used + &body.size_bytes > BigInt::from(limit) {
            return Err(domain(
                ErrorCode::ValidationFailed,
                format!("a job's outputs are limited to {limit} bytes in total"),
            ));
        }
        let upload = Repository::new(&mut tx, state.profile.repository)
            .create_upload(CreateUpload {
                attempt_id: attempt.id,
                lease_generation: &BigInt::from(job.lease_generation),
                token_hash: &cannery_identity::secrets::digest(secret.expose()),
                role: &body.role,
                backend: store.backend(),
                bucket: store.bucket(),
                key: &key,
                declared_size: &body.size_bytes,
                declared_sha256: &body.sha256,
                media_type: &body.media_type,
                ttl_minutes: state.app.settings.storage.upload_ttl_minutes.as_bigint(),
                max_stream_seconds: state.app.settings.storage.max_stream_seconds.as_bigint(),
                job_id: Some(job.id),
                interface: body.interface.as_deref(),
                transfer: plan.transfer,
                multipart_upload_id: plan.id.as_deref(),
                part_size: plan.part_size.as_ref(),
                slot: Some(&slot),
            })
            .await
            .map_err(|_| internal(&r, "job upload insert"))?
            .ok_or_else(|| {
                domain(
                    ErrorCode::Conflict,
                    format!("{} was already granted or uploaded", body.path),
                )
            })?;
        if Repository::new(&mut tx, state.profile.repository)
            .count_open_uploads(attempt.id, Some(job.id))
            .await
            .map_err(|_| internal(&r, "job open grant count"))?
            > 64
        {
            return Err(domain(
                ErrorCode::Conflict,
                "a job holds at most 64 open upload grants",
            ));
        }
        let base = format!(
            "{}/api/job-uploads/{}",
            state
                .app
                .settings
                .server
                .public_base_url
                .trim_end_matches('/'),
            upload.id.0
        );
        let direct = if let Some(bucket) = bucket {
            let now: Timestamp = sqlx::query_scalar("SELECT now()")
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| internal(&r, "job grant clock"))?;
            let lifetime = upload_direct::lifetime(bucket, &upload, now, &r)?;
            let signed = upload_direct::presign(
                bucket,
                &upload,
                None,
                &lifetime,
                now,
                (state.profile.signing_clock)(),
                &r,
            )
            .await?;
            Repository::new(&mut tx, state.profile.repository)
                .record_urls(upload.id, &lifetime)
                .await
                .map_err(|_| internal(&r, "job grant URL fencing"))?;
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
                    .map(|size| {
                        cannery_storage::s3::part_count(
                            &BigInt::from(upload.declared_size),
                            &BigInt::from(size),
                        )
                    })
                    .transpose()
                    .map_err(|_| internal(&r, "job grant part count"))?
                    .map(|n| {
                        n.to_i64()
                            .ok_or_else(|| internal(&r, "job grant count range"))
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
            .map_err(|_| internal(&r, "job grant commit"))?;
        Ok::<_, Failure>((upload, base, direct))
    }
    .await;
    let (upload, base, direct) = match result {
        Ok(v) => v,
        Err(e) => {
            upload_direct::discard(bucket, &key, &plan).await;
            cleanup.disarm();
            return Err(e);
        }
    };
    cleanup.disarm();
    Ok((
        StatusCode::CREATED,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_vec(&crate::api_models::UploadGrant {
            upload_url: base,
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
                .map_err(|_| internal(&r, "job direct upload contract"))?,
        })
        .map_err(|_| internal(&r, "job upload response"))?,
    )
        .into_response())
}
