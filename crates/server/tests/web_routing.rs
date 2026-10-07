use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
    routing::get,
};
use cannery_server::web::WebState;
use std::{
    error::Error,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use tower::ServiceExt;

type Result<T = ()> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
const INDEX: &str = "<!doctype html><title>owned web fixture</title>";
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    directory: PathBuf,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Result<Self> {
        let directory = std::env::temp_dir().join(format!(
            "cannery-web-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory)?;
        let root = directory.join("dist");
        std::fs::create_dir_all(root.join("assets"))?;
        std::fs::write(root.join("index.html"), INDEX)?;
        std::fs::write(root.join("assets/chunk.js"), "console.log(1)")?;
        std::fs::write(root.join("logo.png"), b"\x89PNG\r\n")?;
        std::fs::write(root.join("manifest.webmanifest"), "{}")?;
        std::fs::write(directory.join("secret.txt"), "outside private sentinel")?;
        Ok(Self { directory, root })
    }
    fn state(&self) -> Result<WebState> {
        Ok(WebState::new(self.root.to_str())?)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

async fn request(
    app: Router,
    method: Method,
    path: &str,
) -> Result<(StatusCode, axum::http::HeaderMap, Vec<u8>)> {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())?,
        )
        .await?;
    let (parts, body) = response.into_parts();
    Ok((
        parts.status,
        parts.headers,
        to_bytes(body, 16 * 1024 * 1024).await?.to_vec(),
    ))
}

#[tokio::test]
async fn override_static_spa_and_head() -> Result {
    let fixture = Fixture::new()?;
    let app = fixture.state()?.router();
    for path in [
        "/",
        "/index.html",
        "/projects/demo",
        "/projects/v1.2",
        "/hypotheses/12.3",
    ] {
        let (status, headers, body) = request(app.clone(), Method::GET, path).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, INDEX.as_bytes());
        assert_eq!(headers["cache-control"], "no-cache");
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(headers["x-frame-options"], "DENY");
        assert_eq!(headers["content-security-policy"], "frame-ancestors 'none'");
        assert_eq!(
            headers["referrer-policy"],
            "strict-origin-when-cross-origin"
        );
        let (_, head, body) = request(app.clone(), Method::HEAD, path).await?;
        assert_eq!(body, [] as [u8; 0]);
        assert_eq!(head["content-length"], INDEX.len().to_string());
        assert_eq!(head["content-type"], headers["content-type"]);
    }
    for (path, content, mime, cache) in [
        (
            "/assets/chunk.js",
            b"console.log(1)".as_slice(),
            "text/javascript; charset=utf-8",
            "public, max-age=31536000, immutable",
        ),
        (
            "/logo.png",
            b"\x89PNG\r\n".as_slice(),
            "image/png",
            "no-cache",
        ),
        (
            "/manifest.webmanifest",
            b"{}".as_slice(),
            "application/manifest+json",
            "no-cache",
        ),
    ] {
        let (status, headers, body) = request(app.clone(), Method::GET, path).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, content);
        assert_eq!(headers["content-type"], mime);
        assert_eq!(headers["cache-control"], cache);
    }
    Ok(())
}

#[tokio::test]
async fn mime_fallback_preserves_text_charset_and_binary_types() -> Result {
    let fixture = Fixture::new()?;
    let app = fixture.state()?.router();
    for (suffix, media) in [
        ("tif", "image/tiff"),
        ("bmp", "image/bmp"),
        ("csv", "text/csv; charset=utf-8"),
        ("md", "text/markdown; charset=utf-8"),
        ("wasm", "application/wasm"),
        ("MJS", "text/javascript; charset=utf-8"),
        ("woff", "font/woff"),
        ("svg", "image/svg+xml"),
        ("unknown_suffix", "application/octet-stream"),
    ] {
        let name = format!("sample.{suffix}");
        std::fs::write(fixture.root.join(&name), "owned MIME fixture")?;
        let (status, headers, body) =
            request(app.clone(), Method::GET, &format!("/{name}")).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["content-type"], media, "{suffix}");
        assert_eq!(body, b"owned MIME fixture");
    }
    Ok(())
}

#[tokio::test]
async fn boundaries_missing_files_and_methods() -> Result {
    let fixture = Fixture::new()?;
    let app = Router::new()
        .route("/api/present", get(|| async { "actual handler" }))
        .merge(fixture.state()?.router());
    for path in [
        "/api",
        "/api/unknown",
        "/auth/unknown",
        "/mcp",
        "/docs/unknown",
        "/redoc",
        "/openapi.json",
        "/__conformance/sweep",
        "/assets/missing.js",
        "/assets/missing",
        "/favicon.ico",
        "/robots.txt",
        "/projects/report.pdf",
        "/projects/file.a1234567",
    ] {
        let (status, headers, body) = request(app.clone(), Method::GET, path).await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(headers["content-type"], "application/json");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body)?,
            serde_json::json!({"detail":"Not Found"})
        );
        let (status, _, body) = request(app.clone(), Method::HEAD, path).await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, [] as [u8; 0]);
    }
    for path in [
        "/apiculture",
        "/mcp-other",
        "/docs-other",
        "/__conformance-other",
        "/projects/file.123",
        "/projects/file.abcdefghi",
    ] {
        assert_eq!(
            request(app.clone(), Method::GET, path).await?.0,
            StatusCode::OK,
            "{path}"
        );
    }
    assert_eq!(
        request(app.clone(), Method::GET, "/api/present").await?.2,
        b"actual handler"
    );
    assert_eq!(
        request(app.clone(), Method::PUT, "/api/present").await?.0,
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        request(app, Method::POST, "/projects").await?.0,
        StatusCode::NOT_FOUND
    );
    Ok(())
}

#[tokio::test]
async fn traversal_symlinks_and_asset_containment() -> Result {
    let fixture = Fixture::new()?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            fixture.directory.join("secret.txt"),
            fixture.root.join("escape.txt"),
        )?;
        std::os::unix::fs::symlink(
            fixture.root.join("index.html"),
            fixture.root.join("assets/index-link.html"),
        )?;
        std::os::unix::fs::symlink(
            fixture.root.join("assets/chunk.js"),
            fixture.root.join("assets/allowed.js"),
        )?;
    }
    let state = fixture.state()?;
    for path in [
        "/../secret.txt",
        "/assets/../../secret.txt",
        "/%2e%2e/secret.txt",
        "/assets/%2e%2e/%2e%2e/secret.txt",
        "/escape.txt",
        "/assets/index-link.html",
        "/assets/../index.html",
        "/logo.png%00",
        "/invalid%FF",
    ] {
        let response = state.serve(&Method::GET, path).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        let body = to_bytes(response.into_body(), 4096).await?;
        assert!(!String::from_utf8_lossy(&body).contains("outside private sentinel"));
    }
    #[cfg(unix)]
    assert_eq!(
        state
            .serve(&Method::GET, "/assets/allowed.js")
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        state.serve(&Method::GET, "/assets").await.status(),
        StatusCode::OK
    );
    let absent = fixture.directory.join("absent");
    assert!(WebState::new(absent.to_str()).is_err());
    assert!(WebState::new(fixture.directory.to_str()).is_err());
    Ok(())
}

#[tokio::test]
async fn embedded_bundle_contains_real_linked_assets() -> Result {
    let app = WebState::new(None)?.router();
    let (status, _, index) = request(app.clone(), Method::GET, "/").await?;
    assert_eq!(status, StatusCode::OK);
    let html = String::from_utf8(index)?;
    assert!(html.contains("<html"));
    let assets: Vec<_> = html
        .split('"')
        .filter(|value| value.starts_with("/assets/"))
        .collect();
    assert_ne!(assets, [] as [&str; 0]);
    for path in assets {
        let (status, headers, body) = request(app.clone(), Method::GET, path).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            headers["cache-control"],
            "public, max-age=31536000, immutable"
        );
        assert_ne!(body, [] as [u8; 0]);
    }
    assert_eq!(
        request(app, Method::GET, "/projects/embedded-default")
            .await?
            .0,
        StatusCode::OK
    );
    Ok(())
}
