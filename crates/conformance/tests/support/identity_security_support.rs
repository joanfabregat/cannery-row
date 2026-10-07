use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use conformance::{Harness, Result};
use reqwest::{Client, Method, RequestBuilder, Response, header::HeaderMap};
use serde_json::{Value, json};
use std::path::PathBuf;

pub struct Context {
    pub h: Harness,
    pub client: Client,
    pub base: String,
    pub oidc: String,
    control: String,
}
pub struct Started {
    pub binding: String,
    pub state: String,
    pub code: String,
}
pub struct Session {
    pub cookie: String,
    pub csrf: String,
    pub me: Value,
}
pub struct AdminActor {
    pub admin: String,
}
pub struct ProjectFixture {
    pub project: String,
}
impl ProjectFixture {
    pub fn base(&self) -> String {
        format!("/api/projects/{}", self.project)
    }
}
pub struct ProjectActors {
    pub admin: String,
    pub agent: String,
}
pub fn string(value: &Value) -> Result<String> {
    Ok(value.as_str().ok_or("response string absent")?.to_owned())
}
pub fn opaque(value: &str, prefix: &str) -> Result<()> {
    if !value.starts_with(prefix)
        || value.len() != prefix.len() + 43
        || URL_SAFE_NO_PAD
            .decode(&value[prefix.len()..])
            .map_err(|_| "opaque secret encoding differs")?
            .len()
            != 32
    {
        return Err("opaque secret prefix, length or encoding differs".into());
    }
    Ok(())
}
fn cookie(headers: &HeaderMap, name: &str) -> Result<String> {
    let prefix = format!("{name}=");
    headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| value.split(';').next())
        .find(|value| value.starts_with(&prefix))
        .map(ToOwned::to_owned)
        .ok_or_else(|| "expected browser cookie absent".into())
}
impl Context {
    pub async fn admin_actor(&mut self, label: &str) -> Result<(Session, AdminActor)> {
        let session = self
            .login(
                claims("conformance-admin", "admin@conformance.test", true),
                "/",
            )
            .await?;
        let token = self.mint(&session, label, &["read", "write"]).await?;
        Ok((
            session,
            AdminActor {
                admin: string(&token["token"])?,
            },
        ))
    }
    /// Public identity/project setup deliberately registers no research configuration or records.
    pub async fn project_fixture(
        &mut self,
        label: &str,
    ) -> Result<(ProjectFixture, ProjectActors)> {
        let (session, admin) = self.admin_actor(label).await?;
        let token_id = self
            .api(Method::GET, "/api/me", "/api/me", &admin.admin, None, 200)
            .await?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let world = ProjectFixture {
            project: format!("identity-{label}-{nonce}"),
        };
        self.api(
            Method::POST,
            "/api/projects",
            "/api/projects",
            &admin.admin,
            Some(json!({"slug":world.project,"title":"Identity conformance"})),
            201,
        )
        .await?;
        self.api(
            Method::PUT,
            "/api/projects/{slug}/members/{user_id}",
            &format!(
                "{}/members/{}",
                world.base(),
                string(&token_id["user"]["id"])?
            ),
            &admin.admin,
            Some(json!({"role":"researcher"})),
            200,
        )
        .await?;
        self.api(
            Method::POST,
            "/api/projects/{slug}/service-accounts",
            &format!("{}/service-accounts", world.base()),
            &admin.admin,
            Some(json!({"kind":"agent","name":"identity-agent"})),
            201,
        )
        .await?;
        let token = self
            .browser_api(
                Method::POST,
                "/api/projects/{slug}/service-accounts/{name}/tokens",
                &format!("{}/service-accounts/identity-agent/tokens", world.base()),
                &session,
                Some(
                    json!({"name":"identity-agent","expires_in_days":1,"scopes":["read","write"]}),
                ),
                201,
            )
            .await?;
        Ok((
            world,
            ProjectActors {
                admin: admin.admin,
                agent: string(&token["token"])?,
            },
        ))
    }
    pub fn new() -> Result<Self> {
        let base = std::env::var("CANNERY_CONFORMANCE_URL")?;
        Ok(Self {
            h: Harness::new(&base)?,
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            base,
            oidc: std::env::var("CANNERY_CONFORMANCE_OIDC_URL")?,
            control: std::env::var("CANNERY_TEST_OIDC_CONTROL_TOKEN")?,
        })
    }
    pub async fn begin(&self, claims: Value, return_to: &str, options: Value) -> Result<Started> {
        let response = self
            .client
            .get(format!("{}/auth/login", self.base))
            .query(&[("return_to", return_to)])
            .send()
            .await?;
        assert_eq!(response.status().as_u16(), 302);
        let binding = cookie(response.headers(), "cr_login")?;
        opaque(
            binding
                .strip_prefix("cr_login=")
                .ok_or("login binding format differs")?,
            "",
        )?;
        let raw = response
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|v| v.starts_with("cr_login="))
            .ok_or("login cookie attributes absent")?;
        assert!(
            raw.contains("HttpOnly")
                && raw.contains("Path=/auth")
                && raw.to_lowercase().contains("samesite=lax"),
            "login cookie attributes differ"
        );
        let location = response
            .headers()
            .get("location")
            .ok_or("login redirect absent")?
            .to_str()?;
        let url = reqwest::Url::parse(location)?;
        let query: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(
            query.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(query.get("response_type").map(String::as_str), Some("code"));
        assert_eq!(
            query
                .get("code_challenge")
                .ok_or("PKCE challenge absent")?
                .len(),
            43
        );
        let mut approval = json!({"authorization_url":location,"claims":claims});
        approval
            .as_object_mut()
            .ok_or("approval object absent")?
            .extend(
                options
                    .as_object()
                    .ok_or("approval options absent")?
                    .clone(),
            );
        let body: Value = self
            .client
            .post(format!("{}/__conformance/approve", self.oidc))
            .bearer_auth(&self.control)
            .json(&approval)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(Started {
            binding,
            state: string(&body["state"])?,
            code: string(&body["code"])?,
        })
    }
    pub async fn callback(&self, start: &Started, binding: Option<&str>) -> Result<Response> {
        let mut request = self
            .client
            .get(format!("{}/auth/callback", self.base))
            .query(&[("state", &start.state), ("code", &start.code)]);
        if let Some(binding) = binding {
            request = request.header("cookie", binding);
        }
        request
            .send()
            .await
            .map_err(|_| "callback transport failed".into())
    }
    pub async fn login(&mut self, claims: Value, return_to: &str) -> Result<Session> {
        let start = self.begin(claims, return_to, json!({})).await?;
        let response = self.callback(&start, Some(&start.binding)).await?;
        assert_eq!(response.status().as_u16(), 302);
        assert_eq!(
            response
                .headers()
                .get("location")
                .ok_or("callback redirect absent")?
                .to_str()?,
            return_to
        );
        self.session(response).await
    }
    pub async fn session(&mut self, response: Response) -> Result<Session> {
        let session = cookie(response.headers(), "cr_session")?;
        opaque(
            session
                .strip_prefix("cr_session=")
                .ok_or("session cookie format differs")?,
            "cr_ses_",
        )?;
        let raw = response
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|v| v.starts_with("cr_session="))
            .ok_or("session cookie attributes absent")?;
        assert!(
            raw.contains("HttpOnly")
                && raw.contains("Path=/")
                && raw.to_lowercase().contains("samesite=lax"),
            "session cookie attributes differ"
        );
        assert!(
            response
                .headers()
                .get_all("set-cookie")
                .iter()
                .filter_map(|v| v.to_str().ok())
                .any(|v| v.starts_with("cr_login=") && v.contains("Max-Age=0")),
            "login binding cookie not cleared"
        );
        let request = self
            .h
            .request(Method::GET, "/api/me")?
            .header("cookie", &session);
        let me = self.checked(Method::GET, "/api/me", request, 200).await?;
        assert_eq!(me["kind"], "user");
        assert_eq!(me["channel"], "ui");
        opaque(&string(&me["csrf_token"])?, "")?;
        Ok(Session {
            cookie: session,
            csrf: string(&me["csrf_token"])?,
            me,
        })
    }
    pub async fn checked(
        &mut self,
        method: Method,
        template: &str,
        request: RequestBuilder,
        status: u16,
    ) -> Result<Value> {
        Ok(self
            .h
            .check_response(method, template, request.send().await?, status)
            .await?
            .body)
    }
    pub async fn api(
        &mut self,
        method: Method,
        template: &str,
        path: &str,
        token: &str,
        body: Option<Value>,
        status: u16,
    ) -> Result<Value> {
        let mut request = self.h.request(method.clone(), path)?.bearer_auth(token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        self.checked(method, template, request, status).await
    }
    pub async fn browser_api(
        &mut self,
        method: Method,
        template: &str,
        path: &str,
        session: &Session,
        body: Option<Value>,
        status: u16,
    ) -> Result<Value> {
        let mut request = self
            .h
            .request(method.clone(), path)?
            .header("cookie", &session.cookie)
            .header("X-CSRF-Token", &session.csrf);
        if let Some(body) = body {
            request = request.json(&body);
        }
        self.checked(method, template, request, status).await
    }
    pub async fn mint(&mut self, session: &Session, name: &str, scopes: &[&str]) -> Result<Value> {
        self.browser_api(
            Method::POST,
            "/api/tokens",
            "/api/tokens",
            session,
            Some(json!({"name":name,"expires_in_days":1,"scopes":scopes})),
            201,
        )
        .await
    }
    pub async fn stats(&self) -> Result<Value> {
        Ok(self
            .client
            .get(format!("{}/__conformance/oidc/stats", self.oidc))
            .bearer_auth(&self.control)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
    pub async fn finish(&mut self, admin: &str, label: &str) -> Result<()> {
        self.h.fetch_audit(admin, 0).await?;
        if let Ok(directory) = std::env::var("CANNERY_CONFORMANCE_COVERAGE_DIR") {
            let directory = PathBuf::from(directory);
            std::fs::create_dir_all(&directory)?;
            std::fs::write(
                directory.join(format!("identity-{label}.json")),
                serde_json::to_vec_pretty(self.h.coverage())?,
            )?;
        }
        Ok(())
    }
}
pub fn claims(sub: &str, email: &str, verified: bool) -> Value {
    json!({"sub":sub,"email":email,"email_verified":verified,"name":"Identity security scenario"})
}
