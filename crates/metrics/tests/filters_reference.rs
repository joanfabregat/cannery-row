//! Supported application filter recipes and authored native UTF-8 contracts.
#![forbid(unsafe_code)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;

use cannery_metrics::filters::parse_filters;
use serde_json::{Value, json};
use std::error::Error;

fn text(value: &Value) -> Result<String, Box<dyn Error>> {
    let points = value
        .as_array()
        .ok_or("codepoints")?
        .iter()
        .map(|point| u32::try_from(point.as_u64().ok_or("codepoint")?).map_err(Into::into))
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    cannery_core::text::from_codepoints(points).ok_or_else(|| "invalid codepoint".into())
}

fn rejects_surrogates(case: &Value) -> Result<bool, Box<dyn Error>> {
    let invalid_points = case["inputs"]
        .as_array()
        .ok_or("inputs")?
        .iter()
        .map(|input| input.as_array().ok_or("codepoints"))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .filter(|point| (0xd800..=0xdfff).contains(point))
        .collect::<Vec<_>>();
    for point in &invalid_points {
        let wire = format!("\"\\u{point:04x}\"");
        assert!(
            cannery_core::json::decode_str(&wire, 128).is_err(),
            "surrogate filter recipe {} must reject at JSON input",
            case["id"]
        );
    }
    Ok(!invalid_points.is_empty())
}

#[test]
fn metric_filter_recipes_preserve_order_sets_and_errors_for_native_inputs()
-> Result<(), Box<dyn Error>> {
    let reference: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/metrics/tests/fixtures/filters_reference.json"
    ))?;
    let cases = reference["cases"].as_array().ok_or("cases")?;
    assert_eq!(cases.len(), 226);
    assert_eq!(reference["count"], cases.len());
    let mut rejected_unicode = 0;
    for case in cases {
        if rejects_surrogates(case)? {
            rejected_unicode += 1;
            continue;
        }
        let control_index = case["inputs"]
            .as_array()
            .ok_or("inputs")?
            .iter()
            .position(|input| {
                input.as_array().is_some_and(|points| {
                    points.iter().filter_map(Value::as_u64).any(|point| {
                        u32::try_from(point)
                            .ok()
                            .and_then(char::from_u32)
                            .is_some_and(char::is_control)
                    })
                })
            });
        let inputs = case["inputs"]
            .as_array()
            .ok_or("inputs")?
            .iter()
            .map(text)
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(control_index) = control_index {
            let source_error = case["error"]["paths"][0]
                .as_str()
                .and_then(|path| path.strip_prefix("filter/"))
                .map(str::parse::<usize>)
                .transpose()?;
            let expected_index =
                source_error.map_or(control_index, |index| index.min(control_index));
            assert_eq!(
                parse_filters(&inputs)
                    .err()
                    .ok_or("control filter accepted")?
                    .index,
                expected_index,
                "native control rejection recipe {}",
                case["id"]
            );
            continue;
        }
        match parse_filters(&inputs) {
            Ok(groups) => {
                let groups: Vec<Value> = groups
                    .into_iter()
                    .map(|(name, values)| {
                        let mut points: Vec<Vec<u32>> = values
                            .into_iter()
                            .map(|value| value.chars().map(u32::from).collect())
                            .collect();
                        points.sort();
                        json!([name, points])
                    })
                    .collect();
                assert_eq!(json!(groups), case["groups"], "{}", case["id"]);
            }
            Err(error) => assert_eq!(
                json!({"code":"validation_failed", "paths":[format!("filter/{}", error.index)]}),
                case["error"],
                "{}",
                case["id"],
            ),
        }
    }
    assert_eq!(rejected_unicode, 25);
    Ok(())
}

#[test]
fn native_filters_accept_utf8_values_and_enforce_dimension_and_error_constraints()
-> Result<(), Box<dyn Error>> {
    let inputs = [
        "language:français",
        "size:large",
        "language:日本語",
        "language:français",
        "source:https://example.test:443",
    ]
    .map(str::to_owned);
    assert_eq!(
        parse_filters(&inputs)?,
        vec![
            (
                "language".to_owned(),
                vec!["français".to_owned(), "日本語".to_owned()]
            ),
            ("size".to_owned(), vec!["large".to_owned()]),
            (
                "source".to_owned(),
                vec!["https://example.test:443".to_owned()]
            )
        ]
    );
    for value in [
        "",
        "no_colon",
        ":value",
        "Upper:value",
        "é:value",
        "1name:value",
        "name:",
        "name:one\ntwo",
        "name:value\n",
        "name:value\r",
        "name:\tvalue",
        "name:\0value",
        "name:value\u{7f}",
        "name:value\u{85}",
    ] {
        let inputs = [
            "valid:first".to_owned(),
            value.to_owned(),
            "valid:last".to_owned(),
        ];
        let error = parse_filters(&inputs)
            .err()
            .ok_or("invalid filter accepted")?;
        assert_eq!(error.index, 1);
        assert!(!format!("{error} {error:?}").contains(value) || value.is_empty());
    }
    let boundary = format!("{}:valid", "a".repeat(64));
    assert!(parse_filters([&boundary]).is_ok());
    let overflow = format!("{}:valid", "a".repeat(65));
    assert!(parse_filters([&overflow]).is_err());
    assert_eq!(parse_filters(std::iter::empty::<&String>())?, Vec::new());
    Ok(())
}
