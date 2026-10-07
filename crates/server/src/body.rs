//! Bounded control-body parsing runs before authentication and model validation.

use crate::request_context::first_header;
use axum::{
    Json,
    body::to_bytes,
    extract::Request,
    http::{HeaderMap, StatusCode, request::Parts},
    response::{IntoResponse, Response},
};
use cannery_core::{
    errors::{DomainError, ErrorCode},
    json::{self, DecodeError, Document},
};
use serde_json::json;
use std::{fmt, time::Duration};

/// Shared native JSON recursion limit, including the root value.
pub const REST_JSON_NESTING_BUDGET: usize = json::MAX_DEPTH;
/// Limit buffered control bodies, including requests without `Content-Length`.
pub const REST_BODY_MAX_BYTES: usize = 2 * 1024 * 1024;
/// One deadline for the complete read; receiving another chunk does not reset it.
pub const REST_BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);

pub enum DecodedBody {
    Missing,
    Json(Document),
    RawBytes,
}

impl fmt::Debug for DecodedBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Missing => "Missing",
            Self::Json(_) => "Json([body redacted])",
            Self::RawBytes => "RawBytes([body redacted])",
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BodyError {
    #[error(transparent)]
    Validation(DomainError),
    #[error("request body parsing failed")]
    Parsing,
}

impl From<DecodeError> for BodyError {
    fn from(error: DecodeError) -> Self {
        match error {
            DecodeError::Syntax { position } => Self::Validation(
                DomainError::new(ErrorCode::ValidationFailed, "request validation failed")
                    .with_details(
                        json!([{"path":format!("body/{position}"),"message":"JSON decode error"}]),
                    ),
            ),
            DecodeError::Encoding | DecodeError::IntegerLimit | DecodeError::Recursion => {
                Self::Parsing
            }
        }
    }
}

impl IntoResponse for BodyError {
    fn into_response(self) -> Response {
        match self {
            Self::Validation(error) => crate::errors::ApiError::Domain(error).into_response(),
            // Preserve this source framework failure, distinct from domain errors.
            Self::Parsing => (
                StatusCode::BAD_REQUEST,
                Json(crate::api_models::HttpDetail {
                    detail: "There was an error parsing the body".to_owned(),
                }),
            )
                .into_response(),
        }
    }
}

/// The source email.message parser strips the whole type and requires exactly
/// one slash; whitespace within main/subtype is preserved. First header wins.
#[must_use]
pub fn is_json(headers: &HeaderMap) -> bool {
    let Some(value) = first_header(headers, "content-type") else {
        return false;
    };
    let content_type = value
        .split(';')
        .next()
        .unwrap_or("")
        .trim_matches(|ch: char| ch.is_whitespace() || matches!(ch, '\u{001c}'..='\u{001f}'))
        .to_lowercase();
    if content_type.bytes().filter(|&byte| byte == b'/').count() != 1 {
        return false;
    }
    content_type.split_once('/').is_some_and(|(main, sub)| {
        main == "application" && (sub == "json" || sub.ends_with("+json"))
    })
}

/// Read only on control routes using this adapter. Artifact streaming and MCP
/// retain their separate adapters and limits.
/// Raw non-JSON bytes need only a type marker for R2 model validation.
///
/// # Errors
/// Syntax uses the source 422 envelope; encoding, digit-limit, recursion and
/// transport and native budget failures use the generic 400 framework response.
pub async fn read_body(request: Request) -> Result<(Parts, DecodedBody), BodyError> {
    let (parts, body) = request.into_parts();
    let bytes = tokio::time::timeout(REST_BODY_READ_TIMEOUT, to_bytes(body, REST_BODY_MAX_BYTES))
        .await
        .map_err(|_| BodyError::Parsing)?
        .map_err(|_| BodyError::Parsing)?;
    let body = if bytes.is_empty() {
        DecodedBody::Missing
    } else if is_json(&parts.headers) {
        DecodedBody::Json(json::decode(&bytes, REST_JSON_NESTING_BUDGET)?)
    } else {
        DecodedBody::RawBytes
    };
    Ok((parts, body))
}

/// Deserialize the same control-request DTO used by `OpenAPI` after authentication.
/// # Errors
/// Returns a sanitized 422 response for incompatible request values.
pub(crate) fn typed<T: serde::de::DeserializeOwned>(
    body: &DecodedBody,
) -> Result<T, crate::errors::ApiError> {
    let invalid = || {
        crate::errors::ApiError::from(DomainError::new(
            ErrorCode::ValidationFailed,
            "request body does not match its schema",
        ))
    };
    let value = match body {
        DecodedBody::Json(document) => json::to_value(document).map_err(|_| invalid())?,
        DecodedBody::Missing => serde_json::Value::Object(serde_json::Map::new()),
        DecodedBody::RawBytes => return Err(invalid()),
    };
    serde_json::from_value(value).map_err(|_| invalid())
}
