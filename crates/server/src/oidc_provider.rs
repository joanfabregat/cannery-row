//! Object-safe OIDC backend boundary; cryptographic verification belongs to the provider.
use crate::oidc_claims::Identity;
use cannery_core::principal::Secret;
use futures_util::future::BoxFuture;
use std::fmt;

pub struct AuthorizationRequest {
    pub url: String,
    pub state: Secret,
    pub nonce: Secret,
    pub code_verifier: Secret,
}
impl fmt::Debug for AuthorizationRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuthorizationRequest([redacted])")
    }
}

/// Only source `OIDCError` rejections are caught by browser routes. Other source
/// exceptions reach the generic 500 boundary without native diagnostics.
pub enum ProviderError {
    Rejected(String),
    Unhandled,
}
impl fmt::Debug for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Rejected(_) => "ProviderError::Rejected([redacted])",
            Self::Unhandled => "ProviderError::Unhandled",
        })
    }
}
impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OIDC provider operation failed")
    }
}
impl std::error::Error for ProviderError {}

pub trait OidcProvider: Send + Sync {
    fn authorization_request(&self) -> BoxFuture<'_, Result<AuthorizationRequest, ProviderError>>;
    /// Return only a signature-verified, nonce-checked identity. Never project
    /// unchecked token claims into a browser account.
    fn exchange_code<'a>(
        &'a self,
        code: &'a str,
        code_verifier: &'a Secret,
        nonce: &'a Secret,
    ) -> BoxFuture<'a, Result<Identity, ProviderError>>;
    fn end_session_url<'a>(
        &'a self,
        post_logout_redirect_uri: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ProviderError>>;
}
