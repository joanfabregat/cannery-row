//! Stable application error codes shared by HTTP and MCP handlers.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InternalError,
    BadRequest,
    LoginFailed,
    ValidationFailed,
    InvalidContent,
    Unauthenticated,
    Forbidden,
    CsrfInvalid,
    NotFound,
    Conflict,
    NothingToClaim,
    WorkflowUnavailable,
    ConcernOpen,
    StaleRevision,
    StaleLease,
    UploadExpired,
    Unavailable,
    StoreUnavailable,
}

impl ErrorCode {
    #[must_use]
    pub const fn status(self) -> u16 {
        match self {
            Self::InternalError => 500,
            Self::BadRequest | Self::LoginFailed => 400,
            Self::Unauthenticated => 401,
            Self::Forbidden | Self::CsrfInvalid => 403,
            Self::NotFound => 404,
            Self::Conflict
            | Self::NothingToClaim
            | Self::WorkflowUnavailable
            | Self::ConcernOpen
            | Self::StaleRevision
            | Self::StaleLease
            | Self::UploadExpired => 409,
            Self::ValidationFailed | Self::InvalidContent => 422,
            Self::Unavailable | Self::StoreUnavailable => 503,
        }
    }
}

/// Public application failures. Messages and details must contain no secrets.
#[derive(Debug, thiserror::Error, Serialize)]
#[error("{message}")]
pub struct DomainError {
    pub code: ErrorCode,
    pub message: String,
    pub details: Value,
}

impl DomainError {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: Value::Null,
        }
    }

    #[must_use]
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: DomainError,
}
