#![allow(clippy::expect_used, clippy::panic)]

use cannery_core::principal::Secret;
use cannery_runner::runtime::{
    RuntimeError,
    http::{ApiClient, read_json},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

async fn response(body: String, declared: Option<usize>) -> reqwest::Response {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("owned listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("fixture connection");
        let mut request = [0; 4096];
        let amount = stream.read(&mut request).await.expect("fixture request");
        assert!(amount > 0);
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            declared.unwrap_or(body.len())
        );
        stream
            .write_all(header.as_bytes())
            .await
            .expect("response header");
        stream
            .write_all(body.as_bytes())
            .await
            .expect("response body");
    });
    let result = reqwest::get(format!("http://{address}"))
        .await
        .expect("fixture response");
    server.await.expect("fixture server settlement");
    result
}

#[tokio::test]
async fn api_json_has_byte_and_depth_limits() {
    let valid = read_json(response("{\"ok\":true}".to_owned(), None).await)
        .await
        .expect("bounded JSON");
    assert_eq!(valid["ok"], true);
    let oversized = read_json(response(String::new(), Some(16 * 1024 * 1024 + 1)).await).await;
    assert!(matches!(oversized, Err(RuntimeError::Contract)));
    let deep = format!("{}null{}", "[".repeat(130), "]".repeat(130));
    assert!(matches!(
        read_json(response(deep, None).await).await,
        Err(RuntimeError::Contract)
    ));
}

#[tokio::test]
async fn api_requests_reject_credential_and_foreign_origin_urls() {
    let client = ApiClient::new("http://127.0.0.1:1").expect("configured API");
    let token = Secret::new("synthetic-test-value".to_owned());
    for path in [
        "http://other.invalid/upload",
        "http://user@127.0.0.1:1/upload",
        "/upload#fragment",
    ] {
        let result = client
            .request(reqwest::Method::GET, path, &token, None, None)
            .await;
        assert!(matches!(result, Err(RuntimeError::Contract)));
    }
}
