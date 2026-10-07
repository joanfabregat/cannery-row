//! Job persistence only; transaction, authorization and transition orchestration belong to callers.
#![forbid(unsafe_code)]
mod integer;
pub mod repo;
