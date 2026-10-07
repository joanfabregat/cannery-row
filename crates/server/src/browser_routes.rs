//! Native browser login and logout with source autocommit/transaction boundaries.
use crate::{
    AppState,
    authentication::authenticate,
    browser,
    errors::ApiError,
    oidc_claims::Identity,
    oidc_provider::{OidcProvider, ProviderError},
    request_context::{self, QueryParams},
    requests::RequestContext,
};
use axum::{
    Extension, Json, Router,
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use cannery_core::{
    audit::{self, Attribution, Record},
    errors::{DomainError, ErrorCode},
    principal::{Channel, Principal, Secret, UserPrincipal, Via, all_scopes},
    timestamps::Timestamp,
};
use cannery_identity::{
    models::{LoginUpsert, User},
    repo::{self, LoginUser, NewSession, PendingLogin},
    secrets,
};
use num_traits::ToPrimitive;
use serde_json::json;
use sqlx::{Acquire, Postgres, pool::PoolConnection};
use std::{sync::Arc, time::SystemTime};

pub(crate) fn routes(state: AppState) -> Router {
    Router::new()
        .route(
            "/auth/login",
            get(login).head(get_not_allowed).fallback(get_not_allowed),
        )
        .route(
            "/auth/callback",
            get(callback)
                .head(get_not_allowed)
                .fallback(get_not_allowed),
        )
        .route("/auth/logout", post(logout).fallback(post_not_allowed))
        .with_state(state)
}
async fn get_not_allowed() -> impl IntoResponse {
    method_not_allowed("GET")
}
async fn post_not_allowed() -> impl IntoResponse {
    method_not_allowed("POST")
}
fn method_not_allowed(allow: &'static str) -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, allow)],
        Json(json!({"detail":"Method Not Allowed"})),
    )
}
fn domain(code: ErrorCode, message: &'static str) -> ApiError {
    DomainError::new(code, message).into()
}
fn failed(message: &String, context: &RequestContext) -> ApiError {
    message.as_utf8().map_or_else(
        || context.internal("login error encoding"),
        |message| DomainError::new(ErrorCode::LoginFailed, message).into(),
    )
}
fn provider_error(error: ProviderError, prefix: &str, context: &RequestContext) -> ApiError {
    match error {
        ProviderError::Rejected(message) => {
            let points = prefix
                .chars()
                .map(u32::from)
                .chain(message.codepoints().iter().copied())
                .collect();
            cannery_core::text::from_codepoints(points).map_or_else(
                || context.internal("provider error encoding"),
                |message| failed(&message, context),
            )
        }
        ProviderError::Unhandled => context.internal("OIDC provider"),
    }
}
fn oidc(state: &AppState) -> Result<&Arc<dyn OidcProvider>, ApiError> {
    state.oidc.as_ref().ok_or_else(|| {
        domain(
            ErrorCode::NotFound,
            "OIDC login is not configured on this installation",
        )
    })
}
async fn connection(
    state: &AppState,
    context: &RequestContext,
) -> Result<PoolConnection<Postgres>, ApiError> {
    state
        .pool
        .acquire()
        .await
        .map_err(|_| context.internal("database connection"))
}
fn query(request: &Request) -> QueryParams {
    QueryParams::parse(request.uri().query().unwrap_or_default().as_bytes())
}
fn now() -> Timestamp {
    Timestamp(chrono::DateTime::<chrono::Utc>::from(SystemTime::now()).fixed_offset())
}
fn redirect(value: &str, context: &RequestContext) -> Result<Response, ApiError> {
    let location =
        browser::redirect_location(value).map_err(|_| context.internal("redirect encoding"))?;
    let mut response = StatusCode::FOUND.into_response();
    response.headers_mut().insert(header::LOCATION, location);
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        axum::http::HeaderValue::from_static("0"),
    );
    Ok(response)
}
async fn login(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    request: Request,
) -> Result<Response, ApiError> {
    let mut connection = connection(&state, &context).await?;
    let provider = oidc(&state)?;
    let auth = provider
        .authorization_request()
        .await
        .map_err(|error| provider_error(error, "identity provider unavailable: ", &context))?;
    let binding = secrets::new_secret("").map_err(|error| context.identity_error(error))?;
    let params = query(&request);
    let return_to = params.get("return_to").map(String::from);
    let return_to = browser::safe_return_to(return_to.as_ref())
        .as_utf8()
        .ok_or_else(|| context.internal("return path encoding"))?;
    repo::save_login_request(
        &mut connection,
        PendingLogin {
            state: &auth.state,
            nonce: &auth.nonce,
            code_verifier: &auth.code_verifier,
            return_to: &return_to,
            browser_hash: binding.digest(),
        },
    )
    .await
    .map_err(|error| context.identity_error(error))?;
    // Source persists pending state before constructing redirect/cookie headers.
    let mut response = redirect(&auth.url, &context)?;
    let cookie = browser::login_cookie(binding.plaintext(), state.settings.auth.cookie_secure)
        .and_then(browser::CookieHeader::into_header)
        .map_err(|_| context.internal("login cookie encoding"))?;
    response.headers_mut().append(header::SET_COOKIE, cookie);
    Ok(response)
}
async fn callback(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    request: Request,
) -> Result<Response, ApiError> {
    let mut connection = connection(&state, &context).await?;
    let provider = oidc(&state)?;
    let params = query(&request);
    if let Some(error) = params.get("error") {
        return Err(failed(
            &String::from(&format!("the identity provider refused the login: {error}")),
            &context,
        ));
    }
    let Some((state_param, code)) = params
        .get("state")
        .zip(params.get("code"))
        .filter(|(state, code)| !state.is_empty() && !code.is_empty())
    else {
        return Err(domain(ErrorCode::LoginFailed, "missing state or code"));
    };
    let cookies = request_context::cookies(request.headers());
    let Some(binding) = cookies
        .get(browser::LOGIN_COOKIE)
        .filter(|value| !value.is_empty())
    else {
        return Err(domain(
            ErrorCode::LoginFailed,
            "this login was not started in this browser; start again",
        ));
    };
    let pending = repo::take_login_request(
        &mut connection,
        &Secret::new(state_param.to_owned()),
        &secrets::digest(binding),
        15,
    )
    .await
    .map_err(|error| context.identity_error(error))?
    .ok_or_else(|| {
        domain(
            ErrorCode::LoginFailed,
            "unknown or expired login attempt; start again",
        )
    })?;
    // State consumption is autocommitted even if exchange/policy/encoding fails.
    let identity = provider
        .exchange_code(&String::from(code), &pending.code_verifier, &pending.nonce)
        .await
        .map_err(|error| provider_error(error, "", &context))?;
    let (email, make_admin) = policy(&state, &identity)?;
    let session_secret = persist_identity(
        &mut connection,
        &state,
        identity,
        email,
        make_admin,
        &context,
    )
    .await?;
    let mut response = redirect(&String::from(&pending.return_to), &context)?;
    for cookie in browser::callback_cookies(
        session_secret.plaintext(),
        state.settings.auth.session_ttl_hours.as_bigint(),
        state.settings.auth.cookie_secure,
        now(),
    )
    .map_err(|_| context.internal("session cookie encoding"))?
    {
        response.headers_mut().append(
            header::SET_COOKIE,
            cookie
                .into_header()
                .map_err(|_| context.internal("session cookie header"))?,
        );
    }
    Ok(response)
}
fn policy(state: &AppState, identity: &Identity) -> Result<(Option<String>, bool), ApiError> {
    let email = identity
        .email
        .as_ref()
        .filter(|email| !email.codepoints().is_empty())
        .map(String::lowercase);
    let verified = email.as_ref().filter(|_| identity.email_verified);
    if !state.settings.auth.allowed_email_domains.is_empty() {
        let email_domain = verified.and_then(|email| {
            let points = email.codepoints();
            let start = points
                .iter()
                .rposition(|&point| point == u32::from('@'))
                .map_or(0, |position| position + 1);
            cannery_core::text::from_codepoints(points[start..].to_vec())
        });
        if !state
            .settings
            .auth
            .allowed_email_domains
            .iter()
            .any(|allowed| email_domain.as_ref() == Some(&String::from(allowed).lowercase()))
        {
            return Err(domain(
                ErrorCode::Forbidden,
                "this account's email domain is not allowed here",
            ));
        }
    }
    let make_admin = verified.is_some_and(|email| {
        state
            .settings
            .auth
            .bootstrap_admin_emails
            .iter()
            .any(|allowed| email == &String::from(allowed).lowercase())
    });
    Ok((email, make_admin))
}
fn event<'a>(action: &'a str, kind: &'a str, subject: &'a str) -> Record<'a> {
    Record {
        action,
        subject_type: kind,
        subject_id: subject,
        project_id: None,
        prior_state: None,
        new_state: None,
        reason: None,
        idempotency_key: None,
    }
}
async fn record(
    conn: &mut sqlx::PgConnection,
    attribution: Attribution<'_>,
    event: Record<'_>,
    context: &RequestContext,
) -> Result<(), ApiError> {
    audit::record(conn, attribution, event)
        .await
        .map_err(|_| context.internal("browser audit"))?;
    Ok(())
}
async fn user_audits(
    conn: &mut sqlx::PgConnection,
    upserted: LoginUpsert,
    issuer: &str,
    email: Option<&str>,
    context: &RequestContext,
) -> Result<User, ApiError> {
    let user = upserted.user;
    if upserted.created {
        record(
            conn,
            Attribution::System(None),
            Record {
                new_state: Some(&json!({"email":email,"issuer":issuer})),
                ..event("user.created", "user", &user.id.to_string())
            },
            context,
        )
        .await?;
    }
    if upserted.admin_granted {
        record(
            conn,
            Attribution::System(None),
            Record {
                prior_state: Some(&json!({"is_admin":false})),
                new_state: Some(&json!({"is_admin":true})),
                reason: Some("email listed in auth.bootstrap_admin_emails"),
                ..event("user.admin_granted", "user", &user.id.to_string())
            },
            context,
        )
        .await?;
    }
    Ok(user)
}
async fn persist_identity(
    conn: &mut sqlx::PgConnection,
    state: &AppState,
    identity: Identity,
    email: Option<String>,
    make_admin: bool,
    context: &RequestContext,
) -> Result<secrets::NewSecret, ApiError> {
    let session_secret = secrets::new_secret(secrets::SESSION_PREFIX)
        .map_err(|error| context.identity_error(error))?;
    let mut transaction = conn
        .begin()
        .await
        .map_err(|_| context.internal("login transaction"))?;
    let utf8 = |value: &String| {
        value
            .as_utf8()
            .ok_or_else(|| context.internal("login database text encoding"))
    };
    let issuer = utf8(&identity.issuer)?;
    let subject = utf8(&identity.subject)?;
    let email = email.as_ref().map(utf8).transpose()?;
    let name = identity.name.as_ref().map(utf8).transpose()?;
    let upserted = repo::upsert_login_user(
        &mut transaction,
        LoginUser {
            issuer: &issuer,
            subject: &subject,
            email: email.as_deref(),
            email_verified: identity.email_verified,
            display_name: name.as_deref(),
            make_admin,
        },
    )
    .await
    .map_err(|error| context.identity_error(error))?;
    let user = user_audits(
        &mut transaction,
        upserted,
        &issuer,
        email.as_deref(),
        context,
    )
    .await?;
    let csrf = secrets::new_secret("").map_err(|error| context.identity_error(error))?;
    // Python's make_interval(hours=>ttl) receives an int4 after user/audit work.
    let ttl = state
        .settings
        .auth
        .session_ttl_hours
        .as_bigint()
        .to_i32()
        .ok_or_else(|| context.internal("session TTL database encoding"))?;
    let session = repo::create_session(
        &mut transaction,
        NewSession {
            user_id: user.id,
            secret_digest: session_secret.digest(),
            csrf_token: csrf.plaintext(),
            ttl_hours: ttl,
        },
    )
    .await
    .map_err(|error| context.identity_error(error))?;
    let principal = Principal::User(UserPrincipal {
        user_id: user.id,
        email: user.email,
        display_name: user.display_name,
        is_admin: user.is_admin,
        via: Via {
            channel: Channel::Ui,
            client: None,
        },
        scopes: all_scopes(),
        session_id: Some(session.id),
        csrf_token: None,
    });
    record(
        &mut transaction,
        Attribution::Principal(&principal),
        Record {
            new_state: Some(&json!({"expires_at":session.expires_at.isoformat()})),
            ..event("session.created", "session", &session.id.to_string())
        },
        context,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|_| context.internal("login commit"))?;
    Ok(session_secret)
}
#[utoipa::path(
    post,
    path = "/auth/logout",
    operation_id = "logout_auth_logout_post",
    summary = "Logout",
    responses((status = 200, description = "Successful Response", body = crate::api_models::LogoutResult, content_type = "application/json"))
)]
pub(crate) async fn logout(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let mut authenticated = authenticate(&state, &context, &headers, &Method::POST).await?;
    let user = authenticated.principal.require_user()?;
    if let Some(session_id) = user.session_id {
        let mut transaction = authenticated
            .connection
            .begin()
            .await
            .map_err(|_| context.internal("logout transaction"))?;
        if repo::delete_session(&mut transaction, session_id)
            .await
            .map_err(|error| context.identity_error(error))?
        {
            record(
                &mut transaction,
                Attribution::Principal(&authenticated.principal),
                event("session.ended", "session", &session_id.to_string()),
                &context,
            )
            .await?;
        }
        transaction
            .commit()
            .await
            .map_err(|_| context.internal("logout commit"))?;
    }
    let logout_url = if let Some(provider) = &state.oidc {
        match provider
            .end_session_url(&String::from(&format!(
                "{}/",
                state.settings.server.public_base_url
            )))
            .await
        {
            Ok(value) => value
                .map(|value| {
                    value
                        .as_utf8()
                        .ok_or_else(|| context.internal("logout response encoding"))
                })
                .transpose()?,
            Err(ProviderError::Rejected(_)) => None,
            Err(ProviderError::Unhandled) => return Err(context.internal("OIDC logout provider")),
        }
    } else {
        None
    };
    let mut response = Json(crate::api_models::LogoutResult { logout_url }).into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        browser::delete_session_cookie(now())
            .into_header()
            .map_err(|_| context.internal("logout cookie encoding"))?,
    );
    Ok(response)
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
