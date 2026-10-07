//! Identity repositories, opaque credentials, and relying-party authentication.
#![forbid(unsafe_code)]
pub mod auth;
pub mod error;
pub mod models;
pub mod repo;
pub mod secrets;
pub use error::{IdentityError, Result};
