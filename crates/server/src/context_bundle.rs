// SPDX-License-Identifier: AGPL-3.0-only
//! An attempt's context bundle: what its performer reads before working,
//! assembled on demand from the revisions the attempt pinned at its claim
//! and never stored. The brief, the plan's approach, the unit's fields and
//! brief, what to submit (from the pinned science revision), a one-line
//! index of the track's other units, a summary line with a reference for
//! each context item and each unit it derives from, and how the unit's
//! earlier attempts ended, with their decisions, steering and questions.
use crate::{
    api_models::{ContextBundleRef, ContextItem, PlanRef},
    plan_routes::{context_label, paths, positive},
    plan_units::{self, UnitFields, first_line, truncate},
    requests::RequestContext,
    unit_routes::{Failure, RouteState, domain, internal},
};
use axum::{
    extract::{Request, State},
    http::header,
    response::{IntoResponse, Response},
};
use cannery_core::{errors::ErrorCode, ids::AttemptId};
use cannery_projects::{authz, briefs, repo as projects};
use cannery_tracks::{
    messages,
    plans::{self, AttemptPins, Limits},
};
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
    format!("/api/projects/{slug}/units/{number}/attempts/{sequence}/context.md")
}

/// How to read an attempt's bundle of `bytes` bytes over REST, over MCP with
/// `get_context`, and as an MCP resource; `phase` is `document` or `decide`
/// for the documenter's or the decider's bundle.
pub(crate) fn bundle_ref(
    slug: &str,
    number: i32,
    sequence: i32,
    phase: Option<&str>,
    bytes: usize,
) -> ContextBundleRef {
    let mut arguments = std::collections::BTreeMap::from([
        ("project".to_owned(), Value::from(slug)),
        ("number".to_owned(), Value::from(number)),
        ("sequence".to_owned(), Value::from(sequence)),
    ]);
    if let Some(phase) = phase {
        arguments.insert("phase".to_owned(), Value::from(phase));
    }
    ContextBundleRef {
        r#ref: format!(
            "{}{}",
            bundle_path(slug, number, sequence),
            phase.map_or_else(String::new, |phase| format!("?phase={phase}"))
        ),
        bytes: i64::try_from(bytes).unwrap_or(i64::MAX),
        tool: String::from("get_context"),
        arguments,
        resource: format!(
            "cannery-row://projects/{slug}/units/{number}/attempts/{sequence}/context{}",
            phase.map_or_else(String::new, |phase| format!("/{phase}"))
        ),
    }
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
        Some(bundle_ref(
            &project.slug,
            pins.number,
            pins.sequence,
            None,
            bundle.len(),
        )),
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
                        let brief = plans::current_unit_brief(conn, unit.unit_id)
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
                            format!("/api/projects/{slug}/units/{number}/plan"),
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
                format!(
                    "/api/projects/{slug}/units/{unit}/writeup; the run's report: /api/projects/{slug}/units/{unit}/attempts/{attempt}/report"
                ),
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

/// What a bundle holds: the performer's full or compact bundle, the
/// verifier's, which says what its verify job must produce instead of what
/// to submit, or the documenter's and the decider's, which add the unit's
/// record.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Detail {
    Full,
    Compact,
    Verify,
    Document,
    Decide,
}

/// Build an attempt's bundle. `compact` keeps the brief's goal, the unit's
/// fields and brief and the index, capped at 16 KiB.
pub(crate) async fn build(
    conn: &mut PgConnection,
    project: &projects::Project,
    pins: &AttemptPins,
    compact: bool,
    context: &RequestContext,
) -> Result<String, Failure> {
    build_for(
        conn,
        project,
        pins,
        if compact {
            Detail::Compact
        } else {
            Detail::Full
        },
        context,
    )
    .await
}

fn fenced(value: &str) -> String {
    fenced_as("", value)
}

/// A fenced block of `language`, its fence longer than any run of backticks
/// in `value`.
fn fenced_as(language: &str, value: &str) -> String {
    let mut fence = String::from("```");
    while value.contains(fence.as_str()) {
        fence.push('`');
    }
    format!("{fence}{language}\n{}\n{fence}", value.trim_end())
}

/// The unit's record, in attempt order: each attempt's run document
/// and notes, failures with their logs and verification reports, then the
/// comments, and for the decider the write-up or why there is none.
#[allow(
    clippy::too_many_lines,
    reason = "The record is written in the order a documenter reads it"
)]
async fn record(
    conn: &mut PgConnection,
    project: &projects::Project,
    pins: &AttemptPins,
    detail: Detail,
    body: &mut String,
    context: &RequestContext,
) -> Result<(), Failure> {
    let fail = |_| internal(context, "bundle record");
    let attempts = sqlx::query!(
        r#"SELECT id AS "id: uuid::Uuid", sequence, state FROM attempts WHERE unit_id=$1 ORDER BY sequence"#,
        pins.unit_id.0 as _
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(fail)?;
    let outputs = sqlx::query!(
        r#"SELECT p.attempt_id AS "attempt_id: uuid::Uuid", p.stage, p.revision, p.front_matter::text AS "front_matter!", p.body AS "body!", p.sha256 AS "sha256?", p.id AS "id: uuid::Uuid"
           FROM phase_outputs p JOIN attempts a ON a.id=p.attempt_id
           WHERE a.unit_id=$1 AND p.status='completed' AND p.stage IN ('agent','verification','writeup')
           ORDER BY a.sequence, p.created_at, p.id"#,
        pins.unit_id.0 as _
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(fail)?;
    let failures = sqlx::query!(
        r#"SELECT f.attempt_id AS "attempt_id: uuid::Uuid", f.stage, f.code, f.reason, f.log_refs::text AS "log_refs!"
           FROM attempt_failures f JOIN attempts a ON a.id=f.attempt_id
           WHERE a.unit_id=$1 ORDER BY a.sequence, f.created_at, f.id"#,
        pins.unit_id.0 as _
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(fail)?;
    let slug = &project.slug;
    let number = pins.number;
    body.push_str("## Attempts\n\n");
    for attempt in &attempts {
        let sequence = attempt.sequence;
        let _ = writeln!(
            body,
            "### Attempt #{number}.{sequence} ({})\n",
            attempt.state
        );
        // A verification report is the `verification` of the job that
        // published it; one without a job (imported) is the report's.
        let jobs = cannery_jobs::repo::list_jobs(
            &mut *conn,
            AttemptId(attempt.id),
            None,
            None,
            cannery_jobs::repo::JsonContext {
                encode_nesting_budget: cannery_core::json::MAX_DEPTH,
                decode_nesting_budget: cannery_core::json::MAX_DEPTH,
            },
        )
        .await
        .map_err(|_| internal(context, "bundle record jobs"))?;
        let mut empty = true;
        for output in outputs
            .iter()
            .filter(|output| output.attempt_id == attempt.id && output.stage != "writeup")
        {
            empty = false;
            let report = format!("/api/projects/{slug}/units/{number}/attempts/{sequence}/report");
            let (title, reference) = if output.stage == "agent" {
                (
                    format!("Run document (revision {})", output.revision),
                    report,
                )
            } else {
                (
                    format!(
                        "Verification report (revision {}, ref {}, sha256 {})",
                        output.revision,
                        output.id,
                        output.sha256.as_deref().unwrap_or_default()
                    ),
                    jobs.iter()
                        .find(|job| job.evidence_id.is_some_and(|id| id.0 == output.id))
                        .map_or_else(
                            || format!("{report}, its `verification`"),
                            |job| {
                                format!("/api/projects/{slug}/jobs/{}, its `verification`", job.id)
                            },
                        ),
                )
            };
            let front_matter: Value =
                serde_json::from_str(&output.front_matter).unwrap_or_default();
            let _ = writeln!(
                body,
                "#### {title}\n\n({reference})\n\n{}\n",
                fenced(&serde_json::to_string_pretty(&front_matter).unwrap_or_default())
            );
            if !output.body.trim().is_empty() {
                let _ = writeln!(body, "{}\n", output.body.trim());
            }
        }
        for failure in failures
            .iter()
            .filter(|failure| failure.attempt_id == attempt.id)
        {
            empty = false;
            let _ = writeln!(
                body,
                "#### Failure at {}: {}\n\n{}\n",
                failure.stage,
                failure.code,
                failure.reason.trim()
            );
            let logs: Value = serde_json::from_str(&failure.log_refs).unwrap_or_default();
            for log in logs.as_array().into_iter().flatten() {
                let _ = writeln!(
                    body,
                    "- log {} ({} bytes, sha256 {})",
                    log["key"].as_str().unwrap_or_default(),
                    log["size_bytes"],
                    log["sha256"].as_str().unwrap_or_default()
                );
            }
            if logs.as_array().is_some_and(|logs| !logs.is_empty()) {
                body.push('\n');
            }
        }
        if empty {
            body.push_str("No run document, failure or verification report.\n\n");
        }
    }
    let comments = sqlx::query!(
        r#"SELECT c.body_markdown, c.created_at AS "created_at: cannery_core::timestamps::Timestamp", a.sequence AS "sequence?", coalesce(u.display_name, u.email, 'a researcher') AS "author!"
           FROM comments c JOIN users u ON u.id=c.author_user LEFT JOIN attempts a ON a.id=c.attempt_id
           WHERE c.unit_id=$1 ORDER BY c.created_at, c.id"#,
        pins.unit_id.0 as _
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(fail)?;
    body.push_str("## Comments\n\n");
    if comments.is_empty() {
        body.push_str("None.\n\n");
    }
    for comment in comments {
        let about = comment
            .sequence
            .map_or_else(String::new, |sequence| format!(" on #{number}.{sequence}"));
        let _ = writeln!(
            body,
            "- {} ({}){about}: {}",
            comment.author,
            comment.created_at.isoformat(),
            comment.body_markdown.trim().replace('\n', "\n  ")
        );
    }
    body.push('\n');
    conversation(conn, project, pins, body, context).await?;
    if detail == Detail::Decide {
        body.push_str("## Write-up\n\n");
        let writeup = outputs.iter().rfind(|output| output.stage == "writeup");
        let skipped = sqlx::query_scalar!(
            r#"SELECT j.error_reason AS "reason!" FROM jobs j JOIN attempts a ON a.id=j.attempt_id
               WHERE a.unit_id=$1 AND j.phase='document' AND j.state='skipped'
               ORDER BY j.created_at DESC LIMIT 1"#,
            pins.unit_id.0 as _
        )
        .fetch_optional(&mut *conn)
        .await
        .map_err(fail)?;
        // What the decision document cites, as its decision case does: the
        // pinned attempt's latest verification report when it was
        // verified, and the write-up, null when it was skipped.
        let verified = attempts
            .iter()
            .any(|attempt| attempt.id == pins.attempt_id.0 && attempt.state == "verified");
        let verification = outputs
            .iter()
            .filter(|output| {
                verified && output.attempt_id == pins.attempt_id.0 && output.stage == "verification"
            })
            .max_by_key(|output| output.revision)
            .map(|output| (output.id, output.sha256.clone().unwrap_or_default()));
        let cited = writeup.map(|output| (output.id, output.sha256.clone().unwrap_or_default()));
        let written = writeup.is_some() || skipped.is_some();
        match (writeup, skipped) {
            (Some(writeup), _) => {
                let front_matter: Value =
                    serde_json::from_str(&writeup.front_matter).unwrap_or_default();
                let _ = writeln!(
                    body,
                    "(ref {}, sha256 {}; /api/projects/{slug}/units/{number}/writeup)\n\n{}\n\n{}\n",
                    writeup.id,
                    writeup.sha256.as_deref().unwrap_or_default(),
                    fenced(&serde_json::to_string_pretty(&front_matter).unwrap_or_default()),
                    writeup.body.trim()
                );
            }
            (None, Some(reason)) => {
                let _ = writeln!(body, "No write-up: {}\n", reason.trim());
            }
            (None, None) => body.push_str("Not written yet.\n\n"),
        }
        let cite = |cited: Option<&(uuid::Uuid, String)>| {
            cited.map_or_else(
                || String::from("null"),
                |(id, sha256)| format!("{{ref: \"{id}\", sha256: \"{sha256}\"}}"),
            )
        };
        body.push_str("## Deciding\n\n");
        let _ = writeln!(
            body,
            "The decision document is Markdown with YAML front matter (`get_schema` `decision`) and the reason as its body, sent with `record_decision` on the unit's decision case or `complete_job` for a decide job. `outcome` is `promote` (only on a `pass` verdict), `reject` or `inconclusive`, or `failed` for a unit stopped after a failure, which has no verification report. It cites the verification report and the write-up exactly{}:\n\n{}\n",
            if written {
                ""
            } else {
                " (the write-up is not written yet; the decision case opens once it is, or once a researcher skips it)"
            },
            fenced_as(
                "markdown",
                &format!(
                    "---\noutcome: {}\nverification: {}\nwriteup: {}\n---\n\nWhy this outcome.",
                    if verification.is_some() {
                        "<promote, reject or inconclusive>"
                    } else {
                        "failed"
                    },
                    cite(verification.as_ref()),
                    if written {
                        cite(cited.as_ref())
                    } else {
                        String::from("<the write-up, once written>")
                    }
                )
            )
        );
    }
    Ok(())
}

/// Indent the lines after the first of a Markdown text, so it stays inside
/// its list item.
fn indented(text: &str) -> String {
    text.trim().replace('\n', "\n  ")
}

/// The questions asked during the unit's earlier attempts, with the answers
/// given before this attempt was claimed, so the bundle does not change
/// while it runs; a later answer reaches the attempt in its heartbeats.
/// Nothing when there are none. One line each when `line_max` is given.
async fn earlier_questions(
    conn: &mut PgConnection,
    project: &projects::Project,
    pins: &AttemptPins,
    line_max: Option<usize>,
    body: &mut String,
    context: &RequestContext,
) -> Result<(), Failure> {
    let questions = messages::earlier_questions(conn, project.id, pins.unit_id, pins.sequence)
        .await
        .map_err(persistence(context))?;
    if questions.is_empty() {
        return Ok(());
    }
    let claimed = claimed_at(conn, pins, context).await?;
    body.push_str("## Questions from earlier attempts\n\n");
    for question in questions {
        let answer = question.answer_body.as_deref().filter(|_| {
            question
                .answer_created_at
                .is_some_and(|at| at.0 <= claimed.0)
        });
        let kind = if question.blocking == Some(true) {
            "blocking"
        } else {
            "non-blocking"
        };
        let about = format!(
            "#{}.{}, {kind}, {}",
            question.unit_number,
            question.attempt_sequence,
            if answer.is_some() {
                question.state.as_deref().unwrap_or("open")
            } else {
                "not answered yet"
            }
        );
        if let Some(max) = line_max {
            let line = format!(
                "{about}: {}{}",
                first_line(&question.body),
                answer.map_or_else(String::new, |answer| format!(
                    " Answer: {}",
                    first_line(answer)
                ))
            );
            let _ = writeln!(body, "- {}", truncate(&line, max));
            continue;
        }
        let _ = writeln!(body, "- {about}: {}", indented(&question.body));
        if let Some(default) = &question.default_text {
            let _ = writeln!(body, "  - Assumed meanwhile: {}", indented(default));
        }
        if let Some(answer) = answer {
            let _ = writeln!(
                body,
                "  - Answer ({}): {}",
                question
                    .answer_author_name
                    .as_deref()
                    .unwrap_or("a researcher"),
                indented(answer)
            );
        }
    }
    body.push('\n');
    Ok(())
}

/// The unit's questions, answers and steering notes, oldest first, for its
/// documenter and decider. Nothing when there are none.
async fn conversation(
    conn: &mut PgConnection,
    project: &projects::Project,
    pins: &AttemptPins,
    body: &mut String,
    context: &RequestContext,
) -> Result<(), Failure> {
    let filter = messages::Filter {
        unit: Some(pins.unit_id),
        ..messages::Filter::default()
    };
    let mut found = messages::select(conn, project.id, filter, i64::MAX)
        .await
        .map_err(persistence(context))?;
    found.retain(|message| message.kind != "answer");
    if found.is_empty() {
        return Ok(());
    }
    found.reverse();
    body.push_str("## Questions and steering\n\n");
    for message in found {
        let at = format!("#{}.{}", message.unit_number, message.attempt_sequence);
        let by = message.author_name.as_deref().unwrap_or("someone");
        let when = message.created_at.isoformat();
        if message.kind == "steer" {
            let _ = writeln!(
                body,
                "- Steering note on {at} by {by} ({when}): {}",
                indented(&message.body)
            );
            continue;
        }
        let _ = writeln!(
            body,
            "- Question on {at}{} by {by} ({when}, {}{}): {}",
            message
                .job_phase
                .as_deref()
                .map_or_else(String::new, |phase| format!(" ({phase} job)")),
            if message.blocking == Some(true) {
                "blocking"
            } else {
                "non-blocking"
            },
            message
                .state
                .as_deref()
                .map_or_else(String::new, |state| format!(", {state}")),
            indented(&message.body)
        );
        if let Some(default) = &message.default_text {
            let _ = writeln!(body, "  - Assumed meanwhile: {}", indented(default));
        }
        if let Some(answer) = &message.answer_body {
            let _ = writeln!(
                body,
                "  - Answer ({}): {}",
                message
                    .answer_author_name
                    .as_deref()
                    .unwrap_or("a researcher"),
                indented(answer)
            );
        }
    }
    body.push('\n');
    Ok(())
}

/// When the attempt was claimed: what the bundle shows of other attempts
/// stops there, so it does not change while the attempt runs.
async fn claimed_at(
    conn: &mut PgConnection,
    pins: &AttemptPins,
    context: &RequestContext,
) -> Result<cannery_core::timestamps::Timestamp, Failure> {
    sqlx::query_scalar("SELECT claimed_at FROM attempts WHERE id = $1")
        .bind(pins.attempt_id.0)
        .fetch_one(&mut *conn)
        .await
        .map_err(|_| internal(context, "bundle claim time"))
}

/// The unit's earlier attempts as they stood at this attempt's claim: how
/// each ended, why it failed, the researchers' decisions on it with their
/// reasons (a failure case's `retry` says what to do differently), and the
/// steering notes posted to it. Nothing for a first attempt. One line per
/// attempt and note when `line_max` is given.
#[allow(
    clippy::too_many_lines,
    reason = "Each attempt's failures and decisions, then the notes, in reading order"
)]
async fn earlier_attempts(
    conn: &mut PgConnection,
    project: &projects::Project,
    pins: &AttemptPins,
    line_max: Option<usize>,
    body: &mut String,
    context: &RequestContext,
) -> Result<(), Failure> {
    if pins.sequence <= 1 {
        return Ok(());
    }
    let rows = sqlx::query!(
        r#"WITH claim AS (SELECT claimed_at AS at FROM attempts WHERE id = $3)
           SELECT a.sequence, a.state,
             (SELECT jsonb_agg(jsonb_build_object('stage', f.stage, 'code', f.code, 'reason', f.reason)
                               ORDER BY f.created_at, f.id)
              FROM attempt_failures f, claim WHERE f.attempt_id = a.id AND f.created_at <= claim.at)::text
               AS "failures?",
             (SELECT jsonb_agg(jsonb_build_object('kind', c.kind, 'action', d.action, 'reason', d.reason)
                               ORDER BY d.decided_at, d.id)
              FROM review_cases c JOIN decisions d ON d.review_case_id = c.id, claim
              WHERE c.attempt_id = a.id AND d.decided_at <= claim.at
                AND NOT EXISTS (SELECT 1 FROM decisions s
                                WHERE s.supersedes = d.id AND s.decided_at <= claim.at))::text
               AS "decisions?"
           FROM attempts a WHERE a.unit_id = $1 AND a.sequence < $2 ORDER BY a.sequence"#,
        pins.unit_id.0 as _,
        pins.sequence,
        pins.attempt_id.0 as _
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(|_| internal(context, "bundle earlier attempts"))?;
    if rows.is_empty() {
        return Ok(());
    }
    let claimed = claimed_at(conn, pins, context).await?;
    let filter = messages::Filter {
        unit: Some(pins.unit_id),
        ..messages::Filter::default()
    };
    let mut steering = messages::select(conn, project.id, filter, i64::MAX)
        .await
        .map_err(persistence(context))?;
    steering.retain(|message| {
        message.kind == "steer"
            && message.attempt_sequence < pins.sequence
            && message.created_at.0 <= claimed.0
    });
    steering.reverse();
    let number = pins.number;
    body.push_str("## Earlier attempts\n\n");
    body.push_str(
        "What the unit's earlier attempts ran into, as it stood at your claim. Do not repeat what failed; follow the reasons of the decisions.\n\n",
    );
    for row in rows {
        let failures: Value = row
            .failures
            .as_deref()
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or_default();
        let decisions: Value = row
            .decisions
            .as_deref()
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or_default();
        let mut parts = Vec::new();
        for failure in failures.as_array().into_iter().flatten() {
            parts.push(format!(
                "failed at {} with `{}`: {}",
                failure["stage"].as_str().unwrap_or_default(),
                failure["code"].as_str().unwrap_or_default(),
                failure["reason"].as_str().unwrap_or_default().trim()
            ));
        }
        for decision in decisions.as_array().into_iter().flatten() {
            parts.push(format!(
                "{} decision `{}`: {}",
                decision["kind"].as_str().unwrap_or_default(),
                decision["action"].as_str().unwrap_or_default(),
                decision["reason"].as_str().unwrap_or_default().trim()
            ));
        }
        let at = format!("#{number}.{} ({})", row.sequence, row.state);
        if let Some(max) = line_max {
            let line = format!(
                "{at}: {}",
                parts
                    .iter()
                    .map(|part| first_line(part))
                    .collect::<Vec<_>>()
                    .join("; ")
            );
            let _ = writeln!(body, "- {}", truncate(&line, max));
            continue;
        }
        let _ = writeln!(body, "- {at}");
        for part in parts {
            let _ = writeln!(body, "  - {}", indented(&part).replace('\n', "\n  "));
        }
    }
    for message in steering {
        let line = format!(
            "Steering note on #{number}.{} by {}: {}",
            message.attempt_sequence,
            message.author_name.as_deref().unwrap_or("a researcher"),
            message.body.trim()
        );
        match line_max {
            Some(max) => {
                let _ = writeln!(body, "- {}", truncate(&first_line(&line), max));
            }
            None => {
                let _ = writeln!(body, "- {}", indented(&line));
            }
        }
    }
    body.push('\n');
    Ok(())
}

/// The science revision an attempt pinned: its number and its content.
/// # Errors
/// Fails when the attempt or its revision is missing or unreadable.
pub(crate) async fn pinned_science(
    conn: &mut PgConnection,
    attempt: AttemptId,
) -> Result<(i32, Value), ()> {
    let row = sqlx::query!(
        r#"SELECT a.science_revision, c.content::text AS "content!"
           FROM attempts a JOIN config_revisions c ON c.project_id = a.project_id
             AND c.kind = 'science' AND c.revision = a.science_revision
           WHERE a.id = $1"#,
        attempt.0 as _
    )
    .fetch_one(&mut *conn)
    .await
    .map_err(|_| ())?;
    let science: Value = serde_json::from_str(&row.content).map_err(|_| ())?;
    Ok((row.science_revision, science))
}

/// What the attempt must submit, from the science revision it pinned: the
/// steps, the artifact roles its manifest needs, the metrics it may claim,
/// the datasets and interfaces, and an example manifest and run document.
/// Compact keeps the steps, roles and metric keys.
#[allow(
    clippy::too_many_lines,
    reason = "The section follows the order of the steps it describes"
)]
async fn submitting(
    conn: &mut PgConnection,
    pins: &AttemptPins,
    compact: bool,
    body: &mut String,
    context: &RequestContext,
) -> Result<(), Failure> {
    let (revision, science) = pinned_science(conn, pins.attempt_id)
        .await
        .map_err(|()| internal(context, "bundle science revision"))?;
    let required = crate::job_outputs::required_roles(&science, "attempt");
    let roles = crate::job_outputs::names(&required);
    let metrics = science["metrics"].as_array().cloned().unwrap_or_default();
    let quoted = |items: &[&str]| {
        items
            .iter()
            .map(|item| format!("`{item}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let _ = writeln!(body, "## Submitting\n");
    let _ = writeln!(
        body,
        "From science revision {revision}, which this attempt pinned. When the run is done:\n\n\
         1. Upload each file with `create_upload` (role, name, size_bytes, sha256, media_type) and send its bytes as the grant says.\n\
         2. Record the manifest with `post_manifest`: one object per uploaded file. It returns the manifest's `ref` and `sha256`.\n\
         3. Submit the run document with `submit_attempt`: YAML front matter (`get_schema` `run`) and run notes as the body.\n\n\
         A document the server refuses fails the attempt with `invalid_submission` and the reason, so check it against this section first. A run that failed releases the attempt (`release_attempt`) with a failure report instead.\n"
    );
    if compact || required.iter().all(|role| role.description.is_none()) {
        let _ = writeln!(
            body,
            "Required artifact roles (the manifest needs an object of each): {}\n",
            if roles.is_empty() {
                String::from("none")
            } else {
                quoted(&roles)
            }
        );
    } else {
        let _ = writeln!(
            body,
            "Required artifact roles (the manifest needs an object of each):{}",
            crate::job_outputs::role_list(&required)
        );
    }
    if compact {
        let keys: Vec<&str> = metrics
            .iter()
            .filter_map(|metric| metric["key"].as_str())
            .collect();
        let _ = writeln!(
            body,
            "Metrics you may claim: {}. The full bundle lists their splits and slices and shows an example manifest and run document.\n",
            quoted(&keys)
        );
        return Ok(());
    }
    let list = |value: &Value| {
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    };
    body.push_str("### Metrics\n\nClaim them in `claims`, one entry per metric, split and slice (`dimensions`), with `authority: agent_claim` and the metric's unit and direction. Report a value you could not measure with a `missing_reason` instead of a value, never as zero.\n\n");
    for metric in &metrics {
        let mut line = format!(
            "- `{}`: {}, {} is better, {} over splits {}",
            metric["key"].as_str().unwrap_or_default(),
            metric["unit"].as_str().unwrap_or_default(),
            metric["direction"].as_str().unwrap_or_default(),
            metric["aggregation"].as_str().unwrap_or_default(),
            list(&metric["splits"])
        );
        let dimensions: Vec<String> = metric["dimensions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|dimension| {
                let name = dimension["name"].as_str().unwrap_or_default();
                match dimension.get("values") {
                    Some(values) => format!("{name} ({})", list(values)),
                    None => name.to_owned(),
                }
            })
            .collect();
        if !dimensions.is_empty() {
            let _ = write!(line, "; dimensions {}", dimensions.join(", "));
        }
        let slices: Vec<String> = metric["required_slices"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|slice| {
                format!(
                    "{} = {}{}",
                    slice["dimension"].as_str().unwrap_or_default(),
                    list(&slice["values"]),
                    slice
                        .get("splits")
                        .map_or_else(String::new, |splits| format!(" on {}", list(splits)))
                )
            })
            .collect();
        if !slices.is_empty() {
            let _ = write!(line, "; report every required slice: {}", slices.join("; "));
        }
        let _ = writeln!(body, "{line}");
    }
    body.push('\n');
    if let Some(datasets) = science["datasets"]
        .as_array()
        .filter(|items| !items.is_empty())
    {
        body.push_str("### Datasets\n\n");
        for dataset in datasets {
            let _ = writeln!(
                body,
                "- `{}` revision `{}`{}{}",
                dataset["id"].as_str().unwrap_or_default(),
                dataset["revision"].as_str().unwrap_or_default(),
                if dataset["held_out_labels"].as_bool() == Some(true) {
                    " (held-out labels: never in your outputs)"
                } else {
                    ""
                },
                dataset["description"]
                    .as_str()
                    .map_or_else(String::new, |text| format!(": {}", first_line(text)))
            );
        }
        body.push('\n');
    }
    if let Some(interfaces) = science["interfaces"]
        .as_array()
        .filter(|items| !items.is_empty())
    {
        body.push_str("### Interfaces\n\nThe formats the project's steps exchange:\n\n");
        for interface in interfaces {
            let mut line = format!(
                "- `{}` version {}",
                interface["name"].as_str().unwrap_or_default(),
                interface["version"]
            );
            for field in ["media_type", "format", "encoding"] {
                if let Some(value) = interface[field].as_str() {
                    let _ = write!(line, ", {field} `{value}`");
                }
            }
            let _ = writeln!(body, "{line}");
        }
        body.push('\n');
    }
    let example_role = roles.first().copied().unwrap_or("result");
    let manifest = serde_json::json!({
        "schema_version": "0.2",
        "attempt_id": pins.attempt_id.0.to_string(),
        "objects": [{
            "role": example_role,
            "storage": {"backend": "<as create_upload returned it>", "bucket": "<as returned>", "key": "<as returned>"},
            "size_bytes": 1234,
            "sha256": "<hex SHA-256 of the bytes>",
            "media_type": "application/json"
        }]
    });
    let _ = writeln!(
        body,
        "### Manifest\n\nThe `document` of `post_manifest` (`get_schema` `artifact_manifest`), one object per uploaded file:\n\n{}\n",
        fenced_as(
            "json",
            &serde_json::to_string_pretty(&manifest).unwrap_or_default()
        )
    );
    let metric = metrics.first();
    let claim = metric.map_or_else(String::new, |metric| {
        let split = metric["splits"]
            .as_array()
            .and_then(|splits| splits.first())
            .and_then(Value::as_str)
            .unwrap_or("<split>");
        format!(
            "claims:\n  - metric: {}\n    authority: agent_claim\n    unit: {}\n    direction: {}\n    split: {split}\n    value: 0.0\n",
            metric["key"].as_str().unwrap_or_default(),
            yaml(&metric["unit"]),
            metric["direction"].as_str().unwrap_or_default()
        )
    });
    let _ = writeln!(
        body,
        "### Run document\n\nThe `document` of `submit_attempt`: `provenance.science_revision` must be \"{revision}\", `manifest` the `ref` and `sha256` `post_manifest` returned, `artifact_roles` the roles your notes refer to:\n\n{}\n",
        fenced_as(
            "markdown",
            &format!(
                "---\nprovenance:\n  source_revision: <commit of the code you ran>\n  science_revision: \"{revision}\"\nmanifest:\n  ref: <ref from post_manifest>\n  sha256: <sha256 from post_manifest>\nartifact_roles: [{}]\n{claim}---\n\nWhat you ran and what you saw.",
                roles.join(", ")
            )
        )
    );
    Ok(())
}

/// What the attempt's verify job must produce, from its latest verify job
/// and the science revision the attempt pinned: who runs what, the inputs,
/// the roles its manifest needs, every field of the verification report
/// with the values it must name, and an example completion.
#[allow(
    clippy::too_many_lines,
    reason = "The section follows the order of the steps it describes"
)]
async fn verifying(
    conn: &mut PgConnection,
    pins: &AttemptPins,
    body: &mut String,
    context: &RequestContext,
) -> Result<(), Failure> {
    let jobs = cannery_jobs::repo::list_jobs(
        &mut *conn,
        pins.attempt_id,
        None,
        None,
        cannery_jobs::repo::JsonContext {
            encode_nesting_budget: cannery_core::json::MAX_DEPTH,
            decode_nesting_budget: cannery_core::json::MAX_DEPTH,
        },
    )
    .await
    .map_err(|_| internal(context, "bundle verify jobs"))?;
    body.push_str("## Verifying\n\n");
    let Some(job) = jobs
        .into_iter()
        .filter(|job| job.phase == cannery_jobs::repo::Phase::Verify)
        .max_by_key(|job| job.run_number)
    else {
        body.push_str(
            "The attempt has no verify job: it is verified once its run is submitted.\n\n",
        );
        return Ok(());
    };
    let expected = crate::job_outputs::load_verify(conn, &job)
        .await
        .ok_or_else(|| internal(context, "bundle verify expectations"))?;
    let spec = cannery_core::json::to_value(&job.spec)
        .map_err(|_| internal(context, "bundle verify job"))?;
    let steps: Vec<String> = spec["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|step| {
            format!(
                "`{}` revision {} ({})",
                step["name"].as_str().unwrap_or_default(),
                step["revision"],
                step["manifest"]["spec"]["role"].as_str().unwrap_or("step")
            )
        })
        .collect();
    let number = pins.number;
    let sequence = pins.sequence;
    let _ = writeln!(
        body,
        "Attempt #{number}.{sequence} is verified by job `{}` (run {}), under science revision {}.\n",
        job.id, job.run_number, expected.science_revision
    );
    match expected.performer {
        cannery_jobs::repo::Performer::Agent => {
            let _ = writeln!(
                body,
                "Its performer is `agent`: an agent service account or a researcher who did not run the attempt checks the run's claims against its artifacts, and writes the verification report. You run none of the job's `steps` ({}): they are what a runner verifier would run, and their containers and `cr-evidence` outputs are not yours to produce.\n",
                steps.join(", ")
            );
        }
        cannery_jobs::repo::Performer::Runner => {
            let _ = writeln!(
                body,
                "Its performer is `runner`: the verifier service account `{}` runs the job's steps ({}) under its policy revision `{}` and writes the verification report from the scorer's evidence (`cannery runner`, kind `verify`).\n",
                job.verifier_id.as_deref().unwrap_or_default(),
                steps.join(", "),
                expected.policy_revision
            );
        }
    }
    let roles = &expected.roles;
    let _ = writeln!(
        body,
        "1. Read the inputs with `get_job_input` under the job's lease: `run` (the run document's front matter: its claims and provenance), `manifest` (the run's verified manifest) and `artifacts` (each input artifact with its `download_url`: GET it with the same `Authorization: Bearer` header as the API, or use `get_artifact`). The run notes are not an input: judge the claims against the artifacts.\n\
         2. Heartbeat (`heartbeat_job`) while you work, and upload what you produce under the job's output prefix (`create_job_upload`), one object per file.\n\
         3. Complete the job (`complete_job`) with the verification report and the manifest of your uploads. Only the uploads the manifest lists are the job's outputs. A report the server refuses keeps your lease: correct it and complete again.\n\
         4. If you cannot verify, fail the job (`fail_job`) with a reason. That is not a verdict: the job is queued again for another verifier while the science revision's `max_auto_retries` allows (1 by default), then the attempt fails with a `verify` failure and a researcher decides whether to retry it. A run whose claims do not hold gets a `fail` verdict in a completed report instead.\n"
    );
    let _ = writeln!(
        body,
        "Required output roles (the completion's manifest needs an object of each):{}",
        crate::job_outputs::role_list(roles)
    );
    let datasets = expected.dataset_list();
    let control = expected.control.as_ref().map_or_else(
        || String::from("leave it out: the unit has no control"),
        |(id, revision)| {
            format!("\"{revision}\", the bare revision of the unit's control `{id}` (not `{id}@{revision}`)")
        },
    );
    let dataset = if datasets.is_empty() {
        String::from("leave it out: the scorer reads no registered dataset")
    } else {
        format!(
            "{}, a dataset revision the scorer reads",
            crate::job_outputs::quoted_list(&datasets)
        )
    };
    let source = expected.source_revision.as_str().map_or_else(
        || expected.source_revision.to_string(),
        |text| format!("\"{text}\""),
    );
    let policy = match expected.performer {
        cannery_jobs::repo::Performer::Agent => format!(
            "\"{}\", the pinned science revision, which registers agent verification",
            expected.policy_revision
        ),
        cannery_jobs::repo::Performer::Runner => format!(
            "\"{}\", the policy revision the science revision registers for the verifier",
            expected.policy_revision
        ),
    };
    let (_, science) = pinned_science(conn, pins.attempt_id)
        .await
        .map_err(|()| internal(context, "bundle verify science"))?;
    let slices = crate::job_completion_checks::required_slices(&science);
    let json = |value: &Value| serde_json::to_string(value).unwrap_or_default();
    let required = if slices.is_empty() {
        String::from("No slice is required: `[]` when you measured none.")
    } else {
        format!(
            "Report one measurement of each required slice, whose only dimension is the slice's: {}.",
            slices
                .iter()
                .map(|slice| format!(
                    "`{}` on {} with {} = {}",
                    slice.metric["key"].as_str().unwrap_or_default(),
                    json(slice.split),
                    json(slice.dimension),
                    json(slice.value)
                ))
                .collect::<Vec<_>>()
                .join("; ")
        )
    };
    let mut measured = String::new();
    for slice in &slices {
        let claimed = expected
            .claims
            .as_array()
            .into_iter()
            .flatten()
            .find(|claim| slice.matches(claim))
            .map(|claim| &claim["value"])
            .filter(|value| value.is_number());
        let _ = write!(
            measured,
            "\n  - metric: {}\n    split: {}\n    dimensions: {{{}: {}}}\n    authority: tester_verified\n    unit: {}\n    direction: {}\n    {}",
            json(&slice.metric["key"]),
            json(slice.split),
            json(slice.dimension),
            json(slice.value),
            json(&slice.metric["unit"]),
            json(&slice.metric["direction"]),
            claimed.map_or_else(
                || String::from("missing_reason: <why you could not measure it>"),
                |value| format!("value: {value}")
            )
        );
    }
    if measured.is_empty() {
        measured.push_str(" []");
    }
    let _ = writeln!(
        body,
        "### The verification report\n\n\
         Markdown with YAML front matter (`get_schema` `verification`) and your observations as an optional body (at most the science revision's `limits.report_max_bytes`). The front matter:\n\n\
         - `verdict`: `pass`, `fail` or `inconclusive`. A `pass` needs every gate passed; `inconclusive` when the artifacts cannot settle the claims.\n\
         - `reason`: why this verdict, in a sentence or two.\n\
         - `policy_revision`: {policy}.\n\
         - `gates`: at least one `{{id, result, detail}}`: each check you applied (a slug), its `result` (`pass`, `fail` or `unknown`) and, in `detail`, the values it used.\n\
         - `measurements`: the metric values you measured yourself, each with `metric`, `authority: tester_verified`, the metric's `unit` and `direction`, `split`, `dimensions` for a slice, and `value` (or `missing_reason`, never a zero). {required}\n\
         - `discrepancies`: where a claim disagrees with what you measured: `metric`, `split`, `dimensions`, `claimed_value`, `verified_value` and a `description`.\n\
         - `comparisons`: what you compared, one per metric, split and slice: the `value` (one of your measurements, `source: tester`) and the `reference` it was compared against (`value`, `label`, `kind`).\n\
         - `provenance`: `science_revision` \"{}\"; `source_revision` {source}, the run's; `control_revision` {control}; `dataset_revision` {dataset}.\n\
         - `artifact_roles`: the roles of your manifest the report refers to.\n",
        expected.science_revision
    );
    let mut provenance = format!(
        "  science_revision: \"{}\"\n  source_revision: {source}\n",
        expected.science_revision
    );
    if let Some((_, revision)) = &expected.control {
        let _ = writeln!(provenance, "  control_revision: \"{revision}\"");
    }
    if let Some(revision) = datasets.first() {
        let _ = writeln!(provenance, "  dataset_revision: \"{revision}\"");
    }
    let names = crate::job_outputs::names(roles);
    let report = format!(
        "---\nverdict: pass\nreason: Every claim matches what the artifacts show.\npolicy_revision: \"{}\"\ngates:\n  - id: claims-reproduce\n    result: pass\n    detail: <the values the check used>\nmeasurements:{measured}\ndiscrepancies: []\nprovenance:\n{provenance}artifact_roles: [{}]\n---\n\nWhat you checked and what you saw.",
        expected.policy_revision,
        names.join(", ")
    );
    let completion = serde_json::json!({
        "schema_version": "0.2",
        "job_id": job.id.to_string(),
        "document": report,
        "manifest": {
            "schema_version": "0.2",
            "attempt_id": pins.attempt_id.0.to_string(),
            "objects": names.iter().map(|role| serde_json::json!({
                "role": role,
                "storage": {"backend": "<as create_job_upload returned it>", "bucket": "<as returned>", "key": "<as returned>"},
                "size_bytes": 1234,
                "sha256": "<hex SHA-256 of the bytes>",
                "media_type": "text/plain"
            })).collect::<Vec<_>>()
        }
    });
    let _ = writeln!(
        body,
        "### Completion\n\nThe `document` argument of `complete_job` (`get_schema` `job_completion`): the report above as one string, and the manifest of your uploads. For example (each measured value in it is the run's claim: replace it with what you measured):\n\n{}\n\nThe report as Markdown, for reading:\n\n{}\n",
        fenced_as(
            "json",
            &serde_json::to_string_pretty(&completion).unwrap_or_default()
        ),
        fenced_as("markdown", &report)
    );
    Ok(())
}

/// Build an attempt's bundle with the given detail.
#[allow(
    clippy::too_many_lines,
    reason = "The bundle's sections are written in the order a performer reads them"
)]
pub(crate) async fn build_for(
    conn: &mut PgConnection,
    project: &projects::Project,
    pins: &AttemptPins,
    detail: Detail,
    context: &RequestContext,
) -> Result<String, Failure> {
    let compact = detail == Detail::Compact;
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
    let stored = plans::unit_revision(conn, pins.unit_id, pins.unit_revision)
        .await
        .map_err(persistence(context))?
        .ok_or_else(|| internal(context, "bundle unit revision"))?;
    let document: Value =
        serde_json::from_str(&stored.content).map_err(|_| internal(context, "bundle unit"))?;
    let mut fields = plan_units::fields_from_unit(&document);
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
            if entry.unit_id == Some(pins.unit_id) {
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
        "- Unit revision: {}\n\n### Acceptance\n\n```json\n{}\n```\n",
        pins.unit_revision,
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
    if matches!(detail, Detail::Full | Detail::Compact) {
        submitting(conn, pins, compact, &mut body, context).await?;
    }
    if detail == Detail::Verify {
        verifying(conn, pins, &mut body, context).await?;
    }
    let index = plans::track_units(conn, pins.track_id, None, None, i64::MAX)
        .await
        .map_err(persistence(context))?;
    let others: Vec<_> = index
        .iter()
        .rev()
        .filter(|unit| unit.unit_id != pins.unit_id)
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
            .filter_map(|relation| relation.unit.as_i64())
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
                        match plans::latest_output(conn, unit.unit_id)
                            .await
                            .map_err(persistence(context))?
                        {
                            Some((sequence, state, text)) => format!(
                                "#{number} {} ({}): #{number}.{sequence} {state}: {} \
                                 (/api/projects/{slug}/units/{number}/writeup; the run's report: \
                                 /api/projects/{slug}/units/{number}/attempts/{sequence}/report)",
                                unit.title,
                                unit.state,
                                text.map(|text| first_line(&text)).unwrap_or_default(),
                                slug = project.slug
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
    if matches!(detail, Detail::Full | Detail::Compact) {
        let line_max = compact.then(|| limit(limits.context_summary_max_bytes));
        earlier_attempts(conn, project, pins, line_max, &mut body, context).await?;
        earlier_questions(conn, project, pins, line_max, &mut body, context).await?;
    }
    if matches!(detail, Detail::Document | Detail::Decide) {
        record(conn, project, pins, detail, &mut body, context).await?;
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
        let _ = writeln!(header, "unit_revision: {}", pins.unit_revision);
        let _ = writeln!(header, "brief_revision: {}", yaml(&pins.brief_revision));
        let _ = writeln!(header, "plan_revision: {}", yaml(&pins.plan_revision));
        let _ = writeln!(
            header,
            "detail: {}",
            match detail {
                Detail::Full => "full",
                Detail::Compact => "compact",
                Detail::Verify => "verify",
                Detail::Document => "document",
                Detail::Decide => "decide",
            }
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

/// The bundle a query asks for: `detail` (`full` or `compact`) or `phase`
/// (`document` or `decide`), at most one of each, and not a compact
/// documenter's or decider's bundle.
fn detail(query: &str) -> Result<Detail, Failure> {
    let mut detail = None;
    let mut phase = None;
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        match name {
            "detail" if detail.is_none() => {
                detail = Some(match value {
                    "full" => false,
                    "compact" => true,
                    _ => {
                        return Err(crate::plan_routes::invalid(
                            "query/detail",
                            "Input should be 'full' or 'compact'",
                        ));
                    }
                });
            }
            "phase" if phase.is_none() => {
                phase = Some(match value {
                    "verify" => Detail::Verify,
                    "document" => Detail::Document,
                    "decide" => Detail::Decide,
                    _ => {
                        return Err(crate::plan_routes::invalid(
                            "query/phase",
                            "Input should be 'verify', 'document' or 'decide'",
                        ));
                    }
                });
            }
            _ => {
                return Err(crate::plan_routes::invalid(
                    &format!("query/{name}"),
                    "Name detail ('full' or 'compact') or phase ('verify', 'document' or 'decide'), each at most once",
                ));
            }
        }
    }
    match (phase, detail) {
        (Some(_), Some(true)) => Err(crate::plan_routes::invalid(
            "query/detail",
            "The verifier's, the documenter's and the decider's bundles have no compact form",
        )),
        (Some(phase), _) => Ok(phase),
        (None, Some(true)) => Ok(Detail::Compact),
        (None, _) => Ok(Detail::Full),
    }
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/units/{number}/attempts/{sequence}/context.md",
    operation_id = "get_context_api_projects__slug__units__number__attempts__sequence__context_md_get",
    summary = "Get Context Bundle",
    description = "The attempt's context bundle as Markdown, assembled from the revisions it\npinned at its claim: the brief, the plan's approach, the unit's fields and\nbrief, an index of the track's other units, and a summary line and\nreference for each context item and each unit it derives from; what to\nsubmit (the required artifact roles, the metrics, datasets and interfaces\nof the pinned science revision, an example manifest and run document); and\nhow the unit's earlier attempts ended, with the decisions' reasons, their\nsteering notes, questions and answers. The front matter states its size in\nbytes. `detail=compact` keeps the brief's goal, the unit, what to submit,\nthe index and the earlier attempts, capped at 16 KiB. Over MCP:\n`get_context`. `phase=verify` is the\nverifier's bundle: the full bundle with, instead of what to submit, what\nthe attempt's verify job must produce (who runs which steps, the inputs,\nthe roles its manifest needs, each field of the verification report\nwith the values it must name, and an example completion). `phase=document` is the\ndocumenter's bundle: the full bundle and the unit's record, every\nattempt's run document and notes, failures and their logs, verification\nreports and the comments. `phase=decide` adds the write-up, or why there\nis none.",
    params(("slug" = String, Path), ("number" = i64, Path), ("sequence" = i64, Path),
        ("detail" = Option<String>, Query, description = "`full` (the default) or `compact`."),
        ("phase" = Option<String>, Query, description = "`verify`, `document` or `decide`: the verifier's, the documenter's or the decider's bundle.")),
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
    let detail = detail(parts.uri.query().unwrap_or_default())?;
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
    let bundle = build_for(&mut auth.connection, &project, &pins, detail, &context).await?;
    Ok((
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        bundle,
    )
        .into_response())
}
