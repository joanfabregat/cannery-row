//! Native UTF-8 metric filter syntax, before repository consumption.

/// Dimension names retain first occurrence order; each value occurs once.
pub type Filters = Vec<(String, Vec<String>)>;

/// Only the failing position is retained, never the user's filter text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("metric filter must contain a dimension and value")]
pub struct FilterError {
    pub index: usize,
}

fn split(item: &str) -> Option<(String, String)> {
    let (name, value) = item.split_once(':')?;
    if !(1..=64).contains(&name.len())
        || !name.as_bytes()[0].is_ascii_lowercase()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return None;
    }
    if value.is_empty() || value.chars().any(char::is_control) {
        return None;
    }
    Some((name.to_owned(), value.to_owned()))
}

/// Parse `dimension:value` with an ASCII dimension and nonempty UTF-8 value.
/// Control characters are rejected; first occurrence order is preserved.
/// # Errors
/// Returns the first invalid filter's zero-based position.
pub fn parse_filters<'a>(
    raw: impl IntoIterator<Item = &'a String>,
) -> Result<Filters, FilterError> {
    let mut filters: Filters = Vec::new();
    for (index, item) in raw.into_iter().enumerate() {
        let (name, value) = split(item).ok_or(FilterError { index })?;
        match filters.iter_mut().find(|(existing, _)| existing == &name) {
            Some((_, values)) => {
                if !values.contains(&value) {
                    values.push(value);
                }
            }
            None => filters.push((name, vec![value])),
        }
    }
    Ok(filters)
}
