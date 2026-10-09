// SPDX-License-Identifier: AGPL-3.0-only
//! Plan units: the fields a plan stores for each unit, the hypothesis
//! document each becomes, and the conversions the plan routes share.
use crate::api_models::{ContextItem, PlanUnitOut, UnitRelation};
use cannery_core::json::{self, Document};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// The structured fields of a unit, as a plan entry stores them.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UnitFields {
    pub(crate) title: String,
    pub(crate) question: String,
    pub(crate) intervention: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) control: Option<BTreeMap<String, Value>>,
    pub(crate) acceptance: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parameters: Option<BTreeMap<String, Value>>,
    #[serde(default)]
    pub(crate) relations: Vec<UnitRelation>,
    #[serde(default)]
    pub(crate) context: Vec<ContextItem>,
}

/// The rationale of every hypothesis a plan writes: the reasoning lives in
/// the plan's approach and the unit's brief.
pub(crate) fn rationale(track: &str) -> String {
    format!("Planned in the {track} track plan; see its approach and this unit's brief.")
}

/// The hypothesis document a unit becomes, with its relations already
/// resolved to hypothesis references.
pub(crate) fn hypothesis_value(track: &str, fields: &UnitFields, relations: &[Value]) -> Value {
    let mut document = Map::new();
    document.insert("schema_version".into(), Value::from("0.2"));
    document.insert("track".into(), Value::from(track));
    document.insert("title".into(), Value::from(fields.title.clone()));
    document.insert("question".into(), Value::from(fields.question.clone()));
    document.insert("rationale".into(), Value::from(rationale(track)));
    document.insert(
        "intervention".into(),
        Value::from(fields.intervention.clone()),
    );
    if let Some(control) = &fields.control {
        document.insert("control".into(), object(control));
    }
    document.insert("plan".into(), object(&fields.acceptance));
    if !relations.is_empty() {
        document.insert("relations".into(), Value::Array(relations.to_vec()));
    }
    if let Some(parameters) = &fields.parameters {
        document.insert("project_fields".into(), object(parameters));
    }
    Value::Object(document)
}

fn object(values: &BTreeMap<String, Value>) -> Value {
    Value::Object(
        values
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    )
}

/// The relations of a unit that name hypotheses, as the hypothesis document
/// writes them; relations to other units of the plan are left out.
pub(crate) fn hypothesis_relations(fields: &UnitFields) -> Vec<Value> {
    fields
        .relations
        .iter()
        .filter_map(|relation| {
            relation
                .hypothesis
                .as_ref()
                .map(|target| serde_json::json!({"kind": relation.kind, "hypothesis": target}))
        })
        .collect()
}

/// A JSON value as the domain's JSON document.
pub(crate) fn document(value: &Value) -> Option<Document> {
    let bytes = serde_json::to_vec(value).ok()?;
    json::decode(&bytes, crate::body::REST_JSON_NESTING_BUDGET).ok()
}

/// The fields of a hypothesis that no plan wrote, read back from its document.
pub(crate) fn fields_from_hypothesis(content: &Value) -> UnitFields {
    let text = |key: &str| {
        content
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let map = |key: &str| {
        content.get(key).and_then(Value::as_object).map(|values| {
            values
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<BTreeMap<_, _>>()
        })
    };
    let relations = content
        .get("relations")
        .and_then(Value::as_array)
        .map(|relations| {
            relations
                .iter()
                .map(|relation| UnitRelation {
                    kind: relation
                        .get("kind")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    hypothesis: relation.get("hypothesis").cloned(),
                    unit: None,
                })
                .collect()
        })
        .unwrap_or_default();
    UnitFields {
        title: text("title"),
        question: text("question"),
        intervention: text("intervention"),
        control: map("control"),
        acceptance: map("plan").unwrap_or_default(),
        parameters: map("project_fields").filter(|values| !values.is_empty()),
        relations,
        context: Vec::new(),
    }
}

/// A stored entry as the API shows it.
pub(crate) fn entry_out(entry: &cannery_tracks::plans::PlanUnit) -> Option<PlanUnitOut> {
    let fields: UnitFields = serde_json::from_str(&entry.fields).ok()?;
    Some(PlanUnitOut {
        key: entry.key.clone(),
        number: entry.number.map(i64::from),
        state: entry.state.clone(),
        redo_of: entry.redo_of_number.map(i64::from),
        title: fields.title,
        question: fields.question,
        intervention: fields.intervention,
        control: fields.control,
        acceptance: fields.acceptance,
        parameters: fields.parameters,
        relations: fields.relations,
        context: fields.context,
        brief: entry.brief.clone(),
        science_revision: i64::from(entry.science_revision),
        hypothesis_revision: entry.hypothesis_revision.map(i64::from),
    })
}

/// Whether a slug is a valid unit key.
pub(crate) fn valid_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 63
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

/// The longest prefix of `text` that fits in `limit` bytes on a character
/// boundary, with an ellipsis when it was cut.
pub(crate) fn truncate(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit.saturating_sub('…'.len_utf8());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", text[..end].trim_end())
}

/// The first non-empty line of a text, without Markdown heading marks.
pub(crate) fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("---"))
        .map(|line| line.trim_start_matches('#').trim().to_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields() -> UnitFields {
        UnitFields {
            title: "Lower the learning rate".into(),
            question: "Does a lower rate help?".into(),
            intervention: "Halve it.".into(),
            control: None,
            acceptance: BTreeMap::from([("primary_metric".into(), Value::from("accuracy"))]),
            parameters: None,
            relations: vec![
                UnitRelation {
                    kind: "derived_from".into(),
                    hypothesis: Some(Value::from(3)),
                    unit: None,
                },
                UnitRelation {
                    kind: "related_to".into(),
                    hypothesis: None,
                    unit: Some("other".into()),
                },
            ],
            context: Vec::new(),
        }
    }

    #[test]
    fn a_unit_becomes_a_hypothesis_document() {
        let fields = fields();
        let relations = hypothesis_relations(&fields);
        assert_eq!(
            relations,
            vec![serde_json::json!({"kind":"derived_from","hypothesis":3})]
        );
        let value = hypothesis_value("tuning", &fields, &relations);
        assert_eq!(value["schema_version"], "0.2");
        assert_eq!(value["track"], "tuning");
        assert_eq!(value["plan"]["primary_metric"], "accuracy");
        assert!(value.get("project_fields").is_none());
        assert!(
            value["rationale"]
                .as_str()
                .is_some_and(|text| text.contains("tuning"))
        );
        assert!(document(&value).is_some());
        let back = fields_from_hypothesis(&value);
        assert_eq!(back.title, fields.title);
        assert_eq!(back.acceptance, fields.acceptance);
        assert_eq!(back.relations.len(), 1);
    }

    #[test]
    fn keys_truncation_and_summaries() {
        assert!(valid_key("a-1"));
        assert!(!valid_key("-a"));
        assert!(!valid_key("A"));
        assert!(!valid_key(""));
        assert!(!valid_key(&"a".repeat(64)));
        assert_eq!(truncate("short", 10), "short");
        let cut = truncate("ééééééééé", 9);
        assert!(cut.len() <= 9, "{cut}");
        assert!(cut.ends_with('…'));
        assert_eq!(first_line("\n# Title\nmore"), "Title");
        assert_eq!(first_line(""), "");
    }
}
