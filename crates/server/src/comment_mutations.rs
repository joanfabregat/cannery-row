//! Comment creation and immutable edit history, with explicit mention recursion profile.
use crate::{
    authentication::authenticate,
    body::DecodedBody,
    comment_routes::{self as routes, Failure, RouteState, domain, internal},
    errors::ApiError,
    requests::RequestContext,
    validation::{self, BodyInput},
};
use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use cannery_comments_reports::comments;
use cannery_core::{
    audit::{self, Attribution, Record},
    errors::ErrorCode,
    json::{DocumentBuilder, Node},
    principal::Role,
};
use cannery_hypotheses::repo as hypotheses;
use cannery_projects::authz;
use num_bigint::BigInt;
use sqlx::Acquire;

/// Required source entry-point profile. No production-global recursion assumption.
pub struct CommentMutationContext {
    pub mention_walk_budget: usize,
}
fn input(body: &DecodedBody) -> BodyInput<'_> {
    match body {
        DecodedBody::Missing => BodyInput::Missing,
        DecodedBody::RawBytes => BodyInput::RawBytes,
        DecodedBody::Json(d) => BodyInput::Json(d),
    }
}
fn map_failure(error: crate::hypothesis_routes::Failure) -> Failure {
    Failure(Box::new(error.into_response()))
}

#[allow(
    clippy::too_many_lines,
    reason = "Source transaction and validation order are explicit"
)]
pub(crate) async fn create(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, raw) = crate::body::read_body(request)
        .await
        .map_err(|e| Failure(Box::new(e.into_response())))?;
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(|e| Failure(Box::new(e.into_response())))?;
    let raw = crate::api_contract::typed_body::<crate::api_models::CommentCreate>(raw)
        .map_err(|error| Failure(Box::new(error.into_response())))?;
    let paths = routes::paths(&mut parts, &state).await?;
    let parameters = validation::hypothesis_parameters(
        paths.get("number").map(String::as_str),
        paths.get("sequence").map(String::as_str),
        &[],
        false,
        false,
    );
    let body = crate::comment_request::parse(input(&raw), false);
    let mut errors = Vec::new();
    let parameters = match parameters {
        Ok(p) => Some(p),
        Err(e) => {
            for mut p in e.problems().to_vec() {
                if p.loc[1] == validation::Location::Field(String::from("revision")) {
                    p.loc[1] = validation::Location::Field(String::from("sequence"));
                }
                errors.push(p);
            }
            None
        }
    };
    let body = match body {
        Ok(b) => Some(b),
        Err(e) => {
            errors.extend(e.problems().to_vec());
            None
        }
    };
    if !errors.is_empty() {
        return Err(routes::invalid(
            &validation::ValidationErrors::from_problems(errors),
            &context,
        ));
    }
    let parameters =
        parameters.ok_or_else(|| internal(&context, "comment parameters invariant"))?;
    let body = body.ok_or_else(|| internal(&context, "comment body invariant"))?;
    let user = auth
        .principal
        .require_user()
        .map_err(|e| Failure(Box::new(ApiError::from(e).into_response())))?;
    let project = authz::project_access(
        &mut auth.connection,
        &auth.principal,
        paths
            .get("slug")
            .ok_or_else(|| internal(&context, "comment slug"))?,
        Some(Role::Member),
        &[],
        true,
    )
    .await
    .map_err(|e| Failure(Box::new(context.project_error(e).into_response())))?
    .project;
    let number = parameters
        .number
        .ok_or_else(|| internal(&context, "comment number"))?;
    let profile = state
        .context
        .mutations
        .as_ref()
        .ok_or_else(|| internal(&context, "comment context"))?;
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "comment begin"))?;
    let result = async {
        let hyp = hypotheses::get_hypothesis(
            &mut tx,
            project.id,
            &number,
            false,
            state.context.hypotheses,
        )
        .await
        .map_err(|_| internal(&context, "comment target"))?
        .ok_or_else(|| {
            domain(
                ErrorCode::NotFound,
                format!("hypothesis #{number} not found"),
            )
        })?;
        let attempt = if let Some(sequence) = parameters.revision {
            Some(
                routes::attempt_id(&mut tx, hyp.id, &sequence, &context)
                    .await?
                    .ok_or_else(|| {
                        domain(
                            ErrorCode::NotFound,
                            format!("attempt #{number}.{sequence} not found"),
                        )
                    })?,
            )
        } else {
            None
        };
        let text = body
            .body
            .as_utf8()
            .ok_or_else(|| internal(&context, "comment body encoding"))?;
        let id = comments::create_comment(&mut tx, project.id, hyp.id, attempt, &text, user)
            .await
            .map_err(|_| internal(&context, "comment create"))?;
        let mut builder = DocumentBuilder::new();
        let root = builder
            .push(Node::String(body.body))
            .map_err(|_| internal(&context, "comment mentions document"))?;
        let document = builder
            .finish(root)
            .map_err(|_| internal(&context, "comment mentions document"))?;
        let mentions = crate::hypothesis_mutations::resolve_mentions(
            &mut tx,
            &auth.principal,
            &project,
            &document,
            Some(hyp.id),
            profile.mention_walk_budget,
            &context,
        )
        .await
        .map_err(map_failure)?;
        hypotheses::replace_mentions(
            &mut tx,
            hypotheses::MentionSource::Comment(hypotheses::CommentId(id.0)),
            mentions,
        )
        .await
        .map_err(|_| internal(&context, "comment mentions"))?;
        let created = comments::get_comment(&mut tx, project.id, id, false)
            .await
            .map_err(|_| internal(&context, "comment created read"))?
            .ok_or_else(|| internal(&context, "comment created invariant"))?;
        let output = routes::output(&created);
        let on = output
            .attempt_ref
            .as_deref()
            .map_or_else(|| format!("#{number}"), str::to_owned);
        let new = serde_json::json!({"on":on,"revision":1});
        audit::record(
            &mut tx,
            Attribution::Principal(&auth.principal),
            Record {
                action: "comment.created",
                subject_type: "comment",
                subject_id: &id.0.to_string(),
                project_id: Some(project.id),
                prior_state: None,
                new_state: Some(&new),
                reason: None,
                idempotency_key: None,
            },
        )
        .await
        .map_err(|_| internal(&context, "comment audit"))?;
        Ok(output)
    }
    .await;
    match result {
        Ok(output) => {
            tx.commit()
                .await
                .map_err(|_| internal(&context, "comment commit"))?;
            Ok((StatusCode::CREATED, routes::response(&output, &context)?).into_response())
        }
        Err(e) => {
            if tx.rollback().await.is_err() {
                auth.connection.close_on_drop();
            }
            Err(e)
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "Locked author, stale, no-op and mutation order matches source"
)]
#[utoipa::path(
    put,
    path = "/api/projects/{slug}/comments/{comment_id}",
    operation_id = "edit_comment_api_projects__slug__comments__comment_id__put",
    summary = "Edit Comment",
    description = "Edit your own comment; the previous body stays in its history.",
    params(("slug" = String, Path),
        ("comment_id" = String, Path, format = "uuid")),
    request_body(content = crate::api_models::CommentEdit, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = crate::api_models::CommentOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn edit(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, raw) = crate::body::read_body(request)
        .await
        .map_err(|e| Failure(Box::new(e.into_response())))?;
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(|e| Failure(Box::new(e.into_response())))?;
    let raw = crate::api_contract::typed_body::<crate::api_models::CommentEdit>(raw)
        .map_err(|error| Failure(Box::new(error.into_response())))?;
    let paths = routes::paths(&mut parts, &state).await?;
    let mut errors = Vec::new();
    let id = routes::path_uuid(paths.get("comment_id"), &mut errors, &context)?;
    let body = match crate::comment_request::parse(input(&raw), true) {
        Ok(v) => Some(v),
        Err(e) => {
            errors.extend(e.problems().to_vec());
            None
        }
    };
    if !errors.is_empty() {
        return Err(routes::invalid(
            &validation::ValidationErrors::from_problems(errors),
            &context,
        ));
    }
    let id = id.ok_or_else(|| internal(&context, "comment id invariant"))?;
    let body = body.ok_or_else(|| internal(&context, "comment body invariant"))?;
    let user = auth
        .principal
        .require_user()
        .map_err(|e| Failure(Box::new(ApiError::from(e).into_response())))?;
    let project = authz::project_access(
        &mut auth.connection,
        &auth.principal,
        paths
            .get("slug")
            .ok_or_else(|| internal(&context, "comment slug"))?,
        Some(Role::Member),
        &[],
        true,
    )
    .await
    .map_err(|e| Failure(Box::new(context.project_error(e).into_response())))?
    .project;
    let profile = state
        .context
        .mutations
        .as_ref()
        .ok_or_else(|| internal(&context, "comment context"))?;
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "comment begin"))?;
    let result = async {
        let comment = comments::get_comment(&mut tx, project.id, id, true)
            .await
            .map_err(|_| internal(&context, "comment lock"))?
            .ok_or_else(|| domain(ErrorCode::NotFound, "comment not found"))?;
        if comment.author_user != user.user_id {
            return Err(domain(
                ErrorCode::Forbidden,
                "only its author can edit a comment",
            ));
        }
        if Some(BigInt::from(comment.revision)) != body.revision {
            return Err(domain(
                ErrorCode::StaleRevision,
                format!("the comment is at revision {}", comment.revision),
            ));
        }
        let text = body
            .body
            .as_utf8()
            .ok_or_else(|| internal(&context, "comment body encoding"))?;
        if comment.body_markdown == text {
            return Ok(Some(comment));
        }
        let revision = comments::edit_comment(&mut tx, id, &text)
            .await
            .map_err(|_| internal(&context, "comment edit"))?;
        comments::add_revision(&mut tx, id, &BigInt::from(revision), &text, user)
            .await
            .map_err(|_| internal(&context, "comment revision"))?;
        let mut builder = DocumentBuilder::new();
        let root = builder
            .push(Node::String(body.body))
            .map_err(|_| internal(&context, "comment mentions document"))?;
        let document = builder
            .finish(root)
            .map_err(|_| internal(&context, "comment mentions document"))?;
        let mentions = crate::hypothesis_mutations::resolve_mentions(
            &mut tx,
            &auth.principal,
            &project,
            &document,
            Some(comment.hypothesis_id),
            profile.mention_walk_budget,
            &context,
        )
        .await
        .map_err(map_failure)?;
        hypotheses::replace_mentions(
            &mut tx,
            hypotheses::MentionSource::Comment(hypotheses::CommentId(id.0)),
            mentions,
        )
        .await
        .map_err(|_| internal(&context, "comment mentions"))?;
        let prior = serde_json::json!({"revision":comment.revision});
        let new = serde_json::json!({"revision":revision});
        audit::record(
            &mut tx,
            Attribution::Principal(&auth.principal),
            Record {
                action: "comment.edited",
                subject_type: "comment",
                subject_id: &id.0.to_string(),
                project_id: Some(project.id),
                prior_state: Some(&prior),
                new_state: Some(&new),
                reason: None,
                idempotency_key: None,
            },
        )
        .await
        .map_err(|_| internal(&context, "comment audit"))?;
        let edited = comments::get_comment(&mut tx, project.id, id, false)
            .await
            .map_err(|_| internal(&context, "comment edited read"))?;
        Ok(edited)
    }
    .await;
    match result {
        Ok(edited) => {
            tx.commit()
                .await
                .map_err(|_| internal(&context, "comment commit"))?;
            let edited = edited.ok_or_else(|| internal(&context, "comment edited invariant"))?;
            routes::response(&routes::output(&edited), &context)
        }
        Err(e) => {
            if tx.rollback().await.is_err() {
                auth.connection.close_on_drop();
            }
            Err(e)
        }
    }
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/hypotheses/{number}/comments",
    operation_id = "comment_on_hypothesis_api_projects__slug__hypotheses__number__comments_post",
    summary = "Comment On Hypothesis",
    params(("slug" = String, Path),
        ("number" = i64, Path)),
    request_body(content = crate::api_models::CommentCreate, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::CommentOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_comment_on_hypothesis(
    arg0: State<RouteState>,
    arg1: axum::Extension<RequestContext>,
    arg2: Request,
) -> Result<Response, Failure> {
    create(arg0, arg1, arg2).await
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/comments",
    operation_id = "comment_on_attempt_api_projects__slug__hypotheses__number__attempts__sequence__comments_post",
    summary = "Comment On Attempt",
    params(("slug" = String, Path),
        ("number" = i64, Path),
        ("sequence" = i64, Path)),
    request_body(content = crate::api_models::CommentCreate, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::CommentOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_comment_on_attempt(
    arg0: State<RouteState>,
    arg1: axum::Extension<RequestContext>,
    arg2: Request,
) -> Result<Response, Failure> {
    create(arg0, arg1, arg2).await
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
