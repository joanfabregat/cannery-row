//! The launcher supplies a uniquely owned, Rust-migrated database.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode},
};
use cannery_core::{
    principal::{Scope, Secret, ServiceKind},
    settings::load_settings,
};
use cannery_identity::{
    models::TokenKind,
    repo::{self, LoginUser, NewServiceAccount, NewSession, NewToken},
    secrets::{self, PERSONAL_PREFIX, SERVICE_PREFIX, SESSION_PREFIX},
};
use cannery_projects::repo as projects;
use cannery_server::application;
use serde_json::{Value, json};
use sqlx::Acquire;
use std::{collections::BTreeMap, error::Error};
use tower::ServiceExt;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

async fn request(
    app: &Router,
    method: Method,
    headers: HeaderMap,
) -> Result<(StatusCode, HeaderMap, Option<Value>)> {
    let mut request = Request::builder()
        .uri("/api/me")
        .method(method)
        .body(Body::empty())?;
    *request.headers_mut() = headers;
    let response = app.clone().oneshot(request).await?;
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, 16_384).await?;
    Ok((
        parts.status,
        parts.headers,
        if bytes.is_empty() {
            None
        } else {
            Some(serde_json::from_slice(&bytes)?)
        },
    ))
}

fn bearer(secret: &secrets::NewSecret) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {}", secret.plaintext().expose()))?,
    );
    Ok(headers)
}

#[tokio::test]
#[ignore = "requires a uniquely owned migrated PostgreSQL fixture"]
// One isolated setup exercises all three authentication channels and their
// precedence through the complete application, without replaying fixture writes.
#[allow(clippy::too_many_lines)]
async fn browser_personal_and_service_identity_use_actual_http_authentication() -> Result<()> {
    let uri = std::env::var("CANNERY_IDENTITY_TEST_DATABASE_URL")?;
    let settings = load_settings(
        None,
        &BTreeMap::from([("CANNERY_DATABASE_URL".into(), uri)]),
    )?;
    let (app, state) = application(settings)?;
    let mut connection = state
        .pool
        .acquire()
        .await
        .map_err(|_| "fixture connection failed")?;
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&mut *connection)
        .await
        .map_err(|_| "fixture ownership check failed")?;
    let suffix = database
        .strip_prefix("conformance_")
        .ok_or("fixture database is not isolated")?;
    if suffix.len() != 24 || !suffix.bytes().all(|value| value.is_ascii_hexdigit()) {
        return Err("fixture database is not isolated".into());
    }
    let mut transaction = connection
        .begin()
        .await
        .map_err(|_| "fixture transaction failed")?;
    let user = repo::upsert_login_user(
        &mut transaction,
        LoginUser {
            issuer: "https://fixture.identity",
            subject: "http-adapter",
            email: Some("http@fixture.test"),
            email_verified: true,
            display_name: Some("HTTP fixture"),
            make_admin: true,
        },
    )
    .await?
    .user;
    let session_secret = secrets::new_secret(SESSION_PREFIX)?;
    repo::create_session(
        &mut transaction,
        NewSession {
            user_id: user.id,
            secret_digest: session_secret.digest(),
            csrf_token: &Secret::new("fixture-csrf".into()),
            ttl_hours: 1,
        },
    )
    .await?;
    let personal_secret = secrets::new_secret(PERSONAL_PREFIX)?;
    repo::create_token(
        &mut transaction,
        NewToken {
            secret_digest: personal_secret.digest(),
            display_prefix: personal_secret.display_prefix(),
            kind: TokenKind::Personal,
            user_id: Some(user.id),
            service_account_id: None,
            name: "HTTP token",
            scopes: &[Scope::Read],
            expires_in_days: 1,
        },
    )
    .await?;
    let project = projects::create_project(
        &mut transaction,
        "http-project",
        "HTTP project",
        "",
        user.id,
    )
    .await?
    .ok_or("fixture project already exists")?;
    projects::set_membership(
        &mut transaction,
        project.id,
        user.id,
        cannery_core::principal::Role::Researcher,
        user.id,
    )
    .await?;
    let account = repo::create_service_account(
        &mut transaction,
        NewServiceAccount {
            project_id: project.id,
            kind: ServiceKind::Agent,
            name: "http-agent",
            description: "fixture",
            created_by: user.id,
        },
    )
    .await?;
    let service_secret = secrets::new_secret(SERVICE_PREFIX)?;
    repo::create_token(
        &mut transaction,
        NewToken {
            secret_digest: service_secret.digest(),
            display_prefix: service_secret.display_prefix(),
            kind: TokenKind::Service,
            user_id: None,
            service_account_id: Some(account.id),
            name: "HTTP service token",
            scopes: &[Scope::Write, Scope::Read],
            expires_in_days: 1,
        },
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|_| "fixture commit failed")?;
    drop(connection);

    let (status, headers, body) = request(&app, Method::GET, HeaderMap::new()).await?;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(headers["www-authenticate"], "Bearer");
    assert_eq!(
        body.ok_or("missing error body")?["error"]["code"],
        "unauthenticated"
    );

    let mut browser = HeaderMap::new();
    browser.append("cookie", HeaderValue::from_static("cr_session=obsolete"));
    browser.append(
        "cookie",
        HeaderValue::from_str(&format!(
            "cr_session={}",
            session_secret.plaintext().expose()
        ))?,
    );
    let (status, _, body) = request(&app, Method::GET, browser.clone()).await?;
    assert_eq!(status, StatusCode::OK);
    let body = body.ok_or("missing browser body")?;
    assert_eq!(body["kind"], "user");
    assert_eq!(body["channel"], "ui");
    assert_eq!(
        body["user"],
        json!({"id":user.id,"email":user.email,"email_verified":true,"display_name":user.display_name,"is_admin":true})
    );
    assert_eq!(
        body["memberships"],
        json!([{"project":"http-project","title":"HTTP project","role":"researcher"}])
    );
    assert_eq!(body["csrf_token"], "fixture-csrf");
    assert_eq!(body["scopes"], json!(["read", "write"]));
    assert!(body["user"].get("issuer").is_none());

    let (status, _, body) = request(&app, Method::GET, bearer(&personal_secret)?).await?;
    assert_eq!(status, StatusCode::OK);
    let body = body.ok_or("missing personal token body")?;
    assert_eq!(body["channel"], "api");
    assert_eq!(body["csrf_token"], Value::Null);
    assert_eq!(body["scopes"], json!(["read"]));

    let (status, _, body) = request(&app, Method::GET, bearer(&service_secret)?).await?;
    assert_eq!(status, StatusCode::OK);
    let body = body.ok_or("missing service body")?;
    assert_eq!(body["kind"], "service");
    assert_eq!(body["service_account"]["id"], json!(account.id));
    assert_eq!(body["service_account"]["project"], "http-project");
    assert!(body["service_account"].get("created_by").is_none());
    assert_eq!(body["memberships"], json!([]));
    assert_eq!(body["scopes"], json!(["read", "write"]));

    browser.append("authorization", HeaderValue::from_static("Basic invalid"));
    browser.append(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {}", personal_secret.plaintext().expose()))?,
    );
    let (status, _, _) = request(&app, Method::GET, browser).await?;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, headers, body) = request(&app, Method::HEAD, HeaderMap::new()).await?;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(headers["allow"], "GET");
    assert!(body.is_none());
    state.pool.close().await;
    Ok(())
}
