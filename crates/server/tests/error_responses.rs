#![forbid(unsafe_code)]

use axum::{body::to_bytes, http::header, response::IntoResponse};
use cannery_core::errors::{DomainError, ErrorCode};
use cannery_server::errors::ApiError;
use serde_json::{Value, json};

#[tokio::test]
async fn authentication_and_store_failures_keep_protocol_headers()
-> Result<(), Box<dyn std::error::Error>> {
    for (code, status, name, value) in [
        (
            ErrorCode::Unauthenticated,
            401,
            header::WWW_AUTHENTICATE,
            "Bearer",
        ),
        (ErrorCode::StoreUnavailable, 503, header::RETRY_AFTER, "5"),
    ] {
        let error = DomainError::new(code, "public message");
        let response = if code == ErrorCode::StoreUnavailable {
            ApiError::RetryableStore(error)
        } else {
            ApiError::from(error)
        }
        .into_response();
        assert_eq!(response.status().as_u16(), status);
        assert_eq!(
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok()),
            Some(value)
        );
        let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 4096).await?)?;
        assert_eq!(body["error"]["details"], Value::Null);
        assert_eq!(body["error"]["message"], "public message");
        assert_eq!(body["error"]["code"], serde_json::to_value(code)?);
    }
    let response = ApiError::from(DomainError::new(
        ErrorCode::StoreUnavailable,
        "the object store is unavailable",
    ))
    .into_response();
    assert!(!response.headers().contains_key(header::RETRY_AFTER));
    Ok(())
}

#[tokio::test]
async fn validation_paths_remain_inside_the_existing_error_envelope()
-> Result<(), Box<dyn std::error::Error>> {
    let details = json!([{"path":"body/scopes/0", "message":"unknown scope"}]);
    let response = ApiError::from(
        DomainError::new(ErrorCode::ValidationFailed, "request validation failed")
            .with_details(details.clone()),
    )
    .into_response();
    assert_eq!(response.status().as_u16(), 422);
    assert!(!response.headers().contains_key(header::WWW_AUTHENTICATE));
    let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 4096).await?)?;
    assert_eq!(
        body,
        json!({"error":{"code":"validation_failed","message":"request validation failed","details":details}})
    );
    Ok(())
}
