//! Source's id-only lookup; unrelated unit metadata is never decoded.
use cannery_core::{
    ids::{ProjectId, UnitId},
    pg_integer::Integer,
};
use num_bigint::BigInt;
use sqlx::PgConnection;
/// An explicit intermediate executor profile, pending physical pool ownership.
#[derive(Clone, Copy)]
pub enum LookupContext {
    BorrowedUnnamed,
}
pub(crate) async fn unit(
    connection: &mut PgConnection,
    project: ProjectId,
    number: &BigInt,
    _profile: LookupContext,
) -> Result<Option<UnitId>, Box<dyn std::error::Error + Send + Sync>> {
    let integer = Integer::new(number)?;
    let row = sqlx::query!(
        "SELECT id AS \"id!: UnitId\" FROM units WHERE project_id=$1 AND number=$2",
        project as ProjectId,
        &integer as _
    )
    .fetch_optional(connection)
    .await?;
    Ok(row.map(|row| row.id))
}
