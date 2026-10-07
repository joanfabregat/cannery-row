//! Live attribution, storage and transaction guarantees for the shared writer.
#![forbid(unsafe_code)]

use cannery_core::{
    audit::{self, Actor, ActorKind, Attribution, Record},
    db::DatabaseOptions,
    ids::{ServiceAccountId, UserId},
    principal::{Channel, Via},
};
use serde_json::{Value, json};
use sqlx::Connection;
use std::{error::Error, str::FromStr, time::Duration};

fn event<'a>(subject: &'a str, prior: Option<&'a Value>, new: Option<&'a Value>) -> Record<'a> {
    Record {
        action: "fixture.audit",
        subject_type: "fixture",
        subject_id: subject,
        project_id: None,
        prior_state: prior,
        new_state: new,
        reason: Some("fixture"),
        idempotency_key: Some("fixture-idempotency"),
    }
}

async fn setup(connection: &mut sqlx::PgConnection) -> Result<(), Box<dyn Error>> {
    sqlx::raw_sql(
        "INSERT INTO users (id,issuer,subject) VALUES
         ('a301ba40-de4b-441e-bb5d-740bff7c192a','fixture','audit');
         INSERT INTO projects (id,slug,title,created_by) VALUES
         ('a301ba40-de4b-441e-bb5d-740bff7c192b','audit-fixture','Audit fixture',
          'a301ba40-de4b-441e-bb5d-740bff7c192a');
         INSERT INTO service_accounts (id,project_id,kind,name,created_by) VALUES
         ('a301ba40-de4b-441e-bb5d-740bff7c192c','a301ba40-de4b-441e-bb5d-740bff7c192b',
          'agent','audit-agent','a301ba40-de4b-441e-bb5d-740bff7c192a')",
    )
    .execute(connection)
    .await
    .map_err(|_| "audit fixture setup failed")?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires an isolated migrated PostgreSQL database"]
async fn audit_attribution_storage_pagination_and_rollback() -> Result<(), Box<dyn Error>> {
    let dsn = std::env::var("CANNERY_AUDIT_TEST_DATABASE_URL")
        .map_err(|_| "CANNERY_AUDIT_TEST_DATABASE_URL is required")?;
    let mut connection = DatabaseOptions::parse(&dsn)?
        .connect(Some(Duration::from_secs(5)))
        .await?;
    let mut transaction = connection.begin().await?;
    setup(&mut transaction).await?;
    let user = Actor::User {
        id: UserId::from_str("a301ba40-de4b-441e-bb5d-740bff7c192a")?,
        via: Via {
            channel: Channel::Api,
            client: Some("token:fixture".to_owned()),
        },
    };
    let service = Actor::Service {
        id: ServiceAccountId::from_str("a301ba40-de4b-441e-bb5d-740bff7c192c")?,
        via: Via {
            channel: Channel::Mcp,
            client: Some("fixture-agent".to_owned()),
        },
    };
    let subject = "audit-rollback-fixture";
    let prior = json!({"nested": [false, 1.5, "é"], "null": null});
    let new = json!({"state": "ready"});
    let first = audit::record(
        &mut transaction,
        Attribution::Capability(&user),
        event(subject, Some(&prior), Some(&new)),
    )
    .await?;
    let second = audit::record(
        &mut transaction,
        Attribution::Capability(&service),
        event(subject, Some(&Value::Null), None),
    )
    .await?;
    let cli = Via {
        channel: Channel::Cli,
        client: Some("cannery import".to_owned()),
    };
    let third = audit::record(
        &mut transaction,
        Attribution::System(Some(&cli)),
        event(subject, None, None),
    )
    .await?;
    let fourth = audit::record(
        &mut transaction,
        Attribution::System(None),
        event(subject, None, None),
    )
    .await?;
    assert!(first < second && second < third && third < fourth);
    let rows = audit::history(&mut transaction, "fixture", subject, None, None).await?;
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].actor_kind, ActorKind::User);
    assert_eq!(rows[0].via_channel, Channel::Api);
    assert_eq!(rows[0].via_client.as_deref(), Some("token:fixture"));
    assert_eq!(rows[0].prior_state, prior);
    assert_eq!(rows[0].new_state, new);
    assert_eq!(rows[1].actor_kind, ActorKind::Service);
    assert_eq!(rows[1].via_channel, Channel::Mcp);
    assert_eq!(
        rows[1].actor_service_id,
        Some(ServiceAccountId::from_str(
            "a301ba40-de4b-441e-bb5d-740bff7c192c"
        )?)
    );
    assert_eq!(rows[1].prior_state, Value::Null);
    assert_eq!(rows[2].actor_kind, ActorKind::System);
    assert_eq!(rows[2].via_channel, Channel::Cli);
    assert_eq!(rows[3].via_channel, Channel::System);
    assert!(rows[3].via_client.is_none());
    let page = audit::history(&mut transaction, "fixture", subject, Some(second), Some(1)).await?;
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].seq, third);
    let nulls: bool = sqlx::query_scalar(
        "SELECT prior_state IS NULL AND new_state IS NULL AND
         idempotency_key = 'fixture-idempotency' FROM audit_events WHERE seq = $1",
    )
    .bind(second)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| "audit null storage check failed")?;
    assert!(nulls);
    transaction.rollback().await?;
    assert!(
        audit::history(&mut connection, "fixture", subject, None, None)
            .await?
            .is_empty()
    );
    Ok(())
}
