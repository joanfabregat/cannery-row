//! Application policy over schema positions, never over instance-valued keywords.
use super::ContractViolation;
use serde_json::Value;

const DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";
fn child(path: &str, name: &str) -> String {
    format!("{path}/{}", name.replace('~', "~0").replace('/', "~1"))
}
pub(super) fn errors(root: &Value) -> Vec<ContractViolation> {
    if !root.is_object() {
        return vec![ContractViolation {
            path: String::new(),
            message: "project schema must be an object".into(),
        }];
    }
    let mut errors = Vec::new();
    let mut pending = vec![(root, String::new())];
    while let Some((schema, path)) = pending.pop() {
        let Some(object) = schema.as_object() else {
            continue;
        };
        for (name, value) in object {
            let location = child(&path, name);
            let message = match name.as_str() {
                "$schema" if value.as_str() != Some(DRAFT) => {
                    Some("unsupported JSON Schema dialect")
                }
                "$ref" | "$dynamicRef"
                    if !value
                        .as_str()
                        .is_some_and(|reference| reference.starts_with('#')) =>
                {
                    Some("schema references must be local")
                }
                "format"
                    if !value.as_str().is_some_and(|format| {
                        matches!(format, "date" | "date-time" | "uri" | "uuid")
                    }) =>
                {
                    Some("unsupported schema format")
                }
                _ => None,
            };
            if let Some(message) = message {
                errors.push(ContractViolation {
                    path: location.clone(),
                    message: message.into(),
                });
            }
            match name.as_str() {
                "$defs" | "definitions" | "properties" | "patternProperties"
                | "dependentSchemas" => {
                    if let Some(children) = value.as_object() {
                        pending.extend(
                            children
                                .iter()
                                .map(|(name, schema)| (schema, child(&location, name))),
                        );
                    }
                }
                "allOf" | "anyOf" | "oneOf" | "prefixItems" => {
                    if let Some(children) = value.as_array() {
                        pending.extend(
                            children.iter().enumerate().map(|(index, schema)| {
                                (schema, child(&location, &index.to_string()))
                            }),
                        );
                    }
                }
                "additionalProperties"
                | "unevaluatedProperties"
                | "propertyNames"
                | "contains"
                | "items"
                | "unevaluatedItems"
                | "not"
                | "if"
                | "then"
                | "else"
                | "contentSchema" => pending.push((value, location)),
                _ => {}
            }
        }
    }
    super::sort_errors(root, &mut errors);
    errors
}
