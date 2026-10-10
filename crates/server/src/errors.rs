//! HTTP mapping for the shared public application errors.

use crate::requests::RequestId;
use axum::{
    Json,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use cannery_core::errors::{DomainError, ErrorCode};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("internal request failure")]
    Internal {
        request_id: RequestId,
        operation: &'static str,
    },
    #[error(transparent)]
    Domain(#[from] DomainError),
    /// The Python store-exception handler adds Retry-After; domain errors do not.
    #[error(transparent)]
    RetryableStore(DomainError),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (error, retryable) = match self {
            Self::Internal {
                request_id,
                operation,
            } => {
                tracing::error!(%request_id, operation, "request failed");
                return (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
                    .into_response();
            }
            Self::Domain(error) => (error, false),
            Self::RetryableStore(error) => (error, true),
        };
        let code = error.code;
        let status =
            StatusCode::from_u16(code.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let code_name = match code {
            ErrorCode::InternalError => "internal_error",
            ErrorCode::BadRequest => "bad_request",
            ErrorCode::LoginFailed => "login_failed",
            ErrorCode::ValidationFailed => "validation_failed",
            ErrorCode::InvalidContent => "invalid_content",
            ErrorCode::Unauthenticated => "unauthenticated",
            ErrorCode::Forbidden => "forbidden",
            ErrorCode::CsrfInvalid => "csrf_invalid",
            ErrorCode::NotFound => "not_found",
            ErrorCode::Conflict => "conflict",
            ErrorCode::NothingToClaim => "nothing_to_claim",
            ErrorCode::WorkflowUnavailable => "workflow_unavailable",
            ErrorCode::ConcernOpen => "concern_open",
            ErrorCode::StaleRevision => "stale_revision",
            ErrorCode::StaleLease => "stale_lease",
            ErrorCode::UploadExpired => "upload_expired",
            ErrorCode::Unavailable => "unavailable",
            ErrorCode::StoreUnavailable => "store_unavailable",
        };
        let body = crate::api_models::ErrorResponse {
            error: crate::api_models::ErrorDetail {
                code: code_name.to_owned(),
                message: error.message,
                details: error.details,
            },
        };
        let mut response = (status, Json(body)).into_response();
        if code == ErrorCode::Unauthenticated {
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                header::HeaderValue::from_static("Bearer"),
            );
        }
        if retryable {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, header::HeaderValue::from_static("5"));
        }
        response
    }
}
