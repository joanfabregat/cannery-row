//! Native runner integration. Transport credentials never enter step contracts.
pub mod backend;
pub mod command;
pub mod container_command;
pub mod evaluator;
pub mod evaluator_command;
pub mod experiment;
pub mod github;
pub mod http;
pub mod policy_evaluator;
pub mod process;
pub mod provision;
pub mod validation;
pub mod worker;

use std::{future::Future, pin::Pin};

pub type FutureResult<'a, T> = Pin<Box<dyn Future<Output = Result<T, RuntimeError>> + Send + 'a>>;

/// Deliberately value-free: reqwest errors can contain signed URLs and headers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RuntimeError {
    #[error("runner configuration is invalid")]
    Configuration,
    #[error("runner cache root is already in use")]
    CacheRootInUse,
    #[error("runner HTTP transport failed")]
    Transport,
    #[error("runner API returned HTTP {0}")]
    Http(u16),
    #[error("runner API returned an invalid contract")]
    Contract,
    #[error("runner filesystem operation failed")]
    Filesystem,
    #[error("runner job lease was lost")]
    LostLease,
    #[error("runner operation was cancelled")]
    Cancelled,
    #[error("runner input differs from its pinned digest")]
    Integrity,
    #[error("runner launch failed")]
    Launch,
    #[error("runner step failed")]
    StepFailed,
    #[error("runner setup failed")]
    SetupFailed,
    #[error("runner code repository is not allowed")]
    CodeNotAllowed,
    #[error("runner code tree is invalid")]
    InvalidCode,
    #[error("runner step exceeded its deadline")]
    DeadlineExceeded,
    #[error("runner step output is invalid")]
    InvalidStepOutput,
    #[error("runner output upload was refused")]
    InvalidOutput,
    #[error("runner upload grant expired")]
    UploadExpired,
    #[error("runner job was failed by the API")]
    DurablyFailed,
    #[error("runner step did not produce its declared output")]
    MissingOutput,
}
impl From<std::io::Error> for RuntimeError {
    fn from(_: std::io::Error) -> Self {
        Self::Filesystem
    }
}
impl From<reqwest::Error> for RuntimeError {
    fn from(_: reqwest::Error) -> Self {
        Self::Transport
    }
}
impl From<serde_json::Error> for RuntimeError {
    fn from(_: serde_json::Error) -> Self {
        Self::Contract
    }
}
