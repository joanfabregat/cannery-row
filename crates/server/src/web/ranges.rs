//! HTTP byte ranges with bounded unsigned 64-bit positions.
pub(super) enum Error {
    Malformed(&'static str),
    Unsatisfiable,
}

pub(super) fn parse(value: &str, size: u64) -> Result<Vec<(u64, u64)>, Error> {
    let Some((unit, value)) = value.split_once('=') else {
        return Err(Error::Malformed("Malformed range header."));
    };
    if !unit.trim().eq_ignore_ascii_case("bytes") {
        return Err(Error::Malformed("Only support bytes range"));
    }
    if value.bytes().filter(|byte| *byte == b',').count() >= 100 {
        return Ok(Vec::new());
    }
    let length = i128::from(size);
    let mut ranges = Vec::new();
    for part in value.split(',') {
        let Some((start, end)) = part.trim().split_once('-') else {
            continue;
        };
        let (start, end) = (start.trim(), end.trim());
        let bounds = if start.is_empty() {
            integer(end).map(|suffix| ((length.saturating_sub(suffix)).max(0), length))
        } else {
            integer(start).and_then(|start| {
                if end.is_empty() {
                    Some((start, length))
                } else {
                    integer(end).map(|end| {
                        (
                            start,
                            if end < length {
                                end.saturating_add(1)
                            } else {
                                length
                            },
                        )
                    })
                }
            })
        };
        if let Some(bounds) = bounds {
            ranges.push(bounds);
        }
    }
    if ranges.is_empty() {
        return Err(Error::Malformed("Range header: range must be requested"));
    }
    if ranges
        .iter()
        .any(|(start, _)| *start < 0 || *start >= length)
    {
        return Err(Error::Unsatisfiable);
    }
    if ranges.iter().any(|(start, end)| start >= end) {
        return Err(Error::Malformed(
            "Range header: start must be less than end",
        ));
    }
    ranges.sort_unstable();
    let mut result: Vec<(u64, u64)> = Vec::new();
    for (start, end) in ranges {
        let (Ok(start), Ok(end)) = (u64::try_from(start), u64::try_from(end)) else {
            return Err(Error::Unsatisfiable);
        };
        if let Some((_, last_end)) = result.last_mut()
            && start <= *last_end
        {
            *last_end = (*last_end).max(end);
        } else {
            result.push((start, end));
        }
    }
    Ok(result)
}

// HTTP byte positions are bounded unsigned ASCII decimal integers.
fn integer(value: &str) -> Option<i128> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse::<u64>().ok().map(i128::from)
}
