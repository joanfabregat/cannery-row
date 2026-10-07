#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::panic)] // Frozen oracle assertions.
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_server::datetime_query::{ParsedDateTime, parse};
use chrono::{Datelike, Timelike};
use serde_json::{Value, json};

fn reference() -> Value {
    serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/datetime-query-reference.json"
    ))
    .unwrap()
}
fn record(value: ParsedDateTime) -> Value {
    let (local, offset) = match value {
        ParsedDateTime::Aware(value) => (
            value.0.naive_local(),
            Some(value.0.offset().local_minus_utc()),
        ),
        ParsedDateTime::Naive(value) => (value, None),
    };
    json!({"calendar":[local.year(),local.month(),local.day(),local.hour(),local.minute(),local.second(),local.nanosecond()/1000],"offset_seconds":offset})
}
#[test]
fn frozen_pydantic_strings_preserve_calendar_offset_precision_and_categories() {
    let source = reference();
    assert_eq!(source["version"], "2.46.5");
    for case in source["cases"].as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let native = match parse(input) {
            Ok(value) => record(value),
            Err(error) => json!({"error":error.source_category()}),
        };
        assert_eq!(native, case["result"], "query input {input:?}");
    }
    // Python surrogate strings cannot enter this API's UTF-8 &str domain.
    assert_eq!(source["source_only"].as_array().unwrap().len(), 2);
    for case in source["source_only"].as_array().unwrap() {
        assert_eq!(case["error"], "string_unicode");
    }
}
#[test]
fn bounded_since_until_mapping_matches_actual_fastapi_order_and_last_value() {
    let source = reference();
    for case in source["framework"].as_array().unwrap() {
        let query = case["query"].as_array().unwrap();
        let mut body = serde_json::Map::new();
        let mut paths = Vec::new();
        for name in ["since", "until"] {
            let value = query
                .iter()
                .rev()
                .find(|pair| pair[0] == name)
                .map(|pair| pair[1].as_str().unwrap());
            let parsed = match value {
                None => Value::Null,
                Some(input) => {
                    if let Ok(value) = parse(input) {
                        record(value)
                    } else {
                        paths.push(format!("query/{name}"));
                        Value::Null
                    }
                }
            };
            body.insert(name.to_owned(), parsed);
        }
        let (status, body) = if paths.is_empty() {
            (200, Value::Object(body))
        } else {
            (422, json!({"code":"validation_failed","paths":paths}))
        };
        assert_eq!(status, case["status"].as_u64().unwrap());
        assert_eq!(body, case["body"]);
    }
}
