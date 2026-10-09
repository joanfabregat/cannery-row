// SPDX-License-Identifier: AGPL-3.0-only
//! Approving a plan revision: one transaction that creates a queued
//! hypothesis per new unit, writes a new revision of each changed queued
//! unit, cancels the queued units the plan drops and applies its alignment
//! entries. The caller holds the track lock, records the review and moves a
//! planning track to active.
use crate::{
    hypothesis_routes::{Failure, RouteState, domain, internal},
    plan_routes::{check_planned, replace_links},
    plan_units::{self, UnitFields},
    requests::RequestContext,
};
use cannery_core::{errors::ErrorCode, ids::HypothesisId, principal::Principal};
use cannery_hypotheses::repo as hypotheses;
use cannery_projects::repo as projects;
use cannery_tracks::{
    plans::{self, PlanRevision},
    repo::Track,
};
use serde_json::{Value, json};
use sqlx::PgConnection;
use std::collections::{BTreeMap, BTreeSet};

fn persistence(context: &RequestContext) -> impl Fn(cannery_tracks::repo::TrackError) -> Failure {
    move |_| internal(context, "plan approval")
}

/// Apply an approved revision. Returns what changed, for the audit record.
#[allow(
    clippy::too_many_lines,
    reason = "The approval's writes stay in one visible order inside one transaction"
)]
pub(crate) async fn apply(
    conn: &mut PgConnection,
    state: &RouteState,
    principal: &Principal,
    project: &projects::Project,
    track: &Track,
    plan: &PlanRevision,
    context: &RequestContext,
) -> Result<Value, Failure> {
    let entries = plans::units(conn, plan.id)
        .await
        .map_err(persistence(context))?;
    // Pass 1: every new unit gets its number, so units can name each other.
    // Keys of units earlier approvals wrote stay valid names for them.
    let mut ids: BTreeMap<String, (HypothesisId, i32)> = plans::known_keys(conn, track.id)
        .await
        .map_err(persistence(context))?
        .into_iter()
        .map(|(key, id, number)| (key, (id, number)))
        .collect();
    let mut created = Vec::new();
    for entry in &entries {
        let fields: UnitFields = serde_json::from_str(&entry.fields)
            .map_err(|_| internal(context, "approved unit fields"))?;
        if let (Some(id), Some(number)) = (entry.hypothesis_id, entry.number) {
            ids.insert(entry.key.clone(), (id, number));
            continue;
        }
        let number = hypotheses::next_number(conn, project.id)
            .await
            .map_err(|_| internal(context, "unit number"))?;
        let id = plans::create_unit(
            conn,
            plans::NewUnit {
                project_id: project.id,
                number,
                track_id: track.id,
                title: &fields.title,
                created_by: plan.created_by,
            },
        )
        .await
        .map_err(persistence(context))?;
        ids.insert(entry.key.clone(), (id, number));
        created.push(number);
    }
    // Pass 2: each unit's hypothesis revision, with its relations resolved.
    let mut revised = Vec::new();
    for entry in &entries {
        let fields: UnitFields = serde_json::from_str(&entry.fields)
            .map_err(|_| internal(context, "approved unit fields"))?;
        let (id, number) = ids[&entry.key];
        let is_new = entry.hypothesis_id.is_none();
        if !is_new && entry.state.as_deref() != Some("queued") {
            plans::applied(conn, plan.id, &entry.key, id, None)
                .await
                .map_err(persistence(context))?;
            continue;
        }
        let mut relations = Vec::new();
        for relation in &fields.relations {
            let target = match (&relation.hypothesis, &relation.unit) {
                (Some(target), _) => target.clone(),
                (None, Some(unit)) => {
                    let (_, target) = ids.get(unit).ok_or_else(|| {
                        domain(
                            ErrorCode::Conflict,
                            format!("unit '{}' names an unknown unit '{unit}'", entry.key),
                        )
                    })?;
                    Value::from(*target)
                }
                (None, None) => continue,
            };
            relations.push(json!({"kind": relation.kind, "hypothesis": target}));
        }
        let value = plan_units::hypothesis_value(&track.slug, &fields, &relations);
        let checked = check_planned(conn, state, principal, project, &value, id, context).await?;
        let encoded =
            serde_json::to_string(&value).map_err(|_| internal(context, "unit content"))?;
        let revision = if is_new {
            Some(1)
        } else {
            let current = plans::project_unit(conn, project.id, number)
                .await
                .map_err(persistence(context))?
                .ok_or_else(|| internal(context, "queued unit"))?
                .1;
            let stored = plans::unit_revision(conn, id, current.revision)
                .await
                .map_err(persistence(context))?
                .ok_or_else(|| internal(context, "queued unit revision"))?;
            let unchanged = serde_json::from_str::<Value>(&stored.content).ok()
                == Some(value.clone())
                && stored.brief.as_deref().unwrap_or_default() == entry.brief;
            if unchanged {
                None
            } else {
                Some(
                    plans::revise_unit(conn, id, &fields.title)
                        .await
                        .map_err(persistence(context))?
                        .ok_or_else(|| {
                            domain(
                                ErrorCode::Conflict,
                                format!("#{number} is no longer queued"),
                            )
                        })?,
                )
            }
        };
        if let Some(revision) = revision {
            plans::add_unit_revision(
                conn,
                plans::NewUnitRevision {
                    hypothesis_id: id,
                    revision,
                    content: &encoded,
                    brief: &entry.brief,
                    science_revision: checked.science_revision,
                    author: plan.created_by,
                    via_channel: &plan.via_channel,
                    via_client: plan.via_client.as_deref(),
                },
            )
            .await
            .map_err(persistence(context))?;
            replace_links(conn, id, checked.relations, checked.mentions, context).await?;
            if !is_new {
                revised.push(number);
            }
        }
        plans::applied(conn, plan.id, &entry.key, id, revision)
            .await
            .map_err(persistence(context))?;
    }
    // Queued units of the approved plan that this revision drops.
    let listed: BTreeSet<HypothesisId> = ids.values().map(|(id, _)| *id).collect();
    let mut cancelled = Vec::new();
    if let Some(base) = plans::approved_revision(conn, track.id)
        .await
        .map_err(persistence(context))?
        && let Some(base) = plans::get(conn, track.id, base)
            .await
            .map_err(persistence(context))?
    {
        for entry in plans::units(conn, base.id)
            .await
            .map_err(persistence(context))?
        {
            if let (Some(id), Some(number)) = (entry.hypothesis_id, entry.number)
                && !listed.contains(&id)
                && plans::cancel_queued(conn, id)
                    .await
                    .map_err(persistence(context))?
            {
                cancelled.push(number);
            }
        }
    }
    // Alignment: obsolete and redo cancel an in-flight unit; a decided unit
    // keeps its decision and is marked obsolete by the entry itself.
    let mut obsolete = Vec::new();
    for alignment in plans::alignments(conn, plan.id)
        .await
        .map_err(persistence(context))?
    {
        if alignment.decision == "keep" {
            continue;
        }
        obsolete.push(alignment.number);
        if plans::IN_FLIGHT.contains(&alignment.state.as_str()) {
            plans::cancel_in_flight(conn, alignment.hypothesis_id)
                .await
                .map_err(persistence(context))?;
            cancelled.push(alignment.number);
        }
    }
    Ok(json!({
        "created": created,
        "revised": revised,
        "cancelled": cancelled,
        "obsolete": obsolete,
    }))
}
