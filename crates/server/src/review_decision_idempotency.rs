//! Review-scoped replay lookup, bound to the enclosing caller transaction.
use sqlx::PgConnection;
type Result<T> = std::result::Result<T, sqlx::Error>;
pub(crate) async fn lookup(
    c: &mut PgConnection,
    actor: &str,
    key: &str,
) -> Result<Option<(Vec<u8>, String)>> {
    let lock = format!("review.decide\n{actor}\n{key}");
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        lock
    )
    .execute(&mut *c)
    .await?;
    let row = sqlx::query!("SELECT request_hash,result_id FROM idempotency_keys WHERE scope='review.decide' AND actor=$1 AND key=$2",actor,key).fetch_optional(c).await?;
    Ok(row.map(|v| (v.request_hash, v.result_id)))
}
pub(crate) async fn remember(
    c: &mut PgConnection,
    actor: &str,
    key: &str,
    hash: &[u8],
    id: &str,
) -> Result<()> {
    sqlx::query!("INSERT INTO idempotency_keys(scope,actor,key,request_hash,result_id) VALUES('review.decide',$1,$2,$3,$4)",actor,key,hash,id).execute(c).await?;
    Ok(())
}
