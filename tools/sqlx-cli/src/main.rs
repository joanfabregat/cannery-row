//! Cargo argument adapter for the unchanged upstream `SQLx` CLI library.
#![forbid(unsafe_code)]
use clap::Parser;
use sqlx_cli::Opt;

// Cargo invokes this binary as `cargo-sqlx sqlx <arguments>`.
#[derive(Debug, Parser)]
#[command(bin_name = "cargo")]
enum Cli {
    Sqlx(Opt),
}

#[tokio::main]
async fn main() {
    sqlx_cli::maybe_apply_dotenv();
    sqlx::any::install_default_drivers();
    let Cli::Sqlx(options) = Cli::parse();
    if let Err(error) = sqlx_cli::run(options).await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
