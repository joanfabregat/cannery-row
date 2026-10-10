//! The original append-only migration protocol, using `SQLx` only as a driver.
use super::{ConnectionError, DatabaseOptions};
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use thiserror::Error;

const LOCK_KEY: i64 = 0x6361_6e6e;
/// The original migration lock acquisition deadline.
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(60);
/// A complete migration script. Custom sets support isolated integration tests;
/// the production CLI always uses the embedded packaged set.
#[derive(Clone, Debug)]
pub struct Migration {
    pub version: i32,
    pub name: String,
    pub sql: String,
}
impl Migration {
    /// SHA-256 of UTF-8 script text, without trimming or rewriting it.
    #[must_use]
    pub fn checksum(&self) -> String {
        format!("{:x}", Sha256::digest(self.sql.as_bytes()))
    }
}
/// Migration and database errors expose no connection credentials or SQL text.
#[derive(Debug, Error)]
pub enum MigrationError {
    #[error("invalid migration filename")]
    Filename,
    #[error("duplicate migration versions")]
    Duplicate,
    #[error("migration versions must be contiguous from 1")]
    Noncontiguous,
    #[error("migrate() needs a connection outside any transaction")]
    Transaction,
    #[error("database has migrations unknown to this code: {0:?}")]
    Unknown(Vec<i32>),
    #[error("migration {version:04}_{name} changed after being applied")]
    Changed { version: i32, name: String },
    #[error("another migration runner holds the lock after {0}s")]
    LockTimeout(f64),
    #[error("database migration failed (SQLSTATE {sqlstate:?})")]
    Database { sqlstate: Option<String> },
    #[error(transparent)]
    Connection(#[from] ConnectionError),
}
impl MigrationError {
    fn database(error: &sqlx::Error) -> Self {
        Self::Database {
            sqlstate: error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .map(std::borrow::Cow::into_owned),
        }
    }
    /// PostgreSQL SQLSTATE, when the failure came from the database.
    #[must_use]
    pub fn sqlstate(&self) -> Option<&str> {
        if let Self::Database { sqlstate } = self {
            sqlstate.as_deref()
        } else {
            None
        }
    }
}
/// Load the immutable migration scripts compiled into the binary.
///
/// # Errors
/// Refuses malformed names, duplicate versions or a noncontiguous set.
pub fn load_migrations() -> Result<Vec<Migration>, MigrationError> {
    migrations_from_sources(PACKAGED)
}
/// Validate a named set of script texts using the packaged-loader protocol.
///
/// # Errors
/// Refuses invalid filenames, duplicate versions or gaps.
pub fn migrations_from_sources(sources: &[(&str, &str)]) -> Result<Vec<Migration>, MigrationError> {
    let mut migrations = Vec::new();
    for (filename, sql) in sources {
        let Some(stem) = filename.strip_suffix(".sql") else {
            continue;
        };
        let (number, name) = stem.split_once('_').ok_or(MigrationError::Filename)?;
        if number.len() != 4
            || !number.bytes().all(|b| b.is_ascii_digit())
            || name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(MigrationError::Filename);
        }
        migrations.push(Migration {
            version: number.parse().map_err(|_| MigrationError::Filename)?,
            name: name.to_owned(),
            sql: (*sql).to_owned(),
        });
    }
    migrations.sort_by_key(|m| m.version);
    let versions: BTreeSet<_> = migrations.iter().map(|m| m.version).collect();
    if versions.len() != migrations.len() {
        return Err(MigrationError::Duplicate);
    }
    for (i, migration) in migrations.iter().enumerate() {
        if migration.version != i32::try_from(i + 1).map_err(|_| MigrationError::Noncontiguous)? {
            return Err(MigrationError::Noncontiguous);
        }
    }
    Ok(migrations)
}
/// Apply pending packaged migrations on a fresh dedicated connection.
///
/// # Errors
/// Reports connection errors, inconsistent history, lock timeout or SQL failure.
pub async fn migrate(options: &DatabaseOptions) -> Result<Vec<i32>, MigrationError> {
    migrate_with_timeout(options, DEFAULT_LOCK_TIMEOUT).await
}
/// Apply packaged migrations with an explicit advisory-lock timeout.
///
/// # Errors
/// Reports the same failures as [`migrate`].
pub async fn migrate_with_timeout(
    options: &DatabaseOptions,
    timeout: Duration,
) -> Result<Vec<i32>, MigrationError> {
    let migrations = load_migrations()?;
    let mut connection = options.connect(None).await?;
    let result = migrate_connection(&mut connection, &migrations, timeout).await;
    // The owned connection is never returned to a pool. Closing also releases
    // a session lock if cancellation or a network error interrupted cleanup.
    let _ = connection.close().await;
    result
}
/// Apply a script set on an idle SQLx-managed connection.
///
/// Use `SQLx`'s transaction API rather than issuing raw `BEGIN`: `SQLx` tracks
/// explicit transaction depth. The CLI uses a fresh connection instead.
///
/// # Errors
/// Refuses a tracked transaction, inconsistent history, timeout or SQL failure.
pub async fn migrate_connection(
    connection: &mut PgConnection,
    migrations: &[Migration],
    timeout: Duration,
) -> Result<Vec<i32>, MigrationError> {
    if connection.is_in_transaction() {
        return Err(MigrationError::Transaction);
    }
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let acquired = sqlx::query!(
            "SELECT pg_try_advisory_lock($1::bigint) AS \"acquired!\"",
            LOCK_KEY
        )
        .fetch_one(&mut *connection)
        .await
        .map_err(|e| MigrationError::database(&e))?
        .acquired;
        if acquired {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(MigrationError::LockTimeout(timeout.as_secs_f64()));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let result = apply_locked(connection, migrations).await;
    let released = sqlx::query!(
        "SELECT pg_advisory_unlock($1::bigint) AS \"released!\"",
        LOCK_KEY
    )
    .fetch_one(&mut *connection)
    .await;
    // Preserve the original SQL error if the connection died while applying.
    match (result, released) {
        (Err(error), _) => Err(error),
        (Ok(done), Ok(_)) => Ok(done),
        (Ok(_), Err(error)) => Err(MigrationError::database(&error)),
    }
}
async fn apply_locked(
    connection: &mut PgConnection,
    migrations: &[Migration],
) -> Result<Vec<i32>, MigrationError> {
    sqlx::query!("CREATE TABLE IF NOT EXISTS schema_migrations (version integer PRIMARY KEY, name text NOT NULL, checksum text NOT NULL, applied_at timestamptz NOT NULL DEFAULT now())")
        .execute(&mut *connection).await.map_err(|e|MigrationError::database(&e))?;
    let rows = sqlx::query!("SELECT version, checksum FROM schema_migrations")
        .fetch_all(&mut *connection)
        .await
        .map_err(|e| MigrationError::database(&e))?;
    let applied: BTreeMap<_, _> = rows.into_iter().map(|r| (r.version, r.checksum)).collect();
    let known: BTreeSet<_> = migrations.iter().map(|m| m.version).collect();
    let unknown: Vec<_> = applied
        .keys()
        .filter(|v| !known.contains(v))
        .copied()
        .collect();
    if !unknown.is_empty() {
        return Err(MigrationError::Unknown(unknown));
    }
    let mut done = Vec::new();
    for migration in migrations {
        let checksum = migration.checksum();
        if let Some(recorded) = applied.get(&migration.version) {
            if recorded != &checksum {
                return Err(MigrationError::Changed {
                    version: migration.version,
                    name: migration.name.clone(),
                });
            }
            continue;
        }
        let mut transaction = connection
            .begin()
            .await
            .map_err(|e| MigrationError::database(&e))?;
        let applied = async {
            sqlx::raw_sql(&migration.sql)
                .execute(&mut *transaction)
                .await?;
            sqlx::query!(
                "INSERT INTO schema_migrations (version, name, checksum) VALUES ($1, $2, $3)",
                migration.version,
                migration.name,
                checksum
            )
            .execute(&mut *transaction)
            .await?;
            Ok::<_, sqlx::Error>(())
        }
        .await;
        if let Err(error) = applied {
            let _ = transaction.rollback().await;
            return Err(MigrationError::database(&error));
        }
        transaction
            .commit()
            .await
            .map_err(|e| MigrationError::database(&e))?;
        done.push(migration.version);
    }
    Ok(done)
}
const PACKAGED: &[(&str, &str)] = &[
    (
        "0001_extensions.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0001_extensions.sql"
        )),
    ),
    (
        "0002_identity.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0002_identity.sql"
        )),
    ),
    (
        "0003_research.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0003_research.sql"
        )),
    ),
    (
        "0004_attempts.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0004_attempts.sql"
        )),
    ),
    (
        "0005_jobs.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0005_jobs.sql"
        )),
    ),
    (
        "0006_sweeps.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0006_sweeps.sql"
        )),
    ),
    (
        "0007_evaluation.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0007_evaluation.sql"
        )),
    ),
    (
        "0008_read_side.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0008_read_side.sql"
        )),
    ),
    (
        "0009_attention_indexes.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0009_attention_indexes.sql"
        )),
    ),
    (
        "0010_evaluator_policy.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0010_evaluator_policy.sql"
        )),
    ),
    (
        "0011_output_interfaces.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0011_output_interfaces.sql"
        )),
    ),
    (
        "0012_direct_uploads.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0012_direct_uploads.sql"
        )),
    ),
    (
        "0013_imports.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0013_imports.sql"
        )),
    ),
    (
        "0014_workflow_experiments.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0014_workflow_experiments.sql"
        )),
    ),
    (
        "0015_imported_reports.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0015_imported_reports.sql"
        )),
    ),
    (
        "0016_remove_validating.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0016_remove_validating.sql"
        )),
    ),
    (
        "0017_phase_outputs.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0017_phase_outputs.sql"
        )),
    ),
    (
        "0018_briefs.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0018_briefs.sql"
        )),
    ),
    (
        "0019_track_plans.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0019_track_plans.sql"
        )),
    ),
    (
        "0020_units_from_plans.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0020_units_from_plans.sql"
        )),
    ),
    (
        "0021_run_documents.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0021_run_documents.sql"
        )),
    ),
    (
        "0022_verify_jobs.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0022_verify_jobs.sql"
        )),
    ),
    (
        "0023_document_and_decide.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0023_document_and_decide.sql"
        )),
    ),
    (
        "0024_plan_concerns.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0024_plan_concerns.sql"
        )),
    ),
    (
        "0025_automatic_decisions.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0025_automatic_decisions.sql"
        )),
    ),
    (
        "0026_units.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0026_units.sql"
        )),
    ),
    (
        "0027_questions_and_transcripts.sql",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0027_questions_and_transcripts.sql"
        )),
    ),
];
