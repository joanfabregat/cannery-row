//! Shared accountable identities keep authorization independent of transport.

use crate::{
    errors::{DomainError, ErrorCode},
    ids::{ProjectId, ServiceAccountId, SessionId, UserId},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fmt};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Ui,
    Api,
    Mcp,
    Cli,
    System,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Read,
    Write,
}

impl Scope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceKind {
    Agent,
    Experimenter,
    Verifier,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Viewer,
    Member,
    Researcher,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Via {
    pub channel: Channel,
    pub client: Option<String>,
}

/// Secret-bearing values require an explicit exposure at their use site.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret([redacted])")
    }
}

#[derive(Clone, Debug)]
pub struct UserPrincipal {
    pub user_id: UserId,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub is_admin: bool,
    pub via: Via,
    pub scopes: BTreeSet<Scope>,
    pub session_id: Option<SessionId>,
    pub csrf_token: Option<Secret>,
}

#[derive(Clone, Debug)]
pub struct ServicePrincipal {
    pub service_account_id: ServiceAccountId,
    pub project_id: ProjectId,
    pub kind: ServiceKind,
    pub name: String,
    pub via: Via,
    pub scopes: BTreeSet<Scope>,
}

#[derive(Clone, Debug)]
pub enum Principal {
    User(UserPrincipal),
    Service(ServicePrincipal),
}

impl Principal {
    #[must_use]
    pub fn scopes(&self) -> &BTreeSet<Scope> {
        match self {
            Self::User(user) => &user.scopes,
            Self::Service(service) => &service.scopes,
        }
    }

    #[must_use]
    pub const fn via(&self) -> &Via {
        match self {
            Self::User(user) => &user.via,
            Self::Service(service) => &service.via,
        }
    }

    #[must_use]
    pub fn with_channel(mut self, channel: Channel, client: Option<String>) -> Self {
        let via = match &mut self {
            Self::User(user) => &mut user.via,
            Self::Service(service) => &mut service.via,
        };
        *via = Via { channel, client };
        self
    }

    /// Require a token scope without broadening its accountable identity.
    ///
    /// # Errors
    /// Returns `forbidden` when the requested scope is absent.
    pub fn require_scope(&self, scope: Scope) -> Result<(), DomainError> {
        if self.scopes().contains(&scope) {
            Ok(())
        } else {
            Err(DomainError::new(
                ErrorCode::Forbidden,
                format!("this token lacks the '{}' scope", scope.as_str()),
            ))
        }
    }

    /// Require a human principal.
    ///
    /// # Errors
    /// Returns `forbidden` for service accounts.
    pub fn require_user(&self) -> Result<&UserPrincipal, DomainError> {
        match self {
            Self::User(user) => Ok(user),
            Self::Service(_) => Err(DomainError::new(
                ErrorCode::Forbidden,
                "only people can do this, not service accounts",
            )),
        }
    }

    /// Require an installation administrator, and optionally its write scope.
    ///
    /// # Errors
    /// Returns `forbidden` for a service, nonadministrator, or insufficient scope.
    pub fn require_admin(&self, write: bool) -> Result<&UserPrincipal, DomainError> {
        let user = self.require_user()?;
        if !user.is_admin {
            return Err(DomainError::new(
                ErrorCode::Forbidden,
                "installation administrators only",
            ));
        }
        if write {
            self.require_scope(Scope::Write)?;
        }
        Ok(user)
    }
}

#[must_use]
pub fn all_scopes() -> BTreeSet<Scope> {
    BTreeSet::from([Scope::Read, Scope::Write])
}
