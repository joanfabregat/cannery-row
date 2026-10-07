//! Lossless source release body model. Values never enter diagnostics.
use crate::validation::{self, BodyInput, Location, Problem, ValidationErrors};
use cannery_core::json::{Document, Node};
use num_bigint::BigInt;

pub(crate) const CODES: &[&str] = &[
    "step_failed",
    "deadline_exceeded",
    "runner_error",
    "setup_failed",
    "invalid_step_output",
    "invalid_output",
    "missing_output",
    "invalid_code",
    "code_not_allowed",
    "invalid_input",
    "missing_input",
    "input_verification_failed",
    "upload_expired",
    "invalid_job",
    "held_out_labels_to_experiment",
];
pub(crate) struct LogRef {
    pub key: String,
    pub size_bytes: BigInt,
    pub sha256: String,
}
pub(crate) struct Release {
    pub reason: String,
    pub code: Option<&'static str>,
    pub step: Option<String>,
    pub logs: Vec<LogRef>,
}
fn location(parts: &[Location]) -> Vec<Location> {
    let mut loc = vec![Location::Field(String::from("body"))];
    loc.extend_from_slice(parts);
    loc
}
fn field(name: &str) -> Location {
    Location::Field(String::from(name))
}
fn problem(
    errors: &mut Vec<Problem>,
    path: &[Location],
    kind: &'static str,
    message: &'static str,
) {
    errors.push(Problem {
        loc: location(path),
        kind,
        message,
    });
}
fn white_space(point: u32) -> bool {
    matches!(point,
        9..=13
        | 32
        | 0x85
        | 0xa0
        | 0x1680
        | 0x2000..=0x200a
        | 0x2028
        | 0x2029
        | 0x202f
        | 0x205f
        | 0x3000
    )
}
fn string(
    value: Option<&Node>,
    path: &[Location],
    errors: &mut Vec<Problem>,
    trim: bool,
    min: usize,
    max: usize,
) -> Option<String> {
    let text = match value {
        None => {
            problem(errors, path, "missing", "Field required");
            return None;
        }
        Some(Node::String(text)) => text,
        _ => {
            problem(
                errors,
                path,
                "string_type",
                "Input should be a valid string",
            );
            return None;
        }
    };
    let Some(value) = text.as_utf8() else {
        problem(
            errors,
            path,
            "string_unicode",
            "Input should be a valid string, unable to parse raw data as a unicode string",
        );
        return None;
    };
    let value = if trim {
        value.trim_matches(|c| white_space(u32::from(c)))
    } else {
        &value
    };
    let len = value.chars().count();
    if len < min {
        problem(errors, path, "string_too_short", "String is too short");
        return None;
    }
    if len > max {
        problem(errors, path, "string_too_long", "String is too long");
        return None;
    }
    Some(value.to_owned())
}
fn extras(
    d: &Document,
    id: cannery_core::json::NodeId,
    allowed: &[&str],
    path: &[Location],
    errors: &mut Vec<Problem>,
) {
    if let Some(Node::Object(fields)) = d.node(id) {
        for (key, _) in fields {
            if !allowed.iter().any(|name| key.equals_utf8(name)) {
                let mut p = path.to_vec();
                p.push(Location::Field(key.clone()));
                problem(
                    errors,
                    &p,
                    "extra_forbidden",
                    "Extra inputs are not permitted",
                );
            }
        }
    }
}
#[allow(
    clippy::too_many_lines,
    reason = "Source model field and nested item validation order is observable"
)]
pub(crate) fn release(input: BodyInput<'_>) -> Result<Release, ValidationErrors> {
    let mut errors = vec![];
    let d = match input {
        BodyInput::Json(d) if !matches!(d.node(d.root()), Some(Node::Null)) => d,
        BodyInput::Missing | BodyInput::Json(_) => {
            problem(&mut errors, &[], "missing", "Field required");
            return Err(ValidationErrors::from_problems(errors));
        }
        BodyInput::RawBytes => {
            problem(
                &mut errors,
                &[],
                "model_attributes_type",
                "Input should be a valid dictionary or object to extract fields from",
            );
            return Err(ValidationErrors::from_problems(errors));
        }
    };
    if !matches!(d.node(d.root()), Some(Node::Object(_))) {
        problem(
            &mut errors,
            &[],
            "model_attributes_type",
            "Input should be a valid dictionary or object to extract fields from",
        );
        return Err(ValidationErrors::from_problems(errors));
    }
    let get = |key| d.field(d.root(), key).and_then(|id| d.node(id));
    let reason = string(
        get("reason"),
        &[field("reason")],
        &mut errors,
        true,
        1,
        4000,
    );
    let code = match get("code") {
        None | Some(Node::Null) => None,
        Some(Node::String(text)) => {
            let code = CODES.iter().copied().find(|code| text.equals_utf8(code));
            if code.is_none() {
                problem(
                    &mut errors,
                    &[field("code")],
                    "literal_error",
                    "Invalid runner failure code",
                );
            }
            code
        }
        _ => {
            problem(
                &mut errors,
                &[field("code")],
                "literal_error",
                "Invalid runner failure code",
            );
            None
        }
    };
    let step = match get("step") {
        None | Some(Node::Null) => None,
        value => {
            let value = string(value, &[field("step")], &mut errors, false, 0, usize::MAX);
            value.filter(|value| {
                let bytes = value.as_bytes();
                let valid = (1..=63).contains(&bytes.len()) && bytes[0].is_ascii_lowercase()
                    || (1..=63).contains(&bytes.len()) && bytes[0].is_ascii_digit();
                let valid = valid
                    && bytes
                        .iter()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-');
                if !valid {
                    problem(
                        &mut errors,
                        &[field("step")],
                        "string_pattern_mismatch",
                        "Invalid step name",
                    );
                }
                valid
            })
        }
    };
    let mut logs = vec![];
    match get("logs") {
        None => {}
        Some(Node::Array(items)) if items.len() > 64 => problem(
            &mut errors,
            &[field("logs")],
            "too_long",
            "List should have at most 64 items after validation",
        ),
        Some(Node::Array(items)) => {
            for (index, id) in items.iter().copied().enumerate() {
                let path = vec![field("logs"), Location::Index(index)];
                if !matches!(d.node(id), Some(Node::Object(_))) {
                    problem(
                        &mut errors,
                        &path,
                        "model_type",
                        "Input should be a valid dictionary or instance of LogRef",
                    );
                    continue;
                }
                let get = |key| d.field(id, key).and_then(|id| d.node(id));
                let mut p = path.clone();
                p.push(field("key"));
                let key = string(get("key"), &p, &mut errors, false, 1, 1024);
                p.pop();
                p.push(field("size_bytes"));
                let size = match get("size_bytes") {
                    None => {
                        problem(&mut errors, &p, "missing", "Field required");
                        None
                    }
                    Some(value) => match validation::model_integer_at(value, location(&p)) {
                        Err(e) => {
                            errors.extend(e.problems().iter().cloned());
                            None
                        }
                        Ok(n) if n < BigInt::from(0) => {
                            problem(
                                &mut errors,
                                &p,
                                "greater_than_equal",
                                "Input should be greater than or equal to 0",
                            );
                            None
                        }
                        Ok(n) => Some(n),
                    },
                };
                p.pop();
                p.push(field("sha256"));
                let sha =
                    string(get("sha256"), &p, &mut errors, false, 0, usize::MAX).filter(|value| {
                        let valid = value.len() == 64
                            && value
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
                        if !valid {
                            problem(
                                &mut errors,
                                &p,
                                "string_pattern_mismatch",
                                "Invalid SHA-256",
                            );
                        }
                        valid
                    });
                extras(d, id, &["key", "size_bytes", "sha256"], &path, &mut errors);
                if let (Some(key), Some(size_bytes), Some(sha256)) = (key, size, sha) {
                    logs.push(LogRef {
                        key,
                        size_bytes,
                        sha256,
                    });
                }
            }
        }
        _ => problem(
            &mut errors,
            &[field("logs")],
            "list_type",
            "Input should be a valid list",
        ),
    }
    extras(
        d,
        d.root(),
        &["reason", "code", "step", "logs"],
        &[],
        &mut errors,
    );
    match reason {
        Some(reason) if errors.is_empty() => Ok(Release {
            reason,
            code,
            step,
            logs,
        }),
        _ => Err(ValidationErrors::from_problems(errors)),
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
