//! HTTP-neutral authentication. The adapter supplies decoded header/cookie text;
//! this layer owns precedence, cookie CSRF and source-compatible principal data.
use crate::{
    error::{IdentityError, Result},
    repo,
    secrets::digest,
};
use cannery_core::{
    errors::{DomainError, ErrorCode},
    principal::{Channel, Principal, ServicePrincipal, UserPrincipal, Via, all_scopes},
};
use sqlx::PgConnection;
use std::{future::Future, pin::Pin};
use subtle::ConstantTimeEq;

pub(crate) type Operation<'a, T, E> =
    Pin<Box<dyn Future<Output = std::result::Result<T, E>> + Send + 'a>>;
pub(crate) trait Repository {
    type Error: From<IdentityError>;
    fn token(
        &mut self,
        digest: Vec<u8>,
    ) -> Operation<'_, Option<crate::models::ApiToken>, Self::Error>;
    fn user(
        &mut self,
        id: cannery_core::ids::UserId,
    ) -> Operation<'_, Option<crate::models::User>, Self::Error>;
    fn service(
        &mut self,
        id: cannery_core::ids::ServiceAccountId,
    ) -> Operation<'_, Option<crate::models::ServiceAccount>, Self::Error>;
    fn session(
        &mut self,
        digest: Vec<u8>,
    ) -> Operation<'_, Option<(crate::models::SessionRow, crate::models::User)>, Self::Error>;
}
struct Legacy<'a>(&'a mut PgConnection);
impl Repository for Legacy<'_> {
    type Error = IdentityError;
    fn token(
        &mut self,
        digest: Vec<u8>,
    ) -> Operation<'_, Option<crate::models::ApiToken>, Self::Error> {
        Box::pin(async move { repo::lookup_active_token(self.0, &digest).await })
    }
    fn user(
        &mut self,
        id: cannery_core::ids::UserId,
    ) -> Operation<'_, Option<crate::models::User>, Self::Error> {
        Box::pin(repo::get_user(self.0, id))
    }
    fn service(
        &mut self,
        id: cannery_core::ids::ServiceAccountId,
    ) -> Operation<'_, Option<crate::models::ServiceAccount>, Self::Error> {
        Box::pin(repo::get_service_account(self.0, id))
    }
    fn session(
        &mut self,
        digest: Vec<u8>,
    ) -> Operation<'_, Option<(crate::models::SessionRow, crate::models::User)>, Self::Error> {
        Box::pin(async move { repo::lookup_session(self.0, &digest).await })
    }
}

/// An absent authorization header differs from an empty/malformed one. The
/// adapter must preserve that distinction; never fall back to cookies on error.
#[derive(Clone, Copy)]
pub struct AuthInput<'a> {
    pub authorization: Option<&'a str>,
    pub session_cookie: Option<&'a str>,
    pub csrf_token: Option<&'a str>,
    /// HTTP method exactly as supplied by the HTTP adapter (normally uppercase).
    pub method: &'a str,
}
impl std::fmt::Debug for AuthInput<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthInput([credentials redacted])")
    }
}
fn unauthenticated(message: &'static str) -> IdentityError {
    DomainError::new(ErrorCode::Unauthenticated, message).into()
}
fn bearer_secret(authorization: &str) -> Result<&str> {
    let (scheme, secret) = authorization.split_once(' ').unwrap_or((authorization, ""));
    let secret =
        secret.trim_matches(|ch: char| ch.is_whitespace() || matches!(ch, '\u{001c}'..='\u{001f}'));
    if !scheme.eq_ignore_ascii_case("bearer") || secret.is_empty() {
        return Err(unauthenticated("malformed Authorization header"));
    }
    Ok(secret)
}
async fn from_bearer<R: Repository>(
    conn: &mut R,
    secret: &str,
) -> std::result::Result<Principal, R::Error> {
    let token = conn
        .token(digest(secret).to_vec())
        .await?
        .ok_or_else(|| unauthenticated("invalid, expired or revoked token"))?;
    let scopes = token.scopes.into_iter().collect();
    let via = Via {
        channel: Channel::Api,
        client: Some(format!("token:{}", token.name)),
    };
    if let Some(user_id) = token.user_id {
        let user = conn
            .user(user_id)
            .await?
            .ok_or_else(|| unauthenticated("token owner no longer exists"))?;
        return Ok(Principal::User(UserPrincipal {
            user_id: user.id,
            email: user.email,
            display_name: user.display_name,
            is_admin: user.is_admin,
            via,
            scopes,
            session_id: None,
            csrf_token: None,
        }));
    }
    let account_id = token
        .service_account_id
        .ok_or(IdentityError::CorruptData("token owner"))?;
    let account = conn
        .service(account_id)
        .await?
        .filter(|account| account.disabled_at.is_none())
        .ok_or_else(|| unauthenticated("service account is disabled"))?;
    Ok(Principal::Service(ServicePrincipal {
        service_account_id: account.id,
        project_id: account.project_id,
        kind: account.kind,
        name: account.name,
        via,
        scopes,
    }))
}
/// Bearer-only authentication for MCP; browser cookies are deliberately ignored.
/// The MCP adapter can change the resulting principal's channel with `with_channel`.
///
/// # Errors
/// Returns source-compatible unauthenticated errors or sanitized storage failures.
pub async fn bearer_principal(
    conn: &mut PgConnection,
    authorization: Option<&str>,
) -> Result<Principal> {
    let authorization =
        authorization.ok_or_else(|| unauthenticated("a Cannery Row bearer token is required"))?;
    bearer_with(&mut Legacy(conn), Some(authorization)).await
}
pub(crate) async fn bearer_with<R: Repository>(
    conn: &mut R,
    authorization: Option<&str>,
) -> std::result::Result<Principal, R::Error> {
    let authorization =
        authorization.ok_or_else(|| unauthenticated("a Cannery Row bearer token is required"))?;
    from_bearer(conn, bearer_secret(authorization)?).await
}
/// Resolve bearer first, otherwise an active browser session. Missing/expired
/// cookies return None; malformed/expired bearer credentials always return errors.
/// Calls update last-seen/last-used within the caller's transaction.
///
/// # Errors
/// Cookie-authenticated unsafe methods require constant-time CSRF equality.
pub async fn resolve_principal(
    conn: &mut PgConnection,
    input: AuthInput<'_>,
) -> Result<Option<Principal>> {
    resolve_with(&mut Legacy(conn), input).await
}
pub(crate) async fn resolve_with<R: Repository>(
    conn: &mut R,
    input: AuthInput<'_>,
) -> std::result::Result<Option<Principal>, R::Error> {
    if let Some(authorization) = input.authorization {
        return from_bearer(conn, bearer_secret(authorization)?)
            .await
            .map(Some);
    }
    let Some(cookie) = input.session_cookie.filter(|cookie| !cookie.is_empty()) else {
        return Ok(None);
    };
    let Some((session, user)) = conn.session(digest(cookie).to_vec()).await? else {
        return Ok(None);
    };
    check_csrf(input.method, input.csrf_token, session.csrf_token.expose())?;
    Ok(Some(Principal::User(UserPrincipal {
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
        csrf_token: Some(session.csrf_token),
    })))
}
/// Resolve a mandatory principal using the same precedence and CSRF rules.
///
/// # Errors
/// Missing credentials produce the `unauthenticated` domain error.
pub async fn current_principal(conn: &mut PgConnection, input: AuthInput<'_>) -> Result<Principal> {
    current_with(&mut Legacy(conn), input).await
}
pub(crate) async fn current_with<R: Repository>(
    conn: &mut R,
    input: AuthInput<'_>,
) -> std::result::Result<Principal, R::Error> {
    resolve_with(conn, input)
        .await?
        .ok_or_else(|| unauthenticated("authentication required").into())
}
fn check_csrf(method: &str, sent: Option<&str>, expected: &str) -> Result<()> {
    if matches!(method, "POST" | "PUT" | "PATCH" | "DELETE")
        && !bool::from(sent.unwrap_or("").as_bytes().ct_eq(expected.as_bytes()))
    {
        return Err(
            DomainError::new(ErrorCode::CsrfInvalid, "missing or invalid CSRF token").into(),
        );
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_bearer_partition_rules() -> Result<()> {
        for header in [
            "Bearer token",
            "bEaReR   token \t",
            "Bearer \ttoken",
            "Bearer \u{001c}token\u{001f}",
        ] {
            assert_eq!(bearer_secret(header)?, "token");
        }
        for header in [
            "",
            "Bearer",
            "Bearer\ttoken",
            " Bearer token",
            "Basic token",
            "Bearer \t",
        ] {
            assert!(bearer_secret(header).is_err());
        }
        Ok(())
    }
    #[test]
    fn csrf_is_utf8_exact_and_unsafe_methods_only() -> Result<()> {
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            check_csrf(method, Some("é"), "é")?;
            assert!(check_csrf(method, Some("e\u{301}"), "é").is_err());
            assert!(check_csrf(method, None, "é").is_err());
        }
        for method in ["GET", "HEAD", "OPTIONS", "post", "OTHER"] {
            check_csrf(method, None, "é")?;
        }
        Ok(())
    }
}
