//! Bounded recovery runs use a direct connection and per-record transactions.
use crate::{
    AppState, attempt_failure,
    attempt_release_routes::AttemptReleaseContext,
    job_lifecycle::{self, Context},
    requests::RequestContext,
};
use cannery_attempts::repo::Repository;
use cannery_core::{
    audit::{self, Attribution, Record},
    ids::{AttemptId, JobId, ProjectId},
};
use cannery_jobs::repo as jobs;
use cannery_storage::ObjectStore;
use serde::Serialize;
use serde_json::json;
use sqlx::{Connection, PgConnection};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};
use uuid::Uuid;

const LOCK: &str = "cannery_row.sweeps";
pub struct SweepContext {
    pub store: Arc<ObjectStore>,
    pub lifecycle: Arc<Context>,
    pub release: Arc<AttemptReleaseContext>,
    pub max_stream_seconds: f64,
}
#[derive(Default, Serialize)]
pub struct SweepReport {
    pub ran: bool,
    pub attempts_expired: u64,
    pub attempts_requeued: u64,
    pub jobs_failed: u64,
    pub jobs_rerun: u64,
    pub uploads_expired: u64,
    pub uploads_failed: u64,
    pub objects_deleted: u64,
    pub staging_deleted: u64,
    pub errors: u64,
}
#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum SweepError {
    #[error("recovery resources are unavailable")]
    Unavailable,
    #[error("invalid recovery limits")]
    Limits,
    #[error("recovery database operation failed")]
    Database,
    #[error("recovery transition failed")]
    Transition,
    #[error("recovery storage operation failed")]
    Storage,
    #[error("recovery run exceeded its deadline")]
    Timeout,
}
impl From<sqlx::Error> for SweepError {
    fn from(_: sqlx::Error) -> Self {
        Self::Database
    }
}
type Result<T> = std::result::Result<T, SweepError>;
#[derive(Clone, Copy)]
enum Phase {
    Attempts,
    Jobs,
    Uploads,
    FailedUploads,
}

/// Keeps interrupted direct connections owned until their active statements
/// settle. PostgreSQL does not notice a client disconnect while waiting on a
/// relation lock, so merely dropping a socket can retain session advisory locks.
#[derive(Default)]
struct Settlement {
    workers: Mutex<Vec<Arc<SettlementWorker>>>,
}
#[derive(Default)]
struct SettlementWorker {
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    finished: AtomicBool,
    notify: tokio::sync::Notify,
}
struct Finished(Arc<SettlementWorker>);
impl Drop for Finished {
    fn drop(&mut self) {
        self.0.finished.store(true, Ordering::Release);
        self.0.notify.notify_waiters();
    }
}
impl Settlement {
    fn close(self: &Arc<Self>, mut conn: PgConnection, statement: Duration, close: Duration) {
        let keepalive = self.clone();
        let worker = Arc::new(SettlementWorker::default());
        let finished = Finished(worker.clone());
        let task = tokio::spawn(async move {
            let _keepalive = keepalive;
            let _finished = finished;
            // A canceled statement may return its ErrorResponse here. That is
            // still server-side settlement; the ensuing Terminate closes the
            // session rather than returning it to any pool.
            if tokio::time::timeout(statement.saturating_add(close), conn.ping())
                .await
                .is_err()
            {
                tracing::warn!("recovery statement settlement exceeded its deadline");
            }
            if !matches!(tokio::time::timeout(close, conn.close()).await, Ok(Ok(()))) {
                tracing::warn!("recovery connection close failed");
            }
        });
        *worker
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);
        self.workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(worker);
    }
    async fn drain(&self) {
        loop {
            let workers = self
                .workers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if workers.is_empty() {
                break;
            }
            for worker in workers {
                // Await only a completion signal: canceling this waiter must
                // not remove the owned task from the shutdown registry.
                let completed = worker.notify.notified();
                tokio::pin!(completed);
                completed.as_mut().enable();
                if !worker.finished.load(Ordering::Acquire) {
                    completed.await;
                }
            }
            self.workers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain(|worker| !worker.finished.load(Ordering::Acquire));
        }
    }
}
struct OwnedConnection {
    connection: Option<PgConnection>,
    settlement: Arc<Settlement>,
    statement: Duration,
    close: Duration,
}
impl Drop for OwnedConnection {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            self.settlement
                .close(connection, self.statement, self.close);
        }
    }
}
struct Candidate {
    id: Uuid,
    attempt: Option<Uuid>,
    key: Option<String>,
}

fn duration(seconds: f64) -> Result<Duration> {
    if seconds.is_finite() && seconds > 0.0 && seconds <= 86_400.0 {
        Ok(Duration::from_secs_f64(seconds))
    } else {
        Err(SweepError::Limits)
    }
}

/// Owns periodic recovery and cancels the active bounded run during shutdown.
pub struct Task {
    stop: tokio::sync::watch::Sender<bool>,
    worker: tokio::task::JoinHandle<()>,
    settlement: Arc<Settlement>,
}
impl Task {
    /// Start the production periodic worker with its configured initial run.
    /// # Errors
    /// Refuses an invalid interval before spawning any task.
    pub fn start(state: AppState) -> Result<Self> {
        let interval = duration(state.settings.sweeps.interval_seconds)?;
        let (stop, mut receiver) = tokio::sync::watch::channel(false);
        let settlement = Arc::new(Settlement::default());
        let active_settlement = settlement.clone();
        let worker = tokio::spawn(async move {
            let mut ticks = tokio::time::interval(interval);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    _ = receiver.changed() => break,
                    _ = ticks.tick() => {}
                }
                tokio::select! {
                    biased;
                    _ = receiver.changed() => break,
                    result = run_settled(&state, active_settlement.clone()) => match result {
                        Ok(report) => tracing::info!(ran = report.ran, errors = report.errors, "recovery run settled"),
                        Err(error) => tracing::warn!(%error, "recovery run failed"),
                    }
                }
            }
        });
        Ok(Self {
            stop,
            worker,
            settlement,
        })
    }
    /// Cancel the active run and await worker settlement before closing its pool.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = self.worker.await;
        self.settlement.drain().await;
    }
}
/// One direct connection owns the advisory lock, including cancellation.
/// # Errors
/// Returns sanitized configuration, database and overall timeout errors.
pub async fn run(state: &AppState) -> Result<SweepReport> {
    run_settled(state, Arc::new(Settlement::default())).await
}
async fn run_settled(state: &AppState, settlement: Arc<Settlement>) -> Result<SweepReport> {
    let result = run_owned(state, settlement.clone()).await;
    settlement.drain().await;
    result
}
async fn run_owned(state: &AppState, settlement: Arc<Settlement>) -> Result<SweepReport> {
    let profile = state.sweeps.as_ref().ok_or(SweepError::Unavailable)?;
    let settings = &state.settings.sweeps;
    let batch = settings
        .batch_size
        .to_i64("sweeps.batch_size")
        .map_err(|_| SweepError::Limits)?;
    let pages = settings
        .batches_per_run
        .to_i64("sweeps.batches_per_run")
        .map_err(|_| SweepError::Limits)?;
    if !(1..=10_000).contains(&batch) || !(1..=100).contains(&pages) {
        return Err(SweepError::Limits);
    }
    let timeout = duration(settings.timeout_seconds)?;
    let connect = settings
        .connect_timeout_seconds
        .to_u64("sweeps.connect_timeout_seconds")
        .map_err(|_| SweepError::Limits)?;
    if !(1..=86_400).contains(&connect) {
        return Err(SweepError::Limits);
    }
    let connect = Duration::from_secs(connect);
    let statement = duration(settings.statement_timeout_seconds)?;
    tokio::time::timeout(timeout, async {
        let conn = tokio::time::timeout(
            connect,
            PgConnection::connect_with(state.database.connect_options()),
        )
        .await
        .map_err(|_| SweepError::Timeout)??;
        let mut ownership = OwnedConnection {
            connection: Some(conn),
            settlement,
            statement,
            close: connect,
        };
        let conn = ownership.connection.as_mut().ok_or(SweepError::Database)?;
        let lock_ms = format!("{}ms", duration(settings.lock_timeout_seconds)?.as_millis());
        let statement_ms = format!(
            "{}ms",
            duration(settings.statement_timeout_seconds)?.as_millis()
        );
        sqlx::query!(
            "SELECT set_config('lock_timeout',$1,false) AS lock_timeout,set_config('statement_timeout',$2,false) AS statement_timeout",
            lock_ms,
            statement_ms
        )
        .fetch_one(&mut *conn)
        .await?;
        let lock = sqlx::query_scalar!("SELECT pg_try_advisory_lock(hashtextextended($1,0))", LOCK)
            .fetch_one(&mut *conn)
            .await?;
        if lock != Some(true) {
            return Ok(SweepReport::default());
        }
        let mut report = SweepReport {
            ran: true,
            ..SweepReport::default()
        };
        let request = RequestContext::background();
        for phase in [
            Phase::Attempts,
            Phase::Jobs,
            Phase::Uploads,
            Phase::FailedUploads,
        ] {
            if each(
                conn,
                profile,
                phase,
                batch,
                pages,
                &mut report,
                &request,
            )
            .await
            .is_err()
            {
                report.errors += 1;
                tracing::warn!("recovery phase failed");
            }
        }
        if let Ok(count) = profile
            .store
            .sweep_staging(profile.max_stream_seconds, SystemTime::now())
            .await
        {
            report.staging_deleted += count;
        } else {
            report.errors += 1;
            tracing::warn!("recovery staging phase failed");
        }
        // Close rather than return this lock-bearing connection to a pool.
        Ok(report)
    })
    .await
    .map_err(|_| SweepError::Timeout)?
}
async fn each(
    c: &mut PgConnection,
    s: &SweepContext,
    phase: Phase,
    batch: i64,
    pages: i64,
    report: &mut SweepReport,
    r: &RequestContext,
) -> Result<()> {
    let mut seen = Vec::new();
    for _ in 0..pages {
        let rows = candidates(c, s, phase, &seen, batch).await?;
        let short = i64::try_from(rows.len()).map_err(|_| SweepError::Limits)? < batch;
        for row in rows {
            seen.push(row.id);
            let result = match phase {
                Phase::Attempts => expire_attempt(c, s, row.id, report, r).await,
                Phase::Jobs => {
                    expire_job(
                        c,
                        s,
                        row.id,
                        row.attempt.ok_or(SweepError::Database)?,
                        report,
                        r,
                    )
                    .await
                }
                Phase::Uploads => {
                    orphan(
                        c,
                        s,
                        row.id,
                        row.key.as_deref().ok_or(SweepError::Database)?,
                        report,
                    )
                    .await
                }
                Phase::FailedUploads => {
                    delete_failed(
                        c,
                        s,
                        row.id,
                        row.key.as_deref().ok_or(SweepError::Database)?,
                        report,
                    )
                    .await
                }
            };
            if result.is_err() {
                report.errors += 1;
                tracing::warn!("recovery record failed");
            }
        }
        if short {
            break;
        }
    }
    Ok(())
}

async fn candidates(
    c: &mut PgConnection,
    s: &SweepContext,
    phase: Phase,
    seen: &[Uuid],
    limit: i64,
) -> Result<Vec<Candidate>> {
    Ok(match phase {
        Phase::Attempts => sqlx::query!("SELECT id AS \"id: Uuid\" FROM attempts WHERE state IN ('claimed','running') AND (lease_expires_at<=now() OR deadline<=now()) AND id<>ALL($1::uuid[]) ORDER BY lease_expires_at,id LIMIT $2", seen as _, limit).fetch_all(c).await?.into_iter().map(|r| Candidate { id:r.id, attempt:None, key:None }).collect(),
        Phase::Jobs => sqlx::query!("SELECT id AS \"id: Uuid\",attempt_id AS \"attempt_id: Uuid\" FROM jobs WHERE state='claimed' AND (lease_expires_at<=now() OR deadline<=now()) AND id<>ALL($1::uuid[]) ORDER BY lease_expires_at,id LIMIT $2", seen as _, limit).fetch_all(c).await?.into_iter().map(|r| Candidate { id:r.id, attempt:Some(r.attempt_id), key:None }).collect(),
        Phase::Uploads => sqlx::query!("SELECT id AS \"id: Uuid\",key FROM uploads WHERE state IN ('pending','receiving') AND expires_at<=now() AND (state='pending' OR receiving_since<=now()-($1::float8*interval '1 second')) AND backend=$2 AND bucket=$3 AND id<>ALL($4::uuid[]) ORDER BY expires_at,id LIMIT $5", s.max_stream_seconds, s.store.backend(), s.store.bucket(), seen as _, limit).fetch_all(c).await?.into_iter().map(|r| Candidate { id:r.id, attempt:None, key:Some(r.key) }).collect(),
        Phase::FailedUploads => sqlx::query!("SELECT id AS \"id: Uuid\",key FROM uploads WHERE state='failed' AND object_pending_delete AND (urls_expire_at IS NULL OR urls_expire_at<=now()) AND backend=$1 AND bucket=$2 AND id<>ALL($3::uuid[]) ORDER BY id LIMIT $4", s.store.backend(), s.store.bucket(), seen as _, limit).fetch_all(c).await?.into_iter().map(|r| Candidate { id:r.id, attempt:None, key:Some(r.key) }).collect(),
    })
}

async fn expire_attempt(
    c: &mut PgConnection,
    s: &SweepContext,
    id: Uuid,
    report: &mut SweepReport,
    r: &RequestContext,
) -> Result<()> {
    let mut tx = c.begin().await?;
    let locked = sqlx::query!("SELECT id AS \"id: Uuid\",coalesce(deadline<=now(),false) AS \"deadline_passed!\" FROM attempts WHERE id=$1 AND state IN ('claimed','running') AND (lease_expires_at<=now() OR deadline<=now()) FOR UPDATE SKIP LOCKED", id as _).fetch_optional(&mut *tx).await?;
    let Some(locked) = locked else {
        tx.rollback().await?;
        return Ok(());
    };
    let attempt = Repository::new(&mut tx, s.lifecycle.attempts)
        .get_attempt_by_id(AttemptId(id), false)
        .await
        .map_err(|_| SweepError::Database)?
        .ok_or(SweepError::Database)?;
    let expires = attempt.lease_expires_at.ok_or(SweepError::Transition)?;
    let mut details = json!({"lease_generation":attempt.lease_generation,"lease_expires_at":expires.model_isoformat()});
    let (code, reason) = if locked.deadline_passed {
        details["deadline"] = json!(
            attempt
                .deadline
                .ok_or(SweepError::Transition)?
                .model_isoformat()
        );
        (
            "deadline_exceeded",
            "the runner did not submit the attempt before its deadline",
        )
    } else if attempt.workflow.python_none() {
        (
            "lease_expired",
            "the agent's lease expired without a heartbeat",
        )
    } else {
        (
            "lease_expired",
            "the runner's lease expired without a heartbeat",
        )
    };
    let details =
        job_lifecycle::document(&details, &s.lifecycle, r).map_err(|_| SweepError::Transition)?;
    let requeued = attempt_failure::experiment_as(
        &mut tx,
        s.lifecycle.attempts,
        Attribution::System(None),
        &attempt,
        &attempt_failure::Report {
            code,
            reason,
            details: &details,
            log_refs: None,
            step: None,
            idempotency_key: None,
            manifest_sha256: None,
        },
        &s.release,
        r,
    )
    .await
    .map_err(|_| SweepError::Transition)?;
    tx.commit().await?;
    report.attempts_expired += 1;
    report.attempts_requeued += u64::from(requeued);
    Ok(())
}

async fn expire_job(
    c: &mut PgConnection,
    s: &SweepContext,
    id: Uuid,
    attempt_id: Uuid,
    report: &mut SweepReport,
    r: &RequestContext,
) -> Result<()> {
    let mut tx = c.begin().await?;
    let attempt_lock = sqlx::query!(
        "SELECT id AS \"id: Uuid\" FROM attempts WHERE id=$1 FOR UPDATE SKIP LOCKED",
        attempt_id as _
    )
    .fetch_optional(&mut *tx)
    .await?;
    if attempt_lock.is_none() {
        tx.rollback().await?;
        return Ok(());
    }
    let locked = sqlx::query!("SELECT id AS \"id: Uuid\",coalesce(deadline<=now(),false) AS \"deadline_passed!\" FROM jobs WHERE id=$1 AND state='claimed' AND (lease_expires_at<=now() OR deadline<=now()) FOR UPDATE SKIP LOCKED", id as _).fetch_optional(&mut *tx).await?;
    let Some(locked) = locked else {
        tx.rollback().await?;
        return Ok(());
    };
    let attempt = Repository::new(&mut tx, s.lifecycle.attempts)
        .get_attempt_by_id(AttemptId(attempt_id), false)
        .await
        .map_err(|_| SweepError::Database)?
        .ok_or(SweepError::Database)?;
    let job = jobs::get_job(&mut tx, JobId(id), false, s.lifecycle.jobs)
        .await
        .map_err(|_| SweepError::Database)?
        .ok_or(SweepError::Database)?;
    let (code, reason) = if locked.deadline_passed {
        (
            "deadline_exceeded",
            "the job's deadline passed before it finished",
        )
    } else {
        (
            "lease_expired",
            "the job's lease expired without a heartbeat",
        )
    };
    let details = job_lifecycle::document(
        &json!({"lease_generation":job.lease_generation}),
        &s.lifecycle,
        r,
    )
    .map_err(|_| SweepError::Transition)?;
    let empty =
        job_lifecycle::document(&json!([]), &s.lifecycle, r).map_err(|_| SweepError::Transition)?;
    let failure = jobs::Failure {
        step: None,
        code,
        reason,
        logs: &empty,
    };
    let rerun = if attempt.state == cannery_attempts::model::State::Verifying {
        job_lifecycle::fail_job_run_as(
            &mut tx,
            Attribution::System(None),
            &attempt,
            &job,
            failure,
            &details,
            false,
            &s.lifecycle,
            r,
        )
        .await
        .map_err(|_| SweepError::Transition)?
        .1
        .is_some()
    } else {
        jobs::fail_job(&mut tx, job.id, failure, s.lifecycle.jobs)
            .await
            .map_err(|_| SweepError::Transition)?;
        job_lifecycle::event_as(
            &mut tx,
            Attribution::System(None),
            &attempt,
            "job.failed",
            "job",
            &id.to_string(),
            Some(&json!({"state":"claimed"})),
            &json!({"state":"failed","code":code,"run_number":job.run_number}),
            Some(reason),
            r,
        )
        .await
        .map_err(|_| SweepError::Transition)?;
        false
    };
    tx.commit().await?;
    report.jobs_failed += 1;
    report.jobs_rerun += u64::from(rerun);
    Ok(())
}

async fn upload_event(
    c: &mut PgConnection,
    project: Uuid,
    id: Uuid,
    action: &str,
    prior: &str,
    new: &serde_json::Value,
    reason: Option<&str>,
) -> Result<()> {
    audit::record(
        c,
        Attribution::System(None),
        Record {
            action,
            subject_type: "upload",
            subject_id: &id.to_string(),
            project_id: Some(ProjectId(project)),
            prior_state: Some(&json!({"state":prior})),
            new_state: Some(new),
            reason,
            idempotency_key: None,
        },
    )
    .await
    .map_err(|_| SweepError::Transition)?;
    Ok(())
}
async fn orphan(
    c: &mut PgConnection,
    s: &SweepContext,
    id: Uuid,
    key: &str,
    report: &mut SweepReport,
) -> Result<()> {
    let seen = s.store.head(key).await.map_err(|_| SweepError::Storage)?;
    let mut tx = c.begin().await?;
    let locked = sqlx::query!("SELECT uploads.state,attempts.project_id AS \"project_id: Uuid\",uploads.multipart_upload_id FROM uploads JOIN attempts ON attempts.id=uploads.attempt_id WHERE uploads.id=$1 AND uploads.key=$2 AND uploads.state IN ('pending','receiving') AND uploads.backend=$3 AND uploads.bucket=$4 AND uploads.expires_at<=now() AND (uploads.state='pending' OR uploads.receiving_since<=now()-($5::float8*interval '1 second')) FOR UPDATE OF uploads SKIP LOCKED", id as _, key, s.store.backend(), s.store.bucket(), s.max_stream_seconds).fetch_optional(&mut *tx).await?;
    let Some(locked) = locked else {
        tx.rollback().await?;
        return Ok(());
    };
    let verified = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM artifacts WHERE backend=$1 AND bucket=$2 AND key=$3)",
        s.store.backend(),
        s.store.bucket(),
        key
    )
    .fetch_one(&mut *tx)
    .await?
        == Some(true);
    let deleted = if verified {
        false
    } else if let Some(seen) = seen {
        s.store
            .delete(key, seen.generation.as_deref())
            .await
            .map_err(|_| SweepError::Storage)?;
        true
    } else {
        false
    };
    let state = if verified { "failed" } else { "expired" };
    Repository::new(&mut tx, s.lifecycle.attempts)
        .finish_upload(cannery_attempts::model::UploadId(id), state, None, false)
        .await
        .map_err(|_| SweepError::Database)?;
    upload_event(
        &mut tx,
        locked.project_id,
        id,
        if verified {
            "upload.failed"
        } else {
            "upload.expired"
        },
        &locked.state,
        &json!({"state":state,"key":key,"object_deleted":deleted}),
        verified.then_some("the grant expired over a key that already has a verified artifact"),
    )
    .await?;
    tx.commit().await?;
    if verified {
        report.uploads_failed += 1;
    } else {
        report.uploads_expired += 1;
    }
    report.objects_deleted += u64::from(deleted);
    if let Some(multipart) = locked.multipart_upload_id
        && let Some(store) = s.store.presigning()
    {
        store
            .abort_multipart(key, &multipart)
            .await
            .map_err(|_| SweepError::Storage)?;
    }
    Ok(())
}
async fn delete_failed(
    c: &mut PgConnection,
    s: &SweepContext,
    id: Uuid,
    key: &str,
    report: &mut SweepReport,
) -> Result<()> {
    let mut tx = c.begin().await?;
    let locked = sqlx::query!("SELECT id AS \"id: Uuid\" FROM uploads WHERE id=$1 AND key=$2 AND backend=$3 AND bucket=$4 AND state='failed' AND object_pending_delete AND (urls_expire_at IS NULL OR urls_expire_at<=now()) FOR UPDATE SKIP LOCKED", id as _, key, s.store.backend(), s.store.bucket()).fetch_optional(&mut *tx).await?;
    if locked.is_none() {
        tx.rollback().await?;
        return Ok(());
    }
    let verified = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM artifacts WHERE backend=$1 AND bucket=$2 AND key=$3)",
        s.store.backend(),
        s.store.bucket(),
        key
    )
    .fetch_one(&mut *tx)
    .await?
        == Some(true);
    let deleted = if !verified
        && let Some(seen) = s.store.head(key).await.map_err(|_| SweepError::Storage)?
    {
        s.store
            .delete(key, seen.generation.as_deref())
            .await
            .map_err(|_| SweepError::Storage)?;
        true
    } else {
        false
    };
    Repository::new(&mut tx, s.lifecycle.attempts)
        .object_deleted(cannery_attempts::model::UploadId(id))
        .await
        .map_err(|_| SweepError::Database)?;
    tx.commit().await?;
    report.objects_deleted += u64::from(deleted);
    Ok(())
}
