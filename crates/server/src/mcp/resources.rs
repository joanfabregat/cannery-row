//! MCP resources: each project's brief, read through the same domain routes
//! as the tools, under the caller's single authenticated connection.
use super::{DomainHandler, McpState, registry::encode, rpc_error, rpc_result};
use crate::{
    authentication::{Authenticated, McpHandoff},
    requests::RequestContext,
};
use axum::{
    body::{Body, to_bytes},
    extract::Request,
    http::{Method, StatusCode},
    response::Response,
};
use serde_json::{Map, Value, json};
use tower::ServiceExt;

const PREFIX: &str = "cannery-row://projects/";
const MARKDOWN: &str = "text/markdown";
/// MCP's code for a resource that does not exist.
const NOT_FOUND: i32 = -32002;

pub(super) fn templates() -> Value {
    json!({"resourceTemplates":[
        {"uriTemplate":format!("{PREFIX}{{project}}/brief"),"name":"brief","title":"Project brief",
         "description":"The project's current brief: its goal, domain, constraints and conventions, as Markdown with YAML front matter.","mimeType":MARKDOWN},
        {"uriTemplate":format!("{PREFIX}{{project}}/brief/revisions/{{revision}}"),"name":"brief_revision","title":"Project brief revision",
         "description":"One revision of the project's brief, as claims and jobs name it.","mimeType":MARKDOWN},
    ]})
}

/// One GET through the domain router; `None` when its service is not installed
/// or the response is not JSON within the result limit.
async fn get(
    state: &McpState,
    mut context: RequestContext,
    authentication: Authenticated,
    uri: &str,
) -> Option<(StatusCode, Value)> {
    let mut request = Request::new(Body::empty());
    *request.method_mut() = Method::GET;
    *request.uri_mut() = uri.parse().ok()?;
    context.mcp_authentication = Some(McpHandoff::new(authentication));
    request.extensions_mut().insert(context);
    let response = match state.domain.clone().oneshot(request).await {
        Ok(response) => response,
        Err(never) => match never {},
    };
    response.extensions().get::<DomainHandler>()?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), state.result_limit)
        .await
        .ok()?;
    Some((status, serde_json::from_slice(&bytes).ok()?))
}

fn failed(id: Value, status: StatusCode, payload: &Value) -> Response {
    let data = Some(json!({"status":status.as_u16(),"error":payload["error"]["code"]}));
    if status == StatusCode::NOT_FOUND {
        rpc_error(id, NOT_FOUND, "resource not found", data, StatusCode::OK)
    } else {
        rpc_error(
            id,
            -32603,
            "the resource could not be read",
            data,
            StatusCode::OK,
        )
    }
}

/// The brief of every project the caller can read, a page of projects at a
/// time; the cursor is the last project slug.
pub(super) async fn list(
    state: &McpState,
    context: RequestContext,
    authentication: Authenticated,
    id: Value,
    params: &Map<String, Value>,
) -> Response {
    let cursor = match params.get("cursor") {
        None | Some(Value::Null) => None,
        Some(Value::String(cursor)) => Some(encode(cursor)),
        Some(_) => {
            return rpc_error(id, -32602, "cursor must be a string", None, StatusCode::OK);
        }
    };
    let uri = cursor.map_or_else(
        || String::from("/api/projects?limit=200"),
        |cursor| format!("/api/projects?limit=200&before={cursor}"),
    );
    let Some((status, payload)) = get(state, context, authentication, &uri).await else {
        return rpc_error(id, -32603, "internal error", None, StatusCode::OK);
    };
    if !status.is_success() {
        return failed(id, status, &payload);
    }
    let resources = payload["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|project| {
                    let slug = project["slug"].as_str()?;
                    let title = project["title"].as_str().unwrap_or(slug);
                    Some(json!({
                        "uri":format!("{PREFIX}{slug}/brief"),
                        "name":format!("{slug}/brief"),
                        "title":format!("{title}: brief"),
                        "mimeType":MARKDOWN,
                    }))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut result = json!({"resources":resources});
    if let Some(next) = payload["next_before"].as_str() {
        result["nextCursor"] = json!(next);
    }
    rpc_result(id, result)
}

/// The project slug and optional revision a brief URI names.
fn parse(uri: &str) -> Option<(&str, Option<&str>)> {
    let rest = uri.strip_prefix(PREFIX)?;
    let parts: Vec<_> = rest.split('/').collect();
    match parts.as_slice() {
        [slug, "brief"] if !slug.is_empty() => Some((slug, None)),
        [slug, "brief", "revisions", revision]
            if !slug.is_empty()
                && !revision.is_empty()
                && revision.len() <= 10
                && revision.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            Some((slug, Some(revision)))
        }
        _ => None,
    }
}

pub(super) async fn read(
    state: &McpState,
    context: RequestContext,
    authentication: Authenticated,
    id: Value,
    params: &Map<String, Value>,
) -> Response {
    let Some(uri) = params.get("uri").and_then(Value::as_str) else {
        return rpc_error(id, -32602, "uri is required", None, StatusCode::OK);
    };
    let Some((slug, revision)) = parse(uri) else {
        return rpc_error(
            id,
            NOT_FOUND,
            "resource not found",
            Some(json!({"uri":uri})),
            StatusCode::OK,
        );
    };
    let path = revision.map_or_else(
        || format!("/api/projects/{}/brief", encode(slug)),
        |revision| format!("/api/projects/{}/brief/revisions/{revision}", encode(slug)),
    );
    let Some((status, payload)) = get(state, context, authentication, &path).await else {
        return rpc_error(id, -32603, "internal error", None, StatusCode::OK);
    };
    if !status.is_success() {
        return failed(id, status, &payload);
    }
    let Some(document) = payload["document"].as_str() else {
        return rpc_error(id, -32603, "internal error", None, StatusCode::OK);
    };
    rpc_result(
        id,
        json!({"contents":[{"uri":uri,"mimeType":MARKDOWN,"text":document}]}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brief_uris_name_a_project_and_an_optional_revision() {
        assert_eq!(
            parse("cannery-row://projects/demo/brief"),
            Some(("demo", None))
        );
        assert_eq!(
            parse("cannery-row://projects/demo/brief/revisions/3"),
            Some(("demo", Some("3")))
        );
        for uri in [
            "cannery-row://projects//brief",
            "cannery-row://projects/demo",
            "cannery-row://projects/demo/brief/",
            "cannery-row://projects/demo/brief/revisions/",
            "cannery-row://projects/demo/brief/revisions/-1",
            "cannery-row://projects/demo/brief/revisions/12345678901",
            "cannery-row://projects/a/b/brief",
            "https://projects/demo/brief",
        ] {
            assert_eq!(parse(uri), None, "{uri}");
        }
        let templates = templates();
        assert_eq!(
            templates["resourceTemplates"][0]["uriTemplate"],
            "cannery-row://projects/{project}/brief"
        );
    }
}
