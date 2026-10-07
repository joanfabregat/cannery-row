//! Frozen Starlette 1.7 / Uvicorn 0.54 request boundary semantics.
//!
//! Ordinary scalar headers use their first value. Cookies combine all headers;
//! forwarded headers have their own last/all-value behavior.

use axum::http::HeaderMap;
use std::{collections::BTreeMap, fmt, net::IpAddr};

fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&byte| char::from(byte)).collect()
}

fn python_space(value: char) -> bool {
    value.is_whitespace() || matches!(value, '\u{1c}'..='\u{1f}')
}

/// Return the first header value, decoded as Latin-1 like Starlette.
#[must_use]
pub fn first_header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name).map(|value| latin1(value.as_bytes()))
}

fn unquote_cookie(value: &str) -> String {
    let Some(inner) = value.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return value.to_owned();
    };
    let characters: Vec<char> = inner.chars().collect();
    let mut result = String::new();
    let mut index = 0;
    while index < characters.len() {
        if characters[index] == '\\' {
            let following = &characters[index + 1..];
            if following.len() >= 3
                && matches!(following[0], '0'..='3')
                && matches!(following[1], '0'..='7')
                && matches!(following[2], '0'..='7')
            {
                let value = (u32::from(following[0]) - u32::from('0')) * 64
                    + (u32::from(following[1]) - u32::from('0')) * 8
                    + u32::from(following[2])
                    - u32::from('0');
                if let Some(character) = char::from_u32(value) {
                    result.push(character);
                }
                index += 4;
                continue;
            }
            if let Some(&character) = following.first().filter(|&&c| c != '\n') {
                result.push(character);
                index += 2;
                continue;
            }
        }
        result.push(characters[index]);
        index += 1;
    }
    result
}

/// Parse all Cookie headers in order, with later duplicate names winning.
/// Empty names, bare chunks and malformed quoting follow Starlette's parser.
#[must_use]
pub fn cookies(headers: &HeaderMap) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    for header in headers.get_all("cookie") {
        let decoded = latin1(header.as_bytes());
        for chunk in decoded.split(';') {
            let (key, value) = chunk.split_once('=').unwrap_or(("", chunk));
            let key = key.trim_matches(python_space);
            let value = value.trim_matches(python_space);
            if !key.is_empty() || !value.is_empty() {
                result.insert(key.to_owned(), unquote_cookie(value));
            }
        }
    }
    result
}

/// Ordered query pairs, including duplicates and blank values.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryParams {
    pairs: Vec<(String, String)>,
}

impl QueryParams {
    /// Decode Latin-1 raw query bytes, then Python-style UTF-8 percent escapes.
    #[must_use]
    pub fn parse(raw_query: &[u8]) -> Self {
        let query = latin1(raw_query);
        let pairs = query
            .split('&')
            .filter(|part| !part.is_empty())
            .map(|part| {
                let (key, value) = part.split_once('=').unwrap_or((part, ""));
                (unquote_query(key), unquote_query(value))
            })
            .collect();
        Self { pairs }
    }

    /// Preserve wire order for list-valued filters and explicit iteration.
    #[must_use]
    pub fn pairs(&self) -> &[(String, String)] {
        &self.pairs
    }

    /// Scalar parameters use the last duplicate value.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.pairs
            .iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// List parameters use every value in wire order.
    #[must_use]
    pub fn get_all(&self, name: &str) -> Vec<&str> {
        self.pairs
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .collect()
    }
}

fn unquote_ascii(segment: &[u8], result: &mut String) {
    let mut bytes = Vec::new();
    let mut index = 0;
    while index < segment.len() {
        if segment[index] == b'%' && index + 2 < segment.len() {
            let first = char::from(segment[index + 1]).to_digit(16);
            let second = char::from(segment[index + 2]).to_digit(16);
            if let (Some(first), Some(second)) = (first, second) {
                if let Ok(byte) = u8::try_from(first * 16 + second) {
                    bytes.push(byte);
                }
                index += 3;
                continue;
            }
        }
        bytes.push(if segment[index] == b'+' {
            b' '
        } else {
            segment[index]
        });
        index += 1;
    }
    result.push_str(&String::from_utf8_lossy(&bytes));
}

fn unquote_query(value: &str) -> String {
    // urllib.parse.unquote decodes ASCII runs separately, preserving raw
    // non-ASCII Latin-1 characters rather than reinterpreting them as UTF-8.
    let mut result = String::new();
    let mut ascii = Vec::new();
    for character in value.chars() {
        if character.is_ascii() {
            if let Ok(byte) = u8::try_from(u32::from(character)) {
                ascii.push(byte);
            }
        } else {
            unquote_ascii(&ascii, &mut result);
            ascii.clear();
            result.push(character);
        }
    }
    unquote_ascii(&ascii, &mut result);
    result
}

/// A validated forwarded transport port, in canonical decimal form.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForwardedPort(String);

impl ForwardedPort {
    #[must_use]
    pub fn as_decimal(&self) -> &str {
        &self.0
    }
}

impl From<u16> for ForwardedPort {
    fn from(value: u16) -> Self {
        Self(value.to_string())
    }
}

impl fmt::Display for ForwardedPort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Connecting or forwarded client information, independent of socket parsing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientAddress {
    pub host: String,
    pub port: ForwardedPort,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProxyContext {
    pub client: Option<ClientAddress>,
    pub scheme: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Address {
    ip: IpAddr,
    scope: Option<String>,
}

impl Address {
    fn parse(value: &str) -> Option<Self> {
        if let Some((ip, scope)) = value.split_once('%') {
            if scope.is_empty() || scope.contains('%') {
                return None;
            }
            let ip: std::net::Ipv6Addr = ip.parse().ok()?;
            Some(Self {
                ip: IpAddr::V6(ip),
                scope: Some(scope.to_owned()),
            })
        } else {
            Some(Self {
                ip: value.parse().ok()?,
                scope: None,
            })
        }
    }

    fn number(&self) -> (u128, u32) {
        match self.ip {
            IpAddr::V4(ip) => (u128::from(u32::from(ip)), 32),
            IpAddr::V6(ip) => (u128::from(ip), 128),
        }
    }
}

#[derive(Clone, Debug)]
struct Network {
    address: u128,
    mask: u128,
    bits: u32,
}

fn prefix_mask(bits: u32, prefix: u32) -> u128 {
    if prefix == 0 {
        0
    } else {
        (u128::MAX >> (128 - bits)) << (bits - prefix) & (u128::MAX >> (128 - bits))
    }
}

impl Network {
    fn parse(value: &str) -> Option<Self> {
        let (address, prefix) = value.split_once('/')?;
        let (address, bits) = Address::parse(address)?.number();
        let prefix = if !prefix.is_empty() && prefix.bytes().all(|byte| byte.is_ascii_digit()) {
            let prefix: u32 = prefix.parse().ok()?;
            (prefix <= bits).then_some(prefix)?
        } else if bits == 32 {
            let mask: std::net::Ipv4Addr = prefix.parse().ok()?;
            let mask = u32::from(mask);
            let leading = mask.leading_ones();
            if u128::from(mask) == prefix_mask(32, leading) {
                leading
            } else {
                let inverted = !mask;
                let leading = inverted.leading_ones();
                (u128::from(inverted) == prefix_mask(32, leading)).then_some(leading)?
            }
        } else {
            return None;
        };
        let mask = prefix_mask(bits, prefix);
        // ipaddress.ip_network defaults strict=True: host bits are refused.
        (address & mask == address).then_some(Self {
            address,
            mask,
            bits,
        })
    }

    fn contains(&self, address: &Address) -> bool {
        let (number, bits) = address.number();
        bits == self.bits && number & self.mask == self.address
    }
}

/// Uvicorn trusted connecting hosts, strict networks and exact literal names.
/// `Default` trusts no hosts; the application supplies its configured fallback.
#[derive(Clone, Debug, Default)]
pub struct TrustedProxies {
    always: bool,
    addresses: Vec<Address>,
    networks: Vec<Network>,
    literals: Vec<String>,
}

impl TrustedProxies {
    #[must_use]
    pub fn parse(value: &str) -> Self {
        let mut result = Self {
            always: value == "*",
            ..Self::default()
        };
        if !result.always {
            for host in value.split(',').map(|host| host.trim_matches(python_space)) {
                result.add(host);
            }
        }
        result
    }

    /// List configurations preserve each entry; Uvicorn only strips CSV strings.
    #[must_use]
    pub fn from_hosts(hosts: &[String]) -> Self {
        let mut result = Self {
            always: hosts.len() == 1 && hosts[0] == "*",
            ..Self::default()
        };
        if !result.always {
            for host in hosts {
                result.add(host);
            }
        }
        result
    }

    fn add(&mut self, host: &str) {
        if host.contains('/') {
            if let Some(network) = Network::parse(host) {
                self.networks.push(network);
            } else {
                self.literals.push(host.to_owned());
            }
        } else if let Some(address) = Address::parse(host) {
            self.addresses.push(address);
        } else {
            self.literals.push(host.to_owned());
        }
    }

    #[must_use]
    pub fn trusts(&self, host: Option<&str>) -> bool {
        if self.always {
            return true;
        }
        let Some(host) = host.filter(|host| !host.is_empty()) else {
            return false;
        };
        if let Some(address) = Address::parse(host) {
            self.addresses.contains(&address)
                || self
                    .networks
                    .iter()
                    .any(|network| network.contains(&address))
        } else {
            self.literals.iter().any(|literal| literal == host)
        }
    }

    /// Resolve only Uvicorn's X-Forwarded-For and X-Forwarded-Proto behavior.
    /// The caller retains the original Host/server address and other headers.
    #[must_use]
    pub fn resolve(
        &self,
        headers: &HeaderMap,
        client: Option<ClientAddress>,
        scheme: &str,
        websocket: bool,
    ) -> ProxyContext {
        let mut result = ProxyContext {
            client,
            scheme: scheme.to_owned(),
        };
        if !self.trusts(result.client.as_ref().map(|client| client.host.as_str())) {
            return result;
        }
        if let Some(value) = headers.get_all("x-forwarded-proto").iter().next_back() {
            let value = latin1(value.as_bytes());
            let value = value.trim_matches(python_space);
            if matches!(value, "http" | "https" | "ws" | "wss") {
                result.scheme = if websocket {
                    value.replace("http", "ws")
                } else {
                    value.to_owned()
                };
            }
        }
        let forwarded: Vec<String> = headers
            .get_all("x-forwarded-for")
            .iter()
            .map(|value| latin1(value.as_bytes()))
            .collect();
        if !forwarded.is_empty() {
            let combined = forwarded.join(", ");
            let hosts: Vec<&str> = combined
                .split(',')
                .map(|host| host.trim_matches(python_space))
                .collect();
            let selected = if self.always {
                hosts.first().copied()
            } else {
                hosts
                    .iter()
                    .rev()
                    .copied()
                    .find(|host| !self.trusts(Some(&parse_host_port(host).host)))
                    .or_else(|| hosts.first().copied())
            };
            if let Some(host) = selected {
                let client = parse_host_port(host);
                if !client.host.is_empty() {
                    result.client = Some(client);
                }
            }
        }
        result
    }
}

/// Parse Uvicorn's forwarded host/port syntax without socket-port restrictions.
/// Invalid unbracketed ports retain the original literal host; invalid
/// bracketed ports keep the bracket contents with port zero.
#[must_use]
pub fn parse_host_port(value: &str) -> ClientAddress {
    let zero = || ClientAddress {
        host: value.to_owned(),
        port: ForwardedPort::from(0),
    };
    if let Some(bracketed) = value.strip_prefix('[') {
        let Some((host, remainder)) = bracketed.split_once(']') else {
            return zero();
        };
        if remainder.is_empty() {
            return ClientAddress {
                host: host.to_owned(),
                port: ForwardedPort::from(0),
            };
        }
        let Some(port) = remainder.strip_prefix(':') else {
            return zero();
        };
        return ClientAddress {
            host: host.to_owned(),
            port: parse_port(port).unwrap_or_else(|| ForwardedPort::from(0)),
        };
    }
    if value.bytes().filter(|&byte| byte == b':').count() == 1
        && let Some((host, port)) = value.rsplit_once(':')
        && let Some(port) = parse_port(port)
    {
        return ClientAddress {
            host: host.to_owned(),
            port,
        };
    }
    zero()
}

fn parse_port(value: &str) -> Option<ForwardedPort> {
    value.trim().parse::<u16>().ok().map(ForwardedPort::from)
}
