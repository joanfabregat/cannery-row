//! Attempt claims keep hypothesis-before-track locks and post-commit model construction.
use crate::{
    AppState,
    attempt_lease_routes::{Failure, domain, failure, internal, worker_access},
    attempt_read_wire, attempt_workflow,
    authentication::authenticate,
    body::{self, DecodedBody},
    claim_request::{ClaimRequest, claim_mode},
    errors::ApiError,
    requests::RequestContext,
    step_binding::{self, BindingContext, PgManifestQuery, RawEquality},
    validation::BodyInput,
};
use axum::{
    Router,
    extract::{FromRequestParts, Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use cannery_attempts::{
    model::{JsonContext, StoredJson},
    repo::{CreateAttempt, Repository},
};
use cannery_core::{
    audit::{self, Attribution, Record},
    errors::{DomainError, ErrorCode},
    json::{self, Document, DocumentBuilder, Node},
    principal::Secret,
};
use cannery_research::{
    config_repo::{self, Kind},
    job_baselines, job_deadlines,
    science::{RenderingContext, Science},
};
use num_bigint::BigInt;
use sqlx::Acquire;
use std::{collections::BTreeMap, sync::Arc};

/// Entry-selected profiles and entropy; the physical pooled-owner integration is pending.
pub struct AttemptClaimContext {
    pub repository: JsonContext,
    pub configuration: config_repo::JsonContext,
    pub rendering: RenderingContext,
    pub manifest_decode_budget: usize,
    pub audit_budget: usize,
    pub response: attempt_read_wire::ResponseContext,
    pub equality: Arc<dyn RawEquality>,
    pub mint: fn() -> Result<Secret, cannery_identity::error::IdentityError>,
}
#[derive(Clone)]
pub(crate) struct RouteState {
    app: AppState,
    profile: Arc<AttemptClaimContext>,
}
pub fn routes(app: AppState, profile: Arc<AttemptClaimContext>) -> Router {
    Router::new()
        .route("/api/projects/{slug}/claims", post(claim))
        .with_state(RouteState { app, profile })
}
fn mapping(
    b: &mut DocumentBuilder,
    fields: Vec<(&str, cannery_core::json::NodeId)>,
) -> Result<cannery_core::json::NodeId, json::BuildError> {
    b.push(Node::Object(
        fields
            .into_iter()
            .map(|(key, value)| (String::from(key), value))
            .collect(),
    ))
}
fn control_document(
    control: Option<cannery_attempts::repo::Control>,
) -> Result<Option<Document>, json::BuildError> {
    control
        .map(|control| {
            let mut b = DocumentBuilder::new();
            let id = b.push(Node::String(control.id))?;
            let revision = b.push(Node::String(control.revision))?;
            let root = mapping(&mut b, vec![("id", id), ("revision", revision)])?;
            b.finish(root)
        })
        .transpose()
}
fn workflow_document(steps: &[step_binding::ResolvedStep]) -> Result<Document, json::BuildError> {
    let mut b = DocumentBuilder::new();
    let refs = steps
        .iter()
        .map(|step| {
            let reference = step.reference()?;
            b.import(&reference, reference.root())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let refs = b.push(Node::Array(refs))?;
    let root = mapping(&mut b, vec![("steps", refs)])?;
    b.finish(root)
}
fn manifests(
    science: &Science<'_>,
    producer: &Document,
    steps: &[step_binding::ResolvedStep],
) -> Result<Document, Box<dyn std::error::Error + Send + Sync>> {
    let mut b = DocumentBuilder::new();
    let mut values = steps
        .iter()
        .map(|step| b.import(&step.manifest, step.manifest.root()))
        .collect::<Result<Vec<_>, _>>()?;
    values.push(b.import(producer, producer.root())?);
    // The source list evaluates the scorer even when the control is None.
    values.push(b.import(science.content, science.scorer()?)?);
    let root = b.push(Node::Array(values))?;
    Ok(b.finish(root)?)
}
fn conflict(message: String, path: &str, detail: &str) -> Failure {
    failure(ApiError::from(
        DomainError::new(ErrorCode::Conflict, message)
            .with_details(serde_json::json!([{"path":path,"message":detail}])),
    ))
}
fn repr(value: &str, context: &RequestContext) -> Result<String, Failure> {
    cannery_core::text::repr_string(&String::from(value))
        .map_err(|_| internal(context, "claim track repr"))?
        .as_utf8()
        .ok_or_else(|| internal(context, "claim track repr"))
}
#[allow(
    clippy::too_many_lines,
    reason = "The source controller's locking, skip and transaction order are observable"
)]
#[utoipa::path(
    post,
    path = "/api/projects/{slug}/claims",
    operation_id = "claim_api_projects__slug__claims_post",
    summary = "Claim",
    description = "Claim a queued hypothesis.\n\n``409 nothing_to_claim`` when none is available. In ``workflow`` mode a\ntrack whose workflow (or producer) cannot be pinned under the current\nscience revision is skipped, so the other tracks' hypotheses still flow;\nwhen only such tracks have queued hypotheses, ``409 workflow_unavailable``\nnames them.",
    params(("slug" = String, Path)),
    request_body(content = crate::api_models::ClaimRequest, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::ClaimOut, content_type = "application/json"),
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
    let body = crate::api_contract::typed_body::<crate::api_models::ClaimRequest>(body)
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
    let body = ClaimRequest::parse(input).map_err(|e| {
        e.domain_error().map_or_else(
            |_| internal(&context, "claim validation"),
            |e| failure(ApiError::from(e)),
        )
    })?;
    let project = worker_access(
        &mut auth.connection,
        &auth.principal,
        &paths["slug"],
        &context,
    )
    .await?;
    let mode = claim_mode(&auth.principal, body.mode).map_err(|e| failure(ApiError::from(e)))?;
    let secret = (state.profile.mint)().map_err(|_| internal(&context, "claim entropy"))?;
    let ttl = state.app.settings.leases.ttl_seconds.as_bigint();
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "claim transaction"))?;
    let result = async {
        let mut unavailable = BTreeMap::<String, step_binding::Validation>::new();
        loop {
            // UTF-8 conversion belongs to the driver's first parameter adaptation, after authorization/mode/mint.
            let track = body.track.as_ref()
                .map(|value| value.as_utf8()
                    .ok_or_else(|| internal(&context, "claim track encoding")))
                .transpose()?;
            let skip = unavailable.keys().cloned().collect::<Vec<_>>();
            let id = Repository::new(&mut tx, state.profile.repository)
                .pick_claimable(project.id, body.hypothesis.as_ref(), track.as_deref(), mode.as_str(), &skip)
                .await.map_err(|_| internal(&context, "claim pick"))?;
            let Some(id) = id else {
                if !unavailable.is_empty() {
                    let details = unavailable.iter().map(|(track, error)| {
                        let message = error.message.as_utf8()
                            .ok_or_else(|| internal(&context, "workflow diagnostic encoding"))?;
                        Ok(serde_json::json!({"path":format!("/tracks/{track}"),"message":message}))
                    }).collect::<Result<Vec<_>, Failure>>()?;
                    let tracks = unavailable.keys().cloned().collect::<Vec<_>>().join(", ");
                    let message = format!(
                        "queued hypotheses wait in workflow tracks that cannot run under the current science revision: {tracks}"
                    );
                    return Err(failure(ApiError::from(
                        DomainError::new(ErrorCode::WorkflowUnavailable, message)
                            .with_details(serde_json::json!(details))
                    )));
                }
                let suffix = if mode.as_str() == "agent" { "" } else { " in workflow mode" };
                return Err(domain(ErrorCode::NothingToClaim, format!(
                    "no queued hypothesis in an active track is available to claim{suffix}"
                )));
            };
            let revision = config_repo::get_revision(
                &mut tx, project.id, Kind::Science, None, state.profile.configuration
            ).await.map_err(|_| internal(&context, "claim science lookup"))?
                .ok_or_else(|| internal(&context, "claim science absent"))?;
            let science = Science::new(
                BigInt::from(revision.revision), &revision.content, state.profile.rendering
            ).map_err(|_| internal(&context, "claim science construction"))?;
            let track = Repository::new(&mut tx, state.profile.repository)
                .pin_track(id).await.map_err(|_| internal(&context, "claim track lock"))?;
            if track.state.as_str() != "active" {
                return Err(domain(ErrorCode::Conflict, format!(
                    "track {} is {}; its hypotheses wait", repr(&track.slug, &context)?, track.state.as_str()
                )));
            }
            if track.mode.as_str() != mode.as_str() {
                return Err(domain(ErrorCode::Conflict, format!(
                    "track {} is now in {} mode", repr(&track.slug, &context)?, track.mode.as_str()
                )));
            }
            let binding = BindingContext {
                rendering: state.profile.rendering,
                decode_budget: state.profile.manifest_decode_budget,
                equality: state.profile.equality.as_ref(),
            };
            let resolved = async {
                let mut query = PgManifestQuery(&mut tx);
                let selected = match &track.producer {
                    StoredJson::SqlNull => None,
                    StoredJson::Value(d) if matches!(d.node(d.root()), Some(Node::Null)) => None,
                    StoredJson::Value(d) => Some(d.as_ref()),
                };
                let producer = step_binding::resolve_producer_diagnostic(
                    &mut query, project.id, &science, selected, &String::from("/producer"), &binding
                ).await?;
                let steps = if mode.as_str() == "agent" {
                    vec![]
                } else {
                    let StoredJson::Value(workflow) = &track.workflow else {
                        return Err(step_binding::Error::Store);
                    };
                    step_binding::resolve_workflow_diagnostic(
                        &mut query, project.id, &science, workflow, &String::from("/workflow"),
                        Some(&producer.manifest), &binding
                    ).await?
                };
                Ok::<_, step_binding::Error>((producer, steps))
            }.await;
            let (producer, steps) = match resolved {
                Ok(found) => found,
                Err(step_binding::Error::Validation(error)) if mode.as_str() == "workflow" => {
                    unavailable.insert(track.slug, error);
                    continue;
                }
                Err(step_binding::Error::Validation(error)) => {
                    let message = error.message.as_utf8().ok_or_else(|| internal(&context, "claim diagnostic encoding"))?;
                    let path = error.path.as_utf8().ok_or_else(|| internal(&context, "claim diagnostic path"))?;
                    return Err(conflict(format!(
                        "track {} cannot be tested under science revision {}: {message}",
                        repr(&track.slug, &context)?, science.revision
                    ), &path, &message));
                }
                Err(_) => return Err(internal(&context, "claim step binding")),
            };
            let control = Repository::new(&mut tx, state.profile.repository)
                .approved_control(id, state.profile.rendering.nesting_budget)
                .await.map_err(|_| internal(&context, "claim control lookup"))?;
            let control = control_document(control).map_err(|_| internal(&context, "claim control document"))?;
            let all = manifests(&science, &producer.manifest, &steps).map_err(|_| internal(&context, "claim manifests"))?;
            if let Some(message) = job_baselines::control_conflict(
                &science, &all, control.as_ref(), state.profile.rendering
            ).map_err(|_| internal(&context, "claim control conflict"))? {
                let message = message.as_utf8().ok_or_else(|| internal(&context, "claim control encoding"))?;
                return Err(conflict(format!(
                    "the hypothesis cannot be tested under science revision {}: {message}", science.revision
                ), "/control", &message));
            }
            let producer_ref = producer.reference().map_err(|_| internal(&context, "claim producer reference"))?;
            let workflow = if mode.as_str() == "agent" {
                None
            } else {
                Some(workflow_document(&steps).map_err(|_| internal(&context, "claim workflow references"))?)
            };
            let deadline = if mode.as_str() == "agent" {
                None
            } else {
                Some(job_deadlines::workflow_seconds(
                    &science, &steps.iter().map(|s| s.manifest.as_ref()).collect::<Vec<_>>(),
                    state.app.settings.leases.job_overhead_seconds.as_bigint(), state.profile.rendering
                ).map_err(|_| internal(&context, "claim deadline"))?)
            };
            let token_hash = cannery_identity::secrets::digest(secret.expose());
            let attempt_id = Repository::new(&mut tx, state.profile.repository)
                .create_attempt(CreateAttempt {
                    hypothesis_id: id, science_revision: &science.revision, producer: &producer_ref,
                    token_hash: &token_hash, ttl_seconds: ttl, principal: &auth.principal,
                    workflow: workflow.as_ref(), deadline_seconds: deadline.as_ref(),
                }).await.map_err(|_| internal(&context, "claim create"))?;
            let attempt = Repository::new(&mut tx, state.profile.repository)
                .get_attempt_by_id(attempt_id, false).await
                .map_err(|_| internal(&context, "claim attempt lookup"))?
                .ok_or_else(|| internal(&context, "claim attempt absent"))?;
            let run = if mode.as_str() == "agent" {
                None
            } else {
                let resolved = steps.iter().map(|step| attempt_workflow::WorkflowStep {
                    name: &step.name, revision: &step.revision, manifest: &step.manifest,
                }).collect::<Vec<_>>();
                Some(attempt_workflow::run_spec(
                    &mut Repository::new(&mut tx, state.profile.repository),
                    &attempt, &science, &resolved, control.as_ref(), state.profile.rendering
                ).await.map_err(|_| internal(&context, "claim run spec"))?)
            };
            // These references contain only registered UTF-8 names and INT4 revisions, not raw manifests.
            let producer_json = json::encode_ascii_pretty(&producer_ref, state.profile.audit_budget)
                .map_err(|_| internal(&context, "claim audit producer"))?;
            let producer = serde_json::from_str::<serde_json::Value>(&producer_json)
                .map_err(|_| internal(&context, "claim audit producer"))?;
            let mut new = serde_json::json!({
                "ref":format!("#{}.{}",attempt.hypothesis_number,attempt.sequence),
                "hypothesis_state":"active", "lease_generation":attempt.lease_generation,
                "hypothesis_revision":attempt.hypothesis_revision,
                "science_revision":attempt.science_revision, "producer":producer,
            });
            let pins = crate::context_bundle::pins(&mut tx, &project, attempt.id, &context)
                .await
                .map_err(failure)?;
            if let Some(brief) = &pins.brief {
                new["brief_revision"] = brief.revision.into();
            }
            if let Some(plan) = &pins.plan {
                new["plan_revision"] = plan.revision.into();
            }
            if let Some(workflow) = workflow {
                let workflow_json = json::encode_ascii_pretty(&workflow, state.profile.audit_budget)
                    .map_err(|_| internal(&context, "claim audit workflow"))?;
                new["workflow"] = serde_json::from_str(&workflow_json)
                    .map_err(|_| internal(&context, "claim audit workflow"))?;
            }
            let prior = serde_json::json!({"hypothesis_state":"queued"});
            let id = attempt.id.to_string();
            audit::record(&mut tx, Attribution::Principal(&auth.principal), Record {
                action:"attempt.claimed", subject_type:"attempt", subject_id:&id,
                project_id:Some(project.id), prior_state:Some(&prior), new_state:Some(&new),
                reason:None, idempotency_key:None,
            }).await.map_err(|_| internal(&context, "claim audit"))?;
            let heartbeat = std::cmp::max(BigInt::from(1), ttl / BigInt::from(3));
            let bytes = attempt_read_wire::claim(
                &attempt,
                secret.expose(),
                &heartbeat,
                run.as_ref(),
                pins,
                state.profile.response,
            )
            .map_err(|_| internal(&context, "claim response model"))?;
            return Ok(bytes);
        }
    }.await;
    let bytes = match result {
        Ok(value) => {
            tx.commit()
                .await
                .map_err(|_| internal(&context, "claim commit"))?;
            value
        }
        Err(error) => {
            tx.rollback()
                .await
                .map_err(|_| internal(&context, "claim rollback"))?;
            return Err(error);
        }
    };
    Ok((
        StatusCode::CREATED,
        [(header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response())
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
