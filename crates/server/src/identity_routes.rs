//! HTTP identity responses expose only the public source fields.

use crate::api_models::{MeOut, MembershipOut, ServiceAccountOut, TokenCreated, TokenOut, UserOut};
use crate::timestamps::{optional_timestamp, public_timestamp};
use crate::{
    AppState,
    authentication::{Authenticated, authenticate},
    body::{self, DecodedBody},
    errors::ApiError,
    request_context::QueryParams,
    requests::RequestContext,
    validation::{
        self, Body as ValidatedBody, BodyInput, BodyModel, Parameter, ParameterValue,
        ServiceAccountCreate, TokenCreate, ValidationErrors,
    },
};
use axum::{
    Extension, Json, Router,
    extract::{FromRequestParts, MatchedPath, Path, Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use cannery_core::{
    audit::{self, Attribution, Record},
    errors::{DomainError, ErrorCode},
    ids::{ProjectId, TokenId, UserId},
    json::Node,
    principal::{Principal, Scope},
};
use cannery_identity::{
    models::{ApiToken, ServiceAccount, TokenKind, User},
    repo::{self, NewServiceAccount, NewToken},
    secrets,
};
use cannery_projects::repo as projects;
use num_traits::ToPrimitive;
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::Acquire;
use std::collections::BTreeMap;

pub(crate) fn routes(state: AppState) -> Router {
    Router::new()
        .route("/api/me", get(me).head(me_head).fallback(me_head))
        .route(
            "/api/tokens",
            get(rest_list_tokens)
                .post(rest_create_token)
                .head(me_head)
                .fallback(me_head),
        )
        .route(
            "/api/tokens/{token_id}",
            delete(rest_revoke_token).fallback(delete_not_allowed),
        )
        .route(
            "/api/users",
            get(rest_find_users).head(me_head).fallback(me_head),
        )
        .route(
            "/api/projects/{slug}/service-accounts",
            get(rest_list_service_accounts)
                .post(rest_create_service_account)
                .head(me_head)
                .fallback(me_head),
        )
        .route(
            "/api/projects/{slug}/service-accounts/{name}/disable",
            post(rest_disable_service_account).fallback(post_not_allowed),
        )
        .route(
            "/api/projects/{slug}/service-accounts/{name}/tokens",
            get(rest_list_service_tokens)
                .post(rest_create_service_token)
                .head(me_head)
                .fallback(me_head),
        )
        .with_state(state)
}

async fn me_head() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "GET")],
        Json(json!({"detail":"Method Not Allowed"})),
    )
}

pub(crate) fn user_out(user: &User) -> UserOut {
    UserOut {
        id: user.id.to_string(),
        email: user.email.clone(),
        email_verified: user.email_verified,
        display_name: user.display_name.clone(),
        is_admin: user.is_admin,
    }
}
pub(crate) fn service_out(account: &ServiceAccount, project: &str) -> ServiceAccountOut {
    let kind = match account.kind {
        cannery_core::principal::ServiceKind::Agent => "agent",
        cannery_core::principal::ServiceKind::Experimenter => "experimenter",
        cannery_core::principal::ServiceKind::Tester => "tester",
        cannery_core::principal::ServiceKind::Evaluator => "evaluator",
    };
    ServiceAccountOut {
        id: account.id.to_string(),
        project: project.into(),
        kind: kind.into(),
        name: account.name.clone(),
        description: account.description.clone(),
        created_at: public_timestamp(account.created_at),
        disabled_at: optional_timestamp(account.disabled_at),
    }
}

#[utoipa::path(
    get,
    path = "/api/me",
    operation_id = "me_api_me_get",
    summary = "Me",
    responses((status = 200, description = "Successful Response", body = crate::api_models::MeOut, content_type = "application/json"))
)]
pub(crate) async fn me(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
) -> Result<Json<MeOut>, ApiError> {
    let mut authenticated = authenticate(&state, &context, &headers, &Method::GET).await?;
    let principal = &authenticated.principal;
    let connection = &mut authenticated.connection;
    let scopes: Vec<String> = principal
        .scopes()
        .iter()
        .map(|scope| scope.as_str().into())
        .collect();
    let channel: String = match principal.via().channel {
        cannery_core::principal::Channel::Ui => "ui",
        cannery_core::principal::Channel::Api => "api",
        cannery_core::principal::Channel::Mcp => "mcp",
        cannery_core::principal::Channel::Cli => "cli",
        cannery_core::principal::Channel::System => "system",
    }
    .into();
    let value = match principal {
        Principal::User(principal) => {
            let user = repo::get_user(connection, principal.user_id)
                .await
                .map_err(|error| context.identity_error(error))?
                .ok_or_else(|| context.internal("current user missing"))?;
            let memberships =
                projects::list_projects_for_user(connection, principal.user_id, false, None, None)
                    .await
                    .map_err(|error| context.project_error(error))?;
            let memberships = memberships
                .into_iter()
                .map(|project| MembershipOut {
                    project: project.slug,
                    title: project.title,
                    role: project.role.map(|role| {
                        match role {
                            cannery_core::principal::Role::Viewer => "viewer",
                            cannery_core::principal::Role::Member => "member",
                            cannery_core::principal::Role::Researcher => "researcher",
                        }
                        .into()
                    }),
                })
                .collect();
            MeOut {
                kind: "user".into(),
                user: Some(user_out(&user)),
                service_account: None,
                memberships: Some(memberships),
                csrf_token: principal
                    .csrf_token
                    .as_ref()
                    .map(|secret| secret.expose().into()),
                scopes,
                channel,
            }
        }
        Principal::Service(principal) => {
            let account = repo::get_service_account(connection, principal.service_account_id)
                .await
                .map_err(|error| context.identity_error(error))?
                .ok_or_else(|| context.internal("current service missing"))?;
            let project = projects::get_project(connection, principal.project_id)
                .await
                .map_err(|error| context.project_error(error))?
                .ok_or_else(|| context.internal("current project missing"))?;
            MeOut {
                kind: "service".into(),
                user: None,
                service_account: Some(service_out(&account, &project.slug)),
                memberships: Some(Vec::new()),
                csrf_token: None,
                scopes,
                channel,
            }
        }
    };
    Ok(Json(value))
}

#[derive(Clone, Copy)]
enum Action {
    ListTokens,
    CreateToken,
    RevokeToken,
    FindUsers,
    ListAccounts,
    CreateAccount,
    DisableAccount,
    ListServiceTokens,
    CreateServiceToken,
}
impl Action {
    fn model(self) -> Option<BodyModel> {
        match self {
            Self::CreateToken | Self::CreateServiceToken => Some(BodyModel::TokenCreate),
            Self::CreateAccount => Some(BodyModel::ServiceAccountCreate),
            Self::DisableAccount => Some(BodyModel::DisableRequest),
            _ => None,
        }
    }
    fn from_route(path: &str, method: &Method) -> Option<Self> {
        match (path, method.as_str()) {
            ("/api/tokens", "GET") => Some(Self::ListTokens),
            ("/api/tokens", "POST") => Some(Self::CreateToken),
            ("/api/tokens/{token_id}", "DELETE") => Some(Self::RevokeToken),
            ("/api/users", "GET") => Some(Self::FindUsers),
            ("/api/projects/{slug}/service-accounts", "GET") => Some(Self::ListAccounts),
            ("/api/projects/{slug}/service-accounts", "POST") => Some(Self::CreateAccount),
            ("/api/projects/{slug}/service-accounts/{name}/disable", "POST") => {
                Some(Self::DisableAccount)
            }
            ("/api/projects/{slug}/service-accounts/{name}/tokens", "GET") => {
                Some(Self::ListServiceTokens)
            }
            ("/api/projects/{slug}/service-accounts/{name}/tokens", "POST") => {
                Some(Self::CreateServiceToken)
            }
            _ => None,
        }
    }
}

async fn post_not_allowed() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "POST")],
        Json(json!({"detail":"Method Not Allowed"})),
    )
}
async fn delete_not_allowed() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "DELETE")],
        Json(json!({"detail":"Method Not Allowed"})),
    )
}

fn validation_error(context: &RequestContext, errors: &ValidationErrors) -> ApiError {
    errors.domain_error().map_or_else(
        |_| context.internal("validation response encoding"),
        Into::into,
    )
}
fn domain(code: ErrorCode, message: &'static str) -> ApiError {
    DomainError::new(code, message).into()
}
fn body_input(body: &DecodedBody) -> BodyInput<'_> {
    match body {
        DecodedBody::Missing => BodyInput::Missing,
        DecodedBody::Json(document) => BodyInput::Json(document),
        DecodedBody::RawBytes => BodyInput::RawBytes,
    }
}
fn token_out(token: &ApiToken) -> TokenOut {
    TokenOut {
        id: token.id.to_string(),
        kind: token.kind.as_str().into(),
        name: token.name.clone(),
        display_prefix: token.display_prefix.clone(),
        scopes: token
            .scopes
            .iter()
            .map(|scope| scope.as_str().into())
            .collect(),
        created_at: public_timestamp(token.created_at),
        expires_at: public_timestamp(token.expires_at),
        last_used_at: optional_timestamp(token.last_used_at),
        revoked_at: optional_timestamp(token.revoked_at),
    }
}

fn token_state(token: &ApiToken, account: Option<&ServiceAccount>) -> Value {
    let mut state = json!({"kind":token.kind,"name":token.name,"scopes":token.scopes,"expires_at":token.expires_at.isoformat()});
    if let Some(account) = account {
        state["service_account"] = json!(account.name);
    }
    state
}
#[derive(serde::Serialize)]
struct Page<T> {
    items: Vec<T>,
    next_before: Option<String>,
}
fn page<T, O: Serialize>(
    mut rows: Vec<T>,
    limit: usize,
    key: impl FnOnce(&T) -> String,
    output: impl Fn(&T) -> O,
) -> Page<O> {
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next_before = if more { rows.last().map(key) } else { None };
    Page {
        items: rows.iter().map(output).collect(),
        next_before,
    }
}

async fn rest(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    request: Request,
) -> Response {
    let action = request
        .extensions()
        .get::<MatchedPath>()
        .and_then(|path| Action::from_route(path.as_str(), request.method()));
    let Some(action) = action else {
        return context.internal("identity route selection").into_response();
    };
    let (mut parts, body) = if action.model().is_some() {
        match body::read_body(request).await {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        }
    } else {
        let (parts, _) = request.into_parts();
        (parts, DecodedBody::Missing)
    };
    let mut authenticated =
        match authenticate(&state, &context, &parts.headers, &parts.method).await {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    // Extract only after authentication; malformed UUIDs cannot precede 401/CSRF.
    let paths = if matches!(
        action,
        Action::ListTokens | Action::CreateToken | Action::FindUsers
    ) {
        BTreeMap::new()
    } else {
        match Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &state).await {
            Ok(Path(paths)) => paths,
            // Axum rejects invalid UTF-8 path escapes; shared path transport owns parity.
            Err(_) => {
                return domain(ErrorCode::ValidationFailed, "request validation failed")
                    .into_response();
            }
        }
    };
    let query = QueryParams::parse(parts.uri.query().unwrap_or("").as_bytes());
    match dispatch(
        &state,
        &context,
        &mut authenticated,
        action,
        &paths,
        &query,
        &body,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

struct ListParameters {
    before: Option<uuid::Uuid>,
    before_name: Option<String>,
    limit: usize,
}
fn list_parameters(
    context: &RequestContext,
    query: &QueryParams,
    names: bool,
) -> Result<ListParameters, ApiError> {
    let before_node = query
        .get("before")
        .map_or(Node::Null, |value| Node::String(String::from(value)));
    let limit_node = query.get("limit").map_or_else(
        || Node::Integer(50.into()),
        |value| Node::String(String::from(value)),
    );
    let before = validation::validate_parameter_node(
        &before_node,
        if names {
            Parameter::BeforeString
        } else {
            Parameter::BeforeUuid
        },
    );
    let limit = validation::validate_parameter_node(&limit_node, Parameter::Limit);
    let (before, limit) = match (before, limit) {
        (Ok(before), Ok(limit)) => (before, limit),
        (Err(mut before), Err(limit)) => {
            before.append(limit);
            return Err(validation_error(context, &before));
        }
        (Err(error), _) | (_, Err(error)) => return Err(validation_error(context, &error)),
    };
    let ParameterValue::Limit(limit) = limit else {
        return Err(context.internal("limit validation"));
    };
    let (before, before_name) = match before {
        ParameterValue::BeforeUuid(value) => (value, None),
        ParameterValue::BeforeString(value) => (
            None,
            value
                .map(|value| {
                    value
                        .as_utf8()
                        .ok_or_else(|| context.internal("query encoding"))
                })
                .transpose()?,
        ),
        _ => return Err(context.internal("cursor validation")),
    };
    Ok(ListParameters {
        before,
        before_name,
        limit,
    })
}
fn path<'a>(
    context: &RequestContext,
    paths: &'a BTreeMap<String, String>,
    name: &str,
) -> Result<&'a str, ApiError> {
    paths
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| context.internal("identity path extraction"))
}
fn fetch_limit(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(200) + 1
}

fn token_create_body(body: &DecodedBody) -> Result<TokenCreate, ApiError> {
    let typed: crate::api_models::TokenCreate = body::typed(body)?;
    Ok(TokenCreate {
        name: typed.name.trim().to_owned(),
        expires_in_days: typed.expires_in_days.into(),
        scopes: typed
            .scopes
            .into_iter()
            .map(|scope| match scope {
                crate::api_models::ScopeName::Read => Scope::Read,
                crate::api_models::ScopeName::Write => Scope::Write,
            })
            .collect(),
    })
}

fn service_account_create_body(body: &DecodedBody) -> Result<ServiceAccountCreate, ApiError> {
    let typed: crate::api_models::ServiceAccountCreate = body::typed(body)?;
    Ok(ServiceAccountCreate {
        name: typed.name,
        description: typed.description.unwrap_or_default(),
        kind: match typed.kind {
            crate::api_models::ServiceKind::Agent => cannery_core::principal::ServiceKind::Agent,
            crate::api_models::ServiceKind::Experimenter => {
                cannery_core::principal::ServiceKind::Experimenter
            }
            crate::api_models::ServiceKind::Tester => cannery_core::principal::ServiceKind::Tester,
            crate::api_models::ServiceKind::Evaluator => {
                cannery_core::principal::ServiceKind::Evaluator
            }
        },
    })
}

async fn dispatch(
    state: &AppState,
    context: &RequestContext,
    auth: &mut Authenticated,
    action: Action,
    paths: &BTreeMap<String, String>,
    query: &QueryParams,
    body: &DecodedBody,
) -> Result<Response, ApiError> {
    // Pure request checks precede route-body authorization and all lookups.
    let model = action
        .model()
        .map(|model| validation::validate_body_input(body_input(body), model))
        .transpose()
        .map_err(|error| validation_error(context, &error))?;
    match action {
        Action::CreateToken | Action::CreateServiceToken => {
            let Some(ValidatedBody::TokenCreate(_)) = model else {
                return Err(context.internal("token body selection"));
            };
            let body = token_create_body(body)?;
            create_token(
                state,
                context,
                auth,
                paths,
                body,
                matches!(action, Action::CreateServiceToken),
            )
            .await
        }
        Action::CreateAccount => {
            let Some(ValidatedBody::ServiceAccountCreate(_)) = model else {
                return Err(context.internal("service body selection"));
            };
            let body = service_account_create_body(body)?;
            create_account(context, auth, path(context, paths, "slug")?, body).await
        }
        Action::DisableAccount => {
            let Some(ValidatedBody::DisableRequest(_)) = model else {
                return Err(context.internal("disable body selection"));
            };
            let typed: crate::api_models::DisableRequest = body::typed(body)?;
            disable_account(
                context,
                auth,
                path(context, paths, "slug")?,
                path(context, paths, "name")?,
                &typed.reason,
            )
            .await
        }
        Action::RevokeToken => {
            let node = Node::String(String::from(path(context, paths, "token_id")?));
            let ParameterValue::TokenId(id) =
                validation::validate_parameter_node(&node, Parameter::TokenId)
                    .map_err(|error| validation_error(context, &error))?
            else {
                return Err(context.internal("token path validation"));
            };
            revoke_token(context, auth, id).await
        }
        Action::FindUsers => {
            let pairs: Vec<_> = query
                .pairs()
                .iter()
                .map(|(key, value)| (String::from(key), String::from(value)))
                .collect();
            let query = validation::find_users_query(&pairs)
                .map_err(|error| validation_error(context, &error))?;
            auth.principal.require_admin(false)?;
            let rows = repo::find_users_by_email(
                &mut auth.connection,
                &query.email,
                query.before.map(UserId),
                Some(fetch_limit(query.limit)),
            )
            .await
            .map_err(|error| context.identity_error(error))?;
            Ok(Json(page(
                rows,
                query.limit,
                |user| user.id.to_string(),
                user_out,
            ))
            .into_response())
        }
        Action::ListTokens | Action::ListAccounts | Action::ListServiceTokens => {
            let query = list_parameters(context, query, matches!(action, Action::ListAccounts))?;
            list(context, auth, action, paths, &query).await
        }
    }
}

async fn admin_project(
    context: &RequestContext,
    connection: &mut sqlx::PgConnection,
    slug: &str,
) -> Result<projects::Project, ApiError> {
    projects::get_project_by_slug(connection, slug)
        .await
        .map_err(|error| context.project_error(error))?
        .ok_or_else(|| domain(ErrorCode::NotFound, "project not found"))
}
async fn account(
    context: &RequestContext,
    connection: &mut sqlx::PgConnection,
    project_id: ProjectId,
    name: &str,
) -> Result<ServiceAccount, ApiError> {
    repo::get_service_account_by_name(connection, project_id, name)
        .await
        .map_err(|error| context.identity_error(error))?
        .ok_or_else(|| domain(ErrorCode::NotFound, "service account not found"))
}
async fn list(
    context: &RequestContext,
    auth: &mut Authenticated,
    action: Action,
    paths: &BTreeMap<String, String>,
    query: &ListParameters,
) -> Result<Response, ApiError> {
    if matches!(action, Action::ListTokens) {
        let user = auth.principal.require_user()?;
        let rows = repo::list_user_tokens(
            &mut auth.connection,
            user.user_id,
            query.before.map(TokenId),
            Some(fetch_limit(query.limit)),
        )
        .await
        .map_err(|error| context.identity_error(error))?;
        return Ok(Json(page(
            rows,
            query.limit,
            |token| token.id.to_string(),
            token_out,
        ))
        .into_response());
    }
    auth.principal.require_admin(false)?;
    let slug = path(context, paths, "slug")?;
    let project = admin_project(context, &mut auth.connection, slug).await?;
    if matches!(action, Action::ListAccounts) {
        let rows = repo::list_service_accounts(
            &mut auth.connection,
            project.id,
            query.before_name.as_deref(),
            Some(fetch_limit(query.limit)),
        )
        .await
        .map_err(|error| context.identity_error(error))?;
        Ok(Json(page(
            rows,
            query.limit,
            |account| account.name.clone(),
            |account| service_out(account, slug),
        ))
        .into_response())
    } else {
        let account = account(
            context,
            &mut auth.connection,
            project.id,
            path(context, paths, "name")?,
        )
        .await?;
        let rows = repo::list_service_tokens(
            &mut auth.connection,
            account.id,
            query.before.map(TokenId),
            Some(fetch_limit(query.limit)),
        )
        .await
        .map_err(|error| context.identity_error(error))?;
        Ok(Json(page(
            rows,
            query.limit,
            |token| token.id.to_string(),
            token_out,
        ))
        .into_response())
    }
}

fn token_created(token: &ApiToken, plaintext: &str) -> TokenCreated {
    let token = token_out(token);
    TokenCreated {
        id: token.id,
        kind: token.kind,
        name: token.name,
        display_prefix: token.display_prefix,
        scopes: token.scopes,
        created_at: token.created_at,
        expires_at: token.expires_at,
        last_used_at: token.last_used_at,
        revoked_at: token.revoked_at,
        token: plaintext.into(),
    }
}

fn token_owner(principal: &Principal, service: bool) -> Result<UserId, ApiError> {
    let user = if service {
        principal.require_admin(true)?
    } else {
        principal.require_user()?
    };
    if user.session_id.is_none() {
        return Err(domain(
            ErrorCode::Forbidden,
            if service {
                "service tokens can only be created from a signed-in browser session"
            } else {
                "personal tokens can only be created from a signed-in browser session"
            },
        ));
    }
    Ok(user.user_id)
}

async fn create_token(
    state: &AppState,
    context: &RequestContext,
    auth: &mut Authenticated,
    paths: &BTreeMap<String, String>,
    body: TokenCreate,
    service: bool,
) -> Result<Response, ApiError> {
    let owner_id = token_owner(&auth.principal, service)?;
    let service_account = if service {
        let project =
            admin_project(context, &mut auth.connection, path(context, paths, "slug")?).await?;
        let account = account(
            context,
            &mut auth.connection,
            project.id,
            path(context, paths, "name")?,
        )
        .await?;
        if account.disabled_at.is_some() {
            return Err(domain(
                ErrorCode::Conflict,
                "the service account is disabled",
            ));
        }
        Some(account)
    } else {
        None
    };
    let scopes = validation::check_token_request(
        &body,
        state.settings.auth.personal_token_max_days.as_bigint(),
    )?;
    let days = body
        .expires_in_days
        .to_i32()
        .ok_or_else(|| context.internal("token interval range"))?;
    let secret = secrets::new_secret(if service {
        secrets::SERVICE_PREFIX
    } else {
        secrets::PERSONAL_PREFIX
    })
    .map_err(|error| context.identity_error(error))?;
    let mut transaction = auth
        .connection
        .begin()
        .await
        .map_err(|_| context.internal("token transaction"))?;
    let token = repo::create_token(
        &mut transaction,
        NewToken {
            secret_digest: secret.digest(),
            display_prefix: secret.display_prefix(),
            kind: if service {
                TokenKind::Service
            } else {
                TokenKind::Personal
            },
            user_id: if service { None } else { Some(owner_id) },
            service_account_id: service_account.as_ref().map(|account| account.id),
            name: &body.name,
            scopes: &scopes,
            expires_in_days: days,
        },
    )
    .await
    .map_err(|error| context.identity_error(error))?;
    let new_state = token_state(&token, service_account.as_ref());
    audit::record(
        &mut transaction,
        Attribution::Principal(&auth.principal),
        Record {
            action: "token.created",
            subject_type: "api_token",
            subject_id: &token.id.to_string(),
            project_id: service_account.as_ref().map(|account| account.project_id),
            prior_state: None,
            new_state: Some(&new_state),
            reason: None,
            idempotency_key: None,
        },
    )
    .await
    .map_err(|_| context.internal("token audit"))?;
    transaction
        .commit()
        .await
        .map_err(|_| context.internal("token commit"))?;
    Ok((
        StatusCode::CREATED,
        Json(token_created(&token, secret.plaintext().expose())),
    )
        .into_response())
}

async fn create_account(
    context: &RequestContext,
    auth: &mut Authenticated,
    slug: &str,
    body: ServiceAccountCreate,
) -> Result<Response, ApiError> {
    let admin = auth.principal.require_admin(true)?;
    let project = admin_project(context, &mut auth.connection, slug).await?;
    if repo::get_service_account_by_name(&mut auth.connection, project.id, &body.name)
        .await
        .map_err(|error| context.identity_error(error))?
        .is_some()
    {
        return Err(domain(
            ErrorCode::Conflict,
            "a service account with this name already exists in the project",
        ));
    }
    let description = body
        .description
        .as_utf8()
        .ok_or_else(|| context.internal("service description encoding"))?;
    let mut transaction = auth
        .connection
        .begin()
        .await
        .map_err(|_| context.internal("service transaction"))?;
    let account = repo::create_service_account(
        &mut transaction,
        NewServiceAccount {
            project_id: project.id,
            kind: body.kind,
            name: &body.name,
            description: &description,
            created_by: admin.user_id,
        },
    )
    .await
    .map_err(|error| {
        if error.sqlstate() == Some("23505") {
            domain(
                ErrorCode::Conflict,
                "a service account with this name already exists in the project",
            )
        } else {
            context.identity_error(error)
        }
    })?;
    audit::record(
        &mut transaction,
        Attribution::Principal(&auth.principal),
        Record {
            action: "service_account.created",
            subject_type: "service_account",
            subject_id: &account.id.to_string(),
            project_id: Some(project.id),
            prior_state: None,
            new_state: Some(&json!({"kind":account.kind,"name":account.name})),
            reason: None,
            idempotency_key: None,
        },
    )
    .await
    .map_err(|_| context.internal("service audit"))?;
    transaction
        .commit()
        .await
        .map_err(|_| context.internal("service commit"))?;
    Ok((StatusCode::CREATED, Json(service_out(&account, slug))).into_response())
}
async fn disable_account(
    context: &RequestContext,
    auth: &mut Authenticated,
    slug: &str,
    name: &str,
    reason: &str,
) -> Result<Response, ApiError> {
    auth.principal.require_admin(true)?;
    let project = admin_project(context, &mut auth.connection, slug).await?;
    let account = account(context, &mut auth.connection, project.id, name).await?;
    let mut transaction = auth
        .connection
        .begin()
        .await
        .map_err(|_| context.internal("service disable transaction"))?;
    let disabled = repo::disable_service_account(&mut transaction, account.id)
        .await
        .map_err(|error| context.identity_error(error))?;
    let account = if let Some(disabled) = disabled {
        audit::record(
            &mut transaction,
            Attribution::Principal(&auth.principal),
            Record {
                action: "service_account.disabled",
                subject_type: "service_account",
                subject_id: &account.id.to_string(),
                project_id: Some(project.id),
                prior_state: None,
                new_state: None,
                reason: Some(reason),
                idempotency_key: None,
            },
        )
        .await
        .map_err(|_| context.internal("service disable audit"))?;
        disabled
    } else {
        repo::get_service_account(&mut transaction, account.id)
            .await
            .map_err(|error| context.identity_error(error))?
            .ok_or_else(|| context.internal("disabled service missing"))?
    };
    transaction
        .commit()
        .await
        .map_err(|_| context.internal("service disable commit"))?;
    Ok(Json(service_out(&account, slug)).into_response())
}
async fn revoke_token(
    context: &RequestContext,
    auth: &mut Authenticated,
    id: TokenId,
) -> Result<Response, ApiError> {
    let user = auth.principal.require_user()?;
    auth.principal.require_scope(Scope::Write)?;
    let token = repo::get_token(&mut auth.connection, id)
        .await
        .map_err(|error| context.identity_error(error))?
        .ok_or_else(|| domain(ErrorCode::NotFound, "token not found"))?;
    if token.user_id != Some(user.user_id) && !user.is_admin {
        return Err(domain(ErrorCode::NotFound, "token not found"));
    }
    let project_id = if let Some(id) = token.service_account_id {
        repo::get_service_account(&mut auth.connection, id)
            .await
            .map_err(|error| context.identity_error(error))?
            .map(|account| account.project_id)
    } else {
        None
    };
    let mut transaction = auth
        .connection
        .begin()
        .await
        .map_err(|_| context.internal("revoke transaction"))?;
    let revoked = repo::revoke_token(&mut transaction, id)
        .await
        .map_err(|error| context.identity_error(error))?;
    let token = if let Some(revoked) = revoked {
        audit::record(
            &mut transaction,
            Attribution::Principal(&auth.principal),
            Record {
                action: "token.revoked",
                subject_type: "api_token",
                subject_id: &id.to_string(),
                project_id,
                prior_state: None,
                new_state: None,
                reason: None,
                idempotency_key: None,
            },
        )
        .await
        .map_err(|_| context.internal("revoke audit"))?;
        revoked
    } else {
        repo::get_token(&mut transaction, id)
            .await
            .map_err(|error| context.identity_error(error))?
            .ok_or_else(|| domain(ErrorCode::NotFound, "token not found"))?
    };
    transaction
        .commit()
        .await
        .map_err(|_| context.internal("revoke commit"))?;
    Ok(Json(token_out(&token)).into_response())
}

#[utoipa::path(
    get,
    path = "/api/tokens",
    operation_id = "list_tokens_api_tokens_get",
    summary = "List Tokens",
    description = "The caller's personal tokens, newest first.",
    params(("before" = Option<String>, Query, description = "Continue after this token id.", format = "uuid"),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_TokenOut_UUID_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_list_tokens(
    arg0: State<AppState>,
    arg1: Extension<RequestContext>,
    arg2: Request,
) -> Response {
    rest(arg0, arg1, arg2).await
}

#[utoipa::path(
    post,
    path = "/api/tokens",
    operation_id = "create_token_api_tokens_post",
    summary = "Create Token",
    request_body(content = crate::api_models::TokenCreate, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::TokenCreated, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_create_token(
    arg0: State<AppState>,
    arg1: Extension<RequestContext>,
    arg2: Request,
) -> Response {
    rest(arg0, arg1, arg2).await
}

#[utoipa::path(
    delete,
    path = "/api/tokens/{token_id}",
    operation_id = "revoke_token_api_tokens__token_id__delete",
    summary = "Revoke Token",
    params(("token_id" = String, Path, format = "uuid")),
    responses((status = 200, description = "Successful Response", body = crate::api_models::TokenOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_revoke_token(
    arg0: State<AppState>,
    arg1: Extension<RequestContext>,
    arg2: Request,
) -> Response {
    rest(arg0, arg1, arg2).await
}

#[utoipa::path(
    get,
    path = "/api/users",
    operation_id = "find_users_api_users_get",
    summary = "Find Users",
    params(("email" = String, Query, min_length = 3),
        ("before" = Option<String>, Query, description = "Continue after this user id.", format = "uuid"),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_UserOut_UUID_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_find_users(
    arg0: State<AppState>,
    arg1: Extension<RequestContext>,
    arg2: Request,
) -> Response {
    rest(arg0, arg1, arg2).await
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/service-accounts",
    operation_id = "list_service_accounts_api_projects__slug__service_accounts_get",
    summary = "List Service Accounts",
    description = "The project's service accounts, by name.",
    params(("slug" = String, Path),
        ("before" = Option<String>, Query, description = "Continue after this account name."),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_ServiceAccountOut_str_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_list_service_accounts(
    arg0: State<AppState>,
    arg1: Extension<RequestContext>,
    arg2: Request,
) -> Response {
    rest(arg0, arg1, arg2).await
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/service-accounts",
    operation_id = "create_service_account_api_projects__slug__service_accounts_post",
    summary = "Create Service Account",
    params(("slug" = String, Path)),
    request_body(content = crate::api_models::ServiceAccountCreate, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::ServiceAccountOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_create_service_account(
    arg0: State<AppState>,
    arg1: Extension<RequestContext>,
    arg2: Request,
) -> Response {
    rest(arg0, arg1, arg2).await
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/service-accounts/{name}/disable",
    operation_id = "disable_service_account_api_projects__slug__service_accounts__name__disable_post",
    summary = "Disable Service Account",
    params(("slug" = String, Path),
        ("name" = String, Path)),
    request_body(content = crate::api_models::DisableRequest, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = crate::api_models::ServiceAccountOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_disable_service_account(
    arg0: State<AppState>,
    arg1: Extension<RequestContext>,
    arg2: Request,
) -> Response {
    rest(arg0, arg1, arg2).await
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/service-accounts/{name}/tokens",
    operation_id = "list_service_tokens_api_projects__slug__service_accounts__name__tokens_get",
    summary = "List Service Tokens",
    description = "The service account's tokens, newest first.",
    params(("slug" = String, Path),
        ("name" = String, Path),
        ("before" = Option<String>, Query, description = "Continue after this token id.", format = "uuid"),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_TokenOut_UUID_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_list_service_tokens(
    arg0: State<AppState>,
    arg1: Extension<RequestContext>,
    arg2: Request,
) -> Response {
    rest(arg0, arg1, arg2).await
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/service-accounts/{name}/tokens",
    operation_id = "create_service_token_api_projects__slug__service_accounts__name__tokens_post",
    summary = "Create Service Token",
    params(("slug" = String, Path),
        ("name" = String, Path)),
    request_body(content = crate::api_models::TokenCreate, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::TokenCreated, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 400, description = "Invalid request", body = crate::api_models::BadRequestResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn rest_create_service_token(
    arg0: State<AppState>,
    arg1: Extension<RequestContext>,
    arg2: Request,
) -> Response {
    rest(arg0, arg1, arg2).await
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
