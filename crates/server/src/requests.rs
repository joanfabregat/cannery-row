//! Request attribution and trusted proxy handling never log credentials or URLs.

use crate::request_context::{ClientAddress, ForwardedPort, ProxyContext, TrustedProxies};
use axum::{
    extract::{ConnectInfo, MatchedPath, Request, State},
    middleware::Next,
    response::Response,
};
use std::{
    fmt,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tracing::Instrument;

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Actual socket endpoints, independent of forwarded client headers.
#[derive(Clone, Copy, Debug)]
pub struct ConnectionAddresses {
    pub peer: SocketAddr,
    pub local: Option<SocketAddr>,
}

impl
    axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, tokio::net::TcpListener>>
    for ConnectionAddresses
{
    fn connect_info(stream: axum::serve::IncomingStream<'_, tokio::net::TcpListener>) -> Self {
        Self {
            peer: *stream.remote_addr(),
            local: stream.io().local_addr().ok(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct RequestId(String);

impl RequestId {
    fn next() -> Self {
        Self(format!(
            "{}-{:x}",
            std::process::id(),
            NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone)]
pub struct RequestContext {
    pub id: RequestId,
    pub proxy: ProxyContext,
    pub(crate) mcp_authentication: Option<crate::authentication::McpHandoff>,
}

impl fmt::Debug for RequestContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestContext")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl RequestContext {
    pub(crate) fn background() -> Self {
        Self {
            id: RequestId::next(),
            proxy: ProxyContext {
                client: None,
                scheme: "http".to_owned(),
            },
            mcp_authentication: None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct RequestState {
    proxies: TrustedProxies,
}

impl RequestState {
    pub(crate) fn new(forwarded_allow_ips: &str) -> Arc<Self> {
        Arc::new(Self {
            proxies: TrustedProxies::parse(forwarded_allow_ips),
        })
    }

    pub(crate) fn proxy(&self, request: &Request) -> ProxyContext {
        let peer = request
            .extensions()
            .get::<ConnectInfo<ConnectionAddresses>>()
            .map(|address| address.0.peer)
            .or_else(|| {
                request
                    .extensions()
                    .get::<ConnectInfo<SocketAddr>>()
                    .map(|address| address.0)
            });
        let client = peer.map(|address| ClientAddress {
            host: address.ip().to_string(),
            port: ForwardedPort::from(address.port()),
        });
        self.proxies.resolve(
            request.headers(),
            client,
            request.uri().scheme_str().unwrap_or("http"),
            false,
        )
    }
}

pub(crate) async fn contextualize(
    State(state): State<Arc<RequestState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let proxy = state.proxy(&request);
    let id = RequestId::next();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
        .or_else(|| crate::transport::route_template(request.uri().path()))
        .unwrap_or("fallback");
    // A matched template is safe to log; a concrete path/query may contain an
    // upload capability, OIDC authorization code or another opaque credential.
    let span =
        tracing::info_span!("http_request", request_id = %id, method = %request.method(), route);
    request.extensions_mut().insert(RequestContext {
        id,
        proxy,
        mcp_authentication: None,
    });
    async move {
        let response = next.run(request).await;
        tracing::info!(status = response.status().as_u16(), "request completed");
        response
    }
    .instrument(span)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Extension, Json, Router,
        body::Body,
        http::{Request as HttpRequest, StatusCode},
        middleware,
        routing::get,
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    async fn echo(Extension(context): Extension<RequestContext>) -> Json<Value> {
        Json(json!({
            "id": context.id.to_string(), "scheme": context.proxy.scheme,
            "client": context.proxy.client.map(|client| json!({"host":client.host,"port":client.port.as_decimal()})),
        }))
    }

    async fn internal(Extension(context): Extension<RequestContext>) -> crate::errors::ApiError {
        crate::errors::ApiError::Internal {
            request_id: context.id,
            operation: "fixture database failure",
        }
    }

    fn routes() -> Router {
        Router::new()
            .route("/probe", get(echo))
            .route("/failure", get(internal))
            .layer(middleware::from_fn_with_state(
                RequestState::new("127.0.0.1"),
                contextualize,
            ))
    }

    async fn response(
        uri: &str,
        peer: &str,
    ) -> Result<(StatusCode, Value), Box<dyn std::error::Error>> {
        let mut request = HttpRequest::builder()
            .uri(uri)
            .header("x-forwarded-for", "203.0.113.4:70000")
            .header("x-forwarded-proto", "https")
            .body(Body::empty())?;
        request
            .extensions_mut()
            .insert(ConnectInfo(peer.parse::<SocketAddr>()?));
        let response = routes().oneshot(request).await?;
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 4096).await?;
        let value = if status == StatusCode::INTERNAL_SERVER_ERROR {
            assert_eq!(body.as_ref(), b"Internal Server Error");
            Value::String(String::from_utf8(body.to_vec())?)
        } else {
            serde_json::from_slice(&body)?
        };
        Ok((status, value))
    }

    #[tokio::test]
    async fn middleware_uses_real_peer_and_keeps_request_ids_distinct()
    -> Result<(), Box<dyn std::error::Error>> {
        let (status, trusted) = response("/probe?code=private-marker", "127.0.0.1:321").await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(trusted["scheme"], "https");
        assert_eq!(
            trusted["client"],
            json!({"host":"203.0.113.4:70000","port":"0"})
        );
        let (_, untrusted) = response("/probe", "192.0.2.7:321").await?;
        assert_eq!(untrusted["scheme"], "http");
        assert_eq!(
            untrusted["client"],
            json!({"host":"192.0.2.7","port":"321"})
        );
        assert_ne!(trusted["id"], untrusted["id"]);
        assert!(!trusted.to_string().contains("private-marker"));
        Ok(())
    }

    #[tokio::test]
    async fn internal_failures_return_generic_500_without_diagnostics()
    -> Result<(), Box<dyn std::error::Error>> {
        let (status, body) = response("/failure", "127.0.0.1:321").await?;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body, json!("Internal Server Error"));
        assert!(!body.to_string().contains("fixture database failure"));
        Ok(())
    }
}
