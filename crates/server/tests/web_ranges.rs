use axum::{
    body::{Body, to_bytes},
    http::{HeaderMap, Method, Request, StatusCode},
};
use cannery_server::web::WebState;
use futures_util::StreamExt;
use std::{
    error::Error,
    fs::FileTimes,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, UNIX_EPOCH},
};
use tower::ServiceExt;

type Result<T = ()> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "cannery-web-range-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("index.html"), "owned index")?;
        std::fs::write(root.join("range.txt"), "0123456789")?;
        std::fs::File::open(root.join("range.txt"))?.set_times(
            FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_millis(123_500)),
        )?;
        Ok(Self(root))
    }
    fn state(&self) -> Result<WebState> {
        Ok(WebState::new(self.0.to_str())?)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn request(
    state: &WebState,
    method: Method,
    headers: &[(&str, &str)],
) -> Result<(StatusCode, HeaderMap, Vec<u8>)> {
    let mut request = Request::builder().method(method).uri("/range.txt");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = state
        .clone()
        .router()
        .oneshot(request.body(Body::empty())?)
        .await?;
    let (parts, body) = response.into_parts();
    Ok((
        parts.status,
        parts.headers,
        to_bytes(body, 1024 * 1024).await?.to_vec(),
    ))
}

#[tokio::test]
async fn metadata_headers_and_if_range_match_frozen_file_response() -> Result {
    let fixture = Fixture::new()?;
    let state = fixture.state()?;
    let (status, headers, body) = request(&state, Method::GET, &[]).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"0123456789");
    assert_eq!(headers["content-length"], "10");
    assert_eq!(headers["accept-ranges"], "bytes");
    assert_eq!(headers["last-modified"], "Thu, 01 Jan 1970 00:02:03 GMT");
    // Frozen Python hashlib.md5(b"123.5-10", usedforsecurity=False).
    assert_eq!(headers["etag"], "\"45a1f17e716095e55d49d793efa2d656\"");
    for condition in [
        headers["etag"].to_str()?,
        headers["last-modified"].to_str()?,
    ] {
        let (status, partial, body) = request(
            &state,
            Method::GET,
            &[("range", "bytes=1-3"), ("if-range", condition)],
        )
        .await?;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(partial["content-range"], "bytes 1-3/10");
        assert_eq!(partial["etag"], headers["etag"]);
        assert_eq!(body, b"123");
    }
    let (status, _, body) = request(
        &state,
        Method::GET,
        &[("range", "malformed"), ("if-range", "different")],
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"0123456789");
    // FileResponse alone does not implement conditional 304 responses.
    assert_eq!(
        request(
            &state,
            Method::GET,
            &[("if-none-match", headers["etag"].to_str()?)]
        )
        .await?
        .0,
        StatusCode::OK
    );
    let (_, head, body) = request(&state, Method::HEAD, &[]).await?;
    assert_eq!(body, [] as [u8; 0]);
    assert_eq!(head, headers);
    Ok(())
}

#[tokio::test]
async fn metadata_nanosecond_rounding_matches_python_stat_and_datetime() -> Result {
    let fixture = Fixture::new()?;
    let state = fixture.state()?;
    // Exported from actual CPython os.stat + frozen FileResponse metadata logic.
    for (nanos, etag, date) in [
        (
            1_i64,
            "3727fd1fe8b671996f54198f8ea42a48",
            "Thu, 01 Jan 1970 00:00:00 GMT",
        ),
        (
            1000,
            "a3342ec3aef7f22b1289bad0f8a21132",
            "Thu, 01 Jan 1970 00:00:00 GMT",
        ),
        (
            999_999_999,
            "fb5ce19ff70705e9d7a7d1c4261da645",
            "Thu, 01 Jan 1970 00:00:01 GMT",
        ),
        (
            1_700_000_000_000_000_001,
            "0f134b9d66a9d3b04370ee502d145b2d",
            "Tue, 14 Nov 2023 22:13:20 GMT",
        ),
        (
            1_700_000_000_999_999_999,
            "c5570763b1cb5bbf01aace503b6e9c4b",
            "Tue, 14 Nov 2023 22:13:21 GMT",
        ),
        (
            -1,
            "fde5617964d2907267f29c7761663ace",
            "Thu, 01 Jan 1970 00:00:00 GMT",
        ),
        (
            -1000,
            "85786f76e3de0e2cfdd8f37aa47479d9",
            "Wed, 31 Dec 1969 23:59:59 GMT",
        ),
        (
            -999_999_999,
            "800232aaa46b31fc4a70a4973639ae54",
            "Wed, 31 Dec 1969 23:59:59 GMT",
        ),
    ] {
        let duration = Duration::from_nanos(nanos.unsigned_abs());
        let modified = if nanos >= 0 {
            UNIX_EPOCH + duration
        } else {
            UNIX_EPOCH - duration
        };
        std::fs::File::open(fixture.0.join("range.txt"))?
            .set_times(FileTimes::new().set_modified(modified))?;
        let (_, headers, _) = request(&state, Method::HEAD, &[]).await?;
        assert_eq!(headers["etag"], format!("\"{etag}\""), "{nanos}");
        assert_eq!(headers["last-modified"], date, "{nanos}");
    }
    Ok(())
}

#[tokio::test]
async fn single_suffix_open_merged_and_ignored_ranges() -> Result {
    let fixture = Fixture::new()?;
    let state = fixture.state()?;
    for (range, expected, content_range) in [
        ("bytes=0-2", "012", "bytes 0-2/10"),
        ("bytes=000-002", "012", "bytes 0-2/10"),
        ("bytes=7-", "789", "bytes 7-9/10"),
        ("bytes=-3", "789", "bytes 7-9/10"),
        ("bytes=-99", "0123456789", "bytes 0-9/10"),
        ("bytes=8-99", "89", "bytes 8-9/10"),
        ("bytes=0-1,2-4,3-5", "012345", "bytes 0-5/10"),
        ("BYTES = 0 - 2, nonsense, -,", "012", "bytes 0-2/10"),
        ("bytes=0-1,0-1", "01", "bytes 0-1/10"),
    ] {
        let (status, headers, body) = request(&state, Method::GET, &[("range", range)]).await?;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT, "{range}");
        assert_eq!(body, expected.as_bytes(), "{range}");
        assert_eq!(headers["content-range"], content_range);
        assert_eq!(headers["content-length"], expected.len().to_string());
        let (status, head, body) = request(&state, Method::HEAD, &[("range", range)]).await?;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(head, headers);
        assert_eq!(body, [] as [u8; 0]);
    }
    let excessive = format!("bytes={}", vec!["0-0"; 101].join(","));
    let (status, headers, body) = request(&state, Method::GET, &[("range", &excessive)]).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(!headers.contains_key("content-range"));
    assert_eq!(body, b"0123456789");
    let mut latin1 = HeaderMap::new();
    latin1.insert(
        "range",
        axum::http::HeaderValue::from_bytes(b"bytes=\xa00-1\xa0")?,
    );
    let response = state
        .serve_request(&Method::GET, "/range.txt", &latin1)
        .await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(to_bytes(response.into_body(), 4096).await?.as_ref(), b"01");
    Ok(())
}

#[tokio::test]
async fn malformed_unsatisfiable_empty_and_multipart_ranges() -> Result {
    let fixture = Fixture::new()?;
    let state = fixture.state()?;
    for (range, status, message) in [
        ("bytes", StatusCode::BAD_REQUEST, "Malformed range header."),
        (
            "items=0-1",
            StatusCode::BAD_REQUEST,
            "Only support bytes range",
        ),
        (
            "bytes=invalid,-",
            StatusCode::BAD_REQUEST,
            "Range header: range must be requested",
        ),
        (
            "bytes=2-1",
            StatusCode::BAD_REQUEST,
            "Range header: start must be less than end",
        ),
        (
            "bytes=0--1",
            StatusCode::BAD_REQUEST,
            "Range header: range must be requested",
        ),
        ("bytes=10-", StatusCode::RANGE_NOT_SATISFIABLE, ""),
        ("bytes=-0", StatusCode::RANGE_NOT_SATISFIABLE, ""),
        ("bytes=0-1,20-30", StatusCode::RANGE_NOT_SATISFIABLE, ""),
        (
            "bytes=999999999999999999999999999999999999999999-",
            StatusCode::BAD_REQUEST,
            "Range header: range must be requested",
        ),
    ] {
        let (actual, headers, body) = request(&state, Method::GET, &[("range", range)]).await?;
        assert_eq!(actual, status, "{range}");
        assert_eq!(body, message.as_bytes());
        assert_eq!(headers["content-type"], "text/plain; charset=utf-8");
        assert!(!headers.contains_key("etag"));
        if status == StatusCode::RANGE_NOT_SATISFIABLE {
            assert_eq!(headers["content-range"], "bytes */10");
        }
        assert_eq!(
            request(&state, Method::HEAD, &[("range", range)]).await?.2,
            [] as [u8; 0]
        );
    }
    let (status, headers, body) =
        request(&state, Method::GET, &[("range", "bytes=7-9,0-1")]).await?;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    let boundary = headers["content-type"]
        .to_str()?
        .strip_prefix("multipart/byteranges; boundary=")
        .ok_or("multipart MIME")?;
    assert_eq!(boundary.len(), 26);
    assert!(boundary.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let expected = format!(
        "--{boundary}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Range: bytes 0-1/10\r\n\r\n01\r\n--{boundary}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Range: bytes 7-9/10\r\n\r\n789\r\n--{boundary}--"
    );
    assert_eq!(body, expected.as_bytes());
    assert_eq!(headers["content-length"], body.len().to_string());
    let (_, head, body) = request(&state, Method::HEAD, &[("range", "bytes=7-9,0-1")]).await?;
    assert_eq!(head["content-length"], headers["content-length"]);
    assert_eq!(body, [] as [u8; 0]);
    std::fs::write(fixture.0.join("range.txt"), "")?;
    assert_eq!(request(&state, Method::GET, &[]).await?.2, [] as [u8; 0]);
    let (status, headers, _) = request(&state, Method::GET, &[("range", "bytes=0-")]).await?;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(headers["content-range"], "bytes */0");
    Ok(())
}

#[tokio::test]
async fn filesystem_stream_is_chunked_and_pre_epoch_metadata_does_not_panic() -> Result {
    let fixture = Fixture::new()?;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(fixture.0.join("range.txt"))?;
    file.set_len(8 * 1024 * 1024)?;
    file.set_times(FileTimes::new().set_modified(UNIX_EPOCH - Duration::from_secs(1)))?;
    let response = fixture.state()?.serve(&Method::GET, "/range.txt").await;
    assert_eq!(
        response.headers()["last-modified"],
        "Wed, 31 Dec 1969 23:59:59 GMT"
    );
    assert_eq!(response.headers()["content-length"], "8388608");
    let mut stream = response.into_body().into_data_stream();
    let first = stream.next().await.ok_or("missing chunk")??;
    assert_eq!(first.len(), 64 * 1024);
    assert_eq!(&first[..10], b"0123456789");
    drop(stream);
    Ok(())
}

#[tokio::test]
async fn native_range_positions_are_unsigned_ascii_u64_and_preserve_large_suffixes() -> Result {
    let fixture = Fixture::new()?;
    let state = fixture.state()?;
    for value in [
        "bytes=+1-2",
        "bytes=1_0-",
        "bytes=0-1_0",
        "bytes=0-+2",
        "bytes=18446744073709551616-",
        "bytes=-18446744073709551616",
    ] {
        let (status, headers, body) = request(&state, Method::GET, &[("range", value)]).await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
        assert_eq!(body, b"Range header: range must be requested");
        assert!(!headers.contains_key("content-range"));
    }
    for value in [
        "bytes=0-18446744073709551615",
        "bytes=-18446744073709551615",
    ] {
        let (status, headers, body) = request(&state, Method::GET, &[("range", value)]).await?;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(headers["content-range"], "bytes 0-9/10");
        assert_eq!(body, b"0123456789");
    }
    let (status, headers, body) = request(
        &state,
        Method::GET,
        &[("range", "bytes=18446744073709551615-")],
    )
    .await?;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(headers["content-range"], "bytes */10");
    assert_eq!(body, [] as [u8; 0]);
    Ok(())
}
