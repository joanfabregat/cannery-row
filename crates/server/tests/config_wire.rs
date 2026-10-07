//! Typed configuration responses preserve ordinary JSON values and pagination.
#![forbid(unsafe_code)]
use cannery_core::{
    ids::{ProjectId, UserId},
    json,
    timestamps::Timestamp,
};
use cannery_research::config_repo::ConfigRevision;
use cannery_server::{
    api_models::{ConfigOut, Page_ConfigOut_int_},
    config_wire::{self, ResponseContext},
};
use serde_json::{Value, json as value};
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;
const CONTEXT: ResponseContext = ResponseContext {
    inferred_nesting_budget: 64,
};

fn science() -> Result<Value, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/contracts/science_revision/valid/external_evaluator_minimal.json"
    ))?)
}

fn dashboard() -> Result<Value, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/contracts/dashboard_views/valid/table_and_facets.json"
    ))?)
}

fn row(content: &str) -> Result<ConfigRevision, Box<dyn std::error::Error>> {
    Ok(ConfigRevision {
        project_id: ProjectId(Uuid::nil()),
        kind: "science".into(),
        revision: 7,
        science_revision: None,
        content: json::decode_str(content, 64)?,
        created_by: UserId(Uuid::from_u128(1)),
        created_at: Timestamp(chrono::DateTime::parse_from_rfc3339(
            "2026-01-01T00:00:00Z",
        )?),
    })
}

#[test]
fn configuration_response_has_typed_metadata_and_required_nullability() -> TestResult {
    let content = science()?;
    let science_row = row(&serde_json::to_string(&content)?)?;
    let bytes = config_wire::config_bytes(&science_row, CONTEXT)?;
    let actual: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(
        actual,
        value!({
            "kind":"science", "revision":7, "science_revision":null,
            "content":content,
            "created_by":"00000000-0000-0000-0000-000000000001",
            "created_at":"2026-01-01T00:00:00Z"
        })
    );
    let typed: ConfigOut = serde_json::from_slice(&bytes)?;
    assert_eq!(typed.revision, 7);
    assert_eq!(typed.science_revision, None);
    let dashboard_content = dashboard()?;
    let mut dashboard_row = row(&serde_json::to_string(&dashboard_content)?)?;
    dashboard_row.kind = "dashboard".into();
    dashboard_row.science_revision = Some(3);
    let typed: ConfigOut =
        serde_json::from_slice(&config_wire::config_bytes(&dashboard_row, CONTEXT)?)?;
    assert_eq!(typed.kind, "dashboard");
    assert_eq!(typed.science_revision, Some(3));
    assert_eq!(serde_json::to_value(typed.content)?, dashboard_content);
    Ok(())
}

#[test]
fn content_round_trips_json_strings_numbers_arrays_and_nested_objects() -> TestResult {
    let expected = value!({
        "text":"Monterey 🦦\n\"quoted\"\\path", "empty":"", "null":null,
        "integer":9_223_372_036_854_775_807_i64, "negative":-17, "fraction":1.25,
        "array":[true,false,null,{},[],{"nested":"é"}], "object":{"a":1,"b":2}
    });
    let mut content = science()?;
    // JSON Schema's const is an instance-valued boundary, so arbitrary project
    // JSON is valid here without weakening the fixed configuration envelope.
    content["hypothesis_fields"] = value!({"const":expected});
    let row = row(&serde_json::to_string(&content)?)?;
    let actual: ConfigOut = serde_json::from_slice(&config_wire::config_bytes(&row, CONTEXT)?)?;
    let actual = serde_json::to_value(actual.content)?;
    assert_eq!(actual, content);
    assert_eq!(actual["hypothesis_fields"]["const"], expected);
    Ok(())
}

#[test]
fn page_preserves_item_order_and_serializes_optional_cursor() -> TestResult {
    let mut content = science()?;
    content["tester"]["revision"] = value!("page-first");
    let first = row(&serde_json::to_string(&content)?)?;
    content["tester"]["revision"] = value!("page-second");
    let mut second = row(&serde_json::to_string(&content)?)?;
    second.revision = 6;
    let bytes = config_wire::page_bytes(&[first, second], Some(6), CONTEXT)?;
    let page: Page_ConfigOut_int_ = serde_json::from_slice(&bytes)?;
    assert_eq!(page.next_before, Some(6));
    assert_eq!(
        page.items
            .iter()
            .map(|item| item.revision)
            .collect::<Vec<_>>(),
        [7, 6]
    );
    for (item, revision) in page.items.iter().zip(["page-first", "page-second"]) {
        let content = serde_json::to_value(&item.content)?;
        assert_eq!(content["tester"]["revision"], revision);
    }
    let empty: Value = serde_json::from_slice(&config_wire::page_bytes(&[], None, CONTEXT)?)?;
    assert_eq!(empty, value!({"items":[], "next_before":null}));
    Ok(())
}

#[test]
fn stored_content_must_be_an_object() -> TestResult {
    for raw in ["null", "[]", "1", "true", r#""text""#] {
        assert!(
            config_wire::config_bytes(&row(raw)?, CONTEXT).is_err(),
            "{raw}"
        );
    }
    Ok(())
}

#[test]
fn stored_content_rejects_malformed_fixed_configuration_fields() -> TestResult {
    for content in [value!({}), value!({"enabled":true})] {
        assert!(
            config_wire::config_bytes(&row(&serde_json::to_string(&content)?)?, CONTEXT).is_err()
        );
    }
    let mut content = science()?;
    content["tester"]["revision"] = value!(false);
    assert!(config_wire::config_bytes(&row(&serde_json::to_string(&content)?)?, CONTEXT).is_err());
    let mut content = dashboard()?;
    content["views"] = value!({"id":"not-an-array"});
    let mut malformed_dashboard = row(&serde_json::to_string(&content)?)?;
    malformed_dashboard.kind = "dashboard".into();
    assert!(config_wire::config_bytes(&malformed_dashboard, CONTEXT).is_err());
    Ok(())
}
