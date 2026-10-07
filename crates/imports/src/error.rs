use serde::Serialize;

/// A location and value-free diagnostic in the reviewed bundle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Problem {
    pub file: String,
    pub pointer: String,
    pub message: String,
}
impl Problem {
    pub(crate) fn new(file: &str, pointer: &str, message: &str) -> Self {
        Self {
            file: file.into(),
            pointer: pointer.into(),
            message: message.into(),
        }
    }
}
impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {}: {}",
            self.file.escape_default(),
            if self.pointer.is_empty() {
                "/"
            } else {
                &self.pointer
            },
            self.message
        )
    }
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the bundle was refused")]
    Refused(Vec<Problem>),
    #[error("the import database operation failed")]
    Database {
        code: Option<String>,
        constraint: Option<String>,
    },
    #[error("the stored import data is invalid")]
    CorruptData,
}
impl Error {
    pub(crate) fn problem(file: &str, pointer: &str, message: &str) -> Self {
        Self::Refused(vec![Problem::new(file, pointer, message)])
    }
}
impl From<sqlx::Error> for Error {
    fn from(error: sqlx::Error) -> Self {
        let database = error.as_database_error();
        Self::Database {
            code: database
                .and_then(sqlx::error::DatabaseError::code)
                .map(std::borrow::Cow::into_owned),
            constraint: database
                .and_then(sqlx::error::DatabaseError::constraint)
                .map(str::to_owned),
        }
    }
}
pub type Result<T> = std::result::Result<T, Error>;
