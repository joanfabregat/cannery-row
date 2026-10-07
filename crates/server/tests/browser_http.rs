//! Native browser HTTP/DB tests; the synthetic provider does not prove crypto parity.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode},
};
use cannery_core::{
    principal::{Scope, Secret, ServiceKind},
    settings::{Settings, load_settings},
};
use cannery_identity::{
    models::TokenKind,
    repo::{self, NewServiceAccount, NewToken},
    secrets,
};
use cannery_server::{
    AppState, application, application_with_oidc,
    oidc_claims::Identity,
    oidc_provider::{AuthorizationRequest, OidcProvider, ProviderError},
};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    error::Error,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
};
use tower::ServiceExt;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
fn text(value: &str) -> String {
    String::from(value)
}
struct FixtureProvider {
    issuer: String,
    subject: String,
    issued: Mutex<Vec<(Secret, Secret)>>,
    auth_mode: AtomicU8,
    end_mode: AtomicU8,
    exchange_count: AtomicUsize,
    redirect_uri: Mutex<Option<String>>,
}
impl FixtureProvider {
    fn identity(&self, code: &str) -> std::result::Result<Identity, ProviderError> {
        let mut identity = Identity {
            issuer: text(&self.issuer),
            subject: text(&self.subject),
            email: Some(text("ADMIN@EXAMPLE.TEST")),
            email_verified: true,
            name: Some(text("Fixture person")),
        };
        match Some(code) {
            Some("reject") => return Err(ProviderError::Rejected(text("provider refused code"))),
            Some("unhandled") => return Err(ProviderError::Unhandled),
            Some("unicode-error") => {
                return Err(ProviderError::Rejected(text("provider refused é")));
            }
            Some("updated") => {
                identity.email = Some(text("OTHER@EXAMPLE.TEST"));
                identity.email_verified = false;
                identity.name = Some(text("Updated person"));
            }
            Some("unicode") => identity.email = Some(text("ΟΣ@ΟΣ.TEST")),
            Some("unverified") => identity.email_verified = false,
            Some("empty-email") => identity.email = Some(text("")),
            Some("naked-domain") => identity.email = Some(text("EXAMPLE.TEST")),
            Some("nul-email") => identity.email = Some(text("fixture\0@example.test")),
            Some("nul-sub") => identity.subject = text("fixture\0subject"),
            _ => {}
        }
        Ok(identity)
    }
}
impl OidcProvider for FixtureProvider {
    fn authorization_request(
        &self,
    ) -> BoxFuture<'_, std::result::Result<AuthorizationRequest, ProviderError>> {
        Box::pin(async move {
            match self.auth_mode.load(Ordering::SeqCst) {
                1 => return Err(ProviderError::Rejected(text("provider unavailable"))),
                2 => return Err(ProviderError::Unhandled),
                _ => {}
            }
            let state = secrets::new_secret("").map_err(|_| ProviderError::Unhandled)?;
            let nonce = secrets::new_secret("").map_err(|_| ProviderError::Unhandled)?;
            let verifier = secrets::new_secret("").map_err(|_| ProviderError::Unhandled)?;
            self.issued
                .lock()
                .map_err(|_| ProviderError::Unhandled)?
                .push((nonce.plaintext().clone(), verifier.plaintext().clone()));
            let url = if self.auth_mode.load(Ordering::SeqCst) == 3 {
                text("https://provider.fixture/authorize?message=é\r\nX-Test: injected")
            } else {
                text(&format!(
                    "https://provider.fixture/authorize?state={}",
                    state.plaintext().expose()
                ))
            };
            Ok(AuthorizationRequest {
                url,
                state: state.plaintext().clone(),
                nonce: nonce.plaintext().clone(),
                code_verifier: verifier.plaintext().clone(),
            })
        })
    }
    fn exchange_code<'a>(
        &'a self,
        code: &'a str,
        verifier: &'a Secret,
        nonce: &'a Secret,
    ) -> BoxFuture<'a, std::result::Result<Identity, ProviderError>> {
        Box::pin(async move {
            self.exchange_count.fetch_add(1, Ordering::SeqCst);
            let correct = self
                .issued
                .lock()
                .map_err(|_| ProviderError::Unhandled)?
                .iter()
                .any(|(saved_nonce, saved_verifier)| {
                    saved_nonce.expose() == nonce.expose()
                        && saved_verifier.expose() == verifier.expose()
                });
            if !correct {
                return Err(ProviderError::Unhandled);
            }
            self.identity(code)
        })
    }
    fn end_session_url<'a>(
        &'a self,
        uri: &'a str,
    ) -> BoxFuture<'a, std::result::Result<Option<String>, ProviderError>> {
        Box::pin(async move {
            *self
                .redirect_uri
                .lock()
                .map_err(|_| ProviderError::Unhandled)? = Some(uri.to_owned());
            match self.end_mode.load(Ordering::SeqCst) {
                1 => Err(ProviderError::Rejected(text("end session unavailable"))),
                2 => Err(ProviderError::Unhandled),
                3 => Ok(Some(text(
                    "https://provider.fixture/end-session?message=مرحبا",
                ))),
                4 => Ok(None),
                _ => Ok(Some(text("https://provider.fixture/end-session"))),
            }
        })
    }
}
struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}
async fn call(app: &Router, method: Method, uri: &str, headers: HeaderMap) -> Result<Reply> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())?;
    *request.headers_mut() = headers;
    let response = app.clone().oneshot(request).await?;
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, 65_536).await?;
    let body = if parts.status == StatusCode::INTERNAL_SERVER_ERROR {
        assert!(
            bytes.as_ref() == b"Internal Server Error",
            "internal response must be generic"
        );
        assert_eq!(
            parts.headers.get("content-type").map(HeaderValue::as_bytes),
            Some(b"text/plain; charset=utf-8".as_slice())
        );
        Value::Null
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
fn cookie(headers: &HeaderMap, name: &str) -> Result<Secret> {
    for header in headers.get_all("set-cookie") {
        let value = header.to_str().map_err(|_| "cookie encoding failed")?;
        let first = value.split(';').next().ok_or("missing cookie")?;
        if first.starts_with(&format!("{name}=")) {
            return Ok(Secret::new(first.to_owned()));
        }
    }
    Err("missing expected cookie".into())
}
fn cookie_headers(value: &Secret) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "cookie",
        HeaderValue::from_str(value.expose()).map_err(|_| "fixture cookie header failed")?,
    );
    Ok(headers)
}
fn rejection(reply: &Reply, status: u16, code: &str, message: &str) {
    assert_eq!(reply.status.as_u16(), status);
    assert_eq!(reply.body["error"]["code"], code);
    assert_eq!(reply.body["error"]["message"], message);
    assert!(reply.headers.get("set-cookie").is_none());
}
struct Pending {
    state: Secret,
    cookie: Secret,
}
struct World {
    app: Router,
    state: AppState,
    provider: Arc<FixtureProvider>,
}
impl World {
    async fn new(label: &str, configure: impl FnOnce(&mut Settings)) -> Result<Self> {
        let url = std::env::var("CANNERY_IDENTITY_TEST_DATABASE_URL")
            .map_err(|_| "isolated DB environment required")?;
        let mut settings = load_settings(
            None,
            &BTreeMap::from([("CANNERY_DATABASE_URL".into(), url)]),
        )?;
        settings.auth.cookie_secure = false;
        configure(&mut settings);
        let provider = Arc::new(FixtureProvider {
            issuer: format!("https://provider.fixture/{label}"),
            subject: format!("subject-{label}"),
            issued: Mutex::new(Vec::new()),
            auth_mode: AtomicU8::new(0),
            end_mode: AtomicU8::new(0),
            exchange_count: AtomicUsize::new(0),
            redirect_uri: Mutex::new(None),
        });
        let (app, state) = application_with_oidc(settings, Some(provider.clone()))?;
        let database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&state.pool)
            .await
            .map_err(|_| "fixture guard query failed")?;
        let suffix = database
            .strip_prefix("conformance_")
            .ok_or("database ownership guard failed")?;
        if suffix.len() != 24
            || !suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("database ownership guard failed".into());
        }
        Ok(Self {
            app,
            state,
            provider,
        })
    }
    async fn login(&self, query: &str) -> Result<Pending> {
        let reply = call(
            &self.app,
            Method::GET,
            &format!("/auth/login{query}"),
            HeaderMap::new(),
        )
        .await?;
        assert_eq!(reply.status, StatusCode::FOUND);
        assert_eq!(
            reply
                .headers
                .get("content-length")
                .map(HeaderValue::as_bytes),
            Some(b"0".as_slice())
        );
        assert!(reply.headers.get("content-type").is_none());
        let location = reply
            .headers
            .get("location")
            .ok_or("missing authorization location")?
            .to_str()
            .map_err(|_| "authorization URL encoding")?;
        let state = location
            .split_once("?state=")
            .ok_or("missing authorization state")?
            .1;
        let binding = cookie(&reply.headers, "cr_login")?;
        let header = reply
            .headers
            .get("set-cookie")
            .ok_or("missing login cookie")?
            .to_str()
            .map_err(|_| "login cookie encoding")?;
        assert!(
            header.contains("; HttpOnly; Max-Age=900; Path=/auth; SameSite=lax"),
            "login cookie attributes differ"
        );
        assert_eq!(
            header.ends_with("; Secure"),
            self.state.settings.auth.cookie_secure
        );
        Ok(Pending {
            state: Secret::new(state.to_owned()),
            cookie: binding,
        })
    }
    async fn callback(&self, pending: &Pending, code: &str) -> Result<Reply> {
        call(
            &self.app,
            Method::GET,
            &format!(
                "/auth/callback?state={}&code={code}",
                pending.state.expose()
            ),
            cookie_headers(&pending.cookie)?,
        )
        .await
    }
    async fn pending_count(&self) -> Result<i64> {
        // This profile runs only these tests serially in a fresh isolated database.
        sqlx::query_scalar("SELECT count(*) FROM oidc_login_requests")
            .fetch_one(&self.state.pool)
            .await
            .map_err(|_| "pending fixture read failed".into())
    }
    async fn user(&self) -> Result<Option<(String, Option<String>, bool, bool, Option<String>)>> {
        sqlx::query_as("SELECT id::text,email,email_verified,is_admin,display_name FROM users WHERE issuer=$1 AND subject=$2").bind(&self.provider.issuer).bind(&self.provider.subject).fetch_optional(&self.state.pool).await.map_err(|_| "user fixture read failed".into())
    }
}
async fn me(world: &World, session: &Secret) -> Result<Reply> {
    call(&world.app, Method::GET, "/api/me", cookie_headers(session)?).await
}
async fn logout(world: &World, session: &Secret, csrf: &str) -> Result<Reply> {
    let mut headers = cookie_headers(session)?;
    headers.insert("x-csrf-token", HeaderValue::from_str(csrf)?);
    call(&world.app, Method::POST, "/auth/logout", headers).await
}
#[tokio::test]
#[ignore = "requires uniquely owned Rust-migrated PostgreSQL"]
async fn successful_login_upsert_monotonic_admin_sessions_and_logout() -> Result<()> {
    let world = World::new("success", |settings| {
        settings.auth.bootstrap_admin_emails = vec!["admin@example.test".into()];
        settings.auth.cookie_secure = true;
        settings.auth.session_ttl_hours = 3_i64.into();
    })
    .await?;
    let pending = world
        .login("?return_to=%2Ffirst&return_to=%2Fscience%3Fa%3D1")
        .await?;
    let stored: (String, Vec<u8>) =
        sqlx::query_as("SELECT return_to,browser_hash FROM oidc_login_requests WHERE state=$1")
            .bind(pending.state.expose())
            .fetch_one(&world.state.pool)
            .await
            .map_err(|_| "login storage read failed")?;
    assert_eq!(stored.0, "/science?a=1");
    let binding = pending
        .cookie
        .expose()
        .strip_prefix("cr_login=")
        .ok_or("binding prefix absent")?;
    assert!(
        stored.1 == secrets::digest(binding),
        "browser binding digest mismatch"
    );
    let reply = world.callback(&pending, "ok").await?;
    assert_eq!(reply.status, StatusCode::FOUND);
    assert_eq!(
        reply.headers.get("location").map(HeaderValue::as_bytes),
        Some(b"/science?a=1".as_slice())
    );
    let cookies: Vec<_> = reply.headers.get_all("set-cookie").iter().collect();
    assert_eq!(cookies.len(), 2);
    assert!(
        cookies[0]
            .as_bytes()
            .starts_with(b"cr_login=\"\"; expires=")
    );
    assert!(
        cookies[0]
            .to_str()?
            .ends_with("Max-Age=0; Path=/auth; SameSite=lax")
    );
    assert!(
        cookies[1]
            .to_str()?
            .contains("; HttpOnly; Max-Age=10800; Path=/; SameSite=lax; Secure")
    );
    let session = cookie(&reply.headers, "cr_session")?;
    let current = me(&world, &session).await?;
    assert_eq!(current.status, StatusCode::OK);
    assert_eq!(current.body["user"]["email"], "admin@example.test");
    assert_eq!(current.body["user"]["is_admin"], true);
    let csrf = current.body["csrf_token"].as_str().ok_or("missing CSRF")?;
    let user = world.user().await?.ok_or("missing user")?;
    verify_login_storage(&world, &user.0, &session).await?;
    let second = world.login("?return_to=%2F%5Cevil.example").await?;
    let updated = world.callback(&second, "updated").await?;
    assert_eq!(updated.status, StatusCode::FOUND);
    assert_eq!(
        updated.headers.get("location").map(HeaderValue::as_bytes),
        Some(b"/".as_slice())
    );
    let changed = world.user().await?.ok_or("updated user missing")?;
    assert_eq!(changed.0, user.0);
    assert_eq!(changed.1.as_deref(), Some("other@example.test"));
    assert!(!changed.2 && changed.3);
    assert_eq!(changed.4.as_deref(), Some("Updated person"));
    let denied = logout(&world, &session, "wrong").await?;
    rejection(
        &denied,
        403,
        "csrf_invalid",
        "missing or invalid CSRF token",
    );
    let ended = logout(&world, &session, csrf).await?;
    assert_eq!(ended.status, StatusCode::OK);
    assert_eq!(
        ended.body,
        json!({"logout_url":"https://provider.fixture/end-session"})
    );
    assert!(
        cookie(&ended.headers, "cr_session")?
            .expose()
            .starts_with("cr_session=\"\"")
    );
    assert_eq!(me(&world, &session).await?.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        world
            .provider
            .redirect_uri
            .lock()
            .map_err(|_| "provider fixture lock")?
            .as_ref(),
        Some(&text(&format!(
            "{}/",
            world.state.settings.server.public_base_url
        )))
    );
    Ok(())
}
#[tokio::test]
#[ignore = "requires uniquely owned Rust-migrated PostgreSQL"]
async fn callback_precedence_binding_replay_expiry_and_exchange_failures() -> Result<()> {
    let world = World::new("failures", |_| {}).await?;
    let pending = world.login("").await?;
    let error = call(
        &world.app,
        Method::GET,
        "/auth/callback?error=&state=&code=",
        HeaderMap::new(),
    )
    .await?;
    rejection(
        &error,
        400,
        "login_failed",
        "the identity provider refused the login: ",
    );
    assert_eq!(world.pending_count().await?, 1);
    rejection(
        &call(&world.app, Method::GET, "/auth/callback", HeaderMap::new()).await?,
        400,
        "login_failed",
        "missing state or code",
    );
    let no_browser = call(
        &world.app,
        Method::GET,
        &format!("/auth/callback?state={}&code=ok", pending.state.expose()),
        HeaderMap::new(),
    )
    .await?;
    rejection(
        &no_browser,
        400,
        "login_failed",
        "this login was not started in this browser; start again",
    );
    assert_eq!(world.pending_count().await?, 1);
    let wrong = call(
        &world.app,
        Method::GET,
        &format!("/auth/callback?state={}&code=ok", pending.state.expose()),
        cookie_headers(&Secret::new("cr_login=wrong".into()))?,
    )
    .await?;
    rejection(
        &wrong,
        400,
        "login_failed",
        "unknown or expired login attempt; start again",
    );
    assert_eq!(world.pending_count().await?, 0);
    rejection(
        &world.callback(&pending, "ok").await?,
        400,
        "login_failed",
        "unknown or expired login attempt; start again",
    );
    assert_eq!(world.provider.exchange_count.load(Ordering::SeqCst), 0);
    let expired = world.login("").await?;
    sqlx::query(
        "UPDATE oidc_login_requests SET created_at=now()-interval '16 minutes' WHERE state=$1",
    )
    .bind(expired.state.expose())
    .execute(&world.state.pool)
    .await
    .map_err(|_| "expiry fixture setup failed")?;
    rejection(
        &world.callback(&expired, "ok").await?,
        400,
        "login_failed",
        "unknown or expired login attempt; start again",
    );
    verify_exchange_failures(&world).await?;
    let duplicate = world.login("").await?;
    let reply = call(
        &world.app,
        Method::GET,
        &format!(
            "/auth/callback?state=wrong&state={}&code=reject&code=ok",
            duplicate.state.expose()
        ),
        cookie_headers(&duplicate.cookie)?,
    )
    .await?;
    assert_eq!(reply.status, StatusCode::FOUND);
    Ok(())
}
async fn verify_exchange_failures(world: &World) -> Result<()> {
    for code in ["reject", "unhandled", "unicode-error", "nul-sub"] {
        let pending = world.login("").await?;
        let failed = world.callback(&pending, code).await?;
        if code == "reject" {
            rejection(&failed, 400, "login_failed", "provider refused code");
        } else if code == "unicode-error" {
            rejection(&failed, 400, "login_failed", "provider refused é");
        } else {
            assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR);
        }
        assert_eq!(world.pending_count().await?, 0);
        assert!(world.user().await?.is_none());
        rejection(
            &world.callback(&pending, "ok").await?,
            400,
            "login_failed",
            "unknown or expired login attempt; start again",
        );
    }
    Ok(())
}
#[tokio::test]
#[ignore = "requires uniquely owned Rust-migrated PostgreSQL"]
async fn unicode_policy_precedes_database_encoding_and_uses_native_domain_rules() -> Result<()> {
    let world = World::new("unicode", |settings| {
        settings.auth.allowed_email_domains = vec!["ΟΣ.TEST".into()];
        settings.auth.bootstrap_admin_emails = vec!["ΟΣ@ΟΣ.TEST".into()];
    })
    .await?;
    let pending = world.login("").await?;
    assert_eq!(
        world.callback(&pending, "unicode").await?.status,
        StatusCode::FOUND
    );
    let user = world.user().await?.ok_or("Unicode user missing")?;
    assert_eq!(user.1.as_deref(), Some("ος@οσ.test"));
    assert!(user.3);
    for code in ["unverified", "empty-email", "nul-email", "ok"] {
        let pending = world.login("").await?;
        rejection(
            &world.callback(&pending, code).await?,
            403,
            "forbidden",
            "this account's email domain is not allowed here",
        );
        assert_eq!(world.pending_count().await?, 0);
        assert_eq!(world.user().await?.as_ref(), Some(&user));
    }
    let naked = World::new("naked", |settings| {
        settings.auth.allowed_email_domains = vec!["EXAMPLE.TEST".into()];
    })
    .await?;
    assert_eq!(
        naked
            .callback(&naked.login("").await?, "naked-domain")
            .await?
            .status,
        StatusCode::FOUND
    );
    assert_eq!(
        naked
            .user()
            .await?
            .ok_or("naked domain user missing")?
            .1
            .as_deref(),
        Some("example.test")
    );
    let no_policy = World::new("nul-without-policy", |_| {}).await?;
    assert_eq!(
        no_policy
            .callback(&no_policy.login("").await?, "nul-email")
            .await?
            .status,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert!(no_policy.user().await?.is_none());
    Ok(())
}
#[tokio::test]
#[ignore = "requires uniquely owned Rust-migrated PostgreSQL"]
async fn configuration_provider_failures_method_redirects_and_pending_commit() -> Result<()> {
    let world = World::new("configuration", |_| {}).await?;
    let (disabled, disabled_state) = application((*world.state.settings).clone())?;
    for uri in ["/auth/login", "/auth/callback?error=denied"] {
        rejection(
            &call(&disabled, Method::GET, uri, HeaderMap::new()).await?,
            404,
            "not_found",
            "OIDC login is not configured on this installation",
        );
    }
    for (uri, allow) in [
        ("/auth/login", "GET"),
        ("/auth/callback", "GET"),
        ("/auth/logout", "POST"),
    ] {
        let head = call(&world.app, Method::HEAD, uri, HeaderMap::new()).await?;
        assert_eq!(head.status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            head.headers.get("allow").map(HeaderValue::as_bytes),
            Some(allow.as_bytes())
        );
        assert!(head.body.is_null());
        let wrong_method = call(&world.app, Method::PUT, uri, HeaderMap::new()).await?;
        assert_eq!(wrong_method.status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            wrong_method.headers.get("allow").map(HeaderValue::as_bytes),
            Some(allow.as_bytes())
        );
        let slash = call(
            &world.app,
            Method::PUT,
            &format!("{uri}/?raw=%3F"),
            HeaderMap::new(),
        )
        .await?;
        assert_eq!(slash.status, StatusCode::TEMPORARY_REDIRECT);
        assert!(
            slash
                .headers
                .get("location")
                .ok_or("slash Location missing")?
                .to_str()?
                .ends_with(&format!("{uri}?raw=%3F"))
        );
    }
    world.provider.auth_mode.store(1, Ordering::SeqCst);
    rejection(
        &call(&world.app, Method::GET, "/auth/login", HeaderMap::new()).await?,
        400,
        "login_failed",
        "identity provider unavailable: provider unavailable",
    );
    assert_eq!(world.pending_count().await?, 0);
    world.provider.auth_mode.store(2, Ordering::SeqCst);
    assert_eq!(
        call(&world.app, Method::GET, "/auth/login", HeaderMap::new())
            .await?
            .status,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    world.provider.auth_mode.store(3, Ordering::SeqCst);
    let encoded = call(&world.app, Method::GET, "/auth/login", HeaderMap::new()).await?;
    assert_eq!(encoded.status, StatusCode::FOUND);
    assert_eq!(
        encoded
            .headers
            .get("location")
            .ok_or("encoded redirect missing")?
            .to_str()?,
        "https://provider.fixture/authorize?message=%C3%A9%0D%0AX-Test:%20injected"
    );
    assert!(encoded.headers.get("x-test").is_none());
    assert_eq!(
        world.pending_count().await?,
        1,
        "pending state commits with a safely encoded UTF-8 redirect"
    );
    sqlx::query("DELETE FROM oidc_login_requests")
        .execute(&world.state.pool)
        .await
        .map_err(|_| "owned pending fixture cleanup failed")?;
    disabled_state.pool.close().await;
    let settings = (*world.state.settings).clone();
    let (unavailable, unavailable_state) =
        application_with_oidc(settings, Some(world.provider.clone()))?;
    unavailable_state.pool.close().await;
    assert_eq!(
        call(
            &unavailable,
            Method::GET,
            "/auth/callback?error=denied",
            HeaderMap::new()
        )
        .await?
        .status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "connection dependency precedes provider and query errors"
    );
    Ok(())
}
#[tokio::test]
#[ignore = "requires uniquely owned Rust-migrated PostgreSQL"]
async fn logout_provider_failures_are_after_session_commit_and_bearer_can_logout() -> Result<()> {
    let world = World::new("logout", |_| {}).await?;
    for mode in 0..=4 {
        let signed_in = world.callback(&world.login("").await?, "ok").await?;
        let session = cookie(&signed_in.headers, "cr_session")?;
        let current = me(&world, &session).await?;
        let csrf = current.body["csrf_token"].as_str().ok_or("CSRF missing")?;
        world.provider.end_mode.store(mode, Ordering::SeqCst);
        let reply = logout(&world, &session, csrf).await?;
        assert_eq!(
            reply.status,
            if mode == 2 {
                StatusCode::INTERNAL_SERVER_ERROR
            } else {
                StatusCode::OK
            }
        );
        if mode == 1 || mode == 4 {
            assert_eq!(reply.body, json!({"logout_url":null}));
        }
        if mode == 3 {
            assert_eq!(
                reply.body,
                json!({"logout_url":"https://provider.fixture/end-session?message=مرحبا"})
            );
        }
        if mode == 2 {
            assert!(reply.headers.get("set-cookie").is_none());
        }
        assert_eq!(me(&world, &session).await?.status, StatusCode::UNAUTHORIZED);
    }
    world.provider.end_mode.store(4, Ordering::SeqCst);
    let user = world.user().await?.ok_or("logout user absent")?;
    let user_id = user.0.parse::<cannery_core::ids::UserId>()?;
    let token = secrets::new_secret(secrets::PERSONAL_PREFIX)?;
    let mut conn = world
        .state
        .pool
        .acquire()
        .await
        .map_err(|_| "token fixture connection failed")?;
    repo::create_token(
        &mut conn,
        NewToken {
            secret_digest: token.digest(),
            display_prefix: token.display_prefix(),
            kind: TokenKind::Personal,
            user_id: Some(user_id),
            service_account_id: None,
            name: "browser-logout-fixture",
            scopes: &[Scope::Read],
            expires_in_days: 1,
        },
    )
    .await?;
    drop(conn);
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {}", token.plaintext().expose()))?,
    );
    let reply = call(&world.app, Method::POST, "/auth/logout", headers.clone()).await?;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(
        cookie(&reply.headers, "cr_session")?
            .expose()
            .starts_with("cr_session=\"\"")
    );
    assert_eq!(
        call(&world.app, Method::GET, "/api/me", headers)
            .await?
            .status,
        StatusCode::OK
    );
    let ended: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action='session.ended' AND actor_user_id::text=$1 AND via_channel='ui'").bind(&user.0).fetch_one(&world.state.pool).await.map_err(|_| "logout audit verification failed")?;
    assert_eq!(ended, 5);
    Ok(())
}
#[tokio::test]
#[ignore = "requires uniquely owned Rust-migrated PostgreSQL"]
async fn audit_and_ttl_failures_roll_back_user_and_session_but_consume_pending() -> Result<()> {
    let world = World::new("rollback", |settings| {
        settings.auth.bootstrap_admin_emails = vec!["admin@example.test".into()];
    })
    .await?;
    assert_eq!(
        world.callback(&world.login("").await?, "ok").await?.status,
        StatusCode::FOUND
    );
    let baseline = world.user().await?.ok_or("baseline user absent")?;
    // One exact actor is affected, only for future inserts; existing audit rows remain valid.
    let guard = format!(
        "ALTER TABLE audit_events ADD CONSTRAINT browser_fixture_reject CHECK (action <> 'session.created' OR actor_user_id::text <> '{}') NOT VALID",
        baseline.0
    );
    sqlx::query(&guard)
        .execute(&world.state.pool)
        .await
        .map_err(|_| "audit fixture setup failed")?;
    let pending = world.login("").await?;
    assert_eq!(
        world.callback(&pending, "updated").await?.status,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(world.pending_count().await?, 0);
    assert_eq!(
        world.user().await?.ok_or("rolled back user absent")?,
        baseline
    );
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id::text=$1")
        .bind(&baseline.0)
        .fetch_one(&world.state.pool)
        .await
        .map_err(|_| "session rollback verification failed")?;
    assert_eq!(sessions, 1);
    sqlx::query("ALTER TABLE audit_events DROP CONSTRAINT browser_fixture_reject")
        .execute(&world.state.pool)
        .await
        .map_err(|_| "audit fixture cleanup failed")?;
    for (label, ttl) in [
        ("ttl-int4", 2_147_483_648_i64),
        ("ttl-datetime", 100_000_000_i64),
    ] {
        let overflow = World::new(label, |settings| {
            settings.auth.session_ttl_hours = ttl.into();
            settings.auth.bootstrap_admin_emails = vec!["admin@example.test".into()];
        })
        .await?;
        assert_eq!(
            overflow
                .callback(&overflow.login("").await?, "ok")
                .await?
                .status,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert!(overflow.user().await?.is_none());
        assert_eq!(overflow.pending_count().await?, 0);
        let leaked: i64 =
            sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE new_state->>'issuer'=$1")
                .bind(&overflow.provider.issuer)
                .fetch_one(&overflow.state.pool)
                .await
                .map_err(|_| "TTL audit verification failed")?;
        assert_eq!(leaked, 0);
    }
    Ok(())
}
#[test]
fn provider_debug_never_exposes_secret_or_rejection_values() {
    let authorization = AuthorizationRequest {
        url: text("https://fixture.invalid/?secret=synthetic-marker"),
        state: Secret::new("synthetic-marker".into()),
        nonce: Secret::new("synthetic-marker".into()),
        code_verifier: Secret::new("synthetic-marker".into()),
    };
    assert!(!format!("{authorization:?}").contains("synthetic-marker"));
    assert!(
        !format!("{:?}", ProviderError::Rejected(text("synthetic-marker")))
            .contains("synthetic-marker")
    );
}

async fn verify_login_storage(world: &World, user_id: &str, session: &Secret) -> Result<()> {
    let events: Vec<(String, String, String)> = sqlx::query_as("SELECT action,actor_kind,via_channel FROM audit_events WHERE subject_id=$1 OR actor_user_id::text=$1 ORDER BY seq").bind(user_id).fetch_all(&world.state.pool).await.map_err(|_| "audit fixture read failed")?;
    assert_eq!(
        events,
        vec![
            ("user.created".into(), "system".into(), "system".into()),
            (
                "user.admin_granted".into(),
                "system".into(),
                "system".into()
            ),
            ("session.created".into(), "user".into(), "ui".into())
        ]
    );
    let secret = session
        .expose()
        .strip_prefix("cr_session=")
        .ok_or("session prefix absent")?;
    let digest: Vec<u8> =
        sqlx::query_scalar("SELECT secret_hash FROM sessions WHERE user_id::text=$1")
            .bind(user_id)
            .fetch_one(&world.state.pool)
            .await
            .map_err(|_| "session fixture read failed")?;
    assert!(digest == secrets::digest(secret), "session digest mismatch");
    Ok(())
}

#[tokio::test]
#[ignore = "requires uniquely owned Rust-migrated PostgreSQL"]
async fn service_logout_is_refused_and_missing_provider_still_ends_browser_session() -> Result<()> {
    let world = World::new("service-logout", |_| {}).await?;
    let signed_in = world.callback(&world.login("").await?, "ok").await?;
    let session = cookie(&signed_in.headers, "cr_session")?;
    let current = me(&world, &session).await?;
    let csrf = current.body["csrf_token"].as_str().ok_or("CSRF missing")?;
    let user = world.user().await?.ok_or("service fixture user missing")?;
    let user_id = user.0.parse::<cannery_core::ids::UserId>()?;
    let mut connection = world
        .state
        .pool
        .acquire()
        .await
        .map_err(|_| "service fixture connection failed")?;
    let project = cannery_projects::repo::create_project(
        &mut connection,
        "browser-service-fixture",
        "Browser service fixture",
        "",
        user_id,
    )
    .await?
    .ok_or("service fixture project collision")?;
    let account = repo::create_service_account(
        &mut connection,
        NewServiceAccount {
            project_id: project.id,
            kind: ServiceKind::Agent,
            name: "browser-service",
            description: "fixture only",
            created_by: user_id,
        },
    )
    .await?;
    let secret = secrets::new_secret(secrets::SERVICE_PREFIX)?;
    let token = repo::create_token(
        &mut connection,
        NewToken {
            secret_digest: secret.digest(),
            display_prefix: secret.display_prefix(),
            kind: TokenKind::Service,
            user_id: None,
            service_account_id: Some(account.id),
            name: "browser-service-logout",
            scopes: &[Scope::Read],
            expires_in_days: 1,
        },
    )
    .await?;
    drop(connection);
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {}", secret.plaintext().expose()))?,
    );
    rejection(
        &call(&world.app, Method::POST, "/auth/logout", headers).await?,
        403,
        "forbidden",
        "only people can do this, not service accounts",
    );
    let touched = repo::get_token(
        &mut *world
            .state
            .pool
            .acquire()
            .await
            .map_err(|_| "token touch connection failed")?,
        token.id,
    )
    .await?
    .ok_or("service token disappeared")?;
    assert!(touched.last_used_at.is_some());
    rejection(
        &call(&world.app, Method::POST, "/auth/logout", HeaderMap::new()).await?,
        401,
        "unauthenticated",
        "authentication required",
    );
    let (disabled, disabled_state) = application((*world.state.settings).clone())?;
    let mut headers = cookie_headers(&session)?;
    headers.insert("x-csrf-token", HeaderValue::from_str(csrf)?);
    let ended = call(&disabled, Method::POST, "/auth/logout", headers).await?;
    assert_eq!(ended.status, StatusCode::OK);
    assert_eq!(ended.body, json!({"logout_url":null}));
    assert!(
        cookie(&ended.headers, "cr_session")?
            .expose()
            .starts_with("cr_session=\"\"")
    );
    assert_eq!(me(&world, &session).await?.status, StatusCode::UNAUTHORIZED);
    assert!(
        world
            .provider
            .redirect_uri
            .lock()
            .map_err(|_| "provider fixture lock")?
            .is_none()
    );
    disabled_state.pool.close().await;
    Ok(())
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
