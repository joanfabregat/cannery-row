//! Embedded web distribution and the optional filesystem override.
use std::{
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};
mod files;
mod ranges;

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../web/dist/"]
struct Distribution;

const RESERVED: &[&str] = &[
    "/api",
    "/auth",
    "/mcp",
    "/docs",
    "/redoc",
    "/openapi.json",
    "/__conformance",
];
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// Startup failure for a configured web distribution.
#[derive(Debug, thiserror::Error)]
#[error("web.dist_dir has no index.html: build the web app")]
pub struct WebError;

/// A validated web source. The default contains the production bundle.
#[derive(Clone)]
pub struct WebState {
    root: Option<PathBuf>,
}

impl WebState {
    /// Validate an override, or select the compiled distribution.
    ///
    /// # Errors
    /// Returns an error when the source has no regular, contained index file.
    pub fn new(dist_dir: Option<&str>) -> Result<Self, WebError> {
        let root = if let Some(directory) = dist_dir {
            let root = Path::new(directory).canonicalize().map_err(|_| WebError)?;
            let index = root
                .join("index.html")
                .canonicalize()
                .map_err(|_| WebError)?;
            if !index.starts_with(&root) || !index.is_file() {
                return Err(WebError);
            }
            Some(root)
        } else {
            if Distribution::get("index.html").is_none() {
                return Err(WebError);
            }
            None
        };
        Ok(Self { root })
    }

    /// Router containing only the web fallback, suitable for merging with API routes.
    pub fn router(self) -> Router {
        Router::new().fallback(fallback).with_state(self)
    }

    /// Answer an unmatched URI path. Existing handlers and method errors take precedence.
    pub async fn serve(&self, method: &Method, path: &str) -> Response {
        self.serve_request(method, path, &HeaderMap::new()).await
    }

    /// Answer an unmatched path, including the existing file Range semantics.
    pub async fn serve_request(
        &self,
        method: &Method,
        path: &str,
        request_headers: &HeaderMap,
    ) -> Response {
        let Some(path) = decode_path(path) else {
            return not_found(method);
        };
        if !matches!(*method, Method::GET | Method::HEAD)
            || RESERVED
                .iter()
                .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
        {
            return not_found(method);
        }
        let relative = path.trim_start_matches('/');
        let found = self.find(relative, path.starts_with("/assets/")).await;
        let (file, cache) = if path.starts_with("/assets/") {
            let Some(file) = found else {
                return not_found(method);
            };
            (file, IMMUTABLE)
        } else if let Some(file) = found {
            (file, "no-cache")
        } else {
            if file_name(path.rsplit('/').next().unwrap_or_default()) {
                return not_found(method);
            }
            let Some(file) = self.find("index.html", false).await else {
                return not_found(method);
            };
            (file, "no-cache")
        };
        files::response(file, method, request_headers, cache)
    }

    async fn find(&self, relative: &str, asset: bool) -> Option<files::File> {
        if relative.is_empty() || relative.contains('\0') {
            return None;
        }
        if let Some(root) = &self.root {
            let candidate = tokio::fs::canonicalize(root.join(relative)).await.ok()?;
            if !candidate.starts_with(root)
                || (asset && !candidate.starts_with(root.join("assets")))
            {
                return None;
            }
            let metadata = tokio::fs::metadata(&candidate).await.ok()?;
            if !metadata.is_file() {
                return None;
            }
            let name = candidate.file_name()?.to_str()?.to_owned();
            Some(files::File {
                source: files::Source::Filesystem(candidate),
                name,
                size: metadata.len(),
                modified: metadata.modified().ok()?,
            })
        } else {
            let mut segments = Vec::new();
            for segment in relative.split('/') {
                match segment {
                    "" | "." => {}
                    ".." => {
                        segments.pop()?;
                    }
                    value => segments.push(value),
                }
            }
            let name = segments.join("/");
            if asset && !name.starts_with("assets/") {
                return None;
            }
            let embedded = Distribution::get(&name)?;
            Some(files::File {
                size: u64::try_from(embedded.data.len()).ok()?,
                modified: UNIX_EPOCH.checked_add(Duration::from_secs(
                    embedded.metadata.last_modified().unwrap_or(0),
                ))?,
                source: files::Source::Embedded(embedded.data),
                name,
            })
        }
    }
}

fn security_headers(headers: &mut HeaderMap, cache: &str) {
    headers.insert(
        header::CACHE_CONTROL,
        cache
            .parse()
            .unwrap_or_else(|_| header::HeaderValue::from_static("no-cache")),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        header::HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    headers.insert(
        header::X_FRAME_OPTIONS,
        header::HeaderValue::from_static("DENY"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        header::HeaderValue::from_static("frame-ancestors 'none'"),
    );
}

async fn fallback(
    State(state): State<WebState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    state.serve_request(&method, uri.path(), &headers).await
}

fn not_found(method: &Method) -> Response {
    let mut response = (
        StatusCode::NOT_FOUND,
        axum::Json(serde_json::json!({"detail":"Not Found"})),
    )
        .into_response();
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        header::HeaderValue::from_static("22"),
    );
    if *method == Method::HEAD {
        *response.body_mut() = Body::empty();
    }
    response
}

fn decode_path(path: &str) -> Option<String> {
    let mut bytes = path.bytes();
    let mut decoded = Vec::with_capacity(path.len());
    while let Some(byte) = bytes.next() {
        decoded.push(if byte == b'%' {
            let high = char::from(bytes.next()?).to_digit(16)?;
            let low = char::from(bytes.next()?).to_digit(16)?;
            u8::try_from(high * 16 + low).ok()?
        } else {
            byte
        });
    }
    if decoded.contains(&0) {
        return None;
    }
    String::from_utf8(decoded).ok()
}

fn file_name(name: &str) -> bool {
    let Some((_, suffix)) = name.rsplit_once('.') else {
        return false;
    };
    (1..=8).contains(&suffix.len())
        && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && suffix.bytes().any(|byte| byte.is_ascii_alphabetic())
}

fn media_type(name: &str) -> header::HeaderValue {
    let suffix = name
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let media = match suffix.as_str() {
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "json" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        _ => mime_guess::from_path(name)
            .first_raw()
            .unwrap_or("application/octet-stream"),
    };
    let media = if media.starts_with("text/") && !media.contains("charset=") {
        format!("{media}; charset=utf-8")
    } else {
        media.to_owned()
    };
    media
        .parse()
        .unwrap_or_else(|_| header::HeaderValue::from_static("text/plain; charset=utf-8"))
}
