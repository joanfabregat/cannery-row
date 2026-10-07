//! HTTP authentication preserves first-header/cookie decoding and autocommit.

use crate::{AppState, errors::ApiError, request_context, requests::RequestContext};
use axum::http::{HeaderMap, Method};
use cannery_core::principal::Principal;
use cannery_identity::{
    IdentityError,
    auth::{self, AuthInput},
};
use cannery_projects::ProjectError;
use sqlx::{Postgres, pool::PoolConnection};

pub(crate) struct Authenticated {
    pub connection: PoolConnection<Postgres>,
    pub principal: Principal,
}

/// Constructible only inside this crate; headers cannot supply this ownership.
#[derive(Clone)]
pub(crate) struct McpHandoff(std::sync::Arc<tokio::sync::Mutex<Option<Authenticated>>>);

impl McpHandoff {
    pub(crate) fn new(authentication: Authenticated) -> Self {
        Self(std::sync::Arc::new(tokio::sync::Mutex::new(Some(
            authentication,
        ))))
    }

    pub(crate) async fn take(&self, context: &RequestContext) -> Result<Authenticated, ApiError> {
        self.0
            .lock()
            .await
            .take()
            .ok_or_else(|| context.internal("MCP authentication reuse"))
    }
}

impl RequestContext {
    pub(crate) fn internal(&self, operation: &'static str) -> ApiError {
        ApiError::Internal {
            request_id: self.id.clone(),
            operation,
        }
    }

    pub(crate) fn identity_error(&self, error: IdentityError) -> ApiError {
        match error {
            IdentityError::Domain(error) => error.into(),
            IdentityError::Database { .. }
            | IdentityError::CorruptData(_)
            | IdentityError::Random => self.internal("identity operation"),
        }
    }

    pub(crate) fn project_error(&self, error: ProjectError) -> ApiError {
        match error {
            ProjectError::Domain(error) => error.into(),
            ProjectError::Database(_) | ProjectError::CorruptData => {
                self.internal("project operation")
            }
        }
    }
}

/// Authentication touches commit individually, including when later validation
/// or authorization fails, as in the source autocommit pool. Mutation handlers
/// begin their own transaction only around the mutation and its audit records.
pub(crate) async fn authenticate(
    state: &AppState,
    context: &RequestContext,
    headers: &HeaderMap,
    method: &Method,
) -> Result<Authenticated, ApiError> {
    if let Some(handoff) = &context.mcp_authentication {
        return handoff.take(context).await;
    }
    let mut connection = state
        .pool
        .acquire()
        .await
        .map_err(|_| context.internal("database connection"))?;
    let authorization = request_context::first_header(headers, "authorization");
    let csrf = request_context::first_header(headers, "x-csrf-token");
    let cookies = request_context::cookies(headers);
    let principal = auth::current_principal(
        &mut connection,
        AuthInput {
            authorization: authorization.as_deref(),
            session_cookie: cookies.get("cr_session").map(String::as_str),
            csrf_token: csrf.as_deref(),
            method: method.as_str(),
        },
    )
    .await
    .map_err(|error| context.identity_error(error))?;
    Ok(Authenticated {
        connection,
        principal,
    })
}
