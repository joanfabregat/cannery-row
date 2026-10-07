//! Configuration input types backed by the maintained TOML parser.
//! Datetimes retain their type only; configuration consumers reject that type.
use std::collections::BTreeMap;

use num_bigint::BigInt;

#[derive(Clone)]
pub enum Input {
    String(String),
    Integer(BigInt),
    Float(f64),
    Boolean(bool),
    Array(Vec<Self>),
    Table(BTreeMap<String, Self>),
    Datetime,
}

pub type Table = BTreeMap<String, Input>;

/// Value-free configuration parse failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InvalidToml {
    #[error("invalid configuration TOML")]
    Syntax,
}

/// Parse configuration TOML using the maintained TOML library and signed 64-bit integers.
/// # Errors
/// Returns a value-free parse failure.
pub fn document(source: &str) -> Result<Table, InvalidToml> {
    let parsed: toml::Table = toml::from_str(source).map_err(|_| InvalidToml::Syntax)?;
    Ok(parsed
        .into_iter()
        .map(|(key, value)| (key, convert(value)))
        .collect())
}
fn convert(value: toml::Value) -> Input {
    match value {
        toml::Value::String(value) => Input::String(value),
        toml::Value::Integer(value) => Input::Integer(BigInt::from(value)),
        toml::Value::Float(value) => Input::Float(value),
        toml::Value::Boolean(value) => Input::Boolean(value),
        toml::Value::Datetime(_) => Input::Datetime,
        toml::Value::Array(values) => Input::Array(values.into_iter().map(convert).collect()),
        toml::Value::Table(values) => Input::Table(
            values
                .into_iter()
                .map(|(key, value)| (key, convert(value)))
                .collect(),
        ),
    }
}
