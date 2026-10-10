// SPDX-License-Identifier: AGPL-3.0-only
//! Whether a unit can be claimed now, by the conditions `claim_unit` checks
//! on the unit itself: it is queued, its track is active and no concern is
//! open on its track. Read-only; a claim checks again under its lock, and
//! also matches the caller's mode to the track's.
use cannery_core::ids::UnitId;
use sqlx::PgConnection;
use std::collections::BTreeMap;

/// Each unit's reason it cannot be claimed now, or none when it can.
pub(crate) async fn reasons(
    conn: &mut PgConnection,
    ids: &[UnitId],
) -> Result<BTreeMap<UnitId, Option<String>>, sqlx::Error> {
    let ids: Vec<uuid::Uuid> = ids.iter().map(|id| id.0).collect();
    let rows = sqlx::query!(
        r#"
        SELECT h.id AS "id!: UnitId", h.state AS "unit_state!", t.slug AS "track!",
               t.state AS "track_state!",
               EXISTS (SELECT 1 FROM concerns c
                       WHERE c.track_id = t.id AND c.state = 'open') AS "concern_open!"
        FROM units h JOIN tracks t ON t.id = h.track_id
        WHERE h.id = ANY($1::uuid[])
        "#,
        &ids as _
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let reason = if row.unit_state != "queued" {
                Some(format!(
                    "the unit is {}: only a queued unit is claimed",
                    row.unit_state
                ))
            } else if row.track_state != "active" {
                Some(format!(
                    "the {} track is {}, not active",
                    row.track, row.track_state
                ))
            } else if row.concern_open {
                Some(format!(
                    "a concern is open on the {} track: no unit of it is claimed until a plan revision answers it or a researcher dismisses it",
                    row.track
                ))
            } else {
                None
            };
            (row.id, reason)
        })
        .collect())
}
