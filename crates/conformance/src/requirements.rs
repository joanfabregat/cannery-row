//! Source-derived coverage requirements, checked against the frozen API.
use crate::{Coverage, OPENAPI, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Deserialize)]
pub struct Requirements {
    operations: Vec<Operation>,
    tools: BTreeSet<String>,
    audit_actions: BTreeSet<String>,
    #[serde(default)]
    extra_operations: Vec<ExtraOperation>,
    #[serde(default, rename = "opaque_response_requirements")]
    opaque_responses: BTreeMap<String, BTreeSet<u16>>,
}

#[derive(Deserialize)]
struct Operation {
    method: String,
    path: String,
    required_statuses: BTreeSet<u16>,
}

#[derive(Deserialize)]
struct ExtraOperation {
    method: String,
    path: String,
    #[serde(default)]
    success_status: Option<u16>,
    #[serde(default)]
    success_statuses: BTreeSet<u16>,
    #[serde(default)]
    required_statuses: BTreeSet<u16>,
}

impl Requirements {
    /// Check tool and action inventories against the live Python source export.
    ///
    /// # Errors
    /// Returns an error for malformed references or missing/extra names.
    pub fn check_reference(&self, reference: &serde_json::Value) -> Result<()> {
        fn names(value: &serde_json::Value, field: &str) -> Result<BTreeSet<String>> {
            let items = value
                .as_array()
                .ok_or("reference inventory is not an array")?;
            let mut names = BTreeSet::new();
            for item in items {
                let name = item[field]
                    .as_str()
                    .ok_or("reference inventory name missing")?;
                if !names.insert(name.to_owned()) {
                    return Err(format!("duplicate reference name {name}").into());
                }
            }
            Ok(names)
        }
        if names(&reference["mcp"]["tools"], "name")? != self.tools {
            return Err("tool coverage inventory differs from live Python definitions".into());
        }
        if names(&reference["audit"], "action")? != self.audit_actions {
            return Err("audit coverage inventory differs from live Python producers".into());
        }
        let digest = ring::digest::digest(&ring::digest::SHA256, OPENAPI.as_bytes());
        let hash = digest
            .as_ref()
            .iter()
            .flat_map(|byte| {
                [
                    char::from(HEX[usize::from(byte >> 4)]),
                    char::from(HEX[usize::from(byte & 15)]),
                ]
            })
            .collect::<String>();
        if reference["openapi_sha256"].as_str() != Some(hash.as_str()) {
            return Err("reference exporter used a different frozen OpenAPI document".into());
        }
        Ok(())
    }

    /// Refuse incomplete/stale inventories before assessing observed coverage.
    ///
    /// # Errors
    /// Returns an error for missing, duplicate, unknown, or invalid operations.
    pub fn coverage(self) -> Result<Coverage> {
        let frozen: serde_json::Value = serde_json::from_str(OPENAPI)?;
        let paths = frozen["paths"].as_object().ok_or("frozen paths missing")?;
        let mut coverage = Coverage {
            tools: self.tools,
            audit_actions: self.audit_actions,
            ..Coverage::default()
        };
        for operation in self.operations {
            let method = operation.method.to_lowercase();
            let key = format!("{} {}", operation.method.to_uppercase(), operation.path);
            let frozen_operation = &frozen["paths"][&operation.path][&method];
            if frozen_operation.is_null() || operation.required_statuses.is_empty() {
                return Err(format!("unknown or empty requirements operation {key}").into());
            }
            if let Some(responses) = frozen_operation["responses"].as_object() {
                for status in responses
                    .keys()
                    .filter_map(|value| value.parse::<u16>().ok())
                {
                    // FastAPI adds unreachable 422 responses for unconstrained
                    // string path parameters. Error reachability comes from the
                    // reviewed handler inventory rather than that placeholder.
                    if status < 300 && !operation.required_statuses.contains(&status) {
                        return Err(format!("inventory omits documented {key}: {status}").into());
                    }
                }
            }
            if coverage
                .operations
                .insert(key.clone(), operation.required_statuses)
                .is_some()
            {
                return Err(format!("duplicate inventory operation {key}").into());
            }
        }
        for (path, methods) in paths {
            for method in methods
                .as_object()
                .ok_or("frozen path object missing")?
                .keys()
            {
                if ["get", "post", "put", "patch", "delete", "head", "options"]
                    .contains(&method.as_str())
                {
                    let key = format!("{} {path}", method.to_uppercase());
                    if !coverage.operations.contains_key(&key) {
                        return Err(format!("inventory omits frozen operation {key}").into());
                    }
                }
            }
        }
        for operation in self.extra_operations {
            let key = format!("{} {}", operation.method.to_uppercase(), operation.path);
            let mut statuses = operation.required_statuses;
            statuses.extend(operation.success_statuses);
            statuses.extend(operation.success_status);
            if statuses.is_empty() || coverage.operations.insert(key.clone(), statuses).is_some() {
                return Err(format!("empty or duplicate extra operation {key}").into());
            }
        }
        for (key, statuses) in self.opaque_responses {
            coverage
                .operations
                .get_mut(&key)
                .ok_or_else(|| format!("opaque response references unknown operation {key}"))?
                .extend(statuses);
        }
        Ok(coverage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn inventory() -> Result<Value> {
        Ok(serde_json::from_str(include_str!(
            "../fixtures/coverage-requirements.json"
        ))?)
    }

    #[test]
    fn missing_duplicate_and_unknown_operations_fail_closed() -> Result<()> {
        let baseline = inventory()?;
        serde_json::from_value::<Requirements>(baseline.clone())?.coverage()?;
        let mut missing = baseline.clone();
        missing["operations"]
            .as_array_mut()
            .ok_or("operations absent")?
            .pop();
        assert!(
            serde_json::from_value::<Requirements>(missing)?
                .coverage()
                .is_err()
        );
        let mut duplicate = baseline.clone();
        let first = duplicate["operations"][0].clone();
        duplicate["operations"]
            .as_array_mut()
            .ok_or("operations absent")?
            .push(first);
        assert!(
            serde_json::from_value::<Requirements>(duplicate)?
                .coverage()
                .is_err()
        );
        let mut unknown = baseline;
        unknown["operations"][0]["path"] = json!("/api/imaginary");
        assert!(
            serde_json::from_value::<Requirements>(unknown)?
                .coverage()
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn empty_or_missing_documented_statuses_fail_closed() -> Result<()> {
        let baseline = inventory()?;
        for statuses in [json!([]), json!([401, 403])] {
            let mut changed = baseline.clone();
            changed["operations"][0]["required_statuses"] = statuses;
            assert!(
                serde_json::from_value::<Requirements>(changed)?
                    .coverage()
                    .is_err()
            );
        }
        Ok(())
    }
}
