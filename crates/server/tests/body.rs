use axum::{
    body::{Body, to_bytes},
    http::{HeaderMap, HeaderValue, Request, StatusCode},
    response::IntoResponse,
};
use cannery_core::json::Node;
use cannery_server::body::{
    DecodedBody, REST_BODY_MAX_BYTES, REST_BODY_READ_TIMEOUT, REST_JSON_NESTING_BUDGET, is_json,
    read_body,
};
use futures_util::stream;
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tower::ServiceExt;

#[test]
fn content_type_recognizes_json_media_types_and_uses_first_header()
-> Result<(), Box<dyn std::error::Error>> {
    let cases = [
        ("application/json", true),
        (" Application/JSON ; charset=x", true),
        ("application / json", false),
        ("application/json/extra", false),
        ("application/+json", true),
        ("application/json(foo)", false),
        ("application/json;bad", true),
        ("application\t/json", false),
        ("application/ json", false),
        ("Application/Vnd.Test+Json", true),
        ("application/json,application/json", false),
        ("", false),
    ];
    for (value, expected) in cases {
        let mut headers = HeaderMap::new();
        headers.append("content-type", HeaderValue::from_str(value)?);
        headers.append("content-type", HeaderValue::from_static("application/json"));
        assert_eq!(is_json(&headers), expected, "type {value}");
    }
    assert!(!is_json(&HeaderMap::new()));
    Ok(())
}

async fn decode(
    bytes: Vec<u8>,
    content_type: Option<&str>,
) -> Result<Result<DecodedBody, cannery_server::body::BodyError>, Box<dyn std::error::Error>> {
    let mut request = Request::builder().method("POST").uri("/api/projects");
    if let Some(content_type) = content_type {
        request = request.header("content-type", content_type);
    }
    Ok(read_body(request.body(Body::from(bytes))?)
        .await
        .map(|(_, body)| body))
}

#[tokio::test]
async fn native_utf8_json_empty_and_raw_bodies_have_explicit_boundaries()
-> Result<(), Box<dyn std::error::Error>> {
    assert!(matches!(
        decode(Vec::new(), Some("application/json")).await??,
        DecodedBody::Missing
    ));
    assert!(matches!(
        decode(b"malformed".to_vec(), None).await??,
        DecodedBody::RawBytes
    ));
    let text = "{\"value\":1}";
    let mut utf16 = vec![0xff, 0xfe];
    for code in text.encode_utf16() {
        utf16.extend(code.to_le_bytes());
    }
    assert_validation_response(
        decode(utf16, Some("application/json; charset=unknown"))
            .await?
            .err()
            .ok_or("UTF16 accepted")?,
    )
    .await?;
    let DecodedBody::Json(document) = decode(
        text.as_bytes().to_vec(),
        Some("application/json; charset=unknown"),
    )
    .await??
    else {
        return Err("missing JSON document".into());
    };
    assert!(matches!(
        document.node(
            document
                .field(document.root(), "value")
                .ok_or("missing value")?
        ),
        Some(Node::Integer(_))
    ));
    Ok(())
}

async fn assert_validation_response(
    error: cannery_server::body::BodyError,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = error.into_response();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 4096).await?)?;
    assert_eq!(body["error"]["code"], "validation_failed");
    assert_eq!(body["error"]["message"], "request validation failed");
    let details = body["error"]["details"].as_array().ok_or("error details")?;
    assert_eq!(details.len(), 1);
    let path = details[0]["path"].as_str().ok_or("error path")?;
    assert!(
        path.strip_prefix("body/")
            .is_some_and(|position| position.parse::<usize>().is_ok())
    );
    assert_eq!(details[0]["message"], "JSON decode error");
    Ok(())
}

async fn assert_budget_response(
    error: cannery_server::body::BodyError,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = error.into_response();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        serde_json::from_slice::<Value>(&to_bytes(response.into_body(), 4096).await?)?,
        json!({"detail":"There was an error parsing the body"}),
    );
    Ok(())
}

#[tokio::test]
async fn exact_byte_cap_and_chunked_overflow_are_enforced_without_content_length()
-> Result<(), Box<dyn std::error::Error>> {
    for extra in [0, 1] {
        let mut bytes = vec![b'x'; REST_BODY_MAX_BYTES + extra];
        bytes[0] = b'"';
        let end = bytes.len() - 1;
        bytes[end] = b'"';
        let chunks: Vec<Result<Vec<u8>, Infallible>> =
            bytes.chunks(8191).map(|chunk| Ok(chunk.to_vec())).collect();
        let request = Request::builder()
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from_stream(stream::iter(chunks)))?;
        assert!(!request.headers().contains_key("content-length"));
        let result = read_body(request).await;
        if extra == 0 {
            let (_, body) = result?;
            assert!(matches!(body, DecodedBody::Json(_)));
        } else {
            assert_budget_response(result.err().ok_or("accepted overflow")?).await?;
        }
    }
    // A declared huge raw body has the same control-request memory bound.
    let error = decode(vec![b'x'; REST_BODY_MAX_BYTES + 1], None)
        .await?
        .err()
        .ok_or("accepted raw overflow")?;
    assert_budget_response(error).await
}

#[tokio::test]
async fn native_serde_recursion_boundary_has_sanitized_validation_failure()
-> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(REST_JSON_NESTING_BUDGET, 128);
    for depth in [
        REST_JSON_NESTING_BUDGET - 1,
        REST_JSON_NESTING_BUDGET,
        REST_JSON_NESTING_BUDGET + 1,
    ] {
        let input = format!("{}null{}", "[".repeat(depth), "]".repeat(depth));
        let result = decode(input.into_bytes(), Some("application/json")).await?;
        if depth < REST_JSON_NESTING_BUDGET {
            assert!(matches!(result?, DecodedBody::Json(_)));
        } else {
            assert_validation_response(result.err().ok_or("accepted nesting overflow")?).await?;
        }
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn progressing_body_cannot_reset_overall_deadline() -> Result<(), Box<dyn std::error::Error>>
{
    let delivered = Arc::new(AtomicUsize::new(0));
    let observed = delivered.clone();
    let chunks = stream::unfold(observed, |count| async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        count.fetch_add(1, Ordering::SeqCst);
        Some((Ok::<_, Infallible>(vec![b' ']), count))
    });
    let request = Request::builder()
        .header("content-type", "application/json")
        .body(Body::from_stream(chunks))?;
    let start = tokio::time::Instant::now();
    let result = read_body(request).await;
    assert_eq!(start.elapsed(), REST_BODY_READ_TIMEOUT);
    assert!((20..=30).contains(&delivered.load(Ordering::SeqCst)));
    assert_budget_response(result.err().ok_or("accepted endless body")?).await
}

#[tokio::test]
async fn real_token_route_refuses_budgets_and_syntax_before_database_access()
-> Result<(), Box<dyn std::error::Error>> {
    let env = std::collections::BTreeMap::from([(
        "CANNERY_DATABASE_URL".to_owned(),
        "postgresql://postgres@127.0.0.1:1/unused".to_owned(),
    )]);
    let settings = cannery_core::settings::load_settings(None, &env)?;
    let (app, state) = cannery_server::application(settings)?;
    // A closed pool makes authentication access fail immediately. Parsing
    // refusals must still win; a valid body proceeds to authentication.
    state.pool.close().await;
    for (bytes, expected) in [
        (vec![b'x'; REST_BODY_MAX_BYTES + 1], StatusCode::BAD_REQUEST),
        (b"{".to_vec(), StatusCode::UNPROCESSABLE_ENTITY),
        (b"{}".to_vec(), StatusCode::INTERNAL_SERVER_ERROR),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/tokens")
                    .header("content-type", "application/json")
                    .body(Body::from(bytes))?,
            )
            .await?;
        assert_eq!(response.status(), expected);
    }
    state.pool.close().await;
    Ok(())
}

#[tokio::test]
async fn native_json_syntax_and_unsupported_encodings_are_sanitized()
-> Result<(), Box<dyn std::error::Error>> {
    for (bytes, status, expected) in [
        (
            "{\"💩\": }".as_bytes().to_vec(),
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error":{"code":"validation_failed","message":"request validation failed","details":[{"path":"body/10","message":"JSON decode error"}]}}),
        ),
        (
            vec![0xff],
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error":{"code":"validation_failed","message":"request validation failed","details":[{"path":"body/1","message":"JSON decode error"}]}}),
        ),
    ] {
        let response = decode(bytes, Some("application/json"))
            .await?
            .err()
            .ok_or("expected parse failure")?
            .into_response();
        assert_eq!(response.status(), status);
        let bytes = to_bytes(response.into_body(), 4096).await?;
        assert_eq!(serde_json::from_slice::<Value>(&bytes)?, expected);
    }
    let overflow = decode(b"1e999".to_vec(), Some("application/json"))
        .await?
        .err()
        .ok_or("overflow accepted")?;
    assert_budget_response(overflow).await?;
    for input in [
        "NaN",
        "Infinity",
        "-Infinity",
        "\"\\ud800\"",
        "{\"private-synthetic-marker\":}",
    ] {
        let error = decode(input.as_bytes().to_vec(), Some("application/json"))
            .await?
            .err()
            .ok_or("unsupported JSON accepted")?;
        assert!(!format!("{error:?} {error}").contains("private-synthetic-marker"));
        assert_validation_response(error).await?;
    }
    Ok(())
}
