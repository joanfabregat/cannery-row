//! Search query validation in source signature order and exact cursor grammar.
use crate::{
    request_context::QueryParams,
    validation::{self, Location, Problem, ValidationErrors},
};
use base64::{
    Engine, alphabet,
    engine::{
        DecodePaddingMode,
        general_purpose::{GeneralPurpose, GeneralPurposeConfig, URL_SAFE_NO_PAD},
    },
};
use cannery_core::json::{self, Node};
use cannery_search::repo::{ActorId, Criteria, TimeBound};
use num_bigint::BigInt;
use num_traits::ToPrimitive;

pub(crate) struct Parameters {
    pub criteria: Criteria,
    pub before: Option<String>,
    pub limit: usize,
}
fn problem(name: &str, index: Option<usize>, kind: &'static str) -> Problem {
    let mut loc = vec![
        Location::Field(String::from("query")),
        Location::Field(String::from(name)),
    ];
    if let Some(index) = index {
        loc.push(Location::Index(index));
    }
    Problem {
        loc,
        kind,
        message: "Invalid search query parameter",
    }
}
fn list(
    q: &QueryParams,
    name: &str,
    choices: Option<&[&str]>,
    errors: &mut Vec<Problem>,
) -> Option<Vec<String>> {
    let values: Vec<String> = q
        .pairs()
        .iter()
        .filter(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
        .collect();
    if let Some(choices) = choices {
        for (index, value) in values.iter().enumerate() {
            if !choices.contains(&value.as_str()) {
                errors.push(problem(name, Some(index), "literal_error"));
            }
        }
    }
    (!values.is_empty()).then_some(values)
}
fn date(q: &QueryParams, name: &str, errors: &mut Vec<Problem>) -> Option<TimeBound> {
    match crate::datetime_query::parse(q.get(name)?) {
        Ok(crate::datetime_query::ParsedDateTime::Aware(value)) => Some(TimeBound::Aware(value)),
        Ok(crate::datetime_query::ParsedDateTime::Naive(value)) => Some(TimeBound::Naive(value)),
        Err(error) => {
            errors.push(problem(name, None, error.source_category()));
            None
        }
    }
}
fn space(value: char) -> bool {
    value.is_whitespace() || matches!(value, '\u{1c}'..='\u{1f}')
}
fn parse_number(value: &str) -> Option<BigInt> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 9
        || !matches!(bytes[0], b'1'..=b'9')
        || !bytes.iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    value.parse().ok()
}
fn reference(value: &str) -> Option<(Option<String>, BigInt, Option<BigInt>)> {
    let (project, number) = value.split_once('#')?;
    if !project.is_empty() {
        let bytes = project.as_bytes();
        if bytes.len() > 63
            || !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit()
            || !bytes
                .iter()
                .all(|v| v.is_ascii_lowercase() || v.is_ascii_digit() || *v == b'-')
        {
            return None;
        }
    }
    let mut parts = number.split('.');
    let number = parts.next()?;
    let sequence = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let sequence = if let Some(value) = sequence {
        Some(parse_number(value)?)
    } else {
        None
    };
    Some((
        (!project.is_empty()).then(|| project.to_owned()),
        parse_number(number)?,
        sequence,
    ))
}
#[allow(
    clippy::too_many_lines,
    reason = "Preserve query signature validation order"
)]
pub(crate) fn parse(q: &QueryParams) -> Result<Parameters, ValidationErrors> {
    let mut errors = Vec::new();
    let raw = q.get("q");
    if raw.is_some_and(|text| text.chars().count() > 500) {
        errors.push(problem("q", None, "string_too_long"));
    }
    let text = raw
        .map(|text| text.trim_matches(space))
        .filter(|text| !text.is_empty());
    let mut criteria = Criteria::new(None);
    if let Some((project, number, sequence)) = text.and_then(reference) {
        criteria.ref_project = project;
        criteria.ref_number = Some(number);
        criteria.ref_sequence = sequence;
    } else {
        criteria.text = text.map(String::from);
    }
    criteria.projects = list(q, "project", None, &mut errors);
    criteria.kinds = list(
        q,
        "kind",
        Some(&[
            "track",
            "unit",
            "attempt",
            "report",
            "verification",
            "writeup",
            "decision_reason",
            "comment",
        ]),
        &mut errors,
    );
    criteria.tracks = list(q, "track", None, &mut errors);
    criteria.unit_states = list(
        q,
        "unit_state",
        Some(&[
            "queued",
            "active",
            "documenting",
            "deciding",
            "promoted",
            "rejected",
            "inconclusive",
            "failed",
            "cancelled",
        ]),
        &mut errors,
    );
    criteria.attempt_states = list(
        q,
        "attempt_state",
        Some(&[
            "claimed",
            "running",
            "verifying",
            "verified",
            "failed",
            "cancelled",
            "unreviewed",
        ]),
        &mut errors,
    );
    criteria.verdicts = list(
        q,
        "verdict",
        Some(&["pass", "fail", "inconclusive"]),
        &mut errors,
    );
    criteria.decisions = list(
        q,
        "decision",
        Some(&["promote", "reject", "inconclusive", "failed"]),
        &mut errors,
    );
    if let Some(values) = list(q, "actor", None, &mut errors) {
        criteria.actors = Some(
            values
                .iter()
                .enumerate()
                .filter_map(|(index, value)| {
                    if let Ok(value) = uuid::Uuid::parse_str(value) {
                        Some(ActorId(value))
                    } else {
                        errors.push(problem("actor", Some(index), "uuid_parsing"));
                        None
                    }
                })
                .collect(),
        );
    }
    criteria.since = date(q, "since", &mut errors);
    criteria.until = date(q, "until", &mut errors);
    let before = q.get("before").map(str::to_owned);
    if before
        .as_ref()
        .is_some_and(|value| value.chars().count() > 200)
    {
        errors.push(problem("before", None, "string_too_long"));
    }
    let limit = if let Some(value) = q.get("limit") {
        match validation::bounded_query_integer(value, "limit", 1, Some(200)) {
            Ok(value) => value.to_usize().unwrap_or(50),
            Err(error) => {
                errors.extend(error.problems().iter().cloned());
                50
            }
        }
    } else {
        50
    };
    if errors.is_empty() {
        Ok(Parameters {
            criteria,
            before,
            limit,
        })
    } else {
        Err(ValidationErrors::from_problems(errors))
    }
}
/// The public length limit bounds decoded JSON to 150 bytes; the caller supplies
/// its actual decoder profile, independently of that transport bound.
pub(crate) fn decode_cursor(value: &str, budget: usize) -> Option<(f64, BigInt)> {
    if !value.is_ascii() {
        return None;
    }
    let mut encoded = value.replace('-', "+").replace('_', "/");
    encoded.extend(std::iter::repeat_n('=', (4 - encoded.len() % 4) % 4));
    if let Some(index) = encoded.find('=') {
        let suffix = &encoded[index..];
        if !suffix.bytes().all(|v| v == b'=')
            || match index % 4 {
                2 => suffix.len() != 2,
                3 => suffix.len() != 1,
                _ => true,
            }
        {
            return None;
        }
    }
    let engine = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(DecodePaddingMode::RequireCanonical),
    );
    let bytes = engine.decode(encoded).ok()?;
    let document = json::decode(&bytes, budget).ok()?;
    let Node::Array(values) = document.node(document.root())? else {
        return None;
    };
    if values.len() != 2 {
        return None;
    }
    let score = match document.node(values[0])? {
        Node::Float(value) => *value,
        Node::Integer(value) => value.to_f64()?,
        _ => return None,
    };
    let Node::Integer(id) = document.node(values[1])? else {
        return None;
    };
    if !score.is_finite() || id <= &BigInt::from(0) || id > &BigInt::from(i64::MAX) {
        return None;
    }
    Some((score, id.clone()))
}
pub(crate) fn encode_cursor(score: f64, id: i64) -> String {
    URL_SAFE_NO_PAD.encode(format!("[{}, {id}]", json::float_text(score)))
}
