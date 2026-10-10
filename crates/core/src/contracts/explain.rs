// SPDX-License-Identifier: AGPL-3.0-only
//! Validation errors in words: where a document breaks a schema and what the
//! schema expected there, quoting the schema and property names but never a
//! value of the document.
use jsonschema::{
    ValidationError,
    error::{TypeKind, ValidationErrorKind as Kind},
};
use serde_json::Value;

/// Where a validation error is best reported, as a JSON Pointer, and what
/// the schema expected there.
///
/// A missing or unexpected property is reported at the object that should
/// or should not hold it, naming the property. An `anyOf` or `oneOf` error
/// is reported at the deepest place any of its branches failed, which is
/// usually the branch the document meant; when several branches fail there,
/// each one's expectation is listed.
#[must_use]
pub fn explain(error: &ValidationError<'_>) -> (String, String) {
    explain_in(error, None)
}

/// As [`explain`], with the schema the error comes from, when its keyword
/// locations can be looked up in it (a schema without `$ref`): an unexpected
/// property then also says which names the schema allows there.
#[must_use]
pub fn explain_in(error: &ValidationError<'_>, schema: Option<&Value>) -> (String, String) {
    let depth = |path: &str| path.bytes().filter(|byte| *byte == b'/').count();
    let mut best = error.instance_path().to_string();
    // Each leaf with the branch of the outermost anyOf or oneOf it fails in.
    let mut leaves: Vec<(Option<usize>, String, String)> = Vec::new();
    let mut pending = vec![(None, error)];
    while let Some((branch, error)) = pending.pop() {
        let path = error.instance_path().to_string();
        if depth(&path) > depth(&best) {
            best.clone_from(&path);
        }
        match error.kind() {
            Kind::AnyOf { context } | Kind::OneOfNotValid { context } => {
                for (index, errors) in context.iter().enumerate().rev() {
                    let branch = branch.or(Some(index));
                    pending.extend(errors.iter().rev().map(|error| (branch, error)));
                }
            }
            _ => leaves.push((branch, path, describe_in(error, schema))),
        }
    }
    // The expectations at the reported place, grouped by branch in order.
    let mut groups: Vec<(Option<usize>, Vec<String>)> = Vec::new();
    for (branch, path, message) in leaves {
        if path != best {
            continue;
        }
        match groups.iter_mut().find(|(group, _)| *group == branch) {
            Some((_, messages)) if messages.contains(&message) => {}
            Some((_, messages)) => messages.push(message),
            None => groups.push((branch, vec![message])),
        }
    }
    groups.sort_by_key(|(branch, _)| *branch);
    let message = match groups.as_slice() {
        [] => describe_in(error, schema),
        [(_, messages)] => merge(messages.clone()),
        many => format!(
            "matches none of the allowed forms: {}",
            many.iter()
                .enumerate()
                .map(|(index, (branch, messages))| format!(
                    "({}) {}",
                    branch.unwrap_or(index) + 1,
                    merge(messages.clone())
                ))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    };
    (best, message)
}

/// Several expectations at one place as one message: the missing
/// properties named together, then the others, then the alternatives.
#[must_use]
pub fn merge(messages: Vec<String>) -> String {
    const MISSING: &str = "missing required property ";
    let mut missing = Vec::new();
    let mut plain = Vec::new();
    let mut forms = Vec::new();
    for message in messages {
        if let Some(name) = message.strip_prefix(MISSING) {
            missing.push(name.to_owned());
        } else if message.starts_with("matches none of the allowed forms") {
            forms.push(message);
        } else {
            plain.push(message);
        }
    }
    let mut parts = Vec::new();
    match missing.as_slice() {
        [] => {}
        [one] => parts.push(format!("{MISSING}{one}")),
        many => parts.push(format!("missing required properties {}", many.join(", "))),
    }
    parts.extend(plain);
    parts.extend(forms);
    parts.join("; ")
}

/// A name quoted from a document or a schema, bounded and printable.
fn quoted(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .take(64)
        .collect();
    format!("\"{name}\"")
}

/// A value of the schema quoted in a message, bounded.
fn schema_value(value: &Value) -> String {
    let text = value.to_string();
    if text.len() <= 400 {
        return text;
    }
    let mut end = 400;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn plural(count: u64, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

fn article(kind: &str) -> String {
    match kind {
        "array" | "integer" | "object" => format!("an {kind}"),
        "null" => String::from("null"),
        _ => format!("a {kind}"),
    }
}

/// The property names the schema object holding a failed `additionalProperties`
/// or `unevaluatedProperties` allows, when its location resolves in `schema`.
fn allowed(error: &ValidationError<'_>, schema: &Value) -> Option<String> {
    let location = error.schema_path().as_str();
    let (parent, _) = location.rsplit_once('/')?;
    let object = schema.pointer(parent)?;
    let mut parts = Vec::new();
    if let Some(names) = object.get("properties").and_then(Value::as_object)
        && !names.is_empty()
    {
        let mut listed: Vec<_> = names.keys().take(30).map(|name| quoted(name)).collect();
        if names.len() > listed.len() {
            listed.push(String::from("…"));
        }
        parts.push(listed.join(", "));
    }
    if let Some(patterns) = object.get("patternProperties").and_then(Value::as_object)
        && !patterns.is_empty()
    {
        parts.push(format!(
            "names matching {}",
            patterns
                .keys()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join(" or ")
        ));
    }
    Some(if parts.is_empty() {
        String::from("the schema allows no property here")
    } else {
        format!("the schema allows only {}", parts.join(" and "))
    })
}

/// What one validation error expected, in words.
#[must_use]
pub fn describe(error: &ValidationError<'_>) -> String {
    describe_in(error, None)
}

/// As [`describe`], with the schema the error comes from (see [`explain_in`]).
#[must_use]
pub fn describe_in(error: &ValidationError<'_>, schema: Option<&Value>) -> String {
    match error.kind() {
        Kind::Required { property } => format!(
            "missing required property {}",
            property
                .as_str()
                .map_or_else(|| schema_value(property), quoted)
        ),
        Kind::AdditionalProperties { unexpected } | Kind::UnevaluatedProperties { unexpected } => {
            let names: Vec<_> = unexpected
                .iter()
                .take(20)
                .map(|name| quoted(name))
                .collect();
            format!(
                "unexpected {} {}: {}",
                if unexpected.len() == 1 {
                    "property"
                } else {
                    "properties"
                },
                names.join(", "),
                schema
                    .and_then(|schema| allowed(error, schema))
                    .unwrap_or_else(|| format!(
                        "the schema does not allow {}",
                        if unexpected.len() == 1 { "it" } else { "them" }
                    ))
            )
        }
        Kind::Type { kind } => match kind {
            TypeKind::Single(kind) => format!("expected {}", article(&kind.to_string())),
            TypeKind::Multiple(kinds) => format!(
                "expected one of the types {}",
                kinds
                    .iter()
                    .map(|kind| kind.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        },
        Kind::Enum { options } => format!(
            "expected one of {}",
            options.as_array().map_or_else(
                || schema_value(options),
                |options| options
                    .iter()
                    .map(schema_value)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        ),
        Kind::Constant { expected_value } => {
            format!("expected exactly {}", schema_value(expected_value))
        }
        Kind::Pattern { pattern } => format!("does not match the pattern {pattern}"),
        Kind::Format { format } => format!("is not a valid {format}"),
        Kind::MinLength { limit } => {
            format!(
                "is shorter than {}",
                plural(*limit, "character", "characters")
            )
        }
        Kind::MaxLength { limit } => {
            format!(
                "is longer than {}",
                plural(*limit, "character", "characters")
            )
        }
        Kind::Minimum { limit } => format!("is less than the minimum {limit}"),
        Kind::Maximum { limit } => format!("is greater than the maximum {limit}"),
        Kind::ExclusiveMinimum { limit } => format!("must be greater than {limit}"),
        Kind::ExclusiveMaximum { limit } => format!("must be less than {limit}"),
        Kind::MultipleOf { multiple_of } => format!("is not a multiple of {multiple_of}"),
        Kind::MinItems { limit } => format!("has fewer than {}", plural(*limit, "item", "items")),
        Kind::MaxItems { limit } => format!("has more than {}", plural(*limit, "item", "items")),
        Kind::AdditionalItems { limit } => format!("has more than {limit} items"),
        Kind::UniqueItems => String::from("has duplicate items"),
        Kind::MinProperties { limit } => format!(
            "has fewer than {}",
            plural(*limit, "property", "properties")
        ),
        Kind::MaxProperties { limit } => {
            format!("has more than {}", plural(*limit, "property", "properties"))
        }
        Kind::PropertyNames { error } => format!("a property name {}", describe_in(error, schema)),
        Kind::Contains => String::from("has no item of the required form"),
        Kind::FalseSchema => String::from("is not allowed here"),
        Kind::Not { .. } => String::from("matches a form that is not allowed here"),
        Kind::OneOfMultipleValid { .. } => {
            String::from("matches more than one of the allowed forms")
        }
        Kind::AnyOf { .. } | Kind::OneOfNotValid { .. } => {
            String::from("matches none of the allowed forms")
        }
        _ => String::from("value does not satisfy the schema"),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "Assertions on static test schemas")]
mod tests {
    use super::*;
    use serde_json::json;

    fn explained(schema: &Value, value: &Value) -> Vec<(String, String)> {
        let validator = jsonschema::validator_for(schema).expect("schema");
        validator.iter_errors(value).map(|e| explain(&e)).collect()
    }

    #[test]
    fn messages_name_what_was_expected() {
        let schema = json!({
            "type": "object",
            "properties": {
                "budget": {
                    "type": "object",
                    "minProperties": 1,
                    "patternProperties": {"^[a-z]+_max$": {"type": "number"}},
                    "additionalProperties": false
                },
                "kind": {"enum": ["a", "b"]},
                "slug": {"type": "string", "pattern": "^[a-z]+$"},
                "unit": {"oneOf": [{"type": "integer"}, {"type": "string"}]}
            },
            "required": ["budget", "kind"],
            "additionalProperties": false
        });
        assert_eq!(
            explained(&schema, &json!({"kind": "a"})),
            vec![(String::new(), "missing required property \"budget\"".into())]
        );
        let errors = explained(
            &schema,
            &json!({"budget": {"gpu_hours": 3, "secret": "x"}, "kind": "c", "slug": "A", "unit": 1.5, "extra": 1}),
        );
        assert!(errors.contains(&(
            String::new(),
            "unexpected property \"extra\": the schema does not allow it".into()
        )));
        assert!(
            errors.contains(&(
                "/budget".into(),
                "unexpected properties \"gpu_hours\", \"secret\": the schema does not allow them"
                    .into()
            ))
        );
        assert!(errors.contains(&("/kind".into(), "expected one of \"a\", \"b\"".into())));
        assert!(errors.contains(&("/slug".into(), "does not match the pattern ^[a-z]+$".into())));
        assert!(
            errors.contains(&(
                "/unit".into(),
                "matches none of the allowed forms: (1) expected an integer; (2) expected a string"
                    .into()
            ))
        );
        assert!(
            errors.iter().all(|(_, message)| !message.contains("\"x\"")),
            "values never appear: {errors:?}"
        );
        // With the schema at hand, an unexpected property says what is allowed.
        let validator = jsonschema::validator_for(&schema).expect("schema");
        let value = json!({"budget": {"gpu_hours": 3}, "kind": "a", "extra": 1});
        let errors: Vec<_> = validator
            .iter_errors(&value)
            .map(|error| explain_in(&error, Some(&schema)))
            .collect();
        assert!(errors.contains(&(
            "/budget".into(),
            "unexpected property \"gpu_hours\": the schema allows only names matching ^[a-z]+_max$"
                .into()
        )));
        assert!(errors.contains(&(
            String::new(),
            "unexpected property \"extra\": the schema allows only \"budget\", \"kind\", \"slug\", \"unit\""
                .into()
        )));
    }
}
