//! Installation credentials: private key material and tokens never implement Debug.
use super::{CodeCancellation, CodeError, RuntimeError};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use cannery_core::principal::Secret;
use reqwest::{Client, Url};
use ring::{
    rand::SystemRandom,
    signature::{RSA_PKCS1_SHA256, RsaKeyPair},
};
use std::{
    io::Read,
    os::unix::fs::MetadataExt,
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

/// Explicit clock seam; production uses wall time, deterministic tests inject time.
pub trait Clock: Send + Sync {
    /// # Errors
    /// Return a sanitized clock error when Unix seconds cannot be represented.
    fn seconds(&self) -> Result<i64, RuntimeError>;
}
struct WallClock;
impl Clock for WallClock {
    fn seconds(&self) -> Result<i64, RuntimeError> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|value| i64::try_from(value.as_secs()).ok())
            .ok_or(RuntimeError::Configuration)
    }
}
struct Cached {
    token: Secret,
    expires: i64,
}
/// A configured App always fails closed. The refresh mutex prevents mint storms.
pub struct AppCredentials {
    key: RsaKeyPair,
    app_id: String,
    installation_id: String,
    clock: Arc<dyn Clock>,
    cached: Mutex<Option<Cached>>,
}
impl AppCredentials {
    /// # Errors
    /// Reject invalid IDs, inaccessible/nonprivate keys and unsupported PEM formats.
    pub fn new(app_id: &str, installation_id: &str, path: &Path) -> Result<Self, RuntimeError> {
        Self::with_clock(app_id, installation_id, path, Arc::new(WallClock))
    }
    /// # Errors
    /// Same validation as `new`; the required clock must supply Unix seconds.
    pub fn with_clock(
        app_id: &str,
        installation_id: &str,
        path: &Path,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, RuntimeError> {
        if [app_id, installation_id]
            .iter()
            .any(|value| value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return Err(RuntimeError::Configuration);
        }
        let descriptor = rustix::fs::open(
            path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| RuntimeError::Configuration)?;
        let file = std::fs::File::from(descriptor);
        let metadata = file.metadata().map_err(|_| RuntimeError::Configuration)?;
        if !metadata.is_file()
            || metadata.mode() & 0o7777 != 0o600
            || metadata.uid() != rustix::process::getuid().as_raw()
            || metadata.len() > 65536
        {
            return Err(RuntimeError::Configuration);
        }
        let mut pem = String::new();
        file.take(65537)
            .read_to_string(&mut pem)
            .map_err(|_| RuntimeError::Configuration)?;
        if pem.len() > 65536 {
            return Err(RuntimeError::Configuration);
        }
        let (kind, contents) = pem
            .trim()
            .strip_prefix("-----BEGIN ")
            .and_then(|value| value.split_once("-----"))
            .ok_or(RuntimeError::Configuration)?;
        if !matches!(kind, "PRIVATE KEY" | "RSA PRIVATE KEY") {
            return Err(RuntimeError::Configuration);
        }
        let footer = format!("-----END {kind}-----");
        let contents = contents
            .strip_suffix(&footer)
            .ok_or(RuntimeError::Configuration)?;
        let encoded: String = contents
            .chars()
            .filter(|value| !value.is_ascii_whitespace())
            .collect();
        let der = STANDARD
            .decode(encoded)
            .map_err(|_| RuntimeError::Configuration)?;
        let key = if kind == "PRIVATE KEY" {
            RsaKeyPair::from_pkcs8(&der)
        } else {
            RsaKeyPair::from_der(&der)
        }
        .map_err(|_| RuntimeError::Configuration)?;
        let result = Self {
            key,
            app_id: app_id.to_owned(),
            installation_id: installation_id.to_owned(),
            clock,
            cached: Mutex::new(None),
        };
        let _ = result.jwt()?;
        Ok(result)
    }
    fn jwt(&self) -> Result<Secret, RuntimeError> {
        let now = self.clock.seconds()?;
        let iat = now.checked_sub(60).ok_or(RuntimeError::Configuration)?;
        let exp = now.checked_add(540).ok_or(RuntimeError::Configuration)?;
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD
            .encode(serde_json::json!({"iat":iat,"exp":exp,"iss":self.app_id}).to_string());
        let signing = format!("{header}.{payload}");
        let mut signature = vec![0; self.key.public().modulus_len()];
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signing.as_bytes(),
                &mut signature,
            )
            .map_err(|_| RuntimeError::Configuration)?;
        Ok(Secret::new(format!(
            "{signing}.{}",
            URL_SAFE_NO_PAD.encode(signature)
        )))
    }
    pub(super) async fn token(
        &self,
        client: &Client,
        api: &Url,
        cancel: &CodeCancellation,
    ) -> Result<Secret, CodeError> {
        let mut cached = tokio::select! {
            () = cancel.cancelled() => return Err(CodeError::Cancelled),
            cached = self.cached.lock() => cached,
        };
        let now = self.clock.seconds().map_err(|_| CodeError::GitHub)?;
        if let Some(value) = cached.as_ref()
            && value.expires.saturating_sub(now) > 300
        {
            return Ok(value.token.clone());
        }
        let mut url = api.clone();
        url.path_segments_mut()
            .map_err(|()| CodeError::GitHub)?
            .pop_if_empty()
            .extend([
                "app",
                "installations",
                &self.installation_id,
                "access_tokens",
            ]);
        let jwt = self.jwt().map_err(|_| CodeError::GitHub)?;
        let request = client
            .post(url)
            .bearer_auth(jwt.expose())
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "cannery-runner");
        let response = tokio::select! {
            () = cancel.cancelled() => return Err(CodeError::Cancelled),
            response = request.send() => response.map_err(|_| CodeError::GitHub)?,
        };
        if response.status().as_u16() != 201 {
            return Err(CodeError::GitHub);
        }
        let body = tokio::select! {
            () = cancel.cancelled() => return Err(CodeError::Cancelled),
            body = super::super::http::read_json(response) => body.map_err(|_| CodeError::GitHub)?,
        };
        let token = body["token"]
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or(CodeError::GitHub)?;
        let expires = body["expires_at"]
            .as_str()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.timestamp())
            .ok_or(CodeError::GitHub)?;
        if expires <= self.clock.seconds().map_err(|_| CodeError::GitHub)?
            || reqwest::header::HeaderValue::from_str(token).is_err()
        {
            return Err(CodeError::GitHub);
        }
        let token = Secret::new(token.to_owned());
        *cached = Some(Cached {
            token: token.clone(),
            expires,
        });
        Ok(token)
    }
    pub(super) async fn invalidate(&self, cancel: &CodeCancellation) -> Result<(), CodeError> {
        let mut cached = tokio::select! {
            () = cancel.cancelled() => return Err(CodeError::Cancelled),
            cached = self.cached.lock() => cached,
        };
        *cached = None;
        Ok(())
    }
}
