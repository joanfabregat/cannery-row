//! Shared configuration, database and domain foundations.
#![forbid(unsafe_code)]

pub mod audit;
pub mod configuration_toml;
pub mod contracts;
pub mod db;
pub mod errors;
pub mod front_matter;
pub mod ids;
pub mod json;
pub mod pagination;
pub mod pg_integer;
pub mod principal;
pub mod settings;
pub mod sorting;
pub mod text;
pub mod timestamps;
pub mod unicode;
pub mod yaml;
