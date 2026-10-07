//! A managed server from real PostgreSQL binaries: first start, migrations,
//! `pg_trgm`, the data-directory lock, restart and stale-server recovery.
//!
//! Run with `CANNERY_MANAGED_POSTGRES_BIN_DIR` naming a directory with
//! `postgres` and `initdb` (PostgreSQL 17, with `pg_trgm`), as a non-root
//! user: `cargo test -p cannery-managed-postgres --test server -- --ignored`.
#![cfg(unix)]

use cannery_core::db::{DatabaseOptions, migrate};
use cannery_managed_postgres::{Config, ManagedError, ManagedPostgres};
use sqlx::{Connection, PgConnection};
use std::{
    error::Error,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

fn config() -> Result<Config> {
    let bin_dir = PathBuf::from(std::env::var("CANNERY_MANAGED_POSTGRES_BIN_DIR")?);
    // Short: the socket path must fit in a Unix socket address.
    let data_dir = std::env::temp_dir().join(format!("cmp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let mut config = Config::new(data_dir, bin_dir, 4);
    config.startup_timeout = Duration::from_secs(60);
    Ok(config)
}

async fn query_trigram(server: &ManagedPostgres) -> Result<f32> {
    let options = DatabaseOptions::parse(&server.url())?;
    let mut connection = PgConnection::connect_with(options.connect_options()).await?;
    let similarity: f32 = sqlx::query_scalar("SELECT similarity('cannery row', 'canary row')")
        .fetch_one(&mut connection)
        .await?;
    connection.close().await?;
    Ok(similarity)
}

/// Starts PostgreSQL directly, as a crashed supervisor would leave it.
fn orphan_server(config: &Config) -> Result<std::process::Child> {
    let root = std::fs::canonicalize(&config.data_dir)?;
    let child = Command::new(config.bin_dir.join("postgres"))
        .arg("-D")
        .arg(root.join("pgdata"))
        .args(["-c", "listen_addresses=", "-c", "port=5432", "-c"])
        .arg(format!(
            "unix_socket_directories={}",
            root.join("run").display()
        ))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(child)
}

fn wait_for(path: &Path) -> Result {
    for _ in 0..200 {
        if path.exists() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!("{} did not appear", path.display()).into())
}

/// Full-text search and `pg_trgm` lowercase and index non-ASCII letters,
/// which needs a UTF-8 `lc_ctype` (with `C` they are left as is or dropped).
async fn assert_unicode_text_search(server: &ManagedPostgres) -> Result {
    let options = DatabaseOptions::parse(&server.url())?;
    let mut connection = PgConnection::connect_with(options.connect_options()).await?;
    let (vector, similarity): (String, f32) = sqlx::query_as(
        "SELECT to_tsvector('simple', 'École Straße')::text, similarity('café', 'cafe')",
    )
    .fetch_one(&mut connection)
    .await?;
    connection.close().await?;
    assert_eq!(vector, "'straße':2 'école':1");
    // 3 of 7 trigrams are shared when é is indexed; 3 of 6 (0.5) when dropped.
    assert!(
        (similarity - 3.0 / 7.0).abs() < 0.01,
        "pg_trgm similarity('café', 'cafe') = {similarity}"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires PostgreSQL binaries in CANNERY_MANAGED_POSTGRES_BIN_DIR"]
async fn managed_server_lifecycle() -> Result {
    let config = config()?;
    let started = std::time::Instant::now();
    let server = ManagedPostgres::start(&config).await?;
    eprintln!("first start (initdb): {:?}", started.elapsed());

    let applied = migrate(&DatabaseOptions::parse(&server.url())?).await?;
    assert_ne!(applied.len(), 0);
    assert!(query_trigram(&server).await? > 0.3);
    assert_unicode_text_search(&server).await?;

    // One owner per data directory.
    assert!(matches!(
        ManagedPostgres::start(&config).await,
        Err(ManagedError::Locked(_))
    ));
    server.shutdown().await?;

    let started = std::time::Instant::now();
    let server = ManagedPostgres::start(&config).await?;
    eprintln!("second start: {:?}", started.elapsed());
    assert_eq!(
        migrate(&DatabaseOptions::parse(&server.url())?).await?,
        Vec::<i32>::new()
    );
    server.shutdown().await?;

    // A postmaster.pid naming an unrelated live process (PID reuse).
    let root = std::fs::canonicalize(&config.data_dir)?;
    let pid_file = root.join("pgdata/postmaster.pid");
    std::fs::write(&pid_file, format!("{}\n", std::process::id()))?;
    let server = ManagedPostgres::start(&config).await?;
    assert!(query_trigram(&server).await? > 0.3);
    server.shutdown().await?;

    // A server left running by a supervisor that died.
    let mut orphan = orphan_server(&config)?;
    wait_for(&root.join("run/.s.PGSQL.5432"))?;
    let server = ManagedPostgres::start(&config).await?;
    assert!(orphan.try_wait()?.is_some(), "the orphan was not stopped");
    assert!(query_trigram(&server).await? > 0.3);
    server.shutdown().await?;
    assert!(!pid_file.exists());

    std::fs::remove_dir_all(&config.data_dir)?;
    Ok(())
}

#[tokio::test]
async fn missing_binaries_are_reported() -> Result {
    let config = Config::new(
        std::env::temp_dir().join(format!("cmp-missing-{}", std::process::id())),
        PathBuf::from("/nonexistent/postgresql/bin"),
        4,
    );
    let error = ManagedPostgres::start(&config).await;
    if rustix::process::geteuid().is_root() {
        assert!(matches!(error, Err(ManagedError::Root)));
    } else {
        assert!(matches!(error, Err(ManagedError::Binaries(_))));
    }
    Ok(())
}
