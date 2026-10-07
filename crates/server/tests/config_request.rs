//! Authored configuration request contracts.
#![forbid(unsafe_code)]
use cannery_core::json;
use cannery_server::validation::{self, BodyInput, Parameter, ParameterValue};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn configuration_kind_is_a_closed_enum() -> TestResult {
    for kind in ["science", "dashboard"] {
        let document = json::decode_str(&serde_json::to_string(kind)?, 64)?;
        let ParameterValue::ConfigKind(actual) =
            validation::validate_parameter(&document, Parameter::ConfigKind)?
        else {
            return Err("unexpected parameter variant".into());
        };
        assert_eq!(actual, kind);
    }
    for raw in [r#""Science""#, r#""unknown""#, "null", "1", "true", "{}"] {
        let document = json::decode_str(raw, 64)?;
        assert!(
            validation::validate_parameter(&document, Parameter::ConfigKind).is_err(),
            "{raw}"
        );
    }
    Ok(())
}

#[test]
fn revision_accepts_signed_i64_query_values_without_float_or_boolean_coercion() -> TestResult {
    for value in [i64::MIN, -1, 0, 1, i64::MAX] {
        for raw in [
            value.to_string(),
            serde_json::to_string(&value.to_string())?,
        ] {
            let document = json::decode_str(&raw, 64)?;
            let ParameterValue::ConfigRevision(actual) =
                validation::validate_parameter(&document, Parameter::ConfigRevision)?
            else {
                return Err("unexpected parameter variant".into());
            };
            assert_eq!(actual, value.into());
        }
    }
    for raw in [
        "9223372036854775808",
        "-9223372036854775809",
        r#""9223372036854775808""#,
        r#""1.0""#,
        "1.0",
        "true",
        "false",
        "null",
        "[]",
    ] {
        let document = json::decode_str(raw, 64)?;
        assert!(
            validation::validate_parameter(&document, Parameter::ConfigRevision).is_err(),
            "{raw}"
        );
    }
    Ok(())
}

#[test]
fn before_is_optional_and_bounds_the_database_revision_cursor() -> TestResult {
    for (raw, expected) in [
        ("1", 1),
        ("2147483647", i32::MAX),
        (r#""1""#, 1),
        (r#""2147483647""#, i32::MAX),
    ] {
        let document = json::decode_str(raw, 64)?;
        let ParameterValue::ConfigBefore(actual) =
            validation::validate_parameter(&document, Parameter::ConfigBefore)?
        else {
            return Err("unexpected parameter variant".into());
        };
        assert_eq!(actual, Some(expected.into()));
    }
    let null = json::decode_str("null", 64)?;
    assert!(matches!(
        validation::validate_parameter(&null, Parameter::ConfigBefore)?,
        ParameterValue::ConfigBefore(None)
    ));
    for raw in [
        "0",
        "-1",
        "2147483648",
        "9223372036854775808",
        r#""0""#,
        "1.0",
        "true",
        "{}",
    ] {
        let document = json::decode_str(raw, 64)?;
        assert!(
            validation::validate_parameter(&document, Parameter::ConfigBefore).is_err(),
            "{raw}"
        );
    }
    Ok(())
}

#[test]
fn configuration_body_requires_an_object_and_preserves_its_content() -> TestResult {
    for raw in [
        "{}",
        r#"{"nested":{"list":[null,true,3,"text"]},"extra":false}"#,
    ] {
        let document = json::decode_str(raw, 64)?;
        let actual = validation::validate_document_body(BodyInput::Json(&document))?;
        assert!(std::ptr::eq(actual, &raw const document));
    }
    assert!(validation::validate_document_body(BodyInput::Missing).is_err());
    assert!(validation::validate_document_body(BodyInput::RawBytes).is_err());
    for raw in ["null", "[]", "1", "true", r#""text""#] {
        let document = json::decode_str(raw, 64)?;
        assert!(
            validation::validate_document_body(BodyInput::Json(&document)).is_err(),
            "{raw}"
        );
    }
    Ok(())
}
