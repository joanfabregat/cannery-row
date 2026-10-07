//! Test-only OIDC provider. It has no deployment role in the production server.
use crate::Result;
use axum::{
    Form, Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
    signature::{self, KeyPair},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

pub const CLIENT_ID: &str = "cannery-test";
pub const CLIENT_SECRET: &str = "test-client-secret";

/// Fixed startup profile; metadata never changes underneath a relying party cache.
#[derive(Clone, Copy)]
pub enum JwksMode {
    Plain,
    Decoys,
    Unusable,
}
/// Provider logout endpoint profiles exercised by the black-box login tests.
#[derive(Clone, Copy)]
pub enum LogoutMode {
    Default,
    None,
    RemoteHttp,
    Javascript,
    HostlessHttps,
    LocalhostHttp,
    RemoteHttps,
}

#[derive(Clone)]
struct Provider {
    issuer: String,
    control_token: String,
    key: Arc<signature::EcdsaKeyPair>,
    bad_key: Arc<signature::EcdsaKeyPair>,
    decoy_key: Arc<signature::EcdsaKeyPair>,
    jwks_mode: JwksMode,
    logout_mode: LogoutMode,
    jwks_requests: Arc<AtomicU64>,
    codes: Arc<Mutex<BTreeMap<String, Pending>>>,
}
struct Pending {
    nonce: String,
    challenge: String,
    claims: Value,
    nonce_override: Option<String>,
    bad_signature: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Approval {
    authorization_url: String,
    #[serde(default)]
    claims: BTreeMap<String, Value>,
    nonce_override: Option<String>,
    #[serde(default)]
    bad_signature: bool,
}

fn key() -> Result<signature::EcdsaKeyPair> {
    let rng = SystemRandom::new();
    let pkcs8 =
        signature::EcdsaKeyPair::generate_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .map_err(|_| "key generation failed")?;
    Ok(signature::EcdsaKeyPair::from_pkcs8(
        &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        pkcs8.as_ref(),
        &rng,
    )
    .map_err(|_| "key parsing failed")?)
}

/// Build the standalone test provider's routes and ephemeral signing keys.
///
/// # Errors
/// Returns an error for empty control tokens, invalid issuers or key generation.
pub fn router(issuer: &str, control_token: &str) -> Result<Router> {
    router_with_modes(issuer, control_token, JwksMode::Plain, LogoutMode::Default)
}

/// Build a provider with fixed, explicit security test profiles.
///
/// # Errors
/// Returns the same configuration and key-generation errors as [`router`].
pub fn router_with_modes(
    issuer: &str,
    control_token: &str,
    jwks_mode: JwksMode,
    logout_mode: LogoutMode,
) -> Result<Router> {
    if control_token.is_empty() {
        return Err("OIDC control token must not be empty".into());
    }
    let url = reqwest::Url::parse(issuer)?;
    if url.path() != "/oidc" || url.query().is_some() || url.fragment().is_some() {
        return Err("test issuer must end exactly in /oidc".into());
    }
    let provider = Provider {
        issuer: issuer.to_owned(),
        control_token: control_token.to_owned(),
        key: Arc::new(key()?),
        bad_key: Arc::new(key()?),
        decoy_key: Arc::new(key()?),
        jwks_mode,
        logout_mode,
        jwks_requests: Arc::default(),
        codes: Arc::default(),
    };
    Ok(Router::new()
        .route("/oidc/.well-known/openid-configuration", get(discovery))
        .route("/oidc/jwks", get(jwks))
        .route("/oidc/token", post(token))
        .route("/__conformance/approve", post(approve))
        .route("/__conformance/oidc/stats", get(stats))
        .with_state(provider))
}

async fn discovery(State(p): State<Provider>) -> Json<Value> {
    let logout = match p.logout_mode {
        LogoutMode::Default => Some(format!("{}/session/end", p.issuer)),
        LogoutMode::None => None,
        LogoutMode::RemoteHttp => Some("http://identity.invalid/logout".into()),
        LogoutMode::Javascript => Some("javascript:alert(1)//".into()),
        LogoutMode::HostlessHttps => Some("https:///no-host".into()),
        LogoutMode::LocalhostHttp => Some("http://localhost:3001/oidc/session/end".into()),
        LogoutMode::RemoteHttps => Some("https://identity.example/logout".into()),
    };
    Json(
        json!({"issuer":p.issuer,"authorization_endpoint":format!("{}/auth",p.issuer),"token_endpoint":format!("{}/token",p.issuer),"jwks_uri":format!("{}/jwks",p.issuer),"end_session_endpoint":logout,"response_types_supported":["code"],"subject_types_supported":["public"],"id_token_signing_alg_values_supported":["ES256"]}),
    )
}
async fn jwks(State(p): State<Provider>) -> Json<Value> {
    p.jwks_requests.fetch_add(1, Ordering::Relaxed);
    let mut keys = Vec::new();
    if !matches!(p.jwks_mode, JwksMode::Plain) {
        let public = public_jwk(&p.decoy_key);
        let mut encryption = public.clone();
        encryption["use"] = json!("enc");
        let mut algorithm = public.clone();
        algorithm["alg"] = json!("HS256");
        keys.extend([
            encryption,
            algorithm,
            json!({"kty":"oct","kid":"test-key","use":"sig","alg":"ES256","k":"invalid-for-ES256"}),
        ]);
        if matches!(p.jwks_mode, JwksMode::Decoys) {
            keys.push(public);
        }
    }
    if !matches!(p.jwks_mode, JwksMode::Unusable) {
        keys.push(public_jwk(&p.key));
    }
    Json(json!({"keys":keys}))
}
fn public_jwk(key: &signature::EcdsaKeyPair) -> Value {
    let public = key.public_key().as_ref();
    json!({"kty":"EC","crv":"P-256","x":URL_SAFE_NO_PAD.encode(&public[1..33]),"y":URL_SAFE_NO_PAD.encode(&public[33..65]),"kid":"test-key","alg":"ES256","use":"sig"})
}
async fn stats(State(p): State<Provider>, headers: HeaderMap) -> (StatusCode, Json<Value>) {
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some(format!("Bearer {}", p.control_token).as_str())
    {
        return failure(StatusCode::UNAUTHORIZED, "invalid_control_token");
    }
    (
        StatusCode::OK,
        Json(json!({"jwks_requests":p.jwks_requests.load(Ordering::Relaxed)})),
    )
}
fn failure(status: StatusCode, error: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error":error})))
}

async fn approve(
    State(p): State<Provider>,
    headers: HeaderMap,
    Json(approval): Json<Approval>,
) -> (StatusCode, Json<Value>) {
    if headers.get("authorization").and_then(|v| v.to_str().ok())
        != Some(format!("Bearer {}", p.control_token).as_str())
    {
        return failure(StatusCode::UNAUTHORIZED, "invalid_control_token");
    }
    let Ok(url) = reqwest::Url::parse(&approval.authorization_url) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_authorization_url");
    };
    if format!("{}{}", url.origin().ascii_serialization(), url.path())
        != format!("{}/auth", p.issuer)
    {
        return failure(StatusCode::BAD_REQUEST, "invalid_authorization_endpoint");
    }
    let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    let (Some(state), Some(nonce), Some(challenge)) = (
        query.get("state"),
        query.get("nonce"),
        query.get("code_challenge"),
    ) else {
        return failure(StatusCode::BAD_REQUEST, "missing_authorization_fields");
    };
    if query.get("code_challenge_method").map(String::as_str) != Some("S256")
        || query.get("client_id").map(String::as_str) != Some(CLIENT_ID)
    {
        return failure(StatusCode::BAD_REQUEST, "invalid_authorization_fields");
    }
    let mut random = [0; 16];
    if SystemRandom::new().fill(&mut random).is_err() {
        return failure(StatusCode::INTERNAL_SERVER_ERROR, "random_failed");
    }
    let code = URL_SAFE_NO_PAD.encode(random);
    let mut claims = serde_json::Map::from_iter(approval.claims);
    claims.entry("sub").or_insert(json!("subject-1"));
    p.codes.lock().await.insert(
        code.clone(),
        Pending {
            nonce: nonce.clone(),
            challenge: challenge.clone(),
            claims: Value::Object(claims),
            nonce_override: approval.nonce_override,
            bad_signature: approval.bad_signature,
        },
    );
    (StatusCode::OK, Json(json!({"state":state,"code":code})))
}

async fn token(
    State(p): State<Provider>,
    headers: HeaderMap,
    Form(form): Form<BTreeMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    let basic = format!(
        "Basic {}",
        STANDARD.encode(format!("{CLIENT_ID}:{CLIENT_SECRET}"))
    );
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(basic.as_str()) {
        return failure(StatusCode::UNAUTHORIZED, "invalid_client");
    }
    let code = form.get("code").map_or("", String::as_str);
    let Some(pending) = p.codes.lock().await.remove(code) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_grant");
    };
    let verifier = form.get("code_verifier").map_or("", String::as_str);
    if URL_SAFE_NO_PAD.encode(digest::digest(&digest::SHA256, verifier.as_bytes()))
        != pending.challenge
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"invalid_grant","detail":"pkce"})),
        );
    }
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return failure(StatusCode::INTERNAL_SERVER_ERROR, "clock_failed");
    };
    let mut claims = json!({"iss":p.issuer,"aud":CLIENT_ID,"iat":now.as_secs(),"exp":now.as_secs()+300,"nonce":pending.nonce_override.unwrap_or(pending.nonce)});
    if let (Some(base), Some(extra)) = (claims.as_object_mut(), pending.claims.as_object()) {
        base.extend(extra.clone());
    }
    let Ok(jwt) = sign(
        &claims,
        if pending.bad_signature {
            &p.bad_key
        } else {
            &p.key
        },
    ) else {
        return failure(StatusCode::INTERNAL_SERVER_ERROR, "signing_failed");
    };
    (
        StatusCode::OK,
        Json(json!({"access_token":"x","id_token":jwt})),
    )
}
fn sign(claims: &Value, key: &signature::EcdsaKeyPair) -> Result<String> {
    let header = URL_SAFE_NO_PAD.encode(serde_json::to_vec(
        &json!({"alg":"ES256","kid":"test-key","typ":"JWT"}),
    )?);
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims)?);
    let message = format!("{header}.{payload}");
    let signature = key
        .sign(&SystemRandom::new(), message.as_bytes())
        .map_err(|_| "sign failed")?;
    Ok(format!(
        "{message}.{}",
        URL_SAFE_NO_PAD.encode(signature.as_ref())
    ))
}
