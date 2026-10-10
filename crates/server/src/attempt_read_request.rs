//! Attempt read validation follows the source path and query signature order.
use crate::{
    request_context::QueryParams,
    validation::{self, Location, Problem, ValidationErrors},
};
use cannery_core::ids::AttemptId;
use num_bigint::BigInt;
use num_traits::ToPrimitive;

pub(crate) const STATES: &[&str] = &[
    "claimed",
    "running",
    "verifying",
    "verified",
    "failed",
    "cancelled",
    "unreviewed",
];
#[derive(Clone, Copy)]
pub(crate) enum Operation {
    Unit,
    Project,
    Detail,
}
pub(crate) struct Parameters {
    pub number: Option<BigInt>,
    pub sequence: Option<BigInt>,
    pub after: Option<BigInt>,
    pub states: Option<Vec<String>>,
    pub track: Option<String>,
    pub before: Option<AttemptId>,
    pub limit: usize,
}
pub(crate) fn parse(
    number: Option<&str>,
    sequence: Option<&str>,
    query: &QueryParams,
    operation: Operation,
) -> Result<Parameters, ValidationErrors> {
    let mut errors = Vec::new();
    let mut path = |name, raw: Option<&str>| {
        raw.and_then(|raw| match validation::attempt_path_integer(name, raw) {
            Ok(v) => Some(v),
            Err(e) => {
                errors.extend(e.problems().iter().cloned());
                None
            }
        })
    };
    let number = path("number", number);
    let sequence = path("sequence", sequence);
    let mut result = Parameters {
        number,
        sequence,
        after: None,
        states: None,
        track: None,
        before: None,
        limit: 50,
    };
    if matches!(operation, Operation::Project) {
        let states = query
            .pairs()
            .iter()
            .filter(|(key, _)| key == "state")
            .map(|(_, v)| v.clone())
            .collect::<Vec<_>>();
        for (index, state) in states.iter().enumerate() {
            if !STATES.contains(&state.as_str()) {
                errors.push(Problem {
                    loc: vec![
                        Location::Field(String::from("query")),
                        Location::Field(String::from("state")),
                        Location::Index(index),
                    ],
                    kind: "literal_error",
                    message: "Invalid attempt state",
                });
            }
        }
        result.states = (!states.is_empty()).then_some(states);
        result.track = query.get("track").map(str::to_owned);
        let pairs = query
            .pairs()
            .iter()
            .filter(|(key, _)| matches!(key.as_str(), "before" | "limit"))
            .map(|(k, v)| (String::from(k), String::from(v)))
            .collect::<Vec<_>>();
        match validation::review_attention_parameters(None, &pairs, true, false) {
            Ok(v) => {
                result.before = v.before.map(|v| AttemptId(v.0));
                result.limit = v.limit;
            }
            Err(e) => errors.extend(e.problems().iter().cloned()),
        }
    } else if matches!(operation, Operation::Unit) {
        if let Some(raw) = query.get("before") {
            match validation::bounded_query_integer(raw, "before", 0, Some(i64::from(i32::MAX))) {
                Ok(v) => result.after = Some(v),
                Err(e) => errors.extend(e.problems().iter().cloned()),
            }
        }
        if let Some(raw) = query.get("limit") {
            match validation::bounded_query_integer(raw, "limit", 1, Some(200)) {
                Ok(v) => result.limit = v.to_usize().unwrap_or(50),
                Err(e) => errors.extend(e.problems().iter().cloned()),
            }
        }
    }
    if errors.is_empty() {
        Ok(result)
    } else {
        Err(ValidationErrors::from_problems(errors))
    }
}
