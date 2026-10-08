// SPDX-License-Identifier: AGPL-3.0-only
//! Strict YAML: the subset Cannery Row reads from import bundles and from the
//! front matter of phase documents. A duplicate key is an error, anchors,
//! aliases and merge keys are refused, only `true` and `false` are booleans,
//! dates stay text, and every scalar must survive as JSON: no NaN or infinity
//! (`serde_json` would turn them into null), no NUL character.
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use std::fmt;
struct Safe;
struct Scalars;
impl<'de> Deserialize<'de> for Safe {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(Scalars)
    }
}
impl<'de> Visitor<'de> for Scalars {
    type Value = Safe;
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("finite JSON-compatible scalars")
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Safe, E> {
        Ok(Safe)
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Safe, E> {
        Ok(Safe)
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Safe, E> {
        Ok(Safe)
    }
    fn visit_i128<E: de::Error>(self, _: i128) -> Result<Safe, E> {
        Ok(Safe)
    }
    fn visit_u128<E: de::Error>(self, _: u128) -> Result<Safe, E> {
        Ok(Safe)
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Safe, E> {
        if value.is_finite() {
            Ok(Safe)
        } else {
            Err(E::custom("nonfinite number"))
        }
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<Safe, E> {
        if value.contains('\0') {
            Err(E::custom("NUL scalar"))
        } else {
            Ok(Safe)
        }
    }
    fn visit_string<E: de::Error>(self, value: String) -> Result<Safe, E> {
        self.visit_str(&value)
    }
    fn visit_unit<E: de::Error>(self) -> Result<Safe, E> {
        Ok(Safe)
    }
    fn visit_none<E: de::Error>(self) -> Result<Safe, E> {
        Ok(Safe)
    }
    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Safe, D::Error> {
        Safe::deserialize(deserializer)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut values: A) -> Result<Safe, A::Error> {
        while values.next_element::<Safe>()?.is_some() {}
        Ok(Safe)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut values: A) -> Result<Safe, A::Error> {
        while let Some(key) = values.next_key::<String>()? {
            if key.contains('\0') {
                return Err(de::Error::custom("NUL key"));
            }
            values.next_value::<Safe>()?;
        }
        Ok(Safe)
    }
}

/// Options for one strict YAML document within the given parser budget.
#[must_use]
pub fn strict_options(max_depth: usize, max_nodes: usize) -> serde_saphyr::Options {
    serde_saphyr::options! {
        strict_booleans: true,
        with_snippet: false,
        merge_keys: serde_saphyr::options::MergeKeyPolicy::Error,
        duplicate_keys: serde_saphyr::options::DuplicateKeyPolicy::Error,
        alias_limits: serde_saphyr::alias_limits! {
            max_total_replayed_events: 0,
            max_replay_stack_depth: 0,
            max_alias_expansions_per_anchor: 0,
        },
        budget: serde_saphyr::budget! { max_depth: max_depth, max_nodes: max_nodes, max_documents: 1 },
    }
}

/// Check every scalar of a document against `options` without building it.
/// # Errors
/// Returns the parser's error for a nonfinite number, a NUL character, or any
/// document the options refuse.
pub fn validate_scalars(
    text: &str,
    options: serde_saphyr::Options,
) -> Result<(), serde_saphyr::Error> {
    serde_saphyr::from_str_with_options::<Safe>(text, options).map(|_| ())
}

/// Why a strict YAML document was refused. No value from the document is kept.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum YamlError {
    #[error("NUL characters are forbidden")]
    Nul,
    #[error("invalid, duplicate, nonfinite or resource-limited document")]
    Invalid,
    #[error("document resource limit exceeded")]
    Limit,
    #[error("number width limit exceeded")]
    NumberWidth,
}

/// Parse one strict YAML document into JSON, within a depth and node budget.
/// An empty document is JSON `null`.
/// # Errors
/// Refuses anything strict YAML refuses, and documents beyond the budget.
pub fn parse(text: &str, max_depth: usize, max_nodes: usize) -> Result<Value, YamlError> {
    if text.contains('\0') {
        return Err(YamlError::Nul);
    }
    let options = strict_options(max_depth, max_nodes);
    validate_scalars(text, options.clone()).map_err(|_| YamlError::Invalid)?;
    let value: Value =
        serde_saphyr::from_str_with_options(text, options).map_err(|_| YamlError::Invalid)?;
    let mut nodes = 0usize;
    let mut stack = vec![(&value, 0usize)];
    while let Some((value, depth)) = stack.pop() {
        nodes += 1;
        if depth > max_depth || nodes > max_nodes {
            return Err(YamlError::Limit);
        }
        match value {
            Value::Array(items) => stack.extend(items.iter().map(|v| (v, depth + 1))),
            Value::Object(fields) => stack.extend(fields.values().map(|v| (v, depth + 1))),
            Value::Number(number) if number.to_string().len() > 1024 => {
                return Err(YamlError::NumberWidth);
            }
            _ => {}
        }
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_yaml_refuses_what_json_cannot_hold() {
        // A nonfinite number never becomes JSON null: it is refused or kept
        // as the text written.
        for text in ["a: .nan\n", "a: .inf\n", "a: -.inf\n"] {
            let parsed = parse(text, 8, 64);
            assert!(
                parsed.is_err() || parsed.as_ref().is_ok_and(|v| v["a"].is_string()),
                "{text}: {parsed:?}"
            );
        }
        assert_eq!(parse("a: 1\na: 2\n", 8, 64), Err(YamlError::Invalid));
        assert_eq!(parse("a: &x 1\nb: *x\n", 8, 64), Err(YamlError::Invalid));
        assert_eq!(parse("a: 1\0", 8, 64), Err(YamlError::Nul));
        assert!(parse("a: [[[[1]]]]\n", 2, 64).is_err());
        assert!(parse("a: [1, 2, 3, 4, 5]\n", 8, 3).is_err());
    }

    #[test]
    fn strict_yaml_keeps_strings_as_written() -> Result<(), YamlError> {
        let value = parse("on: no\nday: 2024-01-02\nn: 1.5\nempty:\n", 8, 64)?;
        assert_eq!(
            value,
            serde_json::json!({"on": "no", "day": "2024-01-02", "n": 1.5, "empty": null})
        );
        assert_eq!(parse("", 8, 64)?, Value::Null);
        Ok(())
    }
}
