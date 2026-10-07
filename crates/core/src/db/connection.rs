//! Secret-redacted configuration backed by `SQLx`'s PostgreSQL URL parser.
use sqlx::{Connection, PgConnection, postgres::PgConnectOptions};
use std::{fmt, str::FromStr, time::Duration};
use thiserror::Error;

#[derive(Clone)]
pub struct DatabaseOptions {
    options: PgConnectOptions,
}
impl fmt::Debug for DatabaseOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DatabaseOptions { credentials: [redacted] }")
    }
}
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConnectionError {
    #[error("invalid database connection configuration")]
    Invalid,
    #[error("unsupported database connection parameter")]
    Unsupported,
    #[error("database connection timed out")]
    Timeout,
    #[error("database connection failed")]
    Connect,
}
impl DatabaseOptions {
    /// Parse a standard PostgreSQL URL with native `SQLx` connection options.
    /// # Errors
    /// Returns a value-free error for invalid URLs or unknown parameters.
    pub fn parse(input: &str) -> Result<Self, ConnectionError> {
        if input.contains('\0') {
            return Err(ConnectionError::Invalid);
        }
        let url = url::Url::parse(input).map_err(|_| ConnectionError::Invalid)?;
        if !matches!(url.scheme(), "postgres" | "postgresql") {
            return Err(ConnectionError::Invalid);
        }
        // SQLx logs unknown query values. Reject unknown keys before handing
        // the URL to the driver so diagnostics cannot expose credential values.
        for (name, _) in url.query_pairs() {
            if !matches!(
                name.as_ref(),
                "sslmode"
                    | "ssl-mode"
                    | "sslrootcert"
                    | "ssl-root-cert"
                    | "ssl-ca"
                    | "sslcert"
                    | "ssl-cert"
                    | "sslkey"
                    | "ssl-key"
                    | "statement-cache-capacity"
                    | "host"
                    | "hostaddr"
                    | "port"
                    | "dbname"
                    | "user"
                    | "password"
                    | "application_name"
                    | "options"
            ) {
                return Err(ConnectionError::Unsupported);
            }
        }
        let options = PgConnectOptions::from_str(input).map_err(|_| ConnectionError::Invalid)?;
        Ok(Self { options })
    }
    #[must_use]
    pub fn connect_options(&self) -> &PgConnectOptions {
        &self.options
    }
    #[must_use]
    pub fn connect_timeout(&self) -> Option<Duration> {
        Some(Duration::from_secs(30))
    }
    /// Open a dedicated `SQLx` connection with an optional application timeout.
    /// # Errors
    /// Returns sanitized connection or timeout failures.
    pub async fn connect(
        &self,
        timeout: Option<Duration>,
    ) -> Result<PgConnection, ConnectionError> {
        tokio::time::timeout(
            timeout.unwrap_or(Duration::from_secs(30)),
            PgConnection::connect_with(&self.options),
        )
        .await
        .map_err(|_| ConnectionError::Timeout)?
        .map_err(|_| ConnectionError::Connect)
    }
}
