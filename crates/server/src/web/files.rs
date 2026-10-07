use super::{media_type, ranges, security_headers};
use axum::{
    body::Body,
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use md5::{Digest, Md5};
use std::{
    borrow::Cow,
    collections::VecDeque,
    io,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
const HEX: &[u8; 16] = b"0123456789abcdef";

pub(super) enum Source {
    Filesystem(PathBuf),
    Embedded(Cow<'static, [u8]>),
}
pub(super) struct File {
    pub source: Source,
    pub name: String,
    pub size: u64,
    pub modified: SystemTime,
}

pub(super) fn response(file: File, method: &Method, request: &HeaderMap, cache: &str) -> Response {
    let etag = metadata_etag(file.modified, file.size);
    let Some(modified) = modified_date(file.modified) else {
        return plain_error(
            method,
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            None,
        );
    };
    let media = media_type(&file.name);
    let mut headers = HeaderMap::new();
    security_headers(&mut headers, cache);
    set(&mut headers, header::ETAG, &etag);
    set(&mut headers, header::LAST_MODIFIED, &modified);
    headers.insert(
        header::ACCEPT_RANGES,
        header::HeaderValue::from_static("bytes"),
    );
    headers.insert(header::CONTENT_TYPE, media.clone());
    let selected = if let Some(value) = request.get(header::RANGE)
        && request.get(header::IF_RANGE).is_none_or(|value| {
            value.as_bytes() == etag.as_bytes() || value.as_bytes() == modified.as_bytes()
        }) {
        // Starlette's Headers decodes field bytes as Latin-1, including NBSP.
        let value: String = value
            .as_bytes()
            .iter()
            .map(|byte| char::from(*byte))
            .collect();
        match ranges::parse(&value, file.size) {
            Ok(ranges) => ranges,
            Err(ranges::Error::Malformed(message)) => {
                return plain_error(method, StatusCode::BAD_REQUEST, message, None);
            }
            Err(ranges::Error::Unsatisfiable) => {
                return plain_error(
                    method,
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    "",
                    Some(file.size),
                );
            }
        }
    } else {
        Vec::new()
    };
    let mut segments = VecDeque::new();
    let (status, length) = if selected.is_empty() {
        segments.push_back(Segment::Data(0, file.size));
        (StatusCode::OK, u128::from(file.size))
    } else if let [(start, end)] = selected.as_slice() {
        set(
            &mut headers,
            header::CONTENT_RANGE,
            &format!("bytes {start}-{}/{size}", end - 1, size = file.size),
        );
        segments.push_back(Segment::Data(*start, *end));
        (StatusCode::PARTIAL_CONTENT, u128::from(end - start))
    } else {
        let mut random = [0_u8; 13];
        if getrandom::fill(&mut random).is_err() {
            return plain_error(
                method,
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                None,
            );
        }
        let mut boundary = String::with_capacity(26);
        for byte in random {
            boundary.push(char::from(HEX[usize::from(byte >> 4)]));
            boundary.push(char::from(HEX[usize::from(byte & 15)]));
        }
        set(
            &mut headers,
            header::CONTENT_TYPE,
            &format!("multipart/byteranges; boundary={boundary}"),
        );
        let mut length = 0_u128;
        for (start, end) in selected {
            let prefix = format!("--{boundary}\r\nContent-Type: {}\r\nContent-Range: bytes {start}-{}/{size}\r\n\r\n", media.to_str().unwrap_or("application/octet-stream"), end - 1, size=file.size).into_bytes();
            length += prefix.len() as u128 + u128::from(end - start) + 2;
            segments.push_back(Segment::Bytes(prefix));
            segments.push_back(Segment::Data(start, end));
            segments.push_back(Segment::Bytes(b"\r\n".to_vec()));
        }
        let closing = format!("--{boundary}--").into_bytes();
        length += closing.len() as u128;
        segments.push_back(Segment::Bytes(closing));
        (StatusCode::PARTIAL_CONTENT, length)
    };
    set(&mut headers, header::CONTENT_LENGTH, &length.to_string());
    let body = if *method == Method::HEAD {
        Body::empty()
    } else {
        stream(file.source, segments)
    };
    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

fn set(headers: &mut HeaderMap, name: header::HeaderName, value: &str) {
    if let Ok(value) = value.parse() {
        headers.insert(name, value);
    }
}

fn plain_error(
    method: &Method,
    status: StatusCode,
    text: &'static str,
    size: Option<u64>,
) -> Response {
    let mut response = (status, text).into_response();
    set(
        response.headers_mut(),
        header::CONTENT_LENGTH,
        &text.len().to_string(),
    );
    if let Some(size) = size {
        set(
            response.headers_mut(),
            header::CONTENT_RANGE,
            &format!("bytes */{size}"),
        );
    }
    if *method == Method::HEAD {
        *response.body_mut() = Body::empty();
    }
    response
}

fn metadata_etag(modified: SystemTime, size: u64) -> String {
    // MD5 is only the existing weak file-metadata cache validator, never a
    // security digest or a verifier of file contents.
    let seconds = python_mtime(modified);
    let seconds = python_float(seconds);
    format!("\"{:x}\"", Md5::digest(format!("{seconds}-{size}")))
}

#[allow(clippy::cast_precision_loss)] // CPython stat converts seconds/nanoseconds separately.
fn python_mtime(modified: SystemTime) -> f64 {
    let (seconds, nanos) = match modified.duration_since(UNIX_EPOCH) {
        Ok(duration) => (duration.as_secs() as f64, duration.subsec_nanos()),
        Err(error) => {
            let duration = error.duration();
            if duration.subsec_nanos() == 0 {
                (-(duration.as_secs() as f64), 0)
            } else {
                (
                    -(duration.as_secs() as f64) - 1.0,
                    1_000_000_000 - duration.subsec_nanos(),
                )
            }
        }
    };
    seconds + f64::from(nanos) * 1e-9
}

fn python_float(value: f64) -> String {
    if value != 0.0 && (value.abs() < 1e-4 || value.abs() >= 1e16) {
        let scientific = format!("{value:e}");
        if let Some((mantissa, exponent)) = scientific.split_once('e')
            && let Ok(exponent) = exponent.parse::<i32>()
        {
            return format!("{mantissa}e{exponent:+03}");
        }
    }
    let mut value = value.to_string();
    if !value.contains('.') {
        value.push_str(".0");
    }
    value
}

// httpdate panics for pre-epoch timestamps. Shift by Gregorian 400-year
// cycles before formatting, then restore the year; dates outside Python's
// datetime range become a redacted server error instead of panicking.
#[allow(clippy::cast_possible_truncation)] // Finite bounded integral seconds checked before conversion.
fn modified_date(modified: SystemTime) -> Option<String> {
    const CYCLE_SECONDS: i128 = 146_097 * 86_400;
    let seconds = python_mtime(modified);
    if !(-62_135_596_800.0..253_402_300_800.0).contains(&seconds) {
        return None;
    }
    // email.utils.formatdate uses datetime.fromtimestamp, which rounds the
    // fractional second to microseconds before dropping it from the header.
    let whole = seconds.floor();
    let carry = ((seconds - whole) * 1_000_000.0).round_ties_even() >= 1_000_000.0;
    let seconds = whole as i128 + i128::from(carry);
    let cycle = seconds.div_euclid(CYCLE_SECONDS);
    let shifted = u64::try_from(seconds.rem_euclid(CYCLE_SECONDS)).ok()?;
    let mut date =
        httpdate::fmt_http_date(UNIX_EPOCH.checked_add(std::time::Duration::from_secs(shifted))?);
    let year = date.get(12..16)?.parse::<i128>().ok()? + cycle * 400;
    if !(1..=9999).contains(&year) {
        return None;
    }
    date.replace_range(12..16, &format!("{year:04}"));
    Some(date)
}

enum Segment {
    Bytes(Vec<u8>),
    Data(u64, u64),
}
struct Stream {
    source: Source,
    opened: Option<tokio::fs::File>,
    segments: VecDeque<Segment>,
}

fn stream(source: Source, segments: VecDeque<Segment>) -> Body {
    Body::from_stream(futures_util::stream::try_unfold(
        Stream {
            source,
            opened: None,
            segments,
        },
        |mut state| async move {
            loop {
                let Some(segment) = state.segments.pop_front() else {
                    return Ok::<_, io::Error>(None);
                };
                match segment {
                    Segment::Bytes(bytes) => return Ok(Some((bytes, state))),
                    Segment::Data(start, end) => {
                        if start == end {
                            continue;
                        }
                        let length = usize::try_from((end - start).min(64 * 1024))
                            .map_err(io::Error::other)?;
                        let bytes = match &state.source {
                            Source::Embedded(bytes) => {
                                let start = usize::try_from(start).map_err(io::Error::other)?;
                                bytes
                                    .get(start..start + length)
                                    .ok_or_else(|| io::Error::other("embedded file bounds"))?
                                    .to_vec()
                            }
                            Source::Filesystem(path) => {
                                if state.opened.is_none() {
                                    state.opened = Some(tokio::fs::File::open(path).await?);
                                }
                                let file = state
                                    .opened
                                    .as_mut()
                                    .ok_or_else(|| io::Error::other("file unavailable"))?;
                                file.seek(io::SeekFrom::Start(start)).await?;
                                let mut bytes = vec![0; length];
                                let read = file.read(&mut bytes).await?;
                                if read == 0 {
                                    return Err(io::Error::new(
                                        io::ErrorKind::UnexpectedEof,
                                        "web file shortened",
                                    ));
                                }
                                bytes.truncate(read);
                                bytes
                            }
                        };
                        let next = start + u64::try_from(bytes.len()).map_err(io::Error::other)?;
                        if next < end {
                            state.segments.push_front(Segment::Data(next, end));
                        }
                        return Ok(Some((bytes, state)));
                    }
                }
            }
        },
    ))
}
