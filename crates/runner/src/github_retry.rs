//! Bounded GitHub retry decisions; HTTP, credentials and sleeping belong to the caller.
use num_traits::ToPrimitive;

pub const DEFAULT_MAX_RATE_LIMIT_WAIT: f64 = 60.0;
const MAX_EXACT_SECONDS: u64 = (1_u64 << 53) - 1;

/// Decoded, case-insensitive header lookup results supplied by the HTTP adapter.
/// Values are kept out of diagnostics.
#[derive(Default)]
pub struct RateHeaders<'a> {
    pub retry_after: Option<&'a String>,
    pub remaining: Option<&'a String>,
    pub reset: Option<&'a String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RetryError {
    #[error("GitHub retry value must be bounded and finite")]
    Value,
}

fn seconds(value: Option<&String>) -> Result<Option<f64>, RetryError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim_matches([' ', '\t']);
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(None);
    }
    let number = value.parse::<u64>().map_err(|_| RetryError::Value)?;
    if number > MAX_EXACT_SECONDS {
        return Err(RetryError::Value);
    }
    number.to_f64().map(Some).ok_or(RetryError::Value)
}

/// Retry-After takes precedence over a zero-remaining reset. Only usable reset
/// headers consult the clock; a 429 without hints waits the default 60 seconds.
/// Numeric hints accept unsigned ASCII decimals and HTTP optional whitespace.
///
/// # Errors
/// Refuses numeric overflow and a nonfinite or negative clock value.
pub fn rate_limit_wait(
    status: u16,
    headers: &RateHeaders<'_>,
    clock: impl FnOnce() -> f64,
) -> Result<Option<f64>, RetryError> {
    if !matches!(status, 403 | 429) {
        return Ok(None);
    }
    if let Some(retry) = seconds(headers.retry_after)? {
        return Ok(Some(retry));
    }
    if headers.remaining.is_some_and(|value| value == "0")
        && let Some(reset) = seconds(headers.reset)?
    {
        let now = clock();
        if !now.is_finite() || now < 0.0 {
            return Err(RetryError::Value);
        }
        return Ok(Some((reset - now).max(0.0) + 1.0));
    }
    Ok((status == 429).then_some(DEFAULT_MAX_RATE_LIMIT_WAIT))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Decision {
    Return,
    RefreshCredential,
    Sleep { seconds: f64 },
    RefuseRateLimit { seconds: f64 },
}

/// One state per API GET, including all its retries; never shared across requests.
#[derive(Default)]
pub struct RetryState {
    refreshed: bool,
    waited: bool,
}
impl RetryState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A request has independent one-refresh and one-wait budgets. The caller
    /// refreshes its credential or sleeps before issuing the next request.
    ///
    /// # Errors
    /// Refuses invalid numeric hints, clocks and wait budgets without consuming a wait.
    pub fn response(
        &mut self,
        status: u16,
        token_present: bool,
        headers: &RateHeaders<'_>,
        max_wait: f64,
        clock: impl FnOnce() -> f64,
    ) -> Result<Decision, RetryError> {
        if status == 401 && token_present && !self.refreshed {
            self.refreshed = true;
            return Ok(Decision::RefreshCredential);
        }
        if let Some(seconds) = rate_limit_wait(status, headers, clock)? {
            if !max_wait.is_finite() || max_wait < 0.0 {
                return Err(RetryError::Value);
            }
            if self.waited || seconds > max_wait {
                return Ok(Decision::RefuseRateLimit { seconds });
            }
            self.waited = true;
            return Ok(Decision::Sleep { seconds });
        }
        Ok(Decision::Return)
    }
}
