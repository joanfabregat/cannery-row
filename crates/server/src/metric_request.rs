//! Ordered metric query validation with the frozen Pydantic datetime parser.
use crate::{
    request_context::QueryParams,
    validation::{self, Location, Problem, ValidationErrors},
};

use cannery_metrics::repo::TimeBound;
use num_bigint::BigInt;
use num_traits::ToPrimitive;

#[derive(Clone, Copy)]
pub(crate) enum Operation {
    Catalog,
    Query,
    Dashboard,
    View,
}
pub(crate) struct Parameters {
    pub science_revision: Option<BigInt>,
    pub dashboard_revision: Option<BigInt>,
    pub metric: Option<String>,
    pub split: Option<String>,
    pub authority: String,
    pub dimensions: Option<Vec<String>>,
    pub filters: Vec<String>,
    pub all_slices: bool,
    pub tracks: Option<Vec<String>>,
    pub attempt_states: Option<Vec<String>>,
    pub since: Option<TimeBound>,
    pub until: Option<TimeBound>,
    pub before: Option<BigInt>,
    pub limit: usize,
}
fn problem(name: &str, index: Option<usize>, kind: &'static str, message: &'static str) -> Problem {
    let mut loc = vec![
        Location::Field(String::from("query")),
        Location::Field(String::from(name)),
    ];
    if let Some(index) = index {
        loc.push(Location::Index(index));
    }
    Problem { loc, kind, message }
}
fn integer(
    q: &QueryParams,
    name: &str,
    min: i64,
    max: Option<i64>,
    errors: &mut Vec<Problem>,
) -> Option<BigInt> {
    let value = q.get(name)?;
    match validation::bounded_query_integer(value, name, min, max) {
        Ok(value) => Some(value),
        Err(error) => {
            errors.extend(error.problems().iter().cloned());
            None
        }
    }
}
fn list(q: &QueryParams, name: &str) -> Option<Vec<String>> {
    let values = q
        .pairs()
        .iter()
        .filter(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
        .collect::<Vec<_>>();
    (!values.is_empty()).then_some(values)
}
fn datetime(q: &QueryParams, name: &str, errors: &mut Vec<Problem>) -> Option<TimeBound> {
    match crate::datetime_query::parse(q.get(name)?) {
        Ok(crate::datetime_query::ParsedDateTime::Aware(value)) => Some(TimeBound::Aware(value)),
        Ok(crate::datetime_query::ParsedDateTime::Naive(value)) => Some(TimeBound::Naive(value)),
        Err(error) => {
            errors.push(problem(
                name,
                None,
                error.source_category(),
                "Input should be a valid datetime",
            ));
            None
        }
    }
}
fn metric_key(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|v| v.is_ascii_lowercase() || v.is_ascii_digit() || *v == b'_')
}
#[allow(
    clippy::too_many_lines,
    reason = "Preserve the source query signature's validation order"
)]
pub(crate) fn parse(q: &QueryParams, operation: Operation) -> Result<Parameters, ValidationErrors> {
    let mut errors = Vec::new();
    let mut result = Parameters {
        science_revision: None,
        dashboard_revision: None,
        metric: None,
        split: None,
        authority: "tester_verified".into(),
        dimensions: None,
        filters: Vec::new(),
        all_slices: false,
        tracks: None,
        attempt_states: None,
        since: None,
        until: None,
        before: None,
        limit: 50,
    };
    match operation {
        Operation::Catalog => {
            result.science_revision = integer(
                q,
                "science_revision",
                1,
                Some(i64::from(i32::MAX)),
                &mut errors,
            );
        }
        Operation::Dashboard | Operation::View => {
            result.dashboard_revision = integer(
                q,
                "dashboard_revision",
                1,
                Some(i64::from(i32::MAX)),
                &mut errors,
            );
            if matches!(operation, Operation::View)
                && let Some(authority) = q.get("authority")
            {
                if ["tester_verified", "imported"].contains(&authority) {
                    result.authority = authority.into();
                } else {
                    errors.push(problem(
                        "authority",
                        None,
                        "literal_error",
                        "Input should be a supported authority",
                    ));
                }
            }
        }
        Operation::Query => {
            match q.get("metric") {
                None => errors.push(problem("metric", None, "missing", "Field required")),
                Some(value) if metric_key(value) => result.metric = Some(value.into()),
                Some(_) => errors.push(problem(
                    "metric",
                    None,
                    "string_pattern_mismatch",
                    "String should match the metric pattern",
                )),
            }
            result.split = q.get("split").map(str::to_owned);
            if let Some(authority) = q.get("authority") {
                if [
                    "tester_verified",
                    "agent_claim",
                    "imported_artifact",
                    "imported_transcribed",
                    "imported",
                ]
                .contains(&authority)
                {
                    result.authority = authority.into();
                } else {
                    errors.push(problem(
                        "authority",
                        None,
                        "literal_error",
                        "Input should be a supported authority",
                    ));
                }
            }
            result.dimensions = list(q, "dimensions");
            result.filters = list(q, "filter")
                .unwrap_or_default()
                .iter()
                .map(String::from)
                .collect();
            if let Some(value) = q.get("all_slices") {
                match value.to_ascii_lowercase().as_str() {
                    "1" | "true" | "t" | "on" | "yes" | "y" => result.all_slices = true,
                    "0" | "false" | "f" | "off" | "no" | "n" => {}
                    _ => errors.push(problem(
                        "all_slices",
                        None,
                        "bool_parsing",
                        "Input should be a valid boolean",
                    )),
                }
            }
            result.tracks = list(q, "track");
            result.attempt_states = list(q, "attempt_state");
            if let Some(states) = &result.attempt_states {
                for (index, state) in states.iter().enumerate() {
                    if ![
                        "claimed",
                        "running",
                        "verifying",
                        "verified",
                        "failed",
                        "cancelled",
                        "unreviewed",
                    ]
                    .contains(&state.as_str())
                    {
                        errors.push(problem(
                            "attempt_state",
                            Some(index),
                            "literal_error",
                            "Input should be a supported attempt state",
                        ));
                    }
                }
            }
            result.science_revision = integer(
                q,
                "science_revision",
                1,
                Some(i64::from(i32::MAX)),
                &mut errors,
            );
            result.since = datetime(q, "since", &mut errors);
            result.until = datetime(q, "until", &mut errors);
            result.before = integer(q, "before", 1, None, &mut errors);
            match validation::bounded_query_integer(
                q.get("limit").unwrap_or("50"),
                "limit",
                1,
                Some(200),
            ) {
                Ok(value) => result.limit = value.to_usize().unwrap_or(50),
                Err(error) => errors.extend(error.problems().iter().cloned()),
            }
        }
    }
    if errors.is_empty() {
        Ok(result)
    } else {
        Err(ValidationErrors::from_problems(errors))
    }
}
