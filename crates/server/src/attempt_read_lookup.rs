//! Source's id-only lookup; unrelated hypothesis metadata is never decoded.
use cannery_core::{
    ids::{HypothesisId, ProjectId},
    pg_integer::Integer,
};
use num_bigint::BigInt;
use sqlx::PgConnection;
/// An explicit intermediate executor profile, pending physical pool ownership.
#[derive(Clone, Copy)]
pub enum LookupContext {
    BorrowedUnnamed,
}
pub(crate) async fn hypothesis(
    connection: &mut PgConnection,
    project: ProjectId,
    number: &BigInt,
    _profile: LookupContext,
) -> Result<Option<HypothesisId>, Box<dyn std::error::Error + Send + Sync>> {
    let integer = Integer::new(number)?;
    let row = sqlx::query!(
        "SELECT id AS \"id!: HypothesisId\" FROM hypotheses WHERE project_id=$1 AND number=$2",
        project as ProjectId,
        &integer as _
    )
    .fetch_optional(connection)
    .await?;
    Ok(row.map(|row| row.id))
}
