//! Production recovery with real PostgreSQL locks and durable rollback evidence.
#![forbid(unsafe_code)]
use cannery_core::settings::load_settings;
use cannery_server::{
    AppState, production_application,
    sweeps::{self, SweepError, SweepReport, Task},
};
use sqlx::{Connection, PgConnection, PgPool, Row};
use std::{collections::BTreeMap, error::Error, path::PathBuf, time::Duration};
use tokio::task::JoinHandle;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
const UPLOAD: &str = "00000000-0000-0000-0000-000000001001";
const KEY: &str = "recovery/one";
const SEED: &str = r"
INSERT INTO users(id,issuer,subject) VALUES('00000000-0000-0000-0000-000000000001','recovery.fixture','owner');
INSERT INTO projects(id,slug,title,created_by) VALUES('00000000-0000-0000-0000-000000000010','recovery-proof','Recovery','00000000-0000-0000-0000-000000000001');
INSERT INTO tracks(id,project_id,slug,title,created_by) VALUES('00000000-0000-0000-0000-000000000020','00000000-0000-0000-0000-000000000010','track','Track','00000000-0000-0000-0000-000000000001');
INSERT INTO hypotheses(id,project_id,number,track_id,title,created_by_user) VALUES('00000000-0000-0000-0000-000000000030','00000000-0000-0000-0000-000000000010',1,'00000000-0000-0000-0000-000000000020','Hypothesis','00000000-0000-0000-0000-000000000001');
INSERT INTO attempts(id,project_id,hypothesis_id,sequence,state,hypothesis_revision,science_revision,track_id,claimed_by_user,via_channel,lease_generation)
VALUES('00000000-0000-0000-0000-000000000040','00000000-0000-0000-0000-000000000010','00000000-0000-0000-0000-000000000030',1,'failed',1,1,'00000000-0000-0000-0000-000000000020','00000000-0000-0000-0000-000000000001','cli',1);
INSERT INTO uploads(id,attempt_id,lease_generation,token_hash,role,backend,bucket,key,slot,declared_size,declared_sha256,media_type,expires_at)
VALUES('00000000-0000-0000-0000-000000001001','00000000-0000-0000-0000-000000000040',1,sha256('recovery-one'::bytea),'log','local','recovery-test','recovery/one','recovery/one',0,repeat('0',64),'text/plain',now()-interval '1 minute');
";
struct LocalRoot(PathBuf);
impl Drop for LocalRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
async fn state(timeout: f64) -> Result<(AppState, LocalRoot)> {
    let url = std::env::var("CANNERY_TEST_DATABASE_URL")?;
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(|_| "fixture randomness failed")?;
    let root = LocalRoot(std::env::temp_dir().join(format!(
        "cannery-recovery-{}",
        base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, nonce)
    )));
    let mut settings = load_settings(
        None,
        &BTreeMap::from([
            ("CANNERY_DATABASE_URL".into(), url),
            ("CANNERY_STORAGE_BACKEND".into(), "local".into()),
            ("CANNERY_STORAGE_BUCKET".into(), "recovery-test".into()),
            (
                "CANNERY_STORAGE_LOCAL_ROOT".into(),
                root.0.to_str().ok_or("fixture path")?.into(),
            ),
            ("CANNERY_DATABASE_POOL_MIN_SIZE".into(), "0".into()),
            ("CANNERY_DATABASE_POOL_MAX_SIZE".into(), "4".into()),
        ]),
    )?;
    settings.sweeps.interval_seconds = 3600.0;
    settings.sweeps.timeout_seconds = timeout;
    settings.sweeps.statement_timeout_seconds = 3.0;
    settings.sweeps.lock_timeout_seconds = 30.0;
    let (_, state, cancellation) = production_application(settings, "127.0.0.1,::1").await?;
    cancellation
        .drain()
        .await
        .map_err(|_| "fixture cancellation drain failed")?;
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    let suffix = database
        .strip_prefix("conformance_")
        .ok_or("owned database required")?;
    if suffix.len() != 24
        || !suffix
            .bytes()
            .all(|value| value.is_ascii_hexdigit() && !value.is_ascii_uppercase())
    {
        return Err("owned database required".into());
    }
    let version: String = sqlx::query_scalar("SHOW server_version_num")
        .fetch_one(&state.pool)
        .await?;
    assert_eq!(version, "170011");
    sqlx::raw_sql(SEED).execute(&state.pool).await?;
    Ok((state, root))
}
async fn control(state: &AppState) -> Result<PgConnection> {
    PgConnection::connect_with(state.database.connect_options())
        .await
        .map_err(|_| "fixture control connection failed".into())
}
async fn snapshot(pool: &PgPool) -> Result<Vec<String>> {
    Ok(
        sqlx::query_scalar("SELECT to_jsonb(u)::text FROM uploads u ORDER BY id")
            .fetch_all(pool)
            .await?,
    )
}
async fn audit_count(pool: &PgPool) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE subject_type='upload'")
            .fetch_one(pool)
            .await?,
    )
}
async fn holders(pool: &PgPool) -> Result<Vec<i32>> {
    Ok(sqlx::query_scalar(r"SELECT pid FROM pg_locks WHERE locktype='advisory' AND granted AND database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND classid::bigint=((hashtextextended('cannery_row.sweeps',0)>>32)&4294967295) AND objid::bigint=(hashtextextended('cannery_row.sweeps',0)&4294967295) AND objsubid=1 ORDER BY pid").fetch_all(pool).await?)
}
async fn barrier(
    pool: &PgPool,
    fragment: &str,
    operation: Option<&JoinHandle<std::result::Result<SweepReport, SweepError>>>,
) -> Result<i32> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if operation.is_some_and(JoinHandle::is_finished) {
            return Err("recovery settled before PostgreSQL barrier".into());
        }
        let owners = holders(pool).await?;
        if owners.len() == 1 {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid=$1 AND wait_event_type='Lock' AND position($2 in query)>0)").bind(owners[0]).bind(fragment).fetch_one(pool).await?;
            if waiting {
                return Ok(owners[0]);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("PostgreSQL recovery barrier was not reached".into());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
async fn gone(pool: &PgPool, pid: i32) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10),async {
        loop {
            let alive:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid=$1)").bind(pid).fetch_one(pool).await?;
            if !alive && holders(pool).await?.is_empty() {
                return Ok::<(), Box<dyn Error + Send + Sync>>(());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.map_err(|_|"recovery connection/advisory ownership remained after settlement")??;
    Ok(())
}
async fn no_statement_wait(pool: &PgPool, pid: i32) -> Result<()> {
    let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid=$1 AND wait_event_type='Lock')")
        .bind(pid).fetch_one(pool).await?;
    assert!(
        !waiting,
        "shutdown/timeout returned before its statement settled"
    );
    Ok(())
}
async fn older_than_run_timeout(pool: &PgPool, pid: i32) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let elapsed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid=$1 AND wait_event_type='Lock' AND clock_timestamp()-query_start >= interval '0.5 second')")
                .bind(pid).fetch_one(pool).await?;
            if elapsed { return Ok::<(), Box<dyn Error + Send + Sync>>(()); }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.map_err(|_| "statement did not outlive the configured run deadline")??;
    Ok(())
}
fn spawn(state: &AppState) -> JoinHandle<std::result::Result<SweepReport, SweepError>> {
    let state = state.clone();
    tokio::spawn(async move { sweeps::run(&state).await })
}
async fn settled(
    operation: JoinHandle<std::result::Result<SweepReport, SweepError>>,
) -> Result<()> {
    operation.abort();
    assert!(operation.await.is_err_and(|error| error.is_cancelled()));
    Ok(())
}
async fn recovered(state: &AppState, expected: u64) -> Result<()> {
    let prior_seq: i64 = sqlx::query_scalar("SELECT coalesce(max(seq),0) FROM audit_events")
        .fetch_one(&state.pool)
        .await?;
    let before: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&state.pool)
        .await?;
    let report = sweeps::run(state).await?;
    assert!(report.ran);
    assert_eq!(report.errors, 0);
    assert_eq!(report.uploads_expired, expected);
    let after: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&state.pool)
        .await?;
    let rows=sqlx::query("SELECT seq,subject_id,actor_kind,actor_user_id,actor_service_id,via_channel,via_client,action,prior_state::text,new_state::text,occurred_at FROM audit_events WHERE subject_type='upload' ORDER BY seq").fetch_all(&state.pool).await?;
    for row in rows {
        assert_eq!(row.try_get::<String, _>("actor_kind")?, "system");
        assert!(
            row.try_get::<Option<uuid::Uuid>, _>("actor_user_id")?
                .is_none()
        );
        assert!(
            row.try_get::<Option<uuid::Uuid>, _>("actor_service_id")?
                .is_none()
        );
        assert_eq!(row.try_get::<String, _>("via_channel")?, "system");
        assert!(row.try_get::<Option<String>, _>("via_client")?.is_none());
        assert_eq!(row.try_get::<String, _>("action")?, "upload.expired");
        assert_eq!(
            row.try_get::<String, _>("prior_state")?,
            r#"{"state": "pending"}"#
        );
        let stamp: chrono::DateTime<chrono::Utc> = row.try_get("occurred_at")?;
        if row.try_get::<i64, _>("seq")? > prior_seq {
            assert!(before <= stamp && stamp <= after);
        }
        let subject = row.try_get::<String, _>("subject_id")?;
        let key: String = sqlx::query_scalar("SELECT key FROM uploads WHERE id=$1::uuid")
            .bind(&subject)
            .fetch_one(&state.pool)
            .await?;
        assert_eq!(
            row.try_get::<String, _>("new_state")?,
            format!(r#"{{"key": "{key}", "state": "expired", "object_deleted": false}}"#)
        );
        let completed: chrono::DateTime<chrono::Utc> =
            sqlx::query_scalar("SELECT completed_at FROM uploads WHERE id=$1::uuid")
                .bind(&subject)
                .fetch_one(&state.pool)
                .await?;
        assert_eq!(completed, stamp);
    }
    let original = snapshot(&state.pool).await?;
    let count = audit_count(&state.pool).await?;
    let second = sweeps::run(state).await?;
    assert!(second.ran);
    assert_eq!(second.uploads_expired, 0);
    assert_eq!(second.errors, 0);
    assert_eq!(original, snapshot(&state.pool).await?);
    assert_eq!(count, audit_count(&state.pool).await?);
    assert_eq!(holders(&state.pool).await?, Vec::<i32>::new());
    Ok(())
}

#[tokio::test]
#[ignore = "requires a guarded fresh PostgreSQL 17.11 child; use recovery_tests launcher"]
async fn advisory_contention_and_candidate_cancellation() -> Result<()> {
    let (state, _root) = state(60.0).await?;
    let original = snapshot(&state.pool).await?;
    let mut controller = control(&state).await?;
    let mut lock = controller.begin().await?;
    sqlx::raw_sql("LOCK TABLE attempts IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *lock)
        .await?;
    let operation = spawn(&state);
    let pid = barrier(&state.pool, "FROM attempts", Some(&operation)).await?;
    let contender = sweeps::run(&state).await?;
    assert!(!contender.ran);
    assert_eq!(contender.uploads_expired, 0);
    assert_eq!(snapshot(&state.pool).await?, original);
    assert_eq!(holders(&state.pool).await?, vec![pid]);
    settled(operation).await?;
    gone(&state.pool, pid).await?;
    assert_eq!(snapshot(&state.pool).await?, original);
    lock.rollback().await?;
    recovered(&state, 1).await?;
    assert_eq!(audit_count(&state.pool).await?, 1);
    state.pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a guarded fresh PostgreSQL 17.11 child; use recovery_tests launcher"]
async fn cancelled_record_and_whole_deadline_roll_back() -> Result<()> {
    let (mut state, _root) = state(60.0).await?;
    let original = snapshot(&state.pool).await?;
    for deadline in [false, true] {
        set_timeout(&mut state, if deadline { 2.0 } else { 60.0 });
        let mut controller = control(&state).await?;
        let mut lock = controller.begin().await?;
        sqlx::raw_sql("LOCK TABLE audit_events IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *lock)
            .await?;
        let operation = spawn(&state);
        let pid = barrier(&state.pool, "INSERT INTO audit_events", Some(&operation)).await?;
        // A genuine audit INSERT wait proves the upload UPDATE was already sent
        // inside the same uncommitted record transaction.
        assert_eq!(snapshot(&state.pool).await?, original);
        if deadline {
            assert!(matches!(operation.await?, Err(SweepError::Timeout)));
            no_statement_wait(&state.pool, pid).await?;
        } else {
            settled(operation).await?;
        }
        gone(&state.pool, pid).await?;
        assert_eq!(snapshot(&state.pool).await?, original);
        lock.rollback().await?;
        assert_eq!(audit_count(&state.pool).await?, 0);
    }
    set_timeout(&mut state, 60.0);
    recovered(&state, 1).await?;
    assert_eq!(audit_count(&state.pool).await?, 1);
    state.pool.close().await;
    Ok(())
}
fn set_timeout(state: &mut AppState, seconds: f64) {
    std::sync::Arc::make_mut(&mut state.settings)
        .sweeps
        .timeout_seconds = seconds;
}

#[tokio::test]
#[ignore = "requires a guarded fresh PostgreSQL 17.11 child; use recovery_tests launcher"]
async fn periodic_shutdown_settles_before_pool_close() -> Result<()> {
    let (state, _root) = state(60.0).await?;
    let original = snapshot(&state.pool).await?;
    for (table, drain) in [
        ("attempts", false),
        ("audit_events", false),
        ("attempts", true),
    ] {
        let mut controller = control(&state).await?;
        let mut lock = controller.begin().await?;
        sqlx::raw_sql(if table == "attempts" {
            "LOCK TABLE attempts IN ACCESS EXCLUSIVE MODE"
        } else {
            "LOCK TABLE audit_events IN ACCESS EXCLUSIVE MODE"
        })
        .execute(&mut *lock)
        .await?;
        let mut periodic = state.clone();
        if drain {
            set_timeout(&mut periodic, 0.25);
        }
        let worker = Task::start(periodic)?;
        let pid = barrier(
            &state.pool,
            if table == "attempts" {
                "FROM attempts"
            } else {
                "INSERT INTO audit_events"
            },
            None,
        )
        .await?;
        if drain {
            older_than_run_timeout(&state.pool, pid).await?;
        }
        tokio::time::timeout(Duration::from_secs(10), worker.stop())
            .await
            .map_err(|_| "periodic worker did not settle")?;
        no_statement_wait(&state.pool, pid).await?;
        gone(&state.pool, pid).await?;
        assert!(!state.pool.is_closed());
        assert_eq!(snapshot(&state.pool).await?, original);
        lock.rollback().await?;
        assert_eq!(audit_count(&state.pool).await?, 0);
    }
    recovered(&state, 1).await?;
    tokio::time::timeout(Duration::from_secs(5), state.pool.close())
        .await
        .map_err(|_| "pool close did not drain")?;
    assert!(state.pool.is_closed());
    Ok(())
}

#[tokio::test]
#[ignore = "requires a guarded fresh PostgreSQL 17.11 child; use recovery_tests launcher"]
async fn failed_record_isolated_and_later_run_progresses() -> Result<()> {
    let (state, _root) = state(60.0).await?;
    sqlx::raw_sql(r"
INSERT INTO uploads(id,attempt_id,lease_generation,token_hash,role,backend,bucket,key,slot,declared_size,declared_sha256,media_type,expires_at)
SELECT '00000000-0000-0000-0000-000000001002',attempt_id,lease_generation,sha256('recovery-two'::bytea),role,backend,bucket,'recovery/two','recovery/two',declared_size,declared_sha256,media_type,expires_at+interval '1 second' FROM uploads;
CREATE FUNCTION fixture_recovery_fault() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.subject_id='00000000-0000-0000-0000-000000001001' THEN RAISE EXCEPTION 'isolated audit failure'; END IF; RETURN NEW; END $$;
CREATE TRIGGER fixture_recovery_fault BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION fixture_recovery_fault();
").execute(&state.pool).await?;
    let report = sweeps::run(&state).await?;
    assert!(report.ran);
    assert_eq!(report.errors, 1);
    assert_eq!(report.uploads_expired, 1);
    let pending: String = sqlx::query_scalar("SELECT state FROM uploads WHERE id=$1::uuid")
        .bind(UPLOAD)
        .fetch_one(&state.pool)
        .await?;
    assert_eq!(pending, "pending");
    assert_eq!(audit_count(&state.pool).await?, 1);
    sqlx::raw_sql("DROP TRIGGER fixture_recovery_fault ON audit_events; DROP FUNCTION fixture_recovery_fault()").execute(&state.pool).await?;
    recovered(&state, 1).await?;
    assert_eq!(audit_count(&state.pool).await?, 2);
    let key: String = sqlx::query_scalar("SELECT key FROM uploads WHERE id=$1::uuid")
        .bind(UPLOAD)
        .fetch_one(&state.pool)
        .await?;
    assert_eq!(key, KEY);
    state.pool.close().await;
    Ok(())
}
