//! Project HTTP operations preserve the public projection and caller-owned transactions.

use crate::api_models::{MemberOut, Page_MemberOut_UUID_, Page_ProjectOut_str_, ProjectOut};
use crate::{AppState, authentication::authenticate, errors::ApiError, requests::RequestContext};
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use cannery_core::{
    audit::{self, Attribution, Record},
    errors::{DomainError, ErrorCode},
    principal::{Principal, Role},
};
use cannery_projects::{authz, repo};
use serde_json::json;
use sqlx::Acquire;

// Keep mixed transport/domain failures compact without changing their responses.
pub(crate) struct RouteFailure(Box<axum::response::Response>);

impl RouteFailure {
    fn new(error: impl IntoResponse) -> Self {
        Self(Box::new(error.into_response()))
    }
}

impl IntoResponse for RouteFailure {
    fn into_response(self) -> axum::response::Response {
        *self.0
    }
}

pub(crate) fn routes(state: AppState) -> Router {
    Router::new()
        .route(
            "/api/projects",
            get(list_projects).post(create_project).head(head_get),
        )
        .route("/api/projects/{slug}", get(get_project).head(head_get))
        .route(
            "/api/projects/{slug}/members",
            get(list_members).head(head_get),
        )
        .route(
            "/api/projects/{slug}/members/{user_id}",
            axum::routing::put(set_member).delete(remove_member),
        )
        .with_state(state)
}

async fn head_get() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "GET")],
        Json(json!({"detail":"Method Not Allowed"})),
    )
}

fn role_name(role: Role) -> String {
    match role {
        Role::Viewer => "viewer",
        Role::Member => "member",
        Role::Researcher => "researcher",
    }
    .into()
}
fn project_out(project: &repo::Project, role: Option<Role>) -> ProjectOut {
    ProjectOut {
        id: project.id.to_string(),
        slug: project.slug.clone(),
        title: project.title.clone(),
        description: project.description.clone(),
        created_at: crate::timestamps::public_timestamp(project.created_at),
        role: role.map(role_name),
    }
}
fn member_out(member: &repo::Member) -> MemberOut {
    MemberOut {
        user_id: member.user_id.to_string(),
        email: member.email.clone(),
        display_name: member.display_name.clone(),
        role: role_name(member.role),
        granted_at: crate::timestamps::public_timestamp(member.granted_at),
    }
}

fn not_found(message: &'static str) -> ApiError {
    DomainError::new(ErrorCode::NotFound, message).into()
}

async fn project_page(
    connection: &mut sqlx::PgConnection,
    principal: &Principal,
    context: &RequestContext,
    before: Option<&str>,
    limit: usize,
) -> Result<Page_ProjectOut_str_, ApiError> {
    if let Principal::Service(service) = principal {
        let project = repo::get_project(connection, service.project_id)
            .await
            .map_err(|error| context.project_error(error))?;
        let items: Vec<ProjectOut> = project
            .iter()
            .filter(|project| before.is_none_or(|cursor| project.slug.as_str() > cursor))
            .map(|project| project_out(project, None))
            .collect();
        return Ok(Page_ProjectOut_str_ {
            items,
            next_before: None,
        });
    }
    let user = principal.require_user()?;
    let fetch_limit =
        i64::try_from(limit + 1).map_err(|_| context.internal("project page limit"))?;
    let mut rows = repo::list_projects_for_user(
        connection,
        user.user_id,
        user.is_admin,
        before,
        Some(fetch_limit),
    )
    .await
    .map_err(|error| context.project_error(error))?;
    let next = if rows.len() > limit {
        rows.truncate(limit);
        rows.last().map(|row| row.slug.clone())
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(|project| ProjectOut {
            id: project.id.to_string(),
            slug: project.slug,
            title: project.title,
            description: project.description,
            created_at: crate::timestamps::public_timestamp(project.created_at),
            role: project.role.map(role_name),
        })
        .collect();
    Ok(Page_ProjectOut_str_ {
        items,
        next_before: next,
    })
}

async fn create(
    connection: &mut sqlx::PgConnection,
    principal: &Principal,
    context: &RequestContext,
    slug: &str,
    title: &str,
    description: &str,
) -> Result<ProjectOut, ApiError> {
    let admin = principal.require_admin(true)?;
    let mut transaction = connection
        .begin()
        .await
        .map_err(|_| context.internal("project transaction"))?;
    let project = repo::create_project(&mut transaction, slug, title, description, admin.user_id)
        .await
        .map_err(|error| context.project_error(error))?
        .ok_or_else(|| {
            ApiError::from(DomainError::new(
                ErrorCode::Conflict,
                "a project with this slug already exists",
            ))
        })?;
    let subject = project.id.to_string();
    let new_state = json!({"slug":project.slug,"title":project.title});
    audit::record(
        &mut transaction,
        Attribution::Principal(principal),
        Record {
            action: "project.created",
            subject_type: "project",
            subject_id: &subject,
            project_id: Some(project.id),
            prior_state: None,
            new_state: Some(&new_state),
            reason: None,
            idempotency_key: None,
        },
    )
    .await
    .map_err(|_| context.internal("project audit"))?;
    transaction
        .commit()
        .await
        .map_err(|_| context.internal("project commit"))?;
    Ok(project_out(&project, None))
}

async fn member_page(
    connection: &mut sqlx::PgConnection,
    principal: &Principal,
    context: &RequestContext,
    slug: &str,
    before: Option<cannery_core::ids::UserId>,
    limit: usize,
) -> Result<Page_MemberOut_UUID_, ApiError> {
    if !matches!(principal, Principal::User(user) if user.is_admin) {
        authz::project_access(connection, principal, slug, Some(Role::Viewer), &[], false)
            .await
            .map_err(|error| context.project_error(error))?;
    }
    let project = repo::get_project_by_slug(connection, slug)
        .await
        .map_err(|error| context.project_error(error))?
        .ok_or_else(|| not_found("project not found"))?;
    if let Some(cursor) = before
        && repo::get_role(connection, project.id, cursor)
            .await
            .map_err(|error| context.project_error(error))?
            .is_none()
    {
        return Err(
            DomainError::new(ErrorCode::ValidationFailed, "invalid cursor")
                .with_details(json!([{"path":"before","message":"not a member of this project"}]))
                .into(),
        );
    }
    let fetch_limit =
        i64::try_from(limit + 1).map_err(|_| context.internal("member page limit"))?;
    let mut members = repo::list_members(connection, project.id, before, Some(fetch_limit))
        .await
        .map_err(|error| context.project_error(error))?;
    let next = if members.len() > limit {
        members.truncate(limit);
        members.last().map(|member| member.user_id)
    } else {
        None
    };
    let items: Vec<MemberOut> = members.iter().map(member_out).collect();
    Ok(Page_MemberOut_UUID_ {
        items,
        next_before: next.map(|id| id.to_string()),
    })
}

async fn change_member(
    connection: &mut sqlx::PgConnection,
    principal: &Principal,
    context: &RequestContext,
    slug: &str,
    user_id: cannery_core::ids::UserId,
    role: Option<Role>,
) -> Result<Option<MemberOut>, ApiError> {
    let admin = principal.require_admin(true)?;
    let project = repo::get_project_by_slug(connection, slug)
        .await
        .map_err(|error| context.project_error(error))?
        .ok_or_else(|| not_found("project not found"))?;
    if role.is_some()
        && cannery_identity::repo::get_user(connection, user_id)
            .await
            .map_err(|error| context.identity_error(error))?
            .is_none()
    {
        return Err(not_found(
            "user not found; they must sign in once before being added",
        ));
    }
    let mut transaction = connection
        .begin()
        .await
        .map_err(|_| context.internal("membership transaction"))?;
    if !repo::lock_project(&mut transaction, project.id)
        .await
        .map_err(|error| context.project_error(error))?
    {
        return Err(not_found("project not found"));
    }
    let previous = if let Some(role) = role {
        repo::set_membership(&mut transaction, project.id, user_id, role, admin.user_id).await
    } else {
        repo::delete_membership(&mut transaction, project.id, user_id).await
    }
    .map_err(|error| context.project_error(error))?;
    if role.is_none() && previous.is_none() {
        return Err(not_found("this user is not a member of the project"));
    }
    if previous != role {
        let subject = format!("{}:{user_id}", project.id);
        let prior_state = previous.map(|role| json!({"role":role}));
        let new_state = role.map(|role| json!({"role":role}));
        audit::record(
            &mut transaction,
            Attribution::Principal(principal),
            Record {
                action: if role.is_some() {
                    "membership.set"
                } else {
                    "membership.removed"
                },
                subject_type: "membership",
                subject_id: &subject,
                project_id: Some(project.id),
                prior_state: prior_state.as_ref(),
                new_state: new_state.as_ref(),
                reason: None,
                idempotency_key: None,
            },
        )
        .await
        .map_err(|_| context.internal("membership audit"))?;
    }
    let member = if role.is_some() {
        Some(member_out(
            &repo::get_member(&mut transaction, project.id, user_id)
                .await
                .map_err(|error| context.project_error(error))?
                .ok_or_else(|| context.internal("updated member missing"))?,
        ))
    } else {
        None
    };
    transaction
        .commit()
        .await
        .map_err(|_| context.internal("membership commit"))?;
    Ok(member)
}

fn collect<T>(
    result: Result<T, crate::validation::ValidationErrors>,
    errors: &mut Option<crate::validation::ValidationErrors>,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            if let Some(errors) = errors {
                errors.append(error);
            } else {
                *errors = Some(error);
            }
            None
        }
    }
}

fn validation_failure(
    errors: Option<crate::validation::ValidationErrors>,
    context: &RequestContext,
) -> Result<(), ApiError> {
    if let Some(errors) = errors {
        return Err(errors
            .domain_error()
            .map_err(|_| context.internal("validation location encoding"))?
            .into());
    }
    Ok(())
}

fn body_input(body: &crate::body::DecodedBody) -> crate::validation::BodyInput<'_> {
    match body {
        crate::body::DecodedBody::Missing => crate::validation::BodyInput::Missing,
        crate::body::DecodedBody::Json(document) => crate::validation::BodyInput::Json(document),
        crate::body::DecodedBody::RawBytes => crate::validation::BodyInput::RawBytes,
    }
}

fn query_node(value: Option<&str>) -> cannery_core::json::Node {
    value.map_or(cannery_core::json::Node::Null, |value| {
        cannery_core::json::Node::String(String::from(value))
    })
}

async fn path_parameters(
    parts: &mut axum::http::request::Parts,
    state: &AppState,
) -> Result<std::collections::BTreeMap<String, String>, RouteFailure> {
    use axum::extract::{FromRequestParts, Path};
    Path::<std::collections::BTreeMap<String, String>>::from_request_parts(parts, state)
        .await
        .map(|Path(values)| values)
        .map_err(RouteFailure::new)
}

#[utoipa::path(
    get,
    path = "/api/projects",
    operation_id = "list_projects_api_projects_get",
    summary = "List Projects",
    description = "The projects the caller can read, by slug.",
    params(("before" = Option<String>, Query, description = "Continue after this project slug."),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_ProjectOut_str_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn list_projects(
    State(state): State<AppState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<axum::response::Response, RouteFailure> {
    use crate::validation::{Parameter, ParameterValue, validate_parameter_node};
    let (parts, _) = request.into_parts();
    let mut auth = authenticate(&state, &context, &parts.headers, &parts.method)
        .await
        .map_err(RouteFailure::new)?;
    let query =
        crate::request_context::QueryParams::parse(parts.uri.query().unwrap_or("").as_bytes());
    let mut errors = None;
    let before = collect(
        validate_parameter_node(&query_node(query.get("before")), Parameter::BeforeString),
        &mut errors,
    );
    let limit = collect(
        validate_parameter_node(
            &query_node(Some(query.get("limit").unwrap_or("50"))),
            Parameter::Limit,
        ),
        &mut errors,
    );
    validation_failure(errors, &context).map_err(RouteFailure::new)?;
    let (Some(ParameterValue::BeforeString(before)), Some(ParameterValue::Limit(limit))) =
        (before, limit)
    else {
        return Err(RouteFailure::new(context.internal("project query type")));
    };
    let before = before
        .map(|value| {
            value
                .as_utf8()
                .ok_or_else(|| context.internal("project cursor encoding"))
        })
        .transpose()
        .map_err(RouteFailure::new)?;
    project_page(
        &mut auth.connection,
        &auth.principal,
        &context,
        before.as_deref(),
        limit,
    )
    .await
    .map(|body| Json(body).into_response())
    .map_err(RouteFailure::new)
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}",
    operation_id = "get_project_api_projects__slug__get",
    summary = "Get Project",
    params(("slug" = String, Path)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::ProjectOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn get_project(
    State(state): State<AppState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<axum::response::Response, RouteFailure> {
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&state, &context, &parts.headers, &parts.method)
        .await
        .map_err(RouteFailure::new)?;
    let paths = path_parameters(&mut parts, &state).await?;
    let slug = paths
        .get("slug")
        .ok_or_else(|| RouteFailure::new(context.internal("project path missing")))?;
    let access = authz::project_access(
        &mut auth.connection,
        &auth.principal,
        slug,
        Some(Role::Viewer),
        &authz::ALL_SERVICES,
        false,
    )
    .await
    .map_err(|error| RouteFailure::new(context.project_error(error)))?;
    Ok(Json(project_out(&access.project, access.role)).into_response())
}

#[utoipa::path(
    post,
    path = "/api/projects",
    operation_id = "create_project_api_projects_post",
    summary = "Create Project",
    request_body(content = crate::api_models::ProjectCreate, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::ProjectOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn create_project(
    State(state): State<AppState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<axum::response::Response, RouteFailure> {
    let (parts, body) = crate::body::read_body(request)
        .await
        .map_err(RouteFailure::new)?;
    let mut auth = authenticate(&state, &context, &parts.headers, &parts.method)
        .await
        .map_err(RouteFailure::new)?;
    let validated = crate::validation::validate_body_input(
        body_input(&body),
        crate::validation::BodyModel::ProjectCreate,
    )
    .map_err(|error| {
        error.domain_error().map_or_else(
            |_| RouteFailure::new(context.internal("validation location encoding")),
            |error| RouteFailure::new(ApiError::from(error)),
        )
    })?;
    let crate::validation::Body::ProjectCreate(_) = validated else {
        return Err(RouteFailure::new(context.internal("project body type")));
    };
    let typed: crate::api_models::ProjectCreate =
        crate::body::typed(&body).map_err(RouteFailure::new)?;
    create(
        &mut auth.connection,
        &auth.principal,
        &context,
        &typed.slug,
        typed.title.trim(),
        typed.description.as_deref().unwrap_or(""),
    )
    .await
    .map(|body| (StatusCode::CREATED, Json(body)).into_response())
    .map_err(RouteFailure::new)
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/members",
    operation_id = "list_members_api_projects__slug__members_get",
    summary = "List Members",
    description = "Members by name.",
    params(("slug" = String, Path),
        ("before" = Option<String>, Query, description = "Continue after this member's user id.", format = "uuid"),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_MemberOut_UUID_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn list_members(
    State(state): State<AppState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<axum::response::Response, RouteFailure> {
    use crate::validation::{Parameter, ParameterValue, validate_parameter_node};
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&state, &context, &parts.headers, &parts.method)
        .await
        .map_err(RouteFailure::new)?;
    let paths = path_parameters(&mut parts, &state).await?;
    let slug = paths
        .get("slug")
        .ok_or_else(|| RouteFailure::new(context.internal("member path missing")))?;
    let query =
        crate::request_context::QueryParams::parse(parts.uri.query().unwrap_or("").as_bytes());
    let mut errors = None;
    let before = collect(
        validate_parameter_node(&query_node(query.get("before")), Parameter::BeforeUuid),
        &mut errors,
    );
    let limit = collect(
        validate_parameter_node(
            &query_node(Some(query.get("limit").unwrap_or("50"))),
            Parameter::Limit,
        ),
        &mut errors,
    );
    validation_failure(errors, &context).map_err(RouteFailure::new)?;
    let (Some(ParameterValue::BeforeUuid(before)), Some(ParameterValue::Limit(limit))) =
        (before, limit)
    else {
        return Err(RouteFailure::new(context.internal("member query type")));
    };
    member_page(
        &mut auth.connection,
        &auth.principal,
        &context,
        slug,
        before.map(cannery_core::ids::UserId),
        limit,
    )
    .await
    .map(|body| Json(body).into_response())
    .map_err(RouteFailure::new)
}

#[utoipa::path(
    put,
    path = "/api/projects/{slug}/members/{user_id}",
    operation_id = "set_member_api_projects__slug__members__user_id__put",
    summary = "Set Member",
    params(("slug" = String, Path),
        ("user_id" = String, Path, format = "uuid")),
    request_body(content = crate::api_models::MembershipSet, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = crate::api_models::MemberOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn set_member(
    State(state): State<AppState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<axum::response::Response, RouteFailure> {
    use crate::validation::{Parameter, ParameterValue, validate_parameter_node};
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(RouteFailure::new)?;
    let mut auth = authenticate(&state, &context, &parts.headers, &parts.method)
        .await
        .map_err(RouteFailure::new)?;
    let paths = path_parameters(&mut parts, &state).await?;
    let slug = paths
        .get("slug")
        .ok_or_else(|| RouteFailure::new(context.internal("member path missing")))?;
    let mut errors = None;
    let user = collect(
        validate_parameter_node(
            &query_node(paths.get("user_id").map(String::as_str)),
            Parameter::UserId,
        ),
        &mut errors,
    );
    let typed: crate::api_models::MembershipSet =
        crate::body::typed(&body).map_err(RouteFailure::new)?;
    let body = collect(
        crate::validation::validate_body_input(
            body_input(&body),
            crate::validation::BodyModel::MembershipSet,
        ),
        &mut errors,
    );
    validation_failure(errors, &context).map_err(RouteFailure::new)?;
    let (Some(ParameterValue::UserId(user)), Some(crate::validation::Body::MembershipSet(_))) =
        (user, body)
    else {
        return Err(RouteFailure::new(
            context.internal("member validation type"),
        ));
    };
    change_member(
        &mut auth.connection,
        &auth.principal,
        &context,
        slug,
        user,
        Some(match typed.role.as_str() {
            "viewer" => Role::Viewer,
            "member" => Role::Member,
            "researcher" => Role::Researcher,
            _ => {
                return Err(RouteFailure::new(
                    context.internal("validated membership role"),
                ));
            }
        }),
    )
    .await
    .map(|body| Json(body).into_response())
    .map_err(RouteFailure::new)
}

#[utoipa::path(
    delete,
    path = "/api/projects/{slug}/members/{user_id}",
    operation_id = "remove_member_api_projects__slug__members__user_id__delete",
    summary = "Remove Member",
    params(("slug" = String, Path),
        ("user_id" = String, Path, format = "uuid")),
    responses((status = 204, description = "Successful Response"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn remove_member(
    State(state): State<AppState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<axum::response::Response, RouteFailure> {
    use crate::validation::{Parameter, ParameterValue, validate_parameter_node};
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&state, &context, &parts.headers, &parts.method)
        .await
        .map_err(RouteFailure::new)?;
    let paths = path_parameters(&mut parts, &state).await?;
    let slug = paths
        .get("slug")
        .ok_or_else(|| RouteFailure::new(context.internal("member path missing")))?;
    let user = validate_parameter_node(
        &query_node(paths.get("user_id").map(String::as_str)),
        Parameter::UserId,
    )
    .map_err(|error| {
        error.domain_error().map_or_else(
            |_| RouteFailure::new(context.internal("validation location encoding")),
            |error| RouteFailure::new(ApiError::from(error)),
        )
    })?;
    let ParameterValue::UserId(user) = user else {
        return Err(RouteFailure::new(context.internal("member user type")));
    };
    change_member(
        &mut auth.connection,
        &auth.principal,
        &context,
        slug,
        user,
        None,
    )
    .await
    .map(|_| StatusCode::NO_CONTENT.into_response())
    .map_err(RouteFailure::new)
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
