// SPDX-License-Identifier: AGPL-3.0-only
//! Track plans. A researcher, or an agent acting through a researcher's
//! token, builds a plan revision as a draft one call at a time (approach,
//! units, alignment entries), checks it and submits it; a researcher then
//! approves it, sends it back or declines it. Approval creates and revises
//! the track's units in one transaction.
use crate::{
    api_models::{
        AlignmentOut, AlignmentSet, AnswerOut, AnswerSet, ContextItem, Page_PlanRevisionOut_int_,
        Page_UnitIndexOut_int_, PlanApproach, PlanCheckOut, PlanOut, PlanProblem, PlanReview,
        PlanRevisionOut, ProjectLimits, UnitAlignmentOut, UnitCreate, UnitHistoryOut, UnitIndexOut,
        UnitPlanOut, UnitRelation, UnitRevisionOut, UnitUpdate,
    },
    authentication::{Authenticated, authenticate},
    body::DecodedBody,
    errors::ApiError,
    plan_units::{self, UnitFields},
    requests::RequestContext,
    unit_mutations::{check_unit, unit_contract},
    unit_routes::{Failure, RouteState, domain, internal},
};
use axum::{
    Json, Router,
    extract::{FromRequestParts, Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use cannery_core::{
    audit::{self, Attribution, Record},
    errors::{DomainError, ErrorCode},
    ids::UnitId,
    principal::{Channel, Principal, Role},
};
use cannery_projects::{authz, briefs, repo as projects};
use cannery_tracks::{
    concerns,
    plans::{self, Limits, PlanRevision},
    repo::{self as tracks, RowLock, Track, TrackState},
};
use cannery_units::repo::{self as units, MentionSource};
use serde_json::{Value, json};
use sqlx::{Acquire, PgConnection};
use std::collections::{BTreeMap, BTreeSet};

/// Items per page when a list names no limit.
const PAGE: i64 = 50;

pub(crate) fn routes(state: RouteState) -> Router {
    Router::new()
        .route(
            "/api/projects/{slug}/tracks/{track_slug}/plans",
            get(list).post(start),
        )
        .route(
            "/api/projects/{slug}/tracks/{track_slug}/plans/draft/approach",
            put(approach),
        )
        .route(
            "/api/projects/{slug}/tracks/{track_slug}/plans/draft/units",
            post(add_unit),
        )
        .route(
            "/api/projects/{slug}/tracks/{track_slug}/plans/draft/units/{key}",
            put(update_unit).delete(drop_unit),
        )
        .route(
            "/api/projects/{slug}/tracks/{track_slug}/plans/draft/alignments/{number}",
            put(set_alignment),
        )
        .route(
            "/api/projects/{slug}/tracks/{track_slug}/plans/draft/answers/{concern_id}",
            put(set_answer).delete(drop_answer),
        )
        .route(
            "/api/projects/{slug}/tracks/{track_slug}/plans/draft/check",
            get(check),
        )
        .route(
            "/api/projects/{slug}/tracks/{track_slug}/plans/draft/submission",
            post(submit),
        )
        .route(
            "/api/projects/{slug}/tracks/{track_slug}/plans/{revision}",
            get(read),
        )
        .route(
            "/api/projects/{slug}/tracks/{track_slug}/plans/{revision}/plan.md",
            get(markdown),
        )
        .route(
            "/api/projects/{slug}/tracks/{track_slug}/plans/{revision}/review",
            post(review),
        )
        .route(
            "/api/projects/{slug}/tracks/{track_slug}/units",
            get(list_track_units),
        )
        .route("/api/projects/{slug}/units/{number}/plan", get(unit_plan))
        .route(
            "/api/projects/{slug}/units/{number}/history",
            get(unit_history),
        )
        .route("/api/projects/{slug}/limits", get(limits).put(set_limits))
        .route(
            "/api/projects/{slug}/units/{number}/attempts/{sequence}/context.md",
            get(crate::context_bundle::route),
        )
        .with_state(state)
}

// ---------------------------------------------------------------- helpers

pub(crate) async fn paths(
    parts: &mut axum::http::request::Parts,
    state: &RouteState,
) -> Result<BTreeMap<String, String>, Failure> {
    Path::<BTreeMap<String, String>>::from_request_parts(parts, state)
        .await
        .map(|Path(values)| values)
        .map_err(Failure::new)
}

pub(crate) fn invalid(path: &str, message: impl Into<String>) -> Failure {
    let message = message.into();
    Failure::new(ApiError::from(
        DomainError::new(ErrorCode::ValidationFailed, message.clone())
            .with_details(json!([{"path": path, "message": message}])),
    ))
}

/// A write refused because it exceeds one of the project's limits.
fn too_large(path: &str, what: &str, size: usize, limit: i32) -> Failure {
    let message = format!("{what} is {size} bytes; the project's limit is {limit} bytes");
    Failure::new(ApiError::from(
        DomainError::new(ErrorCode::ValidationFailed, message.clone()).with_details(json!([{
            "path": path, "message": message, "size": size, "limit": limit,
        }])),
    ))
}

/// A positive INT4 path value, or `422` naming the path.
pub(crate) fn positive(paths: &BTreeMap<String, String>, name: &str) -> Result<i32, Failure> {
    paths
        .get(name)
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            invalid(
                &format!("path/{name}"),
                "Input should be a positive integer",
            )
        })
}

fn query_value<'a>(parts: &'a axum::http::request::Parts, name: &str) -> Option<&'a str> {
    parts.uri.query().and_then(|query| {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (key == name).then_some(value)
        })
    })
}

/// `before` and `limit` of a paged list.
fn page(parts: &axum::http::request::Parts) -> Result<(Option<i32>, i64), Failure> {
    let before = query_value(parts, "before")
        .map(|value| {
            value
                .parse::<i32>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| invalid("query/before", "Input should be a positive integer"))
        })
        .transpose()?;
    let limit = query_value(parts, "limit")
        .map(|value| {
            value
                .parse::<i64>()
                .ok()
                .filter(|value| (1..=200).contains(value))
                .ok_or_else(|| invalid("query/limit", "Input should be between 1 and 200"))
        })
        .transpose()?
        .unwrap_or(PAGE);
    Ok((before, limit))
}

/// The JSON body as `T`, or `422` saying what does not match.
fn decode<T: serde::de::DeserializeOwned>(body: &DecodedBody) -> Result<T, Failure> {
    let value = match body {
        DecodedBody::Json(document) => cannery_core::json::to_value(document)
            .map_err(|_| invalid("body", "the body is not valid JSON"))?,
        DecodedBody::Missing => return Err(invalid("body", "Field required")),
        DecodedBody::RawBytes => return Err(invalid("body", "the body must be JSON")),
    };
    serde_json::from_value(value).map_err(|error| invalid("body", error.to_string()))
}

pub(crate) fn channel_name(channel: Channel) -> &'static str {
    match channel {
        Channel::Ui => "ui",
        Channel::Mcp => "mcp",
        Channel::Cli => "cli",
        Channel::Api | Channel::System => "api",
    }
}

fn track_error(context: &RequestContext) -> impl Fn(cannery_tracks::repo::TrackError) -> Failure {
    move |_| internal(context, "plan persistence")
}

fn project_failure(context: &RequestContext) -> impl Fn(cannery_projects::ProjectError) -> Failure {
    move |error| Failure::new(context.project_error(error))
}

/// The authenticated caller and the project, for a researcher's plan write.
/// Service accounts are refused.
async fn researcher(
    state: &RouteState,
    context: &RequestContext,
    parts: &mut axum::http::request::Parts,
) -> Result<(Authenticated, projects::Project, BTreeMap<String, String>), Failure> {
    let mut auth = authenticate(&state.app, context, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let paths = paths(parts, state).await?;
    let project = authz::project_access(
        &mut auth.connection,
        &auth.principal,
        &paths["slug"],
        Some(Role::Researcher),
        &[],
        true,
    )
    .await
    .map_err(project_failure(context))?
    .project;
    if !matches!(auth.principal, Principal::User(_)) {
        return Err(domain(ErrorCode::Forbidden, "only researchers write plans"));
    }
    Ok((auth, project, paths))
}

/// The authenticated caller and the project, for a read.
async fn reader(
    state: &RouteState,
    context: &RequestContext,
    parts: &mut axum::http::request::Parts,
) -> Result<(Authenticated, projects::Project, BTreeMap<String, String>), Failure> {
    let mut auth = authenticate(&state.app, context, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let paths = paths(parts, state).await?;
    let project = authz::project_read(&mut auth.connection, &auth.principal, &paths["slug"])
        .await
        .map_err(project_failure(context))?;
    Ok((auth, project, paths))
}

fn track_context(
    state: &RouteState,
    context: &RequestContext,
) -> Result<cannery_tracks::repo::JsonContext, Failure> {
    Ok(state
        .context
        .mutations
        .as_ref()
        .ok_or_else(|| internal(context, "plan track context"))?
        .tracks)
}

pub(crate) async fn load_track(
    conn: &mut PgConnection,
    state: &RouteState,
    context: &RequestContext,
    project: &projects::Project,
    slug: &str,
    lock: Option<RowLock>,
) -> Result<Track, Failure> {
    tracks::get_track(
        conn,
        project.id,
        &slug.to_owned(),
        lock,
        track_context(state, context)?,
    )
    .await
    .map_err(|_| internal(context, "plan track load"))?
    .ok_or_else(|| domain(ErrorCode::NotFound, format!("track '{slug}' not found")))
}

/// The track's open draft, or why there is none.
async fn draft(
    conn: &mut PgConnection,
    track: &Track,
    context: &RequestContext,
) -> Result<PlanRevision, Failure> {
    match plans::open(conn, track.id, true)
        .await
        .map_err(track_error(context))?
    {
        Some(plan) if plan.state == "draft" => Ok(plan),
        Some(plan) => Err(domain(
            ErrorCode::Conflict,
            format!(
                "revision {} of the {} plan is submitted for review; it changes no more",
                plan.revision, track.slug
            ),
        )),
        None => Err(domain(
            ErrorCode::NotFound,
            format!(
                "the {} plan has no draft; start one with start_plan_revision",
                track.slug
            ),
        )),
    }
}

async fn commit(
    transaction: sqlx::Transaction<'_, sqlx::Postgres>,
    context: &RequestContext,
) -> Result<(), Failure> {
    transaction
        .commit()
        .await
        .map_err(|_| internal(context, "plan commit"))
}

#[allow(
    clippy::too_many_arguments,
    reason = "One audit row: the plan, its action, its new state and the reason, under the caller's connection"
)]
async fn record(
    conn: &mut PgConnection,
    principal: &Principal,
    project: &projects::Project,
    plan: &PlanRevision,
    action: &'static str,
    new_state: &Value,
    reason: Option<&str>,
    context: &RequestContext,
) -> Result<(), Failure> {
    let subject = plan.id.to_string();
    audit::record(
        conn,
        Attribution::Principal(principal),
        Record {
            action,
            subject_type: "plan_revision",
            subject_id: &subject,
            project_id: Some(project.id),
            prior_state: None,
            new_state: Some(new_state),
            reason,
            idempotency_key: None,
        },
    )
    .await
    .map(|_| ())
    .map_err(|_| internal(context, "plan audit"))
}

fn index_out(unit: &plans::TrackUnit) -> UnitIndexOut {
    UnitIndexOut {
        number: i64::from(unit.number),
        key: unit.key.clone(),
        title: unit.title.clone(),
        state: unit.state.clone(),
        obsolete: unit.obsolete,
    }
}

fn answer_out(answer: concerns::Answer) -> AnswerOut {
    AnswerOut {
        concern: answer.concern_id.to_string(),
        kind: answer.kind,
        state: answer.state,
        how: answer.how,
    }
}

fn markdown_ref(slug: &str, track: &str, revision: i32) -> String {
    format!("/api/projects/{slug}/tracks/{track}/plans/{revision}/plan.md")
}

/// A revision with its units and alignment entries, as the API shows it.
pub(crate) async fn plan_out(
    conn: &mut PgConnection,
    project: &projects::Project,
    track: &Track,
    plan: &PlanRevision,
    context: &RequestContext,
) -> Result<PlanOut, Failure> {
    let units = plans::units(conn, plan.id)
        .await
        .map_err(track_error(context))?
        .iter()
        .map(|entry| plan_units::entry_out(entry).ok_or_else(|| internal(context, "plan unit")))
        .collect::<Result<Vec<_>, _>>()?;
    let alignments = plans::alignments(conn, plan.id)
        .await
        .map_err(track_error(context))?;
    let needs_alignment = if plan.state == "draft" {
        let aligned: BTreeSet<_> = alignments.iter().map(|a| a.unit_id).collect();
        plans::needing_alignment(conn, track.id)
            .await
            .map_err(track_error(context))?
            .iter()
            .filter(|unit| !aligned.contains(&unit.unit_id))
            .map(index_out)
            .collect()
    } else {
        Vec::new()
    };
    let answers = concerns::answers(conn, plan.id)
        .await
        .map_err(track_error(context))?;
    let needs_answer = if plan.state == "draft" {
        let answered: BTreeSet<_> = answers.iter().map(|answer| answer.concern_id).collect();
        concerns::open_for_track(conn, project.id, track.id)
            .await
            .map_err(track_error(context))?
            .into_iter()
            .filter(|concern| !answered.contains(&concern.id))
            .map(|concern| crate::concern_routes::concern_out(concern, context))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    Ok(PlanOut {
        track: track.slug.clone(),
        revision: i64::from(plan.revision),
        state: plan.state.clone(),
        based_on: plan.based_on.map(i64::from),
        approach: plan.approach.clone(),
        units,
        alignments: alignments
            .into_iter()
            .map(|alignment| AlignmentOut {
                number: i64::from(alignment.number),
                title: alignment.title,
                state: alignment.state,
                decision: alignment.decision,
                reason: alignment.reason,
            })
            .collect(),
        needs_alignment,
        answers: answers.into_iter().map(answer_out).collect(),
        needs_answer,
        created_by: plan.created_by.to_string(),
        created_by_name: plan.created_by_name.clone(),
        via_channel: plan.via_channel.clone(),
        via_client: plan.via_client.clone(),
        created_at: crate::timestamps::public_timestamp(plan.created_at),
        updated_at: crate::timestamps::public_timestamp(plan.updated_at),
        submitted_at: plan.submitted_at.map(crate::timestamps::public_timestamp),
        review_case_id: plan.review_case_id.map(|id| id.to_string()),
        reviewed_by_name: plan.reviewed_by_name.clone(),
        review_reason: plan.review_reason.clone(),
        reviewed_at: plan.reviewed_at.map(crate::timestamps::public_timestamp),
        markdown_ref: markdown_ref(&project.slug, &track.slug, plan.revision),
    })
}

/// The revision a path names: a number, `draft` (the open revision) or
/// `current` (the approved plan, else the newest revision).
async fn named_revision(
    conn: &mut PgConnection,
    track: &Track,
    name: &str,
    context: &RequestContext,
) -> Result<PlanRevision, Failure> {
    let missing = || {
        domain(
            ErrorCode::NotFound,
            format!("the {} plan has no revision '{name}'", track.slug),
        )
    };
    let revision = match name {
        "draft" => plans::open(conn, track.id, false)
            .await
            .map_err(track_error(context))?
            .map(|plan| plan.revision),
        "current" => match plans::approved_revision(conn, track.id)
            .await
            .map_err(track_error(context))?
        {
            Some(revision) => Some(revision),
            None => plans::latest_revision(conn, track.id)
                .await
                .map_err(track_error(context))?,
        },
        number => Some(
            number
                .parse::<i32>()
                .ok()
                .filter(|number| *number > 0)
                .ok_or_else(|| {
                    invalid(
                        "path/revision",
                        "Input should be a positive integer, 'draft' or 'current'",
                    )
                })?,
        ),
    }
    .ok_or_else(missing)?;
    plans::get(conn, track.id, revision)
        .await
        .map_err(track_error(context))?
        .ok_or_else(missing)
}

fn json_response(status: StatusCode, value: &impl serde::Serialize) -> Response {
    (status, Json(value)).into_response()
}

// ------------------------------------------------------------- validation

/// Check one unit entry before it is written: its key, its brief and
/// context against the project's limits, its context references, and the
/// unit document it becomes. Returns the science revision it was
/// checked against.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "The checks run in the order a caller reads their errors"
)]
async fn check_entry(
    conn: &mut PgConnection,
    state: &RouteState,
    principal: &Principal,
    project: &projects::Project,
    track: &Track,
    key: &str,
    fields: &UnitFields,
    brief: &str,
    self_id: Option<UnitId>,
    limits: Limits,
    context: &RequestContext,
) -> Result<i32, Failure> {
    if !plan_units::valid_key(key) {
        return Err(invalid(
            "body/key",
            "a unit key is 1 to 63 lowercase letters, digits and hyphens, starting with a letter or digit",
        ));
    }
    if brief.len() > usize::try_from(limits.unit_brief_max_bytes).unwrap_or(usize::MAX) {
        return Err(too_large(
            "body/brief",
            "the unit brief",
            brief.len(),
            limits.unit_brief_max_bytes,
        ));
    }
    if fields.context.len() > usize::try_from(limits.context_items_max).unwrap_or(usize::MAX) {
        let message = format!(
            "the unit lists {} context items; the project's limit is {}",
            fields.context.len(),
            limits.context_items_max
        );
        return Err(Failure::new(ApiError::from(
            DomainError::new(ErrorCode::ValidationFailed, message.clone()).with_details(json!([{
                "path": "body/context", "message": message,
                "size": fields.context.len(), "limit": limits.context_items_max,
            }])),
        )));
    }
    for (index, relation) in fields.relations.iter().enumerate() {
        let path = format!("body/relations/{index}");
        if !matches!(
            relation.kind.as_str(),
            "derived_from" | "supersedes" | "related_to"
        ) {
            return Err(invalid(
                &format!("{path}/kind"),
                "Input should be 'derived_from', 'supersedes' or 'related_to'",
            ));
        }
        match &relation.unit {
            Value::String(unit) if plan_units::valid_key(unit) && unit != key => {}
            Value::String(_) => {
                return Err(invalid(
                    &format!("{path}/unit"),
                    "a relation by key names another unit of this plan",
                ));
            }
            Value::Number(_) | Value::Object(_) => {}
            _ => {
                return Err(invalid(
                    &format!("{path}/unit"),
                    "a relation names a unit number, {project, number}, or the key of another unit of this plan",
                ));
            }
        }
    }
    for (index, item) in fields.context.iter().enumerate() {
        check_context_item(
            conn,
            project,
            key,
            item,
            &format!("body/context/{index}"),
            context,
        )
        .await?;
    }
    let mut entry =
        serde_json::to_value(fields).map_err(|_| internal(context, "unit entry encoding"))?;
    entry["key"] = Value::from(key);
    entry["brief"] = Value::from(brief);
    let entry =
        plan_units::document(&entry).ok_or_else(|| invalid("body", "the unit is too deep"))?;
    unit_contract(&entry, state, context)?;
    let value = plan_units::unit_value(&track.slug, fields, &plan_units::unit_relations(fields));
    let document =
        plan_units::document(&value).ok_or_else(|| invalid("body", "the unit is too deep"))?;
    match check_unit(conn, principal, project, &document, self_id, state, context).await {
        Ok(checked) => Ok(checked.science_revision),
        Err(failure) => Err(unit_paths(failure).await),
    }
}

/// The document path an error names, as the unit's request field: `/plan`
/// is `acceptance` and `/project_fields` is `parameters`.
fn unit_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix('/')?;
    let (field, tail) = rest.split_once('/').unwrap_or((rest, ""));
    let field = match field {
        "plan" => "acceptance",
        "project_fields" => "parameters",
        "control" | "relations" | "title" | "question" | "intervention" => field,
        _ => return None,
    };
    Some(if tail.is_empty() {
        format!("body/{field}")
    } else {
        format!("body/{field}/{tail}")
    })
}

/// A refusal of the unit document a unit becomes, with its paths
/// renamed to the unit's request fields.
async fn unit_paths(failure: Failure) -> Failure {
    let response = *failure.0;
    if response.status() != StatusCode::UNPROCESSABLE_ENTITY {
        return Failure::new(response);
    }
    let (parts, body) = response.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, 1024 * 1024).await else {
        return Failure::new(parts.status);
    };
    let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
        return Failure::new(Response::from_parts(parts, axum::body::Body::from(bytes)));
    };
    if let Some(details) = value["error"]["details"].as_array_mut() {
        for detail in details {
            if let Some(path) = detail["path"].as_str().and_then(unit_path) {
                detail["path"] = Value::from(path);
            }
        }
    }
    let bytes = serde_json::to_vec(&value).unwrap_or_else(|_| bytes.to_vec());
    let mut parts = parts;
    parts.headers.remove(header::CONTENT_LENGTH);
    Failure::new(Response::from_parts(parts, axum::body::Body::from(bytes)))
}

async fn check_context_item(
    conn: &mut PgConnection,
    project: &projects::Project,
    key: &str,
    item: &ContextItem,
    path: &str,
    context: &RequestContext,
) -> Result<(), Failure> {
    if item.note.as_ref().is_some_and(|note| note.len() > 2000) {
        return Err(invalid(
            &format!("{path}/note"),
            "a note is at most 2000 bytes",
        ));
    }
    let number = |value: &Value| {
        value
            .as_i64()
            .and_then(|number| i32::try_from(number).ok())
            .filter(|number| *number > 0)
    };
    match item.kind.as_str() {
        "unit" => {
            if item.attempt.is_some() || item.artifact.is_some() {
                return Err(invalid(path, "a unit item names only `unit` and `note`"));
            }
            match &item.unit {
                Some(Value::String(unit)) if plan_units::valid_key(unit) && unit != key => Ok(()),
                Some(value) if number(value).is_some() => {
                    let found = plans::project_unit(conn, project.id, number(value).unwrap_or(0))
                        .await
                        .map_err(track_error(context))?;
                    found.map(|_| ()).ok_or_else(|| {
                        invalid(&format!("{path}/unit"), "no such unit in this project")
                    })
                }
                _ => Err(invalid(
                    &format!("{path}/unit"),
                    "name a unit number or another unit's key",
                )),
            }
        }
        "writeup" => {
            if item.artifact.is_some() {
                return Err(invalid(path, "a write-up item names `unit` and `attempt`"));
            }
            let unit = item.unit.as_ref().and_then(number);
            let attempt = item
                .attempt
                .and_then(|attempt| i32::try_from(attempt).ok())
                .filter(|attempt| *attempt > 0);
            let (Some(unit), Some(attempt)) = (unit, attempt) else {
                return Err(invalid(
                    path,
                    "a write-up item names a unit number (`unit`) and an attempt sequence",
                ));
            };
            plans::attempt_id(conn, project.id, unit, attempt)
                .await
                .map_err(track_error(context))?
                .map(|_| ())
                .ok_or_else(|| invalid(path, format!("attempt #{unit}.{attempt} not found")))
        }
        "artifact" => {
            if item.unit.is_some() || item.attempt.is_some() {
                return Err(invalid(
                    path,
                    "an artifact item names only `artifact` and `note`",
                ));
            }
            let id = item
                .artifact
                .as_deref()
                .and_then(|id| uuid::Uuid::parse_str(id).ok())
                .ok_or_else(|| invalid(&format!("{path}/artifact"), "Input should be a UUID"))?;
            plans::artifact(conn, project.id, id)
                .await
                .map_err(track_error(context))?
                .map(|_| ())
                .ok_or_else(|| {
                    invalid(
                        &format!("{path}/artifact"),
                        "no such artifact in this project",
                    )
                })
        }
        _ => Err(invalid(
            &format!("{path}/kind"),
            "Input should be 'unit', 'writeup' or 'artifact'",
        )),
    }
}

/// What blocks submitting a draft.
#[allow(
    clippy::too_many_lines,
    reason = "Every check a submission must pass, in the order check_plan reports them"
)]
pub(crate) async fn problems(
    conn: &mut PgConnection,
    project: &projects::Project,
    track: &Track,
    plan: &PlanRevision,
    context: &RequestContext,
) -> Result<Vec<PlanProblem>, Failure> {
    let limits = plans::limits(conn, project.id)
        .await
        .map_err(track_error(context))?;
    let mut problems = Vec::new();
    let mut problem = |code: &str, path: String, message: String| {
        problems.push(PlanProblem {
            code: code.to_owned(),
            path,
            message,
        });
    };
    if briefs::current_revision(conn, project.id)
        .await
        .map_err(project_failure(context))?
        == 0
    {
        problem(
            "no_brief",
            "brief".into(),
            "the project has no brief yet; a researcher writes it before the first plan is approved".into(),
        );
    }
    if plan.approach.trim().is_empty() {
        problem(
            "empty_approach",
            "approach".into(),
            "the plan has no approach yet".into(),
        );
    }
    if plan.approach.len() > usize::try_from(limits.plan_approach_max_bytes).unwrap_or(usize::MAX) {
        problem(
            "limit_exceeded",
            "approach".into(),
            format!(
                "the approach is {} bytes; the project's limit is {} bytes",
                plan.approach.len(),
                limits.plan_approach_max_bytes
            ),
        );
    }
    let entries = plans::units(conn, plan.id)
        .await
        .map_err(track_error(context))?;
    if entries.is_empty()
        && plans::approved_revision(conn, track.id)
            .await
            .map_err(track_error(context))?
            .is_none()
    {
        problem(
            "no_units",
            "units".into(),
            "the first plan of a track lists at least one unit".into(),
        );
    }
    let mut keys: BTreeSet<_> = entries.iter().map(|entry| entry.key.clone()).collect();
    keys.extend(
        plans::known_keys(conn, track.id)
            .await
            .map_err(track_error(context))?
            .into_iter()
            .map(|(key, _, _)| key),
    );
    for entry in &entries {
        let fields: UnitFields = serde_json::from_str(&entry.fields)
            .map_err(|_| internal(context, "plan unit fields"))?;
        let at = format!("units/{}", entry.key);
        if entry.brief.len() > usize::try_from(limits.unit_brief_max_bytes).unwrap_or(usize::MAX) {
            problem(
                "limit_exceeded",
                format!("{at}/brief"),
                format!(
                    "the unit brief is {} bytes; the project's limit is {} bytes",
                    entry.brief.len(),
                    limits.unit_brief_max_bytes
                ),
            );
        }
        if fields.context.len() > usize::try_from(limits.context_items_max).unwrap_or(usize::MAX) {
            problem(
                "limit_exceeded",
                format!("{at}/context"),
                format!(
                    "the unit lists {} context items; the project's limit is {}",
                    fields.context.len(),
                    limits.context_items_max
                ),
            );
        }
        for (index, relation) in fields.relations.iter().enumerate() {
            if let Some(unit) = relation.key()
                && !keys.contains(unit)
            {
                problem(
                    "unknown_unit",
                    format!("{at}/relations/{index}"),
                    format!("no unit '{unit}' in this plan"),
                );
            }
        }
        for (index, item) in fields.context.iter().enumerate() {
            if let Some(Value::String(unit)) = &item.unit
                && !keys.contains(unit)
            {
                problem(
                    "unknown_unit",
                    format!("{at}/context/{index}"),
                    format!("no unit '{unit}' in this plan"),
                );
            }
        }
        if let (Some(number), Some(state)) = (entry.number, &entry.state)
            && state != "queued"
        {
            problem(
                "unit_not_queued",
                at.clone(),
                format!(
                    "#{number} is {state} now; drop its entry and add an alignment entry for it"
                ),
            );
        }
    }
    let alignments = plans::alignments(conn, plan.id)
        .await
        .map_err(track_error(context))?;
    let aligned: BTreeMap<_, _> = alignments
        .iter()
        .map(|alignment| (alignment.unit_id, alignment.decision.as_str()))
        .collect();
    for unit in plans::needing_alignment(conn, track.id)
        .await
        .map_err(track_error(context))?
    {
        if !aligned.contains_key(&unit.unit_id) {
            problem(
                "missing_alignment",
                format!("alignments/{}", unit.number),
                format!(
                    "#{} ({}) is {}; say whether the plan keeps it, makes it obsolete or redoes it",
                    unit.number, unit.title, unit.state
                ),
            );
        }
    }
    for alignment in &alignments {
        if alignment.decision == "redo"
            && !entries
                .iter()
                .any(|entry| entry.redo_of == Some(alignment.unit_id))
        {
            problem(
                "redo_without_unit",
                format!("alignments/{}", alignment.number),
                format!(
                    "#{} is to be redone but the plan lists no unit redoing it",
                    alignment.number
                ),
            );
        }
    }
    let answers = concerns::answers(conn, plan.id)
        .await
        .map_err(track_error(context))?;
    let answered: BTreeSet<_> = answers.iter().map(|answer| answer.concern_id).collect();
    for concern in concerns::open_for_track(conn, project.id, track.id)
        .await
        .map_err(track_error(context))?
    {
        if !answered.contains(&concern.id) {
            let from = concern.unit_number.map_or_else(String::new, |number| {
                concern.attempt_sequence.map_or_else(
                    || format!(" from #{number}"),
                    |sequence| format!(" from #{number}.{sequence}"),
                )
            });
            problem(
                "unanswered_concern",
                format!("answers/{}", concern.id),
                format!(
                    "a concern ({}){from} is open; say how this revision answers it (answer_concern), or a researcher dismisses it",
                    concern.kind.replace('_', " ")
                ),
            );
        }
    }
    for answer in &answers {
        if answer.state != "open" {
            problem(
                "concern_closed",
                format!("answers/{}", answer.concern_id),
                format!(
                    "the concern is {} now; remove this revision's answer to it",
                    answer.state
                ),
            );
        }
    }
    Ok(problems)
}

// ------------------------------------------------------------------ reads

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/tracks/{track_slug}/plans",
    operation_id = "list_plan_revisions_api_projects__slug__tracks__track_slug__plans_get",
    summary = "List Plan Revisions",
    description = "The revisions of a track's plan, newest first, without their units.",
    params(("slug" = String, Path), ("track_slug" = String, Path),
        ("before" = Option<i64>, Query, description = "Continue before this revision.", minimum = 1, maximum = 2_147_483_647),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_PlanRevisionOut_int_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn list(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let (mut auth, project, paths) = reader(&state, &context, &mut parts).await?;
    let (before, limit) = page(&parts)?;
    let track = load_track(
        &mut auth.connection,
        &state,
        &context,
        &project,
        &paths["track_slug"],
        None,
    )
    .await?;
    let mut rows = plans::list(&mut auth.connection, track.id, before, limit + 1)
        .await
        .map_err(track_error(&context))?;
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let next_before = if rows.len() > limit {
        rows.truncate(limit);
        rows.last().map(|row| i64::from(row.revision))
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(|row| PlanRevisionOut {
            revision: i64::from(row.revision),
            state: row.state,
            based_on: row.based_on.map(i64::from),
            units: row.units,
            created_by_name: row.created_by_name,
            created_at: crate::timestamps::public_timestamp(row.created_at),
            submitted_at: row.submitted_at.map(crate::timestamps::public_timestamp),
            reviewed_by_name: row.reviewed_by_name,
            review_reason: row.review_reason,
            reviewed_at: row.reviewed_at.map(crate::timestamps::public_timestamp),
        })
        .collect();
    Ok(json_response(
        StatusCode::OK,
        &Page_PlanRevisionOut_int_ { items, next_before },
    ))
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/tracks/{track_slug}/plans/{revision}",
    operation_id = "get_plan_api_projects__slug__tracks__track_slug__plans__revision__get",
    summary = "Get Plan",
    description = "One revision of a track's plan with its units and alignment entries.\n`revision` is a number, `draft` (the open revision) or `current` (the\napproved plan, else the newest revision).",
    params(("slug" = String, Path), ("track_slug" = String, Path), ("revision" = String, Path)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::PlanOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn read(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let (mut auth, project, paths) = reader(&state, &context, &mut parts).await?;
    let track = load_track(
        &mut auth.connection,
        &state,
        &context,
        &project,
        &paths["track_slug"],
        None,
    )
    .await?;
    let plan = named_revision(&mut auth.connection, &track, &paths["revision"], &context).await?;
    let out = plan_out(&mut auth.connection, &project, &track, &plan, &context).await?;
    Ok(json_response(StatusCode::OK, &out))
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/tracks/{track_slug}/plans/{revision}/plan.md",
    operation_id = "get_plan_markdown_api_projects__slug__tracks__track_slug__plans__revision__plan_md_get",
    summary = "Get Plan Markdown",
    description = "A read-only Markdown view of one plan revision: YAML front matter and a\nbody with the approach, the units and the alignment entries.",
    params(("slug" = String, Path), ("track_slug" = String, Path), ("revision" = String, Path)),
    responses((status = 200, description = "Successful Response", body = String, content_type = "text/markdown"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn markdown(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let (mut auth, project, paths) = reader(&state, &context, &mut parts).await?;
    let track = load_track(
        &mut auth.connection,
        &state,
        &context,
        &project,
        &paths["track_slug"],
        None,
    )
    .await?;
    let plan = named_revision(&mut auth.connection, &track, &paths["revision"], &context).await?;
    let out = plan_out(&mut auth.connection, &project, &track, &plan, &context).await?;
    Ok((
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        render_markdown(&project.slug, &track, &out),
    )
        .into_response())
}

fn yaml(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| String::from("null"))
}

/// A plan revision as one Markdown document with YAML front matter.
pub(crate) fn render_markdown(slug: &str, track: &Track, plan: &PlanOut) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("---\n");
    let _ = writeln!(out, "project: {}", yaml(&slug));
    let _ = writeln!(out, "track: {}", yaml(&plan.track));
    let _ = writeln!(out, "revision: {}", plan.revision);
    let _ = writeln!(out, "state: {}", yaml(&plan.state));
    let _ = writeln!(out, "based_on: {}", yaml(&plan.based_on));
    let _ = writeln!(out, "created_by: {}", yaml(&plan.created_by_name));
    let _ = writeln!(out, "created_at: {}", yaml(&plan.created_at));
    let _ = writeln!(out, "submitted_at: {}", yaml(&plan.submitted_at));
    let _ = writeln!(out, "reviewed_by: {}", yaml(&plan.reviewed_by_name));
    let _ = writeln!(out, "reviewed_at: {}", yaml(&plan.reviewed_at));
    let _ = writeln!(out, "units: {}", plan.units.len());
    out.push_str("---\n\n");
    let _ = writeln!(
        out,
        "# Plan for {}, revision {}\n\n## Approach\n\n{}\n",
        track.title,
        plan.revision,
        plan.approach.trim()
    );
    out.push_str("## Units\n");
    for unit in &plan.units {
        let number = unit
            .number
            .map_or_else(|| String::from("new"), |number| format!("#{number}"));
        let state = unit.state.as_deref().unwrap_or("planned");
        let _ = writeln!(
            out,
            "\n### {} `{}`: {} ({state})\n",
            number, unit.key, unit.title
        );
        if let Some(redo_of) = unit.redo_of {
            let _ = writeln!(out, "Redoes #{redo_of}.\n");
        }
        let _ = writeln!(out, "- Question: {}", unit.question);
        let _ = writeln!(out, "- Intervention: {}", unit.intervention);
        if let Some(control) = &unit.control {
            let _ = writeln!(out, "- Control: {}", yaml(control));
        }
        for relation in &unit.relations {
            let target = relation
                .key()
                .map_or_else(|| yaml(&relation.unit), |unit| format!("unit `{unit}`"));
            let _ = writeln!(out, "- {}: {target}", relation.kind.replace('_', " "));
        }
        let _ = writeln!(
            out,
            "\nAcceptance:\n\n```json\n{}\n```",
            serde_json::to_string_pretty(&unit.acceptance).unwrap_or_default()
        );
        if let Some(parameters) = &unit.parameters {
            let _ = writeln!(
                out,
                "\nParameters:\n\n```json\n{}\n```",
                serde_json::to_string_pretty(parameters).unwrap_or_default()
            );
        }
        if !unit.context.is_empty() {
            out.push_str("\nContext:\n\n");
            for item in &unit.context {
                let _ = writeln!(out, "- {}", context_label(item));
            }
        }
        if !unit.brief.trim().is_empty() {
            let _ = writeln!(out, "\n{}", unit.brief.trim());
        }
    }
    if !plan.alignments.is_empty() {
        out.push_str("\n## Alignment\n\n");
        for alignment in &plan.alignments {
            let _ = writeln!(
                out,
                "- #{} {} ({}): {}. {}",
                alignment.number,
                alignment.title,
                alignment.state,
                alignment.decision,
                alignment.reason
            );
        }
    }
    if !plan.answers.is_empty() {
        out.push_str("\n## Answers\n\n");
        for answer in &plan.answers {
            let _ = writeln!(
                out,
                "- Concern {} ({}): {}",
                answer.concern,
                answer.kind.replace('_', " "),
                answer.how.trim()
            );
        }
    }
    if let Some(reason) = &plan.review_reason {
        let _ = writeln!(out, "\n## Review\n\n{}: {reason}", plan.state);
    }
    out
}

pub(crate) fn context_label(item: &ContextItem) -> String {
    let unit = item
        .unit
        .as_ref()
        .map_or_else(String::new, |unit| match unit {
            Value::String(key) => format!("unit `{key}`"),
            other => format!("#{other}"),
        });
    let label = match item.kind.as_str() {
        "writeup" => format!("write-up of {unit}.{}", item.attempt.unwrap_or_default()),
        "artifact" => format!("artifact {}", item.artifact.as_deref().unwrap_or_default()),
        _ => unit,
    };
    item.note
        .as_ref()
        .map_or_else(|| label.clone(), |note| format!("{label}: {note}"))
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/tracks/{track_slug}/units",
    operation_id = "list_track_units_api_projects__slug__tracks__track_slug__units_get",
    summary = "List Track Units",
    description = "The units of a track, newest first: number, plan key, title,\nstate and whether an approved plan made them obsolete.",
    params(("slug" = String, Path), ("track_slug" = String, Path),
        ("state" = Option<String>, Query, description = "Only units in this unit state."),
        ("before" = Option<i64>, Query, description = "Continue before this number.", minimum = 1, maximum = 2_147_483_647),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_UnitIndexOut_int_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn list_track_units(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let (mut auth, project, paths) = reader(&state, &context, &mut parts).await?;
    let (before, limit) = page(&parts)?;
    let states = query_value(&parts, "state")
        .map(|state| {
            const STATES: [&str; 9] = [
                "queued",
                "active",
                "documenting",
                "deciding",
                "promoted",
                "rejected",
                "inconclusive",
                "failed",
                "cancelled",
            ];
            STATES
                .contains(&state)
                .then(|| vec![state.to_owned()])
                .ok_or_else(|| invalid("query/state", "Input should be a unit state"))
        })
        .transpose()?;
    let track = load_track(
        &mut auth.connection,
        &state,
        &context,
        &project,
        &paths["track_slug"],
        None,
    )
    .await?;
    let mut rows = plans::track_units(
        &mut auth.connection,
        track.id,
        states.as_deref(),
        before,
        limit + 1,
    )
    .await
    .map_err(track_error(&context))?;
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let next_before = if rows.len() > limit {
        rows.truncate(limit);
        rows.last().map(|row| i64::from(row.number))
    } else {
        None
    };
    Ok(json_response(
        StatusCode::OK,
        &Page_UnitIndexOut_int_ {
            items: rows.iter().map(index_out).collect(),
            next_before,
        },
    ))
}

/// A unit by number, with the plan fields it was last approved with.
pub(crate) async fn unit_out(
    conn: &mut PgConnection,
    project: &projects::Project,
    number: i32,
    context: &RequestContext,
) -> Result<UnitPlanOut, Failure> {
    let (track_id, unit) = plans::project_unit(conn, project.id, number)
        .await
        .map_err(track_error(context))?
        .ok_or_else(|| domain(ErrorCode::NotFound, format!("unit #{number} not found")))?;
    let revision = unit.approved_revision.unwrap_or(unit.revision);
    let stored = plans::unit_revision(conn, unit.unit_id, revision)
        .await
        .map_err(track_error(context))?
        .ok_or_else(|| internal(context, "unit revision"))?;
    let document: Value =
        serde_json::from_str(&stored.content).map_err(|_| internal(context, "unit content"))?;
    let entry = plans::latest_entry(conn, unit.unit_id)
        .await
        .map_err(track_error(context))?;
    let mut fields = plan_units::fields_from_unit(&document);
    let mut plan_revision = None;
    if let Some((revision, entry)) = &entry {
        let planned: UnitFields = serde_json::from_str(&entry.fields)
            .map_err(|_| internal(context, "unit plan fields"))?;
        // Relations come from the unit document, where approval
        // resolved unit keys to numbers; context only the plan holds.
        fields.context = planned.context;
        plan_revision = Some(i64::from(*revision));
    }
    let track = tracks::get_track_by_id(
        conn,
        track_id,
        cannery_tracks::repo::JsonContext {
            encode_nesting_budget: crate::body::REST_JSON_NESTING_BUDGET,
            decode_nesting_budget: crate::body::REST_JSON_NESTING_BUDGET,
        },
    )
    .await
    .map_err(track_error(context))?
    .ok_or_else(|| internal(context, "unit track"))?;
    Ok(UnitPlanOut {
        number: i64::from(unit.number),
        track: track.slug,
        key: unit.key,
        state: unit.state,
        obsolete: unit.obsolete,
        revision: i64::from(unit.revision),
        approved_revision: unit.approved_revision.map(i64::from),
        plan_revision,
        title: fields.title,
        question: fields.question,
        intervention: fields.intervention,
        control: fields.control,
        acceptance: fields.acceptance,
        parameters: fields.parameters,
        relations: fields.relations,
        context: fields.context,
        brief: stored.brief.unwrap_or_default(),
    })
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/units/{number}/plan",
    operation_id = "get_unit_plan_api_projects__slug__units__number__plan_get",
    summary = "Get Unit Plan",
    description = "One unit: its unit with the plan fields, context and brief it was\nlast approved with.",
    params(("slug" = String, Path), ("number" = i64, Path)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::UnitPlanOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn unit_plan(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let (mut auth, project, paths) = reader(&state, &context, &mut parts).await?;
    let number = positive(&paths, "number")?;
    let out = unit_out(&mut auth.connection, &project, number, &context).await?;
    Ok(json_response(StatusCode::OK, &out))
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/units/{number}/history",
    operation_id = "get_unit_history_api_projects__slug__units__number__history_get",
    summary = "Get Unit History",
    description = "A unit's revisions, with the plan revision that wrote each one,\nand the alignment entries approved plans made about it, oldest first.",
    params(("slug" = String, Path), ("number" = i64, Path)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::UnitHistoryOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn unit_history(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let (mut auth, project, paths) = reader(&state, &context, &mut parts).await?;
    let number = positive(&paths, "number")?;
    let (_, unit) = plans::project_unit(&mut auth.connection, project.id, number)
        .await
        .map_err(track_error(&context))?
        .ok_or_else(|| domain(ErrorCode::NotFound, format!("unit #{number} not found")))?;
    let revisions = plans::unit_revisions(&mut auth.connection, unit.unit_id)
        .await
        .map_err(track_error(&context))?
        .into_iter()
        .map(|revision| {
            let document: Value = serde_json::from_str(&revision.content).unwrap_or(Value::Null);
            UnitRevisionOut {
                revision: i64::from(revision.revision),
                plan_revision: revision.plan_revision.map(i64::from),
                title: document
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                brief: revision.brief,
                created_at: crate::timestamps::public_timestamp(revision.created_at),
            }
        })
        .collect();
    let alignments = plans::unit_alignments(&mut auth.connection, unit.unit_id)
        .await
        .map_err(track_error(&context))?
        .into_iter()
        .map(|(revision, decision, reason)| UnitAlignmentOut {
            plan_revision: i64::from(revision),
            decision,
            reason,
        })
        .collect();
    Ok(json_response(
        StatusCode::OK,
        &UnitHistoryOut {
            number: i64::from(number),
            revisions,
            alignments,
        },
    ))
}

fn limits_out(limits: Limits) -> ProjectLimits {
    ProjectLimits {
        brief_max_bytes: i64::from(limits.brief_max_bytes),
        plan_approach_max_bytes: i64::from(limits.plan_approach_max_bytes),
        unit_brief_max_bytes: i64::from(limits.unit_brief_max_bytes),
        context_items_max: i64::from(limits.context_items_max),
        index_line_max_bytes: i64::from(limits.index_line_max_bytes),
        context_summary_max_bytes: i64::from(limits.context_summary_max_bytes),
    }
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/limits",
    operation_id = "get_limits_api_projects__slug__limits_get",
    summary = "Get Limits",
    description = "The project's size limits for briefs, plans, units and context bundles.",
    params(("slug" = String, Path)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::ProjectLimits, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn limits(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let (mut auth, project, _) = reader(&state, &context, &mut parts).await?;
    let limits = plans::limits(&mut auth.connection, project.id)
        .await
        .map_err(track_error(&context))?;
    Ok(json_response(StatusCode::OK, &limits_out(limits)))
}

#[utoipa::path(
    put,
    path = "/api/projects/{slug}/limits",
    operation_id = "set_limits_api_projects__slug__limits_put",
    summary = "Set Limits",
    description = "Replace the project's size limits. Researchers only. Writes over a limit\nare refused where they are made; existing content is not changed.",
    params(("slug" = String, Path)),
    request_body(content = crate::api_models::ProjectLimits, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = crate::api_models::ProjectLimits, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn set_limits(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let (mut auth, project, _) = researcher(&state, &context, &mut parts).await?;
    let input: ProjectLimits = decode(&body)?;
    let field = |name: &str, value: i64, low: i64, high: i64| {
        if (low..=high).contains(&value) {
            i32::try_from(value).map_err(|_| internal(&context, "limit value"))
        } else {
            Err(invalid(
                &format!("body/{name}"),
                format!("Input should be between {low} and {high}"),
            ))
        }
    };
    let limits = Limits {
        brief_max_bytes: field("brief_max_bytes", input.brief_max_bytes, 1024, 262_144)?,
        plan_approach_max_bytes: field(
            "plan_approach_max_bytes",
            input.plan_approach_max_bytes,
            1024,
            262_144,
        )?,
        unit_brief_max_bytes: field(
            "unit_brief_max_bytes",
            input.unit_brief_max_bytes,
            1024,
            131_072,
        )?,
        context_items_max: field("context_items_max", input.context_items_max, 1, 256)?,
        index_line_max_bytes: field("index_line_max_bytes", input.index_line_max_bytes, 40, 1000)?,
        context_summary_max_bytes: field(
            "context_summary_max_bytes",
            input.context_summary_max_bytes,
            80,
            2000,
        )?,
    };
    let Principal::User(user) = &auth.principal else {
        return Err(internal(&context, "limits author"));
    };
    let user_id = user.user_id;
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "limits transaction"))?;
    let prior = plans::limits(&mut tx, project.id)
        .await
        .map_err(track_error(&context))?;
    plans::set_limits(&mut tx, project.id, limits, user_id)
        .await
        .map_err(track_error(&context))?;
    let subject = project.id.to_string();
    let prior = serde_json::to_value(limits_out(prior)).unwrap_or(Value::Null);
    let next = serde_json::to_value(limits_out(limits)).unwrap_or(Value::Null);
    audit::record(
        &mut tx,
        Attribution::Principal(&auth.principal),
        Record {
            action: "project.limits_changed",
            subject_type: "project",
            subject_id: &subject,
            project_id: Some(project.id),
            prior_state: Some(&prior),
            new_state: Some(&next),
            reason: None,
            idempotency_key: None,
        },
    )
    .await
    .map_err(|_| internal(&context, "limits audit"))?;
    commit(tx, &context).await?;
    Ok(json_response(StatusCode::OK, &limits_out(limits)))
}

// ----------------------------------------------------------------- writes

/// A researcher's draft write: the caller, project, locked track and its
/// open draft, inside one transaction.
macro_rules! draft_write {
    ($state:ident, $context:ident, $parts:ident, $auth:ident, $project:ident, $paths:ident, $tx:ident, $track:ident, $plan:ident) => {
        let (mut $auth, $project, $paths) = researcher(&$state, &$context, &mut $parts).await?;
        let mut $tx = $auth
            .connection
            .begin()
            .await
            .map_err(|_| internal(&$context, "plan transaction"))?;
        let $track = load_track(
            &mut $tx,
            &$state,
            &$context,
            &$project,
            &$paths["track_slug"],
            Some(RowLock::Update),
        )
        .await?;
        let $plan = draft(&mut $tx, &$track, &$context).await?;
    };
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/tracks/{track_slug}/plans",
    operation_id = "start_plan_revision_api_projects__slug__tracks__track_slug__plans_post",
    summary = "Start Plan Revision",
    description = "Open the next revision of a track's plan as a draft. A revision starts from\nthe approved plan (or from a newer revision sent back): its approach, its\nnew units and the units still queued. `needs_alignment` lists the done and\nin-flight units every revision says something about, and `needs_answer`\nthe open concerns it answers. Researchers only; one open revision per track.",
    params(("slug" = String, Path), ("track_slug" = String, Path)),
    responses((status = 201, description = "Successful Response", body = crate::api_models::PlanOut, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
#[allow(
    clippy::too_many_lines,
    reason = "A revision starts from the approved or sent-back one, with its units and alignments, in one transaction"
)]
pub(crate) async fn start(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let (mut auth, project, paths) = researcher(&state, &context, &mut parts).await?;
    let Principal::User(user) = &auth.principal else {
        return Err(internal(&context, "plan author"));
    };
    let (user_id, via_channel, via_client) = (
        user.user_id,
        channel_name(user.via.channel),
        user.via.client.clone(),
    );
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "plan transaction"))?;
    let track = load_track(
        &mut tx,
        &state,
        &context,
        &project,
        &paths["track_slug"],
        Some(RowLock::Update),
    )
    .await?;
    if track.state == TrackState::Archived {
        return Err(domain(
            ErrorCode::Conflict,
            format!("track '{}' is archived and takes no plan", track.slug),
        ));
    }
    if let Some(open) = plans::open(&mut tx, track.id, true)
        .await
        .map_err(track_error(&context))?
    {
        return Err(domain(
            ErrorCode::Conflict,
            format!(
                "revision {} of the {} plan is already open ({})",
                open.revision, track.slug, open.state
            ),
        ));
    }
    let approved = plans::approved_revision(&mut tx, track.id)
        .await
        .map_err(track_error(&context))?;
    let latest = match plans::latest_revision(&mut tx, track.id)
        .await
        .map_err(track_error(&context))?
    {
        Some(revision) => plans::get(&mut tx, track.id, revision)
            .await
            .map_err(track_error(&context))?,
        None => None,
    };
    let base = match latest {
        Some(latest) if latest.state == "sent_back" && Some(latest.revision) > approved => {
            Some(latest)
        }
        _ => match approved {
            Some(revision) => plans::get(&mut tx, track.id, revision)
                .await
                .map_err(track_error(&context))?,
            None => None,
        },
    };
    let revision = plans::create(
        &mut tx,
        plans::NewPlan {
            project_id: project.id,
            track_id: track.id,
            based_on: base.as_ref().map(|base| base.revision),
            approach: base.as_ref().map_or("", |base| base.approach.as_str()),
            created_by: user_id,
            via_channel,
            via_client: via_client.as_deref(),
        },
    )
    .await
    .map_err(track_error(&context))?;
    let plan = plans::get(&mut tx, track.id, revision)
        .await
        .map_err(track_error(&context))?
        .ok_or_else(|| internal(&context, "created plan"))?;
    if let Some(base) = &base {
        plans::copy_units(&mut tx, base.id, plan.id)
            .await
            .map_err(track_error(&context))?;
        if base.state == "sent_back" {
            concerns::copy_answers(&mut tx, base.id, plan.id)
                .await
                .map_err(track_error(&context))?;
            for alignment in plans::alignments(&mut tx, base.id)
                .await
                .map_err(track_error(&context))?
            {
                plans::set_alignment(
                    &mut tx,
                    plan.id,
                    alignment.unit_id,
                    &alignment.decision,
                    &alignment.reason,
                )
                .await
                .map_err(track_error(&context))?;
            }
        }
    }
    let new_state = json!({
        "track": track.slug, "revision": revision,
        "based_on": base.as_ref().map(|base| base.revision),
    });
    record(
        &mut tx,
        &auth.principal,
        &project,
        &plan,
        "plan.started",
        &new_state,
        None,
        &context,
    )
    .await?;
    let out = plan_out(&mut tx, &project, &track, &plan, &context).await?;
    commit(tx, &context).await?;
    Ok(json_response(StatusCode::CREATED, &out))
}

#[utoipa::path(
    put,
    path = "/api/projects/{slug}/tracks/{track_slug}/plans/draft/approach",
    operation_id = "set_plan_approach_api_projects__slug__tracks__track_slug__plans_draft_approach_put",
    summary = "Set Plan Approach",
    description = "Replace the draft's approach: the track's shared reasoning, edge cases and\nrisks, as Markdown. Refused over the project's approach limit.",
    params(("slug" = String, Path), ("track_slug" = String, Path)),
    request_body(content = crate::api_models::PlanApproach, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = crate::api_models::PlanOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn approach(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let input: PlanApproach = decode(&body)?;
    draft_write!(state, context, parts, auth, project, paths, tx, track, plan);
    let limits = plans::limits(&mut tx, project.id)
        .await
        .map_err(track_error(&context))?;
    if input.approach.len() > usize::try_from(limits.plan_approach_max_bytes).unwrap_or(usize::MAX)
    {
        return Err(too_large(
            "body/approach",
            "the approach",
            input.approach.len(),
            limits.plan_approach_max_bytes,
        ));
    }
    plans::set_approach(&mut tx, plan.id, &input.approach)
        .await
        .map_err(track_error(&context))?;
    let plan = plans::get(&mut tx, track.id, plan.revision)
        .await
        .map_err(track_error(&context))?
        .ok_or_else(|| internal(&context, "plan reload"))?;
    let out = plan_out(&mut tx, &project, &track, &plan, &context).await?;
    commit(tx, &context).await?;
    Ok(json_response(StatusCode::OK, &out))
}

fn entry_text(fields: &UnitFields, context: &RequestContext) -> Result<String, Failure> {
    serde_json::to_string(fields).map_err(|_| internal(context, "unit fields encoding"))
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/tracks/{track_slug}/plans/draft/units",
    operation_id = "add_unit_api_projects__slug__tracks__track_slug__plans_draft_units_post",
    summary = "Add Unit",
    description = "Add a unit to the draft. `acceptance` is checked against the project's\nmetric registry and `parameters` against its unit fields, as for a\nunit; `context` items must exist; the brief and the number of context\nitems are refused over the project's limits.",
    params(("slug" = String, Path), ("track_slug" = String, Path)),
    request_body(content = crate::api_models::UnitCreate, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::PlanUnitOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn add_unit(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let input: UnitCreate = decode(&body)?;
    draft_write!(state, context, parts, auth, project, paths, tx, track, plan);
    if plans::unit(&mut tx, plan.id, &input.key)
        .await
        .map_err(track_error(&context))?
        .is_some()
    {
        return Err(domain(
            ErrorCode::Conflict,
            format!("the draft already has a unit '{}'", input.key),
        ));
    }
    let fields = UnitFields {
        title: input.title,
        question: input.question,
        intervention: input.intervention,
        control: input.control,
        acceptance: input.acceptance,
        parameters: input.parameters,
        relations: input.relations,
        context: input.context,
    };
    let limits = plans::limits(&mut tx, project.id)
        .await
        .map_err(track_error(&context))?;
    let science_revision = check_entry(
        &mut tx,
        &state,
        &auth.principal,
        &project,
        &track,
        &input.key,
        &fields,
        &input.brief,
        None,
        limits,
        &context,
    )
    .await?;
    let text = entry_text(&fields, &context)?;
    plans::insert_unit(
        &mut tx,
        plan.id,
        plans::UnitEntry {
            key: &input.key,
            unit_id: None,
            redo_of: None,
            fields: &text,
            brief: &input.brief,
            science_revision,
        },
    )
    .await
    .map_err(track_error(&context))?;
    plans::touch(&mut tx, plan.id)
        .await
        .map_err(track_error(&context))?;
    let entry = plans::unit(&mut tx, plan.id, &input.key)
        .await
        .map_err(track_error(&context))?
        .ok_or_else(|| internal(&context, "added unit"))?;
    let out = plan_units::entry_out(&entry).ok_or_else(|| internal(&context, "unit output"))?;
    commit(tx, &context).await?;
    Ok(json_response(StatusCode::CREATED, &out))
}

/// The draft's entry with this key, if it can still change.
async fn changeable_entry(
    conn: &mut PgConnection,
    plan: &PlanRevision,
    key: &str,
    context: &RequestContext,
) -> Result<plans::PlanUnit, Failure> {
    let entry = plans::unit(conn, plan.id, key)
        .await
        .map_err(track_error(context))?
        .ok_or_else(|| {
            domain(
                ErrorCode::NotFound,
                format!("the draft has no unit '{key}'"),
            )
        })?;
    if let (Some(number), Some(state)) = (entry.number, &entry.state)
        && state != "queued"
    {
        return Err(domain(
            ErrorCode::Conflict,
            format!("#{number} is {state}; only queued units change, use an alignment entry"),
        ));
    }
    Ok(entry)
}

#[utoipa::path(
    put,
    path = "/api/projects/{slug}/tracks/{track_slug}/plans/draft/units/{key}",
    operation_id = "update_unit_api_projects__slug__tracks__track_slug__plans_draft_units__key__put",
    summary = "Update Unit",
    description = "Change a new or queued unit of the draft. Omitted fields keep their value;\nthe result is checked as `add_unit` checks a new unit.",
    params(("slug" = String, Path), ("track_slug" = String, Path), ("key" = String, Path)),
    request_body(content = crate::api_models::UnitUpdate, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = crate::api_models::PlanUnitOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn update_unit(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let input: UnitUpdate = decode(&body)?;
    draft_write!(state, context, parts, auth, project, paths, tx, track, plan);
    let key = paths["key"].clone();
    let entry = changeable_entry(&mut tx, &plan, &key, &context).await?;
    let mut fields: UnitFields =
        serde_json::from_str(&entry.fields).map_err(|_| internal(&context, "unit fields"))?;
    let object = |value: Value, path: &str| -> Result<_, Failure> {
        match value {
            Value::Null => Ok(None),
            Value::Object(values) => Ok(Some(values.into_iter().collect())),
            _ => Err(invalid(path, "Input should be an object or null")),
        }
    };
    if let Some(title) = input.title {
        fields.title = title;
    }
    if let Some(question) = input.question {
        fields.question = question;
    }
    if let Some(intervention) = input.intervention {
        fields.intervention = intervention;
    }
    if let Some(control) = input.control {
        fields.control = object(control, "body/control")?;
    }
    if let Some(acceptance) = input.acceptance {
        fields.acceptance = acceptance;
    }
    if let Some(parameters) = input.parameters {
        fields.parameters = object(parameters, "body/parameters")?;
    }
    if let Some(relations) = input.relations {
        fields.relations = relations;
    }
    if let Some(items) = input.context {
        fields.context = items;
    }
    let brief = input.brief.unwrap_or(entry.brief);
    let limits = plans::limits(&mut tx, project.id)
        .await
        .map_err(track_error(&context))?;
    let science_revision = check_entry(
        &mut tx,
        &state,
        &auth.principal,
        &project,
        &track,
        &key,
        &fields,
        &brief,
        entry.unit_id,
        limits,
        &context,
    )
    .await?;
    let text = entry_text(&fields, &context)?;
    plans::update_unit(
        &mut tx,
        plan.id,
        plans::UnitEntry {
            key: &key,
            unit_id: entry.unit_id,
            redo_of: entry.redo_of,
            fields: &text,
            brief: &brief,
            science_revision,
        },
    )
    .await
    .map_err(track_error(&context))?;
    plans::touch(&mut tx, plan.id)
        .await
        .map_err(track_error(&context))?;
    let entry = plans::unit(&mut tx, plan.id, &key)
        .await
        .map_err(track_error(&context))?
        .ok_or_else(|| internal(&context, "updated unit"))?;
    let out = plan_units::entry_out(&entry).ok_or_else(|| internal(&context, "unit output"))?;
    commit(tx, &context).await?;
    Ok(json_response(StatusCode::OK, &out))
}

#[utoipa::path(
    delete,
    path = "/api/projects/{slug}/tracks/{track_slug}/plans/draft/units/{key}",
    operation_id = "drop_unit_api_projects__slug__tracks__track_slug__plans_draft_units__key__delete",
    summary = "Drop Unit",
    description = "Drop a new or queued unit from the draft and return the draft. A queued\nunit the approved plan listed is cancelled when this revision is approved.",
    params(("slug" = String, Path), ("track_slug" = String, Path), ("key" = String, Path)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::PlanOut, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn drop_unit(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    draft_write!(state, context, parts, auth, project, paths, tx, track, plan);
    let key = paths["key"].clone();
    changeable_entry(&mut tx, &plan, &key, &context).await?;
    plans::delete_unit(&mut tx, plan.id, &key)
        .await
        .map_err(track_error(&context))?;
    plans::touch(&mut tx, plan.id)
        .await
        .map_err(track_error(&context))?;
    let out = plan_out(&mut tx, &project, &track, &plan, &context).await?;
    commit(tx, &context).await?;
    Ok(json_response(StatusCode::OK, &out))
}

/// A key for the unit that redoes `#number`, free in the draft.
async fn redo_key(
    conn: &mut PgConnection,
    plan: &PlanRevision,
    number: i32,
    context: &RequestContext,
) -> Result<String, Failure> {
    let base = format!("redo-{number}");
    let mut key = base.clone();
    let mut suffix = 1;
    while plans::unit(conn, plan.id, &key)
        .await
        .map_err(track_error(context))?
        .is_some()
    {
        suffix += 1;
        key = format!("{base}-{suffix}");
    }
    Ok(key)
}

#[utoipa::path(
    put,
    path = "/api/projects/{slug}/tracks/{track_slug}/plans/draft/alignments/{number}",
    operation_id = "set_alignment_api_projects__slug__tracks__track_slug__plans_draft_alignments__number__put",
    summary = "Set Alignment",
    description = "Say what this revision does with a done or in-flight unit: `keep` it,\nmake it `obsolete` (an in-flight unit's attempt is cancelled and kept; a\ndecided unit keeps its decision) or `redo` it (obsolete, plus a new unit\nderived from it, added to the draft with the old unit's fields).",
    params(("slug" = String, Path), ("track_slug" = String, Path), ("number" = i64, Path)),
    request_body(content = crate::api_models::AlignmentSet, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = crate::api_models::AlignmentOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
#[allow(
    clippy::too_many_lines,
    reason = "The redo entry is written in the same transaction as its alignment entry"
)]
pub(crate) async fn set_alignment(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let input: AlignmentSet = decode(&body)?;
    if !matches!(input.decision.as_str(), "keep" | "obsolete" | "redo") {
        return Err(invalid(
            "body/decision",
            "Input should be 'keep', 'obsolete' or 'redo'",
        ));
    }
    if input.reason.trim().is_empty() {
        return Err(invalid("body/reason", "a reason is required"));
    }
    draft_write!(state, context, parts, auth, project, paths, tx, track, plan);
    let number = positive(&paths, "number")?;
    let unit = plans::needing_alignment(&mut tx, track.id)
        .await
        .map_err(track_error(&context))?
        .into_iter()
        .find(|unit| unit.number == number)
        .ok_or_else(|| {
            domain(
                ErrorCode::Conflict,
                format!(
                    "#{number} is not a done or in-flight unit of track '{}' that the plan still \
                     answers for",
                    track.slug
                ),
            )
        })?;
    plans::set_alignment(
        &mut tx,
        plan.id,
        unit.unit_id,
        &input.decision,
        &input.reason,
    )
    .await
    .map_err(track_error(&context))?;
    let redone = plans::units(&mut tx, plan.id)
        .await
        .map_err(track_error(&context))?
        .into_iter()
        .any(|entry| entry.redo_of == Some(unit.unit_id));
    if input.decision == "redo" && !redone {
        let stored = plans::unit_revision(
            &mut tx,
            unit.unit_id,
            unit.approved_revision.unwrap_or(unit.revision),
        )
        .await
        .map_err(track_error(&context))?
        .ok_or_else(|| internal(&context, "redone revision"))?;
        let document: Value = serde_json::from_str(&stored.content)
            .map_err(|_| internal(&context, "redone content"))?;
        let mut fields = plan_units::fields_from_unit(&document);
        if let Some((_, entry)) = plans::latest_entry(&mut tx, unit.unit_id)
            .await
            .map_err(track_error(&context))?
        {
            let planned: UnitFields = serde_json::from_str(&entry.fields)
                .map_err(|_| internal(&context, "redone fields"))?;
            fields.context = planned.context;
        }
        fields.relations.push(UnitRelation {
            kind: "derived_from".into(),
            unit: Value::from(number),
        });
        let key = redo_key(&mut tx, &plan, number, &context).await?;
        let text = entry_text(&fields, &context)?;
        plans::insert_unit(
            &mut tx,
            plan.id,
            plans::UnitEntry {
                key: &key,
                unit_id: None,
                redo_of: Some(unit.unit_id),
                fields: &text,
                brief: stored.brief.as_deref().unwrap_or_default(),
                science_revision: stored.science_revision,
            },
        )
        .await
        .map_err(track_error(&context))?;
    } else if input.decision != "redo" && redone {
        plans::delete_redo_units(&mut tx, plan.id, unit.unit_id)
            .await
            .map_err(track_error(&context))?;
    }
    plans::touch(&mut tx, plan.id)
        .await
        .map_err(track_error(&context))?;
    commit(tx, &context).await?;
    Ok(json_response(
        StatusCode::OK,
        &AlignmentOut {
            number: i64::from(unit.number),
            title: unit.title,
            state: unit.state,
            decision: input.decision,
            reason: input.reason,
        },
    ))
}

/// The open concern of the track a draft answer names.
async fn answerable(
    conn: &mut PgConnection,
    project: &projects::Project,
    track: &Track,
    paths: &BTreeMap<String, String>,
    context: &RequestContext,
) -> Result<concerns::Concern, Failure> {
    let id = paths["concern_id"]
        .parse::<cannery_core::ids::ConcernId>()
        .map_err(|_| invalid("path/concern_id", "Input should be a UUID"))?;
    let concern = concerns::get(conn, project.id, id, false)
        .await
        .map_err(track_error(context))?
        .filter(|concern| concern.track_id == track.id)
        .ok_or_else(|| {
            domain(
                ErrorCode::NotFound,
                format!("no concern {id} about the {} plan", track.slug),
            )
        })?;
    Ok(concern)
}

#[utoipa::path(
    put,
    path = "/api/projects/{slug}/tracks/{track_slug}/plans/draft/answers/{concern_id}",
    operation_id = "answer_concern_api_projects__slug__tracks__track_slug__plans_draft_answers__concern_id__put",
    summary = "Answer Concern",
    description = "Say how this revision answers an open concern about the track's plan.\nApproving the revision closes the concern as answered; every open concern\nis answered before the draft can be submitted, unless a researcher\ndismisses it.",
    params(("slug" = String, Path), ("track_slug" = String, Path), ("concern_id" = String, Path, format = "uuid")),
    request_body(content = crate::api_models::AnswerSet, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = crate::api_models::AnswerOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn set_answer(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let input: AnswerSet = decode(&body)?;
    let how = input.how.trim();
    if how.is_empty() {
        return Err(invalid(
            "body/how",
            "say how the revision answers the concern",
        ));
    }
    if how.len() > 16_384 {
        return Err(invalid("body/how", "an answer is at most 16384 bytes"));
    }
    draft_write!(state, context, parts, auth, project, paths, tx, track, plan);
    let concern = answerable(&mut tx, &project, &track, &paths, &context).await?;
    if concern.state != "open" {
        return Err(domain(
            ErrorCode::Conflict,
            format!("the concern is already {}", concern.state),
        ));
    }
    concerns::set_answer(&mut tx, plan.id, concern.id, how)
        .await
        .map_err(track_error(&context))?;
    plans::touch(&mut tx, plan.id)
        .await
        .map_err(track_error(&context))?;
    commit(tx, &context).await?;
    Ok(json_response(
        StatusCode::OK,
        &AnswerOut {
            concern: concern.id.to_string(),
            kind: concern.kind,
            state: concern.state,
            how: how.to_owned(),
        },
    ))
}

#[utoipa::path(
    delete,
    path = "/api/projects/{slug}/tracks/{track_slug}/plans/draft/answers/{concern_id}",
    operation_id = "drop_answer_api_projects__slug__tracks__track_slug__plans_draft_answers__concern_id__delete",
    summary = "Drop Answer",
    description = "Remove the draft's answer to a concern and return the draft.",
    params(("slug" = String, Path), ("track_slug" = String, Path), ("concern_id" = String, Path, format = "uuid")),
    responses((status = 200, description = "Successful Response", body = crate::api_models::PlanOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn drop_answer(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    draft_write!(state, context, parts, auth, project, paths, tx, track, plan);
    let concern = answerable(&mut tx, &project, &track, &paths, &context).await?;
    if !concerns::delete_answer(&mut tx, plan.id, concern.id)
        .await
        .map_err(track_error(&context))?
    {
        return Err(domain(
            ErrorCode::NotFound,
            format!("the draft does not answer concern {}", concern.id),
        ));
    }
    plans::touch(&mut tx, plan.id)
        .await
        .map_err(track_error(&context))?;
    let plan = plans::get(&mut tx, track.id, plan.revision)
        .await
        .map_err(track_error(&context))?
        .ok_or_else(|| internal(&context, "plan reload"))?;
    let out = plan_out(&mut tx, &project, &track, &plan, &context).await?;
    commit(tx, &context).await?;
    Ok(json_response(StatusCode::OK, &out))
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/tracks/{track_slug}/plans/draft/check",
    operation_id = "check_plan_api_projects__slug__tracks__track_slug__plans_draft_check_get",
    summary = "Check Plan",
    description = "What blocks submitting the draft: no project brief, no approach, missing\nalignment entries, unknown unit keys, units no longer queued, limits\nexceeded, open concerns it does not answer. `ready` when nothing does.",
    params(("slug" = String, Path), ("track_slug" = String, Path)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::PlanCheckOut, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn check(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let (mut auth, project, paths) = reader(&state, &context, &mut parts).await?;
    let track = load_track(
        &mut auth.connection,
        &state,
        &context,
        &project,
        &paths["track_slug"],
        None,
    )
    .await?;
    let plan = match plans::open(&mut auth.connection, track.id, false)
        .await
        .map_err(track_error(&context))?
    {
        Some(plan) if plan.state == "draft" => plan,
        _ => {
            return Err(domain(
                ErrorCode::NotFound,
                format!("the {} plan has no draft", track.slug),
            ));
        }
    };
    let problems = problems(&mut auth.connection, &project, &track, &plan, &context).await?;
    Ok(json_response(
        StatusCode::OK,
        &PlanCheckOut {
            revision: i64::from(plan.revision),
            ready: problems.is_empty(),
            problems,
        },
    ))
}

fn blocked(problems: &[PlanProblem]) -> Failure {
    let details = problems
        .iter()
        .map(|problem| json!({"path": problem.path, "message": problem.message, "code": problem.code}))
        .collect::<Vec<_>>();
    Failure::new(ApiError::from(
        DomainError::new(
            ErrorCode::Conflict,
            format!(
                "the plan is not ready: {} problem(s); see check_plan",
                problems.len()
            ),
        )
        .with_details(Value::Array(details)),
    ))
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/tracks/{track_slug}/plans/draft/submission",
    operation_id = "submit_plan_api_projects__slug__tracks__track_slug__plans_draft_submission_post",
    summary = "Submit Plan",
    description = "Freeze the draft and open its `plan` review case. Refused with the\nproblems `check_plan` reports while there are any.",
    params(("slug" = String, Path), ("track_slug" = String, Path)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::PlanOut, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn submit(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    draft_write!(state, context, parts, auth, project, paths, tx, track, plan);
    let found = problems(&mut tx, &project, &track, &plan, &context).await?;
    if !found.is_empty() {
        return Err(blocked(&found));
    }
    let Principal::User(user) = &auth.principal else {
        return Err(internal(&context, "plan submitter"));
    };
    let case = plans::submit(&mut tx, plan.id, project.id, plan.revision, user.user_id)
        .await
        .map_err(track_error(&context))?;
    let plan = plans::get(&mut tx, track.id, plan.revision)
        .await
        .map_err(track_error(&context))?
        .ok_or_else(|| internal(&context, "submitted plan"))?;
    let new_state = json!({
        "track": track.slug, "revision": plan.revision, "state": "submitted",
        "review_case": case.to_string(), "units": plan.units,
    });
    record(
        &mut tx,
        &auth.principal,
        &project,
        &plan,
        "plan.submitted",
        &new_state,
        None,
        &context,
    )
    .await?;
    let out = plan_out(&mut tx, &project, &track, &plan, &context).await?;
    commit(tx, &context).await?;
    Ok(json_response(StatusCode::OK, &out))
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/tracks/{track_slug}/plans/{revision}/review",
    operation_id = "review_plan_api_projects__slug__tracks__track_slug__plans__revision__review_post",
    summary = "Review Plan",
    description = "A researcher's decision on a submitted revision, with a reason: `approve`\nit (in one transaction: create a queued unit per new unit, write a\nnew revision of each changed one, cancel the queued ones it drops, apply\nits alignment entries, and move a planning track to active), `send_back`\nfor another revision, or `decline` it.",
    params(("slug" = String, Path), ("track_slug" = String, Path), ("revision" = i64, Path)),
    request_body(content = crate::api_models::PlanReview, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = crate::api_models::PlanOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
#[allow(
    clippy::too_many_lines,
    reason = "The review, the approval's writes, the activation and their audit rows stay in one transaction"
)]
pub(crate) async fn review(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let input: PlanReview = decode(&body)?;
    if !matches!(input.action.as_str(), "approve" | "send_back" | "decline") {
        return Err(invalid(
            "body/action",
            "Input should be 'approve', 'send_back' or 'decline'",
        ));
    }
    if input.reason.trim().is_empty() {
        return Err(invalid("body/reason", "a reason is required"));
    }
    let (mut auth, project, paths) = researcher(&state, &context, &mut parts).await?;
    let revision = positive(&paths, "revision")?;
    let Principal::User(user) = &auth.principal else {
        return Err(internal(&context, "plan reviewer"));
    };
    let (user_id, via_channel, via_client) = (
        user.user_id,
        channel_name(user.via.channel),
        user.via.client.clone(),
    );
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "plan transaction"))?;
    let track = load_track(
        &mut tx,
        &state,
        &context,
        &project,
        &paths["track_slug"],
        Some(RowLock::Update),
    )
    .await?;
    let plan = plans::open(&mut tx, track.id, true)
        .await
        .map_err(track_error(&context))?
        .filter(|plan| plan.revision == revision && plan.state == "submitted")
        .ok_or_else(|| {
            domain(
                ErrorCode::Conflict,
                format!(
                    "revision {revision} of the {} plan is not waiting for review",
                    track.slug
                ),
            )
        })?;
    let case = plan
        .review_case_id
        .ok_or_else(|| internal(&context, "plan review case"))?;
    let mut new_state = json!({"track": track.slug, "revision": plan.revision});
    if input.action == "approve" {
        let found = problems(&mut tx, &project, &track, &plan, &context).await?;
        if !found.is_empty() {
            return Err(blocked(&found));
        }
        new_state["applied"] = crate::plan_approval::apply(
            &mut tx,
            &state,
            &auth.principal,
            &project,
            &track,
            &plan,
            &context,
        )
        .await?;
        new_state["answered"] = Value::from(
            crate::concern_routes::answer_approved(
                &mut tx,
                &auth.principal,
                &project,
                &plan,
                &context,
            )
            .await?,
        );
    }
    plans::review(
        &mut tx,
        plans::Review {
            plan: plan.id,
            case,
            revision: plan.revision,
            action: &input.action,
            reason: &input.reason,
            by: user_id,
            via_channel,
            via_client: via_client.as_deref(),
        },
    )
    .await
    .map_err(track_error(&context))?;
    let activated = input.action == "approve"
        && plans::activate(&mut tx, track.id)
            .await
            .map_err(track_error(&context))?;
    if activated {
        let subject = track.id.to_string();
        let prior = json!({"state": "planning"});
        let next = json!({"state": "active"});
        let reason = format!("plan revision {} approved", plan.revision);
        audit::record(
            &mut tx,
            Attribution::Principal(&auth.principal),
            Record {
                action: "track.state_changed",
                subject_type: "track",
                subject_id: &subject,
                project_id: Some(project.id),
                prior_state: Some(&prior),
                new_state: Some(&next),
                reason: Some(&reason),
                idempotency_key: None,
            },
        )
        .await
        .map_err(|_| internal(&context, "track audit"))?;
    }
    let action = match input.action.as_str() {
        "approve" => "plan.approved",
        "send_back" => "plan.sent_back",
        _ => "plan.declined",
    };
    let plan = plans::get(&mut tx, track.id, plan.revision)
        .await
        .map_err(track_error(&context))?
        .ok_or_else(|| internal(&context, "reviewed plan"))?;
    new_state["state"] = Value::from(plan.state.clone());
    record(
        &mut tx,
        &auth.principal,
        &project,
        &plan,
        action,
        &new_state,
        Some(&input.reason),
        &context,
    )
    .await?;
    let out = plan_out(&mut tx, &project, &track, &plan, &context).await?;
    commit(tx, &context).await?;
    Ok(json_response(StatusCode::OK, &out))
}

/// The relations and mentions of a written unit revision.
pub(crate) async fn replace_links(
    conn: &mut PgConnection,
    unit: UnitId,
    relations: Vec<(units::RelationKind, UnitId)>,
    mentions: BTreeSet<UnitId>,
    context: &RequestContext,
) -> Result<(), Failure> {
    units::replace_relations(conn, unit, relations)
        .await
        .map_err(|_| internal(context, "unit relations"))?;
    units::replace_mentions(conn, MentionSource::Unit(unit), mentions)
        .await
        .map_err(|_| internal(context, "unit mentions"))
}

/// `check_entry` for approval, which already holds a checked draft.
pub(crate) async fn check_planned(
    conn: &mut PgConnection,
    state: &RouteState,
    principal: &Principal,
    project: &projects::Project,
    value: &Value,
    self_id: UnitId,
    context: &RequestContext,
) -> Result<crate::unit_mutations::CheckedUnit, Failure> {
    let document = plan_units::document(value).ok_or_else(|| internal(context, "unit document"))?;
    check_unit(
        conn,
        principal,
        project,
        &document,
        Some(self_id),
        state,
        context,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::unit_path;

    #[test]
    fn document_paths_name_the_unit_fields() {
        assert_eq!(
            unit_path("/plan/primary_metric").as_deref(),
            Some("body/acceptance/primary_metric")
        );
        assert_eq!(
            unit_path("/project_fields").as_deref(),
            Some("body/parameters")
        );
        assert_eq!(unit_path("/title").as_deref(), Some("body/title"));
        assert_eq!(unit_path("/rationale"), None);
        assert_eq!(unit_path("plan"), None);
    }
}
