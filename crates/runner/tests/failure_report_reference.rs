//! Source payload observations before the required HTTP encoder boundary.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::json::{decode_str, encode_ascii_pretty};
use cannery_runner::failure_report::{Failure, reports};
use serde_json::Value;

fn reason(recipe: &str) -> String {
    let points = match recipe {
        "short" => "failure".chars().map(u32::from).collect(),
        "ascii" => vec![97; 6001],
        "astral" => vec![0x10000; 6001],
        "surrogate-boundary" => {
            let mut points = vec![97; 5999];
            points.extend([0xd800, 0xdc00, 122]);
            points
        }
        "linebreaks" => [10, 13, 0].repeat(2001),
        _ => panic!("unknown fixture recipe"),
    };
    cannery_core::text::from_codepoints(points).unwrap()
}

#[test]
fn actual_failure_payload_order_coercion_truncation_and_fallback() {
    let reference: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/runner/tests/fixtures/failure_report_reference.json"
    ))
    .unwrap();
    assert_eq!(reference["format"], 1);
    assert_eq!(reference["python"], "3.13.11");
    assert_eq!(reference["count"], 45);
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 45);
    for (index, case) in cases.iter().enumerate() {
        if case["reason"] == "surrogate-boundary" {
            assert!(cannery_core::text::from_codepoints(vec![0xd800, 0xdc00]).is_none());
            continue;
        }
        let reason = reason(case["reason"].as_str().unwrap());
        let logs = decode_str(&case["logs"].to_string(), 1000).unwrap();
        let step = case["step"].as_str().map(String::from);
        let actual = reports(&Failure {
            job_id: &String::from("fixture"),
            code: &String::from("step_error"),
            reason: &reason,
            step: step.as_ref(),
            logs: Some(&logs),
        })
        .unwrap();
        let actual: Vec<_> = actual
            .iter()
            .map(|report| {
                serde_json::from_str::<Value>(&encode_ascii_pretty(report, 1000).unwrap()).unwrap()
            })
            .collect();
        let expected: Vec<_> = case["expected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| serde_json::from_str::<Value>(value.as_str().unwrap()).unwrap())
            .collect();
        assert_eq!(actual, expected, "actual source recipe {index}");
    }
}

#[test]
fn native_unicode_truncation_preserves_scalars_and_bare_fallback() {
    let reason = format!("{}𐀀z", "a".repeat(5999));
    let job_id = String::from("job");
    let code = String::from("step_error");
    let step = String::from("step");
    let logs = decode_str(r#"[{"name":"stderr"}]"#, 128).unwrap();
    let reports = reports(&Failure {
        job_id: &job_id,
        code: &code,
        reason: &reason,
        step: Some(&step),
        logs: Some(&logs),
    })
    .unwrap();
    assert_eq!(reports.len(), 2);
    for (index, report) in reports.iter().enumerate() {
        let value: Value =
            serde_json::from_str(&encode_ascii_pretty(report, 128).unwrap()).unwrap();
        assert_eq!(value["reason"].as_str().unwrap().chars().count(), 6000);
        assert!(value["reason"].as_str().unwrap().ends_with('𐀀'));
        if index == 0 {
            assert_eq!(value["step"], "step");
            assert_eq!(value["logs"], serde_json::json!([{"name":"stderr"}]));
        } else {
            assert!(value.get("step").is_none());
            assert_eq!(value["logs"], serde_json::json!([]));
        }
    }
}
