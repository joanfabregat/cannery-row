//! Transport and process contracts, exercised through public interfaces.
#![allow(clippy::too_many_lines)]
use conformance::{Coverage, Harness, Result, contract_document};
use reqwest::{Client, Method, Response, Url, header};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    net::TcpStream,
    path::PathBuf,
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn native_server() -> Result<bool> {
    match std::env::var("CANNERY_CONFORMANCE_SERVER_IMPLEMENTATION")?.as_str() {
        "rust" => Ok(true),
        "python" => Ok(false),
        _ => Err("unknown server implementation".into()),
    }
}

struct Login {
    cookie: String,
    authorization_url: String,
}
struct Browser {
    cookie: String,
    csrf: String,
    user_id: String,
}
struct Probe {
    harness: Harness,
    client: Client,
    base: String,
    provider: String,
    control: String,
    extra: Coverage,
}
impl Probe {
    fn new() -> Result<Self> {
        let base = std::env::var("CANNERY_CONFORMANCE_URL")?;
        Ok(Self {
            harness: Harness::new(&base)?,
            base,
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            provider: std::env::var("CANNERY_CONFORMANCE_OIDC_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:9011".into()),
            control: std::env::var("CANNERY_TEST_OIDC_CONTROL_TOKEN")?,
            extra: Coverage::default(),
        })
    }
    async fn start(&self, return_to: &str) -> Result<Login> {
        let response = self
            .client
            .get(format!("{}/auth/login", self.base))
            .query(&[("return_to", return_to)])
            .send()
            .await?;
        assert_eq!(response.status().as_u16(), 302);
        let raw = response_cookie(&response, "cr_login")?;
        assert!(
            raw.contains("HttpOnly")
                && raw.contains("Path=/auth")
                && raw.to_lowercase().contains("samesite=lax")
        );
        let authorization_url = text_header(&response, "location")?.to_owned();
        let parsed = Url::parse(&authorization_url)?;
        let query: std::collections::BTreeMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(
            query.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(query.get("response_type").map(String::as_str), Some("code"));
        assert!(query.get("nonce").is_some_and(|value| !value.is_empty()));
        Ok(Login {
            cookie: raw.split(';').next().ok_or("empty login cookie")?.into(),
            authorization_url,
        })
    }
    async fn approve(&self, login: &Login, claims: Value, overrides: Value) -> Result<Value> {
        let mut body = json!({"authorization_url":login.authorization_url,"claims":claims});
        for (key, value) in overrides.as_object().ok_or("overrides must be object")? {
            body[key] = value.clone();
        }
        let response = self
            .client
            .post(format!("{}/__conformance/approve", self.provider))
            .bearer_auth(&self.control)
            .json(&body)
            .send()
            .await?;
        assert_eq!(response.status().as_u16(), 200);
        Ok(response.json().await?)
    }
    async fn callback(&self, approval: &Value, cookie: Option<&str>) -> Result<Response> {
        let mut request = self
            .client
            .get(format!("{}/auth/callback", self.base))
            .query(&[
                ("state", field(approval, "state")?),
                ("code", field(approval, "code")?),
            ]);
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        Ok(request.send().await?)
    }
    async fn login(&mut self, claims: Value) -> Result<Browser> {
        let login = self.start("/").await?;
        let approval = self.approve(&login, claims, json!({})).await?;
        let callback = self.callback(&approval, Some(&login.cookie)).await?;
        assert_eq!(callback.status().as_u16(), 302);
        let session = response_cookie(&callback, "cr_session")?;
        assert!(
            session.contains("HttpOnly")
                && session.contains("Path=/")
                && session.to_lowercase().contains("samesite=lax")
        );
        assert!(response_cookie(&callback, "cr_login")?.contains("Max-Age=0"));
        let cookie = session
            .split(';')
            .next()
            .ok_or("empty session cookie")?
            .to_owned();
        assert!(cookie.starts_with("cr_session=cr_ses_"));
        let response = self
            .harness
            .request(Method::GET, "/api/me")?
            .header(header::COOKIE, &cookie)
            .send()
            .await?;
        let me = self
            .harness
            .check_response(Method::GET, "/api/me", response, 200)
            .await?
            .body;
        Ok(Browser {
            cookie,
            csrf: field(&me, "csrf_token")?.into(),
            user_id: field(&me["user"], "id")?.into(),
        })
    }
    async fn token(&mut self, browser: &Browser) -> Result<String> {
        let response = self
            .harness
            .request(Method::POST, "/api/tokens")?
            .header(header::COOKIE, &browser.cookie)
            .header("X-CSRF-Token", &browser.csrf)
            .json(&json!({"name":"protocol-probe","expires_in_days":1,"scopes":["read","write"]}))
            .send()
            .await?;
        let token = self
            .harness
            .check_response(Method::POST, "/api/tokens", response, 201)
            .await?
            .body;
        Ok(field(&token, "token")?.into())
    }
    async fn rpc(&self, token: Option<&str>, body: Value) -> Result<Response> {
        let mut request = self
            .client
            .post(format!("{}/mcp", self.base))
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-06-18")
            .json(&body);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        Ok(request.send().await?)
    }
    fn export(&self, name: &str) -> Result<()> {
        if let Ok(directory) = std::env::var("CANNERY_CONFORMANCE_COVERAGE_DIR") {
            fs::create_dir_all(&directory)?;
            let mut coverage = self.harness.coverage().clone();
            coverage.merge(&self.extra);
            fs::write(
                PathBuf::from(directory).join(format!("{name}.json")),
                serde_json::to_vec_pretty(&coverage)?,
            )?;
        }
        Ok(())
    }
    fn credit(&mut self, operation: &str, statuses: &[u16]) {
        self.extra
            .operations
            .entry(operation.into())
            .or_default()
            .extend(statuses.iter().copied());
    }
    async fn write(
        &mut self,
        token: &str,
        method: Method,
        template: &str,
        path: &str,
        body: Value,
        status: u16,
    ) -> Result<Value> {
        let response = self
            .harness
            .request(method.clone(), path)?
            .bearer_auth(token)
            .json(&body)
            .send()
            .await?;
        Ok(self
            .harness
            .check_response(method, template, response, status)
            .await?
            .body)
    }
}
fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name]
        .as_str()
        .ok_or_else(|| format!("missing string {name}").into())
}
fn text_header<'a>(response: &'a Response, name: &str) -> Result<&'a str> {
    Ok(response
        .headers()
        .get(name)
        .ok_or_else(|| format!("missing header {name}"))?
        .to_str()?)
}
fn response_cookie(response: &Response, name: &str) -> Result<String> {
    for raw in response.headers().get_all(header::SET_COOKIE) {
        let raw = raw.to_str()?;
        if raw.starts_with(&format!("{name}=")) {
            return Ok(raw.into());
        }
    }
    Err(format!("missing cookie {name}").into())
}
fn stamp() -> Result<String> {
    Ok(format!(
        "protocol-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ))
}
fn claims(subject: &str) -> Value {
    json!({"sub":subject,"email":format!("{subject}@conformance.test"),"email_verified":true,"name":"Protocol probe"})
}
async fn api_error(response: Response, status: u16, code: &str) -> Result<Value> {
    assert_eq!(response.status().as_u16(), status);
    let body: Value = response.json().await?;
    assert_eq!(body["error"]["code"], code);
    Ok(body)
}
async fn rpc_error(response: Response, status: u16, code: i64) -> Result<Value> {
    assert_eq!(response.status().as_u16(), status);
    let body: Value = response.json().await?;
    assert_eq!(body["jsonrpc"], "2.0");
    assert_eq!(body["error"]["code"], code);
    Ok(body)
}

fn oversized_chunked_request(base: &str, size: usize) -> Result<()> {
    let url = Url::parse(base)?;
    if url.scheme() != "http" {
        return Err("chunked transport check needs the HTTP loopback test server".into());
    }
    let host = url.host_str().ok_or("server URL omitted host")?;
    let port = url
        .port_or_known_default()
        .ok_or("server URL omitted port")?;
    let mut stream = TcpStream::connect((host, port))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    write!(
        stream,
        "POST /mcp HTTP/1.1\r\nHost: {host}:{port}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{size:X}\r\n"
    )?;
    stream.write_all(&vec![b' '; size])?;
    stream.write_all(b"\r\n0\r\n\r\n")?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let response = String::from_utf8(response)?;
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .ok_or("invalid HTTP response")?;
    assert!(
        headers
            .lines()
            .next()
            .is_some_and(|line| line.contains(" 413 "))
    );
    let body: Value = serde_json::from_str(body)?;
    assert_eq!(body["error"]["code"], -32600);
    Ok(())
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn oidc_verification_browser_binding_and_logout() -> Result<()> {
    let mut probe = Probe::new()?;
    let unique = stamp()?;
    for (extra, overrides) in [
        (json!({}), json!({"bad_signature":true})),
        (json!({}), json!({"nonce_override":"wrong-nonce"})),
        (json!({"iss":"https://wrong-issuer.test/oidc"}), json!({})),
        (json!({"aud":"wrong-client"}), json!({})),
        (json!({"exp":1}), json!({})),
        (json!({"sub":false}), json!({})),
    ] {
        let login = probe.start("/").await?;
        let mut identity = claims(&unique);
        for (key, value) in extra.as_object().ok_or("claims must be object")? {
            identity[key] = value.clone();
        }
        let approval = probe.approve(&login, identity, overrides).await?;
        api_error(
            probe.callback(&approval, Some(&login.cookie)).await?,
            400,
            "login_failed",
        )
        .await?;
    }
    let login = probe.start("/").await?;
    let approval = probe.approve(&login, claims(&unique), json!({})).await?;
    api_error(probe.callback(&approval, None).await?, 400, "login_failed").await?;
    api_error(
        probe
            .callback(&approval, Some("cr_login=wrong-browser"))
            .await?,
        400,
        "login_failed",
    )
    .await?;
    // A wrong browser consumes the state even though no session is issued.
    api_error(
        probe.callback(&approval, Some(&login.cookie)).await?,
        400,
        "login_failed",
    )
    .await?;
    let login = probe.start("/").await?;
    let approval = probe.approve(&login, claims(&unique), json!({})).await?;
    let mut forged = approval.clone();
    forged["state"] = json!("forged-state");
    api_error(
        probe.callback(&forged, Some(&login.cookie)).await?,
        400,
        "login_failed",
    )
    .await?;
    let accepted = probe.callback(&approval, Some(&login.cookie)).await?;
    assert_eq!(accepted.status().as_u16(), 302);
    api_error(
        probe.callback(&approval, Some(&login.cookie)).await?,
        400,
        "login_failed",
    )
    .await?;
    for path in [
        "/auth/callback",
        "/auth/callback?error=access_denied",
        "/auth/callback?state=only-state",
    ] {
        api_error(
            probe
                .client
                .get(format!("{}{path}", probe.base))
                .send()
                .await?,
            400,
            "login_failed",
        )
        .await?;
    }
    for unsafe_path in [
        "//evil.example/steal",
        "https://evil.example/",
        "evil.example",
        "/\\evil.example",
        "/\t/evil.example",
        "/\n/evil.example",
        "/x\u{7f}",
    ] {
        let login = probe.start(unsafe_path).await?;
        let approval = probe.approve(&login, claims(&unique), json!({})).await?;
        let callback = probe.callback(&approval, Some(&login.cookie)).await?;
        assert_eq!(callback.status().as_u16(), 302);
        assert_eq!(text_header(&callback, "location")?, "/");
    }
    let login = probe.start("/projects/demo?x=1").await?;
    let approval = probe.approve(&login, claims(&unique), json!({})).await?;
    let callback = probe.callback(&approval, Some(&login.cookie)).await?;
    assert_eq!(text_header(&callback, "location")?, "/projects/demo?x=1");
    let login = probe.start("/").await?;
    let approval = probe.approve(&login,json!({"sub":format!("{unique}-unverified"),"email":"admin@conformance.test","email_verified":false}),json!({})).await?;
    let unverified = probe.callback(&approval, Some(&login.cookie)).await?;
    if std::env::var("CANNERY_CONFORMANCE_ALLOWED_EMAIL_DOMAIN").is_ok() {
        api_error(unverified, 403, "forbidden").await?;
    } else {
        assert_eq!(unverified.status().as_u16(), 302);
        let cookie = response_cookie(&unverified, "cr_session")?;
        let me: Value = probe
            .client
            .get(format!("{}/api/me", probe.base))
            .header(
                header::COOKIE,
                cookie.split(';').next().ok_or("empty cookie")?,
            )
            .send()
            .await?
            .json()
            .await?;
        assert_eq!(me["user"]["is_admin"], false);
        assert_eq!(me["user"]["email_verified"], false);
    }
    if std::env::var("CANNERY_CONFORMANCE_ALLOWED_EMAIL_DOMAIN").is_ok() {
        let login = probe.start("/").await?;
        let approval=probe.approve(&login,json!({"sub":format!("{unique}-foreign"),"email":"foreign@not-allowed.test","email_verified":true}),json!({})).await?;
        api_error(
            probe.callback(&approval, Some(&login.cookie)).await?,
            403,
            "forbidden",
        )
        .await?;
    }
    let browser = probe.login(claims(&format!("{unique}-logout"))).await?;
    for wrong in [None, Some("incorrect-csrf")] {
        let mut request = probe
            .client
            .post(format!("{}/auth/logout", probe.base))
            .header(header::COOKIE, &browser.cookie);
        if let Some(wrong) = wrong {
            request = request.header("X-CSRF-Token", wrong);
        }
        api_error(request.send().await?, 403, "csrf_invalid").await?;
    }
    let response = probe
        .harness
        .request(Method::POST, "/auth/logout")?
        .header(header::COOKIE, &browser.cookie)
        .header("X-CSRF-Token", &browser.csrf)
        .send()
        .await?;
    assert!(response_cookie(&response, "cr_session")?.contains("Max-Age=0"));
    let logout = probe
        .harness
        .check_response(Method::POST, "/auth/logout", response, 200)
        .await?
        .body;
    let logout_url = Url::parse(field(&logout, "logout_url")?)?;
    assert_eq!(logout_url.path(), "/oidc/session/end");
    assert!(logout_url.query_pairs().any(
        |(key, value)| key == "post_logout_redirect_uri" && value == format!("{}/", probe.base)
    ));
    let ended = probe
        .harness
        .request(Method::GET, "/api/me")?
        .header(header::COOKIE, &browser.cookie)
        .send()
        .await?;
    probe
        .harness
        .check_response(Method::GET, "/api/me", ended, 401)
        .await?;
    probe.credit("GET /auth/login", &[302]);
    probe.credit("GET /auth/callback", &[302, 400]);
    if std::env::var("CANNERY_CONFORMANCE_ALLOWED_EMAIL_DOMAIN").is_ok() {
        probe.credit("GET /auth/callback", &[403]);
    }
    probe.export("protocol-oidc")?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn mcp_protocol_headers_message_shapes_and_limits() -> Result<()> {
    let mut probe = Probe::new()?;
    let reference_path = PathBuf::from(std::env::var("CANNERY_CONFORMANCE_REFERENCE_PATH")?);
    let reference: Value = serde_json::from_slice(&fs::read(&reference_path)?)?;
    let expected_mcp = reference
        .get("mcp")
        .ok_or("reference omitted MCP contract")?;
    let mut accepted_versions = BTreeSet::new();
    let mut observed_initialize = Value::Null;
    let unique = stamp()?;
    let browser = probe.login(claims(&unique)).await?;
    let token = probe.token(&browser).await?;
    let ping = json!({"jsonrpc":"2.0","id":7,"method":"ping"});
    for authorization in [None, Some("Bearer cr_pat_invalid"), Some("Basic abc")] {
        let mut request = probe.client.post(format!("{}/mcp", probe.base)).json(&ping);
        if let Some(authorization) = authorization {
            request = request.header(header::AUTHORIZATION, authorization);
        }
        let response = request.send().await?;
        assert_eq!(text_header(&response, "www-authenticate")?, "Bearer");
        api_error(response, 401, "unauthenticated").await?;
    }
    let session_secret = browser
        .cookie
        .strip_prefix("cr_session=")
        .ok_or("bad cookie name")?;
    for request in [
        probe
            .client
            .post(format!("{}/mcp", probe.base))
            .header(header::COOKIE, &browser.cookie)
            .header("X-CSRF-Token", &browser.csrf),
        probe
            .client
            .post(format!("{}/mcp", probe.base))
            .bearer_auth(session_secret),
    ] {
        api_error(request.json(&ping).send().await?, 401, "unauthenticated").await?;
    }
    for origin in [
        "https://evil.test",
        "null",
        "http://127.0.0.1:invalid",
        "http://127.0.0.1:80",
        "https://127.0.0.1:9010",
    ] {
        api_error(
            probe
                .client
                .post(format!("{}/mcp", probe.base))
                .bearer_auth(&token)
                .header("Origin", origin)
                .json(&ping)
                .send()
                .await?,
            403,
            "forbidden",
        )
        .await?;
    }
    let response = probe
        .client
        .post(format!("{}/mcp", probe.base))
        .bearer_auth(&token)
        .header("Origin", &probe.base)
        .json(&ping)
        .send()
        .await?;
    assert_eq!(
        response.json::<Value>().await?,
        json!({"jsonrpc":"2.0","id":7,"result":{}})
    );
    for version in ["2025-03-26", "2024-01-01", "2025-06-18-invalid"] {
        api_error(
            probe
                .client
                .post(format!("{}/mcp", probe.base))
                .bearer_auth(&token)
                .header("MCP-Protocol-Version", version)
                .json(&ping)
                .send()
                .await?,
            400,
            "bad_request",
        )
        .await?;
    }
    for requested in ["2025-06-18", "2025-03-26", "1999-01-01"] {
        let response=probe.rpc(Some(&token),json!({"jsonrpc":"2.0","id":"init","method":"initialize","params":{"protocolVersion":requested,"capabilities":{},"clientInfo":{"name":"conformance","version":"1"}}})).await?;
        assert_eq!(response.status().as_u16(), 200);
        let initialized: Value = response.json().await?;
        assert_eq!(initialized["id"], "init");
        assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(
            initialized["result"]["protocolVersion"],
            expected_mcp["protocolVersion"]
        );
        assert_eq!(
            initialized["result"]["instructions"],
            expected_mcp["instructions"]
        );
        accepted_versions.insert(field(&initialized["result"], "protocolVersion")?.to_owned());
        observed_initialize = initialized["result"].clone();
        assert_eq!(
            initialized["result"]["capabilities"],
            json!({"tools":{"listChanged":false}})
        );
        assert_eq!(initialized["result"]["serverInfo"]["name"], "cannery-row");
        assert!(
            initialized["result"]["instructions"]
                .as_str()
                .is_some_and(|text| !text.is_empty())
        );
    }
    for (body, status, code) in [
        (
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
            200,
            -32602,
        ),
        (
            json!({"jsonrpc":"2.0","id":1,"method":"resources/list"}),
            200,
            -32601,
        ),
        (
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"drop_tables"}}),
            200,
            -32602,
        ),
        (
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":[1]}),
            200,
            -32602,
        ),
        (
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_projects","arguments":{"via":"ui"}}}),
            200,
            -32602,
        ),
        (
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_projects","arguments":[1]}}),
            200,
            -32602,
        ),
        (
            json!([{"jsonrpc":"2.0","id":1,"method":"ping"}]),
            400,
            -32600,
        ),
        (json!({"jsonrpc":"1.0","id":1,"method":"ping"}), 400, -32600),
        (
            json!({"jsonrpc":"2.0","id":null,"method":"ping"}),
            400,
            -32600,
        ),
        (
            json!({"jsonrpc":"2.0","id":true,"method":"ping"}),
            400,
            -32600,
        ),
        (json!({"jsonrpc":"2.0","method":3}), 400, -32600),
        (json!({"jsonrpc":"2.0","id":4}), 400, -32600),
        (json!({"jsonrpc":"2.0","result":{}}), 400, -32600),
        (json!(null), 400, -32600),
    ] {
        rpc_error(probe.rpc(Some(&token), body).await?, status, code).await?;
    }
    let invalid=rpc_error(probe.rpc(Some(&token),json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_hypothesis","arguments":{"project":"missing","number":0}}})).await?,200,-32602).await?;
    assert_eq!(
        invalid["error"]["data"][0]["path"],
        if native_server()? {
            "/number"
        } else {
            "number"
        }
    );
    for body in [
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":3,"result":{}}),
        json!({"jsonrpc":"2.0","id":3,"error":{"code":1}}),
    ] {
        let response = probe.rpc(Some(&token), body).await?;
        assert_eq!(response.status().as_u16(), 202);
        assert!(response.bytes().await?.is_empty());
    }
    for malformed in [
        b"{not json".to_vec(),
        vec![0xff],
        format!("{}{}", "[".repeat(100_000), "]".repeat(100_000)).into_bytes(),
    ] {
        rpc_error(
            probe
                .client
                .post(format!("{}/mcp", probe.base))
                .bearer_auth(&token)
                .body(malformed)
                .send()
                .await?,
            400,
            -32700,
        )
        .await?;
    }
    let request_limit = std::env::var("CANNERY_CONFORMANCE_MCP_REQUEST_BYTES")
        .unwrap_or_else(|_| "1048576".into())
        .parse::<usize>()?;
    assert!(request_limit <= 4 * 1024 * 1024);
    rpc_error(
        probe
            .client
            .post(format!("{}/mcp", probe.base))
            .body(vec![b' '; request_limit + 1])
            .send()
            .await?,
        413,
        -32600,
    )
    .await?;
    let stream_base = probe.base.clone();
    tokio::task::spawn_blocking(move || oversized_chunked_request(&stream_base, request_limit + 1))
        .await??;
    let mut exact = serde_json::to_vec(&ping)?;
    exact.resize(request_limit, b' ');
    let response = probe
        .client
        .post(format!("{}/mcp", probe.base))
        .bearer_auth(&token)
        .body(exact)
        .send()
        .await?;
    assert_eq!(
        response.json::<Value>().await?,
        json!({"jsonrpc":"2.0","id":7,"result":{}})
    );
    for method in [Method::GET, Method::DELETE] {
        let response = probe
            .client
            .request(method, format!("{}/mcp", probe.base))
            .send()
            .await?;
        assert_eq!(text_header(&response, "allow")?, "POST");
        api_error(response, 405, "method_not_allowed").await?;
    }
    let listed = probe
        .rpc(
            Some(&token),
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
        )
        .await?;
    let listed: Value = listed.json().await?;
    let tools = listed["result"]["tools"]
        .as_array()
        .ok_or("missing tools")?;
    let mut actual_tools = tools.clone();
    actual_tools.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    let mut reference_tools = expected_mcp["tools"]
        .as_array()
        .ok_or("reference omitted tools")?
        .clone();
    reference_tools.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    if native_server()? {
        let mut native_tools: Vec<Value> =
            serde_json::from_str(include_str!("../../server/src/mcp/tools.json"))?;
        native_tools.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
        assert_eq!(
            native_tools, reference_tools,
            "base tool metadata must remain exact"
        );
        for tool in &mut native_tools {
            for field in ["lease_token", "idempotency_key"] {
                if let Some(property) = tool["inputSchema"]["properties"].get_mut(field) {
                    property["pattern"] = json!("^[\\x20-\\x7e]+$");
                }
            }
            tool["outputSchema"] = json!({"type":"object","additionalProperties":true});
        }
        reference_tools = native_tools;
    }
    assert_eq!(
        actual_tools, reference_tools,
        "MCP descriptions, titles, schemas and annotations must remain exact"
    );
    let accepted_versions: Value = serde_json::to_value(&accepted_versions)?;
    assert_eq!(accepted_versions, expected_mcp["supportedVersions"]);
    let wire_directory = reference_path
        .parent()
        .ok_or("reference has no parent directory")?
        .join("wire");
    fs::create_dir_all(&wire_directory)?;
    fs::write(
        wire_directory.join("actual-mcp.json"),
        serde_json::to_vec_pretty(
            &json!({"tools":actual_tools,"instructions":observed_initialize["instructions"],"protocolVersion":observed_initialize["protocolVersion"],"supportedVersions":accepted_versions}),
        )?,
    )?;
    let expected: Value =
        serde_json::from_str(include_str!("../fixtures/coverage-requirements.json"))?;
    let names: BTreeSet<_> = tools
        .iter()
        .map(|tool| field(tool, "name"))
        .collect::<Result<_>>()?;
    let expected_names: BTreeSet<_> = expected["tools"]
        .as_array()
        .ok_or("tool inventory absent")?
        .iter()
        .map(|name| name.as_str().ok_or_else(|| "invalid tool name".into()))
        .collect::<Result<_>>()?;
    assert_eq!(names, expected_names);
    for tool in tools {
        assert!(field(tool, "description")?.len() > 10);
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        for hint in [
            "readOnlyHint",
            "destructiveHint",
            "idempotentHint",
            "openWorldHint",
        ] {
            assert!(tool["annotations"][hint].is_boolean());
        }
        for forbidden in ["via", "via_channel", "via_client", "actor_user_id"] {
            assert!(tool["inputSchema"]["properties"].get(forbidden).is_none());
        }
        if tool["annotations"]["readOnlyHint"] == true {
            assert_eq!(tool["annotations"]["destructiveHint"], false);
            assert_eq!(tool["annotations"]["idempotentHint"], true);
        }
    }
    let not_found=probe.rpc(Some(&token),json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_track","arguments":{"project":"missing-protocol-project","track":"missing"}}})).await?;
    let failure: Value = not_found.json().await?;
    assert_eq!(failure["result"]["isError"], true);
    assert_eq!(
        failure["result"]["structuredContent"]["error"]["code"],
        "not_found"
    );
    let text: Value = serde_json::from_str(field(&failure["result"]["content"][0], "text")?)?;
    assert_eq!(text, failure["result"]["structuredContent"]);
    let admin=probe.login(json!({"sub":"conformance-admin","email":"admin@conformance.test","email_verified":true,"name":"Protocol admin"})).await?;
    let admin_token = probe.token(&admin).await?;
    let project = format!(
        "protocol-size-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis()
    );
    let base = format!("/api/projects/{project}");
    probe
        .write(
            &admin_token,
            Method::POST,
            "/api/projects",
            "/api/projects",
            json!({"slug":project,"title":"Oversized MCP result"}),
            201,
        )
        .await?;
    probe
        .write(
            &admin_token,
            Method::PUT,
            "/api/projects/{slug}/members/{user_id}",
            &format!("{base}/members/{}", admin.user_id),
            json!({"role":"researcher"}),
            200,
        )
        .await?;
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut science: Value = serde_json::from_slice(&fs::read(
        repository.join("tests/fixtures/contracts/science_revision/valid/stock_evaluator.json"),
    )?)?;
    science["default_producer"] = json!({"name":"sparse-producer","revision":1});
    probe
        .write(
            &admin_token,
            Method::POST,
            "/api/projects/{slug}/config/{kind}",
            &format!("{base}/config/science"),
            science,
            201,
        )
        .await?;
    let producer: Value = serde_json::from_slice(&fs::read(
        repository.join("tests/fixtures/contracts/step_manifest/valid/producer.json"),
    )?)?;
    probe
        .write(
            &admin_token,
            Method::POST,
            "/api/projects/{slug}/producers",
            &format!("{base}/producers"),
            producer,
            201,
        )
        .await?;
    probe
        .write(
            &admin_token,
            Method::POST,
            "/api/projects/{slug}/tracks",
            &format!("{base}/tracks"),
            json!({"slug":"size-probe","title":"Size probe"}),
            201,
        )
        .await?;
    let mut draft: Value = serde_json::from_slice(&fs::read(
        repository.join("tests/fixtures/contracts/hypothesis/valid/minimal.json"),
    )?)?;
    draft["track"] = json!("size-probe");
    draft["project_fields"] = json!({"architecture":"hybrid"});
    let draft = probe
        .write(
            &admin_token,
            Method::POST,
            "/api/projects/{slug}/hypotheses",
            &format!("{base}/hypotheses"),
            draft,
            201,
        )
        .await?;
    let number = draft["number"].as_u64().ok_or("draft omitted number")?;
    let result_limit = std::env::var("CANNERY_CONFORMANCE_MCP_RESULT_BYTES")
        .unwrap_or_else(|_| "1048576".into())
        .parse::<usize>()?;
    assert!((60_000..=2_000_000).contains(&result_limit));
    for _ in 0..=result_limit / 60_000 {
        probe
            .write(
                &admin_token,
                Method::POST,
                "/api/projects/{slug}/hypotheses/{number}/comments",
                &format!("{base}/hypotheses/{number}/comments"),
                json!({"body_markdown":"x".repeat(60_000)}),
                201,
            )
            .await?;
    }
    let oversized=probe.rpc(Some(&admin_token),json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_comments","arguments":{"project":project,"number":number}}})).await?;
    assert_eq!(oversized.status().as_u16(), 200);
    let oversized: Value = oversized.json().await?;
    assert_eq!(oversized["result"]["isError"], true);
    assert_eq!(
        oversized["result"]["structuredContent"]["error"]["code"],
        "result_too_large"
    );
    assert_eq!(
        oversized["result"]["structuredContent"]["error"]["details"]["max_bytes"],
        result_limit
    );
    let smaller = probe
        .harness
        .call_tool(
            &admin_token,
            "list_comments",
            json!({"project":project,"number":number,"limit":1}),
        )
        .await?;
    assert_eq!(
        smaller["items"]
            .as_array()
            .ok_or("comments missing items")?
            .len(),
        1
    );
    probe.credit("POST /mcp", &[200, 202, 400, 401, 403, 413]);
    probe.credit("GET /mcp", &[405]);
    probe.credit("DELETE /mcp", &[405]);
    probe.export("protocol-mcp")?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn public_openapi_documentation_and_web_routing() -> Result<()> {
    let mut probe = Probe::new()?;
    let response = probe
        .harness
        .request(Method::GET, "/api/health")?
        .send()
        .await?;
    let checked = probe
        .harness
        .check_response(Method::GET, "/api/health", response, 200)
        .await?;
    assert_eq!(checked.body["status"], "ok");
    assert_eq!(checked.body["database"], "ok");
    let document = probe
        .client
        .get(format!("{}/openapi.json", probe.base))
        .send()
        .await?;
    assert_eq!(document.status().as_u16(), 200);
    assert_eq!(document.json::<Value>().await?, contract_document()?);
    for path in ["/docs", "/docs/oauth2-redirect", "/redoc"] {
        let response = probe
            .client
            .get(format!("{}{path}", probe.base))
            .send()
            .await?;
        assert_eq!(response.status().as_u16(), 200);
        assert!(text_header(&response, "content-type")?.starts_with("text/html"));
    }
    let root = probe.client.get(format!("{}/", probe.base)).send().await?;
    assert_eq!(
        root.status().as_u16(),
        200,
        "runtime must configure web.dist_dir"
    );
    assert_eq!(text_header(&root, "cache-control")?, "no-cache");
    for (name, value) in [
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("content-security-policy", "frame-ancestors 'none'"),
        ("referrer-policy", "strict-origin-when-cross-origin"),
    ] {
        assert_eq!(text_header(&root, name)?, value);
    }
    let index = root.bytes().await?;
    let head = probe.client.head(format!("{}/", probe.base)).send().await?;
    assert_eq!(head.status().as_u16(), 200);
    assert!(head.bytes().await?.is_empty());
    for path in [
        "/index.html",
        "/projects/demo/hypotheses/42",
        "/projects/v1.2",
        "/hypotheses/12.3",
    ] {
        let response = probe
            .client
            .get(format!("{}{path}", probe.base))
            .send()
            .await?;
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(response.bytes().await?, index);
        let head = probe
            .client
            .head(format!("{}{path}", probe.base))
            .send()
            .await?;
        assert_eq!(head.status().as_u16(), 200);
        assert!(head.bytes().await?.is_empty());
    }
    for path in [
        "/api",
        "/api/protocol-missing",
        "/auth/protocol-missing",
        "/mcp/protocol-missing",
        "/assets/protocol-missing.js",
        "/protocol-missing.ico",
        "/protocol-missing.txt",
    ] {
        let response = probe
            .client
            .get(format!("{}{path}", probe.base))
            .send()
            .await?;
        assert_eq!(response.status().as_u16(), 404);
        assert_eq!(
            response.json::<Value>().await?,
            json!({"detail":"Not Found"})
        );
        let response = probe
            .client
            .head(format!("{}{path}", probe.base))
            .send()
            .await?;
        assert_eq!(response.status().as_u16(), 404);
        assert!(response.bytes().await?.is_empty());
    }
    for (method, path, status) in [
        (Method::PUT, "/mcp", 405),
        (Method::POST, "/projects", 404),
        (Method::POST, "/openapi.json", 405),
    ] {
        assert_eq!(
            probe
                .client
                .request(method, format!("{}{path}", probe.base))
                .send()
                .await?
                .status()
                .as_u16(),
            status
        );
    }
    let html = String::from_utf8(index.to_vec())?;
    let assets: BTreeSet<_> = html
        .split('"')
        .filter(|part| part.starts_with("/assets/"))
        .collect();
    assert!(
        !assets.is_empty(),
        "built web index must reference hashed assets"
    );
    for path in assets {
        let response = probe
            .client
            .get(format!("{}{path}", probe.base))
            .send()
            .await?;
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            text_header(&response, "cache-control")?,
            "public, max-age=31536000, immutable"
        );
        assert_eq!(text_header(&response, "x-content-type-options")?, "nosniff");
        let expected = if std::path::Path::new(path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("css"))
        {
            "text/css"
        } else {
            "text/javascript"
        };
        assert!(text_header(&response, "content-type")?.starts_with(expected));
    }
    for operation in [
        "GET /openapi.json",
        "GET /docs",
        "GET /docs/oauth2-redirect",
        "GET /redoc",
        "GET /",
        "HEAD /",
    ] {
        probe.credit(operation, &[200]);
    }
    probe.export("protocol-public")?;
    Ok(())
}

#[test]
#[ignore = "requires the containerized conformance CLI"]
fn command_line_contract_and_settings_rejections() -> Result<()> {
    let command = std::env::var("CANNERY_CONFORMANCE_CLI").unwrap_or_else(|_| "cannery".into());
    let output = Command::new(&command)
        .arg("openapi")
        .env_remove("CANNERY_SETTINGS")
        .env_remove("CANNERY_DATABASE_URL")
        .output()?;
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout)?,
        contract_document()?
    );
    for args in [
        vec![],
        vec!["unknown-command"],
        vec!["serve", "--invalid-option"],
        vec!["runner"],
        vec![
            "runner",
            "--api-url",
            "http://127.0.0.1:9010",
            "--project",
            "fixture",
            "--data-root",
            "/workspace/examples/fixture/data",
        ],
        vec!["evaluator"],
        vec![
            "import",
            "--bundle",
            "/missing-protocol-bundle",
            "--project",
            "fixture",
        ],
    ] {
        let operational_import = args.first() == Some(&"import");
        let output = Command::new(&command).args(args).output()?;
        // Clap usage/configuration refusals are 2; a parsed native import
        // that cannot read its bundle/settings is an operational failure, 1.
        let expected = if native_server()? && operational_import {
            1
        } else {
            2
        };
        assert_eq!(output.status.code(), Some(expected));
        assert_ne!(output.stderr.len(), 0);
    }
    for (variable, value) in [
        ("CANNERY_LEASES_TTL_SECONDS", "0"),
        ("CANNERY_LEASES_JOB_TTL_SECONDS", "invalid"),
        ("CANNERY_STORAGE_MAX_OBJECT_BYTES", "-1"),
    ] {
        let output = Command::new(&command)
            .arg("migrate")
            .env("CANNERY_DATABASE_URL", "postgresql://unused.invalid/unused")
            .env(variable, value)
            .output()?;
        assert_eq!(output.status.code(), Some(1));
        let error = String::from_utf8(output.stderr)?;
        let suffix = variable.trim_start_matches("CANNERY_").to_lowercase();
        let (section, name) = suffix.split_once('_').ok_or("setting has no section")?;
        assert!(
            error.contains(variable) || error.contains(&format!("{section}.{name}")),
            "configuration error must name invalid setting"
        );
    }
    Ok(())
}
