//! OIDC orchestration with explicit transport and signature-verification boundaries.
//! No network implementation, cryptographic backend, or production startup wiring.
use std::{
    fmt,
    sync::{Arc, Mutex},
    time::Instant,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use cannery_core::{
    json::{Document, Node},
    principal::Secret,
};
use futures_util::future::BoxFuture;

use crate::{
    oidc_claims::Identity,
    oidc_protocol::{
        self as protocol, AuthorizationParameters, MetadataCache, ProtocolError, RenderingContext,
    },
    oidc_provider::{AuthorizationRequest, OidcProvider, ProviderError},
};

pub struct ClientConfig {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: Secret,
    pub redirect_uri: String,
    pub scopes: String,
}
impl fmt::Debug for ClientConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ClientConfig([redacted])")
    }
}

/// The transport distinguishes caught HTTP/JSON/value errors from source exceptions
/// that escape the OIDC error boundary (notably JSON recursion).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportError {
    Rejected,
    Unhandled,
}

pub struct BasicCredentials<'a> {
    pub client_id: &'a String,
    pub client_secret: &'a Secret,
}
impl fmt::Debug for BasicCredentials<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BasicCredentials([redacted])")
    }
}

/// A real implementation must reproduce HTTPX's complete body read, 2xx status
/// policy, UTF-8 Basic credentials, Python JSON decoding and operation timeouts.
/// There is deliberately no reqwest default or approximate timeout implementation.
pub trait JsonTransport: Send + Sync {
    fn get_json<'a>(&'a self, url: &'a str)
    -> BoxFuture<'a, Result<Arc<Document>, TransportError>>;
    /// Perform the same URL preparation as POST, before form encoding. HTTPX
    /// constructs its URL first, so an uncaught invalid URL wins over bad form
    /// text. This required method has no permissive default; real implementations
    /// must apply their actual URL parser and source exception classification.
    /// # Errors
    /// Returns caught provider rejection or a distinct unhandled preparation error.
    fn prepare_post_url(&self, url: &str) -> Result<(), TransportError>;
    /// `form` is already encoded in source insertion order and contains secrets.
    fn post_form<'a>(
        &'a self,
        url: &'a str,
        form: &'a Secret,
        credentials: BasicCredentials<'a>,
    ) -> BoxFuture<'a, Result<Arc<Document>, TransportError>>;
}

/// Clock injection calibrates source corpus tests, not live lease/deadline clocks.
pub trait MonotonicClock: Send + Sync {
    fn now(&self) -> f64;
}

pub struct SystemMonotonicClock(Instant);
impl Default for SystemMonotonicClock {
    fn default() -> Self {
        Self(Instant::now())
    }
}
impl MonotonicClock for SystemMonotonicClock {
    fn now(&self) -> f64 {
        self.0.elapsed().as_secs_f64()
    }
}

/// Configured identity, not a claims projection or signature-verification result.
pub struct VerificationContext<'a> {
    pub issuer: &'a String,
    pub audience: &'a String,
}
impl fmt::Debug for VerificationContext<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("VerificationContext([redacted])")
    }
}

/// Future implementations own header parsing, source key filtering/construction,
/// signature verification and post-signature claims/nonce checks. Invalid signatures
/// alone allow another candidate; skippable key errors differ from unhandled failures.
/// This seam must never return an identity from unchecked claims in production.
pub trait TokenVerifier: Send + Sync {
    fn verify_id_token<'a>(
        &'a self,
        id_token: &'a str,
        nonce: &'a Secret,
        context: VerificationContext<'a>,
        keys: &'a dyn SigningKeySource,
    ) -> BoxFuture<'a, Result<Identity, ProviderError>>;
}

/// Start each signing-key lookup by invoking metadata, as the source does. The
/// verifier may subsequently invoke metadata again for the JWT issuer comparison.
pub trait SigningKeySource: Send + Sync {
    fn metadata(&self) -> BoxFuture<'_, Result<Arc<Document>, ProviderError>>;
    fn begin_signing_keys(&self) -> BoxFuture<'_, Result<SigningKeys<'_>, ProviderError>>;
}

/// Retains the metadata snapshot used at the start of one source key lookup.
/// Refresh uses that snapshot's URI even if another task refreshes discovery.
pub struct SigningKeys<'a> {
    client: &'a OidcClient,
    metadata: Arc<Document>,
}
impl SigningKeys<'_> {
    /// # Errors
    /// Preserves caught transport rejections versus unhandled failures.
    pub async fn document(&self, force_refresh: bool) -> Result<Arc<Document>, ProviderError> {
        self.client.jwks_for(&self.metadata, force_refresh).await
    }
}

pub struct OidcClient {
    config: ClientConfig,
    transport: Arc<dyn JsonTransport>,
    verifier: Arc<dyn TokenVerifier>,
    clock: Arc<dyn MonotonicClock>,
    rendering: RenderingContext,
    cache: Mutex<MetadataCache<Arc<Document>, Arc<Document>>>,
}
impl fmt::Debug for OidcClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OidcClient([redacted])")
    }
}

fn reject(message: &'static str) -> ProviderError {
    ProviderError::Rejected(String::from(message))
}
fn transport_error(error: TransportError, operation: &'static str) -> ProviderError {
    match error {
        TransportError::Rejected => reject(operation),
        TransportError::Unhandled => ProviderError::Unhandled,
    }
}
fn metadata_error(error: ProtocolError) -> ProviderError {
    match error {
        ProtocolError::Shape | ProtocolError::Issuer | ProtocolError::Endpoint => {
            reject("invalid OIDC discovery document")
        }
        ProtocolError::Encoding
        | ProtocolError::IntegerRendering
        | ProtocolError::UnhandledRecursion => ProviderError::Unhandled,
    }
}
fn string_field<'a>(document: &'a Document, field: &str) -> Option<&'a String> {
    match document.node(document.field(document.root(), field)?)? {
        Node::String(value) => Some(value),
        _ => None,
    }
}
fn random_secret(size: usize) -> Result<Secret, ProviderError> {
    let mut bytes = vec![0; size];
    getrandom::fill(&mut bytes).map_err(|_| ProviderError::Unhandled)?;
    Ok(Secret::new(URL_SAFE_NO_PAD.encode(bytes)))
}

impl OidcClient {
    /// Callers supply transport and verifier implementations; native metadata
    /// rendering uses `RenderingContext::DEFAULT` and its shared JSON nesting limit.
    /// # Errors
    /// Returns a sanitized unhandled failure if text invariants are violated.
    pub fn new(
        mut config: ClientConfig,
        transport: Arc<dyn JsonTransport>,
        verifier: Arc<dyn TokenVerifier>,
        clock: Arc<dyn MonotonicClock>,
        rendering: RenderingContext,
    ) -> Result<Self, ProviderError> {
        let end = config
            .issuer
            .codepoints()
            .iter()
            .rposition(|&point| point != 47)
            .map_or(0, |index| index + 1);
        config.issuer =
            cannery_core::text::from_codepoints(config.issuer.codepoints()[..end].to_vec())
                .ok_or(ProviderError::Unhandled)?;
        Ok(Self {
            config,
            transport,
            verifier,
            clock,
            rendering,
            cache: Mutex::new(MetadataCache::default()),
        })
    }

    async fn get_object(&self, url: &str) -> Result<Arc<Document>, ProviderError> {
        let document = self
            .transport
            .get_json(url)
            .await
            .map_err(|error| transport_error(error, "cannot fetch OIDC provider document"))?;
        if !matches!(document.node(document.root()), Some(Node::Object(_))) {
            return Err(reject("unexpected OIDC provider response"));
        }
        Ok(document)
    }

    async fn load_metadata(&self) -> Result<Arc<Document>, ProviderError> {
        {
            let cache = self.cache.lock().map_err(|_| ProviderError::Unhandled)?;
            if !cache.needs_refresh(self.clock.now()) {
                return cache.metadata().cloned().ok_or(ProviderError::Unhandled);
            }
        }
        // Do not serialize misses: source callers fetch independently. No cache
        // guard crosses an await; whichever successful response finishes last wins.
        let mut points = self.config.issuer.codepoints().clone();
        points.extend("/.well-known/openid-configuration".chars().map(u32::from));
        let url = cannery_core::text::from_codepoints(points).ok_or(ProviderError::Unhandled)?;
        let metadata = self.get_object(&url).await?;
        protocol::validate_metadata_with_context(&metadata, &self.config.issuer, self.rendering)
            .map_err(metadata_error)?;
        self.cache
            .lock()
            .map_err(|_| ProviderError::Unhandled)?
            .replace_metadata(Arc::clone(&metadata), self.clock.now());
        Ok(metadata)
    }

    async fn jwks_for(
        &self,
        metadata: &Document,
        force_refresh: bool,
    ) -> Result<Arc<Document>, ProviderError> {
        if !force_refresh {
            let cache = self.cache.lock().map_err(|_| ProviderError::Unhandled)?;
            if let Some(document) = cache.jwks() {
                return Ok(Arc::clone(document));
            }
        }
        let url = string_field(metadata, "jwks_uri").ok_or(ProviderError::Unhandled)?;
        let jwks = self.get_object(url).await?;
        self.cache
            .lock()
            .map_err(|_| ProviderError::Unhandled)?
            .replace_jwks(Arc::clone(&jwks));
        Ok(jwks)
    }
}

impl SigningKeySource for OidcClient {
    fn metadata(&self) -> BoxFuture<'_, Result<Arc<Document>, ProviderError>> {
        Box::pin(self.load_metadata())
    }
    fn begin_signing_keys(&self) -> BoxFuture<'_, Result<SigningKeys<'_>, ProviderError>> {
        Box::pin(async {
            Ok(SigningKeys {
                client: self,
                metadata: self.load_metadata().await?,
            })
        })
    }
}

impl OidcProvider for OidcClient {
    fn authorization_request(&self) -> BoxFuture<'_, Result<AuthorizationRequest, ProviderError>> {
        Box::pin(async {
            let metadata = self.load_metadata().await?;
            let state = random_secret(32)?;
            let nonce = random_secret(32)?;
            let code_verifier = random_secret(64)?;
            let endpoint = string_field(&metadata, "authorization_endpoint")
                .ok_or(ProviderError::Unhandled)?;
            let url = protocol::authorization_url(
                endpoint,
                &AuthorizationParameters {
                    client_id: &self.config.client_id,
                    redirect_uri: &self.config.redirect_uri,
                    scopes: &self.config.scopes,
                    state: &state,
                    nonce: &nonce,
                    verifier: &code_verifier,
                },
            )
            .map_err(|_| ProviderError::Unhandled)?;
            Ok(AuthorizationRequest {
                url: url.expose().clone(),
                state,
                nonce,
                code_verifier,
            })
        })
    }

    fn exchange_code<'a>(
        &'a self,
        code: &'a str,
        code_verifier: &'a Secret,
        nonce: &'a Secret,
    ) -> BoxFuture<'a, Result<Identity, ProviderError>> {
        Box::pin(async move {
            let metadata = self.load_metadata().await?;
            let endpoint =
                string_field(&metadata, "token_endpoint").ok_or(ProviderError::Unhandled)?;
            self.transport
                .prepare_post_url(endpoint)
                .map_err(|error| transport_error(error, "token exchange failed"))?;
            let verifier = String::from(code_verifier.expose());
            let form = protocol::ordered_urlencode(&[
                ("grant_type", &String::from("authorization_code")),
                ("code", code),
                ("redirect_uri", &self.config.redirect_uri),
                ("code_verifier", &verifier),
            ])
            .map_err(|_| reject("token exchange failed"))?;
            let response = self
                .transport
                .post_form(
                    endpoint,
                    &form,
                    BasicCredentials {
                        client_id: &self.config.client_id,
                        client_secret: &self.config.client_secret,
                    },
                )
                .await
                .map_err(|error| transport_error(error, "token exchange failed"))?;
            let token = string_field(&response, "id_token")
                .ok_or_else(|| reject("token response has no id_token"))?;
            self.verifier
                .verify_id_token(
                    token,
                    nonce,
                    VerificationContext {
                        issuer: &self.config.issuer,
                        audience: &self.config.client_id,
                    },
                    self,
                )
                .await
        })
    }

    fn end_session_url<'a>(
        &'a self,
        redirect: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ProviderError>> {
        Box::pin(async move {
            let metadata = self.load_metadata().await?;
            protocol::logout_url(
                string_field(&metadata, "end_session_endpoint").map(String::as_str),
                &self.config.client_id,
                redirect,
            )
            .map(|url| url.map(|url| url.expose().clone()))
            .map_err(|_| ProviderError::Unhandled)
        })
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
