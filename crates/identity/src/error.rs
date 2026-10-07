//! Credential-safe identity failures; native database errors are never retained.
use cannery_core::errors::DomainError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error(transparent)]
    Domain(#[from] DomainError),
    #[error("identity database operation failed (SQLSTATE {sqlstate:?})")]
    Database { sqlstate: Option<String> },
    #[error("invalid stored identity data: {0}")]
    CorruptData(&'static str),
    #[error("credential randomness unavailable")]
    Random,
}
impl IdentityError {
    /// Discard native messages, SQL and bind values; keep only a validated code
    /// so the HTTP boundary can distinguish e.g. uniqueness conflicts.
    #[must_use]
    pub fn database(error: sqlx::Error) -> Self {
        let sqlstate = error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .filter(|code| {
                code.len() == 5
                    && code
                        .bytes()
                        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
            })
            .map(std::borrow::Cow::into_owned);
        drop(error);
        Self::Database { sqlstate }
    }
    #[must_use]
    pub fn sqlstate(&self) -> Option<&str> {
        if let Self::Database { sqlstate } = self {
            sqlstate.as_deref()
        } else {
            None
        }
    }
}
pub type Result<T> = std::result::Result<T, IdentityError>;
