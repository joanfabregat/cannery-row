//! Source `DraftUpdate` field order, reusing the measured arbitrary-int coercer.
use crate::validation::{
    self, BodyInput, Location, Parameter, ParameterValue, Problem, ValidationErrors,
};
use cannery_core::json::{Document, DocumentBuilder, Node};
use num_bigint::BigInt;

pub(crate) struct Update {
    pub revision: BigInt,
    pub document: Document,
}
fn issue(field: Option<String>, kind: &'static str) -> Problem {
    let mut loc = vec![Location::Field(String::from("body"))];
    if let Some(field) = field {
        loc.push(Location::Field(field));
    }
    Problem {
        loc,
        kind,
        message: "Invalid draft update",
    }
}
pub(crate) fn update(input: BodyInput<'_>) -> Result<Update, ValidationErrors> {
    let document = match input {
        BodyInput::Json(d) if matches!(d.node(d.root()), Some(Node::Null)) => {
            return Err(ValidationErrors::from_problems(vec![issue(
                None, "missing",
            )]));
        }
        BodyInput::Json(d) => d,
        BodyInput::Missing => {
            return Err(ValidationErrors::from_problems(vec![issue(
                None, "missing",
            )]));
        }
        BodyInput::RawBytes => {
            return Err(ValidationErrors::from_problems(vec![issue(
                None,
                "model_attributes_type",
            )]));
        }
    };
    let Some(Node::Object(fields)) = document.node(document.root()) else {
        return Err(ValidationErrors::from_problems(vec![issue(
            None,
            "model_attributes_type",
        )]));
    };
    let mut problems = vec![];
    let revision = match document
        .field(document.root(), "expected_revision")
        .and_then(|id| document.node(id))
    {
        None => {
            problems.push(issue(Some(String::from("expected_revision")), "missing"));
            None
        }
        Some(value) => {
            match validation::validate_parameter_node(value, Parameter::ConfigRevision) {
                Ok(ParameterValue::ConfigRevision(v)) => Some(v),
                Ok(_) => None,
                Err(e) => {
                    for mut p in e.problems().to_vec() {
                        p.loc = vec![
                            Location::Field(String::from("body")),
                            Location::Field(String::from("expected_revision")),
                        ];
                        problems.push(p);
                    }
                    None
                }
            }
        }
    };
    let content = match document.field(document.root(), "document") {
        None => {
            problems.push(issue(Some(String::from("document")), "missing"));
            None
        }
        Some(id) if matches!(document.node(id), Some(Node::Object(_))) => {
            let mut builder = DocumentBuilder::new();
            builder
                .import(document, id)
                .and_then(|root| builder.finish(root))
                .ok()
        }
        Some(_) => {
            problems.push(issue(Some(String::from("document")), "dict_type"));
            None
        }
    };
    for (name, _) in fields {
        if !name.equals_utf8("expected_revision") && !name.equals_utf8("document") {
            problems.push(issue(Some(name.clone()), "extra_forbidden"));
        }
    }
    match (revision, content) {
        (Some(revision), Some(document)) if problems.is_empty() => {
            Ok(Update { revision, document })
        }
        _ => Err(ValidationErrors::from_problems(problems)),
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
