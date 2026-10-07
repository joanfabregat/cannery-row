//! Stable sorting with fallible domain comparisons.
use std::cmp::Ordering;
/// # Errors
/// Returns a domain comparison failure; the input is unchanged on failure.
pub fn sort<T: Clone, E>(
    items: &mut [T],
    mut less: impl FnMut(&T, &T) -> Result<bool, E>,
) -> Result<(), E> {
    let mut sorted = items.to_vec();
    let mut failure = None;
    sorted.sort_by(|a, b| {
        if failure.is_some() {
            return Ordering::Equal;
        }
        match less(a, b).and_then(|ab| {
            if ab {
                Ok(Ordering::Less)
            } else {
                less(b, a).map(|ba| {
                    if ba {
                        Ordering::Greater
                    } else {
                        Ordering::Equal
                    }
                })
            }
        }) {
            Ok(order) => order,
            Err(error) => {
                failure = Some(error);
                Ordering::Equal
            }
        }
    });
    if let Some(error) = failure {
        return Err(error);
    }
    items.clone_from_slice(&sorted);
    Ok(())
}
