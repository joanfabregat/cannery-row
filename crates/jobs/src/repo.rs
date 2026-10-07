//! Frozen job specifications, claim queue, lease updates and outcomes.
use cannery_core::{
    ids::{AttemptId, JobId, ProjectId, ServiceAccountId},
    json::{self, Document},
    principal::ServicePrincipal,
    timestamps::Timestamp,
};
use num_bigint::BigInt;
use sqlx::PgConnection;
use uuid::Uuid;

pub(crate) struct RawExpired {
    pub id: JobId,
    pub attempt_id: AttemptId,
}
pub(crate) fn expired_claims_query<'a>(
    exclude: &'a [JobId],
    limit: Option<&'a str>,
) -> sqlx::query::Map<
    'a,
    sqlx::Postgres,
    impl FnMut(sqlx::postgres::PgRow) -> Result<RawExpired, sqlx::Error> + Send,
    sqlx::postgres::PgArguments,
> {
    sqlx::query_file_as!(
        RawExpired,
        "src/sql/expired_claims.sql",
        exclude as &[JobId],
        limit
    )
}
struct RawPicked {
    id: JobId,
}
// Source Jsonb is a binary JSONB parameter: validation must precede matching
// the UPDATE predicate, including when no row matches.
struct JsonbText(String);
impl sqlx::Type<sqlx::Postgres> for JsonbText {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_name("jsonb")
    }
}
impl sqlx::Encode<'_, sqlx::Postgres> for JsonbText {
    fn encode_by_ref(
        &self,
        buffer: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        buffer.push(1);
        buffer.extend_from_slice(self.0.as_bytes());
        Ok(sqlx::encode::IsNull::No)
    }
    fn size_hint(&self) -> usize {
        self.0.len() + 1
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type)]
#[sqlx(transparent)]
pub struct EvidenceId(pub Uuid);
#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type)]
#[sqlx(transparent)]
pub struct ManifestId(pub Uuid);
macro_rules! domain {
    ($name:ident { $($variant:ident => $text:literal),+ }) => {
        #[derive(Clone,Copy,Debug,Eq,PartialEq)] pub enum $name { $($variant),+ }
        impl $name {
            #[must_use] pub const fn as_str(self)-> &'static str { match self {$(Self::$variant => $text),+} }
            fn parse(s:&str)->Result<Self,JobError> { match s { $($text=>Ok(Self::$variant)),+, _=>Err(JobError::CorruptData) } }
        }
    };
}
domain!(Stage {Tester=>"tester",Evaluator=>"evaluator"});
domain!(State {Pending=>"pending",Claimed=>"claimed",Completed=>"completed",Failed=>"failed"});
domain!(Origin {Submission=>"submission",AutoRetry=>"auto_retry",HumanRetry=>"human_retry"});
#[derive(Clone, Copy, Debug)]
pub struct JsonContext {
    pub encode_nesting_budget: usize,
    pub decode_nesting_budget: usize,
}
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error("job database operation failed")]
    Database { sqlstate: Option<String> },
    #[error("job row contains an invalid domain value")]
    CorruptData,
    #[error("job persistence invariant failed")]
    Invariant,
    #[error("the job is no longer claimed")]
    StaleLease,
    #[error("job JSON encoding failed")]
    Encode(json::EncodeError),
    #[error("job JSON decoding failed")]
    Decode(json::DecodeError),
}
impl JobError {
    pub(crate) fn database(e: &sqlx::Error) -> Self {
        Self::Database {
            sqlstate: e
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .filter(|s| {
                    s.len() == 5
                        && s.bytes()
                            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
                })
                .map(std::borrow::Cow::into_owned),
        }
    }
}
fn integer(value: Option<&BigInt>) -> Result<Option<String>, JobError> {
    value
        .map(|n| {
            let s = n.to_string();
            if s.trim_start_matches('-').len() > 131_072 {
                Err(JobError::Database { sqlstate: None })
            } else {
                Ok(s)
            }
        })
        .transpose()
}
fn text(value: &str) -> Result<(), JobError> {
    if value.contains('\0') {
        Err(JobError::Database { sqlstate: None })
    } else {
        Ok(())
    }
}
fn encode(value: &Document, c: JsonContext) -> Result<String, JobError> {
    json::encode_ascii_pretty(value, c.encode_nesting_budget).map_err(JobError::Encode)
}
fn decode(value: &str, c: JsonContext) -> Result<Document, JobError> {
    json::decode_str(value, c.decode_nesting_budget).map_err(JobError::Decode)
}
pub struct Job {
    pub id: JobId,
    pub project_id: ProjectId,
    pub attempt_id: AttemptId,
    pub stage: Stage,
    pub run_number: i32,
    pub state: State,
    pub science_revision: i32,
    pub tester_id: String,
    pub spec: Document,
    pub deadline_seconds: i32,
    pub created_at: Timestamp,
    pub claimed_by_service: Option<ServiceAccountId>,
    pub via_channel: Option<String>,
    pub via_client: Option<String>,
    pub lease_generation: i32,
    pub lease_token_hash: Option<Vec<u8>>,
    pub lease_expires_at: Option<Timestamp>,
    pub claimed_at: Option<Timestamp>,
    pub deadline: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub evidence_id: Option<EvidenceId>,
    pub manifest_id: Option<ManifestId>,
    pub error_step: Option<String>,
    pub error_code: Option<String>,
    pub error_reason: Option<String>,
    pub logs: Document,
    pub origin: Origin,
    pub previous_run_id: Option<JobId>,
}
impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Job([redacted])")
    }
}
#[derive(sqlx::FromRow)]
struct RawJob {
    #[sqlx(rename = "id!: _")]
    id: JobId,
    #[sqlx(rename = "project_id!: _")]
    project_id: ProjectId,
    #[sqlx(rename = "attempt_id!: _")]
    attempt_id: AttemptId,
    stage: String,
    run_number: i32,
    state: String,
    science_revision: i32,
    tester_id: String,
    #[sqlx(rename = "spec!")]
    spec: String,
    deadline_seconds: i32,
    #[sqlx(rename = "created_at!: _")]
    created_at: Timestamp,
    #[sqlx(rename = "claimed_by_service: _")]
    claimed_by_service: Option<ServiceAccountId>,
    via_channel: Option<String>,
    via_client: Option<String>,
    lease_generation: i32,
    lease_token_hash: Option<Vec<u8>>,
    #[sqlx(rename = "lease_expires_at: _")]
    lease_expires_at: Option<Timestamp>,
    #[sqlx(rename = "claimed_at: _")]
    claimed_at: Option<Timestamp>,
    #[sqlx(rename = "deadline: _")]
    deadline: Option<Timestamp>,
    #[sqlx(rename = "finished_at: _")]
    finished_at: Option<Timestamp>,
    #[sqlx(rename = "evidence_id: _")]
    evidence_id: Option<EvidenceId>,
    #[sqlx(rename = "manifest_id: _")]
    manifest_id: Option<ManifestId>,
    error_step: Option<String>,
    error_code: Option<String>,
    error_reason: Option<String>,
    #[sqlx(rename = "logs!")]
    logs: String,
    origin: String,
    #[sqlx(rename = "previous_run_id: _")]
    previous_run_id: Option<JobId>,
}
impl RawJob {
    fn decode(self, c: JsonContext) -> Result<Job, JobError> {
        Ok(Job {
            id: self.id,
            project_id: self.project_id,
            attempt_id: self.attempt_id,
            stage: Stage::parse(&self.stage)?,
            run_number: self.run_number,
            state: State::parse(&self.state)?,
            science_revision: self.science_revision,
            tester_id: self.tester_id,
            spec: decode(&self.spec, c)?,
            deadline_seconds: self.deadline_seconds,
            created_at: self.created_at,
            claimed_by_service: self.claimed_by_service,
            via_channel: self.via_channel,
            via_client: self.via_client,
            lease_generation: self.lease_generation,
            lease_token_hash: self.lease_token_hash,
            lease_expires_at: self.lease_expires_at,
            claimed_at: self.claimed_at,
            deadline: self.deadline,
            finished_at: self.finished_at,
            evidence_id: self.evidence_id,
            manifest_id: self.manifest_id,
            error_step: self.error_step,
            error_code: self.error_code,
            error_reason: self.error_reason,
            logs: decode(&self.logs, c)?,
            origin: Origin::parse(&self.origin)?,
            previous_run_id: self.previous_run_id,
        })
    }
}
/// Queue the next run; caller holds the attempt lock.
pub struct NewJob<'a> {
    pub id: JobId,
    pub project_id: ProjectId,
    pub attempt_id: AttemptId,
    pub stage: Stage,
    pub science_revision: &'a BigInt,
    pub tester_id: &'a str,
    pub spec: &'a Document,
    pub deadline_seconds: &'a BigInt,
    pub origin: Origin,
    pub previous_run_id: Option<JobId>,
}
/// # Errors
/// Preserves source driver/SQL failures and post-write JSON decoding errors.
pub async fn create_job(
    conn: &mut PgConnection,
    n: NewJob<'_>,
    c: JsonContext,
) -> Result<Job, JobError> {
    let science = integer(Some(n.science_revision))?;
    text(n.tester_id)?;
    let spec = JsonbText(encode(n.spec, c)?);
    let deadline = integer(Some(n.deadline_seconds))?;
    sqlx::query_file_as!(
        RawJob,
        "src/sql/create_job.sql",
        n.id as JobId,
        n.project_id as ProjectId,
        n.attempt_id as AttemptId,
        n.stage.as_str(),
        science.as_deref(),
        n.tester_id,
        spec as _,
        deadline.as_deref(),
        n.origin.as_str(),
        n.previous_run_id as Option<JobId>
    )
    .fetch_one(conn)
    .await
    .map_err(|e| JobError::database(&e))?
    .decode(c)
}
/// # Errors
/// Reports sanitized database or row-decoding failure.
pub async fn get_job(
    conn: &mut PgConnection,
    id: JobId,
    lock: bool,
    c: JsonContext,
) -> Result<Option<Job>, JobError> {
    let row = if lock {
        sqlx::query_file_as!(RawJob, "src/sql/get_job_locked.sql", id as JobId)
            .fetch_optional(conn)
            .await
    } else {
        sqlx::query_file_as!(RawJob, "src/sql/get_job.sql", id as JobId)
            .fetch_optional(conn)
            .await
    }
    .map_err(|e| JobError::database(&e))?;
    row.map(|r| r.decode(c)).transpose()
}
/// # Errors
/// Reports sanitized database or row-decoding failure.
pub async fn latest_job(
    conn: &mut PgConnection,
    attempt: AttemptId,
    stage: Stage,
    c: JsonContext,
) -> Result<Option<Job>, JobError> {
    sqlx::query_file_as!(
        RawJob,
        "src/sql/latest_job.sql",
        attempt as AttemptId,
        stage.as_str()
    )
    .fetch_optional(conn)
    .await
    .map_err(|e| JobError::database(&e))?
    .map(|r| r.decode(c))
    .transpose()
}
/// # Errors
/// Reports sanitized database failure.
pub async fn automatic_reruns(
    conn: &mut PgConnection,
    attempt: AttemptId,
    stage: Stage,
) -> Result<i64, JobError> {
    Ok(sqlx::query_file!(
        "src/sql/automatic_reruns.sql",
        attempt as AttemptId,
        stage.as_str()
    )
    .fetch_one(conn)
    .await
    .map_err(|e| JobError::database(&e))?
    .count)
}
/// # Errors
/// Reports sanitized database or integer driver failure.
pub async fn expired_claims(
    conn: &mut PgConnection,
    limit: Option<&BigInt>,
    exclude: &[JobId],
) -> Result<Vec<(JobId, AttemptId)>, JobError> {
    let limit = integer(limit)?;
    Ok(expired_claims_query(exclude, limit.as_deref())
        .fetch_all(conn)
        .await
        .map_err(|e| JobError::database(&e))?
        .into_iter()
        .map(|r| (r.id, r.attempt_id))
        .collect())
}
/// # Errors
/// Reports sanitized database, integer driver or row-decoding failure.
pub async fn list_jobs(
    conn: &mut PgConnection,
    attempt: AttemptId,
    after: Option<JobId>,
    limit: Option<&BigInt>,
    c: JsonContext,
) -> Result<Vec<Job>, JobError> {
    let limit = integer(limit)?;
    sqlx::query_file_as!(
        RawJob,
        "src/sql/list_jobs.sql",
        attempt as AttemptId,
        after as Option<JobId>,
        limit.as_deref()
    )
    .fetch_all(conn)
    .await
    .map_err(|e| JobError::database(&e))?
    .into_iter()
    .map(|r| r.decode(c))
    .collect()
}
/// # Errors
/// Reports sanitized database or source text-adaptation failure.
pub async fn pick_pending(
    conn: &mut PgConnection,
    project: ProjectId,
    stage: Stage,
    tester: &str,
    revision: Option<&str>,
) -> Result<Option<JobId>, JobError> {
    text(tester)?;
    if let Some(r) = revision {
        text(r)?;
    }
    Ok(sqlx::query_file_as!(
        RawPicked,
        "src/sql/pick_pending.sql",
        project as ProjectId,
        stage.as_str(),
        tester,
        revision
    )
    .fetch_optional(conn)
    .await
    .map_err(|e| JobError::database(&e))?
    .map(|r| r.id))
}
/// # Errors
/// Missing/nonpending rows are invariants; route-level lease checks are separate.
pub async fn claim_job(
    conn: &mut PgConnection,
    id: JobId,
    p: &ServicePrincipal,
    hash: &[u8],
    ttl: &BigInt,
    c: JsonContext,
) -> Result<Job, JobError> {
    if let Some(s) = &p.via.client {
        text(s)?;
    }
    let ttl = crate::integer::PgInteger::new(ttl)?;
    let channel = match p.via.channel {
        cannery_core::principal::Channel::Ui => "ui",
        cannery_core::principal::Channel::Api => "api",
        cannery_core::principal::Channel::Mcp => "mcp",
        cannery_core::principal::Channel::Cli => "cli",
        cannery_core::principal::Channel::System => "system",
    };
    sqlx::query_file_as!(
        RawJob,
        "src/sql/claim_job.sql",
        p.service_account_id as ServiceAccountId,
        channel,
        p.via.client.as_deref(),
        hash,
        ttl as _,
        id as JobId
    )
    .fetch_optional(conn)
    .await
    .map_err(|e| JobError::database(&e))?
    .ok_or(JobError::Invariant)?
    .decode(c)
}
/// # Errors
/// A row no longer claimed is `StaleLease`; expiry is intentionally not checked.
pub async fn rotate_token(
    conn: &mut PgConnection,
    id: JobId,
    hash: &[u8],
    c: JsonContext,
) -> Result<Job, JobError> {
    sqlx::query_file_as!(RawJob, "src/sql/rotate_token.sql", hash, id as JobId)
        .fetch_optional(conn)
        .await
        .map_err(|e| JobError::database(&e))?
        .ok_or(JobError::StaleLease)?
        .decode(c)
}
/// # Errors
/// Preserves source interval conversion and claimed-only update failures.
pub async fn extend_lease(
    conn: &mut PgConnection,
    id: JobId,
    ttl: &BigInt,
    c: JsonContext,
) -> Result<Job, JobError> {
    macro_rules! execute {
        ($path:literal,$value:expr,$width:expr) => {{
            sqlx::query_file_as!(RawJob, $path, $value as _, id as JobId)
                .fetch_optional(&mut *conn)
                .await
        }};
    }
    let result = if let Ok(value) = ttl.to_string().parse::<i64>() {
        if let Ok(value) = i16::try_from(value) {
            execute!("src/sql/extend_lease_int2.sql", value, Width::Int2)
        } else if let Ok(value) = i32::try_from(value) {
            execute!("src/sql/extend_lease_int4.sql", value, Width::Int4)
        } else {
            execute!("src/sql/extend_lease_int8.sql", value, Width::Int8)
        }
    } else {
        let ttl = crate::integer::PgInteger::new(ttl)?;
        execute!("src/sql/extend_lease.sql", ttl, Width::Numeric)
    };
    result
        .map_err(|e| JobError::database(&e))?
        .ok_or(JobError::StaleLease)?
        .decode(c)
}
/// # Errors
/// Reports stale, SQL constraint or post-write decoding failure.
pub async fn complete_job(
    conn: &mut PgConnection,
    id: JobId,
    evidence: EvidenceId,
    manifest: Option<ManifestId>,
    c: JsonContext,
) -> Result<Job, JobError> {
    sqlx::query_file_as!(
        RawJob,
        "src/sql/complete_job.sql",
        evidence as EvidenceId,
        manifest as Option<ManifestId>,
        id as JobId
    )
    .fetch_optional(conn)
    .await
    .map_err(|e| JobError::database(&e))?
    .ok_or(JobError::StaleLease)?
    .decode(c)
}
pub struct Failure<'a> {
    pub step: Option<&'a str>,
    pub code: &'a str,
    pub reason: &'a str,
    pub logs: &'a Document,
}
/// # Errors
/// Reports source adaptation, stale, SQL or post-write decoding failure.
pub async fn fail_job(
    conn: &mut PgConnection,
    id: JobId,
    f: Failure<'_>,
    c: JsonContext,
) -> Result<Job, JobError> {
    if let Some(s) = f.step {
        text(s)?;
    }
    text(f.code)?;
    text(f.reason)?;
    let logs = JsonbText(encode(f.logs, c)?);
    sqlx::query_file_as!(
        RawJob,
        "src/sql/fail_job.sql",
        f.step,
        f.code,
        f.reason,
        logs as _,
        id as JobId
    )
    .fetch_optional(conn)
    .await
    .map_err(|e| JobError::database(&e))?
    .ok_or(JobError::StaleLease)?
    .decode(c)
}
/// # Errors
/// Reports sanitized database or corrupt integer aggregate failure.
pub async fn committed_output_bytes(
    conn: &mut PgConnection,
    id: JobId,
) -> Result<BigInt, JobError> {
    sqlx::query_file!("src/sql/committed_output_bytes.sql", id as JobId)
        .fetch_one(conn)
        .await
        .map_err(|e| JobError::database(&e))?
        .bytes
        .parse()
        .map_err(|_| JobError::CorruptData)
}
