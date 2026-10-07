//! Job claim replay records share the caller transaction and advisory key lock.
use sqlx::PgConnection;
type Result<T> = std::result::Result<T, sqlx::Error>;
pub(crate) async fn lookup(
    c: &mut PgConnection,
    actor: &str,
    key: &str,
) -> Result<Option<(Vec<u8>, String)>> {
    let lock = format!("job.claim\n{actor}\n{key}");
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        lock
    )
    .execute(&mut *c)
    .await?;
    let row = sqlx::query!(
        "SELECT request_hash,result_id FROM idempotency_keys WHERE scope='job.claim' AND actor=$1 AND key=$2",
        actor,
        key
    )
    .fetch_optional(c)
    .await?;
    Ok(row.map(|row| (row.request_hash, row.result_id)))
}
pub(crate) async fn remember(
    c: &mut PgConnection,
    actor: &str,
    key: &str,
    hash: &[u8],
    id: &str,
) -> Result<()> {
    sqlx::query!(
        "INSERT INTO idempotency_keys(scope,actor,key,request_hash,result_id) VALUES('job.claim',$1,$2,$3,$4)",
        actor,
        key,
        hash,
        id
    )
    .execute(c)
    .await?;
    Ok(())
}
pub(crate) async fn replace(c: &mut PgConnection, actor: &str, key: &str, id: &str) -> Result<()> {
    sqlx::query!(
        "UPDATE idempotency_keys SET result_id=$1 WHERE scope='job.claim' AND actor=$2 AND key=$3",
        id,
        actor,
        key
    )
    .execute(c)
    .await?;
    Ok(())
}
