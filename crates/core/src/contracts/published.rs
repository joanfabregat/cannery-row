// SPDX-License-Identifier: AGPL-3.0-only
//! Every published contract schema by name, in the two forms the server hands
//! out: bundled, as one self-contained document whose referenced schemas are
//! embedded under `$defs` (`GET /api/schemas/{name}`), and inlined, with every
//! reference replaced by the schema it names (the MCP tools' input schemas,
//! which clients show as is).
use super::{ContractError, PUBLISHED_SOURCES, formats};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::LazyLock,
};

static SOURCES: LazyLock<BTreeMap<&'static str, Value>> = LazyLock::new(|| {
    PUBLISHED_SOURCES
        .iter()
        .filter_map(|(name, source)| Some((*name, serde_json::from_str(source).ok()?)))
        .collect()
});

/// The published schema names, which are also their file name stems, sorted.
#[must_use]
pub fn names() -> Vec<&'static str> {
    let mut names: Vec<_> = PUBLISHED_SOURCES.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    names
}

/// The validation options of the published schemas: Draft 2020-12, offline,
/// with their `date` and `date-time` formats.
#[must_use]
pub fn options<'a>() -> jsonschema::ValidationOptions<'a> {
    formats::options()
}

fn source(name: &str) -> Result<&'static Value, ContractError> {
    SOURCES.get(name).ok_or(ContractError::Schema)
}

/// The published schema `name` with every schema it references embedded
/// under `$defs`, so it validates on its own with any Draft 2020-12 validator.
/// # Errors
/// Rejects an unknown name or a schema that cannot be bundled.
pub fn bundled(name: &str) -> Result<Value, ContractError> {
    let mut document = source(name)?.clone();
    let mut embedded = BTreeSet::new();
    let mut pending = references(&document);
    while let Some(reference) = pending.pop() {
        if reference != name && embedded.insert(reference.clone()) {
            pending.extend(references(source(&reference)?));
        }
    }
    let definitions = document
        .as_object_mut()
        .ok_or(ContractError::Schema)?
        .entry("$defs")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or(ContractError::Schema)?;
    for reference in embedded {
        let key = format!("{reference}.schema.json");
        if definitions.contains_key(&key) {
            return Err(ContractError::Schema);
        }
        definitions.insert(key, source(&reference)?.clone());
    }
    Ok(document)
}

/// The published schemas a schema references, by file name stem.
fn references(schema: &Value) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![schema];
    while let Some(value) = pending.pop() {
        match value {
            Value::Object(fields) => {
                if let Some(Value::String(reference)) = fields.get("$ref") {
                    let resource = reference.split('#').next().unwrap_or_default();
                    if let Some(stem) = resource.strip_suffix(".schema.json") {
                        found.push(stem.to_owned());
                    }
                }
                pending.extend(fields.values());
            }
            Value::Array(items) => pending.extend(items),
            _ => {}
        }
    }
    found
}

/// Keywords that only annotate a schema; beside a `$ref` they are merged
/// into the schema it names rather than combined with it.
const ANNOTATIONS: [&str; 5] = ["description", "title", "default", "examples", "$comment"];
/// Keywords that identify or hold a document, dropped once it is inlined.
const DOCUMENT_KEYWORDS: [&str; 4] = ["$schema", "$id", "$comment", "$defs"];
/// Nesting no published schema reaches; guards against a reference cycle.
const MAX_DEPTH: usize = 64;

/// The part of the published schema `name` at the JSON Pointer `pointer`
/// (empty for the whole schema), with every reference replaced by the schema
/// it names, recursively, so it needs no `$defs` nor any other document.
/// # Errors
/// Rejects an unknown name or pointer, or a reference that cannot be inlined.
pub fn inlined(name: &str, pointer: &str) -> Result<Value, ContractError> {
    let value = source(name)?
        .pointer(pointer)
        .ok_or(ContractError::Schema)?;
    inline(name, value, 0)
}

fn inline(document: &str, value: &Value, depth: usize) -> Result<Value, ContractError> {
    if depth > MAX_DEPTH {
        return Err(ContractError::Schema);
    }
    match value {
        Value::Array(items) => Ok(Value::Array(
            items
                .iter()
                .map(|item| inline(document, item, depth + 1))
                .collect::<Result<_, _>>()?,
        )),
        Value::Object(fields) => {
            let mut siblings = Map::new();
            for (key, value) in fields {
                if key != "$ref" && !DOCUMENT_KEYWORDS.contains(&key.as_str()) {
                    siblings.insert(key.clone(), inline(document, value, depth + 1)?);
                }
            }
            let Some(reference) = fields.get("$ref") else {
                return Ok(Value::Object(siblings));
            };
            let reference = reference.as_str().ok_or(ContractError::Schema)?;
            let (resource, pointer) = reference.split_once('#').unwrap_or((reference, ""));
            let target_document = if resource.is_empty() {
                document
            } else {
                resource
                    .strip_suffix(".schema.json")
                    .ok_or(ContractError::Schema)?
            };
            let target = source(target_document)?
                .pointer(pointer)
                .ok_or(ContractError::Schema)?;
            let resolved = inline(target_document, target, depth + 1)?;
            if siblings
                .keys()
                .all(|key| ANNOTATIONS.contains(&key.as_str()))
            {
                let Value::Object(mut merged) = resolved else {
                    return Ok(resolved);
                };
                merged.extend(siblings);
                Ok(Value::Object(merged))
            } else {
                let mut combined = Map::new();
                combined.insert("allOf".into(), Value::Array(vec![resolved]));
                combined.extend(siblings);
                Ok(Value::Object(combined))
            }
        }
        other => Ok(other.clone()),
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "Assertions on the static published schemas"
)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_published_schema_bundles_and_inlines() {
        assert_eq!(names().len(), PUBLISHED_SOURCES.len());
        for name in names() {
            let bundled = bundled(name).expect("bundled");
            jsonschema::options()
                .with_draft(jsonschema::Draft::Draft202012)
                .offline()
                .build(&bundled)
                .expect("self-contained");
            let inlined = inlined(name, "").expect("inlined");
            assert!(
                !serde_json::to_string(&inlined)
                    .expect("text")
                    .contains("\"$ref\""),
                "{name}"
            );
            options().build(&inlined).expect("compiles");
        }
        assert!(bundled("plan").is_err());
    }

    #[test]
    fn inlined_schemas_validate_like_the_published_ones() {
        let acceptance = inlined("unit", "/properties/acceptance").expect("acceptance");
        assert_eq!(
            acceptance["properties"]["primary_metric"]["pattern"],
            "^[a-z][a-z0-9_]{0,63}$"
        );
        assert!(
            acceptance["required"]
                .as_array()
                .expect("required")
                .contains(&json!("compute_budget"))
        );
        let manifest = inlined("artifact_manifest", "").expect("manifest");
        let validator = options().build(&manifest).expect("manifest compiles");
        let object = json!({"role": "candidate", "storage": {"backend": "local", "bucket": "b", "key": "k"}, "size_bytes": 1, "sha256": "a".repeat(64), "media_type": "text/plain"});
        assert!(
            validator.is_valid(
                &json!({"schema_version": "0.2", "attempt_id": "x", "objects": [object]})
            )
        );
        assert!(!validator.is_valid(
            &json!({"schema_version": "0.2", "attempt_id": "x", "objects": [{"role": "candidate"}]})
        ));
        // A reference beside other keywords keeps both.
        let run = inlined("run", "/properties/claims/items").expect("claims");
        assert!(run["allOf"][0]["properties"]["metric"].is_object());
        assert_eq!(run["properties"]["authority"]["const"], "agent_claim");
    }
}
