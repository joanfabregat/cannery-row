//! Job claim/replay and heartbeat retain caller transactions and source lock order.
use crate::attempt_lease_routes::{Failure, domain, failure, internal};
use crate::{
    AppState,
    authentication::authenticate,
    body::{self, DecodedBody},
    errors::ApiError,
    job_claim_idempotency as replay, job_claim_request, job_workers,
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
    principal::{Principal, Secret, ServiceKind},
    timestamps::Timestamp,
};
use cannery_jobs::repo::{self, Claimant, Job, Performer, Phase, State as JobState};
use cannery_projects::repo::Project;
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
    phase: Phase,
    revision: Option<&str>,
    budget: usize,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let mut b = DocumentBuilder::new();
    let value = string(&mut b, phase.as_str())?;
    let mut fields = vec![(String::from("phase"), value)];
    if let Some(revision) = revision {
        let value = string(&mut b, revision)?;
        fields.push((String::from("revision"), value));
    }
    let root = b.push(Node::Object(fields))?;
    crate::unit_idempotency::envelope_hash(slug, None, "claim", &b.finish(root)?, budget)
}
async fn claim_document(
    c: &mut PgConnection,
    project: &Project,
    job: &Job,
    token: &Secret,
    ttl: &BigInt,
    profile: &JobClaimContext,
    context: &RequestContext,
) -> Result<crate::api_models::JobClaimOut, Failure> {
    // A job runs under the brief and plan its attempt pinned.
    let pins = crate::context_bundle::pins(c, project, job.attempt_id, context)
        .await
        .map_err(failure)?;
    let attempt = cannery_attempts::repo::Repository::new(c, profile.attempts)
        .get_attempt_by_id(job.attempt_id, false)
        .await
        .map_err(|_| internal(context, "job claim attempt"))?
        .ok_or_else(|| internal(context, "job claim attempt invariant"))?;
    let heartbeat_seconds = std::cmp::max(BigInt::from(1), ttl / BigInt::from(3))
        .to_i64()
        .ok_or_else(|| internal(context, "job claim heartbeat"))?;
    if job.phase == Phase::Decide {
        return decide_job(
            c,
            project,
            job,
            &attempt,
            token,
            pins,
            heartbeat_seconds,
            profile,
            context,
        )
        .await;
    }
    if job.phase == Phase::Document {
        return document_job(
            c,
            project,
            job,
            &attempt,
            token,
            pins,
            heartbeat_seconds,
            context,
        )
        .await;
    }
    let phase = String::from(job.phase.as_str());
    let science_revision = BigInt::from(job.science_revision);
    let generation = BigInt::from(job.lease_generation);
    let value = job_documents::job_document(
        &JobDocument {
            id: job.id,
            attempt_id: job.attempt_id,
            phase: &phase,
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
        attempt_ref: format!("#{}.{}", attempt.unit_number, attempt.sequence),
        heartbeat_seconds,
        brief: pins.brief,
        plan: pins.plan,
        context: pins.context,
    })
}
/// A claimed document job: what its write-up covers and cites, and the
/// documenter's context bundle.
#[allow(
    clippy::too_many_arguments,
    reason = "Claim response inputs stay explicit"
)]
async fn document_job(
    c: &mut PgConnection,
    project: &Project,
    job: &Job,
    attempt: &cannery_attempts::model::Attempt,
    token: &Secret,
    mut pins: crate::context_bundle::Pins,
    heartbeat_seconds: i64,
    context: &RequestContext,
) -> Result<crate::api_models::JobClaimOut, Failure> {
    let inputs = crate::document_jobs::inputs(c, attempt, context).await?;
    let (deadline, expires) = job
        .deadline
        .zip(job.lease_expires_at)
        .ok_or_else(|| internal(context, "document job lease invariant"))?;
    let text = |name: &str| {
        let spec = &job.spec;
        spec.field(spec.root(), name)
            .and_then(|id| spec.node(id))
            .and_then(|node| match node {
                Node::String(value) => value.as_utf8(),
                _ => None,
            })
            .ok_or_else(|| internal(context, "document job specification"))
    };
    if let Some(bundle) = pins.context.as_mut() {
        bundle.r#ref.push_str("?phase=document");
        let pinned = cannery_tracks::plans::attempt_pins_by_id(c, attempt.id)
            .await
            .map_err(|_| internal(context, "document job pins"))?
            .ok_or_else(|| internal(context, "document job pins invariant"))?;
        let built = crate::context_bundle::build_for(
            c,
            project,
            &pinned,
            crate::context_bundle::Detail::Document,
            context,
        )
        .await
        .map_err(failure)?;
        bundle.bytes = i64::try_from(built.len()).unwrap_or(i64::MAX);
    }
    Ok(crate::api_models::JobClaimOut {
        job: crate::api_models::ClaimedJobDocument::Document(Box::new(
            crate::api_models::ClaimedDocumentJob {
                schema_version: crate::api_models::RequestCommonSchemaVersion::Value0,
                job_id: job.id.to_string(),
                phase: crate::api_models::ClaimedDocumentPhase::Document,
                attempt_id: job.attempt_id.to_string(),
                performer: crate::api_models::ClaimedDocumentPerformer::Agent,
                track: text("track")?,
                unit: i64::from(attempt.unit_number),
                science_revision: job.science_revision.to_string(),
                inputs: crate::api_models::ClaimedDocumentInputs {
                    attempts: inputs.attempts.clone(),
                    verification: inputs.verification.as_ref().map(|cited| {
                        crate::api_models::RequestCommonContentRef {
                            r#ref: cited.id.to_string(),
                            sha256: cited.sha256.clone(),
                        }
                    }),
                },
                output_prefix: text("output_prefix")?,
                deadline: deadline.isoformat(),
                lease: crate::api_models::ClaimedJobLease {
                    token: String::from(token.expose()),
                    generation: i64::from(job.lease_generation),
                    expires_at: expires.isoformat(),
                },
            },
        )),
        attempt_ref: format!("#{}.{}", attempt.unit_number, attempt.sequence),
        heartbeat_seconds,
        brief: pins.brief,
        plan: pins.plan,
        context: pins.context,
    })
}
/// A claimed decide job: the decision case, what its decision document
/// cites, and the decider's context bundle, whether or not the attempt
/// pinned a plan.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "Claim response inputs stay explicit"
)]
async fn decide_job(
    c: &mut PgConnection,
    project: &Project,
    job: &Job,
    attempt: &cannery_attempts::model::Attempt,
    token: &Secret,
    mut pins: crate::context_bundle::Pins,
    heartbeat_seconds: i64,
    profile: &JobClaimContext,
    context: &RequestContext,
) -> Result<crate::api_models::JobClaimOut, Failure> {
    let (deadline, expires) = job
        .deadline
        .zip(job.lease_expires_at)
        .ok_or_else(|| internal(context, "decide job lease invariant"))?;
    let spec = cannery_core::json::to_value(&job.spec)
        .map_err(|_| internal(context, "decide job specification"))?;
    let text = |value: &serde_json::Value| {
        value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| internal(context, "decide job specification"))
    };
    let case_id = text(&spec["review_case_id"])?;
    let case = cannery_reviews::repo::get_case(
        c,
        project.id,
        cannery_core::ids::ReviewCaseId(
            uuid::Uuid::parse_str(&case_id).map_err(|_| internal(context, "decide job case"))?,
        ),
        false,
    )
    .await
    .map_err(|_| internal(context, "decide job case"))?
    .ok_or_else(|| internal(context, "decide job case invariant"))?;
    // The verification report and the write-up the decision cites.
    let mut cited = Vec::new();
    for id in [case.evidence_id, case.writeup_id] {
        let Some(id) = id.map(|id| cannery_attempts::model::EvidenceId(id.0)) else {
            cited.push(None);
            continue;
        };
        let (_, sha256) = cannery_attempts::repo::Repository::new(&mut *c, profile.attempts)
            .get_evidence_by_id(attempt.id, id)
            .await
            .map_err(|_| internal(context, "decide job cited output"))?
            .ok_or_else(|| internal(context, "decide job cited output invariant"))?;
        cited.push(Some(crate::api_models::RequestCommonContentRef {
            r#ref: id.0.to_string(),
            sha256,
        }));
    }
    let writeup = cited.pop().flatten();
    let verification = cited.pop().flatten();
    let pinned = cannery_tracks::plans::attempt_pins_by_id(c, attempt.id)
        .await
        .map_err(|_| internal(context, "decide job pins"))?
        .ok_or_else(|| internal(context, "decide job pins invariant"))?;
    let built = crate::context_bundle::build_for(
        c,
        project,
        &pinned,
        crate::context_bundle::Detail::Decide,
        context,
    )
    .await
    .map_err(failure)?;
    pins.context = Some(crate::api_models::ContextBundleRef {
        r#ref: format!(
            "{}?phase=decide",
            crate::context_bundle::bundle_path(
                &project.slug,
                attempt.unit_number,
                attempt.sequence
            )
        ),
        bytes: i64::try_from(built.len()).unwrap_or(i64::MAX),
    });
    Ok(crate::api_models::JobClaimOut {
        job: crate::api_models::ClaimedJobDocument::Decide(Box::new(
            crate::api_models::ClaimedDecideJob {
                schema_version: crate::api_models::RequestCommonSchemaVersion::Value0,
                job_id: job.id.to_string(),
                phase: crate::api_models::ClaimedDecidePhase::Decide,
                attempt_id: job.attempt_id.to_string(),
                performer: crate::api_models::ClaimedDecidePerformer::Runner,
                decider: crate::api_models::ClaimedJobPinnedRef {
                    id: text(&spec["decider"]["id"])?,
                    revision: text(&spec["decider"]["revision"])?,
                },
                track: text(&spec["track"])?,
                unit: i64::from(attempt.unit_number),
                science_revision: job.science_revision.to_string(),
                review_case_id: case_id,
                inputs: crate::api_models::ClaimedDecideInputs {
                    verification,
                    writeup,
                },
                output_prefix: text(&spec["output_prefix"])?,
                deadline: deadline.isoformat(),
                limits: crate::api_models::ClaimedDecideLimits {
                    max_output_bytes: spec["limits"]["max_output_bytes"]
                        .as_i64()
                        .ok_or_else(|| internal(context, "decide job limits"))?,
                },
                lease: crate::api_models::ClaimedJobLease {
                    token: String::from(token.expose()),
                    generation: i64::from(job.lease_generation),
                    expires_at: expires.isoformat(),
                },
            },
        )),
        attempt_ref: format!("#{}.{}", attempt.unit_number, attempt.sequence),
        heartbeat_seconds,
        brief: pins.brief,
        plan: pins.plan,
        context: pins.context,
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
    description = "Claim the oldest waiting job of the phase (`verify` by default) the caller\nmay perform.\n\nA verifier service account names the policy revision it applies and claims\nonly the runner jobs its project's science revision registers under its\naccount name and that revision. An agent service account or a researcher\nnames no revision and claims agent jobs, never one of an attempt it ran\nitself.\n\nWith `phase: document`, an agent service account or a researcher claims\nthe oldest waiting document job: it writes up a unit whose last\nattempt was verified, or that a researcher stopped after a failure.\n\nWith `phase: decide`, the decider service account a science revision\nregisters names the revision of its step and claims the oldest waiting\ndecide job registered to it: it decides a written-up unit on its\ndecision case.\n\nReplaying an ``Idempotency-Key`` while its claim still holds the lease\nreissues the lease token under the next lease generation (the first\nresponse may have been lost); the earlier token and every upload grant\nissued under it stop working.",
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
    let project = job_workers::worker(
        &mut auth.connection,
        &auth.principal,
        &paths["slug"],
        &context,
    )
    .await?;
    let phase = body.phase.unwrap_or(Phase::Verify);
    let performer = job_workers::performer(&auth.principal);
    let decider =
        matches!(&auth.principal, Principal::Service(s) if s.kind == ServiceKind::Decider);
    if decider && phase != Phase::Decide {
        return Err(violation("/phase", "a decider claims decide jobs"));
    }
    if !decider && phase == Phase::Decide {
        return Err(violation(
            "/phase",
            "only the decider service account a science revision registers claims decide jobs",
        ));
    }
    if performer == Performer::Runner && phase == Phase::Document {
        return Err(violation(
            "/phase",
            "a verifier claims verify jobs; an agent or a researcher writes units up",
        ));
    }
    if decider && body.revision.is_none() {
        return Err(violation(
            "/revision",
            "a decider names the revision of its step",
        ));
    }
    if performer == Performer::Runner && body.revision.is_none() {
        return Err(violation(
            "/revision",
            "a verifier names the policy revision it applies",
        ));
    }
    if performer == Performer::Agent && body.revision.is_some() {
        return Err(violation(
            "/revision",
            "only a verifier names a policy revision",
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
        phase,
        body.revision.as_deref(),
        state.profile.hash_budget,
    )
    .map_err(|_| internal(&context, "job claim hash"))?;
    let secret = (state.profile.mint)().map_err(|_| internal(&context, "job claim entropy"))?;
    let ttl = state.app.settings.leases.job_ttl_seconds.as_bigint();
    let actor = job_workers::actor(&auth.principal);
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
                || job.claimant() != Some(Claimant::of(&auth.principal))
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
                &project,
                &job,
                &secret,
                ttl,
                &state.profile,
                &context,
            )
            .await
            .map(|d| (StatusCode::OK, d));
        }
        let claimant = Claimant::of(&auth.principal);
        let id = match (&auth.principal, body.revision.as_deref()) {
            (Principal::Service(decider), Some(revision)) if phase == Phase::Decide => {
                repo::pick_pending_decider(&mut tx, project.id, &decider.name, revision).await
            }
            (Principal::Service(verifier), Some(revision)) if performer == Performer::Runner => {
                repo::pick_pending_runner(&mut tx, project.id, &verifier.name, revision).await
            }
            _ => repo::pick_pending_agent(&mut tx, project.id, claimant, phase).await,
        }
        .map_err(|_| internal(&context, "job claim selection"))?;
        let Some(id) = id else {
            if let (Principal::Service(decider), Some(revision)) =
                (&auth.principal, body.revision.as_deref())
                && phase == Phase::Decide
            {
                let repr = |v: &str| {
                    cannery_core::text::repr_string(&String::from(v))
                        .ok()
                        .and_then(|v| v.as_utf8())
                        .ok_or_else(|| internal(&context, "job worker representation"))
                };
                return Err(domain(
                    ErrorCode::Conflict,
                    format!(
                        "no decide job is waiting for decider {} under step revision {}",
                        repr(&decider.name)?,
                        repr(revision)?
                    ),
                ));
            }
            if let (Principal::Service(verifier), Some(revision)) =
                (&auth.principal, body.revision.as_deref())
                && performer == Performer::Runner
            {
                let repr = |v: &str| {
                    cannery_core::text::repr_string(&String::from(v))
                        .ok()
                        .and_then(|v| v.as_utf8())
                        .ok_or_else(|| internal(&context, "job worker representation"))
                };
                return Err(domain(
                    ErrorCode::Conflict,
                    format!(
                        "no {} job is waiting for verifier {} under policy revision {}",
                        phase.as_str(),
                        repr(&verifier.name)?,
                        repr(revision)?
                    ),
                ));
            }
            let own = if phase == Phase::Verify {
                repo::own_pending_agent(&mut tx, project.id, claimant)
                    .await
                    .map_err(|_| internal(&context, "job claim own attempts"))?
            } else {
                0
            };
            return Err(domain(
                ErrorCode::Conflict,
                if own > 0 {
                    format!(
                        "the waiting {} jobs are of attempts you ran; another agent or researcher verifies them",
                        phase.as_str()
                    )
                } else {
                    format!("no {} job is waiting for an agent", phase.as_str())
                },
            ));
        };
        let job = repo::claim_job(
            &mut tx,
            id,
            &auth.principal,
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
            &project,
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
    let project = job_workers::worker(
        &mut auth.connection,
        &auth.principal,
        &paths["slug"],
        &context,
    )
    .await?;
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
        job_workers::holder(&job, &auth.principal)?;
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
        if job.phase == Phase::Verify && attempt.state != cannery_attempts::model::State::Verifying
        {
            return Err(domain(
                ErrorCode::StaleLease,
                format!(
                    "the attempt is no longer being verified ({})",
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
