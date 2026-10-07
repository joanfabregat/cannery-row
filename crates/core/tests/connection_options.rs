//! Native PostgreSQL URLs and secret-safe diagnostics.
use cannery_core::db::{ConnectionError, DatabaseOptions};

#[test]
fn native_url_preserves_connection_identity_and_redacts_credentials() -> Result<(), ConnectionError>
{
    let options = DatabaseOptions::parse(
        "postgresql://alice:private-password@localhost:5433/research%20data?sslmode=require",
    )?;
    let native = options.connect_options();
    assert_eq!(native.get_host(), "localhost");
    assert_eq!(native.get_port(), 5433);
    assert_eq!(native.get_username(), "alice");
    assert_eq!(native.get_database(), Some("research data"));
    assert_eq!(
        format!("{options:?}"),
        "DatabaseOptions { credentials: [redacted] }"
    );
    Ok(())
}
#[test]
fn unknown_parameters_are_rejected_before_driver_logging() {
    assert!(matches!(
        DatabaseOptions::parse("postgres://localhost/research?typo=private-password"),
        Err(ConnectionError::Unsupported)
    ));
    assert!(matches!(
        DatabaseOptions::parse("host=localhost dbname=research"),
        Err(ConnectionError::Invalid)
    ));
    assert!(matches!(
        DatabaseOptions::parse("https://localhost/research"),
        Err(ConnectionError::Invalid)
    ));
}
