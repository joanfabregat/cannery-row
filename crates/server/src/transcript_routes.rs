// SPDX-License-Identifier: AGPL-3.0-only
//! Transcripts of agent-mode attempts. The agent appends its transcript in
//! chunks while it holds the attempt's lease: each event is one line of
//! `transcript.schema.json`, each chunk one stored object of JSON Lines,
//! and the whole is capped by the project's `transcript_max_bytes`.
//! Submitting the attempt seals the chunks into one `transcript` artifact;
//! nothing is appended after. Redacting secrets is the performer's job.
use crate::{
    api_models::{TranscriptAppend, TranscriptAppendOut, TranscriptEventOut, TranscriptOut},
    attempt_lease_routes,
    authentication::authenticate,
    errors::ApiError,
    message_routes::{
        RouteState, decode, idempotency_key, invalid, persistence, positive, query_value, remember,
        replayed,
    },
    plan_routes::channel_name,
    requests::RequestContext,
    unit_routes::{Failure, domain, internal},
};
use axum::{
    Json,
    extract::{FromRequestParts, Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use cannery_attempts::{
    model::{Attempt, Mode},
    repo::Repository,
};
use cannery_core::{
    contracts::ContractKind,
    errors::{DomainError, ErrorCode},
    ids::ProjectId,
    principal::Principal,
};
use cannery_projects::authz;
use cannery_storage::{Error as StoreError, ObjectReader, ObjectStore, StoredObject};
use cannery_tracks::{
    plans,
    transcripts::{self, Chunk, NewChunk},
};
use num_bigint::BigInt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Acquire, PgConnection};
use std::{collections::BTreeMap, sync::Arc};

/// Events per append.
const APPEND_MAX_EVENTS: usize = 1000;
/// Events per page of a read when it names no limit.
const PAGE: usize = 200;
/// Events per page of a read at most.
const PAGE_MAX: usize = 1000;

fn store_unavailable() -> Failure {
    domain(
        ErrorCode::StoreUnavailable,
        "the object store is unavailable; retry later",
    )
}

/// The transcript's totals: events and bytes.
fn totals(chunks: &[Chunk]) -> (i64, i64) {
    let bytes = chunks.iter().map(|chunk| chunk.size_bytes).sum();
    let events = chunks.last().map_or(0, |chunk| {
        i64::from(chunk.first_event) + i64::from(chunk.events)
    });
    (events, bytes)
}

/// Write a new object, accepting an identical one an interrupted earlier
/// write left behind.
async fn write_once(
    store: &ObjectStore,
    key: &str,
    bytes: Vec<u8>,
) -> Result<StoredObject, StoreError> {
    let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let chunks = futures_util::stream::iter([Ok::<_, StoreError>(bytes)]);
    match store.write_new(key, chunks, BigInt::from(size)).await {
        Err(StoreError::ObjectExists) => match store.stat(key).await? {
            Some(found) if found.size_bytes == size && found.sha256 == sha256 => Ok(found),
            _ => Err(StoreError::ObjectExists),
        },
        other => other,
    }
}

async fn read_all(store: &ObjectStore, key: &str) -> Result<Vec<u8>, StoreError> {
    let mut reader = store.read(key).await?;
    let mut bytes = Vec::new();
    while let Some(chunk) = reader.next_chunk().await? {
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// The events of an append as JSON Lines, or `422` naming each event that
/// breaks `transcript.schema.json`.
fn lines(state: &RouteState, input: &TranscriptAppend) -> Result<(Vec<u8>, i32), Failure> {
    if input.events.is_empty() || input.events.len() > APPEND_MAX_EVENTS {
        return Err(invalid(
            "body/events",
            format!("append 1 to {APPEND_MAX_EVENTS} events"),
        ));
    }
    let mut details = Vec::new();
    let mut bytes = Vec::new();
    let received_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    for (index, event) in input.events.iter().enumerate() {
        let document = cannery_core::json::from_value(event.clone())
            .map_err(|_| invalid(&format!("body/events/{index}"), "not a JSON object"))?;
        let violations = state
            .context
            .contracts
            .violations(ContractKind::Transcript, &document)
            .map_err(|_| invalid(&format!("body/events/{index}"), "not a JSON object"))?;
        for violation in violations {
            details.push(json!({
                "path": format!("body/events/{index}{}", violation.path),
                "message": violation.message,
            }));
        }
        // The server's clock, not the performer's: an agent has none it
        // can trust, so every event says when it arrived.
        let mut event = event.clone();
        if let Some(fields) = event.as_object_mut() {
            fields.insert("received_at".into(), Value::String(received_at.clone()));
        }
        let line = serde_json::to_vec(&event).map_err(|_| invalid("body/events", "not JSON"))?;
        bytes.extend_from_slice(&line);
        bytes.push(b'\n');
    }
    if !details.is_empty() {
        return Err(Failure::new(ApiError::from(
            DomainError::new(
                ErrorCode::ValidationFailed,
                "events break transcript.schema.json",
            )
            .with_details(Value::Array(details)),
        )));
    }
    Ok((bytes, i32::try_from(input.events.len()).unwrap_or(i32::MAX)))
}

#[utoipa::path(
    post,
    path = "/api/projects/{slug}/units/{number}/attempts/{sequence}/transcript",
    operation_id = "append_transcript_api_projects__slug__units__number__attempts__sequence__transcript_post",
    summary = "Append Transcript",
    description = "Append events to the attempt's transcript under its lease (agent mode\nonly), as you go: each event is one line of `transcript.schema.json` (`ts`,\n`kind`, `content`), at most 1000 per append. The transcript is stored as\nJSON Lines and read live with `get_transcript`; submitting the attempt seals\nit as its `transcript` artifact. An append that would take the transcript\nover the project's `transcript_max_bytes` is refused with its `size` and the\n`limit`. Redact secrets before appending. With an `Idempotency-Key`, a retry\nappends nothing twice.",
    params(("slug" = String, Path),
        ("number" = i64, Path),
        ("sequence" = i64, Path),
        ("X-Lease-Token" = Option<String>, Header),
        ("X-Lease-Generation" = Option<i64>, Header),
        ("Idempotency-Key" = Option<String>, Header)),
    request_body(content = crate::api_models::TranscriptAppend, content_type = "application/json"),
    responses((status = 201, description = "Successful Response", body = crate::api_models::TranscriptAppendOut, content_type = "application/json"),
        (status = 200, description = "Replayed", body = crate::api_models::TranscriptAppendOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 409, description = "Resource conflict or stale lease", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
#[allow(
    clippy::too_many_lines,
    reason = "The lease, the cap, the stored chunk and its row, in one transaction"
)]
pub(crate) async fn append(
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
    let paths = Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &state)
        .await
        .map_err(Failure::new)?
        .0;
    let parameters =
        attempt_lease_routes::parameters(&paths, &parts.headers, &context).map_err(Failure::new)?;
    let input: TranscriptAppend = decode(&body)?;
    let (bytes, count) = lines(&state, &input)?;
    let key = idempotency_key(&parts)?;
    let project = attempt_lease_routes::worker_access(
        &mut auth.connection,
        &auth.principal,
        &paths["slug"],
        &context,
    )
    .await
    .map_err(Failure::new)?;
    let mut hasher = Sha256::new();
    hasher.update(project.slug.as_bytes());
    hasher.update(b"\n");
    hasher.update(parameters.reference().as_bytes());
    hasher.update(b"\n");
    // The events as sent: the stored lines carry the time they arrived,
    // which a retry must not change.
    hasher.update(
        serde_json::to_vec(&input.events).map_err(|_| internal(&context, "transcript digest"))?,
    );
    let hash = hasher.finalize().to_vec();
    let (user, service, via) = match &auth.principal {
        Principal::User(user) => (Some(user.user_id), None, user.via.clone()),
        Principal::Service(service) => {
            (None, Some(service.service_account_id), service.via.clone())
        }
    };
    let mut tx = auth
        .connection
        .begin()
        .await
        .map_err(|_| internal(&context, "transcript transaction"))?;
    let replay = match key.as_deref() {
        Some(key) => {
            replayed(
                &mut tx,
                "transcript.append",
                &auth.principal,
                key,
                &hash,
                &context,
            )
            .await?
        }
        None => None,
    };
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
            "only an agent-mode attempt keeps a transcript",
        ));
    }
    let limits = plans::limits(&mut tx, project.id)
        .await
        .map_err(persistence(&context))?;
    let chunks = transcripts::chunks(&mut tx, attempt.id)
        .await
        .map_err(persistence(&context))?;
    let (events, size) = totals(&chunks);
    if let Some(sequence) = replay {
        tx.commit()
            .await
            .map_err(|_| internal(&context, "transcript commit"))?;
        let mut response = (
            StatusCode::OK,
            Json(TranscriptAppendOut {
                chunk: sequence.parse().unwrap_or_default(),
                events,
                bytes: size,
                limit: limits.transcript_max_bytes,
            }),
        )
            .into_response();
        response
            .extensions_mut()
            .insert(crate::mcp::ReplayOutcome(true));
        return Ok(response);
    }
    let added = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
    if size.saturating_add(added) > limits.transcript_max_bytes {
        let total = size.saturating_add(added);
        let message = format!(
            "the transcript of attempt #{}.{} would be {total} bytes; the project's limit is {} bytes",
            attempt.unit_number, attempt.sequence, limits.transcript_max_bytes
        );
        return Err(Failure::new(ApiError::from(
            DomainError::new(ErrorCode::ValidationFailed, message.clone()).with_details(json!([{
                "path": "body/events", "message": message,
                "location": format!("attempt #{}.{} transcript", attempt.unit_number, attempt.sequence),
                "size": total, "limit": limits.transcript_max_bytes,
            }])),
        )));
    }
    let sequence = i32::try_from(chunks.len() + 1).map_err(|_| internal(&context, "chunk"))?;
    let object = format!(
        "projects/{}/attempts/{}/transcript/{sequence:06}.jsonl",
        project.id, attempt.id
    );
    let stored = write_once(&state.context.store, &object, bytes)
        .await
        .map_err(|_| store_unavailable())?;
    let first_event = i32::try_from(events).map_err(|_| internal(&context, "transcript events"))?;
    transcripts::insert(
        &mut tx,
        NewChunk {
            attempt_id: attempt.id,
            sequence,
            first_event,
            events: count,
            backend: state.context.store.backend(),
            bucket: state.context.store.bucket(),
            key: &object,
            size_bytes: added,
            sha256: &stored.sha256,
            author_user: user,
            author_service: service,
            via_channel: channel_name(via.channel),
            via_client: via.client.as_deref(),
        },
    )
    .await
    .map_err(persistence(&context))?;
    if let Some(key) = key.as_deref() {
        remember(
            &mut tx,
            "transcript.append",
            &auth.principal,
            key,
            &hash,
            &sequence.to_string(),
            &context,
        )
        .await?;
    }
    tx.commit()
        .await
        .map_err(|_| internal(&context, "transcript commit"))?;
    Ok((
        StatusCode::CREATED,
        Json(TranscriptAppendOut {
            chunk: i64::from(sequence),
            events: events + i64::from(count),
            bytes: size + added,
            limit: limits.transcript_max_bytes,
        }),
    )
        .into_response())
}

#[utoipa::path(
    get,
    path = "/api/projects/{slug}/units/{number}/attempts/{sequence}/transcript",
    operation_id = "get_transcript_api_projects__slug__units__number__attempts__sequence__transcript_get",
    summary = "Get Transcript",
    description = "A page of the attempt's transcript, oldest event first, from the event\nafter `after` (an index, from 0): `next_after` names where the next page\nstarts while more events wait; follow a running attempt by asking again\nafter the last index read. `sealed` says whether the attempt was\nsubmitted and its transcript stored as its `transcript` artifact.",
    params(("slug" = String, Path), ("number" = i64, Path), ("sequence" = i64, Path),
        ("after" = Option<i64>, Query, description = "Continue after this event index.", minimum = 0),
        ("limit" = Option<i64>, Query, description = "Events per page.", minimum = 1, maximum = 1000)),
    responses((status = 200, description = "Successful Response", body = crate::api_models::TranscriptOut, content_type = "application/json"),
        (status = 422, description = "Validation failed", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 401, description = "Authentication required", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 403, description = "Permission denied or invalid CSRF token", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 404, description = "Resource not found", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 503, description = "Service unavailable", body = crate::api_models::ErrorResponse, content_type = "application/json"),
        (status = 500, description = "Internal server error", body = String, content_type = "text/plain"))
)]
pub(crate) async fn read(
    State(state): State<RouteState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    request: Request,
) -> Result<Response, Failure> {
    let (mut parts, _) = request.into_parts();
    let mut auth = authenticate(&state.app, &context, &parts.headers, &parts.method)
        .await
        .map_err(Failure::new)?;
    let paths = Path::<BTreeMap<String, String>>::from_request_parts(&mut parts, &state)
        .await
        .map_err(Failure::new)?
        .0;
    let number = positive(&paths, "number")?;
    let sequence = positive(&paths, "sequence")?;
    let after = query_value(&parts, "after")
        .map(|value| {
            value
                .parse::<i64>()
                .ok()
                .filter(|value| *value >= 0)
                .ok_or_else(|| invalid("query/after", "Input should be a non-negative integer"))
        })
        .transpose()?;
    let limit = query_value(&parts, "limit")
        .map(|value| {
            value
                .parse::<usize>()
                .ok()
                .filter(|value| (1..=PAGE_MAX).contains(value))
                .ok_or_else(|| invalid("query/limit", "Input should be between 1 and 1000"))
        })
        .transpose()?
        .unwrap_or(PAGE);
    let project = authz::project_read(&mut auth.connection, &auth.principal, &paths["slug"])
        .await
        .map_err(|error| Failure::new(context.project_error(error)))?;
    let attempt = Repository::new(&mut auth.connection, state.context.attempts)
        .get_attempt(
            project.id,
            &BigInt::from(number),
            &BigInt::from(sequence),
            false,
        )
        .await
        .map_err(|_| internal(&context, "transcript attempt"))?
        .ok_or_else(|| {
            domain(
                ErrorCode::NotFound,
                format!("attempt #{number}.{sequence} not found"),
            )
        })?;
    let chunks = transcripts::chunks(&mut auth.connection, attempt.id)
        .await
        .map_err(persistence(&context))?;
    let sealed = transcripts::sealed(&mut auth.connection, attempt.id)
        .await
        .map_err(persistence(&context))?;
    drop(auth);
    let (total, bytes) = totals(&chunks);
    let start = after.map_or(0, |after| after + 1);
    let end = start
        .saturating_add(i64::try_from(limit).unwrap_or(i64::MAX))
        .min(total);
    let mut events = Vec::new();
    for chunk in &chunks {
        let first = i64::from(chunk.first_event);
        let past = first + i64::from(chunk.events);
        if past <= start || first >= end {
            continue;
        }
        let stored = read_all(&state.context.store, &chunk.key)
            .await
            .map_err(|_| store_unavailable())?;
        for (offset, line) in stored
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .enumerate()
        {
            let index = first + i64::try_from(offset).unwrap_or(i64::MAX);
            if index < start || index >= end {
                continue;
            }
            let event = serde_json::from_slice(line)
                .map_err(|_| internal(&context, "stored transcript line"))?;
            events.push(TranscriptEventOut { index, event });
        }
    }
    Ok(Json(TranscriptOut {
        unit: i64::from(number),
        attempt: i64::from(sequence),
        events,
        next_after: (end < total).then_some(end - 1),
        total_events: total,
        bytes,
        sealed: sealed.is_some(),
        artifact: sealed.map(|sealed| sealed.id.to_string()),
    })
    .into_response())
}

/// A stream of the chunks' bytes, in order.
fn concatenated(
    store: Arc<ObjectStore>,
    keys: Vec<String>,
) -> impl futures_util::Stream<Item = Result<Vec<u8>, StoreError>> + Send {
    futures_util::stream::unfold(
        (keys.into_iter(), None::<ObjectReader>, false),
        move |(mut keys, mut reader, failed)| {
            let store = store.clone();
            async move {
                if failed {
                    return None;
                }
                loop {
                    if let Some(open) = reader.as_mut() {
                        match open.next_chunk().await {
                            Ok(Some(bytes)) => return Some((Ok(bytes), (keys, reader, false))),
                            Ok(None) => {}
                            Err(error) => return Some((Err(error), (keys, None, true))),
                        }
                    }
                    let key = keys.next()?;
                    match store.read(&key).await {
                        Ok(open) => reader = Some(open),
                        Err(error) => return Some((Err(error), (keys, None, true))),
                    }
                }
            }
        },
    )
}

/// Seal the attempt's transcript at its submission: its chunks become one
/// `transcript` artifact, and the attempt, submitted, takes no more.
/// Nothing happens when it has no transcript or it is sealed already.
/// # Errors
/// `503` when the store fails; a sanitized failure otherwise.
pub(crate) async fn seal(
    conn: &mut PgConnection,
    store: &Arc<ObjectStore>,
    project: ProjectId,
    attempt: &Attempt,
    context: &RequestContext,
) -> Result<(), Failure> {
    let chunks = transcripts::chunks(conn, attempt.id)
        .await
        .map_err(persistence(context))?;
    if chunks.is_empty()
        || transcripts::sealed(conn, attempt.id)
            .await
            .map_err(persistence(context))?
            .is_some()
    {
        return Ok(());
    }
    let (_, size) = totals(&chunks);
    let key = format!(
        "projects/{project}/attempts/{}/transcript-{}.jsonl",
        attempt.id,
        chunks.len()
    );
    let keys = chunks.into_iter().map(|chunk| chunk.key).collect();
    let stream = Box::pin(concatenated(store.clone(), keys));
    let stored = match store.write_new(&key, stream, BigInt::from(size)).await {
        Err(StoreError::ObjectExists) => store
            .stat(&key)
            .await
            .map_err(|_| store_unavailable())?
            .filter(|found| i64::try_from(found.size_bytes).ok() == Some(size))
            .ok_or_else(store_unavailable)?,
        Err(_) => return Err(store_unavailable()),
        Ok(stored) => stored,
    };
    transcripts::seal(
        conn,
        project,
        attempt.id,
        store.backend(),
        store.bucket(),
        &key,
        stored.generation.as_deref(),
        size,
        &stored.sha256,
    )
    .await
    .map_err(persistence(context))?;
    Ok(())
}
