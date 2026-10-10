// SPDX-License-Identifier: AGPL-3.0-only
//! Questions, answers and steering notes. The performer of a unit's phase
//! (an agent holding an attempt in agent mode, or whoever holds a job) asks
//! a question about its unit; a blocking one stops its lease clock until a
//! researcher answers it or escalates it into a concern about the track's
//! plan, and a non-blocking one names the default the performer proceeds
//! on. A researcher also posts steering notes to a running attempt. The
//! performer reads answers and notes in its heartbeat responses, waits for
//! an answer with `wait_for_answer`, and acknowledges what it read.
use crate::{
    AppState,
    api_models::{
        AcknowledgementIn, AcknowledgementOut, AnswerIn, EscalationIn, MessageOut, MessagesOut,
        Page_MessageOut_UUID_, QuestionAnswerOut, QuestionAsk, SteeringIn,
    },
    attempt_lease_routes,
    authentication::authenticate,
    body::DecodedBody,
    errors::ApiError,
    job_workers,
    plan_routes::channel_name,
    request_context::first_header,
    requests::RequestContext,
    unit_routes::{Failure, domain, internal},
};
use axum::{
    Json, Router,
    extract::{FromRequestParts, Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use cannery_attempts::{
    model::{Attempt, Mode, State as AttemptState},
    repo::Repository,
};
use cannery_core::{
    audit::{self, Attribution, Record},
    errors::{DomainError, ErrorCode},
    ids::{AttemptId, JobId, MessageId, UnitId},
    principal::{Principal, Role},
    timestamps::Timestamp,
};
use cannery_jobs::repo::{self as jobs, Job, State as JobState};
use cannery_projects::{authz, repo as projects};
use cannery_tracks::{
    concerns,
    messages::{self, Filter, Message, NewMessage},
    plans,
};
use num_bigint::BigInt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Acquire, PgConnection};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

/// The largest message body, in UTF-8 bytes.
pub const MESSAGE_BODY_MAX_BYTES: usize = 16_384;
/// The largest default of a non-blocking question, in UTF-8 bytes.
pub const DEFAULT_MAX_BYTES: usize = 4000;
/// The largest note of an escalation, in UTF-8 bytes.
pub const ESCALATION_NOTE_MAX_BYTES: usize = 8000;
/// How long `wait_for_answer` waits when the caller names no time.
const WAIT_SECONDS: u64 = 30;
/// The longest `wait_for_answer` waits.
const WAIT_MAX_SECONDS: u64 = 60;
const PAGE: i64 = 50;
const STATES: [&str; 3] = ["open", "answered", "escalated"];
const CONCERN_KINDS: [&str; 4] = ["wrong_assumption", "better_idea", "blocker", "other"];

/// The JSON profiles of the attempts and jobs a question is asked under,
/// and the store and schema of transcripts.
pub struct MessageContext {
    pub attempts: cannery_attempts::model::JsonContext,
    pub jobs: jobs::JsonContext,
    /// Where transcripts are stored.
    pub store: Arc<cannery_storage::ObjectStore>,
    /// Validates each transcript event.
    pub contracts: cannery_core::contracts::ContractValidator,
}

#[derive(Clone)]
pub(crate) struct RouteState {
    pub(crate) app: AppState,
    pub(crate) context: Arc<MessageContext>,
}

pub(crate) fn routes(app: AppState, context: Arc<MessageContext>) -> Router {
    Router::new()
        .route(
            "/api/projects/{slug}/units/{number}/attempts/{sequence}/questions",
            post(ask),
        )
        .route(
            "/api/projects/{slug}/jobs/{job_id}/questions",
            post(ask_job),
        )
        .route("/api/projects/{slug}/questions", get(list_questions))
        .route(
            "/api/projects/{slug}/questions/{question_id}",
            get(get_question),
        )
        .route(
            "/api/projects/{slug}/questions/{question_id}/answer",
            get(wait_for_answer).post(answer),
        )
        .route(
            "/api/projects/{slug}/questions/{question_id}/escalation",
            post(escalate),
        )
        .route(
            "/api/projects/{slug}/units/{number}/attempts/{sequence}/steering",
            get(get_steering).post(post_steering),
        )
        .route(
            "/api/projects/{slug}/messages/acknowledgements",
            post(acknowledge),
        )
        .route(
            "/api/projects/{slug}/units/{number}/messages",
            get(list_messages),
        )
        .route(
            "/api/projects/{slug}/units/{number}/attempts/{sequence}/transcript",
            get(crate::transcript_routes::read).post(crate::transcript_routes::append),
        )
        .with_state(RouteState { app, context })
}

// ---------------------------------------------------------------- helpers

async fn paths(
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

pub(crate) fn decode<T: serde::de::DeserializeOwned>(body: &DecodedBody) -> Result<T, Failure> {
    let value = match body {
        DecodedBody::Json(document) => cannery_core::json::to_value(document)
            .map_err(|_| invalid("body", "the body is not valid JSON"))?,
        DecodedBody::Missing => return Err(invalid("body", "Field required")),
        DecodedBody::RawBytes => return Err(invalid("body", "the body must be JSON")),
    };
    serde_json::from_value(value).map_err(|error| invalid("body", error.to_string()))
}

pub(crate) fn query_value<'a>(
    parts: &'a axum::http::request::Parts,
    name: &str,
) -> Option<&'a str> {
    parts.uri.query().and_then(|query| {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (key == name).then_some(value)
        })
    })
}

pub(crate) fn persistence(
    context: &RequestContext,
) -> impl Fn(cannery_tracks::repo::TrackError) -> Failure {
    move |_| internal(context, "message persistence")
}

fn project_failure(context: &RequestContext) -> impl Fn(cannery_projects::ProjectError) -> Failure {
    move |error| Failure::new(context.project_error(error))
}

/// A Markdown text that is not blank and at most `max` bytes.
fn text(path: &str, value: &str, max: usize) -> Result<(), Failure> {
    if value.trim().is_empty() {
        return Err(invalid(path, "the text is empty"));
    }
    if value.len() > max {
        return Err(invalid(
            path,
            format!("the text is {} bytes; at most {max}", value.len()),
        ));
    }
    Ok(())
}

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

fn question_id(paths: &BTreeMap<String, String>) -> Result<MessageId, Failure> {
    paths
        .get("question_id")
        .and_then(|value| value.parse::<MessageId>().ok())
        .ok_or_else(|| invalid("path/question_id", "Input should be a UUID"))
}

pub(crate) fn idempotency_key(
    parts: &axum::http::request::Parts,
) -> Result<Option<String>, Failure> {
    let key = first_header(&parts.headers, "idempotency-key");
    if key
        .as_ref()
        .is_some_and(|key| !(1..=200).contains(&key.chars().count()))
    {
        return Err(invalid(
            "header/Idempotency-Key",
            "Idempotency-Key must be 1 to 200 characters",
        ));
    }
    Ok(key)
}

fn actor(principal: &Principal) -> String {
    match principal {
        Principal::User(user) => format!("user:{}", user.user_id.0),
        Principal::Service(service) => format!("service:{}", service.service_account_id.0),
    }
}

/// The message an earlier request with the same `Idempotency-Key` wrote,
/// under a transaction-scoped lock of the key.
pub(crate) async fn replayed(
    conn: &mut PgConnection,
    scope: &str,
    principal: &Principal,
    key: &str,
    hash: &[u8],
    context: &RequestContext,
) -> Result<Option<String>, Failure> {
    let actor = actor(principal);
    let lock = format!("{scope}\n{actor}\n{key}");
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        lock
    )
    .execute(&mut *conn)
    .await
    .map_err(|_| internal(context, "message replay lock"))?;
    let row = sqlx::query!(
        "SELECT request_hash, result_id FROM idempotency_keys WHERE scope = $1 AND actor = $2 AND key = $3",
        scope,
        actor,
        key
    )
    .fetch_optional(conn)
    .await
    .map_err(|_| internal(context, "message replay lookup"))?;
    match row {
        Some(row) if row.request_hash != hash => Err(domain(
            ErrorCode::Conflict,
            "this Idempotency-Key was already used with a different request",
        )),
        Some(row) => Ok(Some(row.result_id)),
        None => Ok(None),
    }
}

pub(crate) async fn remember(
    conn: &mut PgConnection,
    scope: &str,
    principal: &Principal,
    key: &str,
    hash: &[u8],
    result: &str,
    context: &RequestContext,
) -> Result<(), Failure> {
    let actor = actor(principal);
    sqlx::query!(
        "INSERT INTO idempotency_keys (scope, actor, key, request_hash, result_id) VALUES ($1, $2, $3, $4, $5)",
        scope,
        actor,
        key,
        hash,
        result
    )
    .execute(conn)
    .await
    .map_err(|_| internal(context, "message replay storage"))?;
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "One audit row: the subject, its action, its new state and the reason, under the caller's connection"
)]
async fn record(
    conn: &mut PgConnection,
    principal: &Principal,
    project: &projects::Project,
    subject_type: &'static str,
    subject: &str,
    action: &'static str,
    new_state: &Value,
    idempotency_key: Option<&str>,
    context: &RequestContext,
) -> Result<(), Failure> {
    audit::record(
        conn,
        Attribution::Principal(principal),
        Record {
            action,
            subject_type,
            subject_id: subject,
            project_id: Some(project.id),
            prior_state: None,
            new_state: Some(new_state),
            reason: None,
            idempotency_key,
        },
    )
    .await
    .map(|_| ())
    .map_err(|_| internal(context, "message audit"))
}

/// A message as the API shows it.
pub(crate) fn message_out(message: Message) -> MessageOut {
    let (author_kind, author) = match (message.author_user, message.author_service) {
        (Some(user), _) => ("user", user.to_string()),
        (None, Some(service)) => ("service", service.to_string()),
        (None, None) => ("user", String::new()),
    };
    let answer = match (
        message.answer_id,
        message.answer_body,
        message.answer_created_at,
    ) {
        (Some(id), Some(body), Some(created_at)) => Some(QuestionAnswerOut {
            id: id.to_string(),
            body,
            author: message
                .answer_author
                .map(|user| user.to_string())
                .unwrap_or_default(),
            author_name: message.answer_author_name,
            created_at: crate::timestamps::public_timestamp(created_at),
            acknowledged_at: message
                .answer_acknowledged_at
                .map(crate::timestamps::public_timestamp),
        }),
        _ => None,
    };
    MessageOut {
        id: message.id.to_string(),
        track: message.track_slug,
        unit: i64::from(message.unit_number),
        attempt: i64::from(message.attempt_sequence),
        job: message.job_id.map(|job| job.to_string()),
        job_phase: message.job_phase,
        kind: message.kind,
        blocking: message.blocking,
        default: message.default_text,
        body: message.body,
        sha256: message.sha256,
        question: message.question_id.map(|id| id.to_string()),
        author_kind: author_kind.to_owned(),
        author,
        author_name: message.author_name,
        via_channel: message.via_channel,
        via_client: message.via_client,
        created_at: crate::timestamps::public_timestamp(message.created_at),
        state: message.state,
        closed_at: message.closed_at.map(crate::timestamps::public_timestamp),
        concern: message.concern_id.map(|id| id.to_string()),
        released_at: message.released_at.map(crate::timestamps::public_timestamp),
        acknowledged_at: message
            .acknowledged_at
            .map(crate::timestamps::public_timestamp),
        answer,
    }
}

/// What a performer has not acknowledged yet, split into answers and
/// steering notes, as heartbeat responses carry them.
pub(crate) async fn pending(
    conn: &mut PgConnection,
    project: &projects::Project,
    attempt: AttemptId,
    job: Option<JobId>,
    context: &RequestContext,
) -> Result<(Vec<MessageOut>, Vec<MessageOut>), Failure> {
    let found = messages::pending(conn, project.id, attempt, job)
        .await
        .map_err(persistence(context))?;
    let (answers, steering): (Vec<_>, Vec<_>) = found
        .into_iter()
        .map(message_out)
        .partition(|message| message.kind == "answer");
    Ok((answers, steering))
}

/// Lease seconds as a number of seconds for SQL intervals.
fn seconds(value: &BigInt) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(60.0)
}

/// Restart the lease clock of the attempt or job a closed blocking question
/// stopped, once no other blocking question holds it.
async fn resume(
    conn: &mut PgConnection,
    state: &RouteState,
    question: &Message,
    context: &RequestContext,
) -> Result<(), Failure> {
    if question.blocking != Some(true) {
        return Ok(());
    }
    let leases = &state.app.settings.leases;
    match question.job_id {
        Some(job) => {
            messages::resume_job(conn, job, seconds(leases.job_ttl_seconds.as_bigint()))
                .await
                .map_err(persistence(context))?;
        }
        None => {
            messages::resume_attempt(
                conn,
                question.attempt_id,
                seconds(leases.ttl_seconds.as_bigint()),
            )
            .await
            .map_err(persistence(context))?;
        }
    }
    Ok(())
}

/// The caller and the project, for a researcher's answer, escalation or
/// steering note. Service accounts are refused.
async fn researcher(
    state: &RouteState,
    context: &RequestContext,
    parts: &mut axum::http::request::Parts,
    what: &str,
) -> Result<
    (
        crate::authentication::Authenticated,
        projects::Project,
        BTreeMap<String, String>,
    ),
    Failure,
> {
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
        return Err(domain(
            ErrorCode::Forbidden,
            format!("only researchers {what}"),
        ));
    }
    Ok((auth, project, paths))
}

/// The caller and the project, for a read.
async fn reader(
    state: &RouteState,
    context: &RequestContext,
    parts: &mut axum::http::request::Parts,
) -> Result<
    (
        crate::authentication::Authenticated,
        projects::Project,
        BTreeMap<String, String>,
    ),
    Failure,
> {
    let mut auth = authenticate(&state.app, context, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let paths = paths(parts, state).await?;
    let project = authz::project_read(&mut auth.connection, &auth.principal, &paths["slug"])
        .await
        .map_err(project_failure(context))?;
    Ok((auth, project, paths))
}

/// The checked question: its body and, for a non-blocking one, the
/// default the performer proceeds on.
fn checked_question(input: &QuestionAsk) -> Result<(), Failure> {
    text("body/body", &input.body, MESSAGE_BODY_MAX_BYTES)?;
    match input.default.as_deref() {
        Some(default) => text("body/default", default, DEFAULT_MAX_BYTES),
        None if input.blocking => Ok(()),
        None => Err(invalid(
            "body/default",
            "a non-blocking question states the default you proceed on until it is answered",
        )),
    }
}

fn request_hash(scope: &str, slug: &str, holder: &str, input: &QuestionAsk) -> Vec<u8> {
    let canonical = json!({
        "scope": scope, "project": slug, "holder": holder,
        "body": input.body, "blocking": input.blocking, "default": input.default,
    });
    Sha256::digest(canonical.to_string().as_bytes()).to_vec()
}

struct Asked<'a> {
    project: &'a projects::Project,
    unit: UnitId,
    attempt: AttemptId,
    job: Option<JobId>,
    input: &'a QuestionAsk,
    key: Option<&'a str>,
    hash: &'a [u8],
    scope: &'static str,
}

/// Store a question and stop its performer's lease clock when it blocks.
async fn store_question(
    conn: &mut PgConnection,
    principal: &Principal,
    asked: Asked<'_>,
    context: &RequestContext,
) -> Result<MessageOut, Failure> {
    let (user, service, via) = match principal {
        Principal::User(user) => (Some(user.user_id), None, user.via.clone()),
        Principal::Service(service) => {
            (None, Some(service.service_account_id), service.via.clone())
        }
    };
    let input = asked.input;
    let sha256 = format!("{:x}", Sha256::digest(input.body.as_bytes()));
    let id = messages::insert(
        conn,
        NewMessage {
            project_id: asked.project.id,
            unit_id: asked.unit,
            attempt_id: asked.attempt,
            job_id: asked.job,
            kind: "question",
            blocking: Some(input.blocking),
            default_text: input.default.as_deref(),
            body: &input.body,
            sha256: &sha256,
            question_id: None,
            author_user: user,
            author_service: service,
            via_channel: channel_name(via.channel),
            via_client: via.client.as_deref(),
        },
    )
    .await
    .map_err(persistence(context))?;
    if input.blocking {
        match asked.job {
            Some(job) => messages::pause_job(conn, job).await,
            None => messages::pause_attempt(conn, asked.attempt).await,
        }
        .map_err(persistence(context))?;
    }
    let message = messages::get(conn, asked.project.id, id, false)
        .await
        .map_err(persistence(context))?
        .ok_or_else(|| internal(context, "asked question"))?;
    let new_state = json!({
        "unit": message.unit_number, "attempt": message.attempt_sequence,
        "job": asked.job.map(|job| job.to_string()), "blocking": input.blocking,
        "state": "open", "sha256": sha256,
    });
    record(
        conn,
        principal,
        asked.project,
        "message",
        &id.to_string(),
        "question.asked",
        &new_state,
        asked.key,
        context,
    )
    .await?;
    if let Some(key) = asked.key {
        remember(
            conn,
            asked.scope,
            principal,
            key,
            asked.hash,
            &id.to_string(),
            context,
        )
        .await?;
    }
    Ok(message_out(message))
}

/// The answer to a replayed request: the message it wrote, marked a replay.
async fn replay_response(
    conn: &mut PgConnection,
    project: &projects::Project,
    id: &str,
    context: &RequestContext,
) -> Result<Response, Failure> {
    let id = id
        .parse::<MessageId>()
        .map_err(|_| internal(context, "message replay id"))?;
    let message = messages::get(conn, project.id, id, false)
        .await
        .map_err(persistence(context))?
        .ok_or_else(|| internal(context, "message replay result"))?;
    let mut response = (StatusCode::OK, Json(message_out(message))).into_response();
    response
        .extensions_mut()
        .insert(crate::mcp::ReplayOutcome(true));
    Ok(response)
}

// ------------------------------------------------------------------ routes

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/units/{number}/attempts/{sequence}/questions",
    operation_id = "ask_api_projects__slug__units__number__attempts__sequence__questions_post",
    summary = "Ask",
    description = "Ask a question about the unit under the attempt's lease (agent mode only):\nwhat you need, as Markdown (at most 16 KiB). A blocking question puts the\nattempt in `waiting_on_human` and stops its lease clock: neither the lease\nnor the deadline expires while it waits, and a fresh lease starts once a\nresearcher answers it. Unanswered after the project's\n`question_wait_seconds`, the attempt is released (`unanswered_question`)\nand the unit queued again, the question attached for the next attempt. A\nnon-blocking question states the `default` you proceed on until it is\nanswered. Read the answer with `wait_for_answer` or in the next heartbeat\nresponse. With an `Idempotency-Key`, a retry returns the same question.",
    params(("slug" = String, Path),
        ("number" = i64, Path),
        ("sequence" = i64, Path),
        ("X-Lease-Token" = Option<String>, Header),
        ("X-Lease-Generation" = Option<i64>, Header),
        ("Idempotency-Key" = Option<String>, Header)),
    request_body(content = crate::api_models::QuestionAsk, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::MessageOut, content_type = "application/json"),
        (status = 200, description = "Replayed", body = crate::api_models::MessageOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn ask(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let paths = paths(&mut parts, &state).await?;
    let parameters =
        attempt_lease_routes::parameters(&paths, &parts.headers, &context).map_err(Failure::new)?;
    let input: QuestionAsk = decode(&body)?;
    checked_question(&input)?;
    let key = idempotency_key(&parts)?;
    let project = attempt_lease_routes::worker_access(
        &mut auth.connection,
        &auth.principal,
        &paths["slug"],
        &context,
    )
    .await
    .map_err(Failure::new)?;
    let hash = request_hash(
        "message.ask",
        &project.slug,
        &parameters.reference(),
        &input,
    );
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "question transaction"))?;
    if let Some(key) = key.as_deref()
        && let Some(id) = replayed(
            &mut tx,
            "message.ask",
            &auth.principal,
            key,
            &hash,
            &context,
        )
        .await?
    {
        let response = replay_response(&mut tx, &project, &id, &context).await?;
        tx.commit()
            .await
            .map_err(|_| internal(&context, "question commit"))?;
        return Ok(response);
    }
    let attempt = attempt_lease_routes::leased_or_waiting(
        &mut tx,
        &auth.principal,
        &project,
        &parameters,
        state.context.attempts,
        &context,
    )
    .await
    .map_err(Failure::new)?;
    if attempt.mode() != Mode::Agent {
        return Err(domain(
            ErrorCode::Conflict,
            "a workflow attempt's steps ask no questions; raise a concern or release the attempt",
        ));
    }
    let out = store_question(
        &mut tx,
        &auth.principal,
        Asked {
            project: &project,
            unit: attempt.unit_id,
            attempt: attempt.id,
            job: None,
            input: &input,
            key: key.as_deref(),
            hash: &hash,
            scope: "message.ask",
        },
        &context,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|_| internal(&context, "question commit"))?;
    Ok((StatusCode::CREATED, Json(out)).into_response())
}

/// The job a caller holds the lease of, and whether a blocking question
/// stopped its lease clock: then neither its lease nor its deadline
/// expires.
pub(crate) async fn leased_job(
    conn: &mut PgConnection,
    principal: &Principal,
    project: &projects::Project,
    id: JobId,
    headers: &axum::http::HeaderMap,
    profile: jobs::JsonContext,
    context: &RequestContext,
) -> Result<(Job, bool), Failure> {
    let generation = first_header(headers, "x-lease-generation")
        .map(|value| {
            value
                .parse::<i64>()
                .map_err(|_| invalid("header/X-Lease-Generation", "Input should be an integer"))
        })
        .transpose()?;
    let job = jobs::get_job(conn, id, true, profile)
        .await
        .map_err(|_| internal(context, "leased job lookup"))?
        .filter(|job| job.project_id == project.id)
        .ok_or_else(|| domain(ErrorCode::NotFound, "job not found"))?;
    job_workers::holder(&job, principal).map_err(Failure::new)?;
    let token = first_header(headers, "x-lease-token");
    let (Some(token), Some(generation)) = (token, generation) else {
        return Err(domain(
            ErrorCode::StaleLease,
            "send X-Lease-Token and X-Lease-Generation",
        ));
    };
    if job.state != JobState::Claimed
        || !job
            .lease_token_hash
            .as_ref()
            .is_some_and(|held| cannery_identity::secrets::matches_digest(held, &token))
        || generation != i64::from(job.lease_generation)
    {
        return Err(domain(
            ErrorCode::StaleLease,
            "this lease is no longer valid for the job",
        ));
    }
    let paused = messages::job_paused_at(conn, job.id)
        .await
        .map_err(persistence(context))?
        .is_some();
    if !paused {
        let now: Timestamp = sqlx::query_scalar("SELECT now()")
            .fetch_one(&mut *conn)
            .await
            .map_err(|_| internal(context, "job lease clock"))?;
        if job.deadline.is_none_or(|time| time.0 <= now.0) {
            return Err(domain(ErrorCode::StaleLease, "the job's deadline passed"));
        }
        if job.lease_expires_at.is_none_or(|time| time.0 <= now.0) {
            return Err(domain(ErrorCode::StaleLease, "the lease expired"));
        }
    }
    Ok((job, paused))
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/jobs/{job_id}/questions",
    operation_id = "ask_job_api_projects__slug__jobs__job_id__questions_post",
    summary = "Ask Job",
    description = "Ask a question about the job's unit under the job's lease, as `ask` does\nfor an attempt. A blocking question stops the job's lease clock until a\nresearcher answers it; unanswered after the project's\n`question_wait_seconds`, the job fails (`unanswered_question`) as an\nexpired one does.",
    params(("slug" = String, Path),
        ("job_id" = String, Path, format = "uuid"),
        ("X-Lease-Token" = Option<String>, Header),
        ("X-Lease-Generation" = Option<i64>, Header),
        ("Idempotency-Key" = Option<String>, Header)),
    request_body(content = crate::api_models::QuestionAsk, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::MessageOut, content_type = "application/json"),
        (status = 200, description = "Replayed", body = crate::api_models::MessageOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn ask_job(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let paths = paths(&mut parts, &state).await?;
    let id = paths["job_id"]
        .parse::<JobId>()
        .map_err(|_| invalid("path/job_id", "Input should be a UUID"))?;
    let input: QuestionAsk = decode(&body)?;
    checked_question(&input)?;
    let key = idempotency_key(&parts)?;
    let project = job_workers::worker(
        &mut auth.connection,
        &auth.principal,
        &paths["slug"],
        &context,
    )
    .await
    .map_err(Failure::new)?;
    let hash = request_hash("message.ask_job", &project.slug, &id.to_string(), &input);
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "question transaction"))?;
    if let Some(key) = key.as_deref()
        && let Some(found) = replayed(
            &mut tx,
            "message.ask_job",
            &auth.principal,
            key,
            &hash,
            &context,
        )
        .await?
    {
        let response = replay_response(&mut tx, &project, &found, &context).await?;
        tx.commit()
            .await
            .map_err(|_| internal(&context, "question commit"))?;
        return Ok(response);
    }
    let (job, _) = leased_job(
        &mut tx,
        &auth.principal,
        &project,
        id,
        &parts.headers,
        state.context.jobs,
        &context,
    )
    .await?;
    let attempt = Repository::new(&mut tx, state.context.attempts)
        .get_attempt_by_id(job.attempt_id, false)
        .await
        .map_err(|_| internal(&context, "question job attempt"))?
        .ok_or_else(|| internal(&context, "question job attempt invariant"))?;
    let out = store_question(
        &mut tx,
        &auth.principal,
        Asked {
            project: &project,
            unit: attempt.unit_id,
            attempt: attempt.id,
            job: Some(job.id),
            input: &input,
            key: key.as_deref(),
            hash: &hash,
            scope: "message.ask_job",
        },
        &context,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|_| internal(&context, "question commit"))?;
    Ok((StatusCode::CREATED, Json(out)).into_response())
}

/// `before` and `limit` of a message list.
fn page_query(parts: &axum::http::request::Parts) -> Result<(Option<MessageId>, i64), Failure> {
    let before = query_value(parts, "before")
        .map(|value| {
            value
                .parse::<MessageId>()
                .map_err(|_| invalid("query/before", "Input should be a UUID"))
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

async fn page(
    conn: &mut PgConnection,
    project: &projects::Project,
    filter: Filter<'_>,
    limit: i64,
    context: &RequestContext,
) -> Result<Page_MessageOut_UUID_, Failure> {
    let mut rows = messages::select(conn, project.id, filter, limit + 1)
        .await
        .map_err(persistence(context))?;
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let next_before = if rows.len() > limit {
        rows.truncate(limit);
        rows.last().map(|row| row.id.to_string())
    } else {
        None
    };
    Ok(Page_MessageOut_UUID_ {
        items: rows.into_iter().map(message_out).collect(),
        next_before,
    })
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/questions",
    operation_id = "list_questions_api_projects__slug__questions_get",
    summary = "List Questions",
    description = "The questions performers asked about the project's units, newest first,\nwith their answers: of one track when `track` is given. `state=open`\nlists those waiting for a researcher.",
    params(("slug" = String, Path),
        ("track" = Option<String>, Query, description = "Only questions about units of this track."),
        ("state" = Option<String>, Query, description = "Only questions in this state: `open`, `answered` or `escalated`."),
        ("before" = Option<String>, Query, description = "Continue before this question.", format = "uuid"),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_MessageOut_UUID_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn list_questions(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let (mut auth, project, _) = reader(&state, &context, &mut parts).await?;
    let question_state = query_value(&parts, "state")
        .map(|value| {
            STATES
                .into_iter()
                .find(|state| *state == value)
                .ok_or_else(|| {
                    invalid(
                        "query/state",
                        "Input should be 'open', 'answered' or 'escalated'",
                    )
                })
        })
        .transpose()?;
    let (before, limit) = page_query(&parts)?;
    let track = query_value(&parts, "track");
    let filter = Filter {
        track,
        kind: Some("question"),
        state: question_state,
        before,
        ..Filter::default()
    };
    let out = page(&mut auth.connection, &project, filter, limit, &context).await?;
    Ok(Json(out).into_response())
}

async fn load_question(
    conn: &mut PgConnection,
    project: &projects::Project,
    id: MessageId,
    lock: bool,
    context: &RequestContext,
) -> Result<Message, Failure> {
    messages::get(conn, project.id, id, lock)
        .await
        .map_err(persistence(context))?
        .filter(|message| message.kind == "question")
        .ok_or_else(|| domain(ErrorCode::NotFound, "question not found"))
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/questions/{question_id}",
    operation_id = "get_question_api_projects__slug__questions__question_id__get",
    summary = "Get Question",
    description = "One question: who asked it, under which attempt or job, whether it blocks,\nthe default it proceeds on, and its answer once given.",
    params(("slug" = String, Path), ("question_id" = String, Path, format = "uuid")),
    responses((status = 200, description = "Successful Response", body = crate::api_models::MessageOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn get_question(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let (mut auth, project, paths) = reader(&state, &context, &mut parts).await?;
    let id = question_id(&paths)?;
    let question = load_question(&mut auth.connection, &project, id, false, &context).await?;
    Ok(Json(message_out(question)).into_response())
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/questions/{question_id}/answer",
    operation_id = "wait_for_answer_api_projects__slug__questions__question_id__answer_get",
    summary = "Wait For Answer",
    description = "Wait up to `wait` seconds (default 30, at most 60) for a question to be\nanswered or escalated, then return it, with its `answer` once given\n(`null` when the wait ran out: ask again). When you asked the question, the\nanswer returned is acknowledged.",
    params(("slug" = String, Path), ("question_id" = String, Path, format = "uuid"),
        ("wait" = Option<i64>, Query, description = "Seconds to wait for the answer.", minimum = 0, maximum = 60)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::MessageOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn wait_for_answer(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let wait = query_value(&parts, "wait")
        .map(|value| {
            value
                .parse::<u64>()
                .ok()
                .filter(|value| *value <= WAIT_MAX_SECONDS)
                .ok_or_else(|| invalid("query/wait", "Input should be between 0 and 60"))
        })
        .transpose()?
        .unwrap_or(WAIT_SECONDS);
    let (mut auth, project, paths) = reader(&state, &context, &mut parts).await?;
    let id = question_id(&paths)?;
    let mut question = load_question(&mut auth.connection, &project, id, false, &context).await?;
    let principal = auth.principal.clone();
    // Never hold a pooled connection while waiting.
    drop(auth);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(wait);
    while question.state.as_deref() == Some("open") && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(
            Duration::from_secs(1)
                .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
        )
        .await;
        let mut conn = state
            .app
            .pool
            .acquire()
            .await
            .map_err(|_| domain(ErrorCode::Unavailable, "the database is busy; ask again"))?;
        question = load_question(&mut conn, &project, id, false, &context).await?;
    }
    if let Some(answer) = question.answer_id
        && question.answer_acknowledged_at.is_none()
    {
        let (user, service) = match &principal {
            Principal::User(user) => (Some(user.user_id), None),
            Principal::Service(service) => (None, Some(service.service_account_id)),
        };
        if (user, service) == (question.author_user, question.author_service) {
            let mut conn = state
                .app
                .pool
                .acquire()
                .await
                .map_err(|_| internal(&context, "answer acknowledgement connection"))?;
            messages::acknowledge(&mut conn, project.id, &[answer.0], user, service)
                .await
                .map_err(persistence(&context))?;
            question = load_question(&mut conn, &project, id, false, &context).await?;
        }
    }
    Ok(Json(message_out(question)).into_response())
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/questions/{question_id}/answer",
    operation_id = "answer_question_api_projects__slug__questions__question_id__answer_post",
    summary = "Answer Question",
    description = "A researcher answers an open question, as Markdown (at most 16 KiB). The\nperformer reads it with `wait_for_answer` or in its next heartbeat\nresponse; a blocking question's attempt or job gets a fresh lease and runs\non. A question whose attempt was released is still answered: the answer\nreaches the unit's next attempt.",
    params(("slug" = String, Path), ("question_id" = String, Path, format = "uuid")),
    request_body(content = crate::api_models::AnswerIn, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = crate::api_models::MessageOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn answer(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let (mut auth, project, paths) =
        researcher(&state, &context, &mut parts, "answer questions").await?;
    let id = question_id(&paths)?;
    let input: AnswerIn = decode(&body)?;
    text("body/body", &input.body, MESSAGE_BODY_MAX_BYTES)?;
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "answer transaction"))?;
    let question = load_question(&mut tx, &project, id, true, &context).await?;
    if question.state.as_deref() != Some("open") {
        return Err(domain(
            ErrorCode::Conflict,
            format!(
                "the question is already {}",
                question.state.as_deref().unwrap_or("closed")
            ),
        ));
    }
    write_answer(
        &mut tx,
        &auth.principal,
        &project,
        &question,
        &input.body,
        &context,
    )
    .await?;
    messages::close(&mut tx, id, None)
        .await
        .map_err(persistence(&context))?;
    resume(&mut tx, &state, &question, &context).await?;
    record(
        &mut tx,
        &auth.principal,
        &project,
        "message",
        &id.to_string(),
        "question.answered",
        &json!({"state": "answered", "unit": question.unit_number, "attempt": question.attempt_sequence}),
        None,
        &context,
    )
    .await?;
    let question = load_question(&mut tx, &project, id, false, &context).await?;
    tx.commit()
        .await
        .map_err(|_| internal(&context, "answer commit"))?;
    Ok(Json(message_out(question)).into_response())
}

async fn write_answer(
    conn: &mut PgConnection,
    principal: &Principal,
    project: &projects::Project,
    question: &Message,
    body: &str,
    context: &RequestContext,
) -> Result<MessageId, Failure> {
    let Principal::User(user) = principal else {
        return Err(internal(context, "answer author"));
    };
    let sha256 = format!("{:x}", Sha256::digest(body.as_bytes()));
    messages::insert(
        conn,
        NewMessage {
            project_id: project.id,
            unit_id: question.unit_id,
            attempt_id: question.attempt_id,
            job_id: None,
            kind: "answer",
            blocking: None,
            default_text: None,
            body,
            sha256: &sha256,
            question_id: Some(question.id),
            author_user: Some(user.user_id),
            author_service: None,
            via_channel: channel_name(user.via.channel),
            via_client: user.via.client.as_deref(),
        },
    )
    .await
    .map_err(persistence(context))
}

/// The longest prefix of `value` that fits in `max` bytes.
fn clip(value: &str, max: usize) -> &str {
    if value.len() <= max {
        return value;
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/questions/{question_id}/escalation",
    operation_id = "escalate_question_api_projects__slug__questions__question_id__escalation_post",
    summary = "Escalate Question",
    description = "A researcher escalates an open question into a concern about the plan of\nthe unit's track: the concern takes the `kind` and the `note`, quoting the\nquestion, and blocks new claims on the track until a plan revision answers\nit or a researcher dismisses it. The question closes as `escalated`, and the\nnote is its answer: the performer reads it as any answer and its lease\nclock restarts.",
    params(("slug" = String, Path), ("question_id" = String, Path, format = "uuid")),
    request_body(content = crate::api_models::EscalationIn, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = crate::api_models::MessageOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
#[allow(
    clippy::too_many_lines,
    reason = "The concern, the answer, the closed question and their audit rows, in one transaction"
)]
pub(crate) async fn escalate(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let (mut auth, project, paths) =
        researcher(&state, &context, &mut parts, "escalate questions").await?;
    let id = question_id(&paths)?;
    let input: EscalationIn = decode(&body)?;
    if !CONCERN_KINDS.contains(&input.kind.as_str()) {
        return Err(invalid(
            "body/kind",
            "Input should be 'wrong_assumption', 'better_idea', 'blocker' or 'other'",
        ));
    }
    text("body/note", &input.note, ESCALATION_NOTE_MAX_BYTES)?;
    let Principal::User(user) = &auth.principal else {
        return Err(internal(&context, "escalation author"));
    };
    let (user_id, via) = (user.user_id, user.via.clone());
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "escalation transaction"))?;
    let question = load_question(&mut tx, &project, id, true, &context).await?;
    if question.state.as_deref() != Some("open") {
        return Err(domain(
            ErrorCode::Conflict,
            format!(
                "the question is already {}",
                question.state.as_deref().unwrap_or("closed")
            ),
        ));
    }
    let (track_id, track_slug, track_state) = messages::unit_track(&mut tx, question.unit_id)
        .await
        .map_err(persistence(&context))?;
    if track_state == "archived" {
        return Err(domain(
            ErrorCode::Conflict,
            format!("track '{track_slug}' is archived and takes no concern"),
        ));
    }
    let (number, sequence) = (question.unit_number, question.attempt_sequence);
    let note = input.note.trim();
    let lead = format!(
        "{note}\n\nEscalated from a question asked during attempt #{number}.{sequence}:\n\n"
    );
    let quoted = question
        .body
        .trim()
        .lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let room = crate::concern_routes::CONCERN_BODY_MAX_BYTES.saturating_sub(lead.len() + 4);
    let concern_body = if quoted.len() > room {
        format!("{lead}{}\n> …", clip(&quoted, room))
    } else {
        format!("{lead}{quoted}")
    };
    let front_matter = json!({"kind": input.kind, "unit": number, "attempt": sequence});
    let front_matter_text = front_matter.to_string();
    let document = format!(
        "---\nkind: {}\nunit: {number}\nattempt: {sequence}\n---\n{concern_body}\n",
        input.kind
    );
    let sha256 = format!("{:x}", Sha256::digest(document.as_bytes()));
    let concern = concerns::insert(
        &mut tx,
        concerns::NewConcern {
            project_id: project.id,
            track_id,
            kind: &input.kind,
            unit_id: Some(question.unit_id),
            attempt_id: Some(question.attempt_id),
            front_matter: &front_matter_text,
            body: &concern_body,
            sha256: &sha256,
            raised_by_user: Some(user_id),
            raised_by_service: None,
            via_channel: channel_name(via.channel),
            via_client: via.client.as_deref(),
        },
    )
    .await
    .map_err(persistence(&context))?;
    let answer_body = format!(
        "{note}\n\nThis question became a concern about the {track_slug} plan ({concern}); no new unit of the track is claimed until it is answered or dismissed."
    );
    write_answer(
        &mut tx,
        &auth.principal,
        &project,
        &question,
        &answer_body,
        &context,
    )
    .await?;
    messages::close(&mut tx, id, Some(concern))
        .await
        .map_err(persistence(&context))?;
    resume(&mut tx, &state, &question, &context).await?;
    record(
        &mut tx,
        &auth.principal,
        &project,
        "concern",
        &concern.to_string(),
        "concern.raised",
        &json!({
            "track": track_slug, "kind": input.kind, "unit": number, "attempt": sequence,
            "state": "open", "sha256": sha256, "question": id.to_string(),
        }),
        None,
        &context,
    )
    .await?;
    record(
        &mut tx,
        &auth.principal,
        &project,
        "message",
        &id.to_string(),
        "question.escalated",
        &json!({"state": "escalated", "concern": concern.to_string()}),
        None,
        &context,
    )
    .await?;
    let question = load_question(&mut tx, &project, id, false, &context).await?;
    tx.commit()
        .await
        .map_err(|_| internal(&context, "escalation commit"))?;
    Ok(Json(message_out(question)).into_response())
}

async fn attempt_of(
    conn: &mut PgConnection,
    project: &projects::Project,
    paths: &BTreeMap<String, String>,
    lock: bool,
    profile: cannery_attempts::model::JsonContext,
    context: &RequestContext,
) -> Result<Attempt, Failure> {
    let number = positive(paths, "number")?;
    let sequence = positive(paths, "sequence")?;
    Repository::new(conn, profile)
        .get_attempt(
            project.id,
            &BigInt::from(number),
            &BigInt::from(sequence),
            lock,
        )
        .await
        .map_err(|_| internal(context, "steering attempt lookup"))?
        .ok_or_else(|| {
            domain(
                ErrorCode::NotFound,
                format!("attempt #{number}.{sequence} not found"),
            )
        })
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/units/{number}/attempts/{sequence}/steering",
    operation_id = "get_steering_api_projects__slug__units__number__attempts__sequence__steering_get",
    summary = "Get Steering",
    description = "The steering notes posted to an attempt, oldest first; only those not yet\nacknowledged with `pending=true`. Heartbeat responses carry the same\nunacknowledged notes.",
    params(("slug" = String, Path), ("number" = i64, Path), ("sequence" = i64, Path),
        ("pending" = Option<bool>, Query, description = "Only notes not yet acknowledged.")),
    responses((status = 200, description = "Successful Response", body = crate::api_models::MessagesOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn get_steering(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let only_pending = match query_value(&parts, "pending") {
        None | Some("false") => false,
        Some("true") => true,
        Some(_) => return Err(invalid("query/pending", "Input should be a valid boolean")),
    };
    let (mut auth, project, paths) = reader(&state, &context, &mut parts).await?;
    let attempt = attempt_of(
        &mut auth.connection,
        &project,
        &paths,
        false,
        state.context.attempts,
        &context,
    )
    .await?;
    let items = messages::of_attempt(&mut auth.connection, project.id, attempt.id)
        .await
        .map_err(persistence(&context))?
        .into_iter()
        .filter(|message| message.kind == "steer")
        .filter(|message| !only_pending || message.acknowledged_at.is_none())
        .map(message_out)
        .collect();
    Ok(Json(MessagesOut { items }).into_response())
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/units/{number}/attempts/{sequence}/steering",
    operation_id = "post_steering_api_projects__slug__units__number__attempts__sequence__steering_post",
    summary = "Post Steering",
    description = "A researcher posts a steering note to a running attempt in agent mode,\nunasked, as Markdown (at most 16 KiB). The agent reads it in its next\nheartbeat response or with `get_steering`, and acknowledges it; until then\nit shows as unacknowledged on the attempt.",
    params(("slug" = String, Path), ("number" = i64, Path), ("sequence" = i64, Path)),
    request_body(content = crate::api_models::SteeringIn, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::MessageOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn post_steering(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let (mut auth, project, paths) =
        researcher(&state, &context, &mut parts, "steer attempts").await?;
    let input: SteeringIn = decode(&body)?;
    text("body/body", &input.body, MESSAGE_BODY_MAX_BYTES)?;
    let Principal::User(user) = &auth.principal else {
        return Err(internal(&context, "steering author"));
    };
    let (user_id, via) = (user.user_id, user.via.clone());
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "steering transaction"))?;
    let attempt = attempt_of(
        &mut tx,
        &project,
        &paths,
        true,
        state.context.attempts,
        &context,
    )
    .await?;
    if !matches!(
        attempt.state,
        AttemptState::Claimed | AttemptState::Running | AttemptState::WaitingOnHuman
    ) {
        return Err(domain(
            ErrorCode::Conflict,
            format!(
                "attempt #{}.{} is {}; steering goes to a running attempt",
                attempt.unit_number,
                attempt.sequence,
                attempt.state.as_str()
            ),
        ));
    }
    if attempt.mode() != Mode::Agent {
        return Err(domain(
            ErrorCode::Conflict,
            "a workflow attempt runs steps that read no steering",
        ));
    }
    let sha256 = format!("{:x}", Sha256::digest(input.body.as_bytes()));
    let id = messages::insert(
        &mut tx,
        NewMessage {
            project_id: project.id,
            unit_id: attempt.unit_id,
            attempt_id: attempt.id,
            job_id: None,
            kind: "steer",
            blocking: None,
            default_text: None,
            body: &input.body,
            sha256: &sha256,
            question_id: None,
            author_user: Some(user_id),
            author_service: None,
            via_channel: channel_name(via.channel),
            via_client: via.client.as_deref(),
        },
    )
    .await
    .map_err(persistence(&context))?;
    record(
        &mut tx,
        &auth.principal,
        &project,
        "message",
        &id.to_string(),
        "steering.posted",
        &json!({"unit": attempt.unit_number, "attempt": attempt.sequence, "sha256": sha256}),
        None,
        &context,
    )
    .await?;
    let message = messages::get(&mut tx, project.id, id, false)
        .await
        .map_err(persistence(&context))?
        .ok_or_else(|| internal(&context, "posted steering"))?;
    tx.commit()
        .await
        .map_err(|_| internal(&context, "steering commit"))?;
    Ok((StatusCode::CREATED, Json(message_out(message))).into_response())
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/messages/acknowledgements",
    operation_id = "ack_steering_api_projects__slug__messages_acknowledgements_post",
    summary = "Ack Steering",
    description = "Acknowledge steering notes and answers you have read, by id: a note posted\nto an attempt you hold, an answer to a question you asked, or an answer to\na released question of the unit whose attempt you now hold. Heartbeat\nresponses stop carrying them. Acknowledging twice is harmless.",
    params(("slug" = String, Path)),
    request_body(content = crate::api_models::AcknowledgementIn, content_type = "application/json"),
    responses((status = 200, description = "Successful Response", body = crate::api_models::AcknowledgementOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn acknowledge(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, body) = crate::body::read_body(request)
        .await
        .map_err(Failure::new)?;
    let (mut auth, project, _) = reader(&state, &context, &mut parts).await?;
    let input: AcknowledgementIn = decode(&body)?;
    if input.ids.is_empty() || input.ids.len() > 200 {
        return Err(invalid("body/ids", "name 1 to 200 messages"));
    }
    let ids = input
        .ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            uuid::Uuid::parse_str(id)
                .map_err(|_| invalid(&format!("body/ids/{index}"), "Input should be a UUID"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (user, service) = match &auth.principal {
        Principal::User(user) => (Some(user.user_id), None),
        Principal::Service(service) => (None, Some(service.service_account_id)),
    };
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "acknowledgement transaction"))?;
    let found = messages::acknowledged(&mut tx, project.id, &ids)
        .await
        .map_err(persistence(&context))?;
    if let Some(missing) = ids
        .iter()
        .find(|id| !found.iter().any(|(found, _)| found.0 == **id))
    {
        return Err(domain(
            ErrorCode::NotFound,
            format!("message {missing} not found"),
        ));
    }
    let done = messages::acknowledge(&mut tx, project.id, &ids, user, service)
        .await
        .map_err(persistence(&context))?;
    if let Some((refused, _)) = found
        .iter()
        .find(|(id, already)| !already && !done.contains(id))
    {
        return Err(domain(
            ErrorCode::Forbidden,
            format!("message {refused} is not an answer or steering note meant for you"),
        ));
    }
    tx.commit()
        .await
        .map_err(|_| internal(&context, "acknowledgement commit"))?;
    Ok(Json(AcknowledgementOut {
        acknowledged: done.iter().map(ToString::to_string).collect(),
    })
    .into_response())
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/units/{number}/messages",
    operation_id = "list_messages_api_projects__slug__units__number__messages_get",
    summary = "List Messages",
    description = "The questions, answers and steering notes of a unit, newest first: of one\nattempt when `attempt` (its sequence) is given.",
    params(("slug" = String, Path), ("number" = i64, Path),
        ("attempt" = Option<i64>, Query, description = "Only messages of this attempt.", minimum = 1),
        ("before" = Option<String>, Query, description = "Continue before this message.", format = "uuid"),
        ("limit" = Option<i64>, Query, description = "Items per page.", minimum = 1, maximum = 200)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::Page_MessageOut_UUID_, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn list_messages(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let (mut auth, project, paths) = reader(&state, &context, &mut parts).await?;
    let number = positive(&paths, "number")?;
    let sequence = query_value(&parts, "attempt")
        .map(|value| {
            value
                .parse::<i32>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| invalid("query/attempt", "Input should be a positive integer"))
        })
        .transpose()?;
    let (before, limit) = page_query(&parts)?;
    let (_, unit) = plans::project_unit(&mut auth.connection, project.id, number)
        .await
        .map_err(persistence(&context))?
        .ok_or_else(|| domain(ErrorCode::NotFound, format!("unit #{number} not found")))?;
    let attempt = match sequence {
        Some(sequence) => Some(
            plans::attempt_id(&mut auth.connection, project.id, number, sequence)
                .await
                .map_err(persistence(&context))?
                .ok_or_else(|| {
                    domain(
                        ErrorCode::NotFound,
                        format!("attempt #{number}.{sequence} not found"),
                    )
                })?,
        ),
        None => None,
    };
    let filter = Filter {
        unit: Some(unit.unit_id),
        attempt,
        before,
        ..Filter::default()
    };
    let out = page(&mut auth.connection, &project, filter, limit, &context).await?;
    Ok(Json(out).into_response())
}
