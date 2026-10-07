//! Startup diagnostics must not expose invalid environment values.
#![cfg(unix)]

use std::{error::Error, ffi::OsString, os::unix::ffi::OsStringExt, process::Command};

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;
const SENTINEL: &str = "synthetic-private-environment-value";

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cannery"));
    command.env_clear().arg("migrate");
    command.env("CANNERY_DATABASE_URL", "invalid-keyword-dsn");
    command
}

fn invalid_value() -> OsString {
    let mut value = SENTINEL.as_bytes().to_vec();
    value.push(0xff);
    OsString::from_vec(value)
}

#[test]
fn unrelated_non_unicode_environment_does_not_panic_or_disclose() -> Result {
    let output = command()
        .env("UNRELATED", invalid_value())
        .env("CANNERY_UNKNOWN_VARIABLE", invalid_value())
        .env(
            OsString::from_vec(b"UNRELATED\xff".to_vec()),
            invalid_value(),
        )
        .output()?;
    let stderr = String::from_utf8(output.stderr)?;
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr.contains("invalid database connection configuration"));
    assert!(!stderr.contains(SENTINEL));
    assert!(!stderr.contains("panicked"));
    Ok(())
}

#[test]
fn recognized_non_unicode_setting_names_the_variable_only() -> Result {
    let output = command()
        .env("CANNERY_DATABASE_URL", invalid_value())
        .output()?;
    let stderr = String::from_utf8(output.stderr)?;
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr.contains("CANNERY_DATABASE_URL"));
    assert!(stderr.contains("not UTF-8"));
    assert!(!stderr.contains(SENTINEL));
    assert!(!stderr.contains("panicked"));
    Ok(())
}
