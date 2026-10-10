//! Application chart recipes checked against native model and JSON contracts.
#![forbid(unsafe_code)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::{ids::AttemptId, json, timestamps::Timestamp};
use cannery_metrics::{numeric::PgNumeric, projection, repo::Point};
use serde_json::Value;
use std::error::Error;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
fn required<'a>(row: &'a Value, key: &str) -> Result<&'a Value> {
    row.get(key).ok_or_else(|| "fixture field missing".into())
}
fn string(row: &Value, key: &str) -> Result<String> {
    required(row, key)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "fixture string".into())
}
fn optional_string(row: &Value, key: &str) -> Result<Option<String>> {
    let v = required(row, key)?;
    if v.is_null() {
        Ok(None)
    } else {
        Ok(Some(v.as_str().ok_or("fixture string")?.to_owned()))
    }
}
fn number(row: &Value, key: &str) -> Result<i64> {
    required(row, key)?
        .as_i64()
        .ok_or_else(|| "fixture integer".into())
}
fn float(row: &Value, key: &str) -> Result<Option<f64>> {
    let value = required(row, key)?;
    if value.is_null() {
        return Ok(None);
    }
    if let Some(bits) = value.get("float_bits").and_then(Value::as_str) {
        return Ok(Some(f64::from_bits(u64::from_str_radix(bits, 16)?)));
    }
    if let Some(text) = value.get("float").and_then(Value::as_str) {
        return Ok(Some(text.parse()?));
    }
    Ok(Some(value.as_f64().ok_or("fixture float")?))
}
fn timestamp(row: &Value, key: &str) -> Result<Option<Timestamp>> {
    let value = required(row, key)?;
    if value.is_null() {
        return Ok(None);
    }
    Ok(Some(
        value
            .get("datetime")
            .unwrap_or(value)
            .as_str()
            .ok_or("fixture datetime")?
            .parse()?,
    ))
}
fn document(value: &Value) -> Result<json::Document> {
    Ok(json::decode_str(&serde_json::to_string(value)?, 128)?)
}
fn optional_document(row: &Value, key: &str) -> Result<Option<json::Document>> {
    let value = required(row, key)?;
    if value.is_null() {
        Ok(None)
    } else {
        Ok(Some(document(value)?))
    }
}
fn point(default: &Value, overrides: &Value, index: i32) -> Result<Point> {
    let mut row = default.clone();
    let fields = row.as_object_mut().ok_or("fixture point")?;
    fields.insert("id".into(), Value::from(index));
    fields.insert("unit_number".into(), Value::from(index));
    fields.insert(
        "value".into(),
        serde_json::json!({"float_bits":format!("{:016x}",f64::from(index).to_bits())}),
    );
    for (key, value) in overrides.as_object().ok_or("fixture overrides")? {
        fields.insert(key.clone(), value.clone());
    }
    let sample = required(&row, "sample_count")?;
    let sample_count = if sample.is_null() {
        None
    } else {
        Some(
            sample["decimal"]
                .as_str()
                .ok_or("fixture decimal")?
                .parse::<PgNumeric>()?,
        )
    };
    Ok(Point {
        id: number(&row, "id")?,
        attempt_id: AttemptId(format!("00000000-0000-0000-0000-{index:012}").parse()?),
        unit_number: i32::try_from(number(&row, "unit_number")?)?,
        unit_title: string(&row, "unit_title")?,
        unit_state: string(&row, "unit_state")?,
        attempt_sequence: i32::try_from(number(&row, "attempt_sequence")?)?,
        attempt_state: string(&row, "attempt_state")?,
        science_revision: i32::try_from(number(&row, "science_revision")?)?,
        claimed_at: timestamp(&row, "claimed_at")?.ok_or("fixture claimed clock")?,
        submitted_at: timestamp(&row, "submitted_at")?,
        finished_at: timestamp(&row, "finished_at")?,
        track_slug: string(&row, "track_slug")?,
        track_title: string(&row, "track_title")?,
        metric: string(&row, "metric")?,
        split: string(&row, "split")?,
        dimensions: document(required(&row, "dimensions")?)?,
        value: float(&row, "value")?,
        missing_reason: optional_string(&row, "missing_reason")?,
        unit: string(&row, "unit")?,
        direction: string(&row, "direction")?,
        sample_count,
        control_value: float(&row, "control_value")?,
        uncertainty_method: optional_string(&row, "uncertainty_method")?,
        uncertainty_lower: float(&row, "uncertainty_lower")?,
        uncertainty_upper: float(&row, "uncertainty_upper")?,
        authority: string(&row, "authority")?,
        source_ref: optional_string(&row, "source_ref")?,
        recorded_at: timestamp(&row, "recorded_at")?.ok_or("fixture recorded clock")?,
        control: optional_document(&row, "control")?,
        project_fields: optional_document(&row, "project_fields")?,
        reference_value: float(&row, "reference_value")?,
        reference_label: optional_string(&row, "reference_label")?,
        reference_kind: optional_string(&row, "reference_kind")?,
        reference_ref: optional_string(&row, "reference_ref")?,
    })
}
fn error_class(error: projection::Error) -> &'static str {
    match error {
        projection::Error::Validation => "ValidationError",
        projection::Error::Value => "ValueError",
        projection::Error::Overflow => "OverflowError",
        projection::Error::Attribute => "AttributeError",
        projection::Error::Type => "TypeError",
        projection::Error::Key => "KeyError",
        projection::Error::Recursion => "RecursionError",
        projection::Error::Model(_) => "PydanticSerializationError",
    }
}
fn native_warnings(case: &Value) -> Result<Value> {
    let mut expected = case["outcome"]["ok"][1].clone();
    if let Some(warnings) = expected.as_array_mut() {
        for warning in warnings {
            if warning
                .as_str()
                .is_some_and(|text| text.starts_with("x field "))
            {
                let field = case["recipe"]["view"]["x"]
                    .as_str()
                    .ok_or("unsupported x field")?;
                *warning = Value::String(format!(
                    "x field {} is not supported; x is null",
                    serde_json::to_string(field)?
                ));
            }
        }
    }
    Ok(expected)
}
#[test]
fn production_series_models_and_structural_responses() -> Result<()> {
    let source: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/metrics/tests/fixtures/chart_reference.json"
    ))?;
    let mut compared = 0;
    for case in source["cases"].as_array().ok_or("fixture cases")? {
        let recipe = &case["recipe"];
        if recipe["operation"] != "series" {
            continue;
        }
        compared += 1;
        let rows = recipe["rows"]
            .as_array()
            .ok_or("fixture rows")?
            .iter()
            .enumerate()
            .map(|(index, row)| point(&source["default_point"], row, i32::try_from(index + 1)?))
            .collect::<Result<Vec<_>>>()?;
        let view = document(&recipe["view"])?;
        let method = recipe["aggregation"].as_str().map(String::from);
        let outcome = cannery_metrics::series::series(&view, &rows, method.as_ref()).and_then(
            |(models, warnings)| {
                let wires = models
                    .iter()
                    .map(|model| {
                        model.bytes(projection::Context {
                            inferred_nesting_budget: 128,
                        })
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok((models, wires, warnings))
            },
        );
        match outcome {
            Ok((models, wires, warnings)) => {
                let expected = case["outcome"]["ok"][0]
                    .as_array()
                    .ok_or("expected series")?;
                assert_eq!(models.len(), expected.len(), "recipe {recipe}");
                assert_eq!(
                    serde_json::to_value(warnings)?,
                    native_warnings(case)?,
                    "recipe {recipe}"
                );
                for ((model, wire), expected) in models.iter().zip(wires).zip(expected) {
                    assert_eq!(
                        serde_json::from_slice::<Value>(&wire)?,
                        serde_json::from_str::<Value>(
                            expected["wire"].as_str().ok_or("series wire")?
                        )?,
                        "recipe {recipe}"
                    );
                    assert_eq!(
                        model.points.len(),
                        expected["fields"]["points"]
                            .as_array()
                            .ok_or("series points")?
                            .len()
                    );
                    for (point, expected) in model.points.iter().zip(
                        expected["fields"]["points"]
                            .as_array()
                            .ok_or("series points")?,
                    ) {
                        assert_numeric(point.value, &expected["value"])?;
                        assert_numeric(point.control_value, &expected["control_value"])?;
                    }
                }
            }
            Err(error)
                if rows
                    .iter()
                    .any(|point| point.value.is_some_and(|value| !value.is_finite())) =>
            {
                assert_eq!(
                    error,
                    projection::Error::Value,
                    "nonfinite aggregation recipe {recipe}"
                );
            }
            Err(error) => assert_eq!(
                error_class(error),
                case["outcome"]["error"]
                    .as_str()
                    .ok_or("expected series error")?,
                "recipe {recipe}"
            ),
        }
    }
    assert_eq!(compared, 222);
    Ok(())
}
fn assert_numeric(actual: Option<f64>, expected: &Value) -> Result<()> {
    let expected = float(&serde_json::json!({"value": expected}), "value")?;
    match (actual, expected) {
        (None, None) => {}
        (Some(actual), Some(expected)) if expected.is_nan() => assert!(actual.is_nan()),
        (Some(actual), Some(expected)) if expected.is_infinite() => {
            assert!(actual.is_infinite());
            assert_eq!(actual.is_sign_negative(), expected.is_sign_negative());
        }
        (Some(actual), Some(expected)) => assert!(
            actual.is_finite() && (actual - expected).abs() <= expected.abs().max(1.0) * 1e-12,
            "expected {expected}, actual {actual}"
        ),
        _ => return Err("numeric presence changed".into()),
    }
    Ok(())
}
#[test]
fn production_point_models_and_structural_responses() -> Result<()> {
    let source: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/metrics/tests/fixtures/chart_reference.json"
    ))?;
    let mut compared = 0;
    for case in source["cases"].as_array().ok_or("fixture cases")? {
        if case["recipe"]["operation"] != "point" {
            continue;
        }
        compared += 1;
        let point = point(&source["default_point"], &case["recipe"]["rows"][0], 1)?;
        let outcome = projection::point_out(&point).and_then(|model| {
            model
                .bytes(projection::Context {
                    inferred_nesting_budget: 128,
                })
                .map(|bytes| (model, bytes))
        });
        match outcome {
            Ok((model, bytes)) => {
                assert_eq!(
                    serde_json::from_slice::<Value>(&bytes)?,
                    serde_json::from_str::<Value>(
                        case["outcome"]["ok"]["wire"]
                            .as_str()
                            .ok_or("expected model bytes")?
                    )?
                );
                let fields = &case["outcome"]["ok"]["fields"];
                assert_numeric(model.point.value, &fields["value"])?;
                assert_numeric(model.point.control_value, &fields["control_value"])?;
                assert_eq!(
                    model.sample_count.as_ref().map(ToString::to_string),
                    fields["sample_count"]
                        .as_i64()
                        .map(|value| value.to_string())
                );
                if let Some(reference) = &model.reference {
                    assert_numeric(Some(reference.value), &fields["reference"]["value"])?;
                } else {
                    assert!(fields["reference"].is_null());
                }
            }
            Err(error) => assert_eq!(
                error_class(error),
                case["outcome"]["error"]
                    .as_str()
                    .ok_or("expected error class")?
            ),
        }
    }
    assert_eq!(compared, 26);
    Ok(())
}
#[test]
fn production_derived_views() -> Result<()> {
    let source: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/metrics/tests/fixtures/chart_reference.json"
    ))?;
    let mut compared = 0;
    for case in source["cases"].as_array().ok_or("fixture cases")? {
        if case["recipe"]["operation"] != "derived" {
            continue;
        }
        compared += 1;
        let content = document(&case["recipe"]["science"])?;
        match cannery_metrics::views::derived_views(&content, 128) {
            Ok(result) => {
                let bytes = json::model::encode_inferred(&result, result.root(), 128)?;
                assert_eq!(
                    serde_json::from_slice::<Value>(&bytes)?,
                    case["outcome"]["ok"]
                );
            }
            Err(error) => assert_eq!(
                error_class(error),
                case["outcome"]["error"]
                    .as_str()
                    .ok_or("expected derived error")?
            ),
        }
    }
    assert_eq!(compared, 4);
    Ok(())
}

#[test]
fn native_chart_buckets_keep_counts_references_and_single_attempt_details() -> Result<()> {
    let source: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/metrics/tests/fixtures/chart_reference.json"
    ))?;
    let rows = [
        point(
            &source["default_point"],
            &serde_json::json!({"value":2,"attempt_sequence":1}),
            1,
        )?,
        point(
            &source["default_point"],
            &serde_json::json!({"value":4,"attempt_sequence":1}),
            2,
        )?,
    ];
    let view = document(
        &serde_json::json!({"x":"attempt.sequence","group_by":["track"],"baseline":"control"}),
    )?;
    let (buckets, warnings) =
        cannery_metrics::series::series(&view, &rows, Some(&"mean".to_owned()))?;
    assert_eq!(warnings, Vec::<String>::new());
    assert_eq!(buckets.len(), 1);
    assert_eq!(buckets[0].science_revision, 1);
    assert_eq!(buckets[0].points.len(), 1);
    let point = &buckets[0].points[0];
    assert_numeric(point.value, &serde_json::json!(3))?;
    assert_eq!(point.count, 2);
    assert_eq!(point.attempt_refs, ["#1.1", "#2.1"]);
    assert_numeric(point.control_value, &serde_json::json!(0.5))?;
    assert_eq!(point.reference_label, Some("Reference"));
    assert!(point.uncertainty.is_none());
    assert!(point.sample_count.is_none());
    let (buckets, warnings) = cannery_metrics::series::series(&view, &rows[..1], None)?;
    assert_eq!(warnings, Vec::<String>::new());
    assert_eq!(buckets[0].points[0].count, 1);
    assert_eq!(
        buckets[0].points[0]
            .sample_count
            .as_ref()
            .map(ToString::to_string),
        Some("2".to_owned())
    );
    assert_eq!(
        buckets[0].points[0]
            .uncertainty
            .as_ref()
            .map(|value| value.method),
        Some("bootstrap")
    );
    Ok(())
}

#[test]
fn native_chart_aggregation_rejects_nonfinite_values_while_point_wire_projects_null() -> Result<()>
{
    let source: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/metrics/tests/fixtures/chart_reference.json"
    ))?;
    let view = document(&serde_json::json!({"x":"attempt.sequence"}))?;
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut row = point(&source["default_point"], &serde_json::json!({}), 1)?;
        row.value = Some(value);
        row.reference_value = Some(value);
        let model = projection::point_out(&row)?;
        let wire: Value = serde_json::from_slice(&model.bytes(projection::Context {
            inferred_nesting_budget: 128,
        })?)?;
        assert!(wire["value"].is_null());
        assert!(wire["reference"]["value"].is_null());
        for method in [
            None,
            Some("mean"),
            Some("count"),
            Some("sum"),
            Some("median"),
            Some("min"),
            Some("max"),
        ] {
            let method = method.map(str::to_owned);
            assert!(matches!(
                cannery_metrics::series::series(&view, std::slice::from_ref(&row), method.as_ref()),
                Err(projection::Error::Value)
            ));
        }
    }
    let mut row = point(&source["default_point"], &serde_json::json!({}), 1)?;
    row.value = Some(f64::MAX);
    row.attempt_sequence = 1;
    let mut other = point(&source["default_point"], &serde_json::json!({}), 2)?;
    other.value = Some(f64::MAX);
    other.attempt_sequence = 1;
    assert!(matches!(
        cannery_metrics::series::series(&view, &[row, other], Some(&"sum".to_owned())),
        Err(projection::Error::Overflow)
    ));
    Ok(())
}
