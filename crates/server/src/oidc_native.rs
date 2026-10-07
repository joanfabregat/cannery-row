//! Production OIDC signatures and bounded rustls HTTP transport.
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use cannery_core::{
    json::{self, Document, Node},
    principal::Secret,
};
use futures_util::future::BoxFuture;
use ring::signature;
use serde_json::Value;

use crate::{
    oidc_claims::{self, ClaimContext, ClockTime, Identity},
    oidc_client::{
        BasicCredentials, JsonTransport, SigningKeySource, TokenVerifier, TransportError,
        VerificationContext,
    },
    oidc_provider::ProviderError,
};

const MAX_TOKEN: usize = 64 * 1024;
const MAX_RESPONSE: usize = 1024 * 1024;
const JSON_DEPTH: usize = 128;

fn rejected(message: &'static str) -> ProviderError {
    ProviderError::Rejected(String::from(message))
}

/// Uses standard rustls trust roots, no redirects, a ten-second whole-request
/// deadline and a one-MiB decoded response limit. These are production policies,
/// not an emulation of HTTPX's separate operation timeout accounting.
pub struct HttpTransport(reqwest::Client);
impl HttpTransport {
    /// # Errors
    /// Returns a value-free startup error if the HTTP client cannot be built.
    pub fn new() -> Result<Self, ProviderError> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(10))
            .build()
            .map(Self)
            .map_err(|_| ProviderError::Unhandled)
    }
    async fn response(request: reqwest::RequestBuilder) -> Result<Arc<Document>, TransportError> {
        let mut response = request.send().await.map_err(|_| TransportError::Rejected)?;
        if !response.status().is_success() {
            return Err(TransportError::Rejected);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| TransportError::Rejected)?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE {
                return Err(TransportError::Rejected);
            }
            bytes.extend_from_slice(&chunk);
        }
        json::decode(&bytes, JSON_DEPTH)
            .map(Arc::new)
            .map_err(|_| TransportError::Rejected)
    }
    fn url(value: &str) -> Result<reqwest::Url, TransportError> {
        let url = reqwest::Url::parse(value).map_err(|_| TransportError::Rejected)?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(TransportError::Rejected);
        }
        Ok(url)
    }
}
impl JsonTransport for HttpTransport {
    fn get_json<'a>(
        &'a self,
        url: &'a str,
    ) -> BoxFuture<'a, Result<Arc<Document>, TransportError>> {
        Box::pin(async move { Self::response(self.0.get(Self::url(url)?)).await })
    }
    fn prepare_post_url(&self, url: &str) -> Result<(), TransportError> {
        Self::url(url).map(|_| ())
    }
    fn post_form<'a>(
        &'a self,
        url: &'a str,
        form: &'a Secret,
        credentials: BasicCredentials<'a>,
    ) -> BoxFuture<'a, Result<Arc<Document>, TransportError>> {
        Box::pin(async move {
            let client = credentials
                .client_id
                .as_utf8()
                .ok_or(TransportError::Rejected)?;
            Self::response(
                self.0
                    .post(Self::url(url)?)
                    .basic_auth(client, Some(credentials.client_secret.expose()))
                    .header(
                        reqwest::header::CONTENT_TYPE,
                        "application/x-www-form-urlencoded",
                    )
                    .body(form.expose().to_owned()),
            )
            .await
        })
    }
}

#[derive(Clone, Copy)]
enum Algorithm {
    Rs256,
    Rs384,
    Rs512,
    Ps256,
    Es256,
    Es384,
    Ed25519,
}
impl Algorithm {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "RS256" => Self::Rs256,
            "RS384" => Self::Rs384,
            "RS512" => Self::Rs512,
            "PS256" => Self::Ps256,
            "ES256" => Self::Es256,
            "ES384" => Self::Es384,
            "EdDSA" => Self::Ed25519,
            _ => return None,
        })
    }
}

enum Key {
    Rsa { n: Vec<u8>, e: Vec<u8> },
    Ec(Vec<u8>),
    Ed25519(Vec<u8>),
}
fn binary(value: &Value, name: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(value.get(name)?.as_str()?).ok()
}
fn unsigned(bytes: &[u8]) -> &[u8] {
    &bytes[bytes
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(bytes.len())..]
}
fn bit_length(bytes: &[u8]) -> usize {
    let bytes = unsigned(bytes);
    bytes
        .first()
        .map_or(0, |first| bytes.len() * 8 - first.leading_zeros() as usize)
}
impl Key {
    fn from_jwk(jwk: &Value, algorithm: Algorithm) -> Option<Self> {
        if jwk.get("key_ops").is_some_and(|ops| {
            !ops.as_array()
                .is_some_and(|ops| ops.iter().any(|op| op == "verify"))
        }) {
            return None;
        }
        match algorithm {
            Algorithm::Rs256 | Algorithm::Rs384 | Algorithm::Rs512 | Algorithm::Ps256 => {
                if jwk.get("kty")?.as_str()? != "RSA" {
                    return None;
                }
                let n = binary(jwk, "n")?;
                let e = binary(jwk, "e")?;
                if !(2048..=8192).contains(&bit_length(&n))
                    || !(2..=33).contains(&bit_length(&e))
                    || e.last()? & 1 == 0
                {
                    return None;
                }
                Some(Self::Rsa { n, e })
            }
            Algorithm::Es256 | Algorithm::Es384 => {
                let (curve, length) = match algorithm {
                    Algorithm::Es256 => ("P-256", 32),
                    _ => ("P-384", 48),
                };
                if jwk.get("kty")?.as_str()? != "EC" || jwk.get("crv")?.as_str()? != curve {
                    return None;
                }
                let x = binary(jwk, "x")?;
                let y = binary(jwk, "y")?;
                if x.len() != length || y.len() != length {
                    return None;
                }
                let mut point = Vec::with_capacity(1 + length * 2);
                point.push(4);
                point.extend(x);
                point.extend(y);
                Some(Self::Ec(point))
            }
            Algorithm::Ed25519 => {
                if jwk.get("kty")?.as_str()? != "OKP" || jwk.get("crv")?.as_str()? != "Ed25519" {
                    return None;
                }
                let key = binary(jwk, "x")?;
                (key.len() == 32).then_some(Self::Ed25519(key))
            }
        }
    }
    fn verify(&self, algorithm: Algorithm, message: &[u8], sig: &[u8]) -> bool {
        match (self, algorithm) {
            (
                Self::Rsa { n, e },
                Algorithm::Rs256 | Algorithm::Rs384 | Algorithm::Rs512 | Algorithm::Ps256,
            ) => {
                let parameters = match algorithm {
                    Algorithm::Rs256 => &signature::RSA_PKCS1_2048_8192_SHA256,
                    Algorithm::Rs384 => &signature::RSA_PKCS1_2048_8192_SHA384,
                    Algorithm::Rs512 => &signature::RSA_PKCS1_2048_8192_SHA512,
                    _ => &signature::RSA_PSS_2048_8192_SHA256,
                };
                signature::RsaPublicKeyComponents {
                    n: unsigned(n),
                    e: unsigned(e),
                }
                .verify(parameters, message, sig)
                .is_ok()
            }
            (Self::Ec(key), Algorithm::Es256) => {
                signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, key)
                    .verify(message, sig)
                    .is_ok()
            }
            (Self::Ec(key), Algorithm::Es384) => {
                signature::UnparsedPublicKey::new(&signature::ECDSA_P384_SHA384_FIXED, key)
                    .verify(message, sig)
                    .is_ok()
            }
            (Self::Ed25519(key), Algorithm::Ed25519) => {
                signature::UnparsedPublicKey::new(&signature::ED25519, key)
                    .verify(message, sig)
                    .is_ok()
            }
            _ => false,
        }
    }
}

struct Token<'a> {
    algorithm: Algorithm,
    name: String,
    kid: Option<String>,
    message: &'a [u8],
    signature: Vec<u8>,
    payload: &'a str,
}
impl<'a> Token<'a> {
    fn parse(value: &'a str) -> Result<Self, ProviderError> {
        if value.len() > MAX_TOKEN {
            return Err(rejected("id_token exceeds the supported size"));
        }
        let (message, encoded_signature) = value
            .rsplit_once('.')
            .ok_or_else(|| rejected("invalid id_token"))?;
        let (header, payload) = message
            .split_once('.')
            .ok_or_else(|| rejected("invalid id_token"))?;
        if payload.contains('.') {
            return Err(rejected("invalid id_token"));
        }
        let header = URL_SAFE_NO_PAD
            .decode(header)
            .map_err(|_| rejected("invalid id_token header"))?;
        let header: Value =
            serde_json::from_slice(&header).map_err(|_| rejected("invalid id_token header"))?;
        if header.get("crit").is_some() || header.get("b64").is_some() {
            return Err(rejected("unsupported id_token header extension"));
        }
        let name = header
            .get("alg")
            .and_then(Value::as_str)
            .ok_or_else(|| rejected("invalid id_token algorithm"))?
            .to_owned();
        let algorithm =
            Algorithm::parse(&name).ok_or_else(|| rejected("unsupported id_token algorithm"))?;
        let kid = header
            .get("kid")
            .map(|kid| {
                kid.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| rejected("invalid signing key identifier"))
            })
            .transpose()?;
        let signature = URL_SAFE_NO_PAD
            .decode(encoded_signature)
            .map_err(|_| rejected("invalid id_token signature"))?;
        Ok(Self {
            algorithm,
            name,
            kid,
            message: message.as_bytes(),
            signature,
            payload,
        })
    }
    fn candidates(&self, document: &Document) -> Result<Vec<Key>, ProviderError> {
        let bytes = json::encode_http(document, JSON_DEPTH)
            .map_err(|_| rejected("invalid signing key document"))?;
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| rejected("invalid signing key document"))?;
        let keys = value
            .get("keys")
            .and_then(Value::as_array)
            .ok_or_else(|| rejected("invalid signing key document"))?;
        Ok(keys
            .iter()
            .filter(|jwk| {
                self.kid
                    .as_ref()
                    .is_none_or(|kid| jwk.get("kid").and_then(Value::as_str) == Some(kid))
                    && jwk.get("use").is_none_or(|usage| usage == "sig")
                    && jwk
                        .get("alg")
                        .is_none_or(|algorithm| algorithm == &self.name)
            })
            .filter_map(|jwk| Key::from_jwk(jwk, self.algorithm))
            .collect())
    }
    fn claims(
        &self,
        candidates: &[Key],
        context: &ClaimContext<'_>,
    ) -> Result<Identity, ProviderError> {
        if !candidates
            .iter()
            .any(|key| key.verify(self.algorithm, self.message, &self.signature))
        {
            return Err(rejected("invalid id_token signature"));
        }
        let payload = URL_SAFE_NO_PAD
            .decode(self.payload)
            .map_err(|_| rejected("invalid id_token payload"))?;
        let payload =
            json::decode(&payload, JSON_DEPTH).map_err(|_| rejected("invalid id_token payload"))?;
        oidc_claims::validate_claims(&payload, context)
            .map_err(|error| ProviderError::Rejected(String::from(&error.oidc_message())))
    }
}

/// The only production token verifier: authenticates signatures before claims.
pub struct RingVerifier;
impl TokenVerifier for RingVerifier {
    fn verify_id_token<'a>(
        &'a self,
        id_token: &'a str,
        nonce: &'a Secret,
        context: VerificationContext<'a>,
        keys: &'a dyn SigningKeySource,
    ) -> BoxFuture<'a, Result<Identity, ProviderError>> {
        Box::pin(async move {
            let token = Token::parse(id_token)?;
            let lookup = keys.begin_signing_keys().await?;
            let mut candidates = token.candidates(lookup.document(false).await?.as_ref())?;
            if candidates.is_empty() {
                candidates = token.candidates(lookup.document(true).await?.as_ref())?;
            }
            if candidates.is_empty() {
                return Err(rejected("no supported OIDC signing key"));
            }
            let metadata = keys.metadata().await?;
            let issuer = metadata
                .field(metadata.root(), "issuer")
                .and_then(|node| match metadata.node(node) {
                    Some(Node::String(value)) => Some(value),
                    _ => None,
                })
                .ok_or_else(|| rejected("invalid OIDC issuer"))?;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| ProviderError::Unhandled)?;
            let nonce = String::from(nonce.expose());
            token.claims(
                &candidates,
                &ClaimContext {
                    stored_issuer: context.issuer,
                    token_issuer: issuer,
                    audience: context.audience,
                    nonce: &nonce,
                    now: ClockTime::new(now.as_secs_f64()).ok_or(ProviderError::Unhandled)?,
                },
            )
        })
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "Assertions over published fixture constants and test-only local servers"
)]
mod tests {
    use super::*;
    use crate::{
        oidc_client::{ClientConfig, OidcClient, SystemMonotonicClock},
        oidc_protocol::RenderingContext,
        oidc_provider::OidcProvider,
    };
    use axum::{
        Json, Router,
        routing::{get, post},
    };
    use ring::signature::KeyPair;
    use serde_json::json;

    #[test]
    fn actual_python_signatures_follow_the_supported_policy() {
        let vectors: Vec<Value> = serde_json::from_str(runtime_reference!(
            "/tests/fixtures/oidc_crypto_vectors.json"
        ))
        .unwrap();
        assert_eq!(vectors.len(), 10);
        let issuer = String::from("https://oidc.fixture.invalid");
        let audience = String::from("public-fixture-client");
        let nonce = String::from("public-fixture-nonce");
        let context = ClaimContext {
            stored_issuer: &issuer,
            token_issuer: &issuer,
            audience: &audience,
            nonce: &nonce,
            now: ClockTime::new(1_767_225_600.0).unwrap(),
        };
        for vector in vectors {
            let text = vector["token"].as_str().unwrap();
            let supported = vector["supported"].as_bool().unwrap();
            let result = Token::parse(text).and_then(|token| {
                let doc = json::decode(
                    &serde_json::to_vec(&json!({"keys":[vector["jwk"].clone()]})).unwrap(),
                    JSON_DEPTH,
                )
                .unwrap();
                token.claims(&token.candidates(&doc)?, &context)
            });
            assert_eq!(result.is_ok(), supported, "{}", vector["name"]);
            if supported {
                let mut token = Token::parse(text).unwrap();
                let doc = json::decode(
                    &serde_json::to_vec(&json!({"keys":[vector["jwk"].clone()]})).unwrap(),
                    JSON_DEPTH,
                )
                .unwrap();
                let keys = token.candidates(&doc).unwrap();
                token.signature[0] ^= 1;
                assert!(token.claims(&keys, &context).is_err());
            }
        }
    }

    #[test]
    fn algorithm_confusion_and_unsupported_extensions_are_rejected() {
        for header in [
            json!({"alg":"none"}),
            json!({"alg":"HS256"}),
            json!({"alg":"ES512"}),
            json!({"alg":"EdDSA","crit":["x"]}),
            json!({"alg":"EdDSA","b64":false}),
            json!({"alg":"EdDSA","kid":1}),
        ] {
            let token = format!(
                "{}.e30.AA",
                URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap())
            );
            assert!(Token::parse(&token).is_err());
        }
        let vectors: Vec<Value> = serde_json::from_str(runtime_reference!(
            "/tests/fixtures/oidc_crypto_vectors.json"
        ))
        .unwrap();
        let mut jwk = vectors[4]["jwk"].clone();
        assert!(Key::from_jwk(&jwk, Algorithm::Es384).is_none());
        jwk["key_ops"] = json!(["sign"]);
        assert!(Key::from_jwk(&jwk, Algorithm::Es256).is_none());
    }

    #[tokio::test]
    async fn actual_http_exchange_authenticates_only_verified_claims() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        // Published deterministic test key; no credential is stored or generated.
        let pair = signature::Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = json!({"iss":issuer,"aud":"test-client","sub":"subject","iat":now,"exp":now+300,"nonce":"test-nonce","email":"test@example.invalid","email_verified":true});
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"EdDSA","kid":"test"}"#),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
        );
        let token = format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(pair.sign(signing_input.as_bytes()).as_ref())
        );
        let jwks = json!({"keys":[{"kty":"OKP","crv":"Ed25519","kid":"test","x":URL_SAFE_NO_PAD.encode(pair.public_key().as_ref())}]});
        let metadata = json!({"issuer":issuer,"authorization_endpoint":format!("{issuer}/authorize"),"token_endpoint":format!("{issuer}/token"),"jwks_uri":format!("{issuer}/jwks")});
        let app = Router::new()
            .route(
                "/.well-known/openid-configuration",
                get(move || async move { Json(metadata) }),
            )
            .route("/jwks", get(move || async move { Json(jwks) }))
            .route(
                "/token",
                post(
                    move |headers: axum::http::HeaderMap, body: String| async move {
                        assert_eq!(
                            headers.get("authorization").unwrap(),
                            "Basic dGVzdC1jbGllbnQ6dGVzdC1zZWNyZXQ="
                        );
                        assert!(body.contains("grant_type=authorization_code"));
                        Json(json!({"id_token":token}))
                    },
                ),
            );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = OidcClient::new(
            ClientConfig {
                issuer: String::from(&issuer),
                client_id: String::from("test-client"),
                client_secret: Secret::new("test-secret".to_owned()),
                redirect_uri: String::from("http://localhost/callback"),
                scopes: String::from("openid email"),
            },
            Arc::new(HttpTransport::new().unwrap()),
            Arc::new(RingVerifier),
            Arc::new(SystemMonotonicClock::default()),
            RenderingContext::DEFAULT,
        )
        .unwrap();
        let code = String::from("public-code");
        let verifier = Secret::new("public-code-verifier".to_owned());
        let identity = client
            .exchange_code(&code, &verifier, &Secret::new("test-nonce".to_owned()))
            .await
            .unwrap();
        assert!(identity.subject.equals_utf8("subject"));
        assert!(identity.email_verified);
        assert!(
            client
                .exchange_code(&code, &verifier, &Secret::new("wrong-nonce".to_owned()))
                .await
                .is_err()
        );
        server.abort();
        let _ = server.await;
    }
}
