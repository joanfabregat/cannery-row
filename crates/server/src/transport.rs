//! Decode the ASGI path before matching, then preserve Starlette redirects and Allow.

use crate::{
    request_context::first_header,
    requests::{ConnectionAddresses, RequestState},
};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{StatusCode, Uri, Version, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{fmt::Write, sync::Arc};

#[derive(Clone, Copy)]
pub(crate) struct Route {
    pub(crate) pattern: &'static str,
    pub(crate) first_method: &'static str,
}

#[derive(Clone)]
pub(crate) struct TransportState {
    pub(crate) requests: Arc<RequestState>,
    pub(crate) routes: Vec<Route>,
}

fn matches(pattern: &str, path: &str) -> bool {
    let mut actual = path.split('/');
    for expected in pattern.split('/') {
        let Some(value) = actual.next() else {
            return false;
        };
        if expected.starts_with('{') && expected.ends_with('}') {
            if value.is_empty() {
                return false;
            }
        } else if expected != value {
            return false;
        }
    }
    actual.next().is_none()
}

fn matching(routes: &[Route], path: &str) -> Option<Route> {
    routes
        .iter()
        .copied()
        .find(|route| matches(route.pattern, path))
}

fn decode_path(raw: &str) -> String {
    let input = raw.as_bytes();
    let mut bytes = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] == b'%' && index + 2 < input.len() {
            let high = char::from(input[index + 1]).to_digit(16);
            let low = char::from(input[index + 2]).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                if let Ok(value) = u8::try_from(high * 16 + low) {
                    bytes.push(value);
                }
                index += 3;
                continue;
            }
        }
        bytes.push(input[index]);
        index += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn quote(value: &str, safe: &[u8]) -> String {
    let mut text = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) || safe.contains(&byte) {
            text.push(char::from(byte));
        } else {
            let _ = write!(text, "%{byte:02X}");
        }
    }
    text
}

fn canonical_uri(uri: &Uri, decoded: &str) -> Option<Uri> {
    // Re-encode literal percent signs so Axum's parameter extractor decodes once.
    let mut path = quote(decoded, b"/");
    if let Some(query) = uri.query() {
        path.push('?');
        path.push_str(query);
    }
    let mut parts = uri.clone().into_parts();
    parts.path_and_query = Some(path.parse().ok()?);
    Uri::from_parts(parts).ok()
}

fn valid_host(value: &str) -> bool {
    let (host, port) = if let Some(bracketed) = value.strip_prefix('[') {
        let Some((host, suffix)) = bracketed.split_once(']') else {
            return false;
        };
        let port = if suffix.is_empty() {
            None
        } else if let Some(port) = suffix.strip_prefix(':') {
            Some(port)
        } else {
            return false;
        };
        let future = host
            .strip_prefix('v')
            .and_then(|future| future.split_once('.'))
            .is_some_and(|(version, address)| {
                !version.is_empty()
                    && version.bytes().all(|byte| byte.is_ascii_hexdigit())
                    && !address.is_empty()
                    && address.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || b"._~!$&'()*+,;=:-".contains(&byte)
                    })
            });
        if !future && host.parse::<std::net::Ipv6Addr>().is_err() {
            return false;
        }
        (host, port)
    } else {
        let (host, port) = value
            .split_once(':')
            .map_or((value, None), |(host, port)| (host, Some(port)));
        if host.is_empty()
            || !host
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._~%!$&'()*+,;=-".contains(&byte))
        {
            return false;
        }
        (host, port)
    };
    if host.is_empty() {
        return false;
    }
    port.is_none_or(|port| {
        if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
        let significant = port.trim_start_matches('0');
        significant.is_empty() || (significant.len() <= 5 && significant.parse::<u16>().is_ok())
    })
}

fn redirect_location(state: &TransportState, request: &Request, path: &str) -> String {
    let scheme = state.requests.proxy(request).scheme;
    let authority = first_header(request.headers(), "host")
        .filter(|host| valid_host(host))
        .or_else(|| {
            request
                .extensions()
                .get::<ConnectInfo<ConnectionAddresses>>()
                .and_then(|connection| connection.0.local)
                .map(|local| {
                    let host = match local.ip() {
                        std::net::IpAddr::V4(ip) => ip.to_string(),
                        std::net::IpAddr::V6(ip) => format!("[{ip}]"),
                    };
                    if (scheme == "http" && local.port() == 80)
                        || (scheme == "https" && local.port() == 443)
                    {
                        host
                    } else {
                        format!("{host}:{}", local.port())
                    }
                })
        });
    let mut location = authority.map_or_else(String::new, |host| format!("{scheme}://{host}"));
    location.push_str(path);
    if let Some(query) = request.uri().query() {
        location.push('?');
        location.push_str(query);
    }
    quote(&location, b":/%#?=@[]!$&'()*+,;")
}

pub(crate) async fn normalize(
    State(state): State<TransportState>,
    mut request: Request,
    next: Next,
) -> Response {
    // Hyper admits requests h11 rejects; enforce these rules only at the listener.
    // Direct application calls represent ASGI scopes and may omit/duplicate Host.
    if request
        .extensions()
        .get::<ConnectInfo<ConnectionAddresses>>()
        .is_some()
    {
        let hosts = request.headers().get_all(header::HOST).iter().count();
        let target_is_ascii = request
            .uri()
            .to_string()
            .bytes()
            .all(|byte| (0x21..=0x7e).contains(&byte));
        if hosts > 1 || (request.version() == Version::HTTP_11 && hosts == 0) || !target_is_ascii {
            return (
                StatusCode::BAD_REQUEST,
                [(header::CONNECTION, "close")],
                "Invalid HTTP request received.",
            )
                .into_response();
        }
    }
    let path = decode_path(request.uri().path());
    let route = matching(&state.routes, &path);
    if route.is_none() && path != "/" {
        let alternate = if path.ends_with('/') {
            path.trim_end_matches('/').to_owned()
        } else {
            format!("{path}/")
        };
        if matching(&state.routes, &alternate).is_some() {
            return (
                StatusCode::TEMPORARY_REDIRECT,
                [(
                    header::LOCATION,
                    redirect_location(&state, &request, &alternate),
                )],
            )
                .into_response();
        }
    }
    let Some(uri) = canonical_uri(request.uri(), &path) else {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({"detail":"Bad Request"})),
        )
            .into_response();
    };
    *request.uri_mut() = uri;
    let mut response = next.run(request).await;
    if response.status() == StatusCode::METHOD_NOT_ALLOWED
        && let Some(route) = route
    {
        response.headers_mut().insert(
            header::ALLOW,
            header::HeaderValue::from_static(route.first_method),
        );
    }
    response
}

pub(crate) fn installed_routes() -> Vec<Route> {
    [
        ("/api/health", "GET"),
        ("/api/schemas/{phase}", "GET"),
        ("/mcp", "POST"),
        ("/openapi.json", "GET, HEAD"),
        ("/docs", "GET, HEAD"),
        ("/docs/oauth2-redirect", "GET, HEAD"),
        ("/redoc", "GET, HEAD"),
        ("/api/me", "GET"),
        ("/auth/login", "GET"),
        ("/auth/callback", "GET"),
        ("/auth/logout", "POST"),
        ("/api/tokens", "GET"),
        ("/api/tokens/{token_id}", "DELETE"),
        ("/api/users", "GET"),
        ("/api/projects", "GET"),
        ("/api/projects/{slug}", "GET"),
        ("/api/projects/{slug}/members", "GET"),
        ("/api/projects/{slug}/members/{user_id}", "PUT"),
        ("/api/projects/{slug}/config/{kind}", "POST"),
        ("/api/projects/{slug}/config/{kind}/latest", "GET"),
        ("/api/projects/{slug}/config/{kind}/{revision}", "GET"),
        ("/api/projects/{slug}/metrics", "GET"),
        ("/api/projects/{slug}/metrics/query", "GET"),
        ("/api/projects/{slug}/dashboard", "GET"),
        ("/api/projects/{slug}/dashboard/views/{view_id}", "GET"),
        ("/api/projects/{slug}/units", "GET"),
        ("/api/projects/{slug}/review-cases", "GET"),
        ("/api/projects/{slug}/reports", "GET"),
        (
            "/api/projects/{slug}/units/{number}/attempts/{sequence}/report",
            "GET",
        ),
        ("/api/projects/{slug}/attempts", "GET"),
        ("/api/projects/{slug}/artifacts/{artifact_id}", "GET"),
        (
            "/api/projects/{slug}/units/{number}/attempts/{sequence}/heartbeat",
            "POST",
        ),
        ("/api/projects/{slug}/units/{number}/attempts", "GET"),
        (
            "/api/projects/{slug}/units/{number}/attempts/{sequence}",
            "GET",
        ),
        ("/api/projects/{slug}/review-cases/{case_id}", "GET"),
        (
            "/api/projects/{slug}/review-cases/{case_id}/decisions",
            "POST",
        ),
        ("/api/projects/{slug}/attention", "GET"),
        ("/api/search", "GET"),
        ("/api/projects/{slug}/units/{number}/comments", "POST"),
        ("/api/projects/{slug}/units/{number}/comments", "GET"),
        (
            "/api/projects/{slug}/units/{number}/attempts/{sequence}/comments",
            "POST",
        ),
        (
            "/api/projects/{slug}/units/{number}/attempts/{sequence}/comments",
            "GET",
        ),
        ("/api/projects/{slug}/comments/{comment_id}", "GET"),
        ("/api/projects/{slug}/comments/{comment_id}", "PUT"),
        (
            "/api/projects/{slug}/comments/{comment_id}/revisions",
            "GET",
        ),
        ("/api/projects/{slug}/units/{number}", "GET"),
        ("/api/projects/{slug}/units/{number}/revisions", "GET"),
        (
            "/api/projects/{slug}/units/{number}/revisions/{revision}",
            "GET",
        ),
        ("/api/projects/{slug}/tracks", "POST"),
        ("/api/projects/{slug}/tracks/{track_slug}", "GET"),
        (
            "/api/projects/{slug}/tracks/{track_slug}/transitions",
            "POST",
        ),
        ("/api/projects/{slug}/tracks/{track_slug}/history", "GET"),
        ("/api/projects/{slug}/service-accounts", "GET"),
        (
            "/api/projects/{slug}/service-accounts/{name}/disable",
            "POST",
        ),
        ("/api/projects/{slug}/service-accounts/{name}/tokens", "GET"),
    ]
    .into_iter()
    .map(|(pattern, first_method)| Route {
        pattern,
        first_method,
    })
    .collect()
}

pub(crate) fn route_template(raw_path: &str) -> Option<&'static str> {
    matching(&installed_routes(), &decode_path(raw_path)).map(|route| route.pattern)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::Path,
        http::{Method, Request as HttpRequest},
        middleware,
        routing::get,
    };
    use tower::ServiceExt;
    type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    fn app() -> Router {
        let inner = Router::new()
            .route(
                "/api/projects",
                get(|| async { "list" })
                    .post(|| async { "created" })
                    .head(|| async { StatusCode::METHOD_NOT_ALLOWED }),
            )
            .route(
                "/api/projects/{slug}",
                get(|Path(slug): Path<String>| async move { slug })
                    .head(|| async { StatusCode::METHOD_NOT_ALLOWED }),
            )
            .fallback(|| async { StatusCode::NOT_FOUND });
        Router::new()
            .fallback_service(inner)
            .layer(middleware::from_fn_with_state(
                TransportState {
                    requests: RequestState::new("127.0.0.1"),
                    routes: vec![
                        Route {
                            pattern: "/api/projects",
                            first_method: "GET",
                        },
                        Route {
                            pattern: "/api/projects/{slug}",
                            first_method: "GET",
                        },
                    ],
                },
                normalize,
            ))
    }

    async fn send(method: Method, uri: &str) -> Result<Response> {
        Ok(app()
            .oneshot(
                HttpRequest::builder()
                    .method(method)
                    .uri(uri)
                    .header("host", "wire.invalid")
                    .body(Body::empty())?,
            )
            .await?)
    }

    #[tokio::test]
    async fn encoded_paths_are_decoded_before_matching_and_once_for_parameters() -> Result<()> {
        // Observed cases in the actual Python R2 ASGI reference.
        for (raw, text) in [
            ("hello%20world", "hello world"),
            ("%E9", "�"),
            ("%F0%9F%92%A9", "💩"),
            ("%252F", "%2F"),
            ("%00", "\0"),
            ("%ED%A0%80", "���"),
            ("a+b", "a+b"),
        ] {
            let response = send(Method::GET, &format!("/api/projects/{raw}")).await?;
            assert_eq!(response.status(), StatusCode::OK, "{raw}");
            assert_eq!(
                to_bytes(response.into_body(), 4096).await?.as_ref(),
                text.as_bytes(),
                "{raw}"
            );
        }
        let response = send(Method::GET, "/api/projects/a%2Fb").await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        Ok(())
    }

    #[tokio::test]
    async fn slash_redirects_preserve_methods_and_query_and_strip_all_terminal_slashes()
    -> Result<()> {
        for method in [Method::GET, Method::POST, Method::PUT, Method::HEAD] {
            let response = send(method, "/api/projects///?x=%ff&x=a+b&blank").await?;
            assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
            assert_eq!(
                response.headers()[header::LOCATION],
                "http://wire.invalid/api/projects?x=%ff&x=a+b&blank"
            );
        }
        let response = send(Method::GET, "/api/projects/wire%2F").await?;
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            response.headers()[header::LOCATION],
            "http://wire.invalid/api/projects/wire"
        );
        assert_eq!(
            send(Method::GET, "/api/unknown/").await?.status(),
            StatusCode::NOT_FOUND
        );
        Ok(())
    }

    #[tokio::test]
    async fn allow_uses_the_first_source_route_instead_of_combining_methods() -> Result<()> {
        for (method, uri) in [
            (Method::PUT, "/api/projects"),
            (Method::HEAD, "/api/projects/wire"),
        ] {
            let response = send(method, uri).await?;
            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
            assert_eq!(response.headers()[header::ALLOW], "GET");
        }
        Ok(())
    }

    #[tokio::test]
    async fn redirects_use_trusted_scheme_and_actual_local_socket_without_host() -> Result<()> {
        let mut request = HttpRequest::builder()
            .version(Version::HTTP_10)
            .uri("/api/projects/")
            .header("x-forwarded-proto", "https")
            .body(Body::empty())?;
        request
            .extensions_mut()
            .insert(ConnectInfo(ConnectionAddresses {
                peer: "127.0.0.1:1234".parse()?,
                local: Some("[::1]:8080".parse()?),
            }));
        let response = app().oneshot(request).await?;
        assert_eq!(
            response.headers()[header::LOCATION],
            "https://[::1]:8080/api/projects"
        );
        Ok(())
    }

    #[test]
    fn host_validation_retains_source_port_spelling_and_ipvfuture_rules() {
        for host in [
            "Wire.invalid:80",
            "host:0",
            "host:00000065535",
            "host:000000000000000000",
            "[::1]:8080",
            "[v1.x]",
            "[vF.X:Y]",
            "a%zz",
        ] {
            assert!(valid_host(host), "{host}");
        }
        for host in [
            "",
            "host:",
            "host:65536",
            "host:+80",
            "host:１",
            "[::1%eth0]",
            "[V1.x]",
            "[:::]",
            "a/b",
            "a@b",
            "a?b",
            "a#b",
            "a b",
            "ÿ.invalid",
        ] {
            assert!(!valid_host(host), "{host}");
        }
    }

    #[tokio::test]
    async fn invalid_first_host_falls_back_to_server_and_valid_explicit_port_is_preserved()
    -> Result<()> {
        for (first, expected) in [
            (b"\xff.invalid".as_slice(), "http://[::1]/api/projects"),
            (
                b"Wire.invalid:000080".as_slice(),
                "http://Wire.invalid:000080/api/projects",
            ),
        ] {
            let mut request = HttpRequest::builder()
                .uri("/api/projects/")
                .body(Body::empty())?;
            request
                .headers_mut()
                .append(header::HOST, header::HeaderValue::from_bytes(first)?);
            request
                .extensions_mut()
                .insert(ConnectInfo(ConnectionAddresses {
                    peer: "192.0.2.1:1234".parse()?,
                    local: Some("[::1]:80".parse()?),
                }));
            let response = app().oneshot(request).await?;
            assert_eq!(response.headers()[header::LOCATION], expected);
        }
        Ok(())
    }

    #[tokio::test]
    async fn actual_listener_matches_source_host_admission_and_fallback() -> Result<()> {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::{TcpListener, TcpStream},
        };
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (shutdown, receive) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app().into_make_service_with_connect_info::<ConnectionAddresses>(),
            )
            .with_graceful_shutdown(async {
                let _ = receive.await;
            })
            .await
        });
        let outcome = async {
            for (version, hosts, status, location) in [
                (
                    "1.1",
                    b"Host: wire.invalid\r\n".as_slice(),
                    307,
                    Some("http://wire.invalid/api/projects".to_owned()),
                ),
                ("1.1", b"".as_slice(), 400, None),
                (
                    "1.0",
                    b"Host: first.invalid\r\nHost: second.invalid\r\n".as_slice(),
                    400,
                    None,
                ),
                (
                    "1.1",
                    b"Host: first.invalid\r\nHost: second.invalid\r\n".as_slice(),
                    400,
                    None,
                ),
                (
                    "1.0",
                    b"".as_slice(),
                    307,
                    Some(format!("http://{address}/api/projects")),
                ),
                (
                    "1.1",
                    b"Host: \xff.invalid\r\n".as_slice(),
                    307,
                    Some(format!("http://{address}/api/projects")),
                ),
            ] {
                let mut socket = TcpStream::connect(address).await?;
                let mut request = format!("GET /api/projects/ HTTP/{version}\r\n").into_bytes();
                request.extend_from_slice(hosts);
                request.extend_from_slice(b"Connection: close\r\n\r\n");
                socket.write_all(&request).await?;
                let mut response = Vec::new();
                tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    socket.read_to_end(&mut response),
                )
                .await??;
                let response = String::from_utf8(response)?;
                assert!(
                    response.starts_with(&format!("HTTP/{version} {status}")),
                    "status mismatch"
                );
                if let Some(location) = location {
                    assert!(
                        response.contains(&format!("location: {location}\r\n")),
                        "redirect mismatch"
                    );
                } else {
                    assert!(response.contains("Invalid HTTP request received."));
                }
            }
            Result::<()>::Ok(())
        }
        .await;
        let _ = shutdown.send(());
        server.await??;
        outcome
    }
}
