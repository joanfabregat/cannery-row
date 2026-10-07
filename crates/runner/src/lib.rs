//! Source-compatible runner contracts, independent of launch backends and HTTP.
#![forbid(unsafe_code)]
pub mod archive;
pub mod cache;
pub mod cancellation;
pub mod cli_depth;
pub mod code_store;
pub mod config;
pub mod container_backends;
pub mod credentials;
pub mod evaluator_inputs;
pub mod failure_report;
pub mod files;
pub mod gates;
pub mod github_retry;
pub mod job;
pub mod launcher;
pub mod lease_retry;
pub mod local_preparation;
pub mod local_process;
pub mod paths;
pub mod policy;
pub mod preparation;
pub mod removal;
pub mod runtime;
pub mod session_lifecycle;
pub mod setup;
pub mod worker_process;
