// Unit assertions intentionally panic on failed fixture expectations.
#![allow(clippy::expect_used)]
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use cannery_core::settings::{
    DatabaseProvider, Settings, SettingsError, StorageBackend, load_settings,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct File(PathBuf);
impl File {
    fn new(text: &str) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cannery-settings-test-{}-{nonce}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).expect("create test work directory");
        let path = root.join("settings.toml");
        std::fs::write(&path, text).expect("write synthetic settings");
        Self(path)
    }
}
impl Drop for File {
    fn drop(&mut self) {
        if let Some(root) = self.0.parent() {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}
fn environment(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    std::iter::once(("CANNERY_DATABASE_URL", "unused"))
        .chain(pairs.iter().copied())
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}
fn from_file(text: &str) -> Result<Settings, SettingsError> {
    let file = File::new(text);
    load_settings(Some(&file.0), &BTreeMap::new())
}
fn locations(error: &SettingsError) -> Vec<String> {
    match error {
        SettingsError::Invalid { issues } => issues
            .iter()
            .map(|issue| issue.location.join("."))
            .collect(),
        _ => Vec::new(),
    }
}

#[test]
fn settings_native_scalar_types_and_lists() {
    for (value, expected) in [
        ("TRUE", true),
        ("yes", true),
        ("1", true),
        ("off", false),
        ("no", false),
        ("0", false),
    ] {
        let settings = load_settings(None, &environment(&[("CANNERY_AUTH_COOKIE_SECURE", value)]))
            .expect("supported environment boolean");
        assert_eq!(settings.auth.cookie_secure, expected);
    }
    for value in [" true ", "1.0", "2", "maybe"] {
        assert!(
            load_settings(None, &environment(&[("CANNERY_AUTH_COOKIE_SECURE", value)])).is_err()
        );
    }
    for (value, expected) in [("1000", "1000"), ("  +001  ", "1"), ("-0", "0")] {
        let settings = load_settings(
            None,
            &environment(&[("CANNERY_DATABASE_POOL_MIN_SIZE", value)]),
        )
        .expect("native checked integer");
        assert_eq!(settings.database.pool_min_size.to_string(), expected);
    }
    for value in [
        "1.00",
        "1_000",
        "01.0",
        "1e2",
        "١٢",
        "true",
        "+0x12",
        "+-1",
        "--1",
        "9223372036854775808",
    ] {
        let error = load_settings(
            None,
            &environment(&[("CANNERY_DATABASE_POOL_MIN_SIZE", value)]),
        )
        .expect_err("invalid native integer");
        assert_eq!(locations(&error), ["database.pool_min_size"]);
        assert!(!error.to_string().contains(value));
    }
    for value in ["inf", "-inf", "NaN", "1e999", "1_000.0", "0", "-1"] {
        let error = load_settings(
            None,
            &environment(&[("CANNERY_SWEEPS_INTERVAL_SECONDS", value)]),
        )
        .expect_err("nonfinite or nonpositive interval");
        assert_eq!(locations(&error), ["sweeps.interval_seconds"]);
    }
    let settings = from_file("[database]\nurl='unused'\npool_min_size=1\npool_max_size=10\n[auth]\ncookie_secure=false\nallowed_email_domains=[' a ', 'b']\nbootstrap_admin_emails=' a, ,b, '\n").expect("native TOML scalar types");
    assert_eq!(settings.database.pool_min_size.to_string(), "1");
    assert!(!settings.auth.cookie_secure);
    assert_eq!(settings.auth.allowed_email_domains, [" a ", "b"]);
    assert_eq!(settings.auth.bootstrap_admin_emails, ["a", "b"]);
    for invalid in ["pool_min_size=true", "pool_min_size=1.0"] {
        assert_eq!(
            locations(
                &from_file(&format!("[database]\nurl='unused'\n{invalid}\n"))
                    .expect_err("integer TOML type")
            ),
            ["database.pool_min_size"]
        );
    }
    assert_eq!(
        locations(
            &from_file("[database]\nurl='unused'\n[auth]\ncookie_secure=0\n")
                .expect_err("boolean TOML type")
        ),
        ["auth.cookie_secure"]
    );
}

#[test]
fn settings_toml_integer_boundaries_and_checked_consumers() {
    for value in ["-9223372036854775808", "9223372036854775807"] {
        let settings = from_file(&format!(
            "[database]\nurl='unused'\npool_min_size={value}\n"
        ))
        .expect("native TOML i64 boundary");
        assert_eq!(settings.database.pool_min_size.to_string(), value);
        assert!(
            settings
                .database
                .pool_min_size
                .to_i64("database.pool_min_size")
                .is_ok()
        );
        if value.starts_with('-') {
            let error = settings
                .database
                .pool_min_size
                .to_u64("database.pool_min_size")
                .expect_err("negative consumer limit");
            assert_eq!(locations(&error), ["database.pool_min_size"]);
        }
    }
    for value in [
        "9223372036854775808",
        "-9223372036854775809",
        "0xffffffffffffffff",
        "999999999999999999999999999999999999999999",
    ] {
        let error = from_file(&format!(
            "[database]\nurl='unused'\npool_min_size={value}\n"
        ))
        .expect_err("native TOML range");
        assert!(matches!(error, SettingsError::Toml { .. }));
        assert!(
            !error.to_string().contains(value),
            "diagnostics redact values"
        );
    }
}

#[test]
fn settings_toml_strings_comments_and_invalid_numeric_syntax() {
    let huge = "999999999999999999999999999999999999999999";
    let settings = from_file(&format!(
        "# pool_min_size={huge}\n[database]\nurl='''pool_min_size={huge}'''\npool_min_size=1\n"
    ))
    .expect("literal and comment contents are not parsed as integers");
    assert_eq!(settings.database.url, format!("pool_min_size={huge}"));
    assert_eq!(settings.database.pool_min_size.to_string(), "1");
    for value in ["01", "+0x12", "1_", "1__0"] {
        assert!(
            matches!(
                from_file(&format!(
                    "[database]\nurl='unused'\npool_min_size={value}\n"
                )),
                Err(SettingsError::Toml { .. })
            ),
            "invalid TOML integer {value}"
        );
    }
    let error = from_file("[database]\nurl='unused'\n[auth]\nallowed_email_domains=[1]\n")
        .expect_err("array item type validation");
    assert_eq!(locations(&error), ["auth.allowed_email_domains.0"]);
    let settings = from_file("[database]\nurl=\"\"\"value\"\"\"\"\"\npool_min_size=1\n")
        .expect("multiline string delimiters");
    assert_eq!(settings.database.url, "value\"\"");
}

#[test]
fn settings_toml_10_rejects_11_syntax_without_rejecting_literal_content() {
    for source in [
        "database={url='unused',}\n",
        "database={\nurl='unused'\n}\n",
        "[database]\nurl=\"\\e\"\n",
        "[database]\nurl=\"\\x41\"\n",
        "[database]\nurl='unused'\n[auth]\nallowed_email_domains=[12:30]\n",
    ] {
        assert!(matches!(from_file(source), Err(SettingsError::Toml { .. })));
    }
    let source = "[database]\nurl='\\e \\x41 {a=1,}'\n# { inline, }\n[web]\ndist_dir='''literal multiline inline table:\n{a=1,}\n'''\n";
    assert!(from_file(source).is_ok());
}

#[test]
fn settings_testing_requires_python_loopback_urls() {
    for url in [
        "http://localhost:8000",
        "http://127.0.0.1:8000",
        "http://127.12.13.14",
        "https://[::1]:443",
        "HTTP://LOCALHOST",
        "http://localhost:",
    ] {
        assert!(
            load_settings(
                None,
                &environment(&[
                    ("CANNERY_TESTING_ENABLED", "true"),
                    ("CANNERY_SERVER_PUBLIC_BASE_URL", url)
                ])
            )
            .is_ok(),
            "source loopback accepted"
        );
    }
    for url in [
        "https://example.org",
        "http://localhost.example.org",
        "http://user:password@localhost",
        "http://localhost@remote.example.org",
        "ftp://localhost",
        "http://127.0.0.1:99999",
        "http://[::1",
        "http://127.1",
        "http://0177.0.0.1",
        "http://localhost.",
        "http://[127.0.0.1]",
    ] {
        assert!(
            load_settings(
                None,
                &environment(&[
                    ("CANNERY_TESTING_ENABLED", "true"),
                    ("CANNERY_SERVER_PUBLIC_BASE_URL", url)
                ])
            )
            .is_err(),
            "source loopback refusal"
        );
    }
    assert!(
        load_settings(
            None,
            &environment(&[("CANNERY_SERVER_PUBLIC_BASE_URL", "not a URL")])
        )
        .is_ok()
    );
}

#[test]
fn settings_s3_part_boundary_secret_redaction_and_aggregate_errors() {
    for (size, valid) in [(52_428_800_000_i64, true), (52_428_800_001, false)] {
        let env = environment(&[
            ("CANNERY_STORAGE_BACKEND", "s3"),
            ("AWS_ACCESS_KEY_ID", "fixture-key"),
            ("AWS_SECRET_ACCESS_KEY", "fixture-secret"),
            ("CANNERY_STORAGE_S3_PART_SIZE_BYTES", "5242880"),
            ("CANNERY_STORAGE_MAX_OBJECT_BYTES", &size.to_string()),
        ]);
        let result = load_settings(None, &env);
        assert_eq!(result.is_ok(), valid);
        if let Ok(settings) = result {
            assert_eq!(settings.storage.backend, StorageBackend::S3);
            assert!(!format!("{settings:?}").contains("fixture-secret"));
        }
    }
    let error = load_settings(
        None,
        &environment(&[
            (
                "CANNERY_DATABASE_POOL_MIN_SIZE",
                "postgresql://user:fixture-password@db",
            ),
            ("CANNERY_STORAGE_S3_PART_SIZE_BYTES", "1024"),
            ("CANNERY_AUTH_COOKIE_SECURE", "fixture-secret"),
        ]),
    )
    .expect_err("aggregated errors");
    for value in ["fixture-password", "fixture-secret"] {
        assert!(!error.to_string().contains(value));
        assert!(!format!("{error:?}").contains(value));
    }
    assert_eq!(
        locations(&error),
        [
            "database.pool_min_size",
            "auth.cookie_secure",
            "storage.s3_part_size_bytes"
        ]
    );
    let settings = load_settings(
        None,
        &environment(&[("CANNERY_STORAGE_S3_PREFIX", "../escape/")]),
    )
    .expect("preserve production prefix grammar");
    assert_eq!(settings.storage.s3_prefix, "../escape/");
}

#[test]
fn settings_wrong_sections_explicit_path_and_string_whitespace() {
    let file = File::new("database='wrong type'\n");
    assert!(matches!(
        load_settings(Some(&file.0), &environment(&[])),
        Err(SettingsError::Section {
            section: "database"
        })
    ));
    assert!(matches!(
        load_settings(Some(&file.0), &BTreeMap::new()),
        Err(SettingsError::Invalid { .. })
    ));
    let settings = load_settings(
        None,
        &environment(&[
            ("CANNERY_DATABASE_URL", "  original URL  "),
            ("CANNERY_UNKNOWN_FIELD", "ignored"),
        ]),
    )
    .expect("accepted untrimmed string");
    assert_eq!(settings.database.url, "  original URL  ");
    let file = File::new("[database]\nurl='from file'\n");
    let env = BTreeMap::from([
        (
            "CANNERY_SETTINGS".to_owned(),
            "/missing-settings-file".to_owned(),
        ),
        ("CANNERY_DATABASE_URL".to_owned(), "  ".to_owned()),
    ]);
    assert_eq!(
        load_settings(Some(&file.0), &env)
            .expect("explicit file wins")
            .database
            .url,
        "from file"
    );
}

#[test]
fn settings_fresh_python_reference_recipes_have_zero_differences() {
    let reference = std::env::var_os("CANNERY_SETTINGS_REFERENCE").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/settings-reference.json"),
        PathBuf::from,
    );
    assert!(
        reference.is_file(),
        "regenerate Python settings reference before differential test"
    );
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_settings-parity"))
        .arg(reference)
        .output()
        .expect("run settings parity adapter");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("zero mismatches"));
}

#[test]
fn settings_native_whitespace_and_finite_numbers() {
    let huge = "9".repeat(310);
    for field in [
        "CANNERY_DATABASE_POOL_MIN_SIZE",
        "CANNERY_SWEEPS_INTERVAL_SECONDS",
    ] {
        assert!(load_settings(None, &environment(&[(field, &huge)])).is_err());
    }
    assert!(matches!(
        from_file(&format!(
            "[database]\nurl='unused'\n[sweeps]\ninterval_seconds={huge}\n"
        )),
        Err(SettingsError::Toml { .. })
    ));
    assert_eq!(
        from_file("\u{feff}[database]\nurl='unused'\n")
            .expect("native TOML byte-order mark")
            .database
            .url,
        "unused"
    );
    let file = File::new("[database]\nurl='file URL'\n");
    for whitespace in ["  \n\t", "\u{2003}"] {
        let settings = load_settings(
            Some(&file.0),
            &BTreeMap::from([("CANNERY_DATABASE_URL".to_owned(), whitespace.to_owned())]),
        )
        .expect("native blank values ignored");
        assert_eq!(settings.database.url, "file URL");
    }
    let settings = load_settings(
        Some(&file.0),
        &BTreeMap::from([("CANNERY_DATABASE_URL".to_owned(), "\u{1c}\u{1f}".to_owned())]),
    )
    .expect("non-whitespace controls retain their value");
    assert_eq!(settings.database.url, "\u{1c}\u{1f}");
    let settings = load_settings(
        None,
        &environment(&[(
            "CANNERY_AUTH_ALLOWED_EMAIL_DOMAINS",
            "\u{2003}one\u{2003}, two ",
        )]),
    )
    .expect("native list whitespace");
    assert_eq!(settings.auth.allowed_email_domains, ["one", "two"]);
    assert!(from_file("[database]\nurl='unused'\n[sweeps]\ninterval_seconds=inf\n").is_err());
    assert!(from_file("[database]\nurl='unused'\n[sweeps]\ninterval_seconds=true\n").is_err());
}

#[test]
fn settings_structured_locations_preserve_literal_dots_and_root_validator()
-> Result<(), Box<dyn std::error::Error>> {
    let error = from_file("[database]\nurl='unused'\n'unknown.with.dots'=true\n")
        .expect_err("unknown literal key");
    if let SettingsError::Invalid { issues } = error {
        assert_eq!(issues[0].location, ["database", "unknown.with.dots"]);
    } else {
        return Err("wrong error category".into());
    }
    let error = load_settings(
        None,
        &environment(&[
            ("CANNERY_TESTING_ENABLED", "true"),
            ("CANNERY_SERVER_PUBLIC_BASE_URL", "https://remote.example"),
        ]),
    )
    .expect_err("root validator");
    if let SettingsError::Invalid { issues } = error {
        assert_eq!(issues[0].location, Vec::<String>::new());
    } else {
        return Err("wrong error category".into());
    }
    Ok(())
}

#[test]
fn settings_managed_database_provider_needs_no_url() {
    let settings = from_file("[database]\nprovider='managed'\ndata_dir='/srv/db'\n")
        .expect("managed database without url");
    assert_eq!(settings.database.provider, DatabaseProvider::Managed);
    assert_eq!(settings.database.data_dir.as_deref(), Some("/srv/db"));
    assert_eq!(settings.database.postgres_bin_dir, None);
    let settings = load_settings(
        None,
        &BTreeMap::from([
            ("CANNERY_DATABASE_PROVIDER".to_owned(), "managed".to_owned()),
            (
                "CANNERY_DATABASE_POSTGRES_BIN_DIR".to_owned(),
                "/usr/lib/postgresql/17/bin".to_owned(),
            ),
        ]),
    )
    .expect("managed database from the environment");
    assert_eq!(
        settings.database.postgres_bin_dir.as_deref(),
        Some("/usr/lib/postgresql/17/bin")
    );
    assert_eq!(
        from_file("[database]\nurl='unused'\n")
            .expect("default provider")
            .database
            .provider,
        DatabaseProvider::Url
    );
    for (text, location) in [
        ("[database]\nprovider='url'\n", "database.url"),
        (
            "[database]\nprovider='sqlite'\nurl='x'\n",
            "database.provider",
        ),
        (
            "[database]\nprovider='managed'\ndata_dir=1\n",
            "database.data_dir",
        ),
    ] {
        let error = from_file(text).expect_err("refused");
        assert_eq!(locations(&error), [location], "{text}");
    }
}
