//! Native OIDC protocol projections; no network or JWT verifier.
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use cannery_core::{
    json::{Document, Node, NodeId},
    principal::Secret,
};
use sha2::{Digest, Sha256};
use std::fmt::Write;

/// UTF-8 provider URL with redacted diagnostics.
pub struct ProtocolUrl(String);
impl std::fmt::Debug for ProtocolUrl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ProtocolUrl([redacted])")
    }
}
impl ProtocolUrl {
    #[must_use]
    pub fn expose(&self) -> &String {
        &self.0
    }
    /// # Errors
    /// Retains the fallible transport interface for protocol callers.
    pub fn into_utf8_secret(self) -> Result<Secret, ProtocolError> {
        Ok(Secret::new(self.0))
    }
}
fn append_query(endpoint: &str, query: &Secret) -> ProtocolUrl {
    ProtocolUrl(format!("{endpoint}?{}", query.expose()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ProtocolError {
    #[error("OIDC text encoding failed")]
    Encoding,
    #[error("OIDC provider response is not an object")]
    Shape,
    #[error("OIDC discovery issuer mismatch")]
    Issuer,
    #[error("OIDC discovery endpoint is missing or not a string")]
    Endpoint,
    #[error("OIDC integer rendering exceeds the Python limit")]
    IntegerRendering,
    #[error("OIDC issuer rendering recursion limit exceeded")]
    UnhandledRecursion,
}

/// Native metadata rendering uses the shared JSON nesting limit.
#[derive(Clone, Copy, Debug)]
pub struct RenderingContext {
    pub nesting_budget: usize,
}
impl RenderingContext {
    /// Shared native JSON nesting limit for metadata and cache projections.
    pub const DEFAULT: Self = Self {
        nesting_budget: cannery_core::json::MAX_DEPTH,
    };
}

fn quote(text: &str) -> String {
    let mut result = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"_.-~".contains(&byte) {
            result.push(char::from(byte));
        } else if byte == b' ' {
            result.push('+');
        } else {
            let _ = write!(result, "%{byte:02X}");
        }
    }
    result
}

/// Encode ordered form pairs without sorting or endpoint normalization.
/// # Errors
/// Retains the fallible encoding interface for protocol callers.
pub fn ordered_urlencode(pairs: &[(&str, &str)]) -> Result<Secret, ProtocolError> {
    let mut result = String::new();
    for (index, (key, value)) in pairs.iter().enumerate() {
        if index != 0 {
            result.push('&');
        }
        result.push_str(&quote(key));
        result.push('=');
        result.push_str(&quote(value));
    }
    Ok(Secret::new(result))
}

#[must_use]
pub fn pkce_challenge(verifier: &Secret) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.expose().as_bytes()))
}

/// Callers generate unpredictable state, nonce and verifier separately.
/// Deliberately has no `Debug` implementation because the fields contain secrets.
pub struct AuthorizationParameters<'a> {
    pub client_id: &'a String,
    pub redirect_uri: &'a String,
    pub scopes: &'a String,
    pub state: &'a Secret,
    pub nonce: &'a Secret,
    pub verifier: &'a Secret,
}

/// Append a literal question mark, even when an endpoint already has a query.
/// # Errors
/// Returns a sanitized text encoding error.
pub fn authorization_url(
    endpoint: &str,
    parameters: &AuthorizationParameters<'_>,
) -> Result<ProtocolUrl, ProtocolError> {
    let state = String::from(parameters.state.expose());
    let nonce = String::from(parameters.nonce.expose());
    let challenge = String::from(&pkce_challenge(parameters.verifier));
    let query = ordered_urlencode(&[
        ("response_type", &String::from("code")),
        ("client_id", parameters.client_id),
        ("redirect_uri", parameters.redirect_uri),
        ("scope", parameters.scopes),
        ("state", &state),
        ("nonce", &nonce),
        ("code_challenge", &challenge),
        ("code_challenge_method", &String::from("S256")),
    ])?;
    Ok(append_query(endpoint, &query))
}

/// Accept hostful HTTPS endpoints or HTTP on the three canonical loopback hosts.
/// Reject credentials, controls, whitespace, backslashes and malformed ports
/// before the standard URL parser can normalize ambiguous input.
#[must_use]
pub fn is_safe_endpoint(value: &str) -> bool {
    if value
        .chars()
        .any(|character| character.is_control() || character.is_whitespace() || character == '\\')
    {
        return false;
    }
    let Some((_, tail)) = value.split_once("://") else {
        return false;
    };
    let authority = tail.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') || authority.ends_with(':') {
        return false;
    }
    let Ok(endpoint) = url::Url::parse(value) else {
        return false;
    };
    if endpoint.host().is_none() || !endpoint.username().is_empty() || endpoint.password().is_some()
    {
        return false;
    }
    match endpoint.scheme() {
        "https" => true,
        "http" => {
            let expected = match endpoint.host() {
                Some(url::Host::Domain(host)) if host.eq_ignore_ascii_case("localhost") => {
                    "localhost"
                }
                Some(url::Host::Ipv4(address)) if address.octets() == [127, 0, 0, 1] => "127.0.0.1",
                Some(url::Host::Ipv6(address)) if address == std::net::Ipv6Addr::LOCALHOST => {
                    "[::1]"
                }
                _ => return false,
            };
            // Check only canonical host spelling. Url owns all actual URL,
            // bracket, address and port parsing, including the 16-bit port bound.
            if authority.eq_ignore_ascii_case(expected) {
                return true;
            }
            authority
                .get(..expected.len())
                .is_some_and(|host| host.eq_ignore_ascii_case(expected))
                && authority
                    .get(expected.len()..)
                    .and_then(|suffix| suffix.strip_prefix(':'))
                    .is_some_and(|port| {
                        !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
                    })
        }
        _ => false,
    }
}

/// # Errors
/// Returns encoding failures only after an endpoint passes the native policy.
pub fn logout_url(
    endpoint: Option<&str>,
    client_id: &str,
    redirect_uri: &str,
) -> Result<Option<ProtocolUrl>, ProtocolError> {
    let Some(endpoint) = endpoint.filter(|v| is_safe_endpoint(v)) else {
        return Ok(None);
    };
    let query = ordered_urlencode(&[
        ("client_id", client_id),
        ("post_logout_redirect_uri", redirect_uri),
    ])?;
    Ok(Some(append_query(endpoint, &query)))
}

/// Render JSON values with native serde formatting.
/// # Errors
/// Returns sanitized model or nesting errors.
pub fn python_str(document: &Document, id: NodeId) -> Result<String, ProtocolError> {
    python_str_with_context(document, id, RenderingContext::DEFAULT)
}
/// Native JSON string rendering with an explicit nesting limit.
/// # Errors
/// Returns integer-limit or distinct unhandled recursion failures.
pub fn python_str_with_context(
    document: &Document,
    id: NodeId,
    context: RenderingContext,
) -> Result<String, ProtocolError> {
    use cannery_core::text::{self, RenderError};
    text::str_value(document, id, context.nesting_budget).map_err(|error| match error {
        RenderError::InvalidNode => ProtocolError::Shape,
        RenderError::IntegerLimit => ProtocolError::IntegerRendering,
        RenderError::Recursion => ProtocolError::UnhandledRecursion,
        RenderError::Encoding => ProtocolError::Encoding,
    })
}

/// Validate source discovery object/issuer/endpoint types, without URL safety checks.
/// # Errors
/// Returns sanitized shape, issuer, endpoint or rendering failures in source order.
pub fn validate_metadata(document: &Document, issuer: &String) -> Result<(), ProtocolError> {
    validate_metadata_with_context(document, issuer, RenderingContext::DEFAULT)
}
/// Discovery validation with an explicit native nesting limit.
/// # Errors
/// Unhandled source recursion remains distinct from OIDC discovery rejection.
pub fn validate_metadata_with_context(
    document: &Document,
    issuer: &String,
    context: RenderingContext,
) -> Result<(), ProtocolError> {
    if !matches!(document.node(document.root()), Some(Node::Object(_))) {
        return Err(ProtocolError::Shape);
    }
    let returned = document
        .field(document.root(), "issuer")
        .map(|id| python_str_with_context(document, id, context))
        .transpose()?
        .unwrap_or_else(String::new);
    let trim = |value: &String| {
        value
            .codepoints()
            .iter()
            .copied()
            .rev()
            .skip_while(|&v| v == 47)
            .collect::<Vec<_>>()
    };
    if trim(&returned) != trim(issuer) {
        return Err(ProtocolError::Issuer);
    }
    for key in ["authorization_endpoint", "token_endpoint", "jwks_uri"] {
        if !matches!(
            document
                .field(document.root(), key)
                .and_then(|id| document.node(id)),
            Some(Node::String(_))
        ) {
            return Err(ProtocolError::Endpoint);
        }
    }
    Ok(())
}

/// Cache ownership stays generic; metadata refresh invalidates JWKS on success only.
/// This type intentionally avoids `Debug` for untrusted provider values.
pub struct MetadataCache<M, J> {
    metadata: Option<M>,
    metadata_at: f64,
    jwks: Option<J>,
}
impl<M, J> Default for MetadataCache<M, J> {
    fn default() -> Self {
        Self {
            metadata: None,
            metadata_at: 0.0,
            jwks: None,
        }
    }
}
impl<M, J> MetadataCache<M, J> {
    #[must_use]
    pub fn needs_refresh(&self, now: f64) -> bool {
        self.metadata.is_none() || now - self.metadata_at > 3600.0
    }
    pub fn replace_metadata(&mut self, metadata: M, now: f64) {
        self.metadata = Some(metadata);
        self.metadata_at = now;
        self.jwks = None;
    }
    pub fn replace_jwks(&mut self, jwks: J) {
        self.jwks = Some(jwks);
    }
    #[must_use]
    pub fn metadata(&self) -> Option<&M> {
        self.metadata.as_ref()
    }
    #[must_use]
    pub fn jwks(&self) -> Option<&J> {
        self.jwks.as_ref()
    }
}

impl<J> MetadataCache<Document, J> {
    /// Validate before changing metadata, timestamp or JWKS ownership.
    /// # Errors
    /// Invalid discovery leaves the previously cached state untouched.
    pub fn accept_metadata(
        &mut self,
        metadata: Document,
        issuer: &String,
        now: f64,
    ) -> Result<(), ProtocolError> {
        self.accept_metadata_with_context(metadata, issuer, now, RenderingContext::DEFAULT)
    }
    /// Validate with caller-specific rendering depth before updating any cache state.
    /// # Errors
    /// Rejections and unhandled rendering recursion preserve previous cache ownership.
    pub fn accept_metadata_with_context(
        &mut self,
        metadata: Document,
        issuer: &String,
        now: f64,
        context: RenderingContext,
    ) -> Result<(), ProtocolError> {
        validate_metadata_with_context(&metadata, issuer, context)?;
        self.replace_metadata(metadata, now);
        Ok(())
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
