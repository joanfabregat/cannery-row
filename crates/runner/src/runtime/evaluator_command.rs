//! `cannery evaluator`: the stock policy, offline. It applies a stock policy
//! configuration's gates to a scorer's evidence and prints the verdict a
//! verification report takes (`verdict`, `reason`, `policy_revision`, `gates`,
//! `comparisons`), so a verifier that runs its steps itself applies the same
//! gates as the runner's verify kind. It reads local files only: no
//! credential, network or launcher.
use super::RuntimeError;
use crate::{gates::Control, launcher::PosixPath, policy, verification};
use clap::Args;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct EvaluatorArgs {
    /// The stock policy configuration; a policy step document is refused.
    #[arg(long)]
    pub config: PathBuf,
    /// The science revision: its content, or the API's response for it.
    #[arg(long)]
    pub science: PathBuf,
    /// The scorer's evidence: provenance and verified measurements.
    #[arg(long)]
    pub evidence: PathBuf,
    /// The control the unit names; the policy's default control otherwise.
    #[arg(long, requires = "control_revision")]
    pub control_id: Option<String>,
    #[arg(long, requires = "control_id")]
    pub control_revision: Option<String>,
}

const DEPTH: usize = 256;

fn read(path: &Path) -> Result<Value, RuntimeError> {
    let bytes = std::fs::read(path).map_err(|_| RuntimeError::Configuration)?;
    cannery_core::json::decode(&bytes, crate::cli_depth::JSON_CONTAINERS)
        .map_err(|_| RuntimeError::Configuration)?;
    serde_json::from_slice(&bytes).map_err(|_| RuntimeError::Configuration)
}

/// # Errors
/// Refuses an invalid policy or unreadable input (configuration, exit 2) and
/// evidence the gates cannot read (exit 1).
pub fn run(args: EvaluatorArgs) -> Result<i32, RuntimeError> {
    let loader = policy::FilePolicyLoader {
        entry_point: crate::cli_depth::PolicyEntryPoint::Evaluator,
        repr_nesting_budget: DEPTH,
    };
    let path = args.config.to_str().ok_or(RuntimeError::Configuration)?;
    let policy::Policy::Stock(policy) = loader
        .load_policy(&PosixPath::new(&String::from(path)))
        .map_err(|_| RuntimeError::Configuration)?
    else {
        return Err(RuntimeError::Configuration);
    };
    let science = read(&args.science)?;
    let science = science.get("content").cloned().unwrap_or(science);
    let evidence = read(&args.evidence)?;
    let control = args
        .control_id
        .zip(args.control_revision)
        .map(|(id, revision)| Control { id, revision });
    let mut verdict =
        verification::stock_assessment(&policy, &science, &evidence, control.as_ref(), DEPTH)
            .map_err(|_| RuntimeError::Contract)?;
    verdict["policy_revision"] = Value::String(policy.revision.clone());
    println!("{}", serde_json::to_string_pretty(&verdict)?);
    Ok(0)
}
