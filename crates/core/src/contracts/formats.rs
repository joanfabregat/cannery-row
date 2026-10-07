//! The source's two format predicates used by published schemas.
//!
//! Python's RFC3339-shaped offset allows minutes up to 99 when the resulting
//! UTC offset remains less than one day. A strict RFC3339 parser would reject
//! accepted source documents. No timestamp is constructed or normalized here.
use chrono::NaiveDate;

pub(super) fn options<'a>() -> jsonschema::ValidationOptions<'a> {
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .offline()
        .should_validate_formats(true)
        .with_format("date", date)
        .with_format("date-time", date_time)
}

fn number(bytes: &[u8]) -> Option<u32> {
    bytes.iter().try_fold(0, |value, byte| {
        byte.is_ascii_digit()
            .then(|| value * 10 + u32::from(byte - b'0'))
    })
}
pub(super) fn date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let Some(year) = number(&bytes[..4]).and_then(|year| i32::try_from(year).ok()) else {
        return false;
    };
    let (Some(month), Some(day)) = (number(&bytes[5..7]), number(&bytes[8..])) else {
        return false;
    };
    year != 0 && NaiveDate::from_ymd_opt(year, month, day).is_some()
}
pub(super) fn date_time(value: &str) -> bool {
    let bytes = value.as_bytes();
    if !value.is_ascii()
        || bytes.len() < 20
        || !date(&value[..10])
        || !matches!(bytes[10], b'T' | b't')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return false;
    }
    if number(&bytes[11..13]).is_none_or(|v| v >= 24)
        || number(&bytes[14..16]).is_none_or(|v| v >= 60)
        || number(&bytes[17..19]).is_none_or(|v| v >= 60)
    {
        return false;
    }
    let mut position = 19;
    if bytes[position] == b'.' {
        position += 1;
        let start = position;
        while bytes.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
        if start == position {
            return false;
        }
    }
    let tail = &bytes[position..];
    if matches!(tail, [b'Z' | b'z']) {
        return true;
    }
    if tail.len() != 6 || !matches!(tail[0], b'+' | b'-') || tail[3] != b':' {
        return false;
    }
    match (number(&tail[1..3]), number(&tail[4..])) {
        (Some(hours), Some(minutes)) => hours < 24 && hours * 60 + minutes < 1440,
        _ => false,
    }
}
