//! `cannery db`: dump, restore and upgrade the database.
//!
//! The managed commands own the data directory for their whole run, like
//! `serve`, so they refuse while another `cannery` holds it.
use crate::managed_database;
use cannery_core::{
    db,
    settings::{DatabaseProvider, DatabaseSettings},
};
use cannery_managed_postgres::{
    self as managed, Config, DATABASE, ManagedError, ManagedPostgres,
    tools::{self, ClientTarget},
};
use clap::{Args, Subcommand};
use sqlx::{Connection, PgConnection};
use std::{
    collections::BTreeMap,
    error::Error,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

type BoxError = Box<dyn Error + Send + Sync>;

/// A restore is written here, checked, and only then renamed to `cannery`,
/// so a failed restore leaves the current database as it was.
const STAGING_DATABASE: &str = "cannery_restore";

#[derive(Args)]
pub(crate) struct DbArgs {
    #[command(subcommand)]
    command: DbCommand,
}

#[derive(Subcommand)]
enum DbCommand {
    /// Write an uncompressed custom-format dump (`pg_dump -Fc`) of the
    /// database to FILE, mode 0600, replacing it atomically.
    Dump { file: PathBuf },
    /// Restore a dump into the managed database, then apply this cannery's
    /// pending migrations. Refuses a database that already has tables.
    Restore {
        file: PathBuf,
        /// Discard the current database and replace it with the dump.
        #[arg(long)]
        replace: bool,
    },
    /// Rebuild the managed data directory with this cannery's PostgreSQL:
    /// dump with the old binaries, keep the old directory as
    /// `<data_dir>.pg<old>.bak`, initialize a new one and restore.
    Upgrade {
        /// The bin directory of the PostgreSQL that initialized the data
        /// directory, with `postgres` and `pg_dump`.
        #[arg(long)]
        from_bin_dir: PathBuf,
    },
}

pub(crate) async fn run(arguments: DbArgs, settings: &DatabaseSettings) -> Result<(), BoxError> {
    match (arguments.command, settings.provider) {
        (DbCommand::Dump { file }, DatabaseProvider::Url) => {
            let pg_dump = url_pg_dump(
                settings.postgres_bin_dir.as_deref(),
                std::env::var_os("PATH").as_deref(),
            )?;
            tools::dump(&pg_dump, &url_target(&settings.url)?, &file).await?;
            println!("wrote {}", file.display());
            Ok(())
        }
        (DbCommand::Dump { file }, DatabaseProvider::Managed) => {
            let config = managed_database::config(settings)?;
            let pg_dump = tools::tool(&config.bin_dir, "pg_dump")?;
            let server = start(&config, "db dump").await?;
            let result = tools::dump(&pg_dump, &server.client_target(DATABASE), &file).await;
            stop(server, result.map_err(Into::into)).await?;
            println!("wrote {}", file.display());
            Ok(())
        }
        (DbCommand::Restore { file, replace }, DatabaseProvider::Managed) => {
            let config = managed_database::config(settings)?;
            let pg_restore = tools::tool(&config.bin_dir, "pg_restore")?;
            tools::check_archive(&pg_restore, &file).await?;
            let server = start(&config, "db restore").await?;
            let result = restore(&server, &pg_restore, &file, replace, None).await;
            let applied = stop(server, result).await?;
            println!(
                "restored {}; applied {} migration(s)",
                file.display(),
                applied.len()
            );
            Ok(())
        }
        (DbCommand::Upgrade { from_bin_dir }, DatabaseProvider::Managed) => {
            upgrade(settings, &from_bin_dir).await
        }
        (DbCommand::Restore { .. } | DbCommand::Upgrade { .. }, DatabaseProvider::Url) => Err(
            "this command needs [database] provider = \"managed\"; with a database server, use its own pg_restore and upgrade procedure".into(),
        ),
    }
}

/// Starts the managed server, naming the command when another `cannery`
/// holds the data directory.
async fn start(config: &Config, command: &str) -> Result<ManagedPostgres, BoxError> {
    match ManagedPostgres::start(config).await {
        Ok(server) => Ok(server),
        Err(ManagedError::Locked(path)) => Err(format!(
            "database data directory {} is in use by another cannery process (a running `cannery serve`?); stop it before `cannery {command}`",
            path.display()
        )
        .into()),
        Err(error) => Err(error.into()),
    }
}

/// Stops the server whatever the outcome; the command's error comes first.
async fn stop<T>(server: ManagedPostgres, result: Result<T, BoxError>) -> Result<T, BoxError> {
    let stopped = server.shutdown().await;
    let value = result?;
    stopped?;
    Ok(value)
}

async fn connect(server: &ManagedPostgres, database: &str) -> Result<PgConnection, BoxError> {
    Ok(PgConnection::connect_with(&server.connect_options(database)).await?)
}

/// Restores into [`STAGING_DATABASE`], checks it (`expected` row counts, then
/// the migration bookkeeping, applying pending migrations), and swaps it in
/// for `cannery`. Returns the migrations applied.
async fn restore(
    server: &ManagedPostgres,
    pg_restore: &Path,
    file: &Path,
    replace: bool,
    expected: Option<&BTreeMap<String, i64>>,
) -> Result<Vec<i32>, BoxError> {
    if !replace {
        let mut current = connect(server, DATABASE).await?;
        let tables = row_counts(&mut current).await;
        let _ = current.close().await;
        let tables = tables?.len();
        if tables > 0 {
            return Err(format!(
                "the managed database is not empty ({tables} tables); `cannery db restore --replace` discards it (dump it first)"
            )
            .into());
        }
    }
    let mut admin = connect(server, "postgres").await?;
    let staged = stage(server, &mut admin, pg_restore, file, expected).await;
    let result = match staged {
        Ok(applied) => swap(&mut admin).await.map(|()| applied),
        Err(error) => {
            let _ = drop_database(&mut admin, STAGING_DATABASE).await;
            Err(error)
        }
    };
    let _ = admin.close().await;
    result
}

async fn stage(
    server: &ManagedPostgres,
    admin: &mut PgConnection,
    pg_restore: &Path,
    file: &Path,
    expected: Option<&BTreeMap<String, i64>>,
) -> Result<Vec<i32>, BoxError> {
    drop_database(admin, STAGING_DATABASE).await?;
    sqlx::query(&format!(
        "CREATE DATABASE {STAGING_DATABASE} TEMPLATE template0"
    ))
    .execute(&mut *admin)
    .await?;
    tools::restore(pg_restore, &server.client_target(STAGING_DATABASE), file).await?;
    if let Some(expected) = expected {
        let mut staged = connect(server, STAGING_DATABASE).await?;
        let counts = row_counts(&mut staged).await;
        let _ = staged.close().await;
        let counts = counts?;
        if &counts != expected {
            return Err(format!(
                "the restored database differs from the dump's source: {}",
                count_differences(expected, &counts)
            )
            .into());
        }
    }
    let options = db::DatabaseOptions::parse(&server.database_url(STAGING_DATABASE))?;
    db::migrate(&options).await.map_err(|error| {
        BoxError::from(format!(
            "the restored database does not match this cannery's migrations: {error}"
        ))
    })
}

async fn swap(admin: &mut PgConnection) -> Result<(), BoxError> {
    drop_database(admin, DATABASE).await?;
    sqlx::query(&format!(
        "ALTER DATABASE {STAGING_DATABASE} RENAME TO {DATABASE}"
    ))
    .execute(&mut *admin)
    .await?;
    Ok(())
}

async fn drop_database(admin: &mut PgConnection, database: &str) -> Result<(), BoxError> {
    sqlx::query(&format!("DROP DATABASE IF EXISTS {database} WITH (FORCE)"))
        .execute(&mut *admin)
        .await?;
    Ok(())
}

fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Exact row counts of every ordinary table outside the system schemas.
async fn row_counts(connection: &mut PgConnection) -> Result<BTreeMap<String, i64>, BoxError> {
    let tables: Vec<(String, String)> = sqlx::query_as(
        "SELECT n.nspname::text, c.relname::text FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.relkind = 'r' AND n.nspname <> 'information_schema' \
         AND n.nspname NOT LIKE 'pg\\_%' ORDER BY 1, 2",
    )
    .fetch_all(&mut *connection)
    .await?;
    let mut counts = BTreeMap::new();
    for (schema, table) in tables {
        let count: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {}.{}",
            quote_identifier(&schema),
            quote_identifier(&table)
        ))
        .fetch_one(&mut *connection)
        .await?;
        counts.insert(format!("{schema}.{table}"), count);
    }
    Ok(counts)
}

fn count_differences(expected: &BTreeMap<String, i64>, actual: &BTreeMap<String, i64>) -> String {
    let mut names: Vec<&String> = expected.keys().chain(actual.keys()).collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .filter(|name| expected.get(*name) != actual.get(*name))
        .map(|name| {
            let show =
                |count: Option<&i64>| count.map_or_else(|| "missing".to_owned(), i64::to_string);
            format!(
                "{name} {} -> {}",
                show(expected.get(name)),
                show(actual.get(name))
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `<data_dir>.pg<major>.bak`, next to the data directory.
fn backup_path(data_dir: &Path, major: &str) -> PathBuf {
    let mut path = data_dir.as_os_str().to_owned();
    path.push(format!(".pg{major}.bak"));
    PathBuf::from(path)
}

/// Refuses an upgrade that cannot work before anything is touched.
fn check_upgrade(
    data: &str,
    from: &str,
    to: &str,
    backup: &Path,
    backup_exists: bool,
) -> Result<(), String> {
    if from != data {
        return Err(format!(
            "--from-bin-dir holds PostgreSQL {from} but the data directory was initialized by PostgreSQL {data}"
        ));
    }
    let major = |version: &str| version.parse::<u32>().ok();
    match (major(data), major(to)) {
        (Some(data_major), Some(to_major)) if to_major < data_major => {
            return Err(format!(
                "this cannery runs PostgreSQL {to}, older than the data directory's PostgreSQL {data}; use a newer cannery"
            ));
        }
        (Some(_), Some(_)) => {}
        _ => return Err(format!("unrecognized PostgreSQL versions {data} and {to}")),
    }
    if backup_exists {
        return Err(format!(
            "{} already exists; move it away before upgrading",
            backup.display()
        ));
    }
    Ok(())
}

async fn upgrade(settings: &DatabaseSettings, from_bin_dir: &Path) -> Result<(), BoxError> {
    let config = managed_database::config(settings)?;
    let data_dir = fs::canonicalize(&config.data_dir)
        .map_err(|error| format!("cannot resolve {}: {error}", config.data_dir.display()))?;
    let data = managed::data_major(&data_dir)?.ok_or_else(|| {
        format!(
            "{} holds no PostgreSQL cluster to upgrade",
            data_dir.display()
        )
    })?;
    let from = managed::binaries_major(from_bin_dir).await?;
    let to = managed::binaries_major(&config.bin_dir).await?;
    let backup = backup_path(&data_dir, &data);
    check_upgrade(
        &data,
        &from,
        &to,
        &backup,
        fs::symlink_metadata(&backup).is_ok(),
    )?;
    let old_pg_dump = tools::tool(from_bin_dir, "pg_dump")?;
    let pg_restore = tools::tool(&config.bin_dir, "pg_restore")?;
    let dump_name = format!("upgrade-pg{data}.dump");

    // 1. Dump with the binaries that own the cluster, and count its rows.
    let old_config = Config {
        data_dir: data_dir.clone(),
        bin_dir: from_bin_dir.to_path_buf(),
        ..config.clone()
    };
    let server = start(&old_config, "db upgrade").await?;
    let dumped = async {
        tools::dump(
            &old_pg_dump,
            &server.client_target(DATABASE),
            &data_dir.join(&dump_name),
        )
        .await?;
        let mut connection = connect(&server, DATABASE).await?;
        let counts = row_counts(&mut connection).await;
        let _ = connection.close().await;
        counts
    }
    .await;
    let expected = stop(server, dumped).await?;

    // 2. Keep the old directory (with the dump in it); it is never deleted.
    fs::rename(&data_dir, &backup).map_err(|error| {
        format!(
            "cannot move {} to {}: {error}",
            data_dir.display(),
            backup.display()
        )
    })?;
    tracing::info!(backup = %backup.display(), "kept the PostgreSQL {data} data directory");

    // 3. A fresh cluster with this cannery's binaries, restored and checked.
    let new_config = Config {
        data_dir: data_dir.clone(),
        ..config
    };
    let restored = async {
        let server = start(&new_config, "db upgrade").await?;
        let result = restore(
            &server,
            &pg_restore,
            &backup.join(&dump_name),
            false,
            Some(&expected),
        )
        .await;
        stop(server, result).await
    }
    .await;
    match restored {
        Ok(applied) => {
            println!(
                "upgraded {} from PostgreSQL {data} to {to}; applied {} migration(s); the old data directory is kept at {}",
                data_dir.display(),
                applied.len(),
                backup.display()
            );
            Ok(())
        }
        Err(error) => Err(format!(
            "{error}; the PostgreSQL {data} data directory is untouched at {}: to go back, remove {} and rename it back",
            backup.display(),
            data_dir.display()
        )
        .into()),
    }
}

/// `pg_dump` for `provider = "url"`: from `postgres_bin_dir`, else `PATH`.
fn url_pg_dump(bin_dir: Option<&str>, path: Option<&std::ffi::OsStr>) -> Result<PathBuf, BoxError> {
    let directory = match bin_dir {
        Some(directory) => PathBuf::from(directory),
        None => tools::find_in_path("pg_dump", path).ok_or(
            "cannery db dump needs pg_dump: set database.postgres_bin_dir or put pg_dump on PATH",
        )?,
    };
    Ok(tools::tool(&directory, "pg_dump")?)
}

/// `database.url` as a libpq connection URI without its password, which is
/// returned separately for `PGPASSWORD`. `SQLx`-only parameters are
/// translated to their libpq names or dropped.
fn url_target(input: &str) -> Result<ClientTarget, BoxError> {
    // The same validation, and value-free errors, as connecting.
    db::DatabaseOptions::parse(input)?;
    let invalid = || -> BoxError { "invalid database connection configuration".into() };
    let mut url = url::Url::parse(input).map_err(|_| invalid())?;
    let mut password = match url.password() {
        Some(encoded) => Some(
            percent_encoding::percent_decode_str(encoded)
                .decode_utf8()
                .map_err(|_| invalid())?
                .into_owned(),
        ),
        None => None,
    };
    if password.is_some() {
        url.set_password(None).map_err(|()| invalid())?;
    }
    let mut pairs = Vec::new();
    for (name, value) in url.query_pairs() {
        let name = match name.as_ref() {
            "password" => {
                password = Some(value.into_owned());
                continue;
            }
            "statement-cache-capacity" => continue,
            "ssl-mode" => "sslmode".to_owned(),
            "ssl-root-cert" | "ssl-ca" => "sslrootcert".to_owned(),
            "ssl-cert" => "sslcert".to_owned(),
            "ssl-key" => "sslkey".to_owned(),
            other => other.to_owned(),
        };
        pairs.push((name, value.into_owned()));
    }
    url.set_query(None);
    if !pairs.is_empty() {
        url.query_pairs_mut().extend_pairs(&pairs);
    }
    let mut dbname = OsString::from("--dbname=");
    dbname.push(url.as_str());
    Ok(ClientTarget {
        args: vec![dbname],
        password,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        DbArgs, DbCommand, backup_path, check_upgrade, count_differences, url_pg_dump, url_target,
    };
    use clap::Parser;
    use std::{collections::BTreeMap, path::Path};

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        db: DbArgs,
    }

    #[test]
    fn arguments_parse() {
        assert!(matches!(
            Cli::try_parse_from(["db", "restore", "x.dump", "--replace"]).map(|cli| cli.db.command),
            Ok(DbCommand::Restore { replace: true, .. })
        ));
        assert!(matches!(
            Cli::try_parse_from(["db", "dump", "x.dump"]).map(|cli| cli.db.command),
            Ok(DbCommand::Dump { .. })
        ));
        assert!(Cli::try_parse_from(["db", "dump"]).is_err());
        assert!(Cli::try_parse_from(["db", "upgrade"]).is_err());
        assert!(Cli::try_parse_from(["db", "upgrade", "--from-bin-dir", "/old/bin"]).is_ok());
    }

    #[test]
    fn the_url_password_never_reaches_the_arguments()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let target = url_target(
            "postgresql://ops:p%40ss@db.example:5433/cannery?ssl-mode=require&statement-cache-capacity=10&application_name=backup",
        )?;
        assert_eq!(target.password.as_deref(), Some("p@ss"));
        assert_eq!(
            target.args,
            [
                "--dbname=postgresql://ops@db.example:5433/cannery?sslmode=require&application_name=backup"
            ]
        );
        let target = url_target("postgresql:///cannery?host=%2Frun%2Fpg&password=secret")?;
        assert_eq!(target.password.as_deref(), Some("secret"));
        assert_eq!(
            target.args,
            ["--dbname=postgresql:///cannery?host=%2Frun%2Fpg"]
        );
        let target = url_target("postgres://db.example/cannery")?;
        assert_eq!(target.password, None);
        assert!(url_target("postgresql://db/cannery?unknown=1").is_err());
        assert!(url_target("mysql://db/cannery").is_err());
        Ok(())
    }

    #[test]
    fn url_dumps_need_a_pg_dump() {
        let error = url_pg_dump(None, None)
            .map(|_| ())
            .map_err(|error| error.to_string());
        assert!(error.is_err_and(|error| error.contains("postgres_bin_dir")));
        let error = url_pg_dump(Some("/nonexistent/bin"), None)
            .map(|_| ())
            .map_err(|error| error.to_string());
        assert!(error.is_err_and(|error| error.contains("no pg_dump in /nonexistent/bin")));
    }

    #[test]
    fn upgrades_are_checked_before_anything_moves() {
        let backup = backup_path(Path::new("/data/postgres"), "17");
        assert_eq!(backup, Path::new("/data/postgres.pg17.bak"));
        assert!(check_upgrade("17", "17", "18", &backup, false).is_ok());
        // Same major: a rebuild, used to exercise the path with one major.
        assert!(check_upgrade("17", "17", "17", &backup, false).is_ok());
        assert!(
            check_upgrade("17", "16", "18", &backup, false)
                .is_err_and(|error| error.contains("--from-bin-dir"))
        );
        assert!(
            check_upgrade("18", "18", "17", &backup, false)
                .is_err_and(|error| error.contains("older"))
        );
        assert!(
            check_upgrade("17", "17", "18", &backup, true)
                .is_err_and(|error| error.contains("already exists"))
        );
        assert!(check_upgrade("x", "x", "18", &backup, false).is_err());
    }

    #[test]
    fn count_differences_name_each_table() {
        let expected = BTreeMap::from([("public.a".to_owned(), 2), ("public.b".to_owned(), 1)]);
        let actual = BTreeMap::from([("public.a".to_owned(), 1), ("public.c".to_owned(), 0)]);
        assert_eq!(
            count_differences(&expected, &actual),
            "public.a 2 -> 1, public.b 1 -> missing, public.c missing -> 0"
        );
    }
}
