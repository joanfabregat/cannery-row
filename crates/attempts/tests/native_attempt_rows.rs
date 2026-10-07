//! Stock `SQLx` must decode every attempt timestamp through its checked row mapping.
use cannery_attempts::{
    model::JsonContext,
    repo::{CreateUpload, Repository},
};
use cannery_core::{
    db::{load_migrations, migrate_connection},
    ids::{AttemptId, HypothesisId, ProjectId},
};
use sqlx::{Connection, PgConnection, postgres::PgConnectOptions};
use std::{
    error::Error,
    str::FromStr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

async fn exercise(options: PgConnectOptions) -> Result<()> {
    let mut connection = PgConnection::connect_with(&options).await?;
    initialize(&mut connection).await?;
    check_attempt_reads(&mut connection).await?;
    connection.close().await?;
    Ok(())
}

async fn initialize(connection: &mut PgConnection) -> Result<()> {
    migrate_connection(connection, &load_migrations()?, Duration::from_secs(30)).await?;
    sqlx::raw_sql(include_str!("fixtures/seed.sql"))
        .execute(connection)
        .await?;
    Ok(())
}

async fn check_attempt_reads(connection: &mut PgConnection) -> Result<()> {
    let project = ProjectId("00000000-0000-0000-0000-000000000002".parse()?);
    let hypothesis = HypothesisId("00000000-0000-0000-0000-000000000011".parse()?);
    let first_id = AttemptId("00000000-0000-0000-0000-000000000021".parse()?);
    let second_id = AttemptId("00000000-0000-0000-0000-000000000022".parse()?);
    let missing = AttemptId("00000000-0000-0000-0000-000000000099".parse()?);
    let context = JsonContext {
        encode_nesting_budget: 64,
        decode_nesting_budget: 64,
    };
    let mut transaction = connection.begin().await?;
    let mut repository = Repository::new(&mut transaction, context);
    let first = repository
        .get_attempt_by_id(first_id, false)
        .await?
        .ok_or("claimed attempt absent")?;
    assert_eq!(first.id, first_id);
    check_claimed_timestamps(&first);
    let second = repository
        .get_attempt_by_id(second_id, false)
        .await?
        .ok_or("testing attempt absent")?;
    assert_eq!(second.id, second_id);
    assert!(second.lease_expires_at.is_none() && second.deadline.is_none());
    assert_eq!(
        repository
            .get_attempt_by_id(first_id, true)
            .await?
            .ok_or("locked attempt absent")?
            .id,
        first_id
    );
    assert_eq!(
        repository
            .get_attempt(project, &1.into(), &1.into(), false)
            .await?
            .ok_or("numbered attempt absent")?
            .id,
        first_id
    );
    assert_eq!(
        repository
            .get_attempt(project, &1.into(), &1.into(), true)
            .await?
            .ok_or("locked numbered attempt absent")?
            .id,
        first_id
    );
    assert_eq!(
        repository
            .list_attempts(hypothesis, None, None)
            .await?
            .iter()
            .map(|attempt| attempt.id)
            .collect::<Vec<_>>(),
        [first_id]
    );
    let rows = repository
        .list_project_attempts(project, None, None, None, &10.into())
        .await?;
    assert!(rows.iter().any(|attempt| attempt.id == first_id));
    assert!(rows.iter().any(|attempt| attempt.id == second_id));
    let page = repository
        .list_project_attempts(project, None, None, Some(second_id), &10.into())
        .await?;
    assert_eq!(
        page.iter().map(|attempt| attempt.id).collect::<Vec<_>>(),
        [first_id]
    );
    assert!(
        repository
            .get_attempt_by_id(missing, false)
            .await?
            .is_none()
    );
    assert!(
        repository
            .get_attempt(project, &99.into(), &1.into(), false)
            .await?
            .is_none()
    );
    assert!(
        repository
            .list_attempts(hypothesis, Some(&1.into()), None)
            .await?
            .is_empty()
    );
    check_upload_expiration(&mut repository, first_id).await?;
    transaction.rollback().await?;
    Ok(())
}

fn check_claimed_timestamps(first: &cannery_attempts::model::Attempt) {
    assert_eq!(
        first.claimed_at.isoformat(),
        "2025-01-02T03:04:05.123456+00:00"
    );
    assert!(first.lease_expires_at.is_some());
    assert!(first.deadline.is_some());
    assert!(
        first.started_at.is_none() && first.submitted_at.is_none() && first.finished_at.is_none()
    );
}

async fn check_upload_expiration(
    repository: &mut Repository<'_>,
    attempt: AttemptId,
) -> Result<()> {
    let expected_expiration =
        i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())? + 15 * 60;
    let lifetime = 15.into();
    let upload = repository
        .create_upload(CreateUpload {
            attempt_id: attempt,
            lease_generation: &1.into(),
            token_hash: &[9; 32],
            role: "result",
            backend: "s3",
            bucket: "fixture",
            key: "native-regression",
            declared_size: &17.into(),
            declared_sha256: &"0".repeat(64),
            media_type: "application/json",
            ttl_minutes: &lifetime,
            max_stream_seconds: &60.into(),
            job_id: None,
            interface: None,
            transfer: "stream",
            multipart_upload_id: None,
            part_size: None,
            slot: None,
        })
        .await?
        .ok_or("upload grant absent")?;
    assert_eq!(upload.attempt_id, attempt);
    assert_eq!(upload.state, cannery_attempts::model::UploadState::Pending);
    assert!((upload.expires_at.0.timestamp() - expected_expiration).abs() <= 5);
    Ok(())
}

#[tokio::test]
#[ignore = "requires the task-owned PostgreSQL administrator URL"]
async fn stock_sqlx_attempt_reads_decode_nullable_and_required_timestamps() -> Result<()> {
    let administrator = std::env::var("CANNERY_TEST_DATABASE_URL")?;
    let options = PgConnectOptions::from_str(&administrator)?;
    if options.get_database() != Some("postgres")
        || !matches!(options.get_host(), "db" | "localhost" | "127.0.0.1" | "::1")
    {
        return Err("task-owned administrator database required".into());
    }
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() & ((1_u128 << 96) - 1);
    let database = format!("conformance_{nonce:024x}");
    let mut admin = PgConnection::connect_with(&options).await?;
    sqlx::raw_sql(&format!("CREATE DATABASE \"{database}\""))
        .execute(&mut admin)
        .await?;
    let child = options.database(&database);
    // Await a worker before cleanup so a failing assertion cannot leak the database.
    let outcome = tokio::task::spawn_blocking(move || -> Result<()> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(exercise(child))
    })
    .await;
    sqlx::raw_sql(&format!("DROP DATABASE \"{database}\" WITH (FORCE)"))
        .execute(&mut admin)
        .await?;
    admin.close().await?;
    outcome.map_err(|_| "attempt row regression worker failed")??;
    Ok(())
}
