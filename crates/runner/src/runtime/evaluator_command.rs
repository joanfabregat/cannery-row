//! Dedicated stock evaluator command, using HTTP and its service credential only.
use super::RuntimeError;
use crate::{config, launcher::PosixPath, policy};

use clap::Args;
use std::{path::PathBuf, time::Duration};

#[derive(Args)]
pub struct EvaluatorArgs {
    #[arg(long)]
    pub token_file: Option<PathBuf>,
    #[arg(long)]
    pub api_url: String,
    #[arg(long)]
    pub project: String,
    #[arg(long)]
    pub config: PathBuf,
    #[arg(long, default_value_t = 10.0)]
    pub poll_seconds: f64,
    #[arg(long)]
    pub once: bool,
}

/// # Errors
/// Reject invalid policy, private file and polling settings before contacting the API.
pub async fn run(args: EvaluatorArgs) -> Result<i32, RuntimeError> {
    let token_file = args
        .token_file
        .or_else(|| {
            std::env::var("CANNERY_EVALUATOR_TOKEN_FILE")
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .ok_or(RuntimeError::Configuration)?;
    let token = crate::credentials::read_token_file(&token_file)
        .map_err(|_| RuntimeError::Configuration)?;
    if !args.poll_seconds.is_finite()
        || args.poll_seconds < 0.0
        || Duration::try_from_secs_f64(args.poll_seconds).is_err()
    {
        return Err(RuntimeError::Configuration);
    }
    let loader = policy::FilePolicyLoader {
        entry_point: crate::cli_depth::PolicyEntryPoint::Evaluator,
        repr_nesting_budget: 256,
    };
    let path = args.config.to_str().ok_or(RuntimeError::Configuration)?;
    let candidate = loader
        .load_policy(&PosixPath::new(&String::from(path)))
        .map_err(|_| RuntimeError::Configuration)?;
    let policy::Policy::Stock(policy) = candidate else {
        return Err(RuntimeError::Configuration);
    };
    let config = config::ProcessConfig {
        api_url: String::from(&args.api_url),
        project: String::from(&args.project),
        kinds: vec![config::KindEntry {
            name: String::from("eval"),
            kind: config::KindConfig::Eval(config::EvaluationPolicy::Stock(policy.document)),
            token,
            poll_seconds: args.poll_seconds,
            concurrency: 1.into(),
        }],
        launcher: None,
        step_root: None,
        data_root: None,
        work_root: None,
        cache_root: None,
        cache_max_bytes: None,
        runner_id: None,
        docker: config::DockerSettings::default(),
        kubernetes: config::KubernetesSettings::default(),
        github: config::GitHubSettings::default(),
    };
    super::command::run_config(config, args.once).await
}
