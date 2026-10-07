//! Bounded historical import installation with owned database settlement.
use cannery_core::{contracts::ContractValidator, settings::Settings};
use cannery_imports::{BundleLimits, ImportContext, ImportOptions};
use clap::Args;
use serde_json::Value;
use std::{error::Error, fs::File, io::Read, path::PathBuf, sync::Arc, time::Duration};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Args)]
pub struct ImportArgs {
    #[arg(long)]
    bundle: PathBuf,
    #[arg(long)]
    project: String,
    /// Register this JSON science document, or use the project's current revision.
    #[arg(long)]
    science: Option<PathBuf>,
    /// Validate and execute the complete import, then roll back.
    #[arg(long)]
    dry_run: bool,
    /// Retain previously imported entries absent from this bundle.
    #[arg(long)]
    allow_missing: bool,
    /// PostgreSQL per-statement deadline, including lock waits (1–300 seconds).
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=300))]
    statement_timeout_seconds: u64,
}

const LIMITS: BundleLimits = BundleLimits {
    max_files: 20_000,
    max_file_bytes: 8 * 1024 * 1024,
    max_report_bytes: 256 * 1024,
    max_total_bytes: 32 * 1024 * 1024,
    max_depth: 200,
    max_nodes: 200_000,
};

fn science_file(path: &std::path::Path, depth: usize) -> Result<Value> {
    use rustix::fs::{Mode, OFlags, open};
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| "cannot safely open the science JSON file")?;
    let file = File::from(descriptor);
    if !file
        .metadata()
        .map_err(|_| "cannot inspect the science JSON file")?
        .is_file()
    {
        return Err("science JSON must be a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(u64::try_from(LIMITS.max_file_bytes)? + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read the science JSON file")?;
    if bytes.len() > LIMITS.max_file_bytes {
        return Err("science JSON exceeds the 8 MiB limit".into());
    }
    // Decode through the same finite, bounded codec used by installation writes.
    let document = cannery_core::json::decode(&bytes, depth)
        .map_err(|_| "invalid or excessively nested science JSON")?;
    let bytes = cannery_core::json::encode_http(&document, depth)
        .map_err(|_| "science JSON must contain finite JSON values")?;
    serde_json::from_slice(&bytes).map_err(|_| "invalid science JSON".into())
}

pub async fn run(arguments: ImportArgs, settings: &Settings) -> Result<()> {
    let (rendering, json_budget) = cannery_server::research_validation_policy();
    let contracts = Arc::new(ContractValidator::new().map_err(|_| "cannot initialize contracts")?);
    let reader_contracts = contracts.clone();
    let root = arguments.bundle;
    let science = arguments.science;
    let (bundle, science) = tokio::task::spawn_blocking(move || -> Result<_> {
        let bundle = cannery_imports::read_bundle(&root, LIMITS, &reader_contracts)?;
        let science = science
            .as_deref()
            .map(|path| science_file(path, json_budget))
            .transpose()?;
        Ok((bundle, science))
    })
    .await
    .map_err(|_| "bundle reader failed")??;
    let options = cannery_core::db::DatabaseOptions::parse(&settings.database.url)?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(options.connect_timeout().unwrap_or(Duration::from_secs(30)))
        .connect_lazy_with(options.connect_options().clone());
    let context = ImportContext {
        contracts,

        rendering,
        json_budget,
        statement_timeout: Duration::from_secs(arguments.statement_timeout_seconds),
    };
    let result: Result<cannery_imports::Plan> = tokio::select! {
        result = cannery_imports::run_import(&pool, &bundle, ImportOptions {
            slug: &arguments.project,
            science_document: science.as_ref(),
            dry_run: arguments.dry_run,
            allow_missing: arguments.allow_missing,
        }, &context) => result.map_err(Into::into),
        () = interrupted() => Err("import interrupted".into()),
    };
    // SQLx settles canceled statements and rolls back before closing the owner.
    pool.close().await;
    if let Err(error) = &result
        && let Some(cannery_imports::Error::Refused(problems)) = error.downcast_ref()
    {
        for problem in problems {
            eprintln!("cannery import: {}", serde_json::to_string(problem)?);
        }
    }
    let plan: cannery_imports::Plan = result?;
    for line in plan.describe(arguments.dry_run) {
        println!("{line}");
    }
    Ok(())
}

async fn interrupted() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = terminate.recv() => {},
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
