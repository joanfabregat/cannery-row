//! `[database] provider = "managed"`: the private PostgreSQL server.
use cannery_core::settings::DatabaseSettings;
use cannery_managed_postgres::{Config, ManagedPostgres, default_cache_dir, default_data_dir};
use std::{error::Error, path::PathBuf};

type BoxError = Box<dyn Error + Send + Sync>;

#[cfg(cannery_bundled_postgres)]
static BUNDLE: &[u8] = include_bytes!(env!("CANNERY_POSTGRES_BUNDLE_PATH"));
#[cfg(cannery_bundled_postgres)]
const BUNDLE_SHA256: &str = env!("CANNERY_POSTGRES_BUNDLE_SHA256");

/// Starts the managed server and returns it; the caller points
/// `database.url` at [`ManagedPostgres::url`] and shuts it down on exit.
pub(crate) async fn start(settings: &DatabaseSettings) -> Result<ManagedPostgres, BoxError> {
    Ok(ManagedPostgres::start(&config(settings)?).await?)
}

/// The managed server's configuration: the data directory, and the
/// configured binaries or the bundled ones (unpacked on first use).
pub(crate) fn config(settings: &DatabaseSettings) -> Result<Config, BoxError> {
    let data_dir = settings
        .data_dir
        .as_deref()
        .map(PathBuf::from)
        .or_else(default_data_dir)
        .ok_or("database.data_dir is not set and HOME is unavailable")?;
    let bin_dir = match settings.postgres_bin_dir.as_deref() {
        Some(directory) => PathBuf::from(directory),
        None => bundled_bin_dir(settings)?,
    };
    let pool_max_size = settings
        .pool_max_size
        .to_u64("database.pool_max_size")?
        .try_into()
        .map_err(|_| "database.pool_max_size is too large")?;
    let mut config = Config::new(data_dir, bin_dir, pool_max_size);
    config.exec_shim = std::env::current_exe().ok();
    Ok(config)
}

#[cfg(cannery_bundled_postgres)]
fn bundled_bin_dir(settings: &DatabaseSettings) -> Result<PathBuf, BoxError> {
    let cache = settings
        .postgres_cache_dir
        .as_deref()
        .map(PathBuf::from)
        .or_else(default_cache_dir)
        .ok_or("database.postgres_cache_dir is not set and HOME is unavailable")?;
    let installed = cannery_managed_postgres::bundle::install(BUNDLE, BUNDLE_SHA256, &cache)?;
    Ok(installed.join("bin"))
}

#[cfg(not(cannery_bundled_postgres))]
fn bundled_bin_dir(settings: &DatabaseSettings) -> Result<PathBuf, BoxError> {
    let _ = (&settings.postgres_cache_dir, default_cache_dir);
    Err("this cannery has no bundled PostgreSQL: set database.postgres_bin_dir to a directory with postgres and initdb".into())
}
