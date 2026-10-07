//! Native comments and report projections; the caller owns transactions and locks.
#![forbid(unsafe_code)]
pub mod comments;
pub mod date;
mod integer;
pub mod reports;

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("comment/report database operation failed")]
    Database { sqlstate: Option<String> },
    #[error("comment persistence invariant failed")]
    Invariant,
    #[error("comment/report text parameter cannot be encoded")]
    TextEncoding,
    #[error("comment/report integer parameter cannot be encoded")]
    IntegerEncoding,
    #[error("report JSON decoding failed")]
    JsonDecode(cannery_core::json::DecodeError),
    #[error("report row contains an invalid domain value")]
    CorruptData,
    #[error("source preparation requires physical connection maintenance")]
    PreparationMaintenanceRequired,
}
impl RepositoryError {
    pub(crate) fn database(error: &sqlx::Error) -> Self {
        Self::Database {
            sqlstate: error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .filter(|code| {
                    code.len() == 5
                        && code
                            .bytes()
                            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
                })
                .map(std::borrow::Cow::into_owned),
        }
    }
}
pub(crate) fn text(value: &str) -> Result<(), RepositoryError> {
    if value.contains('\0') {
        Err(RepositoryError::TextEncoding)
    } else {
        Ok(())
    }
}

macro_rules! redacted {
    ($($name:ident),+ $(,)?) => {$(
        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(concat!(stringify!($name), "([redacted])"))
            }
        }
    )+};
}
pub(crate) use redacted;

/// Fetch using the compile-time checked query and caller-owned connection.
macro_rules! source {
    ($query:expr,$fetch:ident,$connection:expr) => {{
        $query
            .$fetch($connection)
            .await
            .map_err(|e| $crate::RepositoryError::database(&e))
    }};
}
pub(crate) use source;
