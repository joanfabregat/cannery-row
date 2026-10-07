//! Native browser paths and RFC cookie headers, independent of OIDC.

use axum::http::HeaderValue;
use cannery_core::{principal::Secret, timestamps::Timestamp};
use chrono::Datelike;
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use std::{fmt, fmt::Write};

pub const LOGIN_COOKIE: &str = "cr_login";
pub const SESSION_COOKIE: &str = "cr_session";

#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    #[error("browser header encoding failed")]
    Encoding,
    #[error("browser header construction failed")]
    Header,
    #[error("browser cookie expiry is out of range")]
    MaxAgeOverflow,
}

/// Return a literal local absolute path; controls and backslashes are refused.
#[must_use]
pub fn safe_return_to(value: Option<&String>) -> String {
    value
        .filter(|value| {
            value.starts_with('/')
                && !value.starts_with("//")
                && !value.contains('\\')
                && !value.chars().any(char::is_control)
        })
        .cloned()
        .unwrap_or_else(|| "/".to_owned())
}

/// Opaque browser tokens use RFC 6265 cookie-octets. A tiny formatter is enough:
/// names, paths and attributes are application constants; values need no quoting
/// or Latin-1 conversion and cannot inject another cookie or header.
/// # Errors
/// Rejects whitespace, controls, non-ASCII and cookie delimiters.
pub fn quote_cookie_value(value: &str) -> Result<Secret, BrowserError> {
    if value
        .bytes()
        .all(|byte| matches!(byte, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e))
    {
        Ok(Secret::new(value.to_owned()))
    } else {
        Err(BrowserError::Encoding)
    }
}

/// Rendered cookies remain explicitly secret-bearing until attached to a response.
pub struct CookieHeader(Secret);
impl fmt::Debug for CookieHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CookieHeader([redacted])")
    }
}
impl CookieHeader {
    /// Expose only at the HTTP transport boundary; `HeaderValue`'s own `Debug` is not redacted.
    /// # Errors
    /// Returns a sanitized error for a header the transport cannot represent.
    pub fn into_header(self) -> Result<HeaderValue, BrowserError> {
        HeaderValue::from_bytes(self.0.expose().as_bytes()).map_err(|_| BrowserError::Header)
    }
}

fn set_cookie(
    name: &str,
    value: &Secret,
    max_age: i64,
    path: &str,
    secure: bool,
) -> Result<CookieHeader, BrowserError> {
    let value = quote_cookie_value(value.expose())?;
    let mut text = format!(
        "{name}={}; HttpOnly; Max-Age={max_age}; Path={path}; SameSite=lax",
        value.expose()
    );
    if secure {
        text.push_str("; Secure");
    }
    Ok(CookieHeader(Secret::new(text)))
}

/// Login binding cookie expires after fifteen minutes.
/// # Errors
/// Returns a sanitized encoding failure for an unsupported cookie value.
pub fn login_cookie(value: &Secret, secure: bool) -> Result<CookieHeader, BrowserError> {
    set_cookie(LOGIN_COOKIE, value, 900, "/auth", secure)
}

/// Convert configured hours to a checked signed 64-bit Max-Age in seconds.
/// # Errors
/// Returns a sanitized encoding failure for an unsupported cookie value.
pub fn session_cookie(
    value: &Secret,
    ttl_hours: &BigInt,
    secure: bool,
) -> Result<CookieHeader, BrowserError> {
    let seconds = ttl_hours
        .to_i64()
        .and_then(|hours| hours.checked_mul(3600))
        .ok_or(BrowserError::MaxAgeOverflow)?;
    set_cookie(SESSION_COOKIE, value, seconds, "/", secure)
}

fn delete_cookie(name: &str, path: &str, now: Timestamp) -> CookieHeader {
    let now = now.0.with_timezone(&chrono::Utc);
    let expires = format!(
        "{} {:4} {} GMT",
        now.format("%a, %d %b"),
        now.year(),
        now.format("%H:%M:%S")
    );
    CookieHeader(Secret::new(format!(
        "{name}=\"\"; expires={expires}; Max-Age=0; Path={path}; SameSite=lax"
    )))
}

#[must_use]
pub fn delete_login_cookie(now: Timestamp) -> CookieHeader {
    delete_cookie(LOGIN_COOKIE, "/auth", now)
}
#[must_use]
pub fn delete_session_cookie(now: Timestamp) -> CookieHeader {
    delete_cookie(SESSION_COOKIE, "/", now)
}

/// Callback headers append the login deletion first, then the new session cookie.
/// # Errors
/// Returns a sanitized failure when the session cookie cannot be represented.
pub fn callback_cookies(
    value: &Secret,
    ttl_hours: &BigInt,
    secure: bool,
    now: Timestamp,
) -> Result<[CookieHeader; 2], BrowserError> {
    Ok([
        delete_login_cookie(now),
        session_cookie(value, ttl_hours, secure)?,
    ])
}

/// Percent-encode UTF-8 path bytes while retaining URI syntax and percent escapes.
/// # Errors
/// Returns a sanitized HTTP header construction failure.
pub fn redirect_location(value: &str) -> Result<HeaderValue, BrowserError> {
    let mut result = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"_.-~:/%#?=@[]!$&'()*+,;".contains(&byte) {
            result.push(char::from(byte));
        } else {
            let _ = write!(result, "%{byte:02X}");
        }
    }
    HeaderValue::from_bytes(result.as_bytes()).map_err(|_| BrowserError::Header)
}
