//! Database diagnostics are discarded; only validated SQLSTATE codes survive.

use cannery_core::errors::DomainError;
use std::{error::Error, fmt};

pub enum ProjectError {
    Domain(DomainError),
    Database(DatabaseFailure),
    CorruptData,
}

/// Sanitized database failure. Native messages and error chains are discarded.
#[derive(Debug)]
pub struct DatabaseFailure {
    sqlstate: Option<String>,
}

impl ProjectError {
    /// Return a validated five-character PostgreSQL diagnostic code, if present.
    #[must_use]
    pub fn sqlstate(&self) -> Option<&str> {
        match self {
            Self::Database(error) => error.sqlstate.as_deref(),
            Self::Domain(_) | Self::CorruptData => None,
        }
    }
}

impl From<DomainError> for ProjectError {
    fn from(error: DomainError) -> Self {
        Self::Domain(error)
    }
}

impl From<sqlx::Error> for ProjectError {
    fn from(error: sqlx::Error) -> Self {
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
        Self::Database(DatabaseFailure { sqlstate })
    }
}

impl fmt::Display for ProjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Domain(error) => fmt::Display::fmt(error, f),
            Self::Database(_) => f.write_str("project database operation failed"),
            Self::CorruptData => f.write_str("invalid persisted project data"),
        }
    }
}

impl fmt::Debug for ProjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Domain(error) => f.debug_tuple("Domain").field(&error.code).finish(),
            Self::Database(_) => f.write_str("Database([redacted])"),
            Self::CorruptData => f.write_str("CorruptData"),
        }
    }
}

impl Error for ProjectError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Domain(error) => Some(error),
            Self::Database(_) | Self::CorruptData => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProjectError;
    use std::{
        borrow::Cow,
        error::Error,
        fmt,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    #[derive(Debug)]
    struct NativeError {
        code: &'static str,
        dropped: Arc<AtomicBool>,
    }

    impl Drop for NativeError {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    impl fmt::Display for NativeError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("synthetic-private-database-value")
        }
    }

    impl Error for NativeError {}

    impl sqlx::error::DatabaseError for NativeError {
        fn message(&self) -> &'static str {
            "synthetic-private-database-value"
        }
        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed(self.code))
        }
        fn as_error(&self) -> &(dyn Error + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn Error + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn Error + Send + Sync + 'static> {
            self
        }
        fn kind(&self) -> sqlx::error::ErrorKind {
            sqlx::error::ErrorKind::Other
        }
    }

    #[test]
    fn native_errors_are_destroyed_and_only_valid_sqlstates_survive() {
        for (code, expected) in [
            ("55P03", Some("55P03")),
            ("23505", Some("23505")),
            ("00000", Some("00000")),
            ("55p03", None),
            ("55P03X", None),
            ("55P0", None),
            ("55-03", None),
            ("55Ｐ3", None),
            ("synthetic-private-code", None),
            ("", None),
        ] {
            let dropped = Arc::new(AtomicBool::new(false));
            let native = sqlx::Error::Database(Box::new(NativeError {
                code,
                dropped: Arc::clone(&dropped),
            }));
            let error = ProjectError::from(native);
            assert!(dropped.load(Ordering::SeqCst));
            assert_eq!(error.sqlstate(), expected);
            assert_eq!(error.to_string(), "project database operation failed");
            assert_eq!(format!("{error:?}"), "Database([redacted])");
            assert!(error.source().is_none());
        }
    }
}
