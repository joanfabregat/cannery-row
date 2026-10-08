//! Job claim/replay and heartbeat retain caller transactions and source lock order.
use crate::attempt_lease_routes::{Failure, domain, failure, internal};
use crate::{
    AppState,
    authentication::authenticate,
    body::{self, DecodedBody},
    errors::ApiError,
    job_claim_idempotency as replay, job_claim_request,
    request_context::first_header,
    requests::RequestContext,
    validation::{self, BodyInput},
};
use axum::{
    Router,
    extract::{FromRequestParts, Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use cannery_core::{
    audit::{self, Attribution, Record},
    errors::{DomainError, ErrorCode},
    ids::JobId,
    json::{self, DocumentBuilder, Node},
    principal::{Principal, Secret, ServiceKind, ServicePrincipal},
    timestamps::Timestamp,
};
use cannery_jobs::repo::{self, Job, Stage, State as JobState};
use cannery_projects::{authz, repo::Project};
use cannery_research::{
    job_documents::{self, JobDocument},
    science::RenderingContext,
};
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use sqlx::{Acquire, PgConnection};
use std::{collections::BTreeMap, sync::Arc};

/// Required entry profiles; no physical pool/preparation policy is inferred.
pub struct JobClaimContext {
    pub jobs: repo::JsonContext,
    pub attempts: cannery_attempts::model::JsonContext,
    pub rendering: RenderingContext,
    pub hash_budget: usize,
    pub response_budget: usize,
    pub mint: fn() -> Result<Secret, cannery_identity::error::IdentityError>,
}
#[derive(Clone)]
pub(crate) struct RouteState {
    app: AppState,
    profile: Arc<JobClaimContext>,
}
pub fn routes(app: AppState, profile: Arc<JobClaimContext>) -> Router {
    Router::new()
        .route(
            "/api/projects/{slug}/jobs/claims",
            post(claim)
                .get(claim_method)
                .head(claim_method)
                .fallback(claim_method),
        )
        .route(
            "/api/projects/{slug}/jobs/{job_id}/heartbeat",
            post(heartbeat),
        )
        .with_state(RouteState { app, profile })
}
// Starlette scans methods before specificity: GET "claims" reaches GET jobs/{job_id}.
// Its impossible UUID fails after authentication and before the read controller.
async fn claim_method(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    if request.method() != axum::http::Method::GET {
        return Ok((
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, "POST")],
            axum::Json(serde_json::json!({"detail":"Method Not Allowed"})),
        )
            .into_response());
    }
    let _auth = authenticate(&state.app, &context, request.headers(), request.method())
        .await
        .map_err(failure)?;
    match validation::job_uuid("claims") {
        Err(errors) => Err(checked_errors(&errors, &context)),
        Ok(_) => Err(internal(&context, "literal job path invariant")),
    }
}
async fn worker(
    c: &mut PgConnection,
    p: &Principal,
    slug: &str,
    context: &RequestContext,
) -> Result<Project, Failure> {
    authz::project_access(
        c,
        p,
        slug,
        None,
        &[ServiceKind::Tester, ServiceKind::Evaluator],
        true,
    )
    .await
    .map(|v| v.project)
    .map_err(|e| failure(context.project_error(e)))
}
fn service<'a>(
    principal: &'a Principal,
    context: &RequestContext,
) -> Result<&'a ServicePrincipal, Failure> {
    match principal {
        Principal::Service(p) => Ok(p),
        Principal::User(_) => Err(internal(context, "job worker invariant")),
    }
}
fn stage(p: &ServicePrincipal) -> Stage {
    if p.kind == ServiceKind::Tester {
        Stage::Tester
    } else {
        Stage::Evaluator
    }
}
fn violation(path: &str, message: &str) -> Failure {
    failure(ApiError::from(
        DomainError::new(ErrorCode::ValidationFailed, message)
            .with_details(serde_json::json!([{"path":path,"message":message}])),
    ))
}
fn checked_errors(e: &validation::ValidationErrors, context: &RequestContext) -> Failure {
    e.domain_error().map_or_else(
        |_| internal(context, "job request validation"),
        |e| failure(ApiError::from(e)),
    )
}
fn string(
    b: &mut DocumentBuilder,
    value: &str,
) -> Result<cannery_core::json::NodeId, cannery_core::json::BuildError> {
    b.push(Node::String(String::from(value)))
}
fn hash(
    slug: &str,
    stage: Stage,
    revision: Option<&str>,
    budget: usize,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let mut b = DocumentBuilder::new();
    let value = string(&mut b, stage.as_str())?;
    let mut fields = vec![(String::from("stage"), value)];
    if let Some(revision) = revision {
        let value = string(&mut b, revision)?;
        fields.push((String::from("revision"), value));
    }
    let root = b.push(Node::Object(fields))?;
    crate::hypothesis_idempotency::envelope_hash(slug, None, "claim", &b.finish(root)?, budget)
}
async fn claim_document(
    c: &mut PgConnection,
    slug: &str,
    job: &Job,
    token: &Secret,
    ttl: &BigInt,
    profile: &JobClaimContext,
    context: &RequestContext,
) -> Result<crate::api_models::JobClaimOut, Failure> {
    // A job runs under the brief its attempt pinned.
    let brief = crate::brief_routes::pinned(c, slug, job.attempt_id)
        .await
        .map_err(|error| failure(context.project_error(error)))?;
    let attempt = cannery_attempts::repo::Repository::new(c, profile.attempts)
        .get_attempt_by_id(job.attempt_id, false)
        .await
        .map_err(|_| internal(context, "job claim attempt"))?
        .ok_or_else(|| internal(context, "job claim attempt invariant"))?;
    let stage = String::from(job.stage.as_str());
    let science_revision = BigInt::from(job.science_revision);
    let generation = BigInt::from(job.lease_generation);
    let value = job_documents::job_document(
        &JobDocument {
            id: job.id,
            attempt_id: job.attempt_id,
            stage: &stage,
            science_revision: &science_revision,
            lease_generation: &generation,
            deadline: job.deadline,
            lease_expires_at: job.lease_expires_at,
            spec: &job.spec,
        },
        &String::from(token.expose()),
        profile.rendering,
    )
    .map_err(|_| internal(context, "job claim document"))?;
    Ok(crate::api_models::JobClaimOut {
        job: crate::api_contract::decode(
            &json::model::encode_model_mapping(&value, value.root(), profile.response_budget)
                .map_err(|_| internal(context, "job claim model"))?,
        )
        .map_err(|_| internal(context, "job claim model"))?,
        attempt_ref: format!("#{}.{}", attempt.hypothesis_number, attempt.sequence),
        heartbeat_seconds: std::cmp::max(BigInt::from(1), ttl / BigInt::from(3))
            .to_i64()
            .ok_or_else(|| internal(context, "job claim heartbeat"))?,
        brief,
    })
}
async fn audit_claim(
    c: &mut PgConnection,
    principal: &Principal,
    job: &Job,
    key: Option<&str>,
    old: Option<&BigInt>,
    context: &RequestContext,
) -> Result<(), Failure> {
    let prior = match old {
        Some(generation) => {
            serde_json::json!({"lease_generation":generation.to_string().parse::<i32>().map_err(|_|internal(context,"job replay generation invariant"))?})
        }
        None => serde_json::json!({"state":"pending"}),
    };
    let new = if old.is_some() {
        serde_json::json!({"lease_generation":job.lease_generation})
    } else {
        serde_json::json!({"state":"claimed","lease_generation":job.lease_generation,"attempt_id":job.attempt_id.to_string(),"run_number":job.run_number})
    };
    audit::record(
        c,
        Attribution::Principal(principal),
        Record {
            action: if old.is_some() {
                "job.lease_reissued"
            } else {
                "job.claimed"
            },
            subject_type: "job",
            subject_id: &job.id.to_string(),
            project_id: Some(job.project_id),
            prior_state: Some(&prior),
            new_state: Some(&new),
            reason: None,
            idempotency_key: key,
        },
    )
    .await
    .map_err(|_| internal(context, "job claim audit"))?;
    Ok(())
}
fn generation(value: &str, context: &RequestContext) -> Result<BigInt, Failure> {
    value
        .parse::<i32>()
        .map(BigInt::from)
        .map_err(|_| internal(context, "replay generation"))
}

#[cfg(test)]
mod replay_generation_tests {
    use super::{RequestContext, generation};
    use num_bigint::BigInt;

    #[test]
    fn stored_generation_accepts_checked_database_integers() {
        let context = RequestContext::background();
        for value in [i32::MIN, 0, 1, i32::MAX] {
            assert_eq!(
                generation(&value.to_string(), &context).ok(),
                Some(BigInt::from(value))
            );
        }
        for value in ["", "true", "1.0", " 1", "1\n", "2147483648", "-2147483649"] {
            assert!(generation(value, &context).is_err());
        }
    }
}
#[allow(
    clippy::too_many_lines,
    clippy::similar_names,
    reason = "Source replay, mutation and model construction phases retain transaction order"
)]
#[utoipa::path(
    post,
    path = "/api/projects/{slug}/jobs/claims",
    operation_id = "claim_job_api_projects__slug__jobs_claims_post",
    summary = "Claim Job",
    description = "Claim the oldest waiting job of the caller's stage registered to its name.\n\nA tester claims test jobs and an evaluator evaluation jobs, each only the\njobs its project's science revision registers under its account name. An\nevaluator names the policy revision it applies and receives only jobs\npinned to it, so evaluators of two revisions can run side by side.\n\nReplaying an ``Idempotency-Key`` while its claim still holds the lease\nreissues the lease token under the next lease generation (the first\nresponse may have been lost); the earlier token and every upload grant\nissued under it stop working.",
    params(("slug" = String, Path),
        ("idempotency-key" = Option<String>, Header)),
    request_body(content = crate::api_models::JobClaimRequest, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::JobClaimOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn claim(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = body::read_body(request).await.map_err(failure)?;
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(failure)?;
    let body = crate::api_contract::typed_body::<crate::api_models::JobClaimRequest>(body)
        .map_err(failure)?;
    let paths = Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &state)
        .await
        .map_err(failure)?
        .0;
    let input = match &body {
        DecodedBody::Missing => BodyInput::Missing,
        DecodedBody::RawBytes => BodyInput::RawBytes,
        DecodedBody::Json(d) => BodyInput::Json(d),
    };
    let body = job_claim_request::claim(input).map_err(|e| checked_errors(&e, &context))?;
    let project = worker(
        &mut auth.connection,
        &auth.principal,
        &paths["slug"],
        &context,
    )
    .await?;
    let tester = service(&auth.principal, &context)?;
    let stage = body.stage.unwrap_or_else(|| stage(tester));
    if stage != self::stage(tester) {
        return Err(domain(
            ErrorCode::Forbidden,
            format!(
                "a {} service account cannot claim {} jobs",
                self::stage(tester).as_str(),
                stage.as_str()
            ),
        ));
    }
    if stage == Stage::Evaluator && body.revision.is_none() {
        return Err(violation(
            "/revision",
            "an evaluation claim names the policy revision it applies",
        ));
    }
    if stage == Stage::Tester && body.revision.is_some() {
        return Err(violation(
            "/revision",
            "only an evaluation claim names a policy revision",
        ));
    }
    let key = first_header(&parts.headers, "idempotency-key");
    if key
        .as_ref()
        .is_some_and(|key| !(1..=200).contains(&key.chars().count()))
    {
        return Err(domain(
            ErrorCode::ValidationFailed,
            "Idempotency-Key must be 1 to 200 characters",
        ));
    }
    let hash = hash(
        &project.slug,
        stage,
        body.revision.as_deref(),
        state.profile.hash_budget,
    )
    .map_err(|_| internal(&context, "job claim hash"))?;
    let secret = (state.profile.mint)().map_err(|_| internal(&context, "job claim entropy"))?;
    let ttl = state.app.settings.leases.job_ttl_seconds.as_bigint();
    let actor = format!("service:{}", tester.service_account_id);
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "job claim transaction"))?;
    let result = async {
        if let Some(key) = key.as_deref()
            && let Some((remembered, result)) = replay::lookup(&mut tx, &actor, key)
                .await
                .map_err(|_| internal(&context, "job claim replay"))?
        {
            if remembered != hash {
                return Err(domain(
                    ErrorCode::Conflict,
                    "this Idempotency-Key was already used with a different request",
                ));
            }
            let (id, raw_generation) = result.split_once(':').unwrap_or((&result, ""));
            let id =
                JobId(uuid::Uuid::parse_str(id).map_err(|_| internal(&context, "job replay id"))?);
            let job = repo::get_job(&mut tx, id, true, state.profile.jobs)
                .await
                .map_err(|_| internal(&context, "job replay lookup"))?;
            let now: Timestamp = sqlx::query_scalar("SELECT now()")
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| internal(&context, "job replay clock"))?;
            let job = job.ok_or_else(|| {
                domain(
                    ErrorCode::Conflict,
                    "the job this Idempotency-Key claimed is no longer leased",
                )
            })?;
            if job.state != JobState::Claimed {
                return Err(domain(
                    ErrorCode::Conflict,
                    "the job this Idempotency-Key claimed is no longer leased",
                ));
            }
            let old = generation(raw_generation, &context)?;
            if old != BigInt::from(job.lease_generation)
                || job.claimed_by_service != Some(tester.service_account_id)
                || job.lease_expires_at.is_none_or(|time| time.0 <= now.0)
            {
                return Err(domain(
                    ErrorCode::Conflict,
                    "the job this Idempotency-Key claimed is no longer leased",
                ));
            }
            let job = repo::rotate_token(
                &mut tx,
                job.id,
                &cannery_identity::secrets::digest(secret.expose()),
                state.profile.jobs,
            )
            .await
            .map_err(|_| internal(&context, "job replay rotation"))?;
            replay::replace(
                &mut tx,
                &actor,
                key,
                &format!("{}:{}", job.id, job.lease_generation),
            )
            .await
            .map_err(|_| internal(&context, "job replay replacement"))?;
            audit_claim(
                &mut tx,
                &auth.principal,
                &job,
                Some(key),
                Some(&old),
                &context,
            )
            .await?;
            return claim_document(
                &mut tx,
                &project.slug,
                &job,
                &secret,
                ttl,
                &state.profile,
                &context,
            )
            .await
            .map(|d| (StatusCode::OK, d));
        }
        let id = repo::pick_pending(
            &mut tx,
            project.id,
            stage,
            &tester.name,
            body.revision.as_deref(),
        )
        .await
        .map_err(|_| internal(&context, "job claim selection"))?;
        let Some(id) = id else {
            let repr = cannery_core::text::repr_string(&String::from(&tester.name))
                .map_err(|_| internal(&context, "job worker representation"))?
                .as_utf8()
                .ok_or_else(|| internal(&context, "job worker representation"))?;
            let wanted = body
                .revision
                .as_ref()
                .map(|v| cannery_core::text::repr_string(&String::from(v)))
                .transpose()
                .map_err(|_| internal(&context, "job policy representation"))?
                .and_then(|v| v.as_utf8().map(|v| format!(" under policy revision {v}")))
                .unwrap_or_default();
            return Err(domain(
                ErrorCode::Conflict,
                format!(
                    "no {} job is waiting for {} {repr}{wanted}",
                    stage.as_str(),
                    stage.as_str()
                ),
            ));
        };
        let job = repo::claim_job(
            &mut tx,
            id,
            tester,
            &cannery_identity::secrets::digest(secret.expose()),
            ttl,
            state.profile.jobs,
        )
        .await
        .map_err(|_| internal(&context, "job claim mutation"))?;
        audit_claim(
            &mut tx,
            &auth.principal,
            &job,
            key.as_deref(),
            None,
            &context,
        )
        .await?;
        if let Some(key) = key.as_deref() {
            replay::remember(
                &mut tx,
                &actor,
                key,
                &hash,
                &format!("{}:{}", job.id, job.lease_generation),
            )
            .await
            .map_err(|_| internal(&context, "job claim remember"))?;
        }
        claim_document(
            &mut tx,
            &project.slug,
            &job,
            &secret,
            ttl,
            &state.profile,
            &context,
        )
        .await
        .map(|d| (StatusCode::CREATED, d))
    }
    .await;
    let (status, d) = match result {
        Ok(value) => {
            tx.commit()
                .await
                .map_err(|_| internal(&context, "job claim commit"))?;
            value
        }
        Err(error) => {
            if tx.rollback().await.is_err() {
                auth.connection.close_on_drop();
            }
            return Err(error);
        }
    };
    let bytes =
        serde_json::to_vec(&d).map_err(|_| internal(&context, "job claim serialization"))?;
    Ok((status, [(header::CONTENT_TYPE, "application/json")], bytes).into_response())
}
#[allow(clippy::too_many_lines)] // Keep dependency/lease/transaction failure order visible.
#[utoipa::path(
    post,
    path = "/api/projects/{slug}/jobs/{job_id}/heartbeat",
    operation_id = "heartbeat_api_projects__slug__jobs__job_id__heartbeat_post",
    summary = "Heartbeat",
    params(("slug" = String, Path),
        ("job_id" = String, Path, format = "uuid"),
        ("X-Lease-Token" = Option<String>, Header),
        ("X-Lease-Generation" = Option<i64>, Header)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::JobLeaseOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn heartbeat(
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
    let mut errors = vec![];
    let id = validation::job_uuid(&paths["job_id"])
        .map_err(|e| errors.extend(e.problems().iter().cloned()))
        .ok();
    let generation = first_header(&parts.headers, "x-lease-generation")
        .map(|value| validation::attempt_header_integer("X-Lease-Generation", &value))
        .transpose()
        .map_err(|e| errors.extend(e.problems().iter().cloned()))
        .ok()
        .flatten();
    if !errors.is_empty() {
        return Err(checked_errors(
            &validation::ValidationErrors::from_problems(errors),
            &context,
        ));
    }
    let id = JobId(id.ok_or_else(|| internal(&context, "job path invariant"))?);
    let project = worker(
        &mut auth.connection,
        &auth.principal,
        &paths["slug"],
        &context,
    )
    .await?;
    let tester = service(&auth.principal, &context)?;
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "job heartbeat transaction"))?;
    let result = async {
        let job = repo::get_job(&mut tx, id, true, state.profile.jobs)
            .await
            .map_err(|_| internal(&context, "leased job lookup"))?
            .filter(|job| job.project_id == project.id)
            .ok_or_else(|| domain(ErrorCode::NotFound, "job not found"))?;
        if job.stage != stage(tester) {
            return Err(domain(
                ErrorCode::Forbidden,
                format!(
                    "a {} service account cannot work on {} jobs",
                    stage(tester).as_str(),
                    job.stage.as_str()
                ),
            ));
        }
        if job.claimed_by_service != Some(tester.service_account_id) {
            return Err(domain(
                ErrorCode::Forbidden,
                format!(
                    "only the {} that claimed this job can work on it",
                    job.stage.as_str()
                ),
            ));
        }
        let token = first_header(&parts.headers, "x-lease-token");
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
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| internal(&context, "job lease clock"))?;
        if job.deadline.is_none_or(|time| time.0 <= now.0) {
            return Err(domain(ErrorCode::StaleLease, "the job's deadline passed"));
        }
        if job.lease_expires_at.is_none_or(|time| time.0 <= now.0) {
            return Err(domain(ErrorCode::StaleLease, "the lease expired"));
        }
        let attempt = cannery_attempts::repo::Repository::new(&mut tx, state.profile.attempts)
            .get_attempt_by_id(job.attempt_id, false)
            .await
            .map_err(|_| internal(&context, "job heartbeat attempt"))?
            .ok_or_else(|| internal(&context, "job heartbeat attempt invariant"))?;
        let expected = if job.stage == Stage::Tester {
            cannery_attempts::model::State::Testing
        } else {
            cannery_attempts::model::State::Evaluating
        };
        if attempt.state != expected {
            return Err(domain(
                ErrorCode::StaleLease,
                format!(
                    "the attempt is no longer in the {} stage ({})",
                    job.stage.as_str(),
                    attempt.state.as_str()
                ),
            ));
        }
        repo::extend_lease(
            &mut tx,
            job.id,
            state.app.settings.leases.job_ttl_seconds.as_bigint(),
            state.profile.jobs,
        )
        .await
        .map_err(|_| internal(&context, "job heartbeat extension"))
    }
    .await;
    let renewed = match result {
        Ok(value) => {
            tx.commit()
                .await
                .map_err(|_| internal(&context, "job heartbeat commit"))?;
            value
        }
        Err(error) => {
            if tx.rollback().await.is_err() {
                auth.connection.close_on_drop();
            }
            return Err(error);
        }
    };
    let expires = renewed
        .lease_expires_at
        .ok_or_else(|| internal(&context, "job heartbeat expiry invariant"))?;
    let deadline = renewed
        .deadline
        .ok_or_else(|| internal(&context, "job heartbeat deadline invariant"))?;
    let bytes = serde_json::to_vec(&crate::api_models::JobLeaseOut {
        lease_generation: i64::from(renewed.lease_generation),
        lease_expires_at: crate::timestamps::public_timestamp(expires),
        deadline: crate::timestamps::public_timestamp(deadline),
    })
    .map_err(|_| internal(&context, "job heartbeat serialization"))?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response())
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
