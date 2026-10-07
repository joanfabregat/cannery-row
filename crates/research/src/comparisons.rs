//! Comparison integrity shared by evaluator workers and publication controllers.
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Debug, thiserror::Error)]
pub enum ComparisonError {
    #[error("invalid comparison at {path}: {message}")]
    Invalid { path: String, message: &'static str },
    #[error("invalid comparison input shape")]
    Shape,
}
fn invalid(path: &str, message: &'static str) -> ComparisonError {
    ComparisonError::Invalid {
        path: path.to_owned(),
        message,
    }
}
fn array<'a>(value: &'a Value, key: &str) -> Result<&'a [Value], ComparisonError> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or(ComparisonError::Shape)
}
// Decimal equality must not lose precision through a binary float conversion.
fn decimal(value: &Value) -> Option<(num_bigint::BigInt, i64)> {
    let text = value.as_number()?.to_string();
    let (mantissa, exponent) = text.split_once(['e', 'E']).unwrap_or((&text, "0"));
    let exponent = exponent.parse::<i64>().ok()?;
    let fractional = mantissa.split_once('.').map_or(0, |(_, tail)| tail.len());
    let mut digits = mantissa.replace('.', "");
    let mut exponent = exponent.checked_sub(i64::try_from(fractional).ok()?)?;
    while digits.ends_with('0') {
        digits.pop();
        exponent = exponent.checked_add(1)?;
    }
    if digits.is_empty() || digits == "-" {
        return Some((0.into(), 0));
    }
    Some((digits.parse().ok()?, exponent))
}

/// Validate registered metric slices and citations against verified measurements.
/// The caller first validates the assessment's published JSON schema.
/// # Errors
/// Rejects unregistered/duplicate slices and incorrect or ambiguous tester citations.
#[allow(
    clippy::too_many_lines,
    reason = "Keep ordered comparison checks together"
)]
pub fn check(science: &Value, assessment: &Value, tested: &Value) -> Result<(), ComparisonError> {
    let mut seen = BTreeSet::new();
    for (index, comparison) in assessment["comparisons"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let path = format!("/evidence/assessment/comparisons/{index}");
        let metric = array(science, "metrics")?
            .iter()
            .find(|metric| metric["key"] == comparison["metric"])
            .ok_or_else(|| invalid(&format!("{path}/metric"), "unknown metric"))?;
        if !array(metric, "splits")?.contains(&comparison["split"]) {
            return Err(invalid(
                &format!("{path}/split"),
                "unregistered metric split",
            ));
        }
        for (name, value) in comparison["dimensions"].as_object().into_iter().flatten() {
            let dimension = metric["dimensions"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|dimension| dimension["name"].as_str() == Some(name))
                .ok_or_else(|| {
                    invalid(
                        &format!("{path}/dimensions/{name}"),
                        "unregistered metric dimension",
                    )
                })?;
            if dimension
                .get("values")
                .and_then(Value::as_array)
                .is_some_and(|values| !values.contains(value))
            {
                return Err(invalid(
                    &format!("{path}/dimensions/{name}"),
                    "unregistered dimension value",
                ));
            }
        }
        let key = serde_json::to_vec(&json!([
            comparison["metric"],
            comparison["split"],
            comparison["dimensions"]
        ]))
        .map_err(|_| ComparisonError::Shape)?;
        let key = cannery_core::json::decode(&key, 256).map_err(|_| ComparisonError::Shape)?;
        let key =
            cannery_core::json::canonical::bytes(&key, 256).map_err(|_| ComparisonError::Shape)?;
        if !seen.insert(key) {
            return Err(invalid(
                &path,
                "another comparison has the same metric, split and slice",
            ));
        }
        if comparison["source"] != "tester" {
            continue;
        }
        let mut cited = tested["measurements"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|measurement| {
                measurement["authority"] == "tester_verified"
                    && measurement["metric"] == comparison["metric"]
                    && measurement["split"] == comparison["split"]
                    && measurement.get("dimensions").map_or_else(
                        || {
                            comparison["dimensions"]
                                .as_object()
                                .is_some_and(serde_json::Map::is_empty)
                        },
                        |dimensions| dimensions == &comparison["dimensions"],
                    )
                    && measurement.get("value").is_some()
            });
        let expected = cited.next().ok_or_else(|| {
            invalid(
                &format!("{path}/source"),
                "source tester requires exactly one verified measurement",
            )
        })?;
        if cited.next().is_some() {
            return Err(invalid(
                &format!("{path}/source"),
                "source tester requires exactly one verified measurement",
            ));
        }
        if decimal(&expected["value"]).is_none()
            || decimal(&expected["value"]) != decimal(&comparison["value"])
        {
            return Err(invalid(
                &format!("{path}/value"),
                "does not match the verified tester value",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overall_measurement_without_dimensions_matches_only_an_empty_slice() {
        let science =
            json!({"metrics":[{"key":"m","splits":["dev"],"dimensions":[{"name":"region"}]}]});
        let measurement =
            json!({"authority":"tester_verified","metric":"m","split":"dev","value":0.42});
        let mut comparison =
            json!({"source":"tester","metric":"m","split":"dev","dimensions":{},"value":0.42});
        assert!(
            check(
                &science,
                &json!({"comparisons":[comparison.clone()]}),
                &json!({"measurements":[measurement.clone()]})
            )
            .is_ok()
        );
        comparison["dimensions"] = json!({"region":"west"});
        assert!(
            matches!(check(&science, &json!({"comparisons":[comparison]}), &json!({"measurements":[measurement.clone()]})), Err(ComparisonError::Invalid {path,..}) if path.ends_with("/source"))
        );
        let comparison =
            json!({"source":"tester","metric":"m","split":"dev","dimensions":{},"value":0.43});
        assert!(
            matches!(check(&science, &json!({"comparisons":[comparison]}), &json!({"measurements":[measurement]})), Err(ComparisonError::Invalid {path,..}) if path.ends_with("/value"))
        );
    }
    #[test]
    fn exact_citation_and_duplicate_dimension_identity() -> Result<(), Box<dyn std::error::Error>> {
        let science = json!({"metrics":[{"key":"m","splits":["dev"],"dimensions":[{"name":"a"},{"name":"b"}]}]});
        let cited: Value = serde_json::from_str(
            r#"{"authority":"tester_verified","metric":"m","split":"dev","dimensions":{"a":"x","b":"y"},"value":9007199254740993.00}"#,
        )?;
        let comparison: Value = serde_json::from_str(
            r#"{"source":"tester","metric":"m","split":"dev","dimensions":{"b":"y","a":"x"},"value":900719925474099300e-2}"#,
        )?;
        check(
            &science,
            &json!({"comparisons":[comparison.clone()]}),
            &json!({"measurements":[cited.clone()]}),
        )?;
        let mut altered = comparison.clone();
        altered["value"] = json!(9_007_199_254_740_992_u64);
        assert!(
            matches!(check(&science, &json!({"comparisons":[altered]}), &json!({"measurements":[cited.clone()]})), Err(ComparisonError::Invalid { path, .. }) if path.ends_with("/value"))
        );
        assert!(
            matches!(check(&science, &json!({"comparisons":[comparison.clone()]}), &json!({"measurements":[cited.clone(),cited.clone()]})), Err(ComparisonError::Invalid { path, .. }) if path.ends_with("/source"))
        );
        let mut reordered = comparison.clone();
        reordered["dimensions"] = json!({"a":"x","b":"y"});
        assert!(
            matches!(check(&science, &json!({"comparisons":[comparison,reordered]}), &json!({"measurements":[cited]})), Err(ComparisonError::Invalid { message, .. }) if message.starts_with("another comparison"))
        );
        Ok(())
    }
}
