//! Append-only events share the caller's transaction and accountable identity.

use crate::{
    ids::{ProjectId, ServiceAccountId, UserId},
    principal::{Channel, Principal, Via},
    timestamps::Timestamp,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ActorKind {
    User,
    Service,
    System,
}

/// A capability can act for exactly one identity recorded when it was issued.
#[derive(Clone, Debug)]
pub enum Actor {
    User { id: UserId, via: Via },
    Service { id: ServiceAccountId, via: Via },
}

#[derive(Clone, Copy, Debug)]
pub enum Attribution<'a> {
    Principal(&'a Principal),
    Capability(&'a Actor),
    System(Option<&'a Via>),
}

pub struct Record<'a> {
    pub action: &'a str,
    pub subject_type: &'a str,
    pub subject_id: &'a str,
    pub project_id: Option<ProjectId>,
    pub prior_state: Option<&'a Value>,
    pub new_state: Option<&'a Value>,
    pub reason: Option<&'a str>,
    pub idempotency_key: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub struct AuditEvent {
    pub seq: i64,
    pub occurred_at: Timestamp,
    pub actor_kind: ActorKind,
    pub actor_user_id: Option<UserId>,
    pub actor_service_id: Option<ServiceAccountId>,
    pub via_channel: Channel,
    pub via_client: Option<String>,
    pub action: String,
    pub prior_state: Value,
    pub new_state: Value,
    pub reason: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("audit database operation failed")]
    Database,
    #[error("invalid stored audit attribution")]
    CorruptAttribution,
}

struct AttributionFields<'a> {
    kind: &'static str,
    user: Option<UserId>,
    service: Option<ServiceAccountId>,
    channel: Channel,
    client: Option<&'a str>,
}

impl<'a> Attribution<'a> {
    fn fields(self) -> AttributionFields<'a> {
        let (kind, user, service, via) = match self {
            Self::Principal(Principal::User(user)) => {
                ("user", Some(user.user_id), None, Some(&user.via))
            }
            Self::Principal(Principal::Service(service)) => (
                "service",
                None,
                Some(service.service_account_id),
                Some(&service.via),
            ),
            Self::Capability(Actor::User { id, via }) => ("user", Some(*id), None, Some(via)),
            Self::Capability(Actor::Service { id, via }) => ("service", None, Some(*id), Some(via)),
            Self::System(via) => ("system", None, None, via),
        };
        AttributionFields {
            kind,
            user,
            service,
            channel: via.map_or(Channel::System, |via| via.channel),
            client: via.and_then(|via| via.client.as_deref()),
        }
    }
}

const fn channel_name(channel: Channel) -> &'static str {
    match channel {
        Channel::Ui => "ui",
        Channel::Api => "api",
        Channel::Mcp => "mcp",
        Channel::Cli => "cli",
        Channel::System => "system",
    }
}

/// Append one event without opening or committing a transaction.
///
/// # Errors
/// Returns a sanitized database failure; the caller must roll back its transaction.
pub async fn record(
    connection: &mut sqlx::PgConnection,
    attribution: Attribution<'_>,
    event: Record<'_>,
) -> Result<i64, AuditError> {
    let actor = attribution.fields();
    // Python's None is SQL NULL, including a top-level JSON null passed as state.
    let prior = event.prior_state.filter(|value| !value.is_null());
    let new = event.new_state.filter(|value| !value.is_null());
    let result = sqlx::query!(
        r#"INSERT INTO audit_events (
              project_id, actor_kind, actor_user_id, actor_service_id, via_channel,
              via_client, action, subject_type, subject_id, prior_state, new_state,
              reason, idempotency_key)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
           RETURNING seq"#,
        event.project_id as _,
        actor.kind,
        actor.user as _,
        actor.service as _,
        channel_name(actor.channel),
        actor.client,
        event.action,
        event.subject_type,
        event.subject_id,
        prior as _,
        new as _,
        event.reason,
        event.idempotency_key,
    )
    .fetch_one(connection)
    .await
    .map_err(|_| AuditError::Database)?;
    Ok(result.seq)
}

struct StoredEvent {
    seq: i64,
    occurred_at: Timestamp,
    actor_kind: String,
    actor_user_id: Option<UserId>,
    actor_service_id: Option<ServiceAccountId>,
    via_channel: String,
    via_client: Option<String>,
    action: String,
    prior_state: Option<Value>,
    new_state: Option<Value>,
    reason: Option<String>,
}

impl TryFrom<StoredEvent> for AuditEvent {
    type Error = AuditError;

    fn try_from(event: StoredEvent) -> Result<Self, Self::Error> {
        let actor_kind = match event.actor_kind.as_str() {
            "user" => ActorKind::User,
            "service" => ActorKind::Service,
            "system" => ActorKind::System,
            _ => return Err(AuditError::CorruptAttribution),
        };
        let via_channel = match event.via_channel.as_str() {
            "ui" => Channel::Ui,
            "api" => Channel::Api,
            "mcp" => Channel::Mcp,
            "cli" => Channel::Cli,
            "system" => Channel::System,
            _ => return Err(AuditError::CorruptAttribution),
        };
        Ok(Self {
            seq: event.seq,
            occurred_at: event.occurred_at,
            actor_kind,
            actor_user_id: event.actor_user_id,
            actor_service_id: event.actor_service_id,
            via_channel,
            via_client: event.via_client,
            action: event.action,
            prior_state: event.prior_state.unwrap_or(Value::Null),
            new_state: event.new_state.unwrap_or(Value::Null),
            reason: event.reason,
        })
    }
}

/// Read a subject's events oldest first, optionally continuing after a sequence.
///
/// # Errors
/// Returns sanitized database or stored-attribution errors.
pub async fn history(
    connection: &mut sqlx::PgConnection,
    subject_type: &str,
    subject_id: &str,
    after: Option<i64>,
    limit: Option<i64>,
) -> Result<Vec<AuditEvent>, AuditError> {
    let rows = sqlx::query_as!(
        StoredEvent,
        r#"SELECT seq, occurred_at AS "occurred_at!: _", actor_kind,
                  actor_user_id AS "actor_user_id?: _",
                  actor_service_id AS "actor_service_id?: _", via_channel,
                  via_client, action, prior_state AS "prior_state?: _",
                  new_state AS "new_state?: _", reason
           FROM audit_events
           WHERE subject_type = $1 AND subject_id = $2
             AND ($3::bigint IS NULL OR seq > $3)
           ORDER BY seq LIMIT $4"#,
        subject_type,
        subject_id,
        after,
        limit,
    )
    .fetch_all(connection)
    .await
    .map_err(|_| AuditError::Database)?;
    rows.into_iter().map(AuditEvent::try_from).collect()
}
