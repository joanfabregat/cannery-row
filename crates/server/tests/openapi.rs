use serde_json::Value;

#[test]
fn generated_rest_contract_matches_the_committed_client_contract()
-> Result<(), Box<dyn std::error::Error>> {
    let committed: Value = serde_json::from_slice(cannery_server::OPENAPI)?;
    let generated = serde_json::to_value(cannery_server::generated_openapi())?;
    assert_eq!(
        generated["paths"], committed["paths"],
        "REST paths, parameters, request bodies, statuses and response schemas must stay synchronized"
    );
    assert_eq!(
        generated["components"], committed["components"],
        "every serde DTO schema must stay synchronized with the web client"
    );
    assert_eq!(
        generated, committed,
        "the complete generated OpenAPI document must stay synchronized"
    );
    assert_eq!(
        generated["paths"].as_object().ok_or("missing paths")?.len(),
        91
    );
    Ok(())
}

#[test]
fn request_and_response_contracts_reject_structural_drift() {
    use cannery_server::api_models::{CommentCreate, ProjectOut};
    assert!(
        serde_json::from_value::<CommentCreate>(serde_json::json!({"body_markdown": 7})).is_err()
    );
    assert!(
        serde_json::from_value::<CommentCreate>(
            serde_json::json!({"body_markdown": "comment", "unexpected": true})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<ProjectOut>(serde_json::json!({"id": "id", "slug": "project"}))
            .is_err()
    );
}
