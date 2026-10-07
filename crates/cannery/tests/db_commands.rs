//! `cannery db dump`, `restore` and `upgrade` with `provider = "managed"`:
//! a dump while another process holds the data directory is refused (a
//! `provider = "url"` dump of the same server is not), a dump restores into a fresh data directory with its rows and `pg_trgm` indexes,
//! a non-empty database is only replaced on request, an unreadable archive
//! changes nothing, and an upgrade keeps the old directory. Only one
//! PostgreSQL major exists, so the upgrade runs with the same binaries.
//!
//! Run with `CANNERY_MANAGED_POSTGRES_BIN_DIR` naming a directory with
//! PostgreSQL 17 `postgres`, `initdb`, `pg_dump` and `pg_restore`, as a
//! non-root user: `cargo test -p cannery --test db_commands -- --ignored`.
#![cfg(unix)]

use cannery_managed_postgres::{Config, ManagedPostgres};
use sqlx::{Connection, PgConnection};
use std::{
    error::Error,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

fn bin_dir() -> Result<PathBuf> {
    Ok(PathBuf::from(std::env::var(
        "CANNERY_MANAGED_POSTGRES_BIN_DIR",
    )?))
}

fn cannery(data_dir: &Path, arguments: &[&str]) -> Result<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_cannery"))
        .env_clear()
        .env("CANNERY_DATABASE_PROVIDER", "managed")
        .env("CANNERY_DATABASE_DATA_DIR", data_dir)
        .env("CANNERY_DATABASE_POSTGRES_BIN_DIR", bin_dir()?)
        .env("CANNERY_DATABASE_POOL_MAX_SIZE", "4")
        .env("CANNERY_STORAGE_LOCAL_ROOT", data_dir.join("objects"))
        .env("CANNERY_SWEEPS_ENABLED", "false")
        .args(arguments)
        .output()?)
}

fn succeeds(output: &Output) -> Result<String> {
    if !output.status.success() {
        return Err(format!(
            "cannery failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout.clone())?)
}

fn fails(output: &Output, message: &str) -> Result {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() || !stderr.contains(message) {
        return Err(format!("expected a failure with {message:?}, got: {stderr}").into());
    }
    Ok(())
}

async fn start(data_dir: &Path) -> Result<ManagedPostgres> {
    Ok(ManagedPostgres::start(&Config::new(data_dir.to_path_buf(), bin_dir()?, 4)).await?)
}

async fn connect(server: &ManagedPostgres) -> Result<PgConnection> {
    Ok(PgConnection::connect_with(&server.connect_options("cannery")).await?)
}

/// A user, a project and a track; the track's trigger indexes it for search.
async fn seed(data_dir: &Path) -> Result {
    let server = start(data_dir).await?;
    let mut connection = connect(&server).await?;
    sqlx::query(
        "WITH u AS (INSERT INTO users (issuer, subject, email) \
                    VALUES ('https://issuer.test', 'u1', 'u1@example.org') RETURNING id), \
              p AS (INSERT INTO projects (slug, title, created_by) \
                    SELECT 'sardines', 'Sardines', id FROM u RETURNING id, created_by) \
         INSERT INTO tracks (project_id, slug, title, created_by) \
         SELECT id, 'canning', 'Cannery row sardine canning', created_by FROM p",
    )
    .execute(&mut connection)
    .await?;
    connection.close().await?;
    server.shutdown().await?;
    Ok(())
}

/// The seeded rows are there and a trigram search uses its index.
async fn check(data_dir: &Path) -> Result {
    let server = start(data_dir).await?;
    let mut connection = connect(&server).await?;
    let tracks: i64 = sqlx::query_scalar("SELECT count(*) FROM tracks")
        .fetch_one(&mut connection)
        .await?;
    assert_eq!(tracks, 1);
    sqlx::query("SET enable_seqscan = off")
        .execute(&mut connection)
        .await?;
    let search = "FROM search_documents WHERE title % 'canery row sardines'";
    let plan: Vec<String> = sqlx::query_scalar(&format!("EXPLAIN SELECT id {search}"))
        .fetch_all(&mut connection)
        .await?;
    assert!(
        plan.join("\n").contains("search_documents_title_trgm_idx"),
        "{plan:?}"
    );
    let found: i64 = sqlx::query_scalar(&format!("SELECT count(*) {search}"))
        .fetch_one(&mut connection)
        .await?;
    assert_eq!(found, 1);
    connection.close().await?;
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires PostgreSQL binaries in CANNERY_MANAGED_POSTGRES_BIN_DIR"]
async fn dump_restore_and_upgrade() -> Result {
    // Short: the socket path must fit in a Unix socket address.
    let root = std::env::temp_dir().join(format!("cdb-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    let source = root.join("a");
    let target = root.join("b");
    let dump = root.join("backup.dump");
    let dump_arg = dump.to_str().ok_or("path")?;

    succeeds(&cannery(&source, &["migrate"])?)?;
    seed(&source).await?;

    // Another process owns the data directory.
    let server = start(&source).await?;
    fails(
        &cannery(&source, &["db", "dump", dump_arg])?,
        "is in use by another cannery process",
    )?;
    assert!(!dump.exists());
    // provider = "url" needs no lock; the URL's password goes to PGPASSWORD.
    let url_dump = root.join("url.dump");
    let output = Command::new(env!("CARGO_BIN_EXE_cannery"))
        .env_clear()
        .env("CANNERY_DATABASE_URL", server.url())
        .env("CANNERY_DATABASE_POSTGRES_BIN_DIR", bin_dir()?)
        .env("CANNERY_STORAGE_LOCAL_ROOT", root.join("objects"))
        .args(["db", "dump", url_dump.to_str().ok_or("path")?])
        .output()?;
    succeeds(&output)?;
    server.shutdown().await?;
    let url_target = root.join("d");
    succeeds(&cannery(
        &url_target,
        &["db", "restore", url_dump.to_str().ok_or("path")?],
    )?)?;
    check(&url_target).await?;

    let stdout = succeeds(&cannery(&source, &["db", "dump", dump_arg])?)?;
    assert!(stdout.starts_with("wrote "), "{stdout}");
    assert_eq!(
        std::fs::metadata(&dump)?.permissions().mode() & 0o777,
        0o600
    );
    let leftovers = std::fs::read_dir(&root)?
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".partial"))
        .count();
    assert_eq!(leftovers, 0);

    // Into a data directory that does not exist yet.
    let stdout = succeeds(&cannery(&target, &["db", "restore", dump_arg])?)?;
    assert!(stdout.contains("applied 0 migration(s)"), "{stdout}");
    check(&target).await?;

    fails(
        &cannery(&target, &["db", "restore", dump_arg])?,
        "is not empty",
    )?;
    succeeds(&cannery(
        &target,
        &["db", "restore", "--replace", dump_arg],
    )?)?;
    check(&target).await?;

    // An unreadable archive is refused before the database is touched.
    let garbage = root.join("garbage.dump");
    std::fs::write(&garbage, "not a dump")?;
    fails(
        &cannery(
            &target,
            &[
                "db",
                "restore",
                "--replace",
                garbage.to_str().ok_or("path")?,
            ],
        )?,
        "pg_restore failed",
    )?;
    check(&target).await?;

    // Rebuild with the same major, keeping the old directory.
    let bin = bin_dir()?;
    let bin_arg = bin.to_str().ok_or("path")?;
    let stdout = succeeds(&cannery(
        &target,
        &["db", "upgrade", "--from-bin-dir", bin_arg],
    )?)?;
    assert!(stdout.starts_with("upgraded "), "{stdout}");
    let backup = root.join("b.pg17.bak");
    assert!(backup.join("pgdata/PG_VERSION").is_file());
    assert!(backup.join("upgrade-pg17.dump").is_file());
    check(&target).await?;
    fails(
        &cannery(&target, &["db", "upgrade", "--from-bin-dir", bin_arg])?,
        "already exists",
    )?;

    // A data directory from another major is refused and named for upgrade.
    let other = root.join("c");
    std::fs::create_dir_all(other.join("pgdata"))?;
    std::fs::write(other.join("pgdata/PG_VERSION"), "16\n")?;
    std::fs::write(other.join("password"), "unused\n")?;
    fails(&cannery(&other, &["migrate"])?, "cannery db upgrade")?;
    fails(
        &cannery(&other, &["db", "upgrade", "--from-bin-dir", bin_arg])?,
        "--from-bin-dir holds PostgreSQL 17",
    )?;

    std::fs::remove_dir_all(&root)?;
    Ok(())
}
