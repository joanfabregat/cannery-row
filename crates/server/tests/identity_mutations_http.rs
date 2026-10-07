//! Native HTTP identity mutations run only in an isolated Rust-migrated database.
#![forbid(unsafe_code)]
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode},
};
use cannery_core::{
    audit,
    ids::{TokenId, UserId},
    principal::Secret,
    settings::load_settings,
};
use cannery_identity::{
    repo::{self, LoginUser, NewSession},
    secrets,
};
use cannery_projects::repo as projects;
use cannery_server::{AppState, application};
use serde_json::{Value, json};
use sqlx::Acquire;
use std::{collections::BTreeMap, error::Error, str::FromStr};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}
async fn request(
    app: &Router,
    method: Method,
    uri: &str,
    headers: HeaderMap,
    body: Option<&Value>,
) -> Result<Reply> {
    let bytes = body
        .map(serde_json::to_vec)
        .transpose()?
        .unwrap_or_default();
    raw_request(app, method, uri, headers, bytes).await
}
async fn raw_request(
    app: &Router,
    method: Method,
    uri: &str,
    mut headers: HeaderMap,
    bytes: Vec<u8>,
) -> Result<Reply> {
    headers
        .entry("content-type")
        .or_insert(HeaderValue::from_static("application/json"));
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::from(bytes))?;
    *request.headers_mut() = headers;
    let response = app.clone().oneshot(request).await?;
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, 65536).await?;
    let body = if parts.status == StatusCode::INTERNAL_SERVER_ERROR {
        assert_eq!(
            parts.headers.get("content-type").map(HeaderValue::as_bytes),
            Some(b"text/plain; charset=utf-8".as_slice())
        );
        let source_body_matches = bytes.as_ref() == b"Internal Server Error";
        assert!(
            source_body_matches,
            "internal failure body differs from source"
        );
        Value::String(
            String::from_utf8(bytes.to_vec()).map_err(|_| "internal failure body is not UTF-8")?,
        )
    } else if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)?
    };
    Ok(Reply {
        status: parts.status,
        headers: parts.headers,
        body,
    })
}
fn bearer(secret: &Secret) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {}", secret.expose()))?,
    );
    Ok(headers)
}
fn browser(secret: &Secret, csrf: &str) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "cookie",
        HeaderValue::from_str(&format!("cr_session={}", secret.expose()))?,
    );
    headers.insert("x-csrf-token", HeaderValue::from_str(csrf)?);
    Ok(headers)
}
fn code(reply: &Reply, expected: StatusCode, error: &str) {
    assert_eq!(reply.status, expected);
    assert_eq!(reply.body["error"]["code"], error);
}
fn plain_internal(reply: &Reply) {
    assert_eq!(reply.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(reply.body, json!("Internal Server Error"));
}
fn token_id(reply: &Reply) -> Result<TokenId> {
    let text = reply.body["id"].as_str().ok_or("token id missing")?;
    TokenId::from_str(text).map_err(|_| "token id malformed".into())
}
fn token_secret(reply: &Reply) -> Result<Secret> {
    reply.body["token"]
        .as_str()
        .map(|value| Secret::new(value.to_owned()))
        .ok_or_else(|| "one-time token missing".into())
}
struct World {
    app: Router,
    state: AppState,
    admin: UserId,
    user: UserId,
    admin_headers: HeaderMap,
    user_headers: HeaderMap,
    slug: String,
}
impl World {
    async fn new(tag: &str) -> Result<Self> {
        Self::with_max_days(tag, None).await
    }
    async fn with_max_days(tag: &str, max_days: Option<i64>) -> Result<Self> {
        let uri = std::env::var("CANNERY_IDENTITY_TEST_DATABASE_URL")?;
        let mut settings = load_settings(
            None,
            &BTreeMap::from([("CANNERY_DATABASE_URL".into(), uri)]),
        )?;
        if let Some(max_days) = max_days {
            settings.auth.personal_token_max_days = max_days.into();
        }
        let (app, state) = application(settings)?;
        let mut connection = state
            .pool
            .acquire()
            .await
            .map_err(|_| "fixture connection failed")?;
        let database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&mut *connection)
            .await
            .map_err(|_| "fixture ownership query failed")?;
        let suffix = database
            .strip_prefix("conformance_")
            .ok_or("fixture database is not isolated")?;
        if suffix.len() != 24
            || !suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("fixture database is not isolated".into());
        }
        let mut transaction = connection
            .begin()
            .await
            .map_err(|_| "fixture begin failed")?;
        let mut users = Vec::new();
        let mut headers = Vec::new();
        for admin in [true, false] {
            let subject = format!("{tag}-{admin}");
            let user = repo::upsert_login_user(
                &mut transaction,
                LoginUser {
                    issuer: "https://fixture.identity",
                    subject: &subject,
                    email: Some(&format!("{subject}@fixture.test")),
                    email_verified: true,
                    display_name: Some(&subject),
                    make_admin: admin,
                },
            )
            .await?
            .user;
            let secret = secrets::new_secret(secrets::SESSION_PREFIX)?;
            let csrf = if admin {
                "admin-fixture-csrf"
            } else {
                "user-fixture-csrf"
            };
            repo::create_session(
                &mut transaction,
                NewSession {
                    user_id: user.id,
                    secret_digest: secret.digest(),
                    csrf_token: &Secret::new(csrf.into()),
                    ttl_hours: 1,
                },
            )
            .await?;
            users.push(user.id);
            headers.push(browser(secret.plaintext(), csrf)?);
        }
        let slug = format!("identity-{tag}");
        projects::create_project(&mut transaction, &slug, "Identity fixture", "", users[0])
            .await?
            .ok_or("fixture project collision")?;
        transaction
            .commit()
            .await
            .map_err(|_| "fixture commit failed")?;
        drop(connection);
        Ok(Self {
            app,
            state,
            admin: users[0],
            user: users[1],
            admin_headers: headers.remove(0),
            user_headers: headers.remove(0),
            slug,
        })
    }
    async fn call(
        &self,
        method: Method,
        uri: &str,
        headers: HeaderMap,
        body: Option<&Value>,
    ) -> Result<Reply> {
        request(&self.app, method, uri, headers, body).await
    }
    async fn mint(&self, headers: HeaderMap, name: &str, scopes: Value) -> Result<Reply> {
        let reply = self
            .call(
                Method::POST,
                "/api/tokens",
                headers,
                Some(&json!({"name":name,"expires_in_days":1,"scopes":scopes})),
            )
            .await?;
        assert_eq!(reply.status, StatusCode::CREATED);
        Ok(reply)
    }
    async fn events(&self, kind: &str, id: &str) -> Result<Vec<audit::AuditEvent>> {
        let mut connection = self
            .state
            .pool
            .acquire()
            .await
            .map_err(|_| "audit connection failed")?;
        audit::history(&mut connection, kind, id, None, None)
            .await
            .map_err(|_| "audit read failed".into())
    }
}

#[tokio::test]
#[ignore = "requires uniquely owned Rust-migrated PostgreSQL"]
#[allow(clippy::too_many_lines)]
async fn personal_tokens_preserve_once_secret_storage_privacy_and_revocation() -> Result<()> {
    let world = World::new("tokens").await?;
    let first = world
        .mint(
            world.user_headers.clone(),
            " First token ",
            json!(["write", "read", "read"]),
        )
        .await?;
    let id = token_id(&first)?;
    let secret = token_secret(&first)?;
    assert!(secret.expose().starts_with(secrets::PERSONAL_PREFIX));
    assert_eq!(secret.expose().len(), secrets::PERSONAL_PREFIX.len() + 43);
    assert_eq!(first.body["name"], "First token");
    assert!(
        first.body["expires_at"]
            .as_str()
            .is_some_and(|value| value.ends_with('Z'))
    );
    assert_eq!(first.body["scopes"], json!(["read", "write"]));
    assert_eq!(
        first.body["display_prefix"].as_str(),
        Some(&secret.expose()[..12])
    );
    assert!(first.body.get("user_id").is_none());
    assert!(first.body.get("token_hash").is_none());
    let mut connection = world
        .state
        .pool
        .acquire()
        .await
        .map_err(|_| "storage connection failed")?;
    let hash: Vec<u8> = sqlx::query_scalar("SELECT token_hash FROM api_tokens WHERE id=$1")
        .bind(id.0)
        .fetch_one(&mut *connection)
        .await
        .map_err(|_| "storage read failed")?;
    let digest_matches = hash.as_slice() == secrets::digest(secret.expose());
    assert!(digest_matches, "stored token digest differs");
    drop(connection);
    let events = world.events("api_token", &id.to_string()).await?;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].action, "token.created");
    assert_eq!(events[0].actor_user_id, Some(world.user));
    assert_eq!(
        events[0].new_state["expires_at"].as_str(),
        first.body["expires_at"]
            .as_str()
            .map(|value| value
                .strip_suffix('Z')
                .map_or_else(|| value.to_owned(), |value| format!("{value}+00:00")))
            .as_deref()
    );
    let second = world
        .mint(world.user_headers.clone(), "Second", json!(["read"]))
        .await?;
    let second_id = token_id(&second)?;
    let page = world
        .call(Method::GET, "/api/tokens?limit=1", bearer(&secret)?, None)
        .await?;
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(page.body["items"][0]["id"], json!(second_id));
    assert_eq!(page.body["next_before"], json!(second_id));
    assert!(page.body["items"][0].get("token").is_none());
    let page = world
        .call(
            Method::GET,
            &format!("/api/tokens?before={second_id}&limit=1"),
            world.user_headers.clone(),
            None,
        )
        .await?;
    assert_eq!(page.body["items"][0]["id"], json!(id));
    assert!(page.body["next_before"].is_null());
    let admin = world
        .mint(world.admin_headers.clone(), "Admin", json!(["write"]))
        .await?;
    let foreign = world
        .call(
            Method::GET,
            &format!("/api/tokens?before={}", token_id(&admin)?),
            world.user_headers.clone(),
            None,
        )
        .await?;
    assert_eq!(foreign.body["items"], json!([]));
    let foreign_revoke = world
        .call(
            Method::DELETE,
            &format!("/api/tokens/{}", token_id(&admin)?),
            bearer(&secret)?,
            None,
        )
        .await?;
    code(&foreign_revoke, StatusCode::NOT_FOUND, "not_found");
    let reader = token_secret(&second)?;
    code(
        &world
            .call(
                Method::DELETE,
                &format!("/api/tokens/{id}"),
                bearer(&reader)?,
                None,
            )
            .await?,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    let rejected = world
        .call(
            Method::POST,
            "/api/tokens",
            bearer(&secret)?,
            Some(&json!({"name":"Forbidden","expires_in_days":400,"scopes":["read"]})),
        )
        .await?;
    code(&rejected, StatusCode::FORBIDDEN, "forbidden");
    let me = world
        .call(Method::GET, "/api/me", bearer(&secret)?, None)
        .await?;
    assert_eq!(me.body["channel"], "api");
    let revoke = world
        .call(
            Method::DELETE,
            &format!("/api/tokens/{id}"),
            world.user_headers.clone(),
            None,
        )
        .await?;
    assert_eq!(revoke.status, StatusCode::OK);
    assert!(!revoke.body["revoked_at"].is_null());
    assert!(
        revoke.body["revoked_at"]
            .as_str()
            .is_some_and(|value| value.ends_with('Z'))
    );
    let repeated = world
        .call(
            Method::DELETE,
            &format!("/api/tokens/{id}"),
            world.user_headers.clone(),
            None,
        )
        .await?;
    assert_eq!(repeated.body, revoke.body);
    assert_eq!(world.events("api_token", &id.to_string()).await?.len(), 2);
    code(
        &world
            .call(Method::GET, "/api/me", bearer(&secret)?, None)
            .await?,
        StatusCode::UNAUTHORIZED,
        "unauthenticated",
    );
    let events = world.events("api_token", &id.to_string()).await?;
    assert_eq!(events[1].action, "token.revoked");
    world.state.pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires uniquely owned Rust-migrated PostgreSQL"]
#[allow(clippy::too_many_lines)]
async fn service_administration_preserves_scope_channel_disabled_and_audit_rules() -> Result<()> {
    let world = World::new("services").await?;
    let base = format!("/api/projects/{}/service-accounts", world.slug);
    let read = world
        .mint(world.admin_headers.clone(), "Reader", json!(["read"]))
        .await?;
    let read = token_secret(&read)?;
    let write = world
        .mint(world.admin_headers.clone(), "Writer", json!(["write"]))
        .await?;
    let write = token_secret(&write)?;
    let body = json!({"kind":"agent","name":"beta","description":" original "});
    code(
        &world
            .call(Method::POST, &base, world.user_headers.clone(), Some(&body))
            .await?,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    code(
        &world
            .call(Method::POST, &base, bearer(&read)?, Some(&body))
            .await?,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    let created = world
        .call(Method::POST, &base, bearer(&write)?, Some(&body))
        .await?;
    assert_eq!(created.status, StatusCode::CREATED);
    assert_eq!(created.body["description"], " original ");
    assert!(
        created.body["created_at"]
            .as_str()
            .is_some_and(|value| value.ends_with('Z'))
    );
    assert!(created.body.get("created_by").is_none());
    let account_id = created.body["id"].as_str().ok_or("account id")?;
    let events = world.events("service_account", account_id).await?;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].actor_user_id, Some(world.admin));
    assert_eq!(events[0].via_client.as_deref(), Some("token:Writer"));
    code(
        &world
            .call(
                Method::POST,
                &base,
                world.admin_headers.clone(),
                Some(&body),
            )
            .await?,
        StatusCode::CONFLICT,
        "conflict",
    );
    world
        .call(
            Method::POST,
            &base,
            world.admin_headers.clone(),
            Some(&json!({"kind":"tester","name":"alpha"})),
        )
        .await?;
    let listed = world
        .call(
            Method::GET,
            &format!("{base}?limit=1"),
            bearer(&read)?,
            None,
        )
        .await?;
    assert_eq!(listed.body["items"][0]["name"], "alpha");
    assert_eq!(listed.body["next_before"], "alpha");
    let listed = world
        .call(
            Method::GET,
            &format!("{base}?before=alpha&limit=1"),
            bearer(&read)?,
            None,
        )
        .await?;
    assert_eq!(listed.body["items"][0]["name"], "beta");
    assert!(listed.body["next_before"].is_null());
    let tokens = format!("{base}/beta/tokens");
    let token_body = json!({"name":"Service","expires_in_days":1,"scopes":["write","read","read"]});
    code(
        &world
            .call(Method::POST, &tokens, bearer(&write)?, Some(&token_body))
            .await?,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    let minted = world
        .call(
            Method::POST,
            &tokens,
            world.admin_headers.clone(),
            Some(&token_body),
        )
        .await?;
    assert_eq!(minted.status, StatusCode::CREATED);
    let id = token_id(&minted)?;
    let secret = token_secret(&minted)?;
    assert!(secret.expose().starts_with(secrets::SERVICE_PREFIX));
    let mut connection = world
        .state
        .pool
        .acquire()
        .await
        .map_err(|_| "service storage connection failed")?;
    let stored: Vec<u8> = sqlx::query_scalar("SELECT token_hash FROM api_tokens WHERE id=$1")
        .bind(id.0)
        .fetch_one(&mut *connection)
        .await
        .map_err(|_| "service storage read failed")?;
    let digest_matches = stored.as_slice() == secrets::digest(secret.expose());
    assert!(digest_matches, "stored service digest differs");
    drop(connection);
    let listed = world
        .call(Method::GET, &tokens, bearer(&read)?, None)
        .await?;
    assert_eq!(listed.body["items"][0]["id"], json!(id));
    assert!(listed.body["items"][0].get("token").is_none());
    let me = world
        .call(Method::GET, "/api/me", bearer(&secret)?, None)
        .await?;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.body["kind"], "service");
    code(
        &world
            .call(Method::GET, "/api/tokens", bearer(&secret)?, None)
            .await?,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    code(
        &world
            .call(Method::GET, &base, bearer(&secret)?, None)
            .await?,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    let disable = format!("{base}/beta/disable");
    let disabled = world
        .call(
            Method::POST,
            &disable,
            bearer(&write)?,
            Some(&json!({"reason":" retirement "})),
        )
        .await?;
    assert_eq!(disabled.status, StatusCode::OK);
    assert!(!disabled.body["disabled_at"].is_null());
    assert!(
        disabled.body["disabled_at"]
            .as_str()
            .is_some_and(|value| value.ends_with('Z'))
    );
    let again = world
        .call(
            Method::POST,
            &disable,
            world.admin_headers.clone(),
            Some(&json!({"reason":"again"})),
        )
        .await?;
    assert_eq!(again.body, disabled.body);
    let events = world.events("service_account", account_id).await?;
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].action, "service_account.disabled");
    assert_eq!(events[1].reason.as_deref(), Some(" retirement "));
    code(
        &world
            .call(Method::GET, "/api/me", bearer(&secret)?, None)
            .await?,
        StatusCode::UNAUTHORIZED,
        "unauthenticated",
    );
    code(
        &world
            .call(
                Method::POST,
                &tokens,
                world.admin_headers.clone(),
                Some(&json!({"name":"Too long","expires_in_days":400,"scopes":["read"]})),
            )
            .await?,
        StatusCode::CONFLICT,
        "conflict",
    );
    let revoke = world
        .call(
            Method::DELETE,
            &format!("/api/tokens/{id}"),
            bearer(&write)?,
            None,
        )
        .await?;
    assert_eq!(revoke.status, StatusCode::OK);
    let events = world.events("api_token", &id.to_string()).await?;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].new_state["service_account"], "beta");
    world.state.pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires uniquely owned Rust-migrated PostgreSQL"]
#[allow(clippy::too_many_lines)]
async fn identity_validation_authentication_lookup_and_rollback_order_uses_native_json()
-> Result<()> {
    let world = World::new("ordering").await?;
    let uri = "/api/projects/missing/service-accounts";
    let invalid = json!({"name":"INVALID","kind":"invalid"});
    code(
        &world
            .call(Method::POST, uri, HeaderMap::new(), Some(&invalid))
            .await?,
        StatusCode::UNAUTHORIZED,
        "unauthenticated",
    );
    let syntax = raw_request(
        &world.app,
        Method::POST,
        uri,
        HeaderMap::new(),
        b"{".to_vec(),
    )
    .await?;
    code(
        &syntax,
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
    let mut no_csrf = world.admin_headers.clone();
    no_csrf.remove("x-csrf-token");
    code(
        &world
            .call(Method::POST, uri, no_csrf, Some(&invalid))
            .await?,
        StatusCode::FORBIDDEN,
        "csrf_invalid",
    );
    let validation = world
        .call(
            Method::POST,
            uri,
            world.user_headers.clone(),
            Some(&invalid),
        )
        .await?;
    code(
        &validation,
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
    assert_eq!(validation.body["error"]["details"][0]["path"], "body/kind");
    code(
        &world
            .call(
                Method::POST,
                uri,
                world.admin_headers.clone(),
                Some(&json!({"kind":"agent","name":"valid"})),
            )
            .await?,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    code(
        &world
            .call(
                Method::DELETE,
                "/api/tokens/invalid",
                HeaderMap::new(),
                None,
            )
            .await?,
        StatusCode::UNAUTHORIZED,
        "unauthenticated",
    );
    code(
        &world
            .call(
                Method::DELETE,
                "/api/tokens/invalid",
                world.user_headers.clone(),
                None,
            )
            .await?,
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
    let base = format!("/api/projects/{}/service-accounts", world.slug);
    let created = world
        .call(
            Method::POST,
            &base,
            world.admin_headers.clone(),
            Some(&json!({"kind":"agent","name":"known"})),
        )
        .await?;
    assert_eq!(created.status, StatusCode::CREATED);
    let id = created.body["id"].as_str().ok_or("account id")?;
    let readonly = world
        .mint(world.admin_headers.clone(), "Read", json!(["read"]))
        .await?;
    let readonly = token_secret(&readonly)?;
    let invalid_query = world
        .call(
            Method::GET,
            &format!("{base}/known/tokens?before=invalid&limit=0"),
            world.user_headers.clone(),
            None,
        )
        .await?;
    code(
        &invalid_query,
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
    assert_eq!(
        invalid_query.body["error"]["details"][0]["path"],
        "query/before"
    );
    assert_eq!(
        invalid_query.body["error"]["details"][1]["path"],
        "query/limit"
    );
    code(
        &world
            .call(Method::GET, "/api/users", world.user_headers.clone(), None)
            .await?,
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
    code(
        &world
            .call(
                Method::GET,
                "/api/users?email=abc",
                world.user_headers.clone(),
                None,
            )
            .await?,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    let email = "identity-query@fixture.test";
    let mut connection = world
        .state
        .pool
        .acquire()
        .await
        .map_err(|_| "query fixture connection failed")?;
    for (subject, verified) in [
        ("query-one", true),
        ("query-two", true),
        ("query-hidden", false),
    ] {
        repo::upsert_login_user(
            &mut connection,
            LoginUser {
                issuer: "https://fixture.identity",
                subject,
                email: Some(email),
                email_verified: verified,
                display_name: Some(subject),
                make_admin: false,
            },
        )
        .await?;
    }
    drop(connection);
    let users = world
        .call(
            Method::GET,
            "/api/users?email=IDENTITY-QUERY%40fixture.test&limit=1",
            bearer(&readonly)?,
            None,
        )
        .await?;
    assert_eq!(users.status, StatusCode::OK);
    assert_eq!(users.body["items"].as_array().ok_or("user items")?.len(), 1);
    let cursor = users.body["next_before"].as_str().ok_or("user cursor")?;
    let users = world
        .call(
            Method::GET,
            &format!("/api/users?email=identity-query%40fixture.test&before={cursor}&limit=1"),
            bearer(&readonly)?,
            None,
        )
        .await?;
    assert_eq!(users.body["items"].as_array().ok_or("user items")?.len(), 1);
    assert!(users.body["next_before"].is_null());
    assert!(users.body["items"][0].get("issuer").is_none());
    let tokens_before = world
        .call(
            Method::GET,
            &format!("{base}/known/tokens"),
            world.admin_headers.clone(),
            None,
        )
        .await?;
    code(
        &world
            .call(
                Method::POST,
                &format!("{base}/known/tokens"),
                world.admin_headers.clone(),
                Some(&json!({"name":"Too long","expires_in_days":400,"scopes":["read"]})),
            )
            .await?,
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
    let tokens_after = world
        .call(
            Method::GET,
            &format!("{base}/known/tokens"),
            world.admin_headers.clone(),
            None,
        )
        .await?;
    assert_eq!(tokens_before.body, tokens_after.body);
    assert_eq!(world.events("service_account", id).await?.len(), 1);
    let surrogate = format!(
        "{{\"kind\":\"agent\",\"name\":\"surrogate\",\"description\":\"{}\"}}",
        "\\ud800"
    );
    code(
        &raw_request(
            &world.app,
            Method::POST,
            &base,
            world.admin_headers.clone(),
            surrogate.into_bytes(),
        )
        .await?,
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
    let accounts = world
        .call(Method::GET, &base, world.admin_headers.clone(), None)
        .await?;
    assert_eq!(
        accounts.body["items"]
            .as_array()
            .ok_or("account items")?
            .len(),
        1
    );
    let head = world
        .call(Method::HEAD, "/api/tokens", HeaderMap::new(), None)
        .await?;
    assert_eq!(head.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(head.headers["allow"], "GET");
    assert!(head.body.is_null());
    let put = world
        .call(
            Method::PUT,
            &format!("{base}/known/disable"),
            HeaderMap::new(),
            None,
        )
        .await?;
    assert_eq!(put.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(put.headers["allow"], "POST");
    let post = world
        .call(Method::POST, "/api/tokens/invalid", HeaderMap::new(), None)
        .await?;
    assert_eq!(post.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(post.headers["allow"], "DELETE");
    world.state.pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires uniquely owned Rust-migrated PostgreSQL"]
async fn failed_audit_rolls_back_mutation_and_keeps_authentication_touch() -> Result<()> {
    let world = World::with_max_days("rollback", Some(3_000_000_000)).await?;
    // The isolated-database guard in setup runs before any fixture writes.
    // This fixed constraint only affects this exact synthetic token label.
    let mut connection = world
        .state
        .pool
        .acquire()
        .await
        .map_err(|_| "rollback fixture connection failed")?;
    sqlx::query("ALTER TABLE audit_events ADD CONSTRAINT identity_http_fixture_reject CHECK (action <> 'token.created' OR new_state->>'name' IS DISTINCT FROM 'rollback-sentinel')").execute(&mut *connection).await.map_err(|_|"rollback fixture setup failed")?;
    let before: Option<String> =
        sqlx::query_scalar("SELECT last_seen_at::text FROM sessions WHERE user_id=$1")
            .bind(world.admin.0)
            .fetch_one(&mut *connection)
            .await
            .map_err(|_| "authentication touch baseline failed")?;
    drop(connection);
    let failed = world
        .call(
            Method::POST,
            "/api/tokens",
            world.admin_headers.clone(),
            Some(&json!({"name":"rollback-sentinel","expires_in_days":1,"scopes":["read"]})),
        )
        .await?;
    plain_internal(&failed);
    let mut connection = world
        .state
        .pool
        .acquire()
        .await
        .map_err(|_| "rollback verification connection failed")?;
    let tokens: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM api_tokens WHERE user_id=$1 AND name='rollback-sentinel'",
    )
    .bind(world.admin.0)
    .fetch_one(&mut *connection)
    .await
    .map_err(|_| "rollback token verification failed")?;
    let events:i64=sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE actor_user_id=$1 AND new_state->>'name'='rollback-sentinel'").bind(world.admin.0).fetch_one(&mut *connection).await.map_err(|_|"rollback audit verification failed")?;
    assert_eq!(tokens, 0);
    assert_eq!(events, 0);
    let after: Option<String> =
        sqlx::query_scalar("SELECT last_seen_at::text FROM sessions WHERE user_id=$1")
            .bind(world.admin.0)
            .fetch_one(&mut *connection)
            .await
            .map_err(|_| "authentication touch verification failed")?;
    assert!(after.is_some());
    assert_ne!(after, before);
    drop(connection);
    let unsupported=world.call(Method::POST,"/api/tokens",world.admin_headers.clone(),Some(&json!({"name":"unsupported-interval","expires_in_days":2_147_483_648_u64,"scopes":["read"]}))).await?;
    plain_internal(&unsupported);
    // This interval fits PostgreSQL's integer parameter but produces a date
    // beyond Python's year-9999 bound. Decoding must fail without committing
    // the credential or its audit event.
    let distant = world
        .call(
            Method::POST,
            "/api/tokens",
            world.admin_headers.clone(),
            Some(&json!({"name":"distant-expiry","expires_in_days":3_000_000,"scopes":["read"]})),
        )
        .await?;
    plain_internal(&distant);
    let distant_events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE actor_user_id=$1 AND new_state->>'name'='distant-expiry'",
    )
    .bind(world.admin.0)
    .fetch_one(&world.state.pool)
    .await
    .map_err(|_| "distant expiry audit verification failed")?;
    assert_eq!(distant_events, 0);
    let listed = world
        .call(
            Method::GET,
            "/api/tokens",
            world.admin_headers.clone(),
            None,
        )
        .await?;
    assert_eq!(listed.body["items"], json!([]));
    world.state.pool.close().await;
    Ok(())
}
