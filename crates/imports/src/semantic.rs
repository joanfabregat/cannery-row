use crate::{Entry, Error, Problem, Result, time};
use cannery_core::{
    ids::UserId,
    json::{self, Document},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or(Error::CorruptData)
}
pub(crate) fn items<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}
pub(crate) fn document(value: &Value, budget: usize) -> Result<Document> {
    json::decode(value.to_string().as_bytes(), budget).map_err(|_| Error::CorruptData)
}
pub(crate) fn provenance(value: &Value, keys: &[&str]) -> Value {
    Value::Object(
        keys.iter()
            .filter_map(|key| value.get(*key).map(|v| ((*key).into(), v.clone())))
            .collect(),
    )
}
pub(crate) fn imported_document(content: &Value, numbers: &BTreeMap<String, i32>) -> Value {
    let mut value =
        json!({"schema_version":"0.2","track":content["track"],"title":content["title"]});
    if let Some(fields) = content.get("document").and_then(Value::as_object) {
        for (key, item) in fields {
            value[key] = item.clone();
        }
    } else {
        value["question"] = content["claim"].clone();
    }
    let relations=items(content,"relations").iter().map(|relation|json!({"kind":relation["type"],"hypothesis":relation["to"].as_str().and_then(|key|numbers.get(key)).copied().unwrap_or(1)})).collect::<Vec<_>>();
    if !relations.is_empty() {
        value["relations"] = json!(relations);
    }
    value
}
pub(crate) struct Knowledge<'a> {
    pub tracks: &'a BTreeMap<String, Value>,
    pub hypotheses: &'a BTreeSet<String>,
    pub policies: &'a BTreeMap<String, Value>,
    pub artifact_uris: &'a BTreeSet<String>,
    pub users: &'a BTreeMap<String, UserId>,
    pub researchers: Option<&'a BTreeSet<UserId>>,
    pub science: &'a Value,
}
fn violation(problems: &mut Vec<Problem>, entry: &Entry, path: &str, message: &str) {
    problems.push(entry.problem(path, message));
}
fn dimensions(value: &Value) -> BTreeMap<String, String> {
    value["dimensions"]
        .as_object()
        .map(|fields| {
            fields
                .iter()
                .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.into())))
                .collect()
        })
        .unwrap_or_default()
}
fn metric_key(value: &Value) -> Result<(String, String, BTreeMap<String, String>)> {
    Ok((
        text(value, "metric")?.into(),
        text(value, "split")?.into(),
        dimensions(value),
    ))
}
fn registered(
    entry: &Entry,
    value: &Value,
    path: &str,
    knowledge: &Knowledge<'_>,
    problems: &mut Vec<Problem>,
) -> bool {
    let Some(metric) = items(knowledge.science, "metrics")
        .iter()
        .find(|metric| metric["key"] == value["metric"])
    else {
        violation(
            problems,
            entry,
            &format!("{path}/metric"),
            "not a registered metric",
        );
        return false;
    };
    if !items(metric, "splits").contains(&value["split"]) {
        violation(
            problems,
            entry,
            &format!("{path}/split"),
            "not a registered split",
        );
    }
    for (name, value) in dimensions(value) {
        let Some(dimension) = items(metric, "dimensions")
            .iter()
            .find(|dimension| dimension["name"] == name)
        else {
            violation(
                problems,
                entry,
                &format!("{path}/dimensions/{name}"),
                "not a registered dimension",
            );
            continue;
        };
        if dimension.get("values").is_some()
            && !items(dimension, "values").contains(&Value::String(value))
        {
            violation(
                problems,
                entry,
                &format!("{path}/dimensions/{name}"),
                "not a registered dimension value",
            );
        }
    }
    true
}
fn document_location(source: &str) -> bool {
    let Some((location, commit)) = source.rsplit_once('@') else {
        return false;
    };
    if !(7..=40).contains(&commit.len())
        || !commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return false;
    }
    let Some((path, lines)) = location.rsplit_once(':') else {
        return false;
    };
    !path.is_empty()
        && !path
            .chars()
            .any(|ch| ch.is_whitespace() || [':', '@', '#'].contains(&ch))
        && lines.split('-').count() <= 2
        && lines
            .split('-')
            .all(|line| line.parse::<u64>().is_ok_and(|number| number > 0))
}
fn decimal(value: &Value) -> Option<(String, i64)> {
    let source = value.as_number()?.to_string();
    let (coefficient, exponent) = source
        .split_once(['e', 'E'])
        .map_or((source.as_str(), Some(0)), |(coefficient, exponent)| {
            (coefficient, exponent.parse::<i64>().ok())
        });
    let mut exponent = exponent?;
    let negative = coefficient.starts_with('-');
    let coefficient = coefficient.trim_start_matches(['-', '+']);
    exponent = exponent.checked_sub(
        i64::try_from(
            coefficient
                .split_once('.')
                .map_or(0, |(_, fraction)| fraction.len()),
        )
        .ok()?,
    )?;
    let mut digits = coefficient
        .replace('.', "")
        .trim_start_matches('0')
        .to_owned();
    if digits.is_empty() {
        return Some(("0".into(), 0));
    }
    while digits.ends_with('0') {
        digits.pop();
        exponent = exponent.checked_add(1)?;
    }
    if negative {
        digits.insert(0, '-');
    }
    Some((digits, exponent))
}
pub(crate) fn equal_numbers(left: &Value, right: &Value) -> bool {
    left == right
        || decimal(left)
            .zip(decimal(right))
            .is_some_and(|(left, right)| left == right)
}
#[allow(clippy::too_many_lines)] // Historical cross-reference checks accumulate file-local problems.
pub(crate) fn check_hypothesis(entry: &Entry, knowledge: &Knowledge<'_>) -> Result<Vec<Problem>> {
    let content = &entry.content;
    let mut problems = Vec::new();
    let state = text(content, "state")?;
    let created = text(content, "created_at")?;
    let track = knowledge.tracks.get(text(content, "track")?);
    if let Some(track) = track {
        if track["state"] == "archived" && state == "awaiting_human_review" {
            violation(
                &mut problems,
                entry,
                "/state",
                "an archived track has no pending hypothesis",
            );
        }
        if time::before(created, text(track, "created_at")?)? {
            violation(
                &mut problems,
                entry,
                "/created_at",
                "before the track was created",
            );
        }
    } else {
        violation(
            &mut problems,
            entry,
            "/track",
            "no such track in the history",
        );
    }
    let mut references = Vec::new();
    if let Some(target) = content["control"].get("hypothesis").and_then(Value::as_str) {
        references.push(("/control/hypothesis".into(), target));
    }
    for (index, relation) in items(content, "relations").iter().enumerate() {
        references.push((format!("/relations/{index}/to"), text(relation, "to")?));
    }
    for (path, target) in references {
        if target == entry.key {
            violation(
                &mut problems,
                entry,
                &path,
                "a hypothesis cannot reference itself",
            );
        } else if !knowledge.hypotheses.contains(target) {
            violation(
                &mut problems,
                entry,
                &path,
                "no such hypothesis in the history",
            );
        }
    }
    let attempts = items(content, "attempts");
    let mut labels = BTreeSet::new();
    for (index, attempt) in attempts.iter().enumerate() {
        let path = format!("/attempts/{index}");
        if !labels.insert(text(attempt, "label")?) {
            violation(
                &mut problems,
                entry,
                &format!("{path}/label"),
                "duplicate attempt label",
            );
        }
        let started = text(attempt, "started_at")?;
        let ended = attempt
            .get("finished_at")
            .and_then(Value::as_str)
            .unwrap_or(started);
        if time::before(started, created)? {
            violation(
                &mut problems,
                entry,
                &format!("{path}/started_at"),
                "before the hypothesis was created",
            );
        }
        if time::before(ended, started)? {
            violation(
                &mut problems,
                entry,
                &format!("{path}/finished_at"),
                "before the attempt started",
            );
        }
        if let Some(report) = attempt.get("report")
            && time::before(text(report, "written_at")?, ended)?
        {
            violation(
                &mut problems,
                entry,
                &format!("{path}/report/written_at"),
                "before the attempt finished",
            );
        }
        let mut slices = BTreeSet::new();
        for (position, measurement) in items(attempt, "measurements").iter().enumerate() {
            let here = format!("{path}/measurements/{position}");
            registered(entry, measurement, &here, knowledge, &mut problems);
            if !slices.insert(metric_key(measurement)?) {
                violation(&mut problems, entry, &here, "duplicate measurement slice");
            }
            let source = text(measurement, "source")?;
            if measurement["authority"] == "imported_transcribed" {
                if !document_location(source) {
                    violation(
                        &mut problems,
                        entry,
                        &format!("{here}/source"),
                        "must cite a document location",
                    );
                }
            } else {
                let uri = source
                    .split_once('#')
                    .filter(|(_, pointer)| pointer.is_empty() || pointer.starts_with('/'))
                    .map(|(uri, _)| uri);
                if !uri.is_some_and(|uri| knowledge.artifact_uris.contains(uri)) {
                    violation(
                        &mut problems,
                        entry,
                        &format!("{here}/source"),
                        "must cite an artifact of the bundle with a JSON pointer",
                    );
                }
            }
        }
        if let Some(verdict) = attempt.get("verdict") {
            let here = format!("{path}/verdict");
            if attempt["status"] == "failed" {
                violation(
                    &mut problems,
                    entry,
                    &here,
                    "a failed attempt has no verdict",
                );
            }
            if let Some(policy) = knowledge.policies.get(text(verdict, "policy")?) {
                for (position, gate) in items(verdict, "gates").iter().enumerate() {
                    if !items(policy, "gates")
                        .iter()
                        .any(|registered| registered["id"] == gate["id"])
                    {
                        violation(
                            &mut problems,
                            entry,
                            &format!("{here}/gates/{position}/id"),
                            "not a gate of the historical policy",
                        );
                    }
                }
            } else {
                violation(
                    &mut problems,
                    entry,
                    &format!("{here}/policy"),
                    "no such historical policy",
                );
            }
            if let Some(evaluated) = verdict.get("evaluated_at").and_then(Value::as_str)
                && time::before(evaluated, ended)?
            {
                violation(
                    &mut problems,
                    entry,
                    &format!("{here}/evaluated_at"),
                    "before the attempt finished",
                );
            }
            let mut compared = BTreeSet::new();
            for (position, comparison) in items(verdict, "comparisons").iter().enumerate() {
                let here = format!("{here}/comparisons/{position}");
                registered(entry, comparison, &here, knowledge, &mut problems);
                let key = metric_key(comparison)?;
                if !compared.insert(key.clone()) {
                    violation(&mut problems, entry, &here, "duplicate comparison slice");
                }
                if comparison["source"] == "tester" {
                    let cited = items(attempt, "measurements")
                        .iter()
                        .filter(|measurement| {
                            metric_key(measurement).is_ok_and(|candidate| candidate == key)
                                && measurement.get("value").is_some()
                        })
                        .collect::<Vec<_>>();
                    if cited.len() != 1 {
                        violation(
                            &mut problems,
                            entry,
                            &format!("{here}/source"),
                            "must cite one imported measurement with a value",
                        );
                    } else if decimal(&comparison["value"]).is_none()
                        || decimal(&cited[0]["value"]) != decimal(&comparison["value"])
                    {
                        violation(
                            &mut problems,
                            entry,
                            &format!("{here}/value"),
                            "differs from the measurement it cites",
                        );
                    }
                }
            }
        }
    }
    let decision = content.get("decision");
    if state == "awaiting_human_review" {
        if decision.is_some() {
            violation(
                &mut problems,
                entry,
                "/decision",
                "a pending hypothesis has no decision yet",
            );
        }
        if !attempts.last().is_some_and(|attempt| {
            attempt["status"] == "completed" && attempt.get("verdict").is_some()
        }) {
            violation(
                &mut problems,
                entry,
                "/attempts",
                "the last attempt must complete with a verdict",
            );
        }
    } else if let Some(decision) = decision {
        let outcome = match text(decision, "action")? {
            "promote" => "promoted",
            "reject" => "rejected",
            "inconclusive" => "inconclusive",
            "close_failed" => "failed",
            _ => return Err(Error::CorruptData),
        };
        if outcome != state {
            violation(
                &mut problems,
                entry,
                "/decision/action",
                "does not lead to the hypothesis state",
            );
        }
        match knowledge.users.get(text(decision, "decided_by")?) {
            None => violation(
                &mut problems,
                entry,
                "/decision/decided_by",
                "no unique user has this verified email",
            ),
            Some(user)
                if knowledge
                    .researchers
                    .is_some_and(|researchers| !researchers.contains(user)) =>
            {
                violation(
                    &mut problems,
                    entry,
                    "/decision/decided_by",
                    "not a researcher of the project",
                );
            }
            _ => {}
        }
        let mut floor = created;
        if let Some(last) = attempts.last() {
            floor = last
                .get("finished_at")
                .and_then(Value::as_str)
                .unwrap_or(text(last, "started_at")?);
            if state == "failed" {
                if last["status"] != "failed" {
                    violation(
                        &mut problems,
                        entry,
                        "/attempts",
                        "close_failed requires the last attempt to fail",
                    );
                }
            } else if let Some(verdict) = last.get("verdict") {
                if last["status"] != "completed" {
                    violation(
                        &mut problems,
                        entry,
                        "/attempts",
                        "a result decision requires a completed attempt",
                    );
                }
                if state == "promoted" && verdict["result"] != "pass" {
                    violation(
                        &mut problems,
                        entry,
                        "/attempts",
                        "promotion requires a pass verdict",
                    );
                }
                floor = verdict
                    .get("evaluated_at")
                    .and_then(Value::as_str)
                    .unwrap_or(floor);
            } else {
                violation(
                    &mut problems,
                    entry,
                    "/attempts",
                    "a result decision requires a verdict",
                );
            }
        } else {
            violation(
                &mut problems,
                entry,
                "/attempts",
                "this decision requires an attempt",
            );
        }
        if time::before(text(decision, "decided_at")?, floor)? {
            violation(
                &mut problems,
                entry,
                "/decision/decided_at",
                "before the event it decides",
            );
        }
    } else {
        violation(
            &mut problems,
            entry,
            "/decision",
            "a terminal hypothesis requires its human decision",
        );
    }
    Ok(problems)
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;

#[cfg(test)]
mod tests {
    use super::decimal;
    use serde_json::Value;
    #[test]
    fn historical_comparisons_keep_decimal_precision_and_bound_exponents()
    -> Result<(), serde_json::Error> {
        let number = |text: &str| serde_json::from_str::<Value>(text);
        assert_eq!(decimal(&number("1.200")?), decimal(&number("12e-1")?));
        assert_ne!(
            decimal(&number("9007199254740992")?),
            decimal(&number("9007199254740993")?)
        );
        assert_eq!(decimal(&number("-0.00")?), decimal(&number("0")?));
        assert!(decimal(&number("10e9223372036854775807")?).is_none());
        assert!(decimal(&number("1.0e-9223372036854775808")?).is_none());
        Ok(())
    }
}
