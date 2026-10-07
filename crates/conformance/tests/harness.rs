use axum::{
    Json, Router,
    http::StatusCode,
    routing::{get, post},
};
use conformance::{Harness, Result};
use reqwest::Method;
use serde_json::json;

async fn server(router: Router) -> Result<(String, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok((url, task))
}

#[tokio::test]
async fn failed_status_and_schema_checks_never_add_coverage() -> Result<()> {
    let router = Router::new().route("/api/me",get(|| async { Json(json!({"not":"a MeOut"})) }))
        .route("/api/projects",get(|| async { (StatusCode::UNAUTHORIZED,Json(json!({"error":{"code":"unauthenticated","message":"login required","details":null}}))) }));
    let (url, task) = server(router).await?;
    let mut harness = Harness::new(&url)?;
    let response = harness.request(Method::GET, "/api/me")?.send().await?;
    assert!(
        harness
            .check_response(Method::GET, "/api/me", response, 401)
            .await
            .is_err()
    );
    let response = harness.request(Method::GET, "/api/me")?.send().await?;
    assert!(
        harness
            .check_response(Method::GET, "/api/me", response, 200)
            .await
            .is_err()
    );
    assert!(harness.coverage().operations.is_empty());
    let response = harness
        .request(Method::GET, "/api/projects")?
        .send()
        .await?;
    harness
        .check_response(Method::GET, "/api/projects", response, 401)
        .await?;
    assert_eq!(harness.coverage().operations["GET /api/projects"].len(), 1);
    let response = harness
        .request(Method::GET, "/api/projects")?
        .send()
        .await?;
    assert!(
        harness
            .check_response(Method::GET, "/api/me", response, 401)
            .await
            .is_err()
    );
    task.abort();
    Ok(())
}

#[tokio::test]
async fn tool_and_audit_failures_do_not_grant_coverage() -> Result<()> {
    let router = Router::new()
        .route("/mcp", post(|| async { Json(json!({"jsonrpc":"2.0","id":1,"result":{"isError":true,"content":[{"type":"text","text":"{}"}],"structuredContent":{}}})) }))
        .route("/__conformance/audit", get(|| async { Json(json!({"items":[{"action":"project.created"},{"action":null}],"next_after":2})) }));
    let (url, task) = server(router).await?;
    let mut harness = Harness::new(&url)?;
    assert!(
        harness
            .call_tool("test-token", "create_project", json!({}))
            .await
            .is_err()
    );
    assert!(harness.fetch_audit("test-token", 0).await.is_err());
    assert!(harness.coverage().tools.is_empty());
    assert!(harness.coverage().audit_actions.is_empty());
    task.abort();
    Ok(())
}

#[tokio::test]
async fn oidc_pkce_signed_token_and_code_replay() -> Result<()> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use ring::digest;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let issuer = format!("{origin}/oidc");
    let router = conformance::oidc::router(&issuer, "test-control")?;
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let client = reqwest::Client::new();
    let discovery: serde_json::Value = client
        .get(format!("{issuer}/.well-known/openid-configuration"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(discovery["issuer"], issuer);
    let verifier = "correct-test-verifier";
    let challenge = URL_SAFE_NO_PAD.encode(digest::digest(&digest::SHA256, verifier.as_bytes()));
    let mut url = reqwest::Url::parse(&format!("{issuer}/auth"))?;
    url.query_pairs_mut().extend_pairs([
        ("state", "test-state"),
        ("nonce", "test-nonce"),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("client_id", conformance::oidc::CLIENT_ID),
    ]);
    let approval = json!({"authorization_url":url.as_str(),"claims":{"sub":"test-user","email":"test@example.com"}});
    let denied = client
        .post(format!("{origin}/__conformance/approve"))
        .json(&approval)
        .send()
        .await?;
    assert_eq!(denied.status(), 401);
    let approved: serde_json::Value = client
        .post(format!("{origin}/__conformance/approve"))
        .bearer_auth("test-control")
        .json(&approval)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(approved["state"], "test-state");
    let form = [
        ("code", approved["code"].as_str().ok_or("code missing")?),
        ("code_verifier", verifier),
    ];
    let token_response = client
        .post(format!("{issuer}/token"))
        .basic_auth(
            conformance::oidc::CLIENT_ID,
            Some(conformance::oidc::CLIENT_SECRET),
        )
        .form(&form)
        .send()
        .await?;
    assert_eq!(token_response.status(), 200);
    let token: serde_json::Value = token_response.json().await?;
    let claims = verify_token(&client, &issuer, &token).await?;
    assert_eq!(claims["nonce"], "test-nonce");
    assert_eq!(claims["iss"], issuer);
    assert_eq!(claims["sub"], "test-user");
    let replay = client
        .post(format!("{issuer}/token"))
        .basic_auth(
            conformance::oidc::CLIENT_ID,
            Some(conformance::oidc::CLIENT_SECRET),
        )
        .form(&form)
        .send()
        .await?;
    assert_eq!(replay.status(), 400);
    let approved: serde_json::Value = client
        .post(format!("{origin}/__conformance/approve"))
        .bearer_auth("test-control")
        .json(&approval)
        .send()
        .await?
        .json()
        .await?;
    let rejected = client
        .post(format!("{issuer}/token"))
        .basic_auth(
            conformance::oidc::CLIENT_ID,
            Some(conformance::oidc::CLIENT_SECRET),
        )
        .form(&[
            ("code", approved["code"].as_str().ok_or("code missing")?),
            ("code_verifier", "wrong"),
        ])
        .send()
        .await?;
    assert_eq!(rejected.status(), 400);
    let error: serde_json::Value = rejected.json().await?;
    assert_eq!(error["detail"], "pkce");
    task.abort();
    Ok(())
}

async fn verify_token(
    client: &reqwest::Client,
    issuer: &str,
    token: &serde_json::Value,
) -> Result<serde_json::Value> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use ring::signature;
    let jwt = token["id_token"].as_str().ok_or("JWT missing")?;
    let parts: Vec<_> = jwt.split('.').collect();
    assert_eq!(parts.len(), 3);
    let claims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1])?)?;
    let jwks: serde_json::Value = client
        .get(format!("{issuer}/jwks"))
        .send()
        .await?
        .json()
        .await?;
    let mut public = vec![4];
    public.extend(URL_SAFE_NO_PAD.decode(jwks["keys"][0]["x"].as_str().ok_or("x missing")?)?);
    public.extend(URL_SAFE_NO_PAD.decode(jwks["keys"][0]["y"].as_str().ok_or("y missing")?)?);
    signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, public)
        .verify(
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &URL_SAFE_NO_PAD.decode(parts[2])?,
        )
        .map_err(|_| "invalid ES256 signature")?;
    Ok(claims)
}

#[tokio::test]
async fn opaque_download_bytes_are_preserved_without_relaxing_json_routes() -> Result<()> {
    let path = "/api/projects/test/artifacts/00000000-0000-0000-0000-000000000001";
    let router = Router::new()
        .route(
            path,
            get(|| async {
                (
                    [
                        ("content-type", "application/octet-stream"),
                        ("content-length", "3"),
                        (
                            "etag",
                            "\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"",
                        ),
                        ("x-content-type-options", "nosniff"),
                        ("content-security-policy", "default-src 'none'; sandbox"),
                        ("cache-control", "private, no-cache"),
                    ],
                    vec![0_u8, 255, 1],
                )
            }),
        )
        .route(
            "/api/me",
            get(|| async {
                (
                    [("content-type", "application/octet-stream")],
                    vec![0_u8, 255, 1],
                )
            }),
        );
    let (url, task) = server(router).await?;
    let mut harness = Harness::new(&url)?;
    let response = harness.request(Method::GET, path)?.send().await?;
    let checked = harness
        .check_response(
            Method::GET,
            "/api/projects/{slug}/artifacts/{artifact_id}",
            response,
            200,
        )
        .await?;
    assert_eq!(checked.raw_body, vec![0, 255, 1]);
    assert_eq!(checked.body, serde_json::Value::Null);
    let response = harness.request(Method::GET, "/api/me")?.send().await?;
    assert!(
        harness
            .check_response(Method::GET, "/api/me", response, 200)
            .await
            .is_err()
    );
    assert!(!harness.coverage().operations.contains_key("GET /api/me"));
    task.abort();
    Ok(())
}
