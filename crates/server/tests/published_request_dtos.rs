//! Concrete request DTOs retain the published field shapes and project payloads.
use cannery_server::api_models::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{error::Error, fs, path::Path};

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

fn published_examples<T: DeserializeOwned + Serialize>(name: &str) -> Result<usize> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/contracts")
        .join(name)
        .join("valid");
    let mut count = 0;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let value: Value = serde_json::from_slice(&fs::read(entry.path())?)?;
            let request: T = serde_json::from_value(value.clone())
                .map_err(|error| format!("{}: {error}", entry.path().display()))?;
            assert_eq!(
                serde_json::to_value(request)?,
                value,
                "{}",
                entry.path().display()
            );
            count += 1;
        }
    }
    assert!(count > 0);
    Ok(count)
}

#[test]
fn every_fixed_envelope_roundtrips_published_valid_examples() -> Result {
    published_examples::<HumanDecisionRequest>("human_decision")?;
    published_examples::<TrackTransitionRequest>("track_transition")?;
    published_examples::<ArtifactManifestRequest>("artifact_manifest")?;
    published_examples::<JobCompletionRequest>("job_completion")?;
    published_examples::<JobFailureRequest>("job_failure")?;
    published_examples::<EvidenceEnvelopeRequest>("evidence_envelope")?;
    published_examples::<UnitCreate>("hypothesis")?;
    published_examples::<TrackCreateRequest>("track")?;
    published_examples::<StepManifestRequest>("step_manifest")?;
    published_examples::<ScienceRevisionRequest>("science_revision")?;
    published_examples::<DashboardRevisionRequest>("dashboard_views")?;
    published_examples::<ConfigRevisionRequest>("science_revision")?;
    published_examples::<ConfigRevisionRequest>("dashboard_views")?;
    Ok(())
}

#[test]
fn fixed_control_fields_and_nested_storage_reject_wrong_types_and_unknown_fields() {
    for revision in [
        json!(1.0),
        json!("1"),
        json!(true),
        json!(18_446_744_073_709_551_615_u64),
    ] {
        assert!(serde_json::from_value::<HumanDecisionRequest>(json!({"review_case_id":"case","evidence_revision":revision,"action":"promote","reason":"ready"})).is_err());
    }
    for value in [
        json!({"key":"a","title":"t","question":"q","intervention":"i","acceptance":{},"state":"queued"}),
        json!({"key":"a","title":"t","question":"q","intervention":"i","acceptance":{},"brief":null}),
        json!({"key":"a","title":"t","question":"q","intervention":"i","acceptance":[]}),
    ] {
        assert!(serde_json::from_value::<UnitCreate>(value).is_err());
    }
    for value in [
        json!({"schema_version":"0.2","job_id":"job","error_code":"step_error","reason":"failure","logs":[{"key":"log","size_bytes":1,"sha256":"hash","token":"private"}]}),
        json!({"schema_version":"0.2","job_id":"job","error_code":"step_error","reason":"failure","logs":{}}),
    ] {
        assert!(serde_json::from_value::<JobFailureRequest>(value).is_err());
    }
    assert!(serde_json::from_value::<ArtifactManifestRequest>(json!({"schema_version":"0.2","attempt_id":"attempt","objects":[{"role":"model","storage":{"backend":"local","bucket":"bucket","key":"key","unknown":1},"size_bytes":1,"sha256":"hash","media_type":"application/octet-stream"}]})).is_err());
}

#[test]
fn unregistered_view_metrics_are_explicit_empty_objects() -> Result {
    let metric: ViewMetric = serde_json::from_value(json!({}))?;
    assert!(matches!(metric, ViewMetric::Unregistered(_)));
    assert_eq!(serde_json::to_value(metric)?, json!({}));
    assert!(serde_json::from_value::<ViewMetric>(json!({"legacy":true})).is_err());
    Ok(())
}

#[test]
fn omission_is_preserved_and_nonnullable_optional_values_reject_null() -> Result {
    let request: HumanDecisionRequest = serde_json::from_value(
        json!({"review_case_id":"case","evidence_revision":2,"action":"promote","reason":"ready"}),
    )?;
    assert!(serde_json::to_value(request)?.get("supersedes").is_none());
    assert!(serde_json::from_value::<HumanDecisionRequest>(json!({"review_case_id":"case","evidence_revision":2,"action":"promote","reason":"ready","supersedes":null})).is_err());
    let mut request: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/contracts/job_completion/valid/tester.json"
    ))?;
    request["manifest"] = Value::Null;
    assert!(serde_json::from_value::<JobCompletionRequest>(request).is_err());
    Ok(())
}

#[test]
fn dynamic_project_values_and_number_spelling_survive_serde() -> Result {
    let mut request: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/contracts/evidence_envelope/valid/agent.json"
    ))?;
    request["extensions"] = json!({"custom":{"nested":[false,null,"é𐀀",{"counter":3}]}});
    let typed: EvidenceEnvelopeRequest = serde_json::from_value(request.clone())?;
    assert_eq!(serde_json::to_value(typed)?, request);
    Ok(())
}

#[test]
fn generated_handler_contracts_reference_the_same_concrete_dtos() -> Result {
    let generated = serde_json::to_value(cannery_server::generated_openapi())?;
    for (path, name) in [
        (
            "/api/projects/{slug}/jobs/{job_id}/completion",
            "JobCompletionRequest",
        ),
        (
            "/api/projects/{slug}/jobs/{job_id}/failure",
            "JobFailureRequest",
        ),
        (
            "/api/projects/{slug}/tracks/{track_slug}/transitions",
            "TrackTransitionRequest",
        ),
    ] {
        let reference = &generated["paths"][path]["post"]["requestBody"]["content"]["application/json"]
            ["schema"]["$ref"];
        assert_eq!(
            reference,
            &Value::String(format!("#/components/schemas/{name}"))
        );
    }
    for name in [
        "HumanDecisionRequest",
        "ArtifactManifestRequest",
        "JobCompletionRequest",
        "JobFailureRequest",
        "EvidenceEnvelopeRequest",
        "UnitCreate",
        "TrackCreateRequest",
        "StepManifestRequest",
        "ScienceRevisionRequest",
        "DashboardRevisionRequest",
    ] {
        let properties = generated["components"]["schemas"][name]["properties"]
            .as_object()
            .ok_or("concrete fields missing")?;
        assert!(!properties.is_empty(), "{name}");
    }
    Ok(())
}
