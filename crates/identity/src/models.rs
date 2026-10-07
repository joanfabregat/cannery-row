//! Typed identity records. Stored discriminator strings never escape this crate.
use cannery_core::{
    ids::{ProjectId, ServiceAccountId, SessionId, TokenId, UserId},
    principal::{Scope, Secret, ServiceKind},
    timestamps::Timestamp,
};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct User {
    pub id: UserId,
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub display_name: Option<String>,
    pub is_admin: bool,
    pub created_at: Timestamp,
    pub last_login_at: Option<Timestamp>,
}
#[derive(Clone, Debug, Serialize)]
pub struct ServiceAccount {
    pub id: ServiceAccountId,
    pub project_id: ProjectId,
    pub kind: ServiceKind,
    pub name: String,
    pub description: String,
    pub created_by: UserId,
    pub created_at: Timestamp,
    pub disabled_at: Option<Timestamp>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenKind {
    Personal,
    Service,
}
impl TokenKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Service => "service",
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct ApiToken {
    pub id: TokenId,
    pub display_prefix: String,
    pub kind: TokenKind,
    pub user_id: Option<UserId>,
    pub service_account_id: Option<ServiceAccountId>,
    pub name: String,
    // Keep source order/multiplicity in the repository. Authentication converts
    // this list into the principal's scope set.
    pub scopes: Vec<Scope>,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
    pub last_used_at: Option<Timestamp>,
    pub revoked_at: Option<Timestamp>,
}
#[derive(Clone, Debug)]
pub struct SessionRow {
    pub id: SessionId,
    pub user_id: UserId,
    pub csrf_token: Secret,
    pub expires_at: Timestamp,
}
#[derive(Clone, Debug)]
pub struct LoginUpsert {
    pub user: User,
    pub created: bool,
    pub admin_granted: bool,
}
/// Sensitive OIDC state consumed exactly once, including on binding mismatch.
#[derive(Clone, Debug)]
pub struct LoginRequest {
    pub nonce: Secret,
    pub code_verifier: Secret,
    pub return_to: String,
}
