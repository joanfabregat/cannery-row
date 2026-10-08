//! Full HTTP application checks use a uniquely guarded, migrated database.
#![forbid(unsafe_code)]

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use cannery_core::{
    principal::{Scope, ServiceKind},
    settings::load_settings,
};
use cannery_identity::{
    models::{TokenKind, User},
    repo::{self, LoginUser, NewServiceAccount, NewToken},
    secrets::{self, PERSONAL_PREFIX, SERVICE_PREFIX},
};
use cannery_server::application;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{collections::BTreeMap, error::Error};
use tower::ServiceExt;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
type StoredEvent = (String, String, Option<Value>, Option<Value>, String);

async fn call(
    app: &Router,
    method: Method,
    path: &str,
    token: Option<&str>,
    raw: Option<&str>,
) -> Result<(u16, Value)> {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    if raw.is_some() {
        request = request.header("content-type", "application/json");
    }
    let response = app
        .clone()
        .oneshot(request.body(raw.map_or_else(Body::empty, |body| Body::from(body.to_owned())))?)
        .await?;
    let status = response.status().as_u16();
    if status == 500 {
        assert_eq!(
            response.headers()["content-type"],
            "text/plain; charset=utf-8"
        );
    }
    let bytes = to_bytes(response.into_body(), 65536).await?;
    if status == 500 {
        assert_eq!(bytes.as_ref(), b"Internal Server Error");
        return Ok((status, Value::String("Internal Server Error".into())));
    }
    Ok((
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)?
        },
    ))
}

async fn expect(
    app: &Router,
    method: Method,
    path: &str,
    token: Option<&str>,
    raw: Option<&str>,
    status: u16,
) -> Result<Value> {
    let (actual, body) = call(app, method, path, token, raw).await?;
    assert_eq!(actual, status, "HTTP status for {path}");
    Ok(body)
}

async fn person(
    pool: &PgPool,
    subject: &str,
    admin: bool,
    scopes: &[Scope],
) -> Result<(User, String)> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|_| "fixture connection failed")?;
    let user = repo::upsert_login_user(
        &mut connection,
        LoginUser {
            issuer: "https://project-http.fixture",
            subject,
            email: Some(subject),
            email_verified: true,
            display_name: Some(subject),
            make_admin: admin,
        },
    )
    .await?
    .user;
    let secret = secrets::new_secret(PERSONAL_PREFIX)?;
    repo::create_token(
        &mut connection,
        NewToken {
            secret_digest: secret.digest(),
            display_prefix: secret.display_prefix(),
            kind: TokenKind::Personal,
            user_id: Some(user.id),
            service_account_id: None,
            name: "project fixture",
            scopes,
            expires_in_days: 1,
        },
    )
    .await?;
    Ok((user, secret.plaintext().expose().to_owned()))
}

async fn fixture() -> Result<(Router, PgPool)> {
    let uri = std::env::var("CANNERY_PROJECTS_TEST_DATABASE_URL")?;
    let settings = load_settings(
        None,
        &BTreeMap::from([("CANNERY_DATABASE_URL".into(), uri)]),
    )?;
    let (app, state) = application(settings)?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await
        .map_err(|_| "fixture guard failed")?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("isolated fixture required")?;
    if suffix.len() != 24
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err("isolated fixture required".into());
    }
    Ok((app, state.pool))
}

#[tokio::test]
#[ignore = "requires uniquely owned migrated PostgreSQL"]
#[allow(clippy::too_many_lines)]
async fn project_membership_http_preserves_visibility_pagination_and_audit() -> Result<()> {
    let (app, pool) = fixture().await?;
    let (admin, admin_token) =
        person(&pool, "admin-projects", true, &[Scope::Read, Scope::Write]).await?;
    let (alice, alice_token) =
        person(&pool, "alice-projects", false, &[Scope::Read, Scope::Write]).await?;
    let (bob, _) = person(&pool, "bob-projects", false, &[Scope::Read]).await?;
    let created = expect(
        &app,
        Method::POST,
        "/api/projects",
        Some(&admin_token),
        Some(r#"{"slug":"alpha","title":" Alpha ","description":"public","tracks":[{"slug":"main","title":"Main"}]}"#),
        201,
    )
    .await?;
    assert_eq!(created["title"], "Alpha");
    assert_eq!(created["role"], Value::Null);
    assert!(
        created["created_at"]
            .as_str()
            .ok_or("public project time")?
            .ends_with('Z')
    );
    assert_eq!(created.as_object().ok_or("project object")?.len(), 6);
    assert!(created.get("created_by").is_none());
    expect(
        &app,
        Method::POST,
        "/api/projects",
        Some(&admin_token),
        Some(r#"{"slug":"alpha","title":"Duplicate","tracks":[{"slug":"main","title":"Main"}]}"#),
        409,
    )
    .await?;
    expect(
        &app,
        Method::POST,
        "/api/projects",
        Some(&admin_token),
        Some(r#"{"slug":"beta","title":"Beta","tracks":[{"slug":"main","title":"Main"}]}"#),
        201,
    )
    .await?;
    // Installation administrators list everything but do not bypass GET project access.
    expect(
        &app,
        Method::GET,
        "/api/projects/alpha",
        Some(&admin_token),
        None,
        404,
    )
    .await?;
    expect(
        &app,
        Method::GET,
        "/api/projects/alpha",
        Some(&alice_token),
        None,
        404,
    )
    .await?;
    let first = expect(
        &app,
        Method::GET,
        "/api/projects?limit=1",
        Some(&admin_token),
        None,
        200,
    )
    .await?;
    assert_eq!(first["items"][0]["slug"], "alpha");
    assert_eq!(first["items"][0]["created_at"], created["created_at"]);
    assert_eq!(first["next_before"], "alpha");
    let second = expect(
        &app,
        Method::GET,
        "/api/projects?before=alpha&limit=1",
        Some(&admin_token),
        None,
        200,
    )
    .await?;
    assert_eq!(second["items"][0]["slug"], "beta");
    assert_eq!(second["next_before"], Value::Null);
    let alice_path = format!("/api/projects/alpha/members/{}", alice.id);
    let bob_path = format!("/api/projects/alpha/members/{}", bob.id);
    let mut previous_grant = Value::Null;
    for role in ["viewer", "viewer", "researcher"] {
        let body = format!(r#"{{"role":"{role}"}}"#);
        let member = expect(
            &app,
            Method::PUT,
            &alice_path,
            Some(&admin_token),
            Some(&body),
            200,
        )
        .await?;
        assert_eq!(member["role"], role);
        assert!(
            member["granted_at"]
                .as_str()
                .ok_or("public grant time")?
                .ends_with('Z')
        );
        assert_ne!(member["granted_at"], previous_grant);
        previous_grant = member["granted_at"].clone();
        assert_eq!(member.as_object().ok_or("member object")?.len(), 5);
        assert!(member.get("granted_by").is_none());
    }
    let (first_grant, simultaneous_grant) = tokio::join!(
        expect(
            &app,
            Method::PUT,
            &bob_path,
            Some(&admin_token),
            Some(r#"{"role":"member"}"#),
            200
        ),
        expect(
            &app,
            Method::PUT,
            &bob_path,
            Some(&admin_token),
            Some(r#"{"role":"member"}"#),
            200
        ),
    );
    assert_eq!(first_grant?["role"], "member");
    assert_eq!(simultaneous_grant?["role"], "member");
    let visible = expect(
        &app,
        Method::GET,
        "/api/projects/alpha",
        Some(&alice_token),
        None,
        200,
    )
    .await?;
    assert_eq!(visible["role"], "researcher");
    assert_eq!(visible["created_at"], created["created_at"]);
    let mine = expect(
        &app,
        Method::GET,
        "/api/projects",
        Some(&alice_token),
        None,
        200,
    )
    .await?;
    assert_eq!(mine["items"].as_array().ok_or("items")?.len(), 1);
    let members = expect(
        &app,
        Method::GET,
        "/api/projects/alpha/members?limit=1",
        Some(&admin_token),
        None,
        200,
    )
    .await?;
    assert_eq!(members["items"][0]["user_id"], json!(alice.id));
    assert_eq!(members["next_before"], json!(alice.id));
    let path = format!("/api/projects/alpha/members?before={}&limit=1", alice.id);
    let members = expect(&app, Method::GET, &path, Some(&alice_token), None, 200).await?;
    assert_eq!(members["items"][0]["user_id"], json!(bob.id));
    assert_eq!(members["next_before"], Value::Null);
    let path = format!("/api/projects/alpha/members?before={}", admin.id);
    let invalid = expect(&app, Method::GET, &path, Some(&admin_token), None, 422).await?;
    assert_eq!(invalid["error"]["details"][0]["path"], "before");
    expect(
        &app,
        Method::PUT,
        &bob_path,
        Some(&alice_token),
        Some(r#"{"role":"viewer"}"#),
        403,
    )
    .await?;
    expect(
        &app,
        Method::GET,
        "/api/projects/beta/members",
        Some(&alice_token),
        None,
        404,
    )
    .await?;
    let removed = expect(
        &app,
        Method::DELETE,
        &alice_path,
        Some(&admin_token),
        None,
        204,
    )
    .await?;
    assert_eq!(removed, Value::Null);
    expect(
        &app,
        Method::DELETE,
        &alice_path,
        Some(&admin_token),
        None,
        404,
    )
    .await?;
    expect(
        &app,
        Method::GET,
        "/api/projects/alpha",
        Some(&alice_token),
        None,
        404,
    )
    .await?;
    let events: Vec<StoredEvent> = sqlx::query_as(
        "SELECT action,subject_id,prior_state,new_state,via_channel FROM audit_events ORDER BY seq",
    )
    .fetch_all(&pool)
    .await
    .map_err(|_| "audit fixture read failed")?;
    assert_eq!(
        events
            .iter()
            .map(|event| event.0.as_str())
            .collect::<Vec<_>>(),
        [
            "project.created",
            "track.created",
            "project.created",
            "track.created",
            "membership.set",
            "membership.set",
            "membership.set",
            "membership.removed"
        ]
    );
    // A project's first tracks are created with it, in the same transaction.
    assert_eq!(events[1].2, None);
    assert_eq!(
        events[1].3.as_ref().map(|state| state["title"].clone()),
        Some(json!("Main"))
    );
    assert_eq!(
        events[4].1,
        format!("{}:{}", created["id"].as_str().ok_or("id")?, alice.id)
    );
    assert_eq!(events[4].2, None);
    assert_eq!(events[4].3, Some(json!({"role":"viewer"})));
    assert_eq!(events[5].2, Some(json!({"role":"viewer"})));
    assert_eq!(events[5].3, Some(json!({"role":"researcher"})));
    assert_eq!(events[7].2, Some(json!({"role":"researcher"})));
    assert_eq!(events[7].3, None);
    assert!(events.iter().all(|event| event.4 == "api"));
    let actors: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT actor_kind, actor_user_id::text, actor_service_id::text FROM audit_events ORDER BY seq",
    )
    .fetch_all(&pool)
    .await
    .map_err(|_| "audit actor fixture read failed")?;
    assert!(actors.iter().all(|(kind, user, service)| kind == "user"
        && user == &admin.id.to_string()
        && service.is_none()));
    pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires uniquely owned migrated PostgreSQL"]
#[allow(clippy::too_many_lines)]
async fn project_validation_precedes_authorization_after_authentication() -> Result<()> {
    let (app, pool) = fixture().await?;
    let (admin, admin_token) = person(
        &pool,
        "admin-validation",
        true,
        &[Scope::Read, Scope::Write],
    )
    .await?;
    let (_, plain) = person(&pool, "user-validation", false, &[Scope::Read]).await?;
    let (_, read_only) = person(&pool, "readonly-validation", true, &[Scope::Read]).await?;
    // JSON surrogate escapes are rejected before authorization by the native decoder.
    let surrogate = r#"{"slug":"surrogate","title":"S","description":"\ud800","tracks":[{"slug":"main","title":"Main"}]}"#;
    for token in [&plain, &read_only, &admin_token] {
        let response = expect(
            &app,
            Method::POST,
            "/api/projects",
            Some(token),
            Some(surrogate),
            422,
        )
        .await?;
        assert_eq!(response["error"]["code"], "validation_failed");
    }
    // Valid JSON containing NUL reaches storage only after admin/write checks.
    let nul = r#"{"slug":"surrogate","title":"S","description":"\u0000","tracks":[{"slug":"main","title":"Main"}]}"#;
    for (token, status) in [(&plain, 403), (&read_only, 403), (&admin_token, 500)] {
        let response = expect(
            &app,
            Method::POST,
            "/api/projects",
            Some(token),
            Some(nul),
            status,
        )
        .await?;
        if status == 500 {
            assert_eq!(response, Value::String("Internal Server Error".into()));
        } else {
            assert_eq!(response["error"]["code"], "forbidden");
        }
        assert!(!response.to_string().contains("description encoding"));
    }
    let mut connection = pool
        .acquire()
        .await
        .map_err(|_| "fixture connection failed")?;
    assert!(
        cannery_projects::repo::get_project_by_slug(&mut connection, "surrogate")
            .await?
            .is_none()
    );
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action = 'project.created' AND new_state->>'slug' = 'surrogate'")
        .fetch_one(&mut *connection).await.map_err(|_| "surrogate audit fixture read failed")?;
    assert_eq!(events, 0);
    drop(connection);
    // Syntax fails before auth; typed model/path/query errors require successful auth.
    expect(&app, Method::POST, "/api/projects", None, Some("{"), 422).await?;
    expect(&app, Method::POST, "/api/projects", None, Some("{}"), 401).await?;
    expect(
        &app,
        Method::POST,
        "/api/projects",
        Some(&plain),
        Some("{}"),
        422,
    )
    .await?;
    expect(
        &app,
        Method::POST,
        "/api/projects",
        Some(&plain),
        Some(r#"{"slug":"v","title":"V","tracks":[{"slug":"main","title":"Main"}]}"#),
        403,
    )
    .await?;
    expect(
        &app,
        Method::POST,
        "/api/projects",
        Some(&read_only),
        Some(r#"{"slug":"v","title":"V","tracks":[{"slug":"main","title":"Main"}]}"#),
        403,
    )
    .await?;
    expect(&app, Method::GET, "/api/projects?limit=0", None, None, 401).await?;
    expect(
        &app,
        Method::GET,
        "/api/projects?limit=0",
        Some(&admin_token),
        None,
        422,
    )
    .await?;
    expect(
        &app,
        Method::GET,
        "/api/projects?limit=201",
        Some(&admin_token),
        None,
        422,
    )
    .await?;
    expect(
        &app,
        Method::GET,
        "/api/projects?limit=1&limit=bad",
        Some(&admin_token),
        None,
        422,
    )
    .await?;
    let failed = expect(
        &app,
        Method::PUT,
        "/api/projects/missing/members/not-uuid",
        Some(&plain),
        Some("{}"),
        422,
    )
    .await?;
    assert_eq!(failed["error"]["code"], "validation_failed");
    assert_eq!(
        failed["error"]["message"],
        "request body does not match its schema"
    );
    assert!(failed["error"]["details"].is_null());
    expect(
        &app,
        Method::DELETE,
        "/api/projects/missing/members/not-uuid",
        None,
        None,
        401,
    )
    .await?;
    expect(
        &app,
        Method::DELETE,
        "/api/projects/missing/members/not-uuid",
        Some(&admin_token),
        None,
        422,
    )
    .await?;
    // A project starts with at least one track, each with a unique slug.
    for (body, path) in [
        (r#"{"slug":"validation","title":"V"}"#, "body/tracks"),
        (
            r#"{"slug":"validation","title":"V","tracks":[]}"#,
            "body/tracks",
        ),
        (
            r#"{"slug":"validation","title":"V","tracks":{"slug":"main"}}"#,
            "body/tracks",
        ),
        (
            r#"{"slug":"validation","title":"V","tracks":[{"slug":"Main","title":"Main"}]}"#,
            "body/tracks/0/slug",
        ),
        (
            r#"{"slug":"validation","title":"V","tracks":[{"slug":"main","title":" "}]}"#,
            "body/tracks/0/title",
        ),
        (
            r#"{"slug":"validation","title":"V","tracks":[{"slug":"main"}]}"#,
            "body/tracks/0/title",
        ),
        (
            r#"{"slug":"validation","title":"V","tracks":[{"slug":"main","title":"M","mode":"workflow"}]}"#,
            "body/tracks/0/mode",
        ),
        (
            r#"{"slug":"validation","title":"V","tracks":[{"slug":"main","title":"A"},{"slug":"main","title":"B"}]}"#,
            "body/tracks/1/slug",
        ),
    ] {
        let response = expect(
            &app,
            Method::POST,
            "/api/projects",
            Some(&admin_token),
            Some(body),
            422,
        )
        .await?;
        assert_eq!(response["error"]["details"][0]["path"], path, "{body}");
    }
    expect(
        &app,
        Method::POST,
        "/api/projects",
        Some(&admin_token),
        Some(
            r#"{"slug":"validation","title":"V","tracks":[{"slug":"main","title":"Main"},{"slug":"second","title":"Second","description":"Another approach."}]}"#,
        ),
        201,
    )
    .await?;
    let mut connection = pool
        .acquire()
        .await
        .map_err(|_| "fixture connection failed")?;
    let stored: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT t.slug, t.title, t.description, t.mode FROM tracks t JOIN projects p ON p.id = t.project_id WHERE p.slug = 'validation' ORDER BY t.slug",
    )
    .fetch_all(&mut *connection)
    .await
    .map_err(|_| "track fixture read failed")?;
    assert_eq!(
        stored,
        [
            ("main".into(), "Main".into(), String::new(), "agent".into()),
            (
                "second".into(),
                "Second".into(),
                "Another approach.".into(),
                "agent".into()
            ),
        ]
    );
    drop(connection);
    let path = format!("/api/projects/missing/members/{}", admin.id);
    expect(
        &app,
        Method::PUT,
        &path,
        Some(&admin_token),
        Some(r#"{"role":"viewer"}"#),
        404,
    )
    .await?;
    let missing = "/api/projects/validation/members/00000000-0000-0000-0000-000000000000";
    expect(
        &app,
        Method::PUT,
        missing,
        Some(&admin_token),
        Some(r#"{"role":"viewer"}"#),
        404,
    )
    .await?;
    expect(&app, Method::DELETE, missing, Some(&admin_token), None, 404).await?;
    // A real service can list/read its own project but cannot inspect members.
    let mut connection = pool
        .acquire()
        .await
        .map_err(|_| "fixture connection failed")?;
    let project = cannery_projects::repo::get_project_by_slug(&mut connection, "validation")
        .await?
        .ok_or("project")?;
    let account = repo::create_service_account(
        &mut connection,
        NewServiceAccount {
            project_id: project.id,
            kind: ServiceKind::Agent,
            name: "validation-agent",
            description: "",
            created_by: admin.id,
        },
    )
    .await?;
    let secret = secrets::new_secret(SERVICE_PREFIX)?;
    repo::create_token(
        &mut connection,
        NewToken {
            secret_digest: secret.digest(),
            display_prefix: secret.display_prefix(),
            kind: TokenKind::Service,
            user_id: None,
            service_account_id: Some(account.id),
            name: "service validation",
            scopes: &[Scope::Read],
            expires_in_days: 1,
        },
    )
    .await?;
    drop(connection);
    let service = secret.plaintext().expose();
    expect(
        &app,
        Method::GET,
        "/api/projects/validation",
        Some(service),
        None,
        200,
    )
    .await?;
    expect(
        &app,
        Method::GET,
        "/api/projects/validation/members",
        Some(service),
        None,
        403,
    )
    .await?;
    expect(
        &app,
        Method::GET,
        "/api/projects/missing",
        Some(service),
        None,
        404,
    )
    .await?;
    let mine = expect(
        &app,
        Method::GET,
        "/api/projects?before=validation",
        Some(service),
        None,
        200,
    )
    .await?;
    assert_eq!(mine["items"], json!([]));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri("/api/projects/validation")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()["allow"], "GET");
    assert!(to_bytes(response.into_body(), 4096).await?.is_empty());
    pool.close().await;
    Ok(())
}
