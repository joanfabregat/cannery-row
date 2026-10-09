//! Runs Rust test executables for CI and local checks.
//!
//! `selected -- TEST ARGS...` lists the tests TEST ARGS selects, fails if
//! there are none (a renamed test must not pass by running nothing), then runs
//! them.
//!
//! `database --server-binary CANNERY [OPTIONS] -- TEST ARGS...` does the same
//! against a database of its own: from the administrator URL in
//! `CANNERY_TEST_DATABASE_URL` it creates `<prefix><random>` (`--prefix`,
//! default `conformance_`), migrates it with `CANNERY migrate`, runs TEST with
//! that database's URL in the standard test variables and in every
//! `--env NAME`, plus every `--set NAME=VALUE`, and drops the database however
//! the run ends. Inherited `CANNERY_*`, `AWS_*` and `S3_*` variables are not
//! passed on.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

use clap::{Parser, Subcommand};
use sqlx::{Connection, Executor, PgConnection};
use url::Url;

/// Variables the database tests read their URL from.
const DATABASE_VARIABLES: &[&str] = &[
    "CANNERY_ARTIFACT_DOWNLOAD_HTTP_DATABASE_URL",
    "CANNERY_ATTEMPTS_TEST_DATABASE_URL",
    "CANNERY_ATTEMPT_CLAIMS_HTTP_DATABASE_URL",
    "CANNERY_ATTEMPT_LEASES_HTTP_DATABASE_URL",
    "CANNERY_ATTEMPT_READS_HTTP_DATABASE_URL",
    "CANNERY_ATTEMPT_RELEASE_DATABASE_URL",
    "CANNERY_AUDIT_TEST_DATABASE_URL",
    "CANNERY_COMMENTS_REPORTS_DATABASE_URL",
    "CANNERY_COMMENT_MUTATION_DATABASE_URL",
    "CANNERY_COMMENT_READS_DATABASE_URL",
    "CANNERY_CONFIG_TEST_DATABASE_URL",
    "CANNERY_DATABASE_URL",
    "CANNERY_HYPOTHESIS_HTTP_DATABASE_URL",
    "CANNERY_HYPOTHESIS_MUTATION_DATABASE_URL",
    "CANNERY_IDENTITY_TEST_DATABASE_URL",
    "CANNERY_JOBS_TEST_DATABASE_URL",
    "CANNERY_JOB_CLAIMS_HTTP_DATABASE_URL",
    "CANNERY_JOB_INPUTS_HTTP_DATABASE_URL",
    "CANNERY_JOB_LIFECYCLE_DATABASE_URL",
    "CANNERY_JOB_READS_HTTP_DATABASE_URL",
    "CANNERY_MCP_DATABASE_URL",
    "CANNERY_METRICS_TEST_DATABASE_URL",
    "CANNERY_METRIC_HTTP_DATABASE_URL",
    "CANNERY_MIGRATION_ADMIN_URL",
    "CANNERY_PLANS_HTTP_DATABASE_URL",
    "CANNERY_PREDECESSOR_INPUT_HTTP_DATABASE_URL",
    "CANNERY_PROJECTS_TEST_DATABASE_URL",
    "CANNERY_REPORT_READS_HTTP_DATABASE_URL",
    "CANNERY_REVIEW_ATTENTION_HTTP_DATABASE_URL",
    "CANNERY_REVIEW_DECISIONS_HTTP_DATABASE_URL",
    "CANNERY_SEARCH_HTTP_DATABASE_URL",
    "CANNERY_SEARCH_TEST_DATABASE_URL",
    "CANNERY_SQL_TYPE_TEST_DATABASE_URL",
    "CANNERY_STEP_BINDING_DATABASE_URL",
    "CANNERY_STORAGE_HOOK_DATABASE_URL",
    "CANNERY_TEST_DATABASE_URL",
    "CANNERY_TRACKS_TEST_DATABASE_URL",
    "CANNERY_TRACK_HTTP_DATABASE_URL",
    "CANNERY_UPLOAD_HTTP_DATABASE_URL",
    "CANNERY_WORKFLOW_DATABASE_URL",
];

#[derive(Parser)]
#[command(about = "Run Rust test executables, optionally against an isolated database")]
struct Cli {
    #[command(subcommand)]
    command: Mode,
}

#[derive(Subcommand)]
enum Mode {
    /// Run TEST ARGS... after checking that it selects at least one test.
    Selected {
        #[arg(last = true, required = true)]
        test: Vec<OsString>,
    },
    /// Run TEST ARGS... against a new migrated database, then drop it.
    Database {
        /// The `cannery` binary that migrates the database.
        #[arg(long)]
        server_binary: PathBuf,
        #[command(flatten)]
        options: DatabaseOptions,
        #[arg(last = true, required = true)]
        test: Vec<OsString>,
    },
}

#[derive(clap::Args)]
struct DatabaseOptions {
    /// Another variable to set to the database URL.
    #[arg(long = "env", value_name = "NAME")]
    extra: Vec<String>,
    /// A variable to pass to the test, such as a reference path.
    #[arg(long = "set", value_name = "NAME=VALUE", value_parser = parse_assignment)]
    set: Vec<(String, String)>,
    /// The database name prefix some tests require; a random suffix of 24
    /// hexadecimal digits follows it.
    #[arg(long, default_value = "conformance_")]
    prefix: String,
}

fn parse_assignment(text: &str) -> Result<(String, String), String> {
    let (name, value) = text.split_once('=').ok_or("expected NAME=VALUE")?;
    if name.is_empty() {
        return Err("expected NAME=VALUE".into());
    }
    Ok((name.to_owned(), value.to_owned()))
}

type Error = Box<dyn std::error::Error>;

fn main() -> ExitCode {
    match run(Cli::parse().command) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("cannery-test-launcher: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(mode: Mode) -> Result<ExitCode, Error> {
    match mode {
        Mode::Selected { test } => {
            require_selected(&test)?;
            exit_code(&mut command(&test)?)
        }
        Mode::Database {
            server_binary,
            options,
            test,
        } => {
            require_selected(&test)?;
            let admin = std::env::var("CANNERY_TEST_DATABASE_URL")
                .map_err(|_| "CANNERY_TEST_DATABASE_URL is not set")?;
            let admin = Url::parse(&admin)?;
            if !matches!(admin.scheme(), "postgres" | "postgresql") || admin.host().is_none() {
                return Err(
                    "CANNERY_TEST_DATABASE_URL must be a postgresql:// URL with a host".into(),
                );
            }
            if admin.query_pairs().any(|(key, _)| key == "dbname") {
                return Err("CANNERY_TEST_DATABASE_URL must not override dbname".into());
            }
            if options.prefix.is_empty()
                || !options
                    .prefix
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
            {
                return Err("--prefix takes lowercase letters and underscores".into());
            }
            let name = database_name(&options.prefix)?;
            let mut database = admin.clone();
            database.set_path(&format!("/{name}"));

            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime.block_on(execute(&admin, &format!("CREATE DATABASE \"{name}\"")))?;
            let result = migrate_and_run(&server_binary, &database, &options, &test);
            let dropped = runtime.block_on(execute(
                &admin,
                &format!("DROP DATABASE \"{name}\" WITH (FORCE)"),
            ));
            let code = result?;
            dropped?;
            Ok(code)
        }
    }
}

fn migrate_and_run(
    server: &PathBuf,
    database: &Url,
    options: &DatabaseOptions,
    test: &[OsString],
) -> Result<ExitCode, Error> {
    let environment = |command: &mut Command| {
        for (key, _) in std::env::vars_os() {
            let key_text = key.to_string_lossy();
            if ["CANNERY_", "AWS_", "S3_"]
                .iter()
                .any(|prefix| key_text.starts_with(prefix))
            {
                command.env_remove(&key);
            }
        }
        for name in DATABASE_VARIABLES
            .iter()
            .copied()
            .chain(options.extra.iter().map(String::as_str))
        {
            command.env(name, database.as_str());
        }
        for (name, value) in &options.set {
            command.env(name, value);
        }
    };
    let mut migrate = Command::new(server);
    migrate
        .arg("migrate")
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    environment(&mut migrate);
    if !migrate.status()?.success() {
        return Err("migrating the test database failed".into());
    }
    let mut test = command(test)?;
    environment(&mut test);
    exit_code(&mut test)
}

fn require_selected(test: &[OsString]) -> Result<(), Error> {
    if test
        .iter()
        .skip(1)
        .any(|arg| arg == "--list" || arg == "--help")
    {
        return Err("the test command must run tests, not list them".into());
    }
    let output = command(test)?.arg("--list").stdin(Stdio::null()).output()?;
    if !output.status.success() {
        return Err("listing the selected tests failed".into());
    }
    let listing = String::from_utf8(output.stdout).map_err(|_| "the test listing is not UTF-8")?;
    if !listing.lines().any(|line| line.ends_with(": test")) {
        return Err("the test selection matched no tests".into());
    }
    Ok(())
}

fn command(test: &[OsString]) -> Result<Command, Error> {
    let (program, args) = test
        .split_first()
        .ok_or("a test executable is required after --")?;
    let mut command = Command::new(program);
    command.args(args);
    Ok(command)
}

fn exit_code(command: &mut Command) -> Result<ExitCode, Error> {
    let status = command.stdin(Stdio::null()).status()?;
    Ok(match status.code() {
        Some(0) => ExitCode::SUCCESS,
        Some(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        None => ExitCode::FAILURE,
    })
}

fn database_name(prefix: &str) -> Result<String, Error> {
    let mut bytes = [0_u8; 12];
    getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes.iter().fold(String::from(prefix), |mut name, byte| {
        let _ = write!(name, "{byte:02x}");
        name
    }))
}

async fn execute(admin: &Url, statement: &str) -> Result<(), Error> {
    let mut connection = PgConnection::connect(admin.as_str()).await?;
    connection.execute(statement).await?;
    connection.close().await?;
    Ok(())
}
