//! Comment models preserve source field validation order and lossless strings.
use crate::validation::{
    self, BodyInput, Location, Parameter, ParameterValue, Problem, ValidationErrors,
};
use cannery_core::json::Node;
use num_bigint::BigInt;

pub(crate) struct CommentBody {
    pub body: String,
    pub revision: Option<BigInt>,
}

fn problem(field: Option<&str>, kind: &'static str) -> Problem {
    let mut loc = vec![Location::Field(String::from("body"))];
    if let Some(field) = field {
        loc.push(Location::Field(String::from(field)));
    }
    Problem {
        loc,
        kind,
        message: "Invalid comment",
    }
}

fn body_issue(value: &String) -> Option<&'static str> {
    if value.as_utf8().is_none() {
        Some("string_unicode")
    } else if value.codepoints().is_empty() {
        Some("string_too_short")
    } else if value.codepoints().len() > 65_536 {
        Some("string_too_long")
    } else if value.codepoints().iter().all(|point| {
        // Pydantic's Unicode White_Space, distinct from Python isspace.
        matches!(
            point,
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
    }) {
        Some("string_pattern_mismatch")
    } else {
        None
    }
}
pub(crate) fn parse(input: BodyInput<'_>, editing: bool) -> Result<CommentBody, ValidationErrors> {
    let document = match input {
        BodyInput::Missing => {
            return Err(ValidationErrors::from_problems(vec![problem(
                None, "missing",
            )]));
        }
        BodyInput::RawBytes => {
            return Err(ValidationErrors::from_problems(vec![problem(
                None,
                "model_attributes_type",
            )]));
        }
        BodyInput::Json(document) => document,
    };
    let fields = match document.node(document.root()) {
        Some(Node::Null) => {
            return Err(ValidationErrors::from_problems(vec![problem(
                None, "missing",
            )]));
        }
        Some(Node::Object(fields)) => fields,
        _ => {
            return Err(ValidationErrors::from_problems(vec![problem(
                None,
                "model_attributes_type",
            )]));
        }
    };
    let mut problems = Vec::new();
    let revision = if editing {
        match document
            .field(document.root(), "expected_revision")
            .and_then(|id| document.node(id))
        {
            None => {
                problems.push(problem(Some("expected_revision"), "missing"));
                None
            }
            Some(value) => {
                match validation::validate_parameter_node(value, Parameter::ConfigRevision) {
                    Ok(ParameterValue::ConfigRevision(value)) => Some(value),
                    Ok(_) => None,
                    Err(errors) => {
                        for mut error in errors.problems().to_vec() {
                            error.loc = problem(Some("expected_revision"), error.kind).loc;
                            problems.push(error);
                        }
                        None
                    }
                }
            }
        }
    } else {
        None
    };
    let body = match document
        .field(document.root(), "body_markdown")
        .and_then(|id| document.node(id))
    {
        None => {
            problems.push(problem(Some("body_markdown"), "missing"));
            None
        }
        Some(Node::String(value)) => {
            if let Some(kind) = body_issue(value) {
                problems.push(problem(Some("body_markdown"), kind));
                None
            } else {
                Some(value.clone())
            }
        }
        Some(_) => {
            problems.push(problem(Some("body_markdown"), "string_type"));
            None
        }
    };
    for (name, _) in fields {
        if !(name.equals_utf8("body_markdown") || editing && name.equals_utf8("expected_revision"))
        {
            let mut error = problem(None, "extra_forbidden");
            error.loc.push(Location::Field(name.clone()));
            problems.push(error);
        }
    }
    if let Some(body) = body
        && problems.is_empty()
    {
        Ok(CommentBody { body, revision })
    } else {
        Err(ValidationErrors::from_problems(problems))
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
