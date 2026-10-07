//! Cannery Row command-line entry point.
#![forbid(unsafe_code)]

use cannery_core::{
    db,
    settings::{DatabaseProvider, Settings, environment_names, load_settings},
};
use clap::{Parser, Subcommand};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    io::Write,
    path::PathBuf,
    process::ExitCode,
};
mod db_command;
mod import_command;
mod managed_database;

#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
#[command(name = "cannery", about = "Cannery Row research coordination")]
struct Arguments {
    /// Settings TOML file (or `CANNERY_SETTINGS`).
    #[arg(long)]
    settings: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Claim and execute jobs with a dedicated runner credential.
    Runner(Box<cannery_runner::runtime::command::RunnerArgs>),
    /// Apply a stock evaluation policy using a dedicated evaluator credential.
    Evaluator(cannery_runner::runtime::evaluator_command::EvaluatorArgs),
    /// Apply pending database migrations.
    Migrate,
    /// Dump, restore or upgrade the database.
    Db(db_command::DbArgs),
    /// Atomically import reviewed historical research bundles.
    Import(import_command::ImportArgs),
    /// Run the API server.
    Serve {
        #[arg(long, default_value = "0.0.0.0")]
        host: String,
        #[arg(long, default_value_t = 8000)]
        port: i64,
        /// Comma-separated proxy addresses whose forwarded headers are trusted.
        #[arg(long)]
        forwarded_allow_ips: Option<String>,
    },
    /// Write the frozen `OpenAPI` document; needs no settings or database.
    Openapi {
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    if let Some(result) = cannery_runner::local_process::dispatch_bootstrap() {
        return result;
    }
    if let Some(result) = cannery_managed_postgres::dispatch_exec_shim() {
        return result;
    }
    if let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        runtime.block_on(regular_main())
    } else {
        eprintln!("cannery: cannot start asynchronous runtime");
        ExitCode::FAILURE
    }
}

async fn regular_main() -> ExitCode {
    let Arguments { settings, command } = Arguments::parse();
    let command = match command {
        Command::Runner(arguments) => {
            return match cannery_runner::runtime::command::run(*arguments).await {
                Ok(status) => ExitCode::from(u8::try_from(status).unwrap_or(1)),
                Err(error) => {
                    eprintln!("cannery runner: {error}");
                    ExitCode::from(
                        if error == cannery_runner::runtime::RuntimeError::Configuration {
                            2
                        } else {
                            1
                        },
                    )
                }
            };
        }
        Command::Evaluator(arguments) => {
            return match cannery_runner::runtime::evaluator_command::run(arguments).await {
                Ok(status) => ExitCode::from(u8::try_from(status).unwrap_or(1)),
                Err(error) => {
                    eprintln!("cannery evaluator: {error}");
                    ExitCode::from(
                        if error == cannery_runner::runtime::RuntimeError::Configuration {
                            2
                        } else {
                            1
                        },
                    )
                }
            };
        }
        command => command,
    };
    let arguments = Arguments { settings, command };
    match Box::pin(run(arguments)).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cannery: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(arguments: Arguments) -> Result<(), Box<dyn Error + Send + Sync>> {
    if let Command::Openapi { output } = arguments.command {
        let document = cannery_server::generated_openapi().to_pretty_json()? + "\n";
        if let Some(path) = output {
            std::fs::write(path, document.as_bytes())?;
        } else {
            std::io::stdout().lock().write_all(document.as_bytes())?;
        }
        return Ok(());
    }
    let environment = installation_environment()?;
    let mut settings = load_settings(arguments.settings.as_deref(), &environment)?;
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| "could not initialize TLS provider")?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .json()
        .with_writer(std::io::stderr)
        .try_init()?;
    // `db` commands start and stop a managed server themselves.
    if let Command::Db(arguments) = arguments.command {
        return db_command::run(arguments, &settings.database).await;
    }
    // A managed database runs for the life of this command and is stopped
    // with a fast shutdown whatever the command's outcome.
    let Some(mut managed) = (match settings.database.provider {
        DatabaseProvider::Url => None,
        DatabaseProvider::Managed => Some(managed_database::start(&settings.database).await?),
    }) else {
        return Box::pin(run_command(arguments.command, settings, &environment)).await;
    };
    settings.database.url = managed.url();
    let result = tokio::select! {
        result = Box::pin(run_command(arguments.command, settings, &environment)) => result,
        status = managed.exited() => Err(format!(
            "managed PostgreSQL exited unexpectedly ({}); see {}",
            status.map_or_else(|error| error.to_string(), |status| status.to_string()),
            managed.log_path().display()
        ).into()),
    };
    let stopped = managed.shutdown().await;
    result?;
    Ok(stopped?)
}

async fn run_command(
    command: Command,
    settings: Settings,
    environment: &BTreeMap<String, String>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let managed = settings.database.provider == DatabaseProvider::Managed;
    match command {
        Command::Import(arguments) => import_command::run(arguments, &settings).await?,
        Command::Migrate => {
            let options = db::DatabaseOptions::parse(&settings.database.url)?;
            let applied = db::migrate(&options).await?;
            print!("applied {} migration(s)", applied.len());
            if !applied.is_empty() {
                print!(": {applied:?}");
            }
            println!();
        }
        Command::Serve {
            host,
            port,
            forwarded_allow_ips,
        } => {
            let port = u16::try_from(port).map_err(|_| "serve port is outside 0..65535")?;
            let forwarded_allow_ips = forwarded_allow_ips
                .or_else(|| environment.get("FORWARDED_ALLOW_IPS").cloned())
                .unwrap_or_else(|| "127.0.0.1,::1".to_owned());
            // Only this process can reach a managed database, so it migrates
            // it before serving; with a URL, `migrate` stays a
            // separate deployment step.
            if managed {
                let options = db::DatabaseOptions::parse(&settings.database.url)?;
                let applied = db::migrate(&options).await?;
                tracing::info!(applied = applied.len(), "managed database migrated");
            }
            cannery_server::serve(settings, &host, port, &forwarded_allow_ips).await?;
        }
        Command::Openapi { .. } => {}
        Command::Runner(_) | Command::Evaluator(_) | Command::Db(_) => {
            return Err(std::io::Error::other("runner command dispatch failed").into());
        }
    }
    Ok(())
}

fn installation_environment() -> Result<BTreeMap<String, String>, Box<dyn Error + Send + Sync>> {
    let names: BTreeSet<_> = environment_names()
        .into_iter()
        .chain(std::iter::once("FORWARDED_ALLOW_IPS".to_owned()))
        .collect();
    let mut environment = BTreeMap::new();
    for (name, value) in std::env::vars_os() {
        let Some(name) = name.to_str() else {
            continue;
        };
        if !names.contains(name) {
            continue;
        }
        let value = value
            .into_string()
            .map_err(|_| format!("invalid environment variable {name}: value is not UTF-8"))?;
        environment.insert(name.to_owned(), value);
    }
    Ok(environment)
}
