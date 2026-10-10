// SPDX-License-Identifier: AGPL-3.0-only
//! The working protocol every performer follows, `docs/agents.md`: served
//! as Markdown at `/api/protocol`, referenced by every claim and job claim,
//! and sent as the MCP server's instructions, so all three say the same.
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
    description = "The working protocol every performer follows, as Markdown: when to ask a\nquestion and when to proceed on a stated default, when to raise a concern\ninstead, reading and acknowledging steering at each heartbeat, and\nappending the transcript. Every claim and job claim names it as `protocol`\nwith its `sha256`, also sent as the `ETag`; the MCP server sends the same\ntext as its instructions. No authentication.",
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
