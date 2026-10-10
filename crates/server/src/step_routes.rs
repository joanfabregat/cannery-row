//! Append-only registered producer and experiment manifests.
use crate::{
    AppState,
    attempt_lease_routes::{Failure, domain, failure, internal},
    authentication::authenticate,
    body::{self, DecodedBody},
    manifest_routes::violation,
    request_context::QueryParams,
    requests::RequestContext,
};
use axum::{
    Json, Router,
    extract::{FromRequestParts, Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use cannery_core::{
    audit::{self, Attribution, Record},
    contracts::{ContractKind, ContractValidator},
    errors::ErrorCode,
    ids::{ProjectId, UserId},
    json::{self, Document, Node},
    timestamps::Timestamp,
};
use cannery_projects::{authz, repo as projects};
use cannery_research::{
    config_repo,
    science::{RenderingContext, Science, ScienceError},
    steps,
};
use serde_json::json;
use sqlx::{Acquire, PgConnection};
use std::{collections::BTreeMap, sync::Arc};

pub struct StepContext {
    pub contracts: ContractValidator,
    pub config: config_repo::JsonContext,
    pub rendering: RenderingContext,
    pub nesting_budget: usize,
}
#[derive(Clone)]
pub(crate) struct RouteState {
    app: AppState,
    profile: Arc<StepContext>,
    experiment: bool,
}
struct Stored {
    name: String,
    revision: i32,
    content: String,
    created_by: UserId,
    created_at: Timestamp,
}
impl Stored {
    fn output(
        self,
        request_context: &RequestContext,
    ) -> Result<crate::api_models::ProducerOut, Failure> {
        let content = serde_json::from_str(&self.content)
            .map_err(|_| internal(request_context, "registered manifest content"))?;
        Ok(crate::api_models::ProducerOut {
            name: self.name,
            revision: i64::from(self.revision),
            content,
            created_by: self.created_by.0.to_string(),
            created_at: crate::timestamps::public_timestamp(self.created_at),
        })
    }
}
pub fn routes(app: AppState, profile: Arc<StepContext>) -> Router {
    let producer = Router::new()
        .route(
            "/api/projects/{slug}/producers",
            get(rest_list_producers).post(rest_register_producer),
        )
        .route(
            "/api/projects/{slug}/producers/{name}/{revision}",
            get(rest_get_producer),
        )
        .with_state(RouteState {
            app: app.clone(),
            profile: profile.clone(),
            experiment: false,
        });
    let experiment = Router::new()
        .route(
            "/api/projects/{slug}/experiment-steps",
            get(rest_list_experiment_steps).post(rest_register_experiment_step),
        )
        .route(
            "/api/projects/{slug}/experiment-steps/{name}/{revision}",
            get(rest_get_experiment_step),
        )
        .with_state(RouteState {
            app,
            profile,
            experiment: true,
        });
    producer.merge(experiment)
}
fn semantic(error: ScienceError, request_context: &RequestContext) -> Failure {
    match error {
        ScienceError::Validation { path, message } => {
            violation(&path.as_utf8().unwrap_or_default(), message)
        }
        _ => internal(request_context, "registered step validation"),
    }
}
async fn store(
    conn: &mut PgConnection,
    project: ProjectId,
    name: &str,
    content: &str,
    by: UserId,
    experiment: bool,
    request_context: &RequestContext,
) -> Result<Stored, Failure> {
    let result = if experiment {
        sqlx::query_file_as!(
            Stored,
            "src/sql/steps/create_experiment.sql",
            project as ProjectId,
            name,
            content,
            by as UserId
        )
        .fetch_one(conn)
        .await
    } else {
        sqlx::query_file_as!(
            Stored,
            "src/sql/steps/create_producer.sql",
            project as ProjectId,
            name,
            content,
            by as UserId
        )
        .fetch_one(conn)
        .await
    };
    result.map_err(|_| internal(request_context, "registered step insert"))
}
#[allow(
    clippy::too_many_lines,
    reason = "Registration keeps its project lock, pinned validation, insert and audit in one visible transaction"
)]
async fn create(
    State(state): State<RouteState>,
    axum::Extension(request_context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let mut auth = authenticate(
        &state.app,
        &request_context,
        request.headers(),
        request.method(),
    )
    .await
    .map_err(failure)?;
    let admin = auth
        .principal
        .require_admin(true)
        .map_err(|error| failure(crate::errors::ApiError::from(error)))?;
    let by = admin.user_id;
    let (mut parts, body) = body::read_body(request).await.map_err(failure)?;
    let paths = Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &state)
        .await
        .map_err(failure)?
        .0;
    let DecodedBody::Json(document) = body else {
        return Err(domain(
            ErrorCode::ValidationFailed,
            "step manifest must be a JSON object",
        ));
    };
    validate(&document, &state.profile, &request_context)?;
    let document =
        crate::api_models::request_document::<crate::api_models::StepManifestRequest>(&document)
            .map_err(failure)?;
    let project = projects::get_project_by_slug(
        &mut auth.connection,
        paths
            .get("slug")
            .ok_or_else(|| internal(&request_context, "step slug"))?,
    )
    .await
    .map_err(|_| internal(&request_context, "step project lookup"))?
    .ok_or_else(|| domain(ErrorCode::NotFound, "project not found"))?;
    let metadata = document
        .field(document.root(), "metadata")
        .ok_or_else(|| internal(&request_context, "step metadata"))?;
    let name = document
        .field(metadata, "name")
        .and_then(|id| document.node(id))
        .and_then(|node| {
            if let Node::String(value) = node {
                value.as_utf8()
            } else {
                None
            }
        })
        .ok_or_else(|| internal(&request_context, "step name"))?;
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&request_context, "step transaction"))?;
    projects::lock_project(&mut tx, project.id)
        .await
        .map_err(|_| internal(&request_context, "step project lock"))?;
    let revision = config_repo::get_revision(
        &mut tx,
        project.id,
        config_repo::Kind::Science,
        None,
        state.profile.config,
    )
    .await
    .map_err(|_| internal(&request_context, "step science lookup"))?
    .ok_or_else(|| {
        domain(
            ErrorCode::Conflict,
            "create a science revision before registering steps",
        )
    })?;
    let science = Science::new(
        revision.revision.into(),
        &revision.content,
        state.profile.rendering,
    )
    .map_err(|error| semantic(error, &request_context))?;
    let role = if state.experiment {
        steps::Role::Experiment
    } else {
        steps::Role::Producer
    };
    steps::check_step(
        &science,
        &document,
        role,
        &String::new(),
        state.profile.rendering,
    )
    .map_err(|error| semantic(error, &request_context))?;
    if !state.experiment {
        steps::check_pair(&science, &document, &String::new(), state.profile.rendering)
            .map_err(|error| semantic(error, &request_context))?;
    }
    let content = json::encode_ascii_default(&document, state.profile.nesting_budget)
        .map_err(|_| internal(&request_context, "step encoding"))?;
    let stored = store(
        &mut tx,
        project.id,
        &name,
        &content,
        by,
        state.experiment,
        &request_context,
    )
    .await?;
    let label = if state.experiment {
        "experiment_step"
    } else {
        "producer"
    };
    let new_state =
        json!({"name":name,"revision":stored.revision,"science_revision":revision.revision});
    audit::record(
        &mut tx,
        Attribution::Principal(&auth.principal),
        Record {
            action: if state.experiment {
                "experiment_step.registered"
            } else {
                "producer.registered"
            },
            subject_type: label,
            subject_id: &format!("{}:{name}:{}", project.id.0, stored.revision),
            project_id: Some(project.id),
            prior_state: None,
            new_state: Some(&new_state),
            reason: None,
            idempotency_key: None,
        },
    )
    .await
    .map_err(|_| internal(&request_context, "step registration audit"))?;
    tx.commit()
        .await
        .map_err(|_| internal(&request_context, "step commit"))?;
    Ok((StatusCode::CREATED, Json(stored.output(&request_context)?)).into_response())
}
fn validate(
    document: &Document,
    profile: &StepContext,
    request_context: &RequestContext,
) -> Result<(), Failure> {
    let violations = profile
        .contracts
        .violations(ContractKind::StepManifest, document)
        .map_err(|_| internal(request_context, "step contract"))?;
    if let Some(first) = violations.first() {
        return Err(violation(
            &first.path.as_utf8().unwrap_or_default(),
            &first.message,
        ));
    }
    Ok(())
}
async fn list(
    State(state): State<RouteState>,
    axum::Extension(request_context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Json<crate::api_models::Page_ProducerOut_str_>, Failure> {
    let mut auth = authenticate(
        &state.app,
        &request_context,
        request.headers(),
        request.method(),
    )
    .await
    .map_err(failure)?;
    let (mut parts, _) = request.into_parts();
    let paths = Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &state)
        .await
        .map_err(failure)?
        .0;
    let slug = paths
        .get("slug")
        .ok_or_else(|| internal(&request_context, "step slug"))?;
    let project = authz::project_read(&mut auth.connection, &auth.principal, slug)
        .await
        .map_err(|error| failure(request_context.project_error(error)))?;
    let query = QueryParams::parse(parts.uri.query().unwrap_or_default().as_bytes());
    let limit = query
        .get("limit")
        .unwrap_or("50")
        .parse::<i64>()
        .ok()
        .filter(|value| (1..=200).contains(value))
        .ok_or_else(|| violation("query/limit", "limit must be between 1 and 200"))?;
    let cursor = query
        .get("before")
        .map(|value| {
            let (name, revision) = value
                .split_once('/')
                .ok_or_else(|| violation("query/before", "invalid step cursor"))?;
            let revision = revision
                .parse::<i32>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| violation("query/before", "invalid step revision"))?;
            Ok::<_, Failure>((name, revision))
        })
        .transpose()?;
    let (after_name, after_revision) = cursor.map_or((None, None), |(name, revision)| {
        (Some(name), Some(revision))
    });
    let name = query.get("name");
    let take = limit + 1;
    let rows = if state.experiment {
        sqlx::query_file_as!(
            Stored,
            "src/sql/steps/list_experiment.sql",
            project.id as ProjectId,
            name,
            after_name,
            after_revision,
            take
        )
        .fetch_all(&mut *auth.connection)
        .await
    } else {
        sqlx::query_file_as!(
            Stored,
            "src/sql/steps/list_producer.sql",
            project.id as ProjectId,
            name,
            after_name,
            after_revision,
            take
        )
        .fetch_all(&mut *auth.connection)
        .await
    }
    .map_err(|_| internal(&request_context, "registered step list"))?;
    let count =
        usize::try_from(limit).map_err(|_| internal(&request_context, "step limit conversion"))?;
    let next = if rows.len() > count {
        rows.get(count - 1)
            .map(|row| format!("{}/{}", row.name, row.revision))
    } else {
        None
    };
    let items = rows
        .into_iter()
        .take(count)
        .map(|row| row.output(&request_context))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(crate::api_models::Page_ProducerOut_str_ {
        items,
        next_before: next,
    }))
}
async fn specific(
    State(state): State<RouteState>,
    axum::Extension(request_context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Json<crate::api_models::ProducerOut>, Failure> {
    let mut auth = authenticate(
        &state.app,
        &request_context,
        request.headers(),
        request.method(),
    )
    .await
    .map_err(failure)?;
    let (mut parts, _) = request.into_parts();
    let paths = Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &state)
        .await
        .map_err(failure)?
        .0;
    let project = authz::project_read(
        &mut auth.connection,
        &auth.principal,
        paths
            .get("slug")
            .ok_or_else(|| internal(&request_context, "step slug"))?,
    )
    .await
    .map_err(|error| failure(request_context.project_error(error)))?;
    let name = paths
        .get("name")
        .ok_or_else(|| internal(&request_context, "step name"))?;
    let revision = paths
        .get("revision")
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| violation("path/revision", "positive revision required"))?;
    let stored = if state.experiment {
        sqlx::query_file_as!(
            Stored,
            "src/sql/steps/get_experiment.sql",
            project.id as ProjectId,
            name,
            revision
        )
        .fetch_optional(&mut *auth.connection)
        .await
    } else {
        sqlx::query_file_as!(
            Stored,
            "src/sql/steps/get_producer.sql",
            project.id as ProjectId,
            name,
            revision
        )
        .fetch_optional(&mut *auth.connection)
        .await
    }
    .map_err(|_| internal(&request_context, "registered step lookup"))?
    .ok_or_else(|| domain(ErrorCode::NotFound, "registered step not found"))?;
    Ok(Json(stored.output(&request_context)?))
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/producers",
    operation_id = "register_producer_api_projects__slug__producers_post",
    summary = "Register Producer",
    params(("slug" = String, Path)),
    request_body(content = crate::api_models::StepManifestRequest, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::ProducerOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_register_producer(
    arg0: State<RouteState>,
    arg1: axum::Extension<RequestContext>,
    arg2: Request,
) -> Result<Response, Failure> {
    create(arg0, arg1, arg2).await
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/producers",
    operation_id = "list_producers_api_projects__slug__producers_get",
    summary = "List Producers",
    description = "Registered producer revisions, by name, newest revision first.",
    params(("slug" = String, Path),
        ("name" = Option<String>, Query, description = "Only this producer's revisions."),
        ("before" = Option<String>, Query, description = "Continue after this `name/revision`.", pattern = "^[a-z0-9][a-z0-9-]{0,62}/[1-9][0-9]{0,9}$"),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_ProducerOut_str_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_list_producers(
    arg0: State<RouteState>,
    arg1: axum::Extension<RequestContext>,
    arg2: Request,
) -> Result<Json<crate::api_models::Page_ProducerOut_str_>, Failure> {
    list(arg0, arg1, arg2).await
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/producers/{name}/{revision}",
    operation_id = "get_producer_api_projects__slug__producers__name___revision__get",
    summary = "Get Producer",
    params(("slug" = String, Path),
        ("name" = String, Path),
        ("revision" = i64, Path)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::ProducerOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_get_producer(
    arg0: State<RouteState>,
    arg1: axum::Extension<RequestContext>,
    arg2: Request,
) -> Result<Json<crate::api_models::ProducerOut>, Failure> {
    specific(arg0, arg1, arg2).await
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/experiment-steps",
    operation_id = "register_experiment_step_api_projects__slug__experiment_steps_post",
    summary = "Register Experiment Step",
    description = "Register the next revision of an experiment step (``role: experiment``), which\nworkflow tracks run. Admins only.",
    params(("slug" = String, Path)),
    request_body(content = crate::api_models::StepManifestRequest, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::ProducerOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_register_experiment_step(
    arg0: State<RouteState>,
    arg1: axum::Extension<RequestContext>,
    arg2: Request,
) -> Result<Response, Failure> {
    create(arg0, arg1, arg2).await
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/experiment-steps",
    operation_id = "list_experiment_steps_api_projects__slug__experiment_steps_get",
    summary = "List Experiment Steps",
    description = "Registered experiment step revisions, by name, newest revision first.",
    params(("slug" = String, Path),
        ("name" = Option<String>, Query, description = "Only this step's revisions."),
        ("before" = Option<String>, Query, description = "Continue after this `name/revision`.", pattern = "^[a-z0-9][a-z0-9-]{0,62}/[1-9][0-9]{0,9}$"),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_ProducerOut_str_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_list_experiment_steps(
    arg0: State<RouteState>,
    arg1: axum::Extension<RequestContext>,
    arg2: Request,
) -> Result<Json<crate::api_models::Page_ProducerOut_str_>, Failure> {
    list(arg0, arg1, arg2).await
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/experiment-steps/{name}/{revision}",
    operation_id = "get_experiment_step_api_projects__slug__experiment_steps__name___revision__get",
    summary = "Get Experiment Step",
    params(("slug" = String, Path),
        ("name" = String, Path),
        ("revision" = i64, Path)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::ProducerOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_get_experiment_step(
    arg0: State<RouteState>,
    arg1: axum::Extension<RequestContext>,
    arg2: Request,
) -> Result<Json<crate::api_models::ProducerOut>, Failure> {
    specific(arg0, arg1, arg2).await
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
