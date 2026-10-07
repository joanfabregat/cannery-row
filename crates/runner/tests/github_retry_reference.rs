//! Authored GitHub header, clock and independent retry-budget contracts.
use cannery_runner::github_retry::{
    Decision, RateHeaders, RetryError, RetryState, rate_limit_wait,
};
use std::{cell::Cell, error::Error};
type Result = std::result::Result<(), Box<dyn Error>>;

#[test]
fn retry_after_has_precedence_and_only_reset_consults_the_clock() -> Result {
    let retry = " \t5\t ".to_owned();
    let remaining = "0".to_owned();
    let reset = "1010".to_owned();
    let calls = Cell::new(0);
    for status in [403, 429] {
        assert_eq!(
            rate_limit_wait(
                status,
                &RateHeaders {
                    retry_after: Some(&retry),
                    remaining: Some(&remaining),
                    reset: Some(&reset)
                },
                || {
                    calls.set(calls.get() + 1);
                    1000.0
                }
            )?,
            Some(5.0)
        );
    }
    assert_eq!(calls.get(), 0);
    assert_eq!(
        rate_limit_wait(
            403,
            &RateHeaders {
                remaining: Some(&remaining),
                reset: Some(&reset),
                ..RateHeaders::default()
            },
            || {
                calls.set(calls.get() + 1);
                1000.0
            }
        )?,
        Some(11.0)
    );
    assert_eq!(calls.get(), 1);
    assert_eq!(
        rate_limit_wait(
            429,
            &RateHeaders {
                remaining: Some(&remaining),
                reset: Some(&reset),
                ..RateHeaders::default()
            },
            || 2000.0
        )?,
        Some(1.0)
    );
    Ok(())
}

#[test]
fn missing_or_malformed_hints_keep_status_specific_fallbacks() -> Result {
    let calls = Cell::new(0);
    for status in [200, 401, 404, 500] {
        assert_eq!(
            rate_limit_wait(status, &RateHeaders::default(), || {
                calls.set(calls.get() + 1);
                0.0
            })?,
            None
        );
    }
    for hint in [
        None,
        Some(""),
        Some("-1"),
        Some("+1"),
        Some("1.5"),
        Some("١"),
        Some("１"),
        Some("²"),
        Some("1\n"),
        Some("NaN"),
        Some("Infinity"),
    ] {
        let hint = hint.map(str::to_owned);
        let headers = RateHeaders {
            retry_after: hint.as_ref(),
            ..RateHeaders::default()
        };
        assert_eq!(
            rate_limit_wait(403, &headers, || {
                calls.set(calls.get() + 1);
                0.0
            })?,
            None
        );
        assert_eq!(
            rate_limit_wait(429, &headers, || {
                calls.set(calls.get() + 1);
                0.0
            })?,
            Some(60.0)
        );
    }
    let reset = "1010".to_owned();
    for remaining in ["1", "00", " 0", "０"] {
        let remaining = remaining.to_owned();
        assert_eq!(
            rate_limit_wait(
                403,
                &RateHeaders {
                    remaining: Some(&remaining),
                    reset: Some(&reset),
                    ..RateHeaders::default()
                },
                || {
                    calls.set(calls.get() + 1);
                    0.0
                }
            )?,
            None
        );
    }
    assert_eq!(calls.get(), 0);
    Ok(())
}

#[test]

fn bounded_numbers_and_finite_clocks_refuse_unsafe_wait_values() -> Result {
    let largest = "9007199254740991".to_owned();
    assert!(
        rate_limit_wait(
            429,
            &RateHeaders {
                retry_after: Some(&largest),
                ..RateHeaders::default()
            },
            || 0.0
        )?
        .is_some_and(f64::is_finite)
    );
    for hint in ["9007199254740992", "18446744073709551616"] {
        let hint = hint.to_owned();
        assert_eq!(
            rate_limit_wait(
                429,
                &RateHeaders {
                    retry_after: Some(&hint),
                    ..RateHeaders::default()
                },
                || 0.0
            ),
            Err(RetryError::Value)
        );
    }
    let remaining = "0".to_owned();
    let reset = "1010".to_owned();
    let headers = RateHeaders {
        remaining: Some(&remaining),
        reset: Some(&reset),
        ..RateHeaders::default()
    };
    for now in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
        assert_eq!(
            rate_limit_wait(429, &headers, || now),
            Err(RetryError::Value)
        );
    }
    assert_eq!(rate_limit_wait(429, &headers, || 1000.5)?, Some(10.5));
    Ok(())
}

#[test]
fn credential_refresh_and_wait_budgets_are_independent_and_per_request() -> Result {
    let headers = RateHeaders::default();
    for statuses in [[401, 429], [429, 401]] {
        let mut state = RetryState::new();
        for status in statuses {
            let expected = if status == 401 {
                Decision::RefreshCredential
            } else {
                Decision::Sleep { seconds: 60.0 }
            };
            assert_eq!(
                state.response(status, true, &headers, 60.0, || 0.0)?,
                expected
            );
        }
        assert_eq!(
            state.response(401, true, &headers, 60.0, || 0.0)?,
            Decision::Return
        );
        assert_eq!(
            state.response(429, true, &headers, 60.0, || 0.0)?,
            Decision::RefuseRateLimit { seconds: 60.0 }
        );
        assert_eq!(
            state.response(200, true, &headers, 60.0, || 0.0)?,
            Decision::Return
        );
    }
    assert_eq!(
        RetryState::new().response(401, false, &headers, 60.0, || 0.0)?,
        Decision::Return
    );
    assert_eq!(
        RetryState::new().response(429, true, &headers, 59.0, || 0.0)?,
        Decision::RefuseRateLimit { seconds: 60.0 }
    );
    Ok(())
}

#[test]
fn invalid_values_do_not_consume_a_wait_budget() -> Result {
    let retry = "1".to_owned();
    let headers = RateHeaders {
        retry_after: Some(&retry),
        ..RateHeaders::default()
    };
    for maximum in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
        let mut state = RetryState::new();
        assert_eq!(
            state.response(429, true, &headers, maximum, || 0.0),
            Err(RetryError::Value)
        );
        assert_eq!(
            state.response(429, true, &headers, 1.0, || 0.0)?,
            Decision::Sleep { seconds: 1.0 }
        );
    }
    let zero = "0".to_owned();
    assert_eq!(
        RetryState::new().response(
            429,
            true,
            &RateHeaders {
                retry_after: Some(&zero),
                ..RateHeaders::default()
            },
            0.0,
            || 0.0
        )?,
        Decision::Sleep { seconds: 0.0 }
    );
    Ok(())
}
