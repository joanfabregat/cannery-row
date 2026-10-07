//! Release preserves source pre-lock authorization and post-commit response checks.
use crate::{
    attempt_failure::{self, Report},
    attempt_lease_routes::{self, Failure, RouteState, domain, failure, internal},
    attempt_read_wire, attempt_release_request,
    authentication::authenticate,
    body::{self, DecodedBody},
    errors::ApiError,
    requests::RequestContext,
    validation::BodyInput,
};
use axum::{
    extract::{FromRequestParts, Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use cannery_attempts::repo::Repository;
use cannery_core::{
    errors::{DomainError, ErrorCode},
    json::{Document, DocumentBuilder, Node},
    principal::{Principal, ServiceKind},
};
use num_bigint::BigInt;
use serde_json::json;
use sqlx::Acquire;
use std::collections::BTreeMap;

pub struct AttemptReleaseContext {
    pub configuration: cannery_research::config_repo::JsonContext,
    pub science: cannery_research::science::RenderingContext,
    pub response: attempt_read_wire::ResponseContext,
}
fn violation(path: &str, message: &str) -> Failure {
    failure(ApiError::from(
        DomainError::new(ErrorCode::ValidationFailed, message)
            .with_details(json!([{"path":path,"message":message}])),
    ))
}
fn documents(
    body: &attempt_release_request::Release,
) -> Result<(Document, Document), cannery_core::json::BuildError> {
    let mut b = DocumentBuilder::new();
    let fields = if let Some(step) = &body.step {
        let v = b.push(Node::String(String::from(step)))?;
        vec![(String::from("step"), v)]
    } else {
        vec![]
    };
    let root = b.push(Node::Object(fields))?;
    let details = b.finish(root)?;
    let mut b = DocumentBuilder::new();
    let mut logs = vec![];
    for log in &body.logs {
        let key = b.push(Node::String(String::from(&log.key)))?;
        let size = b.push(Node::Integer(log.size_bytes.clone()))?;
        let sha = b.push(Node::String(String::from(&log.sha256)))?;
        logs.push(b.push(Node::Object(vec![
            (String::from("key"), key),
            (String::from("size_bytes"), size),
            (String::from("sha256"), sha),
        ]))?);
    }
    let root = b.push(Node::Array(logs))?;
    Ok((details, b.finish(root)?))
}
#[allow(
    clippy::too_many_lines,
    reason = "Authorization, locked mutation and post-commit model phases retain source order"
)]
#[utoipa::path(
    post,
    path = "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/release",
    operation_id = "release_api_projects__slug__hypotheses__number__attempts__sequence__release_post",
    summary = "Release",
    description = "Give up the attempt. It fails and a researcher decides whether to retry.\n\nAn experimenter (a Cannery Row runner) giving up a runner-driven attempt\nreports why with ``code``, the failing ``step`` and its ``logs``. Its\nreport is trusted: the failure is one of the run, and the hypothesis is\nqueued again automatically while retries remain (docs/spec.md). Only an\nexperimenter may send them.",
    params(("slug" = String, Path),
        ("number" = i64, Path),
        ("sequence" = i64, Path),
        ("X-Lease-Token" = Option<String>, Header),
        ("X-Lease-Generation" = Option<i64>, Header)),
    request_body(content = crate::api_models::ReleaseRequest, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = crate::api_models::AttemptOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn release(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = body::read_body(request).await.map_err(failure)?;
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(failure)?;
    let body = crate::api_contract::typed_body::<crate::api_models::ReleaseRequest>(body)
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
    let checked = attempt_release_request::release(input);
    let errors = checked.as_ref().err().map_or(&[][..], |e| e.problems());
    let parameters =
        attempt_lease_routes::parameters_with_body(&paths, &parts.headers, &context, errors)?;
    let body = checked.map_err(|_| internal(&context, "release body validation"))?;
    let project = attempt_lease_routes::worker_access(
        &mut auth.connection,
        &auth.principal,
        &paths["slug"],
        &context,
    )
    .await?;
    let experimenter =
        matches!(&auth.principal,Principal::Service(p) if p.kind==ServiceKind::Experimenter);
    if !experimenter && (body.code.is_some() || body.step.is_some() || !body.logs.is_empty()) {
        return Err(domain(
            ErrorCode::Forbidden,
            "only an experimenter reports a failure code, step or logs",
        ));
    }
    let profile = state
        .context
        .release
        .as_ref()
        .ok_or_else(|| internal(&context, "release profile absent"))?;
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "release transaction"))?;
    let result = async {
        let attempt = attempt_lease_routes::leased(
            &mut tx,
            &auth.principal,
            &project,
            &parameters,
            state.context.repository,
            &context,
        )
        .await?;
        if attempt.workflow.python_none() {
            // Agent mode ignores otherwise permitted experimenter report fields.
            let mut builder = DocumentBuilder::new();
            let root = builder
                .push(Node::Object(vec![]))
                .map_err(|_| internal(&context, "agent failure details"))?;
            let details = builder
                .finish(root)
                .map_err(|_| internal(&context, "agent failure details"))?;
            attempt_failure::agent(
                &mut tx,
                state.context.repository,
                &auth.principal,
                &attempt,
                &Report {
                    code: "released",
                    reason: &body.reason,
                    details: &details,
                    log_refs: None,
                    step: None,
                    idempotency_key: None,
                    manifest_sha256: None,
                },
                &context,
            )
            .await?;
        } else {
            let code = body
                .code
                .ok_or_else(|| violation("/code", "an experimenter says why the run failed"))?;
            let artifacts = Repository::new(&mut tx, state.context.repository)
                .list_artifacts(attempt.id)
                .await
                .map_err(|_| internal(&context, "release artifacts"))?;
            let verified = artifacts
                .iter()
                .filter(|a| a.job_id.is_none())
                .map(|a| (&a.key, a))
                .collect::<BTreeMap<_, _>>();
            for (index, log) in body.logs.iter().enumerate() {
                if !verified.get(&log.key).is_some_and(|a| {
                    BigInt::from(a.size_bytes) == log.size_bytes && a.sha256 == log.sha256
                }) {
                    return Err(violation(
                        &format!("/logs/{index}"),
                        "not a verified upload of this attempt",
                    ));
                }
            }
            let (details, logs) =
                documents(&body).map_err(|_| internal(&context, "release report documents"))?;
            attempt_failure::experiment(
                &mut tx,
                state.context.repository,
                &auth.principal,
                &attempt,
                &Report {
                    code,
                    reason: &body.reason,
                    details: &details,
                    log_refs: Some(&logs),
                    step: body.step.as_deref(),
                    idempotency_key: None,
                    manifest_sha256: None,
                },
                profile,
                &context,
            )
            .await?;
        }
        Repository::new(&mut tx, state.context.repository)
            .get_attempt_by_id(attempt.id, false)
            .await
            .map_err(|_| internal(&context, "released attempt lookup"))?
            .ok_or_else(|| internal(&context, "released attempt absent"))
    }
    .await;
    let attempt = match result {
        Ok(attempt) => {
            tx.commit()
                .await
                .map_err(|_| internal(&context, "release commit"))?;
            attempt
        }
        Err(error) => {
            if tx.rollback().await.is_err() {
                auth.connection.close_on_drop();
            }
            return Err(error);
        }
    };
    let bytes = attempt_read_wire::attempt(&attempt, profile.response)
        .map_err(|_| internal(&context, "release response model"))?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response())
}
