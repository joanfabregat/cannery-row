//! Measurement reads; callers own connections, transactions and authorization.
#![forbid(unsafe_code)]
#[cfg(test)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;

pub mod aggregation;
pub mod filters;
pub mod numeric;
pub mod projection;
pub mod repo;
pub mod series;
pub mod views;
