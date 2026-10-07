//! Source-compatible checked SQL. Every operation borrows a connection; callers
//! own transaction boundaries, including login upserts and state consumption.
use crate::{
    error::{IdentityError, Result},
    models::{ApiToken, LoginRequest, LoginUpsert, ServiceAccount, SessionRow, TokenKind, User},
};
use cannery_core::{
    ids::{ProjectId, ServiceAccountId, SessionId, TokenId, UserId},
    principal::{Scope, Secret, ServiceKind},
    timestamps::Timestamp,
};
use sqlx::PgConnection;
use subtle::ConstantTimeEq;

struct DbSession {
    id: SessionId,
    user_id: UserId,
    csrf_token: String,
    expires_at: Timestamp,
}
impl From<DbSession> for SessionRow {
    fn from(row: DbSession) -> Self {
        Self {
            id: row.id,
            user_id: row.user_id,
            csrf_token: Secret::new(row.csrf_token),
            expires_at: row.expires_at,
        }
    }
}
pub(crate) struct DbToken {
    pub(crate) id: TokenId,
    pub(crate) display_prefix: String,
    pub(crate) kind: String,
    pub(crate) user_id: Option<UserId>,
    pub(crate) service_account_id: Option<ServiceAccountId>,
    pub(crate) name: String,
    pub(crate) scopes: Vec<String>,
    pub(crate) created_at: Timestamp,
    pub(crate) expires_at: Timestamp,
    pub(crate) last_used_at: Option<Timestamp>,
    pub(crate) revoked_at: Option<Timestamp>,
}
impl TryFrom<DbToken> for ApiToken {
    type Error = IdentityError;
    fn try_from(row: DbToken) -> Result<Self> {
        let kind = match row.kind.as_str() {
            "personal" => TokenKind::Personal,
            "service" => TokenKind::Service,
            _ => return Err(IdentityError::CorruptData("token kind")),
        };
        let scopes = row
            .scopes
            .into_iter()
            .map(|scope| match scope.as_str() {
                "read" => Ok(Scope::Read),
                "write" => Ok(Scope::Write),
                _ => Err(IdentityError::CorruptData("token scope")),
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            id: row.id,
            display_prefix: row.display_prefix,
            kind,
            user_id: row.user_id,
            service_account_id: row.service_account_id,
            name: row.name,
            scopes,
            created_at: row.created_at,
            expires_at: row.expires_at,
            last_used_at: row.last_used_at,
            revoked_at: row.revoked_at,
        })
    }
}
pub(crate) struct DbService {
    pub(crate) id: ServiceAccountId,
    pub(crate) project_id: ProjectId,
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) created_by: UserId,
    pub(crate) created_at: Timestamp,
    pub(crate) disabled_at: Option<Timestamp>,
}
impl TryFrom<DbService> for ServiceAccount {
    type Error = IdentityError;
    fn try_from(row: DbService) -> Result<Self> {
        let kind = match row.kind.as_str() {
            "agent" => ServiceKind::Agent,
            "experimenter" => ServiceKind::Experimenter,
            "tester" => ServiceKind::Tester,
            "evaluator" => ServiceKind::Evaluator,
            _ => return Err(IdentityError::CorruptData("service kind")),
        };
        Ok(Self {
            id: row.id,
            project_id: row.project_id,
            kind,
            name: row.name,
            description: row.description,
            created_by: row.created_by,
            created_at: row.created_at,
            disabled_at: row.disabled_at,
        })
    }
}
fn service_kind(kind: ServiceKind) -> &'static str {
    match kind {
        ServiceKind::Agent => "agent",
        ServiceKind::Experimenter => "experimenter",
        ServiceKind::Tester => "tester",
        ServiceKind::Evaluator => "evaluator",
    }
}

pub struct LoginUser<'a> {
    pub issuer: &'a str,
    pub subject: &'a str,
    pub email: Option<&'a str>,
    pub email_verified: bool,
    pub display_name: Option<&'a str>,
    pub make_admin: bool,
}
/// Create/refresh an OIDC user with monotonic admin grants. Call inside a transaction.
/// # Errors
/// Returns a sanitized database failure.
pub async fn upsert_login_user(
    conn: &mut PgConnection,
    input: LoginUser<'_>,
) -> Result<LoginUpsert> {
    let prior = sqlx::query!(
        "SELECT is_admin FROM users WHERE issuer = $1 AND subject = $2 FOR UPDATE",
        input.issuer,
        input.subject
    )
    .fetch_optional(&mut *conn)
    .await
    .map_err(IdentityError::database)?;
    let user = sqlx::query_as!(User,r#"
        INSERT INTO users (issuer,subject,email,email_verified,display_name,is_admin,last_login_at)
        VALUES ($1,$2,$3,$4,$5,$6,now())
        ON CONFLICT (issuer,subject) DO UPDATE SET email=EXCLUDED.email,email_verified=EXCLUDED.email_verified,
          display_name=EXCLUDED.display_name,is_admin=users.is_admin OR EXCLUDED.is_admin,last_login_at=now()
        RETURNING id AS "id: _",issuer,subject,email,email_verified,display_name,is_admin,
          created_at AS "created_at: _",last_login_at AS "last_login_at: _"
    "#,input.issuer,input.subject,input.email,input.email_verified,input.display_name,input.make_admin)
        .fetch_one(conn).await.map_err(IdentityError::database)?;
    let was_admin = prior.as_ref().is_some_and(|row| row.is_admin);
    let admin_granted = user.is_admin && !was_admin;
    Ok(LoginUpsert {
        user,
        created: prior.is_none(),
        admin_granted,
    })
}
/// Get a user without changing its login state.
/// # Errors
/// Returns a sanitized database failure.
pub async fn get_user(conn: &mut PgConnection, user_id: UserId) -> Result<Option<User>> {
    sqlx::query_as!(
        User,
        r#"SELECT id AS "id: _",issuer,subject,email,email_verified,display_name,is_admin,
        created_at AS "created_at: _",last_login_at AS "last_login_at: _" FROM users WHERE id=$1"#,
        user_id as _
    )
    .fetch_optional(conn)
    .await
    .map_err(IdentityError::database)
}
/// Exact case-insensitive verified-email lookup, oldest first. A missing cursor
/// yields no rows, matching the source scalar subquery; no fuzzy identity lookup.
/// # Errors
/// Returns a sanitized database failure.
pub async fn find_users_by_email(
    conn: &mut PgConnection,
    email: &str,
    after: Option<UserId>,
    limit: Option<i64>,
) -> Result<Vec<User>> {
    sqlx::query_as!(
        User,
        r#"SELECT id AS "id: _",issuer,subject,email,email_verified,display_name,is_admin,
        created_at AS "created_at: _",last_login_at AS "last_login_at: _" FROM users
        WHERE lower(email)=lower($1) AND email_verified
          AND ($2::uuid IS NULL OR (created_at,id) > (SELECT created_at,id FROM users WHERE id=$2))
        ORDER BY created_at,id LIMIT $3"#,
        email,
        after as _,
        limit
    )
    .fetch_all(conn)
    .await
    .map_err(IdentityError::database)
}
pub struct NewSession<'a> {
    pub user_id: UserId,
    pub secret_digest: &'a [u8],
    pub csrf_token: &'a Secret,
    pub ttl_hours: i32,
}
/// Create an opaque browser session using PostgreSQL's current time.
/// # Errors
/// Returns a sanitized database failure.
pub async fn create_session(conn: &mut PgConnection, input: NewSession<'_>) -> Result<SessionRow> {
    sqlx::query_as!(
        DbSession,
        r#"INSERT INTO sessions(secret_hash,user_id,csrf_token,expires_at)
        VALUES($1,$2,$3,now()+make_interval(hours=>$4))
        RETURNING id AS "id: _",user_id AS "user_id: _",csrf_token,expires_at AS "expires_at: _""#,
        input.secret_digest,
        input.user_id as _,
        input.csrf_token.expose(),
        input.ttl_hours
    )
    .fetch_one(conn)
    .await
    .map(SessionRow::from)
    .map_err(IdentityError::database)
}
/// Touch an unexpired session and return its owner; expired credentials are absent.
/// # Errors
/// Returns a sanitized database failure.
pub async fn lookup_session(
    conn: &mut PgConnection,
    secret_digest: &[u8],
) -> Result<Option<(SessionRow, User)>> {
    let row = sqlx::query_as!(
        DbSession,
        r#"UPDATE sessions SET last_seen_at=now()
        WHERE secret_hash=$1 AND expires_at>now()
        RETURNING id AS "id: _",user_id AS "user_id: _",csrf_token,expires_at AS "expires_at: _""#,
        secret_digest
    )
    .fetch_optional(&mut *conn)
    .await
    .map_err(IdentityError::database)?;
    let Some(row) = row else { return Ok(None) };
    Ok(get_user(conn, row.user_id)
        .await?
        .map(|user| (row.into(), user)))
}
/// Delete one session; subsequent deletes return false.
/// # Errors
/// Returns a sanitized database failure.
pub async fn delete_session(conn: &mut PgConnection, session_id: SessionId) -> Result<bool> {
    sqlx::query!("DELETE FROM sessions WHERE id=$1", session_id as _)
        .execute(conn)
        .await
        .map(|result| result.rows_affected() > 0)
        .map_err(IdentityError::database)
}
pub struct PendingLogin<'a> {
    pub state: &'a Secret,
    pub nonce: &'a Secret,
    pub code_verifier: &'a Secret,
    pub return_to: &'a str,
    pub browser_hash: &'a [u8],
}
/// Save pending state and its browser binding, without opening a transaction.
/// # Errors
/// Returns a sanitized database failure.
pub async fn save_login_request(conn: &mut PgConnection, input: PendingLogin<'_>) -> Result<()> {
    sqlx::query!("INSERT INTO oidc_login_requests(state,nonce,code_verifier,return_to,browser_hash) VALUES($1,$2,$3,$4,$5)",
        input.state.expose(),input.nonce.expose(),input.code_verifier.expose(),input.return_to,input.browser_hash)
        .execute(conn).await.map(|_|()).map_err(IdentityError::database)
}
/// Delete expired requests globally, then consume matching state even on a wrong
/// browser binding. Caller must commit consumption on an unsuccessful callback.
/// # Errors
/// Returns a sanitized database failure.
pub async fn take_login_request(
    conn: &mut PgConnection,
    state: &Secret,
    browser_hash: &[u8],
    max_age_minutes: i32,
) -> Result<Option<LoginRequest>> {
    sqlx::query!(
        "DELETE FROM oidc_login_requests WHERE created_at < now()-make_interval(mins=>$1)",
        max_age_minutes
    )
    .execute(&mut *conn)
    .await
    .map_err(IdentityError::database)?;
    let row=sqlx::query!("DELETE FROM oidc_login_requests WHERE state=$1 RETURNING nonce,code_verifier,return_to,browser_hash",state.expose())
        .fetch_optional(conn).await.map_err(IdentityError::database)?;
    let Some(row) = row else { return Ok(None) };
    if !bool::from(row.browser_hash.as_slice().ct_eq(browser_hash)) {
        return Ok(None);
    }
    Ok(Some(LoginRequest {
        nonce: Secret::new(row.nonce),
        code_verifier: Secret::new(row.code_verifier),
        return_to: row.return_to,
    }))
}
pub struct NewToken<'a> {
    pub secret_digest: &'a [u8],
    pub display_prefix: &'a str,
    pub kind: TokenKind,
    pub user_id: Option<UserId>,
    pub service_account_id: Option<ServiceAccountId>,
    pub name: &'a str,
    pub scopes: &'a [Scope],
    pub expires_in_days: i32,
}
/// Create a personal/service token. Database constraints enforce its exact owner.
/// # Errors
/// Returns a sanitized database failure or invalid stored discriminator.
pub async fn create_token(conn: &mut PgConnection, input: NewToken<'_>) -> Result<ApiToken> {
    let scopes: Vec<_> = input
        .scopes
        .iter()
        .map(|scope| scope.as_str().to_owned())
        .collect();
    sqlx::query_as!(DbToken,r#"INSERT INTO api_tokens(token_hash,display_prefix,kind,user_id,service_account_id,name,scopes,expires_at)
        VALUES($1,$2,$3,$4,$5,$6,$7,now()+make_interval(days=>$8))
        RETURNING id AS "id: _",display_prefix,kind,user_id AS "user_id: _",service_account_id AS "service_account_id: _",
        name,scopes,created_at AS "created_at: _",expires_at AS "expires_at: _",last_used_at AS "last_used_at: _",revoked_at AS "revoked_at: _""#,
        input.secret_digest,input.display_prefix,input.kind.as_str(),input.user_id as _,input.service_account_id as _,input.name,&scopes,input.expires_in_days)
        .fetch_one(conn).await.map_err(IdentityError::database)?.try_into()
}
/// Touch a live token, including one whose service account has since disabled;
/// authentication rejects the latter after this source-compatible update.
/// # Errors
/// Returns a sanitized database failure or invalid stored discriminator.
pub async fn lookup_active_token(
    conn: &mut PgConnection,
    secret_digest: &[u8],
) -> Result<Option<ApiToken>> {
    sqlx::query_as!(DbToken,r#"UPDATE api_tokens SET last_used_at=now()
        WHERE token_hash=$1 AND revoked_at IS NULL AND expires_at>now()
        RETURNING id AS "id: _",display_prefix,kind,user_id AS "user_id: _",service_account_id AS "service_account_id: _",
        name,scopes,created_at AS "created_at: _",expires_at AS "expires_at: _",last_used_at AS "last_used_at: _",revoked_at AS "revoked_at: _""#,secret_digest)
        .fetch_optional(conn).await.map_err(IdentityError::database)?.map(TryInto::try_into).transpose()
}
/// Get token metadata, including revoked or expired tokens.
/// # Errors
/// Returns a sanitized database failure or invalid stored discriminator.
pub async fn get_token(conn: &mut PgConnection, token_id: TokenId) -> Result<Option<ApiToken>> {
    sqlx::query_as!(DbToken,r#"SELECT id AS "id: _",display_prefix,kind,user_id AS "user_id: _",service_account_id AS "service_account_id: _",
        name,scopes,created_at AS "created_at: _",expires_at AS "expires_at: _",last_used_at AS "last_used_at: _",revoked_at AS "revoked_at: _"
        FROM api_tokens WHERE id=$1"#,token_id as _)
        .fetch_optional(conn).await.map_err(IdentityError::database)?.map(TryInto::try_into).transpose()
}
/// List user tokens newest first; a foreign or absent cursor yields no results.
/// # Errors
/// Returns a sanitized database failure or invalid stored discriminator.
pub async fn list_user_tokens(
    conn: &mut PgConnection,
    user_id: UserId,
    before: Option<TokenId>,
    limit: Option<i64>,
) -> Result<Vec<ApiToken>> {
    sqlx::query_as!(DbToken,r#"SELECT id AS "id: _",display_prefix,kind,user_id AS "user_id: _",service_account_id AS "service_account_id: _",
        name,scopes,created_at AS "created_at: _",expires_at AS "expires_at: _",last_used_at AS "last_used_at: _",revoked_at AS "revoked_at: _"
        FROM api_tokens WHERE user_id=$1 AND ($2::uuid IS NULL OR (created_at,id)<(SELECT created_at,id FROM api_tokens WHERE id=$2 AND user_id=$1))
        ORDER BY created_at DESC,id DESC LIMIT $3"#,user_id as _,before as _,limit)
        .fetch_all(conn).await.map_err(IdentityError::database)?.into_iter().map(TryInto::try_into).collect()
}
/// List service tokens newest first; a foreign or absent cursor yields no results.
/// # Errors
/// Returns a sanitized database failure or invalid stored discriminator.
pub async fn list_service_tokens(
    conn: &mut PgConnection,
    service_account_id: ServiceAccountId,
    before: Option<TokenId>,
    limit: Option<i64>,
) -> Result<Vec<ApiToken>> {
    sqlx::query_as!(DbToken,r#"SELECT id AS "id: _",display_prefix,kind,user_id AS "user_id: _",service_account_id AS "service_account_id: _",
        name,scopes,created_at AS "created_at: _",expires_at AS "expires_at: _",last_used_at AS "last_used_at: _",revoked_at AS "revoked_at: _"
        FROM api_tokens WHERE service_account_id=$1 AND ($2::uuid IS NULL OR (created_at,id)<(SELECT created_at,id FROM api_tokens WHERE id=$2 AND service_account_id=$1))
        ORDER BY created_at DESC,id DESC LIMIT $3"#,service_account_id as _,before as _,limit)
        .fetch_all(conn).await.map_err(IdentityError::database)?.into_iter().map(TryInto::try_into).collect()
}
/// Revoke once; a missing or already revoked token is absent.
/// # Errors
/// Returns a sanitized database failure or invalid stored discriminator.
pub async fn revoke_token(conn: &mut PgConnection, token_id: TokenId) -> Result<Option<ApiToken>> {
    sqlx::query_as!(DbToken,r#"UPDATE api_tokens SET revoked_at=now() WHERE id=$1 AND revoked_at IS NULL
        RETURNING id AS "id: _",display_prefix,kind,user_id AS "user_id: _",service_account_id AS "service_account_id: _",
        name,scopes,created_at AS "created_at: _",expires_at AS "expires_at: _",last_used_at AS "last_used_at: _",revoked_at AS "revoked_at: _""#,token_id as _)
        .fetch_optional(conn).await.map_err(IdentityError::database)?.map(TryInto::try_into).transpose()
}
pub struct NewServiceAccount<'a> {
    pub project_id: ProjectId,
    pub kind: ServiceKind,
    pub name: &'a str,
    pub description: &'a str,
    pub created_by: UserId,
}
/// Create a typed service identity within a project.
/// # Errors
/// Returns a sanitized database failure or invalid stored discriminator.
pub async fn create_service_account(
    conn: &mut PgConnection,
    input: NewServiceAccount<'_>,
) -> Result<ServiceAccount> {
    sqlx::query_as!(DbService,r#"INSERT INTO service_accounts(project_id,kind,name,description,created_by) VALUES($1,$2,$3,$4,$5)
        RETURNING id AS "id: _",project_id AS "project_id: _",kind,name,description,created_by AS "created_by: _",
        created_at AS "created_at: _",disabled_at AS "disabled_at: _""#,input.project_id as _,service_kind(input.kind),input.name,input.description,input.created_by as _)
        .fetch_one(conn).await.map_err(IdentityError::database)?.try_into()
}
/// Get account metadata even if disabled.
/// # Errors
/// Returns a sanitized database failure or invalid stored discriminator.
pub async fn get_service_account(
    conn: &mut PgConnection,
    account_id: ServiceAccountId,
) -> Result<Option<ServiceAccount>> {
    sqlx::query_as!(DbService,r#"SELECT id AS "id: _",project_id AS "project_id: _",kind,name,description,created_by AS "created_by: _",
        created_at AS "created_at: _",disabled_at AS "disabled_at: _" FROM service_accounts WHERE id=$1"#,account_id as _)
        .fetch_optional(conn).await.map_err(IdentityError::database)?.map(TryInto::try_into).transpose()
}
/// Find one project-scoped account by exact name.
/// # Errors
/// Returns a sanitized database failure or invalid stored discriminator.
pub async fn get_service_account_by_name(
    conn: &mut PgConnection,
    project_id: ProjectId,
    name: &str,
) -> Result<Option<ServiceAccount>> {
    sqlx::query_as!(DbService,r#"SELECT id AS "id: _",project_id AS "project_id: _",kind,name,description,created_by AS "created_by: _",
        created_at AS "created_at: _",disabled_at AS "disabled_at: _" FROM service_accounts WHERE project_id=$1 AND name=$2"#,project_id as _,name)
        .fetch_optional(conn).await.map_err(IdentityError::database)?.map(TryInto::try_into).transpose()
}
/// List accounts alphabetically, including disabled records.
/// # Errors
/// Returns a sanitized database failure or invalid stored discriminator.
pub async fn list_service_accounts(
    conn: &mut PgConnection,
    project_id: ProjectId,
    after: Option<&str>,
    limit: Option<i64>,
) -> Result<Vec<ServiceAccount>> {
    sqlx::query_as!(DbService,r#"SELECT id AS "id: _",project_id AS "project_id: _",kind,name,description,created_by AS "created_by: _",
        created_at AS "created_at: _",disabled_at AS "disabled_at: _" FROM service_accounts
        WHERE project_id=$1 AND ($2::text IS NULL OR name>$2) ORDER BY name LIMIT $3"#,project_id as _,after,limit)
        .fetch_all(conn).await.map_err(IdentityError::database)?.into_iter().map(TryInto::try_into).collect()
}
/// Disable once; a missing or already disabled account is absent.
/// # Errors
/// Returns a sanitized database failure or invalid stored discriminator.
pub async fn disable_service_account(
    conn: &mut PgConnection,
    account_id: ServiceAccountId,
) -> Result<Option<ServiceAccount>> {
    sqlx::query_as!(DbService,r#"UPDATE service_accounts SET disabled_at=now() WHERE id=$1 AND disabled_at IS NULL
        RETURNING id AS "id: _",project_id AS "project_id: _",kind,name,description,created_by AS "created_by: _",
        created_at AS "created_at: _",disabled_at AS "disabled_at: _""#,account_id as _)
        .fetch_optional(conn).await.map_err(IdentityError::database)?.map(TryInto::try_into).transpose()
}
