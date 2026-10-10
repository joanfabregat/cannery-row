// SPDX-License-Identifier: AGPL-3.0-only
//! The working protocol every performer follows, `docs/agents.md`: served
//! as Markdown at `/api/protocol` and by the MCP tool `get_protocol`, and
//! referenced by every claim and job claim. Its opening, up to the first
//! section, is the MCP server's instructions: short enough for a client to
//! keep whole, and it says to read the rest with `get_protocol`.
use crate::api_models::ProtocolRef;
use axum::{
    Router,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

/// The protocol's Markdown.
pub const TEXT: &str = include_str!("../../../docs/agents.md");
/// Where it is served.
pub const PATH: &str = "/api/protocol";

static DIGEST: LazyLock<String> = LazyLock::new(|| format!("{:x}", Sha256::digest(TEXT)));

/// The MCP server's instructions: the protocol's opening, before its first
/// `##` section.
#[must_use]
pub fn instructions() -> &'static str {
    TEXT.split_once("\n## ")
        .map_or(TEXT, |(opening, _)| opening)
        .trim_end()
}

/// The reference a claim carries.
#[must_use]
pub fn reference() -> ProtocolRef {
    ProtocolRef {
        r#ref: PATH.to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        sha256: DIGEST.clone(),
        bytes: i64::try_from(TEXT.len()).unwrap_or(i64::MAX),
    }
}

pub(crate) fn routes() -> Router {
    Router::new().route(PATH, get(protocol))
}

#[utoipa::path(
    get,
    path = "/api/protocol",
    operation_id = "get_protocol_api_protocol_get",
    summary = "Get Protocol",
    description = "The working protocol every performer follows, as Markdown: when to ask a\nquestion and when to proceed on a stated default, when to raise a concern\ninstead, reading and acknowledging steering at each heartbeat, and\nappending the transcript. Every claim and job claim names it as `protocol`\nwith its `sha256`, also sent as the `ETag`. The MCP tool `get_protocol`\nreturns the same text; its opening, before the first section, is the MCP\nserver's instructions. No authentication.",
    responses((status = 200, description = "Successful Response", body = String, content_type = "text/markdown"),
        (status = 304, description = "Not modified"))
)]
pub(crate) async fn protocol(headers: HeaderMap) -> Response {
    let etag = format!("\"{}\"", *DIGEST);
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|tag| tag.trim() == etag))
    {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response();
    }
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "text/markdown; charset=utf-8".to_owned(),
            ),
            (header::ETAG, etag),
            (header::CACHE_CONTROL, "no-cache".to_owned()),
        ],
        TEXT,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_reference_describes_the_served_text() {
        let reference = super::reference();
        assert_eq!(reference.r#ref, "/api/protocol");
        assert_eq!(
            usize::try_from(reference.bytes).ok(),
            Some(super::TEXT.len())
        );
        assert_eq!(reference.sha256.len(), 64);
        let instructions = super::instructions();
        assert!(
            instructions.len() < 2048,
            "the MCP instructions are {} bytes; keep them under 2 KiB",
            instructions.len()
        );
        assert!(super::TEXT.starts_with(instructions));
        for tool in [
            "get_protocol",
            "get_context",
            "get_schema",
            "claim_unit",
            "claim_job",
        ] {
            assert!(instructions.contains(tool), "the instructions lack {tool}");
        }
        for section in [
            "## Questions",
            "## Steering",
            "## Transcripts",
            "ask",
            "wait_for_answer",
            "ack_steering",
            "append_transcript",
        ] {
            assert!(
                super::TEXT.contains(section),
                "docs/agents.md lacks {section}"
            );
        }
    }
}
