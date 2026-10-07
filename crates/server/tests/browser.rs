//! Supported browser recipes and authored native cookie/redirect boundaries.
#![forbid(unsafe_code)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::{json, principal::Secret, timestamps::Timestamp};
use cannery_server::browser::{self, BrowserError};
use num_bigint::BigInt;
use serde_json::Value;
use std::{error::Error, str::FromStr};
type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;
fn text(value: &Value) -> Result<Option<String>> {
    let points = value
        .as_array()
        .ok_or("text code points absent")?
        .iter()
        .map(|point| -> Result<u32> {
            Ok(u32::try_from(point.as_u64().ok_or("code point absent")?)?)
        })
        .collect::<Result<Vec<_>>>()?;
    if let Some(text) = points
        .iter()
        .copied()
        .map(char::from_u32)
        .collect::<Option<String>>()
    {
        return Ok(Some(text));
    }
    for point in points
        .into_iter()
        .filter(|point| (0xd800..=0xdfff).contains(point))
    {
        assert!(json::decode_str(&format!("\"\\u{point:04x}\""), json::MAX_DEPTH).is_err());
    }
    Ok(None)
}
fn reference() -> Result<Value> {
    Ok(serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/browser_reference.json"
    ))?)
}
#[test]
fn supported_return_recipes_preserve_local_paths_and_utf8_redirect_encoding() -> Result {
    let data = reference()?;
    let cases = data["returns"].as_array().ok_or("return cases absent")?;
    assert_eq!(cases.len(), 152);
    for case in cases {
        let input = if case["input"].is_null() {
            None
        } else {
            let Some(input) = text(&case["input"])? else {
                continue;
            };
            Some(input)
        };
        let safe = browser::safe_return_to(input.as_ref());
        if input
            .as_ref()
            .is_some_and(|value| value.chars().any(char::is_control))
        {
            assert_eq!(safe, "/");
            assert_eq!(browser::redirect_location(&safe)?.as_bytes(), b"/");
        } else {
            assert_eq!(safe, text(&case["safe"])?.ok_or("invalid expected path")?);
            assert!(case.get("error").is_none());
            assert_eq!(case["status"], 302);
            assert_eq!(
                browser::redirect_location(&safe)?.as_bytes(),
                case["location"]
                    .as_str()
                    .ok_or("location absent")?
                    .as_bytes()
            );
        }
    }
    Ok(())
}
fn assert_cookie(header: &str, name: &str, value: &str, max_age: &str, path: &str, secure: bool) {
    let fields = header.split(';').map(str::trim).collect::<Vec<_>>();
    let mut expected = vec![
        format!("{name}={value}"),
        "HttpOnly".to_owned(),
        format!("Max-Age={max_age}"),
        format!("Path={path}"),
        "SameSite=lax".to_owned(),
    ];
    if secure {
        expected.push("Secure".to_owned());
    }
    assert_eq!(fields, expected);
}
#[test]
fn all_cookie_recipes_check_native_octets_expiry_and_callback_order() -> Result {
    let data = reference()?;
    let now = Timestamp::from_str(data["fixed_now"].as_str().ok_or("fixed time absent")?)?;
    let cases = data["cookies"].as_array().ok_or("cookie cases absent")?;
    assert_eq!(cases.len(), 38);
    let mut rejected_unicode = 0;
    for case in cases {
        let Some(value) = text(&case["value"])? else {
            rejected_unicode += 1;
            continue;
        };
        let secure = case["secure"].as_bool().ok_or("secure absent")?;
        let secret = Secret::new(value.clone());
        let hours = BigInt::from_str(case["hours"].as_str().ok_or("hours absent")?)?;
        let valid_octets = value.chars().all(|character| {
            character.is_ascii()
                && !character.is_control()
                && !matches!(character, ' ' | '"' | ',' | ';' | '\\')
        });
        if !valid_octets {
            assert!(matches!(
                browser::quote_cookie_value(&value),
                Err(BrowserError::Encoding)
            ));
            assert!(matches!(
                browser::login_cookie(&secret, secure),
                Err(BrowserError::Encoding)
            ));
            assert!(matches!(
                browser::callback_cookies(&secret, &hours, secure, now),
                Err(BrowserError::Encoding)
            ));
            continue;
        }
        assert_eq!(browser::quote_cookie_value(&value)?.expose(), value);
        let login = browser::login_cookie(&secret, secure)?.into_header()?;
        assert_cookie(login.to_str()?, "cr_login", &value, "900", "/auth", secure);
        let seconds = &hours * 3600_u32;
        if seconds < BigInt::from(i64::MIN) || seconds > BigInt::from(i64::MAX) {
            assert!(matches!(
                browser::session_cookie(&secret, &hours, secure),
                Err(BrowserError::MaxAgeOverflow)
            ));
            assert!(matches!(
                browser::callback_cookies(&secret, &hours, secure, now),
                Err(BrowserError::MaxAgeOverflow)
            ));
            continue;
        }
        let [deletion, session] = browser::callback_cookies(&secret, &hours, secure, now)?;
        let expected_deletion = case["callback"][0]
            .as_str()
            .ok_or("callback deletion absent")?;
        assert_eq!(deletion.into_header()?.to_str()?, expected_deletion);
        assert_cookie(
            session.into_header()?.to_str()?,
            "cr_session",
            &value,
            &seconds.to_string(),
            "/",
            secure,
        );
        assert_cookie(
            browser::session_cookie(&secret, &hours, secure)?
                .into_header()?
                .to_str()?,
            "cr_session",
            &value,
            &seconds.to_string(),
            "/",
            secure,
        );
    }
    assert_eq!(rejected_unicode, 2);
    Ok(())
}
#[test]
fn authored_redirect_and_cookie_security_boundaries_are_explicit() -> Result {
    for path in [
        "https://outside.example/",
        "//outside.example/",
        "/\\outside",
        "/private\nheader",
        "/private\u{85}header",
    ] {
        assert_eq!(browser::safe_return_to(Some(&path.to_owned())), "/");
    }
    let path = "/science/é?view=1&next=%2Fother".to_owned();
    assert_eq!(browser::safe_return_to(Some(&path)), path);
    assert_eq!(
        browser::redirect_location(&path)?.to_str()?,
        "/science/%C3%A9?view=1&next=%2Fother"
    );
    for value in ["state_with-hyphen.123", "", "token:opaque+value/"] {
        assert_eq!(browser::quote_cookie_value(value)?.expose(), value);
    }
    for value in [
        "opaque token",
        "opaque;injection",
        "opaque,other",
        "opaque\"quote",
        "opaque\\slash",
        "opaque\r\nSet-Cookie: injected",
        "opaque\0",
        "opaque\u{7f}",
        "opaqueé",
        "opaque科学",
    ] {
        let error = browser::quote_cookie_value(value)
            .err()
            .ok_or("unsafe cookie accepted")?;
        assert!(matches!(error, BrowserError::Encoding));
        assert!(!format!("{error:?} {error}").contains("opaque"));
    }
    Ok(())
}
#[test]
fn signed_native_cookie_max_age_is_checked_and_overflow_redacted() -> Result {
    let secret = Secret::new("synthetic-token".to_owned());
    for hours in [0, -1, 12, i64::MAX / 3600, i64::MIN / 3600] {
        let cookie = browser::session_cookie(&secret, &BigInt::from(hours), true)?.into_header()?;
        assert_cookie(
            cookie.to_str()?,
            "cr_session",
            secret.expose(),
            &(hours * 3600).to_string(),
            "/",
            true,
        );
    }
    for hours in [
        BigInt::from(i64::MAX / 3600 + 1),
        BigInt::from(i64::MIN / 3600 - 1),
        BigInt::from(i64::MAX) + 1,
    ] {
        let error = browser::session_cookie(&secret, &hours, true)
            .err()
            .ok_or("overflow accepted")?;
        assert!(matches!(error, BrowserError::MaxAgeOverflow));
        assert!(!format!("{error:?} {error}").contains("synthetic-token"));
        assert!(!format!("{error:?} {error}").contains(&hours.to_string()));
    }
    Ok(())
}
#[test]
fn cookie_values_and_errors_do_not_leak_through_debug() -> Result {
    let secret = Secret::new("private-synthetic-browser-marker".into());
    let cookie = browser::login_cookie(&secret, true)?;
    assert_eq!(format!("{cookie:?}"), "CookieHeader([redacted])");
    assert!(!format!("{secret:?}").contains("private-synthetic"));
    let error = browser::login_cookie(&Secret::new("private-synthetic-科学".into()), true)
        .err()
        .ok_or("unsupported Unicode accepted")?;
    assert_eq!(error.to_string(), "browser header encoding failed");
    assert!(!format!("{error:?}").contains("private-synthetic"));
    Ok(())
}

#[test]
fn deletion_dates_preserve_source_utc_and_year_padding() -> Result<()> {
    let data = reference()?;
    let cases = data["deletions"]
        .as_array()
        .ok_or("deletion cases absent")?;
    assert_eq!(cases.len(), 5);
    for case in cases {
        let now = Timestamp::from_str(case["now"].as_str().ok_or("deletion time absent")?)?;
        assert_eq!(
            browser::delete_login_cookie(now).into_header()?.as_bytes(),
            case["login"]
                .as_str()
                .ok_or("login deletion absent")?
                .as_bytes()
        );
        assert_eq!(
            browser::delete_session_cookie(now)
                .into_header()?
                .as_bytes(),
            case["session"]
                .as_str()
                .ok_or("session deletion absent")?
                .as_bytes()
        );
    }
    Ok(())
}
