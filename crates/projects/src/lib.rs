//! Project repositories and project-scoped authorization.
#![forbid(unsafe_code)]

pub mod authz;
pub mod briefs;
mod error;
pub mod repo;

pub use error::ProjectError;
