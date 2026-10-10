//! Live PostgreSQL source-intent tests. Run only against an isolated migrated
//! fixture via `CANNERY_IDENTITY_TEST_DATABASE_URL`, never an installation database.
#![forbid(unsafe_code)]
use cannery_core::{
    db::DatabaseOptions,
    errors::ErrorCode,
    ids::ProjectId,
    principal::{Principal, Scope, Secret, ServiceKind},
};
use cannery_identity::{
    IdentityError, Result,
    auth::{self, AuthInput},
    models::TokenKind,
    repo::{self, LoginUser, NewServiceAccount, NewSession, NewToken, PendingLogin},
    secrets::{self, PERSONAL_PREFIX, SERVICE_PREFIX, SESSION_PREFIX},
};
use sqlx::{Connection, PgConnection};

async fn connection() -> Result<PgConnection> {
    let uri = std::env::var("CANNERY_IDENTITY_TEST_DATABASE_URL")
        .map_err(|_| IdentityError::CorruptData("test database environment"))?;
    let options =
        DatabaseOptions::parse(&uri).map_err(|_| IdentityError::Database { sqlstate: None })?;
    let mut conn = options
        .connect(None)
        .await
        .map_err(|_| IdentityError::Database { sqlstate: None })?;
    let database = sqlx::query_scalar!(r#"SELECT current_database() AS "database!""#)
        .fetch_one(&mut conn)
        .await
        .map_err(IdentityError::database)?;
    let suffix = database
        .strip_prefix("conformance_")
        .ok_or(IdentityError::CorruptData("isolated test database"))?;
    if suffix.len() != 24 || !suffix.bytes().all(|value| value.is_ascii_hexdigit()) {
        return Err(IdentityError::CorruptData("isolated test database"));
    }
    Ok(conn)
}
async fn user(
    conn: &mut PgConnection,
    subject: &str,
    admin: bool,
) -> Result<cannery_identity::models::User> {
    Ok(repo::upsert_login_user(
        conn,
        LoginUser {
            issuer: "https://identity.fixture",
            subject,
            email: Some("Verified@fixture.test"),
            email_verified: true,
            display_name: Some("Fixture"),
            make_admin: admin,
        },
    )
    .await?
    .user)
}
async fn token(
    conn: &mut PgConnection,
    user_id: cannery_core::ids::UserId,
    name: &str,
) -> Result<(secrets::NewSecret, cannery_identity::models::ApiToken)> {
    let secret = secrets::new_secret(PERSONAL_PREFIX)?;
    let row = repo::create_token(
        conn,
        NewToken {
            secret_digest: secret.digest(),
            display_prefix: secret.display_prefix(),
            kind: TokenKind::Personal,
            user_id: Some(user_id),
            service_account_id: None,
            name,
            scopes: &[Scope::Read, Scope::Write],
            expires_in_days: 1,
        },
    )
    .await?;
    Ok((secret, row))
}
fn input<'a>(
    authorization: Option<&'a str>,
    cookie: Option<&'a str>,
    csrf: Option<&'a str>,
    method: &'a str,
) -> AuthInput<'a> {
    AuthInput {
        authorization,
        session_cookie: cookie,
        csrf_token: csrf,
        method,
    }
}
fn error_code<T>(result: Result<T>, code: ErrorCode) {
    assert!(matches!(result,Err(IdentityError::Domain(error)) if error.code==code));
}

#[tokio::test]
#[ignore = "requires a uniquely owned migrated PostgreSQL fixture"]
async fn login_recovery_admin_grants_verified_lookup_and_caller_rollback() -> Result<()> {
    let mut conn = connection().await?;
    let mut tx = conn.begin().await.map_err(IdentityError::database)?;
    let created = repo::upsert_login_user(
        &mut tx,
        LoginUser {
            issuer: "https://identity.fixture",
            subject: "login",
            email: Some("Case@fixture.test"),
            email_verified: true,
            display_name: Some("First"),
            make_admin: false,
        },
    )
    .await?;
    assert!(created.created && !created.admin_granted && !created.user.is_admin);
    let granted = repo::upsert_login_user(
        &mut tx,
        LoginUser {
            issuer: "https://identity.fixture",
            subject: "login",
            email: Some("Case@fixture.test"),
            email_verified: true,
            display_name: Some("Second"),
            make_admin: true,
        },
    )
    .await?;
    assert!(!granted.created && granted.admin_granted && granted.user.is_admin);
    let refreshed = repo::upsert_login_user(
        &mut tx,
        LoginUser {
            issuer: "https://identity.fixture",
            subject: "login",
            email: Some("Case@fixture.test"),
            email_verified: true,
            display_name: None,
            make_admin: false,
        },
    )
    .await?;
    assert!(!refreshed.created && !refreshed.admin_granted && refreshed.user.is_admin);
    assert_eq!(refreshed.user.id, created.user.id);
    assert!(refreshed.user.display_name.is_none());
    tx.commit().await.map_err(IdentityError::database)?;
    // A fresh connection recovers persisted identity without relying on memory.
    let mut recovered = connection().await?;
    assert!(
        repo::get_user(&mut recovered, created.user.id)
            .await?
            .is_some_and(|user| user.is_admin)
    );
    let listed = repo::find_users_by_email(&mut recovered, "cASE@FIXTURE.TEST", None, None).await?;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.user.id);
    assert!(
        repo::find_users_by_email(&mut recovered, "fixture.test", None, None)
            .await?
            .is_empty()
    );
    assert!(
        repo::find_users_by_email(
            &mut recovered,
            "Case@fixture.test",
            Some(created.user.id),
            None
        )
        .await?
        .is_empty()
    );
    let mut tx = conn.begin().await.map_err(IdentityError::database)?;
    let unverified = repo::upsert_login_user(
        &mut tx,
        LoginUser {
            issuer: "https://identity.fixture",
            subject: "unverified",
            email: Some("Case@fixture.test"),
            email_verified: false,
            display_name: None,
            make_admin: false,
        },
    )
    .await?;
    assert_ne!(unverified.user.id, created.user.id);
    assert_eq!(
        repo::find_users_by_email(&mut tx, "Case@fixture.test", None, None)
            .await?
            .len(),
        1
    );
    let rollback_id = unverified.user.id;
    tx.rollback().await.map_err(IdentityError::database)?;
    assert!(repo::get_user(&mut conn, rollback_id).await?.is_none());
    Ok(())
}

#[tokio::test]
#[ignore = "requires a uniquely owned migrated PostgreSQL fixture"]
async fn bearer_session_csrf_precedence_and_genuine_postgres_expiry() -> Result<()> {
    let mut conn = connection().await?;
    let owner = user(&mut conn, "authentication", false).await?;
    let session_secret = secrets::new_secret(SESSION_PREFIX)?;
    let csrf = Secret::new("csrf-fixture-é".into());
    let session = repo::create_session(
        &mut conn,
        NewSession {
            user_id: owner.id,
            secret_digest: session_secret.digest(),
            csrf_token: &csrf,
            ttl_hours: 1,
        },
    )
    .await?;
    let (pat, token) = token(&mut conn, owner.id, "authentication").await?;
    let authorization = format!("Bearer {}", pat.plaintext().expose());
    let cookie = Some(session_secret.plaintext().expose());
    let principal = auth::current_principal(&mut conn, input(None, cookie, None, "GET")).await?;
    assert!(
        matches!(principal,Principal::User(user) if user.session_id==Some(session.id) && user.csrf_token.is_some())
    );
    error_code(
        auth::resolve_principal(&mut conn, input(None, cookie, None, "POST")).await,
        ErrorCode::CsrfInvalid,
    );
    error_code(
        auth::resolve_principal(&mut conn, input(None, cookie, Some("wrong"), "DELETE")).await,
        ErrorCode::CsrfInvalid,
    );
    assert!(
        auth::resolve_principal(&mut conn, input(None, cookie, Some(csrf.expose()), "POST"))
            .await?
            .is_some()
    );
    let bearer =
        auth::current_principal(&mut conn, input(Some(&authorization), cookie, None, "POST"))
            .await?;
    assert!(
        matches!(bearer,Principal::User(user) if user.session_id.is_none() && user.csrf_token.is_none() && user.via.client.as_deref()==Some("token:authentication"))
    );
    error_code(
        auth::resolve_principal(
            &mut conn,
            input(Some(""), cookie, Some(csrf.expose()), "GET"),
        )
        .await,
        ErrorCode::Unauthenticated,
    );
    error_code(
        auth::resolve_principal(
            &mut conn,
            input(Some("Bearer nonexistent"), cookie, None, "GET"),
        )
        .await,
        ErrorCode::Unauthenticated,
    );
    error_code(
        auth::bearer_principal(&mut conn, None).await,
        ErrorCode::Unauthenticated,
    );
    assert!(
        auth::resolve_principal(&mut conn, input(None, Some("invalid"), None, "GET"))
            .await?
            .is_none()
    );
    assert_stored_hash_and_expiry(
        conn,
        &session,
        &session_secret,
        &pat,
        &token,
        &authorization,
    )
    .await
}

async fn assert_stored_hash_and_expiry(
    mut conn: PgConnection,
    session: &cannery_identity::models::SessionRow,
    session_secret: &secrets::NewSecret,
    pat: &secrets::NewSecret,
    token: &cannery_identity::models::ApiToken,
    authorization: &str,
) -> Result<()> {
    let cookie = Some(session_secret.plaintext().expose());
    let correct_hash = sqlx::query_scalar!(
        r#"SELECT token_hash=$2 AS "matches!" FROM api_tokens WHERE id=$1"#,
        token.id as _,
        pat.digest().as_slice()
    )
    .fetch_one(&mut conn)
    .await
    .map_err(IdentityError::database)?;
    assert!(correct_hash);
    let correct_hash = sqlx::query_scalar!(
        r#"SELECT secret_hash=$2 AS "matches!" FROM sessions WHERE id=$1"#,
        session.id as _,
        session_secret.digest().as_slice()
    )
    .fetch_one(&mut conn)
    .await
    .map_err(IdentityError::database)?;
    assert!(correct_hash);
    // Read state without returning credential hashes or CSRF values to diagnostics.
    assert!(
        repo::get_token(&mut conn, token.id)
            .await?
            .is_some_and(|row| row.last_used_at.is_some())
    );
    sqlx::query!(
        "UPDATE sessions SET expires_at=now()+interval '2 seconds' WHERE id=$1",
        session.id as _
    )
    .execute(&mut conn)
    .await
    .map_err(IdentityError::database)?;
    sqlx::query!(
        "UPDATE api_tokens SET expires_at=now()+interval '2 seconds' WHERE id=$1",
        token.id as _
    )
    .execute(&mut conn)
    .await
    .map_err(IdentityError::database)?;
    assert!(
        auth::resolve_principal(&mut conn, input(None, cookie, None, "GET"))
            .await?
            .is_some()
    );
    assert!(
        auth::bearer_principal(&mut conn, Some(authorization))
            .await
            .is_ok()
    );
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    assert!(
        auth::resolve_principal(&mut conn, input(None, cookie, None, "GET"))
            .await?
            .is_none()
    );
    error_code(
        auth::bearer_principal(&mut conn, Some(authorization)).await,
        ErrorCode::Unauthenticated,
    );
    assert!(repo::delete_session(&mut conn, session.id).await?);
    assert!(!repo::delete_session(&mut conn, session.id).await?);
    conn.close().await.map_err(IdentityError::database)?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a uniquely owned migrated PostgreSQL fixture"]
async fn token_pagination_revoke_and_all_service_kinds_disable_once() -> Result<()> {
    let mut conn = connection().await?;
    let owner = user(&mut conn, "tokens", false).await?;
    let other = user(&mut conn, "foreign-token", false).await?;
    let (_, foreign) = token(&mut conn, other.id, "foreign").await?;
    let (_, first) = token(&mut conn, owner.id, "first").await?;
    let (secret, second) = token(&mut conn, owner.id, "second").await?;
    assert_sanitized_duplicate_hash(&mut conn, owner.id, &secret).await?;
    let page = repo::list_user_tokens(&mut conn, owner.id, None, Some(1)).await?;
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].id, second.id);
    let page = repo::list_user_tokens(&mut conn, owner.id, Some(second.id), None).await?;
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].id, first.id);
    assert!(
        repo::list_user_tokens(&mut conn, owner.id, Some(foreign.id), None)
            .await?
            .is_empty()
    );
    assert!(repo::revoke_token(&mut conn, second.id).await?.is_some());
    assert!(repo::revoke_token(&mut conn, second.id).await?.is_none());
    error_code(
        auth::bearer_principal(
            &mut conn,
            Some(&format!("Bearer {}", secret.plaintext().expose())),
        )
        .await,
        ErrorCode::Unauthenticated,
    );
    let project=sqlx::query_scalar!(r#"INSERT INTO projects(slug,title,created_by) VALUES('identity-fixture','Identity fixture',$1) RETURNING id AS "id: ProjectId""#,owner.id as _).fetch_one(&mut conn).await.map_err(IdentityError::database)?;
    for (kind, name) in [
        (ServiceKind::Agent, "agent"),
        (ServiceKind::Experimenter, "experimenter"),
        (ServiceKind::Verifier, "verifier"),
    ] {
        assert_service_kind(&mut conn, project, owner.id, foreign.id, kind, name).await?;
    }
    assert_eq!(
        repo::list_service_accounts(&mut conn, project, None, None)
            .await?
            .len(),
        3
    );
    let page =
        repo::list_service_accounts(&mut conn, project, Some("experimenter"), Some(1)).await?;
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].name, "verifier");
    Ok(())
}

async fn assert_sanitized_duplicate_hash(
    conn: &mut PgConnection,
    owner: cannery_core::ids::UserId,
    secret: &secrets::NewSecret,
) -> Result<()> {
    let result = repo::create_token(
        conn,
        NewToken {
            secret_digest: secret.digest(),
            display_prefix: secret.display_prefix(),
            kind: TokenKind::Personal,
            user_id: Some(owner),
            service_account_id: None,
            name: "duplicate",
            scopes: &[Scope::Read],
            expires_in_days: 1,
        },
    )
    .await;
    let Err(error) = result else {
        return Err(IdentityError::CorruptData("duplicate fixture accepted"));
    };
    assert_eq!(error.sqlstate(), Some("23505"));
    let diagnostic = format!("{error:?}: {error}");
    assert!(!diagnostic.contains(secret.plaintext().expose()));
    assert!(!diagnostic.contains("token_hash"));
    assert!(!diagnostic.contains("duplicate key"));
    Ok(())
}

async fn assert_service_kind(
    conn: &mut PgConnection,
    project: ProjectId,
    owner: cannery_core::ids::UserId,
    foreign: cannery_core::ids::TokenId,
    kind: ServiceKind,
    name: &str,
) -> Result<()> {
    let account = repo::create_service_account(
        conn,
        NewServiceAccount {
            project_id: project,
            kind,
            name,
            description: "fixture",
            created_by: owner,
        },
    )
    .await?;
    assert_eq!(account.kind, kind);
    assert!(
        repo::get_service_account_by_name(conn, project, name)
            .await?
            .is_some_and(|row| row.id == account.id)
    );
    let secret = secrets::new_secret(SERVICE_PREFIX)?;
    let token = repo::create_token(
        conn,
        NewToken {
            secret_digest: secret.digest(),
            display_prefix: secret.display_prefix(),
            kind: TokenKind::Service,
            user_id: None,
            service_account_id: Some(account.id),
            name,
            scopes: &[Scope::Read],
            expires_in_days: 1,
        },
    )
    .await?;
    let principal = auth::bearer_principal(
        conn,
        Some(&format!("Bearer {}", secret.plaintext().expose())),
    )
    .await?;
    assert!(
        matches!(principal,Principal::Service(service) if service.kind==kind && service.project_id==project && service.scopes.contains(&Scope::Read) && !service.scopes.contains(&Scope::Write))
    );
    assert_eq!(
        repo::list_service_tokens(conn, account.id, None, None)
            .await?
            .len(),
        1
    );
    assert!(
        repo::list_service_tokens(conn, account.id, Some(foreign), None)
            .await?
            .is_empty()
    );
    assert!(repo::get_token(conn, token.id).await?.is_some());
    assert!(
        repo::disable_service_account(conn, account.id)
            .await?
            .is_some()
    );
    assert!(
        repo::disable_service_account(conn, account.id)
            .await?
            .is_none()
    );
    error_code(
        auth::bearer_principal(
            conn,
            Some(&format!("Bearer {}", secret.plaintext().expose())),
        )
        .await,
        ErrorCode::Unauthenticated,
    );
    // Disabled account authentication still touches its otherwise-active
    // token before refusing it, exactly like Python's repository order.
    assert!(
        repo::lookup_active_token(conn, secret.digest())
            .await?
            .is_some()
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires a uniquely owned migrated PostgreSQL fixture"]
async fn pending_login_binding_is_consumed_once_and_expired_states_are_purged() -> Result<()> {
    let mut conn = connection().await?;
    let state = Secret::new("fixture-state".into());
    let nonce = Secret::new("fixture-nonce".into());
    let verifier = Secret::new("fixture-verifier".into());
    let binding = secrets::digest("fixture-browser");
    let pending = || PendingLogin {
        state: &state,
        nonce: &nonce,
        code_verifier: &verifier,
        return_to: "/projects",
        browser_hash: &binding,
    };
    repo::save_login_request(&mut conn, pending()).await?;
    assert!(
        repo::take_login_request(&mut conn, &state, &secrets::digest("wrong-browser"), 15)
            .await?
            .is_none()
    );
    assert!(
        repo::take_login_request(&mut conn, &state, &binding, 15)
            .await?
            .is_none()
    );
    repo::save_login_request(&mut conn, pending()).await?;
    let request = repo::take_login_request(&mut conn, &state, &binding, 15)
        .await?
        .ok_or(IdentityError::CorruptData("pending fixture"))?;
    assert!(
        request.nonce.expose() == nonce.expose()
            && request.code_verifier.expose() == verifier.expose()
    );
    assert_eq!(request.return_to, "/projects");
    assert!(
        repo::take_login_request(&mut conn, &state, &binding, 15)
            .await?
            .is_none()
    );
    repo::save_login_request(&mut conn, pending()).await?;
    sqlx::query!(
        "UPDATE oidc_login_requests SET created_at=now()-interval '16 minutes' WHERE state=$1",
        state.expose()
    )
    .execute(&mut conn)
    .await
    .map_err(IdentityError::database)?;
    assert!(
        repo::take_login_request(&mut conn, &state, &binding, 15)
            .await?
            .is_none()
    );
    Ok(())
}
