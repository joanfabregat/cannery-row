//! The seven Python migration intents, executed against owned disposable databases.
use cannery_core::db::{
    DatabaseOptions, Migration, MigrationError, load_migrations, migrate, migrate_connection,
    migrations_from_sources,
};
use serde_json::Value;
use sqlx::{Connection, PgConnection};
use std::{
    error::Error,
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
const TIMEOUT: Duration = Duration::from_secs(5);
fn reference() -> Result<Value> {
    Ok(serde_json::from_str(&std::fs::read_to_string(
        std::env::var("CANNERY_MIGRATION_REFERENCE_PATH")
            .unwrap_or_else(|_| "target/migration-reference.json".into()),
    )?)?)
}
#[test]
#[ignore = "requires the generated Python migration reference"]
fn packaged_sequence_and_all_python_checksums_match() -> Result<()> {
    let reference = reference()?;
    assert_eq!(
        reference["executed_tests"]
            .as_array()
            .ok_or("tests absent")?
            .len(),
        7
    );
    assert_eq!(
        reference["calls"].as_array().ok_or("calls absent")?.len(),
        10
    );
    let migrations = load_migrations()?;
    assert_eq!(migrations.len(), 15);
    for (i, (migration, python)) in migrations
        .iter()
        .zip(reference["packaged"].as_array().ok_or("packaged absent")?)
        .enumerate()
    {
        assert_eq!(migration.version, i32::try_from(i + 1)?);
        assert_eq!(
            migration.name,
            python["name"].as_str().ok_or("name absent")?
        );
        assert_eq!(
            migration.checksum(),
            python["checksum"].as_str().ok_or("checksum absent")?
        );
    }
    assert!(matches!(
        migrations_from_sources(&[("wrong.sql", "SELECT 1")]),
        Err(MigrationError::Filename)
    ));
    assert!(matches!(
        migrations_from_sources(&[("0001_one.sql", ""), ("0001_two.sql", "")]),
        Err(MigrationError::Duplicate)
    ));
    assert!(matches!(
        migrations_from_sources(&[("0002_two.sql", "")]),
        Err(MigrationError::Noncontiguous)
    ));
    Ok(())
}
async fn owned<F, Fut>(label: &str, test: F) -> Result<()>
where
    F: FnOnce(String) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>>,
{
    let admin_url = std::env::var("CANNERY_MIGRATION_ADMIN_URL")?;
    let options = DatabaseOptions::parse(&admin_url)?;
    let mut admin = options.connect(Some(TIMEOUT)).await?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let name = format!("r1_migration_{label}_{}_{nonce}", std::process::id());
    assert!(
        name.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    );
    sqlx::raw_sql(&format!("CREATE DATABASE \"{name}\""))
        .execute(&mut admin)
        .await?;
    let mut url = url::Url::parse(&admin_url)?;
    url.set_path(&name);
    // Joining an owned worker permits cleanup even if an assertion panics.
    let url = url.to_string();
    let outcome = tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(test(url))
    })
    .await;
    let cleanup = sqlx::raw_sql(&format!("DROP DATABASE \"{name}\" WITH (FORCE)"))
        .execute(&mut admin)
        .await;
    if let Err(error) = cleanup {
        return Err(format!(
            "owned test database cleanup failed: {:?}",
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
        )
        .into());
    }
    outcome.map_err(|_| "migration fixture worker failed")?
}
async fn connect(url: &str) -> Result<PgConnection> {
    Ok(DatabaseOptions::parse(url)?.connect(Some(TIMEOUT)).await?)
}
fn observed(call: &Value, result: &std::result::Result<Vec<i32>, MigrationError>) -> Result<()> {
    let expected = &call["expected"];
    match result {
        Ok(versions) => {
            assert_eq!(expected["verdict"], "valid");
            assert_eq!(serde_json::to_value(versions)?, expected["applied"]);
        }
        Err(error) => {
            assert_eq!(expected["verdict"], "error");
            if expected["sqlstate"].is_null() {
                assert!(error.sqlstate().is_none());
            } else {
                assert_eq!(error.sqlstate(), expected["sqlstate"].as_str());
            }
            let classification = if error.sqlstate() == Some("42P01") {
                "UndefinedTable"
            } else {
                "MigrationError"
            };
            assert_eq!(expected["exception_type"], classification);
        }
    }
    Ok(())
}
#[tokio::test]
#[ignore = "requires an isolated PostgreSQL administrator connection"]
async fn original_ten_migration_calls_preserve_database_outcomes() -> Result<()> {
    let reference = reference()?;
    let calls = reference["calls"].as_array().ok_or("calls absent")?;
    let mut groups = std::collections::BTreeMap::<String, Vec<Value>>::new();
    for call in calls {
        groups
            .entry(
                call["source_test"]
                    .as_str()
                    .ok_or("source test absent")?
                    .to_owned(),
            )
            .or_default()
            .push(call.clone());
    }
    for (index, (name, calls)) in groups.into_iter().enumerate() {
        owned(&format!("case{index}"), move |url| async move {
            let mut connection = connect(&url).await?;
            let mut holder = None;
            if name == "test_lock_held_by_another_runner_times_out" {
                let mut lock = connect(&url).await?;
                let acquired = sqlx::query!(
                    "SELECT pg_try_advisory_lock($1::bigint) AS \"acquired!\"",
                    1_667_329_646_i64
                ).fetch_one(&mut lock).await?.acquired;
                assert!(acquired);
                holder = Some(lock);
            }
            for call in calls {
                let migrations = if call["migrations"] == "packaged" {
                    load_migrations()?
                } else {
                    call["migrations"].as_array().ok_or("migration set absent")?
                        .iter().map(|m| Ok(Migration {
                            version: i32::try_from(m["version"].as_i64().ok_or("version absent")?)?,
                            name: m["name"].as_str().ok_or("name absent")?.into(),
                            sql: m["sql"].as_str().ok_or("sql absent")?.into(),
                        })).collect::<Result<Vec<_>>>()?
                };
                let timeout = Duration::from_secs_f64(call["lock_timeout_seconds"].as_f64().ok_or("timeout absent")?);
                let result = if call["before"]["transaction_status"] == "INTRANS" {
                    let mut transaction = connection.begin().await?;
                    let result = migrate_connection(&mut transaction, &migrations, timeout).await;
                    assert!(transaction.is_in_transaction());
                    sqlx::query!("SELECT 1 AS \"value!\"").fetch_one(&mut *transaction).await?;
                    transaction.rollback().await?;
                    result
                } else {
                    migrate_connection(&mut connection, &migrations, timeout).await
                };
                observed(&call, &result)?;
                if let Err(error) = &result {
                    match name.as_str() {
                        "test_changed_migration_is_rejected"=>assert!(matches!(error,MigrationError::Changed{version:1,..})),
                        "test_unknown_applied_migration_is_rejected"=>assert!(matches!(error,MigrationError::Unknown(versions) if versions==&vec![2])),
                        "test_lock_held_by_another_runner_times_out"=>assert!(matches!(error,MigrationError::LockTimeout(_))),
                        "test_migrate_restores_autocommit_and_rejects_open_transaction"=>assert!(matches!(error,MigrationError::Transaction)),
                        _=>assert_eq!(error.sqlstate(),Some("42P01")),
                    }
                }
                assert!(!connection.is_in_transaction());
                if name == "test_failed_migration_rolls_back_and_is_not_recorded" {
                    let row = sqlx::query!("SELECT to_regclass('t2') IS NOT NULL AS \"present!\", (SELECT count(*) FROM schema_migrations) AS \"recorded!\"").fetch_one(&mut connection).await?;
                    assert!(!row.present);
                    assert_eq!(row.recorded, 0);
                }
            }
            if let Some(lock) = holder {
                lock.close().await?;
            }
            // The runner connection remains open: another session must be able
            // to acquire the lock after successful and failed migrations alike.
            let mut probe = connect(&url).await?;
            assert!(sqlx::query!(
                "SELECT pg_try_advisory_lock($1::bigint) AS \"acquired!\"",
                1_667_329_646_i64
            ).fetch_one(&mut probe).await?.acquired);
            probe.close().await?;
            connection.close().await?;
            Ok(())
        }).await?;
    }
    Ok(())
}
#[tokio::test]
#[ignore = "requires an isolated PostgreSQL administrator connection and Python reference CLI"]
async fn python_and_rust_migrations_are_interchangeable() -> Result<()> {
    let cli = std::env::var("CANNERY_MIGRATION_PYTHON_CLI")?;
    for python_first in [false, true] {
        let cli = cli.clone();
        owned(if python_first { "pythonfirst" } else { "rustfirst" }, move |url| {
            async move {
                let options = DatabaseOptions::parse(&url)?;
                if python_first {
                    python_migrate(&cli, &url, "applied 15 migration(s): [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]")?;
                    assert_eq!(migrate(&options).await?, Vec::<i32>::new());
                } else {
                    assert_eq!(migrate(&options).await?, (1..=15).collect::<Vec<_>>());
                    python_migrate(&cli, &url, "applied 0 migration(s)")?;
                }
                let mut connection = connect(&url).await?;
                let rows = sqlx::query!("SELECT version,name,checksum FROM schema_migrations ORDER BY version").fetch_all(&mut connection).await?;
                let migrations = load_migrations()?;
                assert_eq!(rows.len(), migrations.len());
                for (row, migration) in rows.iter().zip(migrations) {
                    assert_eq!(row.version, migration.version);
                    assert_eq!(row.name, migration.name);
                    assert_eq!(row.checksum, migration.checksum());
                }
                connection.close().await?;
                Ok(())
            }
        }).await?;
    }
    Ok(())
}
fn python_migrate(cli: &str, url: &str, expected: &str) -> Result<()> {
    let mut child = Command::new(cli)
        .arg("migrate")
        .env_remove("CANNERY_SETTINGS")
        .env("CANNERY_DATABASE_URL", url)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("reference migration CLI exceeded its deadline".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output()?;
    assert!(
        output.status.success(),
        "reference migration CLI failed; diagnostics withheld"
    );
    assert_eq!(String::from_utf8(output.stdout)?.trim(), expected);
    Ok(())
}
