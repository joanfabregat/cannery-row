// SPDX-License-Identifier: AGPL-3.0-only
//! An attempt's context bundle: what its performer reads before working,
//! assembled on demand from the revisions the attempt pinned at its claim
//! and never stored. The brief, the plan's approach, the unit's fields and
//! brief, a one-line index of the track's other units, and a summary line
//! with a reference for each context item and each unit it derives from.
use crate::{
    api_models::{ContextBundleRef, ContextItem, PlanRef},
    hypothesis_routes::{Failure, RouteState, domain, internal},
    plan_routes::{context_label, paths, positive},
    plan_units::{self, UnitFields, first_line, truncate},
    requests::RequestContext,
};
use axum::{
    extract::{Request, State},
    http::header,
    response::{IntoResponse, Response},
};
use cannery_core::{errors::ErrorCode, ids::AttemptId};
use cannery_projects::{authz, briefs, repo as projects};
use cannery_tracks::plans::{self, AttemptPins, Limits};
use serde_json::Value;
use sqlx::PgConnection;
use std::fmt::Write as _;

/// The compact bundle's cap, in bytes.
pub(crate) const COMPACT_MAX_BYTES: usize = 16_384;

fn persistence(context: &RequestContext) -> impl Fn(cannery_tracks::repo::TrackError) -> Failure {
    move |_| internal(context, "context bundle")
}

/// The revisions an attempt runs under, as claims, jobs and attempt reads
/// hand them out.
#[derive(Default)]
pub(crate) struct Pins {
    pub(crate) brief: Option<crate::api_models::BriefRef>,
    pub(crate) plan: Option<PlanRef>,
    pub(crate) context: Option<ContextBundleRef>,
}

/// The brief, plan and context bundle an attempt pinned.
/// # Errors
/// Returns a sanitized persistence failure.
pub(crate) async fn pins(
    conn: &mut PgConnection,
    project: &projects::Project,
    attempt: AttemptId,
    context: &RequestContext,
) -> Result<Pins, Failure> {
    let brief = crate::brief_routes::pinned(conn, &project.slug, attempt)
        .await
        .map_err(|error| Failure::new(context.project_error(error)))?;
    let (plan, bundle) = refs(conn, project, attempt, context).await?;
    Ok(Pins {
        brief,
        plan,
        context: bundle,
    })
}

/// Where an attempt's bundle is read.
pub(crate) fn bundle_path(slug: &str, number: i32, sequence: i32) -> String {
    format!("/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/context.md")
}

/// The plan revision an attempt pinned and its bundle's reference, as claims,
/// jobs and attempt reads hand them out; both absent when the attempt pinned
/// no plan revision.
/// # Errors
/// Returns a sanitized persistence failure.
pub(crate) async fn refs(
    conn: &mut PgConnection,
    project: &projects::Project,
    attempt: AttemptId,
    context: &RequestContext,
) -> Result<(Option<PlanRef>, Option<ContextBundleRef>), Failure> {
    let pins = plans::attempt_pins_by_id(conn, attempt)
        .await
        .map_err(persistence(context))?
        .ok_or_else(|| internal(context, "attempt pins"))?;
    let Some(revision) = pins.plan_revision else {
        return Ok((None, None));
    };
    let bundle = build(conn, project, &pins, false, context).await?;
    Ok((
        Some(PlanRef {
            revision: i64::from(revision),
            r#ref: format!(
                "/api/projects/{}/tracks/{}/plans/{revision}",
                project.slug, pins.track_slug
            ),
        }),
        Some(ContextBundleRef {
            r#ref: bundle_path(&project.slug, pins.number, pins.sequence),
            bytes: i64::try_from(bundle.len()).unwrap_or(i64::MAX),
        }),
    ))
}

fn yaml(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| String::from("null"))
}

fn limit(value: i32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// The summary line and reference of one context item.
async fn item_line(
    conn: &mut PgConnection,
    project: &projects::Project,
    item: &ContextItem,
    keys: &std::collections::BTreeMap<String, i32>,
    limits: Limits,
    context: &RequestContext,
) -> Result<String, Failure> {
    let slug = &project.slug;
    let number = |value: &Value| match value {
        Value::String(key) => keys.get(key).copied(),
        other => other.as_i64().and_then(|number| i32::try_from(number).ok()),
    };
    let (summary, reference) = match item.kind.as_str() {
        "unit" => match item.unit.as_ref().and_then(number) {
            Some(number) => {
                match plans::project_unit(conn, project.id, number)
                    .await
                    .map_err(persistence(context))?
                {
                    Some((_, unit)) => {
                        let brief = plans::current_unit_brief(conn, unit.hypothesis_id)
                            .await
                            .map_err(persistence(context))?
                            .map(|brief| first_line(&brief))
                            .unwrap_or_default();
                        let detail = if brief.is_empty() {
                            String::new()
                        } else {
                            format!(": {brief}")
                        };
                        (
                            format!("#{number} {} ({}){detail}", unit.title, unit.state),
                            format!("/api/projects/{slug}/units/{number}"),
                        )
                    }
                    None => (format!("#{number} (not found)"), String::new()),
                }
            }
            None => (context_label(item), String::new()),
        },
        "writeup" => {
            let unit = item.unit.as_ref().and_then(number).unwrap_or_default();
            let attempt = item
                .attempt
                .and_then(|attempt| i32::try_from(attempt).ok())
                .unwrap_or_default();
            let text = match plans::attempt_id(conn, project.id, unit, attempt)
                .await
                .map_err(persistence(context))?
            {
                Some(id) => plans::attempt_report(conn, id)
                    .await
                    .map_err(persistence(context))?
                    .map_or_else(|| String::from("no write-up yet"), |text| first_line(&text)),
                None => String::from("not found"),
            };
            (
                format!("write-up of #{unit}.{attempt}: {text}"),
                format!("/api/projects/{slug}/hypotheses/{unit}/attempts/{attempt}/report"),
            )
        }
        _ => {
            let id = item.artifact.clone().unwrap_or_default();
            let line = match uuid::Uuid::parse_str(&id) {
                Ok(uuid) => plans::artifact(conn, project.id, uuid)
                    .await
                    .map_err(persistence(context))?,
                Err(_) => None,
            };
            (
                line.map_or_else(
                    || format!("artifact {id} (not found)"),
                    |line| {
                        format!(
                            "artifact {} ({}, {} bytes) from #{}.{}",
                            line.role, line.media_type, line.size_bytes, line.number, line.sequence
                        )
                    },
                ),
                format!("/api/projects/{slug}/artifacts/{id}"),
            )
        }
    };
    let summary = item
        .note
        .as_ref()
        .map_or(summary.clone(), |note| format!("{summary}. {note}"));
    let summary = truncate(&summary, limit(limits.context_summary_max_bytes));
    Ok(if reference.is_empty() {
        format!("- {summary}")
    } else {
        format!("- {summary} ({reference})")
    })
}

/// Build an attempt's bundle. `compact` keeps the brief's goal, the unit's
/// fields and brief and the index, capped at 16 KiB.
#[allow(
    clippy::too_many_lines,
    reason = "The bundle's sections are written in the order a performer reads them"
)]
pub(crate) async fn build(
    conn: &mut PgConnection,
    project: &projects::Project,
    pins: &AttemptPins,
    compact: bool,
    context: &RequestContext,
) -> Result<String, Failure> {
    let limits = plans::limits(conn, project.id)
        .await
        .map_err(persistence(context))?;
    let brief = match pins.brief_revision {
        Some(revision) => briefs::get_brief(conn, project.id, Some(revision))
            .await
            .map_err(|error| Failure::new(context.project_error(error)))?,
        None => None,
    };
    let plan = match pins.plan_revision {
        Some(revision) => plans::get(conn, pins.track_id, revision)
            .await
            .map_err(persistence(context))?,
        None => None,
    };
    let stored = plans::unit_revision(conn, pins.hypothesis_id, pins.hypothesis_revision)
        .await
        .map_err(persistence(context))?
        .ok_or_else(|| internal(context, "bundle unit revision"))?;
    let document: Value =
        serde_json::from_str(&stored.content).map_err(|_| internal(context, "bundle unit"))?;
    let mut fields = plan_units::fields_from_hypothesis(&document);
    let mut key = None;
    let mut keys: std::collections::BTreeMap<String, i32> = plans::known_keys(conn, pins.track_id)
        .await
        .map_err(persistence(context))?
        .into_iter()
        .map(|(key, _, number)| (key, number))
        .collect();
    if let Some(plan) = &plan {
        for entry in plans::units(conn, plan.id)
            .await
            .map_err(persistence(context))?
        {
            if let Some(number) = entry.number {
                keys.insert(entry.key.clone(), number);
            }
            if entry.hypothesis_id == Some(pins.hypothesis_id) {
                let planned: UnitFields = serde_json::from_str(&entry.fields)
                    .map_err(|_| internal(context, "bundle plan fields"))?;
                fields.context = planned.context;
                key = Some(entry.key.clone());
            }
        }
    }
    let mut body = String::new();
    let _ = writeln!(
        body,
        "# Context for #{}.{}: {}\n",
        pins.number, pins.sequence, pins.title
    );
    match &brief {
        Some(brief) => {
            let front_matter: Value = serde_json::from_str(&brief.front_matter).unwrap_or_default();
            let goal = front_matter
                .get("goal")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let _ = writeln!(body, "## Brief (revision {})\n", brief.revision);
            if compact {
                let _ = writeln!(body, "Goal: {goal}\n");
            } else {
                let title = front_matter
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let _ = writeln!(body, "{title}\n\nGoal: {goal}\n\n{}\n", brief.body.trim());
            }
        }
        None => body.push_str("## Brief\n\nThe project had no brief at the claim.\n\n"),
    }
    if !compact && let Some(plan) = &plan {
        let _ = writeln!(
            body,
            "## Plan approach ({} plan, revision {})\n\n{}\n",
            pins.track_slug,
            plan.revision,
            plan.approach.trim()
        );
    }
    let _ = writeln!(
        body,
        "## Unit #{}{}: {}\n",
        pins.number,
        key.as_ref()
            .map_or_else(String::new, |key| format!(" `{key}`")),
        fields.title
    );
    let _ = writeln!(body, "- Question: {}", fields.question);
    let _ = writeln!(body, "- Intervention: {}", fields.intervention);
    if let Some(control) = &fields.control {
        let _ = writeln!(body, "- Control: {}", yaml(control));
    }
    let _ = writeln!(
        body,
        "- Hypothesis revision: {}\n\n### Acceptance\n\n```json\n{}\n```\n",
        pins.hypothesis_revision,
        serde_json::to_string_pretty(&fields.acceptance).unwrap_or_default()
    );
    if let Some(parameters) = &fields.parameters {
        let _ = writeln!(
            body,
            "### Parameters\n\n```json\n{}\n```\n",
            serde_json::to_string_pretty(parameters).unwrap_or_default()
        );
    }
    if let Some(brief) = stored
        .brief
        .as_deref()
        .filter(|brief| !brief.trim().is_empty())
    {
        let _ = writeln!(body, "### Unit brief\n\n{}\n", brief.trim());
    }
    let index = plans::track_units(conn, pins.track_id, None, None, i64::MAX)
        .await
        .map_err(persistence(context))?;
    let others: Vec<_> = index
        .iter()
        .rev()
        .filter(|unit| unit.hypothesis_id != pins.hypothesis_id)
        .collect();
    let _ = writeln!(body, "## Track {}: other units\n", pins.track_slug);
    if others.is_empty() {
        body.push_str("None.\n");
    }
    for unit in others {
        let key = unit
            .key
            .as_ref()
            .map_or_else(String::new, |key| format!(" `{key}`"));
        let obsolete = if unit.obsolete { ", obsolete" } else { "" };
        let line = format!(
            "- #{}{key} {} ({}{obsolete})",
            unit.number, unit.title, unit.state
        );
        let _ = writeln!(
            body,
            "{}",
            truncate(&line, limit(limits.index_line_max_bytes))
        );
    }
    body.push('\n');
    if !compact {
        if !fields.context.is_empty() {
            body.push_str("## Context\n\n");
            for item in &fields.context {
                let line = item_line(conn, project, item, &keys, limits, context).await?;
                let _ = writeln!(body, "{line}");
            }
            body.push('\n');
        }
        let derived: Vec<i32> = fields
            .relations
            .iter()
            .filter(|relation| relation.kind == "derived_from")
            .filter_map(|relation| relation.hypothesis.as_ref()?.as_i64())
            .filter_map(|number| i32::try_from(number).ok())
            .collect();
        if !derived.is_empty() {
            body.push_str("## Derived from\n\n");
            for number in derived {
                let line = match plans::project_unit(conn, project.id, number)
                    .await
                    .map_err(persistence(context))?
                {
                    Some((_, unit)) => {
                        match plans::latest_output(conn, unit.hypothesis_id)
                            .await
                            .map_err(persistence(context))?
                        {
                            Some((sequence, state, text)) => format!(
                                "#{number} {} ({}): #{number}.{sequence} {state}: {} \
                                 (/api/projects/{}/hypotheses/{number}/attempts/{sequence}/report)",
                                unit.title,
                                unit.state,
                                text.map(|text| first_line(&text)).unwrap_or_default(),
                                project.slug
                            ),
                            None => format!(
                                "#{number} {} ({}): no outputs yet (/api/projects/{}/units/{number})",
                                unit.title, unit.state, project.slug
                            ),
                        }
                    }
                    None => format!("#{number} (not found)"),
                };
                let _ = writeln!(
                    body,
                    "- {}",
                    truncate(&line, limit(limits.context_summary_max_bytes))
                );
            }
            body.push('\n');
        }
    }
    let header = |bytes: usize| {
        let mut header = String::from("---\n");
        let _ = writeln!(header, "project: {}", yaml(&project.slug));
        let _ = writeln!(
            header,
            "attempt: {}",
            yaml(&format!("#{}.{}", pins.number, pins.sequence))
        );
        let _ = writeln!(header, "track: {}", yaml(&pins.track_slug));
        let _ = writeln!(header, "unit: {}", yaml(&key));
        let _ = writeln!(header, "hypothesis_revision: {}", pins.hypothesis_revision);
        let _ = writeln!(header, "brief_revision: {}", yaml(&pins.brief_revision));
        let _ = writeln!(header, "plan_revision: {}", yaml(&pins.plan_revision));
        let _ = writeln!(
            header,
            "detail: {}",
            if compact { "compact" } else { "full" }
        );
        let _ = writeln!(header, "bytes: {bytes}");
        header.push_str("---\n\n");
        header
    };
    if compact {
        let room = COMPACT_MAX_BYTES.saturating_sub(header(COMPACT_MAX_BYTES).len() + 120);
        if body.len() > room {
            let mut end = room;
            while end > 0 && !body.is_char_boundary(end) {
                end -= 1;
            }
            let cut = body[..end].rfind("\n\n").map_or(end, |at| at + 1);
            body.truncate(cut);
            let _ = writeln!(
                body,
                "\n_Truncated at {COMPACT_MAX_BYTES} bytes; read the full bundle for the rest._"
            );
        }
    }
    // The size includes the header that states it.
    let mut bytes = body.len();
    loop {
        let total = header(bytes).len() + body.len();
        if total == bytes {
            break;
        }
        bytes = total;
    }
    Ok(format!("{}{body}", header(bytes)))
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/context.md",
    operation_id = "get_context_api_projects__slug__hypotheses__number__attempts__sequence__context_md_get",
    summary = "Get Context Bundle",
    description = "The attempt's context bundle as Markdown, assembled from the revisions it\npinned at its claim: the brief, the plan's approach, the unit's fields and\nbrief, an index of the track's other units, and a summary line and\nreference for each context item and each unit it derives from. The front\nmatter states its size in bytes. `detail=compact` keeps the brief's goal,\nthe unit and the index, capped at 16 KiB.",
    params(("slug" = String, Path), ("number" = i64, Path), ("sequence" = i64, Path),
        ("detail" = Option<String>, Query, description = "`full` (the default) or `compact`.")),
    responses((status = 200, description = "Successful Response", body = String, content_type = "text/markdown"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn route(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let mut auth =
        crate::authentication::authenticate(&state.app, &context, &parts.headers, &parts.method)
            .await
            .map_err(Failure::new)?;
    let paths = paths(&mut parts, &state).await?;
    let compact = match parts.uri.query().unwrap_or_default() {
        "" | "detail=full" => false,
        "detail=compact" => true,
        _ => {
            return Err(crate::plan_routes::invalid(
                "query/detail",
                "Input should be 'full' or 'compact'",
            ));
        }
    };
    let number = positive(&paths, "number")?;
    let sequence = positive(&paths, "sequence")?;
    let project = authz::project_read(&mut auth.connection, &auth.principal, &paths["slug"])
        .await
        .map_err(|error| Failure::new(context.project_error(error)))?;
    let pins = plans::attempt_pins(&mut auth.connection, project.id, number, sequence)
        .await
        .map_err(persistence(&context))?
        .ok_or_else(|| {
            domain(
                ErrorCode::NotFound,
                format!("attempt #{number}.{sequence} not found"),
            )
        })?;
    let bundle = build(&mut auth.connection, &project, &pins, compact, &context).await?;
    Ok((
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        bundle,
    )
        .into_response())
}
