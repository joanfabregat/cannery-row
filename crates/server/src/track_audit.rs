//! Narrow lossless audit adapter for track snapshots; shared ownership is pending.
use cannery_core::{
    ids::{ProjectId, ServiceAccountId, UserId},
    json::{self, Document},
    principal::{Channel, UserPrincipal},
    timestamps::Timestamp,
};
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use sqlx::{Executor, PgConnection};

pub(crate) struct Jsonb(pub String);
impl sqlx::Type<sqlx::Postgres> for Jsonb {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_name("jsonb")
    }
}
impl sqlx::Encode<'_, sqlx::Postgres> for Jsonb {
    fn encode_by_ref(
        &self,
        buffer: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> std::result::Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        buffer.push(1);
        buffer.extend_from_slice(self.0.as_bytes());
        Ok(sqlx::encode::IsNull::No)
    }
}
pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
#[allow(
    clippy::too_many_arguments,
    reason = "Narrow source audit adapter retains explicit attribution and states"
)]
pub(crate) async fn record(
    c: &mut PgConnection,
    user: &UserPrincipal,
    project: ProjectId,
    id: &str,
    action: &str,
    prior: Option<&Document>,
    new: &Document,
    reason: Option<&str>,
    budget: usize,
) -> Result<()> {
    let prior = prior
        .map(|v| json::encode_ascii_pretty(v, budget))
        .transpose()?
        .map(Jsonb);
    let new = Jsonb(json::encode_ascii_pretty(new, budget)?);
    let channel = match user.via.channel {
        Channel::Ui => "ui",
        Channel::Api => "api",
        Channel::Mcp => "mcp",
        Channel::Cli => "cli",
        Channel::System => "system",
    };
    sqlx::query!(r#"INSERT INTO audit_events(project_id,actor_kind,actor_user_id,actor_service_id,via_channel,via_client,action,subject_type,subject_id,prior_state,new_state,reason,idempotency_key) VALUES($1,'user',$2,NULL,$3,$4,$5,'track',$6,$7,$8,$9,NULL) RETURNING seq"#, project as ProjectId,user.user_id as UserId,channel,user.via.client,action,id,prior as _,new as _,reason).fetch_one(c).await?;
    Ok(())
}
pub(crate) struct Event {
    pub seq: i64,
    pub occurred_at: Timestamp,
    pub action: String,
    pub actor_kind: String,
    pub actor_user_id: Option<UserId>,
    pub actor_service_id: Option<ServiceAccountId>,
    pub via_channel: String,
    pub via_client: Option<String>,
    pub prior_state: Document,
    pub new_state: Document,
    pub reason: Option<String>,
}
struct Raw {
    seq: i64,
    occurred_at: Timestamp,
    action: String,
    actor_kind: String,
    actor_user_id: Option<UserId>,
    actor_service_id: Option<ServiceAccountId>,
    via_channel: String,
    via_client: Option<String>,
    prior_state: Option<String>,
    new_state: Option<String>,
    reason: Option<String>,
}
pub(crate) async fn history(
    c: &mut PgConnection,
    id: &str,
    after: Option<&BigInt>,
    limit: i64,
    budget: usize,
) -> Result<Vec<Event>> {
    let after_narrow = after.map(ToPrimitive::to_i64).transpose_option();
    let rows = if let Some(after_narrow) = after_narrow {
        sqlx::query_as!(Raw,r#"SELECT seq,occurred_at AS "occurred_at!: _",action,actor_kind,actor_user_id AS "actor_user_id?: _",actor_service_id AS "actor_service_id?: _",via_channel,via_client,prior_state::text AS prior_state,new_state::text AS new_state,reason FROM audit_events WHERE subject_type='track' AND subject_id=$1 AND ($2::bigint IS NULL OR seq>$2) ORDER BY seq LIMIT $3"#,id,after_narrow,limit).fetch_all(c).await?
    } else {
        let after = after.ok_or("missing wide cursor")?;
        let after = PgInteger::new(after)?;
        // Wide NUMERIC signatures cannot have a successful explicit-bigint execution.
        // Keep them unnamed and distinct from successful narrow statement history.
        sqlx::query_as!(Raw,r#"SELECT seq,occurred_at AS "occurred_at!: _",action,actor_kind,actor_user_id AS "actor_user_id?: _",actor_service_id AS "actor_service_id?: _",via_channel,via_client,prior_state::text AS prior_state,new_state::text AS new_state,reason FROM audit_events WHERE subject_type='track' AND subject_id=$1 AND ($2::bigint IS NULL OR seq>$2) ORDER BY seq LIMIT $3 /*track-wide-cursor*/"#,id,after as _,limit).fetch_all(Unprepared(c)).await?
    };
    rows.into_iter()
        .map(|r| {
            Ok(Event {
                seq: r.seq,
                occurred_at: r.occurred_at,
                action: r.action,
                actor_kind: r.actor_kind,
                actor_user_id: r.actor_user_id,
                actor_service_id: r.actor_service_id,
                via_channel: r.via_channel,
                via_client: r.via_client,
                prior_state: json::decode_str(r.prior_state.as_deref().unwrap_or("null"), budget)?,
                new_state: json::decode_str(r.new_state.as_deref().unwrap_or("null"), budget)?,
                reason: r.reason,
            })
        })
        .collect()
}
#[allow(
    clippy::option_option,
    reason = "Distinguish absent cursor from a present out-of-range cursor"
)]
trait TransposeOption {
    fn transpose_option(self) -> Option<Option<i64>>;
}
impl TransposeOption for Option<Option<i64>> {
    fn transpose_option(self) -> Option<Option<i64>> {
        match self {
            None => Some(None),
            Some(Some(v)) => Some(Some(v)),
            Some(None) => None,
        }
    }
}

pub(crate) use cannery_core::pg_integer::PgInteger;
use sqlx::{Postgres, postgres::PgTypeInfo};
struct Unprepared<'c>(&'c mut PgConnection);
impl Unprepared<'_> {
    async fn run<'q, E>(
        self,
        mut q: E,
    ) -> std::result::Result<
        Vec<sqlx::Either<sqlx::postgres::PgQueryResult, sqlx::postgres::PgRow>>,
        sqlx::Error,
    >
    where
        E: sqlx::Execute<'q, Postgres>,
    {
        use futures_util::TryStreamExt;
        let args = q
            .take_arguments()
            .map_err(sqlx::Error::Encode)?
            .unwrap_or_default();
        self.0
            .fetch_many(sqlx::query_with(q.sql(), args).persistent(false))
            .try_collect()
            .await
    }
}
impl<'c> sqlx::Executor<'c> for Unprepared<'c> {
    type Database = Postgres;
    fn fetch_many<'e, 'q: 'e, E>(
        self,
        q: E,
    ) -> futures_util::stream::BoxStream<
        'e,
        std::result::Result<
            sqlx::Either<sqlx::postgres::PgQueryResult, sqlx::postgres::PgRow>,
            sqlx::Error,
        >,
    >
    where
        'c: 'e,
        E: 'q + sqlx::Execute<'q, Postgres>,
    {
        use futures_util::StreamExt;
        futures_util::stream::once(async move { self.run(q).await })
            .flat_map(|r| match r {
                Ok(rows) => futures_util::stream::iter(rows.into_iter().map(Ok)).boxed(),
                Err(e) => futures_util::stream::once(async { Err(e) }).boxed(),
            })
            .boxed()
    }
    fn fetch_optional<'e, 'q: 'e, E>(
        self,
        q: E,
    ) -> futures_util::future::BoxFuture<
        'e,
        std::result::Result<Option<sqlx::postgres::PgRow>, sqlx::Error>,
    >
    where
        'c: 'e,
        E: 'q + sqlx::Execute<'q, Postgres>,
    {
        Box::pin(async move {
            Ok(self.run(q).await?.into_iter().find_map(|r| match r {
                sqlx::Either::Right(r) => Some(r),
                sqlx::Either::Left(_) => None,
            }))
        })
    }
    fn prepare_with<'e, 'q: 'e>(
        self,
        sql: &'q str,
        p: &'e [PgTypeInfo],
    ) -> futures_util::future::BoxFuture<
        'e,
        std::result::Result<sqlx::postgres::PgStatement<'q>, sqlx::Error>,
    >
    where
        'c: 'e,
    {
        self.0.prepare_with(sql, p)
    }
    fn describe<'e, 'q: 'e>(
        self,
        sql: &'q str,
    ) -> futures_util::future::BoxFuture<
        'e,
        std::result::Result<sqlx::Describe<Postgres>, sqlx::Error>,
    >
    where
        'c: 'e,
    {
        self.0.describe(sql)
    }
}
impl std::fmt::Debug for Unprepared<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Unprepared([redacted])")
    }
}
