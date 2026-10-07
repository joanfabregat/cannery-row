//! Limit-plus-one pagination; database repositories own each cursor's order.

use serde::Serialize;

pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 200;

#[derive(Debug, Serialize)]
pub struct Page<T, C> {
    pub items: Vec<T>,
    pub next_before: Option<C>,
}

/// Split an ordered limit-plus-one result after validating the request limit.
#[must_use]
pub fn paginate<T, C>(mut rows: Vec<T>, limit: usize, cursor: impl FnOnce(&T) -> C) -> Page<T, C> {
    let next_before = if rows.len() > limit {
        rows.get(limit.saturating_sub(1)).map(cursor)
    } else {
        None
    };
    rows.truncate(limit);
    Page {
        items: rows,
        next_before,
    }
}
