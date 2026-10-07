//! Public API setup and research scenarios; no database seeding is permitted.
#![allow(clippy::too_many_arguments, clippy::too_many_lines)]
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
    async fn tool(&mut self, token: &str, name: &str, args: Value) -> Result<Value> {
        let value = self.harness.call_tool(token, name, args).await?;
        if let Some(content) = value.get("structuredContent") {
            return Ok(content.clone());
        }
        if let Some(text) = value
            .get("content")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|v| v.get("text"))
            .and_then(Value::as_str)
        {
            return Ok(serde_json::from_str(text)?);
        }
        Ok(value)
    }
    async fn tool_error(&self, token: &str, name: &str, args: Value, code: &str) -> Result<()> {
        let response = self
            .harness
            .rpc(
                Some(token),
                "tools/call",
                json!({"name":name,"arguments":args}),
            )
            .await?;
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await?;
        let result = &body["result"];
        assert_eq!(result["isError"], true);
        assert_code(&result["structuredContent"], code);
        let text: Value = serde_json::from_str(
            result["content"][0]["text"]
                .as_str()
                .ok_or("MCP text missing")?,
        )?;
        assert_eq!(text, result["structuredContent"]);
        Ok(())
    }
    fn export(&self) -> Result<()> {
        if let Ok(directory) = std::env::var("CANNERY_CONFORMANCE_COVERAGE_DIR") {
            fs::create_dir_all(&directory)?;
            let coverage = self.harness.coverage();
            fs::write(
                PathBuf::from(directory).join("admin-research.json"),
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
        "{value}"
    );
}
fn assert_violation_path(value: &Value, path: &str) {
    assert!(
        value
            .pointer("/error/details")
            .and_then(Value::as_array)
            .is_some_and(|details| details.iter().any(|detail| detail["path"] == path)),
        "missing violation path {path}: {value}"
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

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn identity_projects_and_research_via_rest_and_mcp() -> Result<()> {
    let mut world = World::new()?;
    let run = unique()?;
    let admin = world
        .login("conformance-admin", "admin@conformance.test")
        .await?;
    let researcher = world
        .login(
            &format!("{run}-researcher"),
            &format!("{run}-researcher@conformance.test"),
        )
        .await?;
    let viewer = world
        .login(
            &format!("{run}-viewer"),
            &format!("{run}-viewer@conformance.test"),
        )
        .await?;
    let outsider = world
        .login(
            &format!("{run}-outsider"),
            &format!("{run}-outsider@conformance.test"),
        )
        .await?;
    let (admin_token, _) = world.token(&admin, "admin", &["read", "write"]).await?;
    let (researcher_token, _) = world
        .token(&researcher, "researcher", &["read", "write"])
        .await?;
    let (viewer_token, _) = world.token(&viewer, "viewer", &["read", "write"]).await?;
    let (readonly_token, revoked_id) = world.token(&researcher, "readonly", &["read"]).await?;
    let (outsider_token, _) = world
        .token(&outsider, "outsider", &["read", "write"])
        .await?;
    let base = format!("/api/projects/{run}");
    let project = json!({"slug":run,"title":"Conformance research"});
    world
        .api(
            Method::POST,
            "/api/projects",
            "/api/projects",
            None,
            Some(&admin_token),
            Some(project.clone()),
            201,
        )
        .await?;
    let conflict = world
        .api(
            Method::POST,
            "/api/projects",
            "/api/projects",
            None,
            Some(&admin_token),
            Some(project),
            409,
        )
        .await?;
    assert_code(&conflict, "conflict");
    for (user, role) in [(&researcher, "researcher"), (&viewer, "viewer")] {
        world
            .api(
                Method::PUT,
                "/api/projects/{slug}/members/{user_id}",
                &format!("{base}/members/{}", user.id),
                None,
                Some(&admin_token),
                Some(json!({"role":role})),
                200,
            )
            .await?;
    }
    world
        .api(
            Method::GET,
            "/api/projects/{slug}/members",
            &format!("{base}/members"),
            None,
            Some(&admin_token),
            None,
            200,
        )
        .await?;
    world
        .api(
            Method::GET,
            "/api/users",
            "/api/users?email=conformance.test",
            None,
            Some(&admin_token),
            None,
            200,
        )
        .await?;
    world
        .api(
            Method::GET,
            "/api/users",
            "/api/users?email=conformance.test",
            None,
            Some(&researcher_token),
            None,
            403,
        )
        .await?;
    world
        .api(
            Method::GET,
            "/api/tokens",
            "/api/tokens",
            Some(&researcher),
            None,
            None,
            200,
        )
        .await?;
    world
        .api(
            Method::POST,
            "/api/tokens",
            "/api/tokens",
            None,
            Some(&researcher_token),
            Some(json!({"name":"cannot-mint","expires_in_days":1,"scopes":["read"]})),
            403,
        )
        .await?;
    world
        .api(
            Method::POST,
            "/api/tokens",
            "/api/tokens",
            Some(&researcher),
            None,
            Some(json!({"name":"bad","expires_in_days":0,"scopes":["read"]})),
            422,
        )
        .await?;
    let missing_csrf = world
        .harness
        .request(Method::POST, "/api/tokens")?
        .header(header::COOKIE, &researcher.cookie)
        .json(&json!({"name":"csrf","expires_in_days":1,"scopes":["read"]}))
        .send()
        .await?;
    let missing_csrf = world
        .harness
        .check_response(Method::POST, "/api/tokens", missing_csrf, 403)
        .await?
        .body;
    assert_code(&missing_csrf, "csrf_invalid");
    for (path, status, token) in [
        (base.clone(), 404, Some(outsider_token.as_str())),
        (
            "/api/projects/no-such-conformance-project".into(),
            404,
            Some(admin_token.as_str()),
        ),
        (base.clone(), 401, None),
    ] {
        world
            .api(
                Method::GET,
                "/api/projects/{slug}",
                &path,
                None,
                token,
                None,
                status,
            )
            .await?;
    }
    world
        .api(
            Method::GET,
            "/api/projects/{slug}",
            &base,
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    world
        .api(
            Method::GET,
            "/api/projects",
            "/api/projects?limit=0",
            None,
            Some(&researcher_token),
            None,
            422,
        )
        .await?;
    let projects = world
        .api(
            Method::GET,
            "/api/projects",
            "/api/projects",
            None,
            Some(&researcher_token),
            None,
            200,
        )
        .await?;
    assert_eq!(
        projects,
        world
            .tool(&researcher_token, "list_projects", json!({}))
            .await?
    );
    let sa = format!("{base}/service-accounts");
    for (name, kind) in [
        ("codex", "agent"),
        ("tester", "tester"),
        ("stock-evaluator", "evaluator"),
        ("experimenter", "experimenter"),
    ] {
        world
            .api(
                Method::POST,
                "/api/projects/{slug}/service-accounts",
                &sa,
                None,
                Some(&admin_token),
                Some(json!({"name":name,"kind":kind})),
                201,
            )
            .await?;
    }
    world
        .api(
            Method::GET,
            "/api/projects/{slug}/service-accounts",
            &sa,
            None,
            Some(&admin_token),
            None,
            200,
        )
        .await?;
    let service_token = world
        .api(
            Method::POST,
            "/api/projects/{slug}/service-accounts/{name}/tokens",
            &format!("{sa}/codex/tokens"),
            Some(&admin),
            None,
            Some(json!({"name":"agent","expires_in_days":1,"scopes":["read","write"]})),
            201,
        )
        .await?;
    let agent_token = string(&service_token, "token")?.to_owned();
    assert!(agent_token.starts_with("cr_svc_"));
    world
        .api(
            Method::GET,
            "/api/projects/{slug}/service-accounts/{name}/tokens",
            &format!("{sa}/codex/tokens"),
            None,
            Some(&admin_token),
            None,
            200,
        )
        .await?;

    let mut science =
        fixture("tests/fixtures/contracts/science_revision/valid/stock_evaluator.json")?;
    science["default_producer"] = json!({"name":"sparse-producer","revision":1});
    world
        .api(
            Method::POST,
            "/api/projects/{slug}/config/{kind}",
            &format!("{base}/config/science"),
            None,
            Some(&admin_token),
            Some(science),
            201,
        )
        .await?;
    for (template, suffix) in [
        ("/api/projects/{slug}/config/{kind}", "science"),
        (
            "/api/projects/{slug}/config/{kind}/latest",
            "science/latest",
        ),
        ("/api/projects/{slug}/config/{kind}/{revision}", "science/1"),
    ] {
        world
            .api(
                Method::GET,
                template,
                &format!("{base}/config/{suffix}"),
                None,
                Some(&viewer_token),
                None,
                200,
            )
            .await?;
    }
    let producer = fixture("tests/fixtures/contracts/step_manifest/valid/producer.json")?;
    world
        .api(
            Method::POST,
            "/api/projects/{slug}/producers",
            &format!("{base}/producers"),
            None,
            Some(&admin_token),
            Some(producer),
            201,
        )
        .await?;
    world
        .api(
            Method::GET,
            "/api/projects/{slug}/producers",
            &format!("{base}/producers"),
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    world
        .api(
            Method::GET,
            "/api/projects/{slug}/producers/{name}/{revision}",
            &format!("{base}/producers/sparse-producer/1"),
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    let track = world
        .tool(
            &researcher_token,
            "create_track",
            json!({"project":run,"document":{"slug":"api-track","title":"API track"}}),
        )
        .await?;
    assert_eq!(track["slug"], "api-track");
    world
        .api(
            Method::POST,
            "/api/projects/{slug}/tracks",
            &format!("{base}/tracks"),
            None,
            Some(&researcher_token),
            Some(json!({"slug":"rest-track","title":"REST track"})),
            201,
        )
        .await?;
    let tracks = world
        .api(
            Method::GET,
            "/api/projects/{slug}/tracks",
            &format!("{base}/tracks"),
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    assert_eq!(
        tracks,
        world
            .tool(&viewer_token, "list_tracks", json!({"project":run}))
            .await?
    );
    let path = format!("{base}/tracks/api-track");
    let rest = world
        .api(
            Method::GET,
            "/api/projects/{slug}/tracks/{track_slug}",
            &path,
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    assert_eq!(
        rest,
        world
            .tool(
                &viewer_token,
                "get_track",
                json!({"project":run,"track":"api-track"})
            )
            .await?
    );
    world
        .api(
            Method::PATCH,
            "/api/projects/{slug}/tracks/{track_slug}",
            &path,
            None,
            Some(&researcher_token),
            Some(json!({"expected_revision":1,"title":"Changed"})),
            200,
        )
        .await?;
    let updated = world.tool(&researcher_token,"update_track",json!({"project":run,"track":"api-track","expected_revision":2,"description":"Via MCP"})).await?;
    assert_eq!(updated["revision"], 3);
    assert_eq!(updated["description"], "Via MCP");
    for (token, code, revision) in [
        (&readonly_token, "forbidden", 3),
        (&viewer_token, "forbidden", 3),
        (&researcher_token, "stale_revision", 1),
    ] {
        world.tool_error(token, "update_track", json!({"project":run,"track":"api-track","expected_revision":revision,"title":"Must not change"}), code).await?;
    }
    world
        .tool_error(
            &outsider_token,
            "get_track",
            json!({"project":run,"track":"api-track"}),
            "not_found",
        )
        .await?;
    let stale = world
        .api(
            Method::PATCH,
            "/api/projects/{slug}/tracks/{track_slug}",
            &path,
            None,
            Some(&researcher_token),
            Some(json!({"expected_revision":1,"title":"Stale"})),
            409,
        )
        .await?;
    assert_code(&stale, "stale_revision");
    world
        .api(
            Method::PATCH,
            "/api/projects/{slug}/tracks/{track_slug}",
            &path,
            None,
            Some(&readonly_token),
            Some(json!({"expected_revision":3,"title":"Forbidden"})),
            403,
        )
        .await?;
    world
        .api(
            Method::PATCH,
            "/api/projects/{slug}/tracks/{track_slug}",
            &path,
            None,
            Some(&researcher_token),
            Some(json!({"expected_revision":3,"alien":true})),
            422,
        )
        .await?;
    let paused = world.tool(&researcher_token,"transition_track",json!({"project":run,"track":"api-track","document":{"to_state":"paused","expected_revision":3,"reason":"Test transition"}})).await?;
    assert_eq!(paused["state"], "paused");
    assert_eq!(paused["revision"], 4);
    world
        .api(
            Method::POST,
            "/api/projects/{slug}/tracks/{track_slug}/transitions",
            &format!("{path}/transitions"),
            None,
            Some(&researcher_token),
            Some(json!({"to_state":"active","expected_revision":4,"reason":"Resume"})),
            200,
        )
        .await?;
    let history = world
        .api(
            Method::GET,
            "/api/projects/{slug}/tracks/{track_slug}/history",
            &format!("{path}/history"),
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    assert_eq!(
        history,
        world
            .tool(
                &viewer_token,
                "track_history",
                json!({"project":run,"track":"api-track"})
            )
            .await?
    );

    let mut draft = fixture("tests/fixtures/contracts/hypothesis/valid/minimal.json")?;
    draft["track"] = json!("api-track");
    draft["title"] = json!("Conformance hybrid draft");
    draft["project_fields"] = json!({"architecture":"hybrid"});
    let first = world
        .api(
            Method::POST,
            "/api/projects/{slug}/hypotheses",
            &format!("{base}/hypotheses"),
            None,
            Some(&agent_token),
            Some(draft.clone()),
            201,
        )
        .await?;
    let number = first["number"]
        .as_u64()
        .ok_or("hypothesis omitted number")?;
    let hypothesis = format!("{base}/hypotheses/{number}");
    world
        .api(
            Method::PUT,
            "/api/projects/{slug}/hypotheses/{number}",
            &hypothesis,
            None,
            Some(&agent_token),
            Some(json!({"expected_revision":1,"document":draft})),
            200,
        )
        .await?;
    world
        .tool(
            &agent_token,
            "revise_draft",
            json!({"project":run,"number":number,"expected_revision":2,"document":draft}),
        )
        .await?;
    let listed_drafts = world
        .api(
            Method::GET,
            "/api/projects/{slug}/hypotheses",
            &format!("{base}/hypotheses"),
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    assert_eq!(
        listed_drafts,
        world
            .tool(&viewer_token, "list_hypotheses", json!({"project":run}))
            .await?
    );
    let detail = world
        .api(
            Method::GET,
            "/api/projects/{slug}/hypotheses/{number}",
            &hypothesis,
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    assert_eq!(
        detail,
        world
            .tool(
                &viewer_token,
                "get_hypothesis",
                json!({"project":run,"number":number})
            )
            .await?
    );
    let revisions = world
        .api(
            Method::GET,
            "/api/projects/{slug}/hypotheses/{number}/revisions",
            &format!("{hypothesis}/revisions"),
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    assert_eq!(
        revisions,
        world
            .tool(
                &viewer_token,
                "list_hypothesis_revisions",
                json!({"project":run,"number":number})
            )
            .await?
    );
    world
        .api(
            Method::GET,
            "/api/projects/{slug}/hypotheses/{number}/revisions/{revision}",
            &format!("{hypothesis}/revisions/1"),
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    world
        .api(
            Method::POST,
            "/api/projects/{slug}/hypotheses/{number}/draft-review",
            &format!("{hypothesis}/draft-review"),
            None,
            Some(&researcher_token),
            Some(json!({"draft_revision":3,"action":"request_revision","reason":"Clarify"})),
            200,
        )
        .await?;
    world
        .tool(
            &agent_token,
            "revise_draft",
            json!({"project":run,"number":number,"expected_revision":3,"document":draft}),
        )
        .await?;
    world
        .tool_error(
            &readonly_token,
            "create_draft",
            json!({"project":run,"document":draft}),
            "forbidden",
        )
        .await?;
    world.tool_error(&viewer_token, "review_draft", json!({"project":run,"number":number,"draft_revision":4,"action":"approve","reason":"Must not approve"}), "forbidden").await?;
    world
        .tool_error(
            &agent_token,
            "revise_draft",
            json!({"project":run,"number":number,"expected_revision":1,"document":draft}),
            "stale_revision",
        )
        .await?;
    world
        .tool_error(
            &outsider_token,
            "get_hypothesis",
            json!({"project":run,"number":number}),
            "not_found",
        )
        .await?;
    let approved = world.tool(&researcher_token,"review_draft",json!({"project":run,"number":number,"draft_revision":4,"action":"approve","reason":"Ready"})).await?;
    assert_eq!(approved["state"], "queued");
    assert_eq!(approved["approved_revision"], 4);
    let second = world
        .tool(
            &agent_token,
            "create_draft",
            json!({"project":run,"document":draft}),
        )
        .await?;
    let second_number = second["number"]
        .as_u64()
        .ok_or("MCP draft omitted number")?;
    world.tool(&researcher_token,"review_draft",json!({"project":run,"number":second_number,"draft_revision":1,"action":"decline","reason":"Duplicate direction"})).await?;

    let comments = format!("{hypothesis}/comments");
    let comment = world
        .api(
            Method::POST,
            "/api/projects/{slug}/hypotheses/{number}/comments",
            &comments,
            None,
            Some(&researcher_token),
            Some(json!({"body_markdown":"Conformance comment"})),
            201,
        )
        .await?;
    let comment_id = string(&comment, "id")?;
    let comment_path = format!("{base}/comments/{comment_id}");
    world
        .api(
            Method::PUT,
            "/api/projects/{slug}/comments/{comment_id}",
            &comment_path,
            None,
            Some(&researcher_token),
            Some(json!({"expected_revision":1,"body_markdown":"Revised comment"})),
            200,
        )
        .await?;
    world.tool(&researcher_token,"edit_comment",json!({"project":run,"comment_id":comment_id,"expected_revision":2,"body_markdown":"Edited through MCP"})).await?;
    world
        .api(
            Method::GET,
            "/api/projects/{slug}/comments/{comment_id}",
            &comment_path,
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    world
        .api(
            Method::GET,
            "/api/projects/{slug}/comments/{comment_id}/revisions",
            &format!("{comment_path}/revisions"),
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    world
        .tool(
            &researcher_token,
            "comment",
            json!({"project":run,"number":number,"body_markdown":"MCP comment"}),
        )
        .await?;
    let listed = world
        .api(
            Method::GET,
            "/api/projects/{slug}/hypotheses/{number}/comments",
            &comments,
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    assert_eq!(
        listed,
        world
            .tool(
                &viewer_token,
                "list_comments",
                json!({"project":run,"number":number})
            )
            .await?
    );
    world
        .api(
            Method::PUT,
            "/api/projects/{slug}/comments/{comment_id}",
            &comment_path,
            None,
            Some(&researcher_token),
            Some(json!({"expected_revision":1,"body_markdown":"Stale"})),
            409,
        )
        .await?;
    world
        .api(
            Method::POST,
            "/api/projects/{slug}/hypotheses/{number}/comments",
            &comments,
            None,
            Some(&viewer_token),
            Some(json!({"body_markdown":"Forbidden"})),
            403,
        )
        .await?;
    world
        .api(
            Method::POST,
            "/api/projects/{slug}/hypotheses/{number}/comments",
            &comments,
            None,
            Some(&researcher_token),
            Some(json!({"body_markdown":" "})),
            422,
        )
        .await?;

    let catalog = world
        .api(
            Method::GET,
            "/api/projects/{slug}/metrics",
            &format!("{base}/metrics"),
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    assert_eq!(
        catalog,
        world
            .tool(&viewer_token, "metric_catalog", json!({"project":run}))
            .await?
    );
    let measurements = world
        .api(
            Method::GET,
            "/api/projects/{slug}/metrics/query",
            &format!("{base}/metrics/query?metric=ndcg_at_10"),
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    assert_eq!(
        measurements,
        world
            .tool(
                &viewer_token,
                "query_metrics",
                json!({"project":run,"metric":"ndcg_at_10"})
            )
            .await?
    );
    let comparisons = world
        .api(
            Method::GET,
            "/api/projects/{slug}/comparisons",
            &format!("{base}/comparisons"),
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    assert_eq!(
        comparisons,
        world
            .tool(&viewer_token, "query_comparisons", json!({"project":run}))
            .await?
    );
    let reports = world
        .api(
            Method::GET,
            "/api/projects/{slug}/reports",
            &format!("{base}/reports"),
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    assert_eq!(
        reports,
        world
            .tool(&viewer_token, "list_reports", json!({"project":run}))
            .await?
    );
    world
        .api(
            Method::GET,
            "/api/projects/{slug}/dashboard",
            &format!("{base}/dashboard"),
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    world
        .api(
            Method::GET,
            "/api/projects/{slug}/attention",
            &format!("{base}/attention"),
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    let searched = world
        .api(
            Method::GET,
            "/api/search",
            &format!("/api/search?q=Conformance&project={run}"),
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    assert_eq!(
        searched,
        world
            .tool(
                &viewer_token,
                "search",
                json!({"q":"Conformance","project":[run]})
            )
            .await?
    );
    assert!(
        searched["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "search must find the drafts created by this scenario"
    );
    let cases = world
        .api(
            Method::GET,
            "/api/projects/{slug}/review-cases",
            &format!("{base}/review-cases"),
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    assert_eq!(
        cases,
        world
            .tool(&viewer_token, "list_review_cases", json!({"project":run}))
            .await?
    );
    let case_id = cases["items"]
        .as_array()
        .and_then(|items| items.first())
        .and_then(|case| case["id"].as_str())
        .ok_or("draft reviews must leave cases")?;
    let case = world
        .api(
            Method::GET,
            "/api/projects/{slug}/review-cases/{case_id}",
            &format!("{base}/review-cases/{case_id}"),
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    assert_eq!(
        case,
        world
            .tool(
                &viewer_token,
                "get_review_case",
                json!({"project":run,"case_id":case_id})
            )
            .await?
    );
    let dashboard = world
        .api(
            Method::GET,
            "/api/projects/{slug}/dashboard",
            &format!("{base}/dashboard"),
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    let view_id = dashboard["views"]
        .as_array()
        .and_then(|views| views.first())
        .and_then(|view| view["id"].as_str())
        .ok_or("science must derive dashboard views")?;
    world
        .api(
            Method::GET,
            "/api/projects/{slug}/dashboard/views/{view_id}",
            &format!("{base}/dashboard/views/{view_id}"),
            None,
            Some(&viewer_token),
            None,
            200,
        )
        .await?;
    for (template, suffix) in [
        ("/api/projects/{slug}/tracks", "tracks"),
        ("/api/projects/{slug}/hypotheses", "hypotheses"),
        ("/api/projects/{slug}/producers", "producers"),
        ("/api/projects/{slug}/reports", "reports"),
        ("/api/projects/{slug}/review-cases", "review-cases"),
        ("/api/projects/{slug}/comparisons", "comparisons"),
    ] {
        let path = format!("{base}/{suffix}");
        let denied = world
            .api(Method::GET, template, &path, None, None, None, 401)
            .await?;
        assert_code(&denied, "unauthenticated");
        let hidden = world
            .api(
                Method::GET,
                template,
                &path,
                None,
                Some(&outsider_token),
                None,
                404,
            )
            .await?;
        assert_code(&hidden, "not_found");
        let invalid = world
            .api(
                Method::GET,
                template,
                &format!("{path}?limit=0"),
                None,
                Some(&viewer_token),
                None,
                422,
            )
            .await?;
        assert_code(&invalid, "validation_failed");
        assert_violation_path(&invalid, "query/limit");
    }
    for (template, path) in [
        (
            "/api/projects/{slug}/tracks/{track_slug}",
            format!("{base}/tracks/missing"),
        ),
        (
            "/api/projects/{slug}/hypotheses/{number}",
            format!("{base}/hypotheses/2147483647"),
        ),
        (
            "/api/projects/{slug}/config/{kind}/{revision}",
            format!("{base}/config/science/2147483647"),
        ),
        (
            "/api/projects/{slug}/producers/{name}/{revision}",
            format!("{base}/producers/missing/1"),
        ),
        (
            "/api/projects/{slug}/dashboard/views/{view_id}",
            format!("{base}/dashboard/views/missing"),
        ),
    ] {
        assert_code(
            &world
                .api(
                    Method::GET,
                    template,
                    &path,
                    None,
                    Some(&viewer_token),
                    None,
                    404,
                )
                .await?,
            "not_found",
        );
    }
    world
        .api(
            Method::POST,
            "/api/projects/{slug}/service-accounts/{name}/disable",
            &format!("{sa}/codex/disable"),
            None,
            Some(&admin_token),
            Some(json!({"reason":"Verify revocation"})),
            200,
        )
        .await?;
    world
        .api(
            Method::GET,
            "/api/me",
            "/api/me",
            None,
            Some(&agent_token),
            None,
            401,
        )
        .await?;
    let revoke = format!("/api/tokens/{revoked_id}");
    world
        .api(
            Method::DELETE,
            "/api/tokens/{token_id}",
            &revoke,
            Some(&researcher),
            None,
            None,
            200,
        )
        .await?;
    world
        .api(
            Method::DELETE,
            "/api/tokens/{token_id}",
            &revoke,
            Some(&researcher),
            None,
            None,
            200,
        )
        .await?;
    world
        .api(
            Method::GET,
            "/api/me",
            "/api/me",
            None,
            Some(&readonly_token),
            None,
            401,
        )
        .await?;
    world
        .api(
            Method::DELETE,
            "/api/projects/{slug}/members/{user_id}",
            &format!("{base}/members/{}", viewer.id),
            None,
            Some(&admin_token),
            None,
            204,
        )
        .await?;
    world
        .api(
            Method::GET,
            "/api/projects/{slug}",
            &base,
            None,
            Some(&viewer_token),
            None,
            404,
        )
        .await?;
    world
        .api(
            Method::POST,
            "/auth/logout",
            "/auth/logout",
            Some(&outsider),
            None,
            None,
            200,
        )
        .await?;
    world
        .api(
            Method::GET,
            "/api/me",
            "/api/me",
            Some(&outsider),
            None,
            None,
            401,
        )
        .await?;
    let audit = world.harness.fetch_audit(&admin_token, 0).await?;
    let events = audit.body["items"]
        .as_array()
        .ok_or("audit omitted items")?;
    for action in [
        "user.created",
        "session.created",
        "session.ended",
        "token.created",
        "token.revoked",
        "project.created",
        "membership.set",
        "membership.removed",
        "service_account.created",
        "service_account.disabled",
        "config.science_created",
        "producer.registered",
        "track.created",
        "track.updated",
        "track.state_changed",
        "hypothesis.draft_created",
        "hypothesis.draft_revised",
        "hypothesis.draft_approved",
        "hypothesis.revision_requested",
        "hypothesis.draft_declined",
        "comment.created",
        "comment.edited",
    ] {
        assert!(
            events.iter().any(|event| event["action"] == action),
            "missing produced audit action {action}"
        );
    }
    assert!(
        events
            .iter()
            .any(|event| event["action"] == "comment.created"
                && event["actor_kind"] == "user"
                && event["via_channel"] == "mcp"),
        "MCP must retain human actor and MCP channel"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["action"] == "token.revoked" && event["subject_id"] == revoked_id)
            .count(),
        1,
        "revoking twice must append one event"
    );
    world.export()?;
    Ok(())
}
