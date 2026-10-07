//! Validation limits preserve array order without interpreting numeric object keys.
use cannery_core::{
    contracts::{
        ContractKind, ContractValidator, instance::ProjectValidator, project_schema_violations,
    },
    json,
};
use num_bigint::BigInt;
use serde_json::{Value, json};
use std::error::Error;
type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

fn instance_paths(schema: Value, value: Value, limit: usize) -> Result<Vec<String>> {
    let validator = ProjectValidator::new(&json::from_value(schema)?)?;
    Ok(validator
        .violations(&json::from_value(value)?, &BigInt::from(limit))?
        .into_iter()
        .map(|error| error.path)
        .collect())
}

#[test]
fn array_errors_are_numeric_before_the_limit_is_applied() -> Result {
    let schema =
        json!({"type":"array","items":{"type":"object","properties":{"value":{"type":"integer"}}}});
    let value = Value::Array(vec![json!({"value":"private"}); 13]);
    assert_eq!(
        instance_paths(schema, value, 5)?,
        ["/0/value", "/1/value", "/2/value", "/3/value", "/4/value"]
    );
    Ok(())
}

#[test]
fn numeric_object_keys_keep_lexical_order() -> Result {
    let schema = json!({"type":"object","additionalProperties":{"type":"integer"}});
    let value = json!({"0":"bad","1":"bad","01":"bad","2":"bad","10":"bad","11":"bad"});
    assert_eq!(
        instance_paths(schema, value, 20)?,
        ["/0", "/01", "/1", "/10", "/11", "/2"]
    );
    Ok(())
}

#[test]
fn pointer_escapes_resolve_array_parents_and_order_actual_keys() -> Result {
    let bad = Value::Array(vec![json!("private"); 13]);
    let schema =
        json!({"type":"object","properties":{"a/b~c":{"type":"array","items":{"type":"integer"}}}});
    assert_eq!(
        instance_paths(schema, json!({"a/b~c":bad}), 5)?,
        [
            "/a~1b~0c/0",
            "/a~1b~0c/1",
            "/a~1b~0c/2",
            "/a~1b~0c/3",
            "/a~1b~0c/4"
        ]
    );
    let schema = json!({"type":"object","additionalProperties":{"type":"integer"}});
    assert_eq!(
        instance_paths(schema, json!({"~1":"bad","/":"bad","":"bad"}), 20)?,
        ["/", "/~1", "/~01"]
    );
    Ok(())
}

#[test]
fn embedded_policy_errors_share_array_order() -> Result {
    let unsupported = Value::Array(vec![json!({"format":"email"}); 13]);
    let schema = json!({"anyOf":unsupported});
    let paths: Vec<_> = project_schema_violations(&json::from_value(schema.clone())?)?
        .into_iter()
        .map(|error| error.path)
        .collect();
    assert_eq!(
        paths,
        (0..13)
            .map(|index| format!("/anyOf/{index}/format"))
            .collect::<Vec<_>>()
    );
    let validator = ContractValidator::new()?;
    let interface = json::from_value(json!({"name":"probe","version":1,"schema":schema}))?;
    let paths: Vec<_> = validator
        .document_violations(ContractKind::Interface, &interface)?
        .into_iter()
        .map(|error| error.path)
        .collect();
    assert_eq!(
        paths,
        (0..13)
            .map(|index| format!("/schema/anyOf/{index}/format"))
            .collect::<Vec<_>>()
    );
    Ok(())
}
