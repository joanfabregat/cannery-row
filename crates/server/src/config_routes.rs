//! Append-only configuration publication and reads, with explicit source contexts.
use crate::{AppState, authentication::authenticate, errors::ApiError, requests::RequestContext};
use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use cannery_core::{
    audit::{self, Attribution, Record},
    contracts::{ContractKind, ContractValidator},
    errors::{DomainError, ErrorCode},
    json::{Document, DocumentBuilder, Node},
};
use cannery_projects::{authz, repo as projects};
use cannery_research::{
    config_repo::{self as repo, ConfigRevision, JsonContext, Kind},
    science::{self, RenderingContext, ScienceError},
    steps,
};
use serde_json::json;
use sqlx::Acquire;
use std::{collections::BTreeMap, sync::Arc};

/// Separate operation contexts; none is inferred from an arbitrary universal limit.
pub struct ConfigContext {
    pub contracts: ContractValidator,
    pub validation_walk_budget: usize,
    pub repr_budget: usize,

    /// Required measured embedded-schema operation profile; no implicit default.
    pub rendering: RenderingContext,
    pub repository: JsonContext,
    pub response: crate::config_wire::ResponseContext,
}
#[derive(Clone)]
pub(crate) struct ConfigState {
    app: AppState,
    context: Arc<ConfigContext>,
}
pub(crate) struct Failure(Box<Response>);
impl Failure {
    fn new(error: impl IntoResponse) -> Self {
        Self(Box::new(error.into_response()))
    }
}
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        *self.0
    }
}

/// Install the four HTTP operations with caller-owned, measured source profiles.
pub fn routes(app: AppState, context: Arc<ConfigContext>) -> Router {
    Router::new()
        .route(
            "/api/projects/{slug}/config/{kind}",
            get(list).post(create).head(head_post),
        )
        .route(
            "/api/projects/{slug}/config/{kind}/latest",
            get(latest).head(head_get),
        )
        .route(
            "/api/projects/{slug}/config/{kind}/{revision}",
            get(specific).head(head_get),
        )
        .with_state(ConfigState { app, context })
}
async fn head_post() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "POST")],
        axum::Json(json!({"detail":"Method Not Allowed"})),
    )
}
async fn head_get() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "GET")],
        axum::Json(json!({"detail":"Method Not Allowed"})),
    )
}
fn internal(context: &RequestContext, operation: &'static str) -> Failure {
    Failure::new(context.internal(operation))
}
fn not_found() -> Failure {
    Failure::new(ApiError::from(DomainError::new(
        ErrorCode::NotFound,
        "configuration revision not found",
    )))
}
fn science_error(context: &RequestContext, error: ScienceError) -> Failure {
    match error {
        ScienceError::Validation { path, message } => match path.as_utf8() {
            Some(path) => Failure::new(ApiError::from(
                DomainError::new(ErrorCode::ValidationFailed, message)
                    .with_details(json!([{"path":path,"message":message}])),
            )),
            None => internal(context, "configuration semantic path encoding"),
        },
        _ => internal(context, "configuration semantics"),
    }
}
fn validate(
    document: &Document,
    kind: Kind,
    profile: &ConfigContext,
    context: &RequestContext,
) -> Result<(), Failure> {
    // Source _non_finite walks all nodes before iter_errors or embedded checks.
    let mut pending = vec![(document.root(), 0usize)];
    while let Some((id, depth)) = pending.pop() {
        if depth >= profile.validation_walk_budget {
            return Err(internal(context, "configuration validation recursion"));
        }
        match document
            .node(id)
            .ok_or_else(|| internal(context, "configuration validation node"))?
        {
            Node::Array(values) => pending.extend(values.iter().rev().map(|id| (*id, depth + 1))),
            Node::Object(values) => {
                pending.extend(values.iter().rev().map(|(_, id)| (*id, depth + 1)));
            }
            _ => {}
        }
    }
    let kind = match kind {
        Kind::Science => ContractKind::ScienceRevision,
        Kind::Dashboard => ContractKind::DashboardViews,
    };
    let violations = profile
        .contracts
        .document_violations(kind, document)
        .map_err(|_| internal(context, "configuration contract validation"))?;
    if violations.is_empty() {
        return Ok(());
    }
    let details = violations
        .into_iter()
        .map(|v| {
            v.path
                .as_utf8()
                .map(|path| json!({"path":path,"message":v.message}))
                .ok_or_else(|| internal(context, "configuration violation encoding"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Err(Failure::new(ApiError::from(
        DomainError::new(
            ErrorCode::ValidationFailed,
            "invalid configuration document",
        )
        .with_details(json!(details)),
    )))
}
fn encoded(
    row: &ConfigRevision,
    profile: &ConfigContext,
    context: &RequestContext,
    status: StatusCode,
) -> Result<Response, Failure> {
    let bytes = crate::config_wire::config_bytes(row, profile.response)
        .map_err(|_| internal(context, "configuration response serialization"))?;
    Ok((status, [(header::CONTENT_TYPE, "application/json")], bytes).into_response())
}
#[allow(
    clippy::too_many_lines,
    reason = "Keep validation, locking, revision and audit transaction ordering together"
)]
async fn publish(
    connection: &mut sqlx::PgConnection,
    principal: &cannery_core::principal::Principal,
    slug: &str,
    kind: Kind,
    document: &Document,
    profile: &ConfigContext,
    context: &RequestContext,
) -> Result<ConfigRevision, Failure> {
    let admin = principal
        .require_admin(true)
        .map_err(|e| Failure::new(ApiError::from(e)))?;
    validate(document, kind, profile, context)?;
    let typed_document =
        crate::api_models::request_document::<crate::api_models::ConfigRevisionRequest>(document)
            .map_err(Failure::new)?;
    let document = &typed_document;
    let project = projects::get_project_by_slug(connection, slug)
        .await
        .map_err(|e| Failure::new(context.project_error(e)))?
        .ok_or_else(not_found)?;
    let mut transaction = connection
        .begin()
        .await
        .map_err(|_| internal(context, "configuration transaction"))?;
    let _locked = projects::lock_project(&mut transaction, project.id)
        .await
        .map_err(|e| Failure::new(context.project_error(e)))?;
    let science_revision = match kind {
        Kind::Science => {
            let science = science::check_science(document, profile.rendering)
                .map_err(|e| science_error(context, e))?;
            let scorer = science.scorer().map_err(|e| science_error(context, e))?;
            let mut builder = DocumentBuilder::new();
            let root = builder
                .import(document, scorer)
                .map_err(|_| internal(context, "configuration scorer projection"))?;
            let scorer = builder
                .finish(root)
                .map_err(|_| internal(context, "configuration scorer projection"))?;
            steps::check_step(
                &science,
                &scorer,
                steps::Role::Scorer,
                &String::from("/scorer"),
                profile.rendering,
            )
            .map_err(|e| science_error(context, e))?;
            steps::check_validators(&science, profile.rendering)
                .map_err(|e| science_error(context, e))?;
            None
        }
        Kind::Dashboard => {
            let current = repo::get_revision(
                &mut transaction,
                project.id,
                Kind::Science,
                None,
                profile.repository,
            )
            .await
            .map_err(|_| internal(context, "configuration science load"))?
            .ok_or_else(|| {
                Failure::new(ApiError::from(DomainError::new(
                    ErrorCode::Conflict,
                    "create a science revision before a dashboard revision",
                )))
            })?;
            let science =
                science::Science::new(current.revision.into(), &current.content, profile.rendering)
                    .map_err(|e| science_error(context, e))?;
            science::check_dashboard(&science, document, profile.rendering)
                .map_err(|e| science_error(context, e))?;
            Some(current.revision)
        }
    };
    let science_number = science_revision.map(Into::into);
    let row = repo::create_revision(
        &mut transaction,
        project.id,
        kind,
        document,
        science_number.as_ref(),
        admin.user_id,
        profile.repository,
    )
    .await
    .map_err(|_| internal(context, "configuration append"))?;
    let subject = format!("{}:{}", project.id, row.revision);
    let new_state = json!({"revision":row.revision,"science_revision":science_revision});
    audit::record(
        &mut transaction,
        Attribution::Principal(principal),
        Record {
            action: match kind {
                Kind::Science => "config.science_created",
                Kind::Dashboard => "config.dashboard_created",
            },
            subject_type: match kind {
                Kind::Science => "science_revision",
                Kind::Dashboard => "dashboard_revision",
            },
            subject_id: &subject,
            project_id: Some(project.id),
            prior_state: None,
            new_state: Some(&new_state),
            reason: None,
            idempotency_key: None,
        },
    )
    .await
    .map_err(|_| internal(context, "configuration audit"))?;
    transaction
        .commit()
        .await
        .map_err(|_| internal(context, "configuration commit"))?;
    Ok(row)
}
async fn paths(
    parts: &mut axum::http::request::Parts,
    state: &ConfigState,
) -> Result<BTreeMap<String, String>, Failure> {
    use axum::extract::{FromRequestParts, Path};
    Path::<BTreeMap<String, String>>::from_request_parts(parts, state)
        .await
        .map(|Path(values)| values)
        .map_err(Failure::new)
}
fn node(value: Option<&str>) -> Node {
    value.map_or(Node::Null, |v| Node::String(String::from(v)))
}
fn collect<T>(
    result: Result<T, crate::validation::ValidationErrors>,
    errors: &mut Option<crate::validation::ValidationErrors>,
) -> Option<T> {
    match result {
        Ok(v) => Some(v),
        Err(e) => {
            if let Some(errors) = errors {
                errors.append(e);
            } else {
                *errors = Some(e);
            }
            None
        }
    }
}
fn errors(
    errors: Option<crate::validation::ValidationErrors>,
    context: &RequestContext,
) -> Result<(), Failure> {
    if let Some(e) = errors {
        return Err(Failure::new(ApiError::from(e.domain_error().map_err(
            |_| internal(context, "configuration model error encoding"),
        )?)));
    }
    Ok(())
}
fn kind(
    value: Option<&crate::validation::ParameterValue>,
    context: &RequestContext,
) -> Result<Kind, Failure> {
    match value {
        Some(crate::validation::ParameterValue::ConfigKind("science")) => Ok(Kind::Science),
        Some(crate::validation::ParameterValue::ConfigKind("dashboard")) => Ok(Kind::Dashboard),
        _ => Err(internal(context, "configuration kind")),
    }
}
#[utoipa::path(
    post,
    path = "/api/projects/{slug}/config/{kind}",
    operation_id = "create_revision_api_projects__slug__config__kind__post",
    summary = "Create Revision",
    params(("slug" = String, Path),
        ("kind" = crate::api_models::cannery_row__config__routes__Kind, Path, description = "`science` or `dashboard`")),
    request_body(content = crate::api_models::ConfigRevisionRequest, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::ConfigOut, content_type = "application/json"),
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
    State(state): State<ConfigState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    use crate::validation::{
        BodyInput, Parameter, validate_document_body, validate_parameter_node,
    };
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let paths = paths(&mut parts, &state).await?;
    let mut problems = None;
    let selected = collect(
        validate_parameter_node(
            &node(paths.get("kind").map(String::as_str)),
            Parameter::ConfigKind,
        ),
        &mut problems,
    );
    let input = match &body {
        crate::body::DecodedBody::Missing => BodyInput::Missing,
        crate::body::DecodedBody::RawBytes => BodyInput::RawBytes,
        crate::body::DecodedBody::Json(document) => BodyInput::Json(document),
    };
    let document = collect(validate_document_body(input), &mut problems);
    errors(problems, &context)?;
    let kind = kind(selected.as_ref(), &context)?;
    let slug = paths
        .get("slug")
        .ok_or_else(|| internal(&context, "configuration slug"))?;
    let document = document.ok_or_else(|| internal(&context, "configuration body"))?;
    let row = publish(
        &mut auth.connection,
        &auth.principal,
        slug,
        kind,
        document,
        &state.context,
        &context,
    )
    .await?;
    encoded(&row, &state.context, &context, StatusCode::CREATED)
}
async fn read(
    State(state): State<ConfigState>,
    context: RequestContext,
    request: Request,
    specific: bool,
    page: bool,
) -> Result<Response, Failure> {
    use crate::validation::{Parameter, ParameterValue, validate_parameter_node};
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let paths = paths(&mut parts, &state).await?;
    let query =
        crate::request_context::QueryParams::parse(parts.uri.query().unwrap_or("").as_bytes());
    let mut problems = None;
    let selected = collect(
        validate_parameter_node(
            &node(paths.get("kind").map(String::as_str)),
            Parameter::ConfigKind,
        ),
        &mut problems,
    );
    let revision = if specific {
        collect(
            validate_parameter_node(
                &node(paths.get("revision").map(String::as_str)),
                Parameter::ConfigRevision,
            ),
            &mut problems,
        )
    } else {
        None
    };
    let before = if page {
        collect(
            validate_parameter_node(&node(query.get("before")), Parameter::ConfigBefore),
            &mut problems,
        )
    } else {
        None
    };
    let limit = if page {
        collect(
            validate_parameter_node(
                &node(Some(query.get("limit").unwrap_or("50"))),
                Parameter::Limit,
            ),
            &mut problems,
        )
    } else {
        None
    };
    errors(problems, &context)?;
    let kind = kind(selected.as_ref(), &context)?;
    let slug = paths
        .get("slug")
        .ok_or_else(|| internal(&context, "configuration slug"))?;
    let project = authz::project_read(&mut auth.connection, &auth.principal, slug)
        .await
        .map_err(|e| Failure::new(context.project_error(e)))?;
    if page {
        let (Some(ParameterValue::ConfigBefore(before)), Some(ParameterValue::Limit(limit))) =
            (before, limit)
        else {
            return Err(internal(&context, "configuration page model"));
        };
        let mut rows = repo::list_revisions(
            &mut auth.connection,
            project.id,
            kind,
            before.as_ref(),
            Some(&(limit + 1).into()),
            state.context.repository,
        )
        .await
        .map_err(|_| internal(&context, "configuration page"))?;
        let next = if rows.len() > limit {
            rows.truncate(limit);
            rows.last().map(|r| r.revision)
        } else {
            None
        };
        let bytes = crate::config_wire::page_bytes(&rows, next, state.context.response)
            .map_err(|_| internal(&context, "configuration page response"))?;
        return Ok(([(header::CONTENT_TYPE, "application/json")], bytes).into_response());
    }
    let revision = match revision {
        Some(ParameterValue::ConfigRevision(value)) => Some(value),
        None => None,
        _ => return Err(internal(&context, "configuration revision model")),
    };
    let row = repo::get_revision(
        &mut auth.connection,
        project.id,
        kind,
        revision.as_ref(),
        state.context.repository,
    )
    .await
    .map_err(|_| internal(&context, "configuration read"))?
    .ok_or_else(not_found)?;
    encoded(&row, &state.context, &context, StatusCode::OK)
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/config/{kind}",
    operation_id = "list_revisions_api_projects__slug__config__kind__get",
    summary = "List Revisions",
    description = "Revisions of one kind, newest first.",
    params(("slug" = String, Path),
        ("kind" = crate::api_models::cannery_row__config__routes__Kind, Path, description = "`science` or `dashboard`"),
        ("before" = Option<i64>, Query, description = "Continue with older revisions.", minimum = 1, maximum = 2_147_483_647),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_ConfigOut_int_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn list(
    state: State<ConfigState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    read(state, context, request, false, true).await
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/config/{kind}/latest",
    operation_id = "latest_revision_api_projects__slug__config__kind__latest_get",
    summary = "Latest Revision",
    params(("slug" = String, Path),
        ("kind" = crate::api_models::cannery_row__config__routes__Kind, Path, description = "`science` or `dashboard`")),
    responses((status = 200, description = "Successful Response", body = crate::api_models::ConfigOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn latest(
    state: State<ConfigState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    read(state, context, request, false, false).await
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/config/{kind}/{revision}",
    operation_id = "get_revision_api_projects__slug__config__kind___revision__get",
    summary = "Get Revision",
    params(("slug" = String, Path),
        ("kind" = crate::api_models::cannery_row__config__routes__Kind, Path, description = "`science` or `dashboard`"),
        ("revision" = i64, Path)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::ConfigOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn specific(
    state: State<ConfigState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    read(state, context, request, true, false).await
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
