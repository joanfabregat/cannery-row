use conformance::Harness;
use reqwest::{Client, Method, header};
use serde_json::{Value, json};
use std::{
    error::Error,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

struct Session {
    cookie: String,
    csrf: String,
    id: String,
}
struct World {
    harness: Harness,
    base: String,
    client: Client,
}
impl World {
    fn new() -> Result<Self> {
        let base = std::env::var("CANNERY_CONFORMANCE_URL")?;
        Ok(Self {
            harness: Harness::new(&base)?,
            base,
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }
    async fn api(
        &mut self,
        method: Method,
        template: &str,
        path: &str,
        session: Option<&Session>,
        token: Option<&str>,
        body: Option<Value>,
        status: u16,
    ) -> Result<Value> {
        let mut request = self.harness.request(method.clone(), path)?;
        if let Some(session) = session {
            request = request
                .header(header::COOKIE, &session.cookie)
                .header("X-CSRF-Token", &session.csrf);
        }
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        Ok(self
            .harness
            .check_response(method, template, request.send().await?, status)
            .await?
            .body)
    }
    async fn login(&mut self, subject: &str, email: &str) -> Result<Session> {
        let login = self
            .client
            .get(format!("{}/auth/login?return_to=%2F", self.base))
            .send()
            .await?;
        assert_eq!(login.status().as_u16(), 302);
        let cookie = response_cookie(&login, "cr_login")?;
        let location = login
            .headers()
            .get(header::LOCATION)
            .ok_or("login omitted location")?
            .to_str()?;
        let provider = std::env::var("CANNERY_CONFORMANCE_OIDC_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:9011".into());
        let control = std::env::var("CANNERY_TEST_OIDC_CONTROL_TOKEN")?;
        let approved = self.client.post(format!("{provider}/__conformance/approve")).bearer_auth(control).json(&json!({"authorization_url":location,"claims":{"sub":subject,"email":email,"email_verified":true,"name":subject}})).send().await?;
        assert_eq!(approved.status().as_u16(), 200);
        let approved: Value = approved.json().await?;
        let callback = self
            .client
            .get(format!("{}/auth/callback", self.base))
            .header(header::COOKIE, cookie)
            .query(&[
                ("state", string(&approved, "state")?),
                ("code", string(&approved, "code")?),
            ])
            .send()
            .await?;
        assert_eq!(callback.status().as_u16(), 302);
        let cookie = response_cookie(&callback, "cr_session")?;
        let me = self
            .api(Method::GET, "/api/me", "/api/me", None, None, None, 401)
            .await?;
        assert_code(&me, "unauthenticated");
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
        Ok(Session {
            cookie,
            csrf: string(&me, "csrf_token")?.into(),
            id: me
                .get("user")
                .and_then(|u| u.get("id"))
                .or_else(|| me.get("user_id"))
                .and_then(Value::as_str)
                .ok_or("me omitted user id")?
                .into(),
        })
    }
    async fn token(
        &mut self,
        session: &Session,
        name: &str,
        scopes: &[&str],
    ) -> Result<(String, String)> {
        let token = self
            .api(
                Method::POST,
                "/api/tokens",
                "/api/tokens",
                Some(session),
                None,
                Some(json!({"name":name,"expires_in_days":1,"scopes":scopes})),
                201,
            )
            .await?;
        let secret = string(&token, "token")?.to_owned();
        assert!(secret.starts_with("cr_pat_"));
        Ok((secret, string(&token, "id")?.into()))
    }
    fn export(&self) -> Result<()> {
        if let Ok(directory) = std::env::var("CANNERY_CONFORMANCE_COVERAGE_DIR") {
            fs::create_dir_all(&directory)?;
            let coverage = self.harness.coverage();
            fs::write(
                PathBuf::from(directory).join("runner-cli.json"),
                serde_json::to_vec_pretty(
                    &json!({"operations":coverage.operations,"tools":coverage.tools,"audit_actions":coverage.audit_actions}),
                )?,
            )?;
        }
        Ok(())
    }
}
fn response_cookie(response: &reqwest::Response, name: &str) -> Result<String> {
    for cookie in response.headers().get_all(header::SET_COOKIE) {
        let value = cookie.to_str()?.split(';').next().ok_or("empty cookie")?;
        if value.starts_with(&format!("{name}=")) {
            return Ok(value.into());
        }
    }
    Err(format!("missing {name} cookie").into())
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string {key}").into())
}
fn assert_code(value: &Value, code: &str) {
    assert_eq!(
        value.pointer("/error/code").and_then(Value::as_str),
        Some(code),
        "response error code must match"
    );
}
fn fixture(path: &str) -> Result<Value> {
    Ok(serde_json::from_str(&fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path),
    )?)?)
}
fn unique() -> Result<String> {
    Ok(format!(
        "conformance-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis()
    ))
}
