//! Stateless Streamable HTTP MCP, with a single authenticated connection handoff.
mod registry;
mod resources;

use crate::{
    AppState,
    artifact_download::DownloadContext,
    authentication::{Authenticated, McpHandoff},
    requests::RequestContext,
};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{StatusCode, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
};
use cannery_core::{
    principal::{Channel, Principal},
    settings::SettingsError,
};
use serde_json::{Map, Value, json};
use std::sync::Arc;
use tower::ServiceExt;

pub const PROTOCOL_VERSION: &str = "2025-06-18";
const INSTRUCTIONS: &str = "Cannery Row manages units of research work, the plans that define them, their attempts, independent verification, and human decisions. Read the project brief (get_brief, or the brief resource) and search before working. To plan a track: read the brief and the track, and when re-planning the plan and the done and in-flight units (get_plan, list_track_units, get_unit_plan); refine the idea with the researcher; check references and outside material with search and the read tools; list edge cases and risks into the approach and unit briefs; define units with their acceptance and the context each needs (start_plan_revision, set_plan_approach, add_unit, set_alignment, and answer_concern for every open concern, saying how the revision answers it); then check_plan and submit_plan; a researcher approves. Concerns: raise a concern (raise_concern) when your work shows that the track's plan itself is wrong, whatever phase you are in: a wrong assumption the plan relies on, a better idea than the plan's for testing the track's idea, or a blocker that stops the plan from being carried out as written. The concern is Markdown with YAML front matter (GET /api/schemas/concern): its kind (wrong_assumption, better_idea, blocker or other) and, when it comes from one, the unit number and the attempt sequence, with the argument as the body (at most 16 KiB): what you saw, why it matters to the plan, and what you would change. A runner step raises one by writing /cr/outputs/concern/concern.md. Otherwise, just continue: a failure of your own run is a failure report, a disagreement with a run's claims is the verification report, an unexpected result is the write-up, and a question about one unit is a comment. A concern holds up the whole track: while one is open, no new unit of the track can be claimed (409 concern_open); work already claimed continues through run, verify, document and decide, so finish what you hold. A plan revision answers the concern (answer_concern in the draft; check_plan lists every open concern the draft does not answer, and approval closes those it answers), or a researcher dismisses it with a reason (dismiss_concern). list_concerns and get_concern read them. To run a unit: claim, read the context bundle the claim names (the context resource), heartbeat, upload, record the manifest and submit the run document under the lease you were given: front matter with the claims, provenance and verified manifest (GET /api/schemas/run), run notes as the body. A run that failed releases the attempt with a failure report instead. Verify: a submitted run is verified before a researcher decides on it. When the science revision's verify performer is agent, an agent service account or a researcher who did not run the attempt verifies it; a claim never hands out a job of an attempt you ran yourself. Claim a verify job (claim_job with {\"phase\": \"verify\"}); the answer names the job, the attempt, the brief, the plan and the context bundle, which holds the unit. Read the inputs (get_job_input): run, the run document's front matter with its claims and provenance, and manifest, the run's verified artifacts (download them with object). The run notes are not an input: judge the claims against the artifacts, not the narrative. Heartbeat (heartbeat_job) and upload what you produce (create_job_upload). Complete the job (complete_job) with the verification report: front matter with the verdict (pass, fail or inconclusive), reason, policy revision, gates, verified measurements, discrepancies, comparisons and provenance (GET /api/schemas/verification), and your observations as an optional body. A pass needs every gate passed, and a comparison cites a measurement of the same report. An invalid report is refused with the details and the lease is kept: correct it and complete again. A valid one marks the attempt verified, and the unit waits for its write-up. If you cannot verify, fail the job (fail_job) with a reason. Document: every unit is written up once: after its last attempt is verified, whatever the verdict, or after a researcher stops it following a failure. An agent service account or a researcher writes it up; only a researcher may skip it, with a reason. Claim a document job (claim_job with {\"phase\": \"document\"}); list_writeups shows what waits. The answer names the unit, its last attempt, what the write-up covers and cites (inputs: the attempts and the verification report) and the documenter's context bundle. Read the bundle: the attempt's bundle, then every attempt's run document and notes, the failures and their logs, the verification reports and the comments. Read it at the claim's context ref (?phase=document), or as the resource cannery-row://projects/{project}/units/{number}/attempts/{sequence}/context/document. Heartbeat (heartbeat_job) while you write. Complete the job (complete_job) with the write-up and no manifest: front matter with a one-sentence summary, the attempts it covers (every attempt of the unit) and the verification it cites (the job's inputs.verification, null for a stopped unit) (GET /api/schemas/writeup), and a body: what was tried, what was found and what it means. An invalid write-up is refused with the details and the lease is kept: correct it and complete again. A valid one sends the unit to its decision. If you cannot write it, fail the job (fail_job) with a reason: it is queued again. A researcher writes a unit up with write_up (\"Write it up\" in the web app), which claims and completes the job in one action, and skips it with skip_writeup. Decide: a researcher decides each unit on its decision case (list_review_cases, get_review_case, record_decision) with a decision document: front matter with the outcome (promote, reject, inconclusive, or failed for a unit stopped after a failure) and the verification and writeup it cites ({ref, sha256}, null when there is none) (GET /api/schemas/decision), and the reason as its body. A promotion needs a pass verdict. The decider's bundle (?phase=decide, or the resource ending in /context/decide) adds the write-up, or the reason it was skipped. When the science revision's decide performer is step, the decider service account it registers decides instead: its runner claims a decide job (claim_job with {\"phase\": \"decide\"} and its step revision), runs its decider step on the decider's bundle and completes the job (complete_job) with the decision document, under the same rules; it promotes only on a pass verdict. While the decide job waits or runs, the case is not a researcher's to decide; once the decision is recorded, a researcher corrects it with supersedes. A decide job that keeps failing after its automatic reruns leaves the case to researchers. A failure case is resolved with retry or stop and a reason: stop sends the unit to be written up, then decided failed. Actor and via are taken from your token; plans and decisions need a person with the researcher role.";

#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error(transparent)]
    Settings(#[from] SettingsError),
    #[error("invalid compiled MCP registry")]
    Registry,
}

#[derive(Clone)]
struct McpState {
    app: AppState,
    domain: Router,
    tools: Arc<Vec<registry::Tool>>,
    request_limit: usize,
    result_limit: usize,
    download: Option<Arc<DownloadContext>>,
}
/// A response-only marker; unmatched web fallbacks never masquerade as tool data.
#[derive(Clone)]
struct DomainHandler;
async fn mark_handler(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.extensions_mut().insert(DomainHandler);
    response
}
/// A domain controller may set this only when its successful result is a replay.
/// It is an in-process response extension, never a trusted external header.
#[derive(Clone, Copy)]
pub(crate) struct ReplayOutcome(pub bool);

/// Add MCP to a domain router BEFORE the outer HTTP request layers are installed.
/// `domain` must exclude MCP and those layers: it receives the private single-use
/// authenticated connection, never a second acquisition or authentication.
/// # Errors
/// Rejects unrepresentable configured limits or an invalid compiled registry.
pub fn routes(
    app: AppState,
    domain: Router,
    download: Option<Arc<DownloadContext>>,
) -> Result<Router, StartupError> {
    let state = McpState {
        request_limit: app
            .settings
            .mcp
            .max_request_bytes
            .to_usize("mcp.max_request_bytes")?,
        result_limit: app
            .settings
            .mcp
            .max_result_bytes
            .to_usize("mcp.max_result_bytes")?,
        tools: Arc::new(registry::tools()?),
        domain: domain.route_layer(middleware::from_fn(mark_handler)),
        app,
        download,
    };
    Ok(Router::new()
        .route(
            "/mcp",
            post(endpoint)
                .get(no_stream)
                .delete(no_stream)
                .head(no_stream),
        )
        .with_state(state))
}
async fn no_stream(State(state): State<McpState>, request: Request) -> Response {
    if let Some(error) = header_error(&state, request.headers()) {
        return error;
    }
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST")], Json(json!({"error":{"code":"method_not_allowed","message":"this MCP server only answers POST","details":null}}))).into_response()
}
fn rpc_error(
    id: Value,
    code: i32,
    message: &'static str,
    data: Option<Value>,
    status: StatusCode,
) -> Response {
    let mut error = json!({"code":code,"message":message});
    if let Some(data) = data {
        error["data"] = data;
    }
    let mut reply = json!({"jsonrpc":"2.0","error":error});
    reply["id"] = id;
    (status, Json(reply)).into_response()
}
fn rpc_result(id: Value, result: Value) -> Response {
    let mut reply = json!({"jsonrpc":"2.0"});
    reply["id"] = id;
    reply["result"] = result;
    Json(reply).into_response()
}
fn api_error(
    code: &'static str,
    message: &'static str,
    status: StatusCode,
    details: Value,
) -> Response {
    let mut body = json!({"error":{"code":code,"message":message}});
    body["error"]["details"] = details;
    (status, Json(body)).into_response()
}
fn header_error(state: &McpState, headers: &axum::http::HeaderMap) -> Option<Response> {
    if let Some(value) = crate::request_context::first_header(headers, "origin")
        && origin(&value, true).is_none_or(|supplied| {
            origin(&state.app.settings.server.public_base_url, false) != Some(supplied)
        })
    {
        return Some(api_error(
            "forbidden",
            "cross-origin MCP requests are refused",
            StatusCode::FORBIDDEN,
            Value::Null,
        ));
    }
    if let Some(version) = crate::request_context::first_header(headers, "mcp-protocol-version")
        && version != PROTOCOL_VERSION
    {
        return Some(api_error(
            "bad_request",
            "unsupported MCP protocol version",
            StatusCode::BAD_REQUEST,
            json!({"supported":[PROTOCOL_VERSION]}),
        ));
    }
    None
}
fn origin(value: &str, strict: bool) -> Option<(String, String, u16)> {
    let uri: Uri = value.parse().ok()?;
    let scheme = uri.scheme_str()?.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") {
        return None;
    }
    let authority = uri.authority()?;
    if authority.port().is_some() && authority.port_u16().is_none() {
        return None;
    }
    if authority.as_str().contains('@')
        || (strict && uri.path_and_query().is_some_and(|p| p.as_str() != "/"))
    {
        return None;
    }
    Some((
        scheme.clone(),
        authority.host().to_ascii_lowercase(),
        authority
            .port_u16()
            .unwrap_or(if scheme == "https" { 443 } else { 80 }),
    ))
}
fn client_via(principal: Principal, user_agent: Option<&str>) -> Principal {
    let mut client: String = principal
        .via()
        .client
        .as_deref()
        .unwrap_or("token")
        .chars()
        .map(|c| {
            if c == ';' || !cannery_core::text::printable(u32::from(c)) {
                '_'
            } else {
                c
            }
        })
        .take(200)
        .collect();
    if let Some(agent) = user_agent {
        let agent: String = agent
            .chars()
            .filter(|c| *c != ';' && cannery_core::text::printable(u32::from(*c)))
            .collect();
        let agent: String = agent.trim().chars().take(200).collect();
        if !agent.trim().is_empty() {
            client.push_str("; ua:");
            client.push_str(agent.trim());
        }
    }
    principal.with_channel(Channel::Mcp, Some(client))
}
async fn bearer(
    state: &McpState,
    context: &RequestContext,
    headers: &axum::http::HeaderMap,
) -> Result<Authenticated, crate::errors::ApiError> {
    let mut connection = state
        .app
        .pool
        .acquire()
        .await
        .map_err(|_| context.internal("MCP database connection"))?;
    let authorization = crate::request_context::first_header(headers, "authorization");
    let principal =
        cannery_identity::auth::bearer_principal(&mut connection, authorization.as_deref())
            .await
            .map_err(|error| context.identity_error(error))?;
    let agent = crate::request_context::first_header(headers, "user-agent");
    Ok(Authenticated {
        connection,
        principal: client_via(principal, agent.as_deref()),
    })
}
#[allow(
    clippy::too_many_lines,
    reason = "Protocol and authentication validation order remains explicit"
)]
async fn endpoint(State(state): State<McpState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let Some(context) = parts.extensions.get::<RequestContext>().cloned() else {
        return rpc_error(
            Value::Null,
            -32603,
            "internal error",
            None,
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    };
    if let Some(error) = header_error(&state, &parts.headers) {
        return error;
    }
    let Ok(bytes) = to_bytes(body, state.request_limit).await else {
        return rpc_error(
            Value::Null,
            -32600,
            "MCP request body is too large or unavailable",
            None,
            StatusCode::PAYLOAD_TOO_LARGE,
        );
    };
    let message: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => {
            return rpc_error(
                Value::Null,
                -32700,
                "parse error",
                None,
                StatusCode::BAD_REQUEST,
            );
        }
    };
    let authentication = match bearer(&state, &context, &parts.headers).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let Some(message) = message.as_object() else {
        return rpc_error(
            Value::Null,
            -32600,
            "a JSON-RPC message is an object; batches are unsupported",
            None,
            StatusCode::BAD_REQUEST,
        );
    };
    let id = message.get("id");
    let valid_id =
        id.is_some_and(|id| id.is_string() || id.as_i64().is_some() || id.as_u64().is_some());
    let reply_id = if valid_id {
        id.cloned().unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return rpc_error(
            reply_id,
            -32600,
            "jsonrpc must be 2.0",
            None,
            StatusCode::BAD_REQUEST,
        );
    }
    let Some(method) = message.get("method") else {
        if valid_id && message.contains_key("result") != message.contains_key("error") {
            return StatusCode::ACCEPTED.into_response();
        }
        return rpc_error(
            reply_id,
            -32600,
            "invalid client response",
            None,
            StatusCode::BAD_REQUEST,
        );
    };
    let Some(method) = method.as_str() else {
        return rpc_error(
            reply_id,
            -32600,
            "method must be a string",
            None,
            StatusCode::BAD_REQUEST,
        );
    };
    if id.is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    if !valid_id {
        return rpc_error(
            Value::Null,
            -32600,
            "id must be a string or an integer",
            None,
            StatusCode::BAD_REQUEST,
        );
    }
    let empty = Map::new();
    let params = match message.get("params") {
        None => &empty,
        Some(value) => match value.as_object() {
            Some(value) => value,
            None => {
                return rpc_error(
                    reply_id,
                    -32602,
                    "params must be an object",
                    None,
                    StatusCode::OK,
                );
            }
        },
    };
    match method {
        "initialize" => {
            if params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .is_none()
            {
                return rpc_error(
                    reply_id,
                    -32602,
                    "protocolVersion is required",
                    None,
                    StatusCode::OK,
                );
            }
            rpc_result(
                reply_id,
                json!({"protocolVersion":PROTOCOL_VERSION,"capabilities":{"tools":{"listChanged":false},"resources":{"listChanged":false}},"serverInfo":{"name":"cannery-row","title":"Cannery Row","version":env!("CARGO_PKG_VERSION")},"instructions":INSTRUCTIONS}),
            )
        }
        "ping" => rpc_result(reply_id, json!({})),
        "tools/list" => rpc_result(
            reply_id,
            json!({"tools":state.tools.iter().map(|tool| &tool.definition).collect::<Vec<_>>()}),
        ),
        "tools/call" => tool_call(&state, context, authentication, reply_id, params).await,
        "resources/templates/list" => rpc_result(reply_id, resources::templates()),
        "resources/list" => {
            resources::list(&state, context, authentication, reply_id, params).await
        }
        "resources/read" => {
            resources::read(&state, context, authentication, reply_id, params).await
        }
        _ => rpc_error(reply_id, -32601, "method not found", None, StatusCode::OK),
    }
}
fn tool_error(code: &'static str, message: &'static str, details: Value) -> Value {
    let mut body = json!({"error":{"code":code,"message":message}});
    body["error"]["details"] = details;
    body
}
fn result_too_large(limit: usize) -> Value {
    tool_error(
        "result_too_large",
        "the result exceeds the configured byte limit; ask for a smaller page or narrow the filters",
        json!({"max_bytes":limit}),
    )
}
fn tool_result(payload: Value, is_error: bool, replayed: bool, limit: usize) -> Value {
    let mut structured = if payload.is_object() {
        payload
    } else {
        json!({"items":payload})
    };
    if replayed {
        structured["replayed"] = json!(true);
    }
    let text = serde_json::to_string(&structured).unwrap_or_default();
    if text.len() > limit {
        return tool_result(result_too_large(limit), true, false, usize::MAX);
    }
    let mut result = json!({"content":[{"type":"text","text":text}],"structuredContent":structured,"isError":is_error});
    if replayed {
        result["_meta"] = json!({"replayed":true});
    }
    result
}
async fn tool_call(
    state: &McpState,
    context: RequestContext,
    authentication: Authenticated,
    id: Value,
    params: &Map<String, Value>,
) -> Response {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return rpc_error(id, -32602, "tool name is required", None, StatusCode::OK);
    };
    let Some(tool) = state.tools.iter().find(|tool| tool.name() == name) else {
        return rpc_error(id, -32602, "unknown tool", None, StatusCode::OK);
    };
    let args = params
        .get("arguments")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| json!({}));
    if let Some(errors) = tool.invalid_arguments(&args) {
        return rpc_error(
            id,
            -32602,
            "invalid tool arguments",
            Some(errors),
            StatusCode::OK,
        );
    }
    let Some(args) = args.as_object() else {
        return rpc_error(
            id,
            -32602,
            "arguments must be an object",
            None,
            StatusCode::OK,
        );
    };
    if name == "get_artifact" {
        return match artifact(state, &context, authentication, args).await {
            Ok(payload) => rpc_result(id, tool_result(payload, false, false, state.result_limit)),
            Err(response) => response_result(state, id, *response, false).await,
        };
    }
    let Ok(request) = tool.request(args) else {
        return rpc_error(
            id,
            -32602,
            "invalid tool transport arguments",
            None,
            StatusCode::OK,
        );
    };
    let mut forwarded = Request::new(Body::from(request.body));
    *forwarded.method_mut() = request.method;
    *forwarded.uri_mut() = request.uri;
    *forwarded.headers_mut() = request.headers;
    let mut context = context;
    context.mcp_authentication = Some(McpHandoff::new(authentication));
    forwarded.extensions_mut().insert(context);
    let response = match state.domain.clone().oneshot(forwarded).await {
        Ok(response) => response,
        Err(never) => match never {},
    };
    if response.extensions().get::<DomainHandler>().is_none() {
        return rpc_result(
            id,
            tool_result(
                tool_error(
                    "tool_unavailable",
                    "the corresponding domain service is not installed",
                    Value::Null,
                ),
                true,
                false,
                state.result_limit,
            ),
        );
    }
    let replayed = response
        .extensions()
        .get::<ReplayOutcome>()
        .is_some_and(|value| value.0)
        || (matches!(name, "claim_job" | "submit_attempt" | "record_decision")
            && response.status() == StatusCode::OK);
    response_result(state, id, response, replayed).await
}
async fn response_result(
    state: &McpState,
    id: Value,
    response: Response,
    replayed: bool,
) -> Response {
    let success = response.status().is_success();
    let Ok(bytes) = to_bytes(response.into_body(), state.result_limit).await else {
        return rpc_result(
            id,
            tool_result(
                result_too_large(state.result_limit),
                true,
                false,
                usize::MAX,
            ),
        );
    };
    match serde_json::from_slice(&bytes) {
        Ok(payload) => rpc_result(
            id,
            tool_result(payload, !success, success && replayed, state.result_limit),
        ),
        Err(_) => rpc_error(id, -32603, "internal error", None, StatusCode::OK),
    }
}
async fn artifact(
    state: &McpState,
    context: &RequestContext,
    mut authentication: Authenticated,
    args: &Map<String, Value>,
) -> Result<Value, Box<Response>> {
    let Some(download) = &state.download else {
        return Err(Box::new(
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(tool_error(
                    "tool_unavailable",
                    "the artifact store identity is not installed",
                    Value::Null,
                )),
            )
                .into_response(),
        ));
    };
    let slug = args["project"].as_str().unwrap_or_default();
    let id = args["artifact_id"]
        .as_str()
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .ok_or_else(|| Box::new(context.internal("MCP artifact identifier").into_response()))?;
    let artifact = crate::artifact_permission::permitted_artifact(
        &mut authentication.connection,
        &authentication.principal,
        slug,
        cannery_attempts::model::ArtifactId(id),
        crate::artifact_permission::PermissionContext {
            backend: download.store.backend(),
            bucket: download.store.bucket(),
            repository: download.repository,
        },
    )
    .await
    .map_err(|error| {
        Box::new(match error {
            crate::artifact_permission::PermissionError::Domain(error) => {
                crate::errors::ApiError::from(error).into_response()
            }
            crate::artifact_permission::PermissionError::Project(error) => {
                context.project_error(error).into_response()
            }
            crate::artifact_permission::PermissionError::Attempt(_) => {
                context.internal("MCP artifact lookup").into_response()
            }
        })
    })?;
    let artifact: Value = serde_json::from_slice(
        &crate::attempt_read_wire::artifact(&artifact)
            .map_err(|_| Box::new(context.internal("MCP artifact projection").into_response()))?,
    )
    .map_err(|_| Box::new(context.internal("MCP artifact encoding").into_response()))?;
    Ok(
        json!({"artifact":artifact,"download_url":format!("{}/api/projects/{}/artifacts/{id}",state.app.settings.server.public_base_url.trim_end_matches('/'),registry::encode(slug)),"method":"GET"}),
    )
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "Assertions on reviewed static test fixtures"
)]
mod tests {
    use super::*;
    #[test]
    fn registry_has_all_source_tools_and_complete_validation() {
        let tools = registry::tools().expect("compiled registry");
        assert_eq!(tools.len(), 64);
        let names: std::collections::BTreeSet<_> = tools.iter().map(registry::Tool::name).collect();
        assert_eq!(names.len(), 64);
        for tool in &tools {
            assert!(
                tool.invalid_arguments(&json!({"unexpected":"secret"}))
                    .is_some()
            );
            assert_eq!(tool.definition["outputSchema"]["type"], "object");
        }
        let find = |name: &str| tools.iter().find(|t| t.name() == name).expect("tool");
        assert!(
            find("get_job")
                .invalid_arguments(&json!({"project":"matrix","job_id":"invalid"}))
                .is_some()
        );
        assert!(
            find("query_metrics")
                .invalid_arguments(&json!({"project":"matrix","metric":"UPPERCASE"}))
                .is_some()
        );
        assert!(
            find("comment")
                .invalid_arguments(&json!({"project":"matrix","number":1,"body_markdown":"Hello"}))
                .is_none()
        );
        assert!(
            find("comment")
                .invalid_arguments(
                    &json!({"project":"matrix","number":"1","body_markdown":"Hello"})
                )
                .is_some()
        );
    }
    #[test]
    #[allow(clippy::too_many_lines, reason = "One assertion per tool mapping")]
    fn path_query_body_and_capabilities_are_separate() {
        let tools = registry::tools().expect("registry");
        let build = |name: &str, args: Value| {
            tools
                .iter()
                .find(|t| t.name() == name)
                .expect("tool")
                .request(args.as_object().expect("args"))
                .expect("request")
        };
        let search = build(
            "search",
            json!({"project":["one","two"],"q":"é/?","track":["a&b","c"]}),
        );
        assert_eq!(search.method, axum::http::Method::GET);
        let query = search.uri.query().expect("query");
        assert!(query.contains("project=one&project=two"));
        assert!(query.contains("q=%C3%A9%2F%3F"));
        assert!(query.contains("track=a%26b&track=c"));
        assert!(query.contains("limit=50"));
        let upload = build(
            "create_upload",
            json!({"project":"m/atrix","number":1,"sequence":2,"lease_token":"opaque","lease_generation":3,"role":"candidate","name":"a","size_bytes":12,"sha256":"hash"}),
        );
        assert_eq!(
            upload.uri.path(),
            "/api/projects/m%2Fatrix/units/1/attempts/2/uploads"
        );
        assert_eq!(upload.headers["x-lease-token"], "opaque");
        let body: Value = serde_json::from_slice(&upload.body).expect("body");
        assert!(body.get("lease_token").is_none());
        assert!(body.get("project").is_none());
        assert_eq!(body["size_bytes"], 12);
        let document = "---\nprovenance: {}\n---\nNotes.\n";
        let submit = build(
            "submit_attempt",
            json!({"project":"matrix","number":1,"sequence":2,"lease_token":"opaque","lease_generation":1,"idempotency_key":"key","document":document}),
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&submit.body).expect("body"),
            json!({"document":document})
        );
        assert_eq!(submit.headers["idempotency-key"], "key");
        let brief = build("get_brief", json!({"project":"matrix"}));
        assert_eq!(brief.uri, "/api/projects/matrix/brief");
        let brief = build("get_brief", json!({"project":"matrix","revision":2}));
        assert_eq!(brief.uri, "/api/projects/matrix/brief/revisions/2");
        let brief = build(
            "revise_brief",
            json!({"project":"matrix","expected_revision":0,"document":"---\ntitle: T\ngoal: G\n---\n"}),
        );
        assert_eq!(brief.method, axum::http::Method::POST);
        assert_eq!(brief.uri, "/api/projects/matrix/brief");
        assert_eq!(
            serde_json::from_slice::<Value>(&brief.body).expect("body"),
            json!({"expected_revision":0,"document":"---\ntitle: T\ngoal: G\n---\n"})
        );
        let plan = build("get_plan", json!({"project":"matrix","track":"t"}));
        assert_eq!(plan.uri, "/api/projects/matrix/tracks/t/plans/current");
        let plan = build(
            "get_plan",
            json!({"project":"matrix","track":"t","revision":"draft"}),
        );
        assert_eq!(plan.uri, "/api/projects/matrix/tracks/t/plans/draft");
        let start = build(
            "start_plan_revision",
            json!({"project":"matrix","track":"t"}),
        );
        assert_eq!(start.method, axum::http::Method::POST);
        assert_eq!(start.uri, "/api/projects/matrix/tracks/t/plans");
        let unit = build(
            "add_unit",
            json!({"project":"matrix","track":"t","key":"k","title":"T","question":"Q","intervention":"I","acceptance":{}}),
        );
        assert_eq!(unit.uri, "/api/projects/matrix/tracks/t/plans/draft/units");
        let body: Value = serde_json::from_slice(&unit.body).expect("body");
        assert_eq!(body["key"], "k");
        assert!(body.get("track").is_none());
        let update = build(
            "update_unit",
            json!({"project":"matrix","track":"t","key":"k","brief":"B"}),
        );
        assert_eq!(update.method, axum::http::Method::PUT);
        assert_eq!(
            update.uri,
            "/api/projects/matrix/tracks/t/plans/draft/units/k"
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&update.body).expect("body"),
            json!({"brief":"B"})
        );
        let dropped = build(
            "drop_unit",
            json!({"project":"matrix","track":"t","key":"k"}),
        );
        assert_eq!(dropped.method, axum::http::Method::DELETE);
        let alignment = build(
            "set_alignment",
            json!({"project":"matrix","track":"t","number":4,"decision":"keep","reason":"R"}),
        );
        assert_eq!(
            alignment.uri,
            "/api/projects/matrix/tracks/t/plans/draft/alignments/4"
        );
        let review = build(
            "review_plan",
            json!({"project":"matrix","track":"t","revision":2,"action":"approve","reason":"R"}),
        );
        assert_eq!(review.uri, "/api/projects/matrix/tracks/t/plans/2/review");
        let units = build("list_track_units", json!({"project":"matrix","track":"t"}));
        assert_eq!(units.uri.path(), "/api/projects/matrix/tracks/t/units");
        let history = build("get_unit_history", json!({"project":"matrix","number":3}));
        assert_eq!(history.uri, "/api/projects/matrix/units/3/history");
        let edit = build(
            "edit_comment",
            json!({"project":"matrix","comment_id":"uuid","expected_revision":1,"body_markdown":"é"}),
        );
        assert_eq!(edit.method, axum::http::Method::PUT);
        let input = build(
            "get_job_input",
            json!({"project":"matrix","job_id":"uuid","input":"run","lease_token":"x","lease_generation":1}),
        );
        assert_eq!(
            input.uri.path(),
            "/api/projects/matrix/jobs/uuid/inputs/run"
        );
        assert_eq!(input.body, [] as [u8; 0]);
        let raised = build(
            "raise_concern",
            json!({"project":"matrix","track":"t","document":"---\nkind: blocker\n---\nWhy.\n"}),
        );
        assert_eq!(raised.method, axum::http::Method::POST);
        assert_eq!(raised.uri, "/api/projects/matrix/tracks/t/concerns");
        assert_eq!(
            serde_json::from_slice::<Value>(&raised.body).expect("body"),
            json!({"document":"---\nkind: blocker\n---\nWhy.\n"})
        );
        let open = build(
            "list_concerns",
            json!({"project":"matrix","track":"t","state":"open"}),
        );
        assert_eq!(open.uri.path(), "/api/projects/matrix/concerns");
        assert!(open.uri.query().expect("query").contains("state=open"));
        let dismissed = build(
            "dismiss_concern",
            json!({"project":"matrix","concern_id":"uuid","reason":"R"}),
        );
        assert_eq!(
            dismissed.uri,
            "/api/projects/matrix/concerns/uuid/dismissal"
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&dismissed.body).expect("body"),
            json!({"reason":"R"})
        );
        let answer = build(
            "answer_concern",
            json!({"project":"matrix","track":"t","concern_id":"uuid","how":"H"}),
        );
        assert_eq!(answer.method, axum::http::Method::PUT);
        assert_eq!(
            answer.uri,
            "/api/projects/matrix/tracks/t/plans/draft/answers/uuid"
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&answer.body).expect("body"),
            json!({"how":"H"})
        );
        let claim = build("claim_job", json!({"project":"matrix","phase":"verify"}));
        assert_eq!(claim.uri, "/api/projects/matrix/jobs/claims");
        assert_eq!(
            serde_json::from_slice::<Value>(&claim.body).expect("body"),
            json!({"phase":"verify"})
        );
    }
    #[test]
    fn origins_and_result_limits_are_explicit() {
        assert_eq!(
            origin("https://example.com:443", true),
            origin("https://EXAMPLE.com", false)
        );
        assert_ne!(
            origin("http://example.com", true),
            origin("https://example.com", false)
        );
        for invalid in [
            "null",
            "https://evil.example",
            "https://user@example.com",
            "https://example.com/path",
            "https://example.com/?x=1",
        ] {
            assert_ne!(origin(invalid, true), origin("https://example.com", false));
        }
        let result = tool_result(json!([1, 2]), false, true, 100);
        assert_eq!(
            result["structuredContent"],
            json!({"items":[1,2],"replayed":true})
        );
        assert_eq!(result["_meta"]["replayed"], true);
        assert_eq!(
            serde_json::from_str::<Value>(result["content"][0]["text"].as_str().expect("text"))
                .expect("JSON"),
            result["structuredContent"]
        );
        let too_large = tool_result(json!({"large":"é😀"}), false, false, 3);
        assert_eq!(too_large["isError"], true);
        assert_eq!(
            too_large["structuredContent"]["error"]["code"],
            "result_too_large"
        );
        assert!(too_large.get("_meta").is_none());
    }
}

#[cfg(all(test, feature = "conformance-testing"))]
mod live;
