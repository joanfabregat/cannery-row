//! Draft-creation idempotency, owned by the enclosing transaction.
use cannery_core::json::Document;
use sqlx::PgConnection;
pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub(crate) fn hash(project: &str, document: &Document, budget: usize) -> Result<Vec<u8>> {
    crate::hypothesis_idempotency::creation_hash(project, document, budget)
}
pub(crate) async fn lookup(
    c: &mut PgConnection,
    actor: &str,
    key: &str,
) -> Result<Option<(Vec<u8>, String)>> {
    let lock = format!("hypothesis.create\n{actor}\n{key}");
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        lock
    )
    .execute(&mut *c)
    .await?;
    let row=sqlx::query!("SELECT request_hash,result_id FROM idempotency_keys WHERE scope='hypothesis.create' AND actor=$1 AND key=$2",actor,key).fetch_optional(c).await?;
    Ok(row.map(|r| (r.request_hash, r.result_id)))
}
pub(crate) async fn remember(
    c: &mut PgConnection,
    actor: &str,
    key: &str,
    hash: &[u8],
    id: &str,
) -> Result<()> {
    sqlx::query!("INSERT INTO idempotency_keys(scope,actor,key,request_hash,result_id) VALUES('hypothesis.create',$1,$2,$3,$4)",actor,key,hash,id).execute(c).await?;
    Ok(())
}
