//! Source track HTTP operations with explicit operation contexts and caller transactions.
use crate::{AppState, authentication::authenticate, errors::ApiError, requests::RequestContext};
use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use cannery_core::{
    contracts::{ContractKind, ContractValidator},
    errors::{DomainError, ErrorCode},
    json::{Document, DocumentBuilder, Node},
    principal::{Principal, Role},
};
use cannery_projects::{authz, repo as projects};
use cannery_research::{
    config_repo,
    science::{RenderingContext, Science, ScienceError},
};
use cannery_tracks::repo::{self, JsonContext, RowLock, Track, TrackMode, TrackState};
use num_bigint::BigInt;
use sqlx::Acquire;
use std::{collections::BTreeMap, sync::Arc};
/// Contexts are separately supplied; production profile binding belongs to the caller.
pub struct TrackContext {
    pub contracts: ContractValidator,
    pub validation_walk_budget: usize,
    pub repr_budget: usize,

    pub rendering: RenderingContext,
    pub repository: JsonContext,
    pub config_repository: config_repo::JsonContext,
    pub audit_encode_budget: usize,
    pub audit_decode_budget: usize,
    pub response: crate::track_wire::ResponseContext,
}
#[derive(Clone)]
pub(crate) struct RouteState {
    app: AppState,
    context: Arc<TrackContext>,
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
fn internal(c: &RequestContext, op: &'static str) -> Failure {
    Failure::new(c.internal(op))
}
fn domain(code: ErrorCode, message: impl Into<String>) -> Failure {
    Failure::new(ApiError::from(DomainError::new(code, message)))
}
fn violation(path: &str, message: &str) -> Failure {
    Failure::new(ApiError::from(
        DomainError::new(ErrorCode::ValidationFailed, "invalid track document")
            .with_details(serde_json::json!([{"path":path,"message":message}])),
    ))
}
fn science_error(c: &RequestContext, error: crate::track_binding::Error) -> Failure {
    match error {
        crate::track_binding::Error::Science(ScienceError::Validation { path, message }) => {
            path.as_utf8().map_or_else(
                || internal(c, "track semantic path"),
                |path| violation(&path, message),
            )
        }
        _ => internal(c, "track binding"),
    }
}
/// Install all six operations with an explicitly supplied context.
pub fn routes(app: AppState, context: Arc<TrackContext>) -> Router {
    Router::new()
        .route(
            "/api/projects/{slug}/tracks",
            post(create).get(list).head(head_post),
        )
        .route(
            "/api/projects/{slug}/tracks/{track_slug}",
            get(read).patch(update).head(head_get),
        )
        .route(
            "/api/projects/{slug}/tracks/{track_slug}/transitions",
            post(transition),
        )
        .route(
            "/api/projects/{slug}/tracks/{track_slug}/history",
            get(history).head(head_get),
        )
        .with_state(RouteState { app, context })
}
async fn head_post() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "POST")],
        axum::Json(serde_json::json!({"detail":"Method Not Allowed"})),
    )
}
async fn head_get() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "GET")],
        axum::Json(serde_json::json!({"detail":"Method Not Allowed"})),
    )
}
async fn paths(
    parts: &mut axum::http::request::Parts,
    state: &RouteState,
) -> Result<BTreeMap<String, String>, Failure> {
    use axum::extract::{FromRequestParts, Path};
    Path::<BTreeMap<String, String>>::from_request_parts(parts, state)
        .await
        .map(|Path(p)| p)
        .map_err(Failure::new)
}
fn path<'a>(
    p: &'a BTreeMap<String, String>,
    name: &str,
    c: &RequestContext,
) -> Result<&'a str, Failure> {
    p.get(name)
        .map(String::as_str)
        .ok_or_else(|| internal(c, "track path"))
}
fn input(body: &crate::body::DecodedBody) -> crate::validation::BodyInput<'_> {
    match body {
        crate::body::DecodedBody::Missing => crate::validation::BodyInput::Missing,
        crate::body::DecodedBody::RawBytes => crate::validation::BodyInput::RawBytes,
        crate::body::DecodedBody::Json(d) => crate::validation::BodyInput::Json(d),
    }
}
fn model_error(error: &crate::validation::ValidationErrors, c: &RequestContext) -> Failure {
    error.domain_error().map_or_else(
        |_| internal(c, "track request error encoding"),
        |e| Failure::new(ApiError::from(e)),
    )
}
async fn researcher(
    conn: &mut sqlx::PgConnection,
    principal: &Principal,
    slug: &str,
    c: &RequestContext,
) -> Result<projects::Project, Failure> {
    principal
        .require_user()
        .map_err(|e| Failure::new(ApiError::from(e)))?;
    authz::project_access(conn, principal, slug, Some(Role::Researcher), &[], true)
        .await
        .map(|a| a.project)
        .map_err(|e| Failure::new(c.project_error(e)))
}
async fn track(
    conn: &mut sqlx::PgConnection,
    project: cannery_core::ids::ProjectId,
    slug: &str,
    lock: Option<RowLock>,
    profile: &TrackContext,
    c: &RequestContext,
) -> Result<Track, Failure> {
    let mut row = repo::get_track(conn, project, &String::from(slug), lock, profile.repository)
        .await
        .map_err(|_| internal(c, "track load"))?
        .ok_or_else(|| domain(ErrorCode::NotFound, "track not found"))?;
    // Psycopg decodes JSON null to Python None before route merging/identity checks.
    if row
        .producer
        .as_ref()
        .is_some_and(|d| matches!(d.node(d.root()), Some(Node::Null)))
    {
        row.producer = None;
    }
    if row
        .workflow
        .as_ref()
        .is_some_and(|d| matches!(d.node(d.root()), Some(Node::Null)))
    {
        row.workflow = None;
    }
    Ok(row)
}
fn encoded(
    row: &Track,
    profile: &TrackContext,
    c: &RequestContext,
    status: StatusCode,
) -> Result<Response, Failure> {
    let bytes = crate::track_wire::track_bytes(row, profile.response)
        .map_err(|_| internal(c, "track model serialization"))?;
    Ok((status, [(header::CONTENT_TYPE, "application/json")], bytes).into_response())
}
fn validate(
    d: &Document,
    kind: ContractKind,
    p: &TrackContext,
    c: &RequestContext,
) -> Result<(), Failure> {
    let mut stack = vec![(d.root(), 0)];
    while let Some((id, depth)) = stack.pop() {
        if depth >= p.validation_walk_budget {
            return Err(internal(c, "track validation recursion"));
        }
        match d.node(id) {
            Some(Node::Array(v)) => stack.extend(v.iter().rev().map(|id| (*id, depth + 1))),
            Some(Node::Object(v)) => stack.extend(v.iter().rev().map(|(_, id)| (*id, depth + 1))),
            Some(_) => {}
            None => return Err(internal(c, "track validation node")),
        }
    }
    let errors = p
        .contracts
        .document_violations(kind, d)
        .map_err(|_| internal(c, "track contract"))?;
    if errors.is_empty() {
        return Ok(());
    }
    let details = errors
        .into_iter()
        .map(|v| {
            v.path
                .as_utf8()
                .map(|path| serde_json::json!({"path":path,"message":v.message}))
                .ok_or_else(|| internal(c, "track contract path"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Err(Failure::new(ApiError::from(
        DomainError::new(ErrorCode::ValidationFailed, "invalid track document")
            .with_details(serde_json::json!(details)),
    )))
}
fn subdocument(d: &Document, key: &str, c: &RequestContext) -> Result<Option<Document>, Failure> {
    let Some(id) = d.field(d.root(), key) else {
        return Ok(None);
    };
    let mut b = DocumentBuilder::new();
    let root = b
        .import(d, id)
        .map_err(|_| internal(c, "track projection"))?;
    Ok(Some(
        b.finish(root)
            .map_err(|_| internal(c, "track projection"))?,
    ))
}
fn text(d: &Document, key: &str, c: &RequestContext) -> Result<String, Failure> {
    match d.field(d.root(), key).and_then(|id| d.node(id)) {
        Some(Node::String(v)) => Ok(v.clone()),
        _ => Err(internal(c, "track document text")),
    }
}
async fn binding(
    conn: &mut sqlx::PgConnection,
    project: cannery_core::ids::ProjectId,
    producer: Option<&Document>,
    workflow: Option<&Document>,
    p: &TrackContext,
    c: &RequestContext,
) -> Result<(), Failure> {
    let revision = config_repo::get_revision(
        conn,
        project,
        config_repo::Kind::Science,
        None,
        p.config_repository,
    )
    .await
    .map_err(|_| internal(c, "track science load"))?;
    let science = revision
        .as_ref()
        .map(|r| Science::new(r.revision.into(), &r.content, p.rendering))
        .transpose()
        .map_err(|e| science_error(c, e.into()))?;
    crate::track_binding::check(
        conn,
        project,
        science.as_ref(),
        producer,
        workflow,
        p.rendering,
        p.repository.decode_nesting_budget,
    )
    .await
    .map_err(|e| science_error(c, e))
}
const SNAPSHOT: &[&str] = &[
    "title",
    "description",
    "producer",
    "mode",
    "workflow",
    "state",
    "revision",
];
#[allow(
    clippy::too_many_arguments,
    reason = "Source audit attribution and transaction fields remain explicit"
)]
async fn audit(
    conn: &mut sqlx::PgConnection,
    principal: &Principal,
    project: cannery_core::ids::ProjectId,
    prior: Option<&Track>,
    new: &Track,
    action: &str,
    fields: &[&str],
    reason: Option<&str>,
    p: &TrackContext,
    c: &RequestContext,
) -> Result<(), Failure> {
    let user = principal
        .require_user()
        .map_err(|e| Failure::new(ApiError::from(e)))?;
    let prior = prior
        .map(|r| crate::track_wire::snapshot(r, fields))
        .transpose()
        .map_err(|_| internal(c, "track audit snapshot"))?;
    let new_state = crate::track_wire::snapshot(new, fields)
        .map_err(|_| internal(c, "track audit snapshot"))?;
    crate::track_audit::record(
        conn,
        user,
        project,
        &new.id.to_string(),
        action,
        prior.as_ref(),
        &new_state,
        reason,
        p.audit_encode_budget,
    )
    .await
    .map_err(|_| internal(c, "track audit"))
}
#[allow(
    clippy::many_single_char_names,
    reason = "Short route-local values are confined to ordered source operation"
)]
#[utoipa::path(
    post,
    path = "/api/projects/{slug}/tracks",
    operation_id = "create_track_api_projects__slug__tracks_post",
    summary = "Create Track",
    params(("slug" = String, Path)),
    request_body(content = crate::api_models::TrackCreateRequest, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::TrackOut, content_type = "application/json"),
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
    State(s): State<RouteState>,
    axum::Extension(c): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let mut auth = authenticate(&s.app, &c, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let p = paths(&mut parts, &s).await?;
    let d =
        crate::validation::validate_document_body(input(&body)).map_err(|e| model_error(&e, &c))?;
    let project = researcher(
        &mut auth.connection,
        &auth.principal,
        path(&p, "slug", &c)?,
        &c,
    )
    .await?;
    validate(d, ContractKind::Track, &s.context, &c)?;
    let typed_document =
        crate::api_models::request_document::<crate::api_models::TrackCreateRequest>(d)
            .map_err(Failure::new)?;
    let d = &typed_document;
    let producer = subdocument(d, "producer", &c)?;
    let workflow = subdocument(d, "workflow", &c)?;
    let slug = text(d, "slug", &c)?;
    let title = text(d, "title", &c)?;
    let description = if d.field(d.root(), "description").is_some() {
        text(d, "description", &c)?
    } else {
        String::new()
    };
    let mode = if let Some(id) = d.field(d.root(), "mode") {
        match d.node(id) {
            Some(Node::String(v)) if v.equals_utf8("workflow") => TrackMode::Workflow,
            _ => TrackMode::Agent,
        }
    } else {
        TrackMode::Agent
    };
    let user = auth
        .principal
        .require_user()
        .map_err(|e| Failure::new(ApiError::from(e)))?;
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&c, "track transaction"))?;
    projects::share_lock_project(&mut tx, project.id)
        .await
        .map_err(|e| Failure::new(c.project_error(e)))?;
    binding(
        &mut tx,
        project.id,
        producer.as_ref(),
        workflow.as_ref(),
        &s.context,
        &c,
    )
    .await?;
    let row = repo::create_track(
        &mut tx,
        repo::CreateTrack {
            project_id: project.id,
            slug: &slug,
            title: &title,
            description: &description,
            producer: producer.as_ref(),
            mode,
            workflow: workflow.as_ref(),
            created_by: user.user_id,
        },
        s.context.repository,
    )
    .await
    .map_err(|_| internal(&c, "track create"))?
    .ok_or_else(|| {
        domain(
            ErrorCode::Conflict,
            "a track with this slug already exists in the project",
        )
    })?;
    audit(
        &mut tx,
        &auth.principal,
        project.id,
        None,
        &row,
        "track.created",
        SNAPSHOT,
        None,
        &s.context,
        &c,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|_| internal(&c, "track commit"))?;
    encoded(&row, &s.context, &c, StatusCode::CREATED)
}
#[allow(
    clippy::too_many_lines,
    reason = "Preserve source lock, validation, update and audit failure order"
)]
#[utoipa::path(
    patch,
    path = "/api/projects/{slug}/tracks/{track_slug}",
    operation_id = "update_track_api_projects__slug__tracks__track_slug__patch",
    summary = "Update Track",
    params(("slug" = String, Path),
        ("track_slug" = String, Path)),
    request_body(content = crate::api_models::TrackUpdate, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = crate::api_models::TrackOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn update(
    State(s): State<RouteState>,
    axum::Extension(c): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let mut auth = authenticate(&s.app, &c, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let body = crate::api_contract::typed_body::<crate::api_models::TrackUpdate>(body)
        .map_err(|error| Failure(Box::new(error.into_response())))?;
    let p = paths(&mut parts, &s).await?;
    let body =
        crate::validation::validate_track_update(input(&body)).map_err(|e| model_error(&e, &c))?;
    let project = researcher(
        &mut auth.connection,
        &auth.principal,
        path(&p, "slug", &c)?,
        &c,
    )
    .await?;
    if body.changed.is_empty() {
        return Err(domain(ErrorCode::ValidationFailed, "nothing to change"));
    }
    let reason = body.reason.as_ref().and_then(|r| {
        let points = r.codepoints();
        let space = |p: &u32| cannery_core::text::whitespace(*p);
        let start = points
            .iter()
            .position(|p| !space(p))
            .unwrap_or(points.len());
        let end = points
            .iter()
            .rposition(|p| !space(p))
            .map_or(start, |i| i + 1);
        cannery_core::text::from_codepoints(points[start..end].to_vec())
    });
    let reason = reason.filter(|r| !r.codepoints().is_empty());
    let binding_changed = ["producer", "mode", "workflow"]
        .iter()
        .any(|v| body.changed.contains(*v));
    if binding_changed && reason.is_none() {
        return Err(domain(
            ErrorCode::ValidationFailed,
            "changing the producer binding, mode or workflow requires a reason",
        ));
    }
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&c, "track transaction"))?;
    let row = track(
        &mut tx,
        project.id,
        path(&p, "track_slug", &c)?,
        Some(RowLock::Update),
        &s.context,
        &c,
    )
    .await?;
    if BigInt::from(row.revision) != body.expected_revision {
        return Err(domain(
            ErrorCode::StaleRevision,
            format!("the track is at revision {}", row.revision),
        ));
    }
    if row.state == TrackState::Archived {
        return Err(domain(
            ErrorCode::Conflict,
            "an archived track is read-only; reactivate it first",
        ));
    }
    let title = if body.changed.contains("title") {
        body.title.as_ref()
    } else {
        Some(&row.title)
    };
    let description = if body.changed.contains("description") {
        body.description.as_ref()
    } else {
        Some(&row.description)
    };
    let producer = if body.changed.contains("producer") {
        body.producer.as_ref()
    } else {
        row.producer.as_ref()
    };
    let mode = body.mode.unwrap_or(row.mode);
    let workflow = if mode == TrackMode::Agent && !body.changed.contains("workflow") {
        None
    } else if body.changed.contains("workflow") {
        body.workflow.as_ref()
    } else {
        row.workflow.as_ref()
    };
    let mut b = DocumentBuilder::new();
    let mut fields = vec![];
    for (key, value) in [
        ("slug", Some(&row.slug)),
        ("title", title),
        (
            "description",
            description.filter(|v| !v.codepoints().is_empty()),
        ),
    ] {
        if key == "description" && value.is_none() {
            continue;
        }
        let id = b
            .push(value.map_or(Node::Null, |v| Node::String(v.clone())))
            .map_err(|_| internal(&c, "track merge"))?;
        fields.push((String::from(key), id));
    }
    for (key, value) in [("producer", producer), ("workflow", workflow)] {
        if let Some(d) = value {
            let id = b
                .import(d, d.root())
                .map_err(|_| internal(&c, "track merge"))?;
            fields.push((String::from(key), id));
        }
    }
    let id = b
        .push(Node::String(String::from(mode.as_str())))
        .map_err(|_| internal(&c, "track merge"))?;
    fields.push((String::from("mode"), id));
    let root = b
        .push(Node::Object(fields))
        .map_err(|_| internal(&c, "track merge"))?;
    let merged = b.finish(root).map_err(|_| internal(&c, "track merge"))?;
    validate(&merged, ContractKind::Track, &s.context, &c)?;
    if binding_changed {
        projects::share_lock_project(&mut tx, project.id)
            .await
            .map_err(|e| Failure::new(c.project_error(e)))?;
        binding(&mut tx, project.id, producer, workflow, &s.context, &c).await?;
    }
    let title = text(&merged, "title", &c)?;
    let description = description.cloned().unwrap_or_else(String::new);
    let expected = row.revision.into();
    let updated = repo::update_track(
        &mut tx,
        row.id,
        repo::UpdateTrack {
            expected_revision: &expected,
            title: &title,
            description: &description,
            producer,
            state: row.state,
            mode,
            workflow,
        },
        s.context.repository,
    )
    .await
    .map_err(|_| internal(&c, "track update"))?
    .ok_or_else(|| internal(&c, "locked track missing"))?;
    let reason = reason
        .as_ref()
        .map(|v| {
            v.as_utf8()
                .ok_or_else(|| internal(&c, "track reason encoding"))
        })
        .transpose()?;
    audit(
        &mut tx,
        &auth.principal,
        project.id,
        Some(&row),
        &updated,
        "track.updated",
        SNAPSHOT,
        reason.as_deref(),
        &s.context,
        &c,
    )
    .await?;
    if updated.mode != row.mode {
        audit(
            &mut tx,
            &auth.principal,
            project.id,
            Some(&row),
            &updated,
            "track.mode_changed",
            &["mode", "workflow"],
            reason.as_deref(),
            &s.context,
            &c,
        )
        .await?;
    }
    tx.commit()
        .await
        .map_err(|_| internal(&c, "track commit"))?;
    encoded(&updated, &s.context, &c, StatusCode::OK)
}
#[allow(
    clippy::too_many_lines,
    reason = "Preserve source revision and transition audit transaction ordering"
)]
#[utoipa::path(
    post,
    path = "/api/projects/{slug}/tracks/{track_slug}/transitions",
    operation_id = "transition_track_api_projects__slug__tracks__track_slug__transitions_post",
    summary = "Transition Track",
    params(("slug" = String, Path),
        ("track_slug" = String, Path)),
    request_body(content = crate::api_models::TrackTransitionRequest, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = crate::api_models::TrackOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn transition(
    State(s): State<RouteState>,
    axum::Extension(c): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let mut auth = authenticate(&s.app, &c, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let p = paths(&mut parts, &s).await?;
    let d =
        crate::validation::validate_document_body(input(&body)).map_err(|e| model_error(&e, &c))?;
    let project = researcher(
        &mut auth.connection,
        &auth.principal,
        path(&p, "slug", &c)?,
        &c,
    )
    .await?;
    validate(d, ContractKind::TrackTransition, &s.context, &c)?;
    let typed_document =
        crate::api_models::request_document::<crate::api_models::TrackTransitionRequest>(d)
            .map_err(Failure::new)?;
    let d = &typed_document;
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&c, "track transaction"))?;
    let row = track(
        &mut tx,
        project.id,
        path(&p, "track_slug", &c)?,
        Some(RowLock::Update),
        &s.context,
        &c,
    )
    .await?;
    let expected = cannery_research::science::configuration_integer(
        d,
        d.field(d.root(), "expected_revision")
            .ok_or_else(|| internal(&c, "track expected revision"))?,
    )
    .map_err(|_| internal(&c, "track expected revision"))?;
    if BigInt::from(row.revision) != expected {
        return Err(domain(
            ErrorCode::StaleRevision,
            format!("the track is at revision {}", row.revision),
        ));
    }
    let state = text(d, "to_state", &c)?
        .as_utf8()
        .ok_or_else(|| internal(&c, "track state encoding"))?;
    let state =
        TrackState::try_from(state.as_str()).map_err(|_| internal(&c, "track state model"))?;
    let allowed = match row.state {
        TrackState::Active => matches!(state, TrackState::Paused | TrackState::Archived),
        TrackState::Paused => matches!(state, TrackState::Active | TrackState::Archived),
        TrackState::Archived => state == TrackState::Active,
    };
    if !allowed {
        return Err(domain(
            ErrorCode::Conflict,
            format!(
                "a track cannot go from {} to {}",
                row.state.as_str(),
                state.as_str()
            ),
        ));
    }
    if state == TrackState::Archived {
        let count = repo::count_open_hypotheses(&mut tx, row.id)
            .await
            .map_err(|_| internal(&c, "track open hypotheses"))?;
        if count > 0 {
            return Err(domain(
                ErrorCode::Conflict,
                format!(
                    "the track still has {count} draft, queued, active or awaiting-review hypotheses"
                ),
            ));
        }
    }
    let updated = repo::update_track(
        &mut tx,
        row.id,
        repo::UpdateTrack {
            expected_revision: &expected,
            title: &row.title,
            description: &row.description,
            producer: row.producer.as_ref(),
            state,
            mode: row.mode,
            workflow: row.workflow.as_ref(),
        },
        s.context.repository,
    )
    .await
    .map_err(|_| internal(&c, "track state update"))?
    .ok_or_else(|| internal(&c, "locked track missing"))?;
    let reason = text(d, "reason", &c)?
        .as_utf8()
        .ok_or_else(|| internal(&c, "track reason encoding"))?;
    audit(
        &mut tx,
        &auth.principal,
        project.id,
        Some(&row),
        &updated,
        "track.state_changed",
        &["state"],
        Some(&reason),
        &s.context,
        &c,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|_| internal(&c, "track commit"))?;
    encoded(&updated, &s.context, &c, StatusCode::OK)
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
fn node(value: Option<&str>) -> Node {
    value.map_or(Node::Null, |v| Node::String(String::from(v)))
}
#[allow(
    clippy::too_many_lines,
    reason = "Keep signature validation and shared source authorization order together"
)]
async fn reading(
    State(s): State<RouteState>,
    c: RequestContext,
    request: Request,
    page: bool,
    history: bool,
) -> Result<Response, Failure> {
    use crate::validation::{Parameter, ParameterValue, validate_parameter_node};
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&s.app, &c, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let p = paths(&mut parts, &s).await?;
    let q = crate::request_context::QueryParams::parse(parts.uri.query().unwrap_or("").as_bytes());
    let mut problems = None;
    let state = if page {
        collect(
            validate_parameter_node(&node(q.get("state")), Parameter::TrackState),
            &mut problems,
        )
    } else {
        None
    };
    let before = if page || history {
        collect(
            validate_parameter_node(
                &node(q.get("before")),
                if history {
                    Parameter::HistoryBefore
                } else {
                    Parameter::BeforeString
                },
            ),
            &mut problems,
        )
    } else {
        None
    };
    let limit = if page || history {
        collect(
            validate_parameter_node(
                &node(Some(q.get("limit").unwrap_or("50"))),
                Parameter::Limit,
            ),
            &mut problems,
        )
    } else {
        None
    };
    if let Some(e) = problems {
        return Err(model_error(&e, &c));
    }
    let project = authz::project_read(&mut auth.connection, &auth.principal, path(&p, "slug", &c)?)
        .await
        .map_err(|e| Failure::new(c.project_error(e)))?;
    if page {
        let (
            Some(ParameterValue::TrackState(state)),
            Some(ParameterValue::BeforeString(before)),
            Some(ParameterValue::Limit(limit)),
        ) = (state, before, limit)
        else {
            return Err(internal(&c, "track page model"));
        };
        let state = state.map(|v| String::from(v.as_str()));
        let mut rows = repo::list_tracks(
            &mut auth.connection,
            project.id,
            state.as_ref(),
            before.as_ref(),
            Some(&BigInt::from(limit + 1)),
            s.context.repository,
        )
        .await
        .map_err(|_| internal(&c, "track list"))?;
        let next = if rows.len() > limit {
            rows.truncate(limit);
            rows.last().map(|r| &r.slug)
        } else {
            None
        };
        let bytes = crate::track_wire::page_bytes(&rows, next, s.context.response)
            .map_err(|_| internal(&c, "track page serialization"))?;
        return Ok(([(header::CONTENT_TYPE, "application/json")], bytes).into_response());
    }
    let row = track(
        &mut auth.connection,
        project.id,
        path(&p, "track_slug", &c)?,
        None,
        &s.context,
        &c,
    )
    .await?;
    if history {
        let (Some(ParameterValue::HistoryBefore(before)), Some(ParameterValue::Limit(limit))) =
            (before, limit)
        else {
            return Err(internal(&c, "track history model"));
        };
        let limit_i64 =
            i64::try_from(limit + 1).map_err(|_| internal(&c, "track history limit"))?;
        let mut rows = crate::track_audit::history(
            &mut auth.connection,
            &row.id.to_string(),
            before.as_ref(),
            limit_i64,
            s.context.audit_decode_budget,
        )
        .await
        .map_err(|_| internal(&c, "track history"))?;
        let next = if rows.len() > limit {
            rows.truncate(limit);
            rows.last().map(|e| e.seq)
        } else {
            None
        };
        let bytes = crate::track_wire::history_bytes(&rows, next, s.context.response)
            .map_err(|_| internal(&c, "track history serialization"))?;
        return Ok(([(header::CONTENT_TYPE, "application/json")], bytes).into_response());
    }
    encoded(&row, &s.context, &c, StatusCode::OK)
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/tracks",
    operation_id = "list_tracks_api_projects__slug__tracks_get",
    summary = "List Tracks",
    description = "Tracks by slug.",
    params(("slug" = String, Path),
        ("state" = Option<String>, Query),
        ("before" = Option<String>, Query, description = "Continue after this track slug."),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_TrackOut_str_, content_type = "application/json"),
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
    s: State<RouteState>,
    axum::Extension(c): axum::Extension<RequestContext>,
    r: Request,
) -> Result<Response, Failure> {
    reading(s, c, r, true, false).await
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/tracks/{track_slug}",
    operation_id = "get_track_api_projects__slug__tracks__track_slug__get",
    summary = "Get Track",
    params(("slug" = String, Path),
        ("track_slug" = String, Path)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::TrackOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn read(
    s: State<RouteState>,
    axum::Extension(c): axum::Extension<RequestContext>,
    r: Request,
) -> Result<Response, Failure> {
    reading(s, c, r, false, false).await
}
#[utoipa::path(
    get,
    path = "/api/projects/{slug}/tracks/{track_slug}/history",
    operation_id = "track_history_api_projects__slug__tracks__track_slug__history_get",
    summary = "Track History",
    description = "The track's audit events, oldest first.",
    params(("slug" = String, Path),
        ("track_slug" = String, Path),
        ("before" = Option<i64>, Query, description = "Continue after this event sequence number.", minimum = 0),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_HistoryEvent_int_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn history(
    s: State<RouteState>,
    axum::Extension(c): axum::Extension<RequestContext>,
    r: Request,
) -> Result<Response, Failure> {
    reading(s, c, r, false, true).await
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
