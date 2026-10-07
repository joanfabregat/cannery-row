//! Project repositories and project-scoped authorization.
#![forbid(unsafe_code)]

pub mod authz;
mod error;
pub mod repo;

pub use error::ProjectError;
