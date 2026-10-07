//! Differential cases exported from the actual frozen Python request stack.

#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use cannery_server::request_context::{
    ClientAddress, ForwardedPort, QueryParams, TrustedProxies, cookies, first_header,
    parse_host_port,
};
use serde_json::{Value, json};
use std::error::Error;

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

fn reference() -> Result<Value> {
    Ok(serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/request_context_reference.json"
    ))?)
}

fn string(value: &Value) -> Result<&str> {
    value
        .as_str()
        .ok_or_else(|| "fixture string missing".into())
}

fn array(value: &Value) -> Result<&Vec<Value>> {
    value
        .as_array()
        .ok_or_else(|| "fixture array missing".into())
}

fn bytes(value: &Value) -> Result<Vec<u8>> {
    array(value)?
        .iter()
        .map(|value| {
            let value = value.as_u64().ok_or("fixture byte missing")?;
            Ok(u8::try_from(value)?)
        })
        .collect()
}

fn trust(value: &Value) -> Result<TrustedProxies> {
    if let Some(value) = value.as_str() {
        Ok(TrustedProxies::parse(value))
    } else {
        let hosts = array(value)?
            .iter()
            .map(|value| string(value).map(str::to_owned))
            .collect::<Result<Vec<_>>>()?;
        Ok(TrustedProxies::from_hosts(&hosts))
    }
}

fn client(value: &Value) -> Result<Option<ClientAddress>> {
    if value.is_null() {
        return Ok(None);
    }
    let port: u16 = string(&value["port"])?.parse()?;
    Ok(Some(ClientAddress {
        host: string(&value["host"])?.to_owned(),
        port: ForwardedPort::from(port),
    }))
}

fn client_value(client: Option<&ClientAddress>) -> Value {
    client.map_or(
        Value::Null,
        |client| json!({"host": client.host, "port": client.port.as_decimal()}),
    )
}

#[test]
fn cookies_match_frozen_starlette_parser_and_repeated_headers() -> Result {
    let reference = reference()?;
    assert_eq!(reference["reference"]["starlette"], "1.7.0");
    for (index, case) in array(&reference["cookies"])?.iter().enumerate() {
        let mut headers = HeaderMap::new();
        for header in array(&case["headers"])? {
            headers.append("cookie", HeaderValue::from_bytes(&bytes(header)?)?);
        }
        assert_eq!(
            serde_json::to_value(cookies(&headers))?,
            case["expected"],
            "cookie case {index}"
        );
    }
    Ok(())
}

#[test]
fn query_pairs_scalars_and_lists_match_frozen_python_decoding() -> Result {
    let reference = reference()?;
    for (index, case) in array(&reference["queries"])?.iter().enumerate() {
        let query = QueryParams::parse(&bytes(&case["raw"])?);
        assert_eq!(
            serde_json::to_value(query.pairs())?,
            case["pairs"],
            "query pairs {index}"
        );
        for (key, expected) in case["scalars"].as_object().ok_or("scalars missing")? {
            assert_eq!(query.get(key), expected.as_str(), "query scalar {index}");
        }
        for (key, expected) in case["lists"].as_object().ok_or("lists missing")? {
            assert_eq!(
                serde_json::to_value(query.get_all(key))?,
                *expected,
                "query list {index}"
            );
        }
        assert!(query.get("absent-fixture-key").is_none());
        assert_eq!(query.get_all("absent-fixture-key"), [] as [&str; 0]);
    }
    Ok(())
}

#[test]
fn trusted_ips_networks_literals_and_wildcards_match_frozen_uvicorn() -> Result {
    let reference = reference()?;
    assert_eq!(reference["reference"]["uvicorn"], "0.54.0");
    for (index, case) in array(&reference["trust"])?.iter().enumerate() {
        let proxies = trust(&case["trusted"])?;
        assert_eq!(
            proxies.trusts(case["host"].as_str()),
            case["expected"].as_bool().ok_or("trust verdict missing")?,
            "trust case {index}"
        );
    }
    Ok(())
}

#[test]
fn forwarded_headers_client_chain_and_schemes_match_frozen_uvicorn() -> Result {
    let reference = reference()?;
    for (index, case) in array(&reference["proxies"])?.iter().enumerate() {
        let proxies = trust(&case["trusted"])?;
        let mut headers = HeaderMap::new();
        for header in array(&case["headers"])? {
            headers.append(
                HeaderName::from_bytes(string(&header[0])?.as_bytes())?,
                HeaderValue::from_bytes(&bytes(&header[1])?)?,
            );
        }
        let context = proxies.resolve(
            &headers,
            client(&case["client"])?,
            string(&case["scheme"])?,
            case["websocket"].as_bool().ok_or("websocket missing")?,
        );
        let mut expected = case["expected"].clone();
        // Keep all proxy-chain cases, with authored native outcomes for the four
        // Python integer constructor examples embedded in this corpus.
        match index {
            14 | 16 | 20 => {
                assert_eq!(expected["client"]["host"], "::1");
                expected["client"]["port"] = json!("0");
            }
            23 => {
                assert_eq!(expected["client"], json!({"host":"host", "port":"70000"}));
                expected["client"] = json!({"host":"host:70000", "port":"0"});
            }
            _ => {}
        }
        assert_eq!(
            json!({"client": client_value(context.client.as_ref()), "scheme": context.scheme}),
            expected,
            "proxy case {index}"
        );
    }
    Ok(())
}

#[test]
fn forwarded_ports_accept_ascii_u16_boundaries() {
    for (raw, host, port) in [
        ("client:0", "client", "0"),
        ("client:65535", "client", "65535"),
        ("client:00443", "client", "443"),
        ("client:+42", "client", "42"),
        ("client: 42 ", "client", "42"),
        ("[::1]:65535", "::1", "65535"),
        ("[2001:db8::1]:443", "2001:db8::1", "443"),
        ("[fe80::1%eth0]:80", "fe80::1%eth0", "80"),
        ("[::1]", "::1", "0"),
        ("2001:db8::1", "2001:db8::1", "0"),
    ] {
        let actual = parse_host_port(raw);
        assert_eq!(actual.host, host, "{raw}");
        assert_eq!(actual.port.as_decimal(), port, "{raw}");
    }
}

#[test]
fn forwarded_ports_reject_out_of_range_and_non_ascii_integer_coercion() {
    for port in [
        "-1",
        "65536",
        "999999999999999999999999999999",
        "١٢",
        "１２",
        "1_2",
        "1.0",
        "bad",
        "",
        "1\u{1c}",
    ] {
        let bracketed = format!("[::1]:{port}");
        let actual = parse_host_port(&bracketed);
        assert_eq!(actual.host, "::1", "{bracketed}");
        assert_eq!(actual.port.as_decimal(), "0", "{bracketed}");
        let unbracketed = format!("client:{port}");
        let actual = parse_host_port(&unbracketed);
        assert_eq!(actual.host, unbracketed);
        assert_eq!(actual.port.as_decimal(), "0");
    }
    for malformed in ["[::1", "[::1]suffix", "[::1]::80"] {
        let actual = parse_host_port(malformed);
        assert_eq!(actual.port.as_decimal(), "0");
    }
}

#[test]
fn ordinary_header_lookup_uses_first_latin1_value_and_case_insensitive_name() -> Result {
    let mut headers = HeaderMap::new();
    headers.append("authorization", HeaderValue::from_bytes(b"first\xff")?);
    headers.append("authorization", HeaderValue::from_static("last"));
    assert_eq!(
        first_header(&headers, "Authorization"),
        Some("firstÿ".to_owned())
    );
    assert!(first_header(&headers, "absent").is_none());
    Ok(())
}
