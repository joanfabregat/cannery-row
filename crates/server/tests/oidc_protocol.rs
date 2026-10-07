#![forbid(unsafe_code)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::{
    json::{self},
    principal::Secret,
};
use cannery_server::oidc_protocol::{
    self as protocol, AuthorizationParameters, MetadataCache, ProtocolError,
};
use serde::Deserialize;
use serde_json::Value;
use std::error::Error;
type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;
#[derive(Deserialize)]
struct Reference {
    endpoints: Vec<Endpoint>,
    urls: Vec<UrlCase>,
    forms: Vec<Form>,
    strings: Vec<StringCase>,
    metadata: Vec<Metadata>,
    pkce: Vec<Pkce>,
}
#[derive(Deserialize)]
struct Endpoint {
    input: Vec<u32>,
    safe: bool,
}
#[derive(Deserialize)]
struct UrlCase {
    input: Vec<u32>,
    authorization: Vec<u32>,
    logout: Option<Vec<u32>>,
}
#[derive(Deserialize)]
struct Form {
    input: Vec<u32>,
    authorization: Option<Vec<u32>>,
    logout: Option<Vec<u32>>,
    error: Option<String>,
}
#[derive(Deserialize)]
struct StringCase {
    json: String,
    #[serde(rename = "str")]
    rendered: Vec<u32>,
}
#[derive(Deserialize)]
struct Metadata {
    json: String,
    issuer: Vec<u32>,
    result: String,
}
#[derive(Deserialize)]
struct Pkce {
    input: Vec<u32>,
    challenge: String,
}
fn reference() -> Result<Reference> {
    Ok(serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/oidc_protocol_reference.json"
    ))?)
}
fn original() -> Result<Value> {
    Ok(serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/oidc_reference.json"
    ))?)
}
fn text(value: &[u32]) -> Result<String> {
    cannery_core::text::from_codepoints(value.to_vec())
        .ok_or_else(|| "fixture codepoint invalid".into())
}
// Invalid Unicode and nonstandard JSON cannot cross the native UTF-8 boundary.
// Each rejected source constructor case asserts that boundary rather than invoking
// a protocol API with a value Rust String cannot represent.
fn native_text(points: &[u32]) -> Result<Option<String>> {
    if let Some(value) = cannery_core::text::from_codepoints(points.to_vec()) {
        return Ok(Some(value));
    }
    let invalid = points
        .iter()
        .copied()
        .find(|point| (0xd800..=0xdfff).contains(point) || *point > 0x0010_ffff)
        .ok_or("unexpected Unicode rejection")?;
    if invalid <= 0xffff {
        assert!(json::decode(format!("\"\\u{invalid:04x}\"").as_bytes(), 64).is_err());
    }
    Ok(None)
}
fn native_document(raw: &str) -> Result<Option<json::Document>> {
    if serde_json::from_str::<Value>(raw).is_err() {
        assert!(
            json::decode(raw.as_bytes(), json::MAX_DEPTH).is_err(),
            "{raw}"
        );
        return Ok(None);
    }
    Ok(Some(json::decode(raw.as_bytes(), json::MAX_DEPTH)?))
}
fn array(value: &Value) -> Result<&[Value]> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| "fixture array missing".into())
}
fn string(value: &Value) -> Result<&str> {
    value
        .as_str()
        .ok_or_else(|| "fixture string missing".into())
}
fn parameters<'a>(
    input: &'a String,
    state: &'a Secret,
    nonce: &'a Secret,
    verifier: &'a Secret,
) -> AuthorizationParameters<'a> {
    AuthorizationParameters {
        client_id: input,
        redirect_uri: input,
        scopes: input,
        state,
        nonce,
        verifier,
    }
}
// Explicit native contract amendments for malformed ports and ambiguous raw
// characters formerly accepted by urlsplit. Other corpus outcomes stay checked.
fn native_endpoint_expected(input: &str, source_safe: bool) -> bool {
    if input
        .chars()
        .any(|character| character.is_control() || character.is_whitespace() || character == '\\')
    {
        return false;
    }
    let authority = input
        .split_once("://")
        .map(|(_, tail)| tail.split(['/', '?', '#']).next().unwrap_or_default())
        .unwrap_or_default();
    if authority.contains('@') || authority.ends_with(':') {
        return false;
    }
    if matches!(
        input,
        "https://remote:bogus/x"
            | "https://remote:999999/x"
            | "https://[::1]:nonsense/x"
            | "https://[v1.a]/x"
            | "https://[vF.a:b]/x"
            | "https://[v1.é]/x"
    ) {
        return false;
    }
    source_safe
}

#[test]
fn endpoint_recipes_require_checked_native_url_policy_outcomes() -> Result {
    let reference = reference()?;
    assert_eq!(reference.endpoints.len(), 326);
    for case in reference.endpoints {
        let Some(input) = native_text(&case.input)? else {
            continue;
        };
        assert_eq!(
            protocol::is_safe_endpoint(&input),
            native_endpoint_expected(&input, case.safe),
            "endpoint policy {input:?}"
        );
    }
    Ok(())
}
#[test]
fn ordered_forms_pkce_and_urls_match_fresh_source() -> Result {
    let reference = reference()?;
    assert_eq!(reference.forms.len(), 133);
    assert_eq!(reference.urls.len(), 326);
    assert_eq!(reference.pkce.len(), 4);
    let state = Secret::new("state+ /".into());
    let nonce = Secret::new("nonce&=".into());
    let verifier = Secret::new("verifier".into());
    for case in reference.forms {
        let Some(input) = native_text(&case.input)? else {
            assert_eq!(case.error.as_deref(), Some("encoding"));
            continue;
        };
        let auth = protocol::authorization_url(
            &String::from("https://protocol.fixture.invalid/auth?old=1#fragment"),
            &parameters(&input, &state, &nonce, &verifier),
        );
        let logout = protocol::logout_url(
            Some(&String::from("https://logout.fixture.invalid/logout?old=1")),
            &input,
            &input,
        );
        if case.error.as_deref() == Some("encoding") {
            assert!(matches!(auth, Err(ProtocolError::Encoding)));
            assert!(matches!(logout, Err(ProtocolError::Encoding)));
        } else {
            assert_eq!(
                auth?.expose(),
                &text(&case.authorization.ok_or("missing authorization")?)?
            );
            assert_eq!(
                logout?.ok_or("missing logout result")?.expose(),
                &text(&case.logout.ok_or("missing source logout")?)?
            );
        }
    }
    for case in reference.pkce {
        assert_eq!(
            protocol::pkce_challenge(&Secret::new(
                text(&case.input)?.as_utf8().ok_or("PKCE encoding")?
            )),
            case.challenge
        );
    }
    let input = String::from("id");
    for case in reference.urls {
        let Some(endpoint) = native_text(&case.input)? else {
            continue;
        };
        let auth =
            protocol::authorization_url(&endpoint, &parameters(&input, &state, &nonce, &verifier))?;
        assert_eq!(auth.expose(), &text(&case.authorization)?);
        let logout = protocol::logout_url(Some(&endpoint), &input, &input)?;
        if native_endpoint_expected(&endpoint, case.logout.is_some()) {
            let expected = case.logout.ok_or("safe source logout expected")?;
            assert_eq!(logout.ok_or("missing logout")?.expose(), &text(&expected)?);
        } else {
            assert!(logout.is_none());
        }
    }
    assert!(native_text(&[104, 116, 116, 112, 115, 58, 47, 47, 0xd800])?.is_none());
    let unicode_url = protocol::authorization_url(
        "https://provider.invalid/é",
        &parameters(&input, &state, &nonce, &verifier),
    )?;
    assert!(unicode_url.into_utf8_secret()?.expose().contains('é'));
    let private = String::from("private-fixture-marker");
    let form = protocol::ordered_urlencode(&[("code", &private)])?;
    assert!(!format!("{form:?}").contains("private-fixture-marker"));
    let url =
        protocol::authorization_url(&private, &parameters(&input, &state, &nonce, &verifier))?;
    assert!(!format!("{url:?}").contains("private-fixture-marker"));
    for error in [
        ProtocolError::Encoding,
        ProtocolError::Shape,
        ProtocolError::Issuer,
        ProtocolError::Endpoint,
        ProtocolError::IntegerRendering,
        ProtocolError::UnhandledRecursion,
    ] {
        assert!(!format!("{error:?}: {error}").contains("private-fixture-marker"));
    }
    Ok(())
}
#[test]
fn discovery_native_json_projection_and_ordinary_metadata_match_source() -> Result {
    let reference = reference()?;
    assert_eq!(reference.strings.len(), 153);
    assert_eq!(reference.metadata.len(), 161);
    for case in reference.strings {
        let Some(document) = native_document(&case.json)? else {
            continue;
        };
        let actual = protocol::python_str(&document, document.root())?;
        if matches!(document.node(document.root()), Some(json::Node::String(_))) {
            assert_eq!(actual, text(&case.rendered)?);
        } else {
            let round_trip = json::decode(actual.as_bytes(), json::MAX_DEPTH)?;
            assert_eq!(
                json::encode_ascii_pretty(&round_trip, json::MAX_DEPTH)?,
                json::encode_ascii_pretty(&document, json::MAX_DEPTH)?
            );
        }
    }
    let mut ordinary_metadata = 0;
    for case in reference.metadata {
        let Some(document) = native_document(&case.json)? else {
            continue;
        };
        let Some(issuer) = native_text(&case.issuer)? else {
            continue;
        };
        let returned = document
            .field(document.root(), "issuer")
            .and_then(|id| document.node(id));
        if !matches!(returned, None | Some(json::Node::String(_))) {
            // A private constructor's non-string issuer cannot match the
            // configured provider URL; retain an explicit issuer-binding check.
            assert_eq!(
                protocol::validate_metadata(
                    &document,
                    &String::from("https://metadata.fixture.invalid")
                ),
                Err(ProtocolError::Issuer)
            );
            continue;
        }
        let actual = protocol::validate_metadata(&document, &issuer);
        match case.result.as_str() {
            "ok" => assert!(actual.is_ok()),
            "shape" => assert_eq!(actual, Err(ProtocolError::Shape)),
            "issuer" => assert_eq!(actual, Err(ProtocolError::Issuer)),
            "endpoint" => assert_eq!(actual, Err(ProtocolError::Endpoint)),
            _ => return Err("unknown source outcome".into()),
        }
        ordinary_metadata += 1;
    }
    assert!(
        ordinary_metadata >= 8,
        "ordinary metadata coverage must remain meaningful"
    );
    let reference = original()?;
    let mut checked = 0;
    for case in array(&reference["cases"])? {
        let name = string(&case["name"])?;
        if !matches!(
            name,
            "discovery/issuer-mismatch"
                | "discovery/missing-issuer"
                | "discovery/endpoint-wrong-type"
                | "discovery/endpoint-missing"
        ) {
            continue;
        }
        let document = json::decode(
            serde_json::to_string(&case["options"]["metadata"])?.as_bytes(),
            1000,
        )?;
        assert_eq!(
            protocol::validate_metadata(&document, &String::from("https://oidc.fixture.invalid")),
            Err(if name.contains("issuer") {
                ProtocolError::Issuer
            } else {
                ProtocolError::Endpoint
            })
        );
        checked += 1;
    }
    assert_eq!(checked, 4);
    Ok(())
}
#[test]
fn original_protocol_and_cache_cases_are_consumed_without_crypto_credits() -> Result {
    let reference = original()?;
    assert_eq!(array(&reference["cases"])?.len(), 286);
    let mut checked = 0;
    for case in array(&reference["cases"])? {
        let name = string(&case["name"])?;
        if name.starts_with("cache/") {
            let mut cache = MetadataCache::<(), ()>::default();
            let mut fetches = 0;
            for action in array(&case["actions"])? {
                let now = action[1].as_f64().ok_or("missing clock")?;
                if cache.needs_refresh(now) {
                    cache.replace_metadata((), now);
                    fetches += 1;
                }
                if cache.jwks().is_none() {
                    cache.replace_jwks(());
                    fetches += 1;
                }
            }
            assert_eq!(fetches, array(&case["requests"])?.len());
            checked += 1;
        } else if name == "authorization/pkce" {
            let state = Secret::new("public-fixture-state".into());
            let nonce = Secret::new("public-fixture-nonce".into());
            let verifier = Secret::new("public-fixture-verifier".into());
            let url = protocol::authorization_url(
                &String::from("https://oidc.fixture.invalid/authorize"),
                &AuthorizationParameters {
                    client_id: &String::from("public-fixture-client"),
                    redirect_uri: &String::from("https://app.fixture.invalid/auth/callback"),
                    scopes: &String::from("openid email profile"),
                    state: &state,
                    nonce: &nonce,
                    verifier: &verifier,
                },
            )?;
            assert_eq!(
                url.expose().as_utf8().ok_or("URL encoding")?,
                string(&case["observed"][0]["result"]["url"])?
            );
            checked += 1;
        } else if name.starts_with("logout/") {
            let endpoint = case["options"]["logout_endpoint"]
                .as_str()
                .map(String::from);
            let actual = protocol::logout_url(
                endpoint.as_deref(),
                &String::from("public-fixture-client"),
                &String::from("https://app.fixture.invalid/"),
            )?
            .map(|url| url.expose().as_utf8());
            let expected = case["observed"][0]["result"].as_str().filter(|_| {
                endpoint
                    .as_deref()
                    .is_some_and(|value| native_endpoint_expected(value, true))
            });
            assert_eq!(actual, expected.map(|value| Some(value.to_owned())));
            checked += 1;
        } else if name == "exchange/valid" {
            let form = protocol::ordered_urlencode(&[
                ("grant_type", &String::from("authorization_code")),
                ("code", &String::from("public-fixture-code")),
                (
                    "redirect_uri",
                    &String::from("https://app.fixture.invalid/auth/callback"),
                ),
                ("code_verifier", &String::from("public-fixture-verifier")),
            ])?;
            assert_eq!(
                form.expose(),
                "grant_type=authorization_code&code=public-fixture-code&redirect_uri=https%3A%2F%2Fapp.fixture.invalid%2Fauth%2Fcallback&code_verifier=public-fixture-verifier"
            );
            assert_eq!(
                case["requests"][1]["form"],
                serde_json::json!([
                    ["grant_type", "authorization_code"],
                    ["code", "public-fixture-code"],
                    ["redirect_uri", "https://app.fixture.invalid/auth/callback"],
                    ["code_verifier", "public-fixture-verifier"]
                ])
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 16);
    Ok(())
}

#[test]
fn validated_cache_refresh_preserves_old_state_on_failure() -> Result {
    let mut cache = MetadataCache::<json::Document, String>::default();
    let valid = || {
        json::decode(br#"{"issuer":"issuer","authorization_endpoint":"","token_endpoint":"javascript:token","jwks_uri":"relative"}"#,1000)
    };
    let issuer = String::from("issuer");
    cache.accept_metadata(valid()?, &issuer, 100.0)?;
    cache.replace_jwks("keys".into());
    assert!(cache.metadata().is_some());
    assert_eq!(cache.jwks().map(String::as_str), Some("keys"));
    assert!(
        cache
            .accept_metadata(json::decode(b"{}", json::MAX_DEPTH)?, &issuer, 3700.001)
            .is_err()
    );
    assert!(cache.jwks().is_some());
    assert!(cache.needs_refresh(3700.001));
    assert!(!cache.needs_refresh(3700.0));
    cache.accept_metadata(valid()?, &issuer, 3700.001)?;
    assert!(cache.jwks().is_none());
    assert!(!cache.needs_refresh(3700.001));
    Ok(())
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;

#[test]
fn native_endpoint_policy_checks_ports_canonical_loopback_and_ambiguous_authorities() -> Result {
    for endpoint in [
        "https://provider.example/authorize",
        "HTTPS://PROVIDER.EXAMPLE:443/auth?old=1#fragment",
        "https://é.example/logout",
        "https://[2001:db8::1]:8443/logout",
        "http://localhost/logout",
        "http://LOCALHOST:9010/logout",
        "http://127.0.0.1:65535/logout",
        "http://[::1]:9010/logout",
    ] {
        assert!(
            protocol::is_safe_endpoint(endpoint),
            "valid endpoint {endpoint}"
        );
    }
    for endpoint in [
        "https://",
        "https:///missing",
        "https:host",
        "//localhost/logout",
        "javascript:bad",
        "ftp://provider.example",
        "http://remote/logout",
        "http://localhost./logout",
        "http://127.1/logout",
        "http://2130706433/logout",
        "http://0x7f000001/logout",
        "http://0177.0.0.1/logout",
        "http://[0:0:0:0:0:0:0:1]/logout",
        "http://[::1%lo]/logout",
        "https://user:password@provider.example/logout",
        "https://@provider.example/logout",
        "http://remote@localhost/logout",
        "http://localhost@remote/logout",
        "https://provider.example:65536/logout",
        "https://provider.example:bogus/logout",
        "https://provider.example:/logout",
        "https://[::1]extra/logout",
        "https://[v1.a]/logout",
        "https://broken]/logout",
        "https://provider.example\\evil/logout",
        " https://provider.example/logout",
        "https://provider.example /logout",
        "https://provider.example/space here",
        "https://provider.example/\nlogout",
        "http://local\thost/logout",
        "https://a＠b.example/logout",
        "https://a／b.example/logout",
    ] {
        assert!(
            !protocol::is_safe_endpoint(endpoint),
            "unsafe endpoint {endpoint:?}"
        );
        assert!(protocol::logout_url(Some(endpoint), "client", "https://app.example/")?.is_none());
    }
    for control in ['\0', '\t', '\n', '\r', '\u{7f}', '\u{85}'] {
        assert!(!protocol::is_safe_endpoint(&format!(
            "https://provider.example/{control}logout"
        )));
    }
    let endpoint = "https://provider.example/logout?old=1#fragment";
    let logout = protocol::logout_url(
        Some(endpoint),
        "client +",
        "https://app.example/after?x=1&y=2",
    )?
    .ok_or("valid logout")?;
    assert_eq!(
        logout.expose(),
        "https://provider.example/logout?old=1#fragment?client_id=client+%2B&post_logout_redirect_uri=https%3A%2F%2Fapp.example%2Fafter%3Fx%3D1%26y%3D2"
    );
    Ok(())
}
