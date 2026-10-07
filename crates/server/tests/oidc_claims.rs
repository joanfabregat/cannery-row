#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::json::{Document, DocumentBuilder, Node, decode};
use cannery_server::oidc_claims::{
    ClaimContext, ClaimRejection, ClockTime, Identity, validate_claims,
};
use serde_json::Value;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn base64url(value: &str) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut bits = 0_u32;
    let mut remaining = 0;
    for byte in value.bytes() {
        let digit = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return Err("invalid public fixture encoding".into()),
        };
        bits = (bits << 6) | u32::from(digit);
        remaining += 6;
        if remaining >= 8 {
            remaining -= 8;
            output.push(u8::try_from((bits >> remaining) & 255)?);
            bits &= (1 << remaining) - 1;
        }
    }
    Ok(output)
}
fn text(value: &Value) -> Result<String> {
    Ok(String::from(value.as_str().ok_or("missing fixture text")?))
}
fn optional_text(value: &Value) -> Result<Option<String>> {
    if value.is_null() {
        Ok(None)
    } else {
        text(value).map(Some)
    }
}
fn expected_identity(value: &Value) -> Result<Identity> {
    Ok(Identity {
        issuer: text(&value["issuer"])?,
        subject: text(&value["subject"])?,
        email: optional_text(&value["email"])?,
        email_verified: value["email_verified"]
            .as_bool()
            .ok_or("missing verified flag")?,
        name: optional_text(&value["name"])?,
    })
}
// Numeric constructor/coercion cases are replaced by authored native boundary tests below.
// All selected ordinary security cases still compare their source outcomes.
fn selected(name: &str) -> bool {
    [
        "signature/",
        "required/",
        "claim/",
        "claims-root/",
        "issuer/",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
        || (name.starts_with("ordering/") && name != "ordering/signature-before-expiry")
}
#[test]
fn ordinary_post_signature_security_reference() -> Result<()> {
    let reference: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/oidc_reference.json"
    ))?;
    let now = ClockTime::new(reference["fixed_now"].as_f64().ok_or("missing clock")?)
        .ok_or("invalid clock")?;
    let stored_issuer = String::from("https://oidc.fixture.invalid");
    let default_issuer = stored_issuer.clone();
    let audience = String::from("public-fixture-client");
    let nonce = String::from("public-fixture-nonce");
    let mut tested = 0;
    for case in reference["cases"].as_array().ok_or("missing cases")? {
        let name = case["name"].as_str().ok_or("missing case name")?;
        if !selected(name) {
            continue;
        }
        let token = case["token"].as_str().ok_or("missing fixture token")?;
        let payload = token.split('.').nth(1).ok_or("missing payload segment")?;
        let document = decode(&base64url(payload)?, 9998)?;
        let token_issuer = case["options"]["metadata"]["issuer"]
            .as_str()
            .map_or_else(|| default_issuer.clone(), String::from);
        let result = validate_claims(
            &document,
            &ClaimContext {
                stored_issuer: &stored_issuer,
                token_issuer: &token_issuer,
                audience: &audience,
                nonce: &nonce,
                now,
            },
        );
        let observed = &case["observed"];
        if observed.get("identity").is_some() {
            assert_eq!(
                result?,
                expected_identity(&observed["identity"])?,
                "case {name}"
            );
        } else {
            let error = result.err().ok_or("expected source claim rejection")?;
            assert_eq!(
                observed["exception"].as_str(),
                Some("OIDCError"),
                "case {name}"
            );
            assert_eq!(
                error.oidc_message(),
                observed["message"]
                    .as_str()
                    .ok_or("missing rejection message")?,
                "case {name}"
            );
            assert_eq!(
                observed["callback_boundary"]["status"].as_u64(),
                Some(400),
                "case {name}"
            );
        }
        tested += 1;
    }
    assert_eq!(
        tested, 82,
        "every selected cryptographically valid reference case must run"
    );
    Ok(())
}

fn fixture(changes: &str) -> Result<Document> {
    // Appending keys also verifies the core decoder's last-value-wins semantics.
    Ok(decode(format!(r#"{{"iss":"issuer","aud":"audience","sub":"subject","iat":0,"exp":10000,"nonce":"nonce",{changes}}}"#).as_bytes(), 9998)?)
}
fn check(document: &Document, now: f64) -> Result<std::result::Result<Identity, ClaimRejection>> {
    let issuer = String::from("issuer");
    let audience = String::from("audience");
    let nonce = String::from("nonce");
    Ok(validate_claims(
        document,
        &ClaimContext {
            stored_issuer: &issuer,
            token_issuer: &issuer,
            audience: &audience,
            nonce: &nonce,
            now: ClockTime::new(now).ok_or("invalid test clock")?,
        },
    ))
}
#[test]
fn fractional_dates_and_clock_respect_sixty_second_leeway() -> Result<()> {
    for changes in [
        r#""iat":160,"nbf":160,"exp":41"#,
        r#""iat":160.9,"nbf":160.9,"exp":41.9"#,
    ] {
        assert!(check(&fixture(changes)?, 100.25)?.is_ok());
    }
    assert_eq!(
        check(&fixture(r#""iat":161"#)?, 100.25)?,
        Err(ClaimRejection::ImmatureIat)
    );
    assert_eq!(
        check(&fixture(r#""nbf":161.0"#)?, 100.25)?,
        Err(ClaimRejection::ImmatureNbf)
    );
    assert_eq!(
        check(&fixture(r#""exp":40.9"#)?, 100.25)?,
        Err(ClaimRejection::Expired)
    );
    assert!(check(&fixture(r#""iat":-60.9,"exp":-59.9"#)?, 0.0)?.is_ok());
    // Adjacent representable i64 values must not collapse during comparison.
    assert_eq!(
        check(
            &fixture(r#""iat":9007199254741053,"exp":9223372036854775807"#)?,
            9_007_199_254_740_992.0
        )?,
        Err(ClaimRejection::ImmatureIat)
    );
    assert!(ClockTime::new(f64::NAN).is_none());
    assert!(ClockTime::new(f64::INFINITY).is_none());
    assert!(ClockTime::new(f64::NEG_INFINITY).is_none());
    Ok(())
}

#[test]
fn numeric_dates_accept_bounded_integers_and_finite_truncated_floats() -> Result<()> {
    for (name, expected) in [
        ("iat", ClaimRejection::ImmatureIat),
        ("nbf", ClaimRejection::ImmatureNbf),
        ("exp", ClaimRejection::Expired),
    ] {
        let high = fixture(&format!(r#""{name}":9223372036854775807"#))?;
        if name == "exp" {
            assert!(check(&high, 0.0)?.is_ok());
        } else {
            assert_eq!(check(&high, 0.0)?, Err(expected));
        }
        let low = fixture(&format!(r#""{name}":-9223372036854775808"#))?;
        if name == "exp" {
            assert_eq!(check(&low, 0.0)?, Err(expected));
        } else {
            assert!(check(&low, 0.0)?.is_ok());
        }
        // Largest in-range binary64 integer below 2^63 and exact -2^63.
        for (value, high) in [
            ("9223372036854774784.0", true),
            ("-9223372036854775808.0", false),
        ] {
            let actual = check(&fixture(&format!(r#""{name}":{value}"#))?, 0.0)?;
            if (name == "exp") == high {
                assert!(actual.is_ok(), "{name}: {value}");
            } else {
                assert_eq!(actual, Err(expected), "{name}: {value}");
            }
        }
    }
    Ok(())
}

#[test]
fn numeric_dates_reject_coercion_and_out_of_range_values() -> Result<()> {
    for (name, expected) in [
        ("iat", ClaimRejection::InvalidIat),
        ("nbf", ClaimRejection::InvalidNbf),
        ("exp", ClaimRejection::InvalidExp),
    ] {
        for value in [
            "true",
            "false",
            r#""100""#,
            r#""100.5""#,
            r#""١٠٠""#,
            "9223372036854775808",
            "-9223372036854775809",
            "9223372036854775808.0",
            "-9223372036854777856.0",
            "1e300",
            "-1e300",
        ] {
            assert_eq!(
                check(&fixture(&format!(r#""{name}":{value}"#))?, 0.0)?,
                Err(expected),
                "{name}: {value}"
            );
        }
    }
    Ok(())
}
#[test]
fn missing_claims_and_all_nonfinite_conversions_remain_login_rejections() -> Result<()> {
    assert_eq!(
        check(&decode(b"{}", 9998)?, 0.0)?,
        Err(ClaimRejection::Missing(
            cannery_server::oidc_claims::RequiredClaim::Exp
        ))
    );
    for (name, expected) in [
        ("iat", ClaimRejection::InvalidIat),
        ("nbf", ClaimRejection::InvalidNbf),
        ("exp", ClaimRejection::InvalidExp),
    ] {
        for value in ["[]", "{}"] {
            assert_eq!(
                check(&fixture(&format!(r#""{name}":{value}"#))?, 0.0)?,
                Err(expected)
            );
        }
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let source = fixture(&format!(r#""{name}":0"#))?;
            let Some(Node::Object(fields)) = source.node(source.root()) else {
                return Err("expected object fixture".into());
            };
            let mut builder = DocumentBuilder::new();
            let mut entries = Vec::new();
            for (key, id) in fields {
                let id = if key == name {
                    builder.push(Node::Float(value))?
                } else {
                    builder.import(&source, *id)?
                };
                entries.push((key.clone(), id));
            }
            let root = builder.push(Node::Object(entries))?;
            assert_eq!(check(&builder.finish(root)?, 0.0)?, Err(expected));
        }
    }
    for literal in ["NaN", "Infinity", "-Infinity"] {
        assert!(decode(format!(r#"{{"exp":{literal}}}"#).as_bytes(), 64).is_err());
    }
    Ok(())
}

#[test]
fn literal_verified_flag_optional_projection_and_redaction() -> Result<()> {
    let document = fixture(
        r#""sub":"subject-é","email":"reader@example.org","name":"Joan","email_verified":1"#,
    )?;
    let identity = check(&document, 0.0)??;
    assert!(!identity.email_verified);
    assert_eq!(identity.subject, String::from("subject-é"));
    assert_eq!(identity.email, Some(String::from("reader@example.org")));
    assert_eq!(identity.name, Some(String::from("Joan")));
    assert_eq!(format!("{identity:?}"), "Identity { claims: [redacted] }");
    let optional = check(
        &fixture(r#""sub":"subject","email":null,"name":null,"email_verified":true"#)?,
        0.0,
    )??;
    assert!(optional.email_verified);
    assert!(optional.email.is_none() && optional.name.is_none());
    Ok(())
}
