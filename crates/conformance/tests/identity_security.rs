#![forbid(unsafe_code)]
#[path = "support/lifecycle_support.rs"]
#[allow(
    dead_code,
    reason = "Bootstrap fixture helpers are shared by integration binaries"
)]
mod lifecycle;
#[path = "support/identity_security_support.rs"]
mod support;
use conformance::Result;
use reqwest::Method;
use serde_json::{Value, json};
use support::{Context, Session, claims, opaque, string};

async fn admin(ctx: &mut Context) -> Result<Session> {
    ctx.login(
        claims("conformance-admin", "admin@conformance.test", true),
        "/",
    )
    .await
}

#[tokio::test]
#[ignore = "requires R3 research routes; keep readonly service scope enforcement separate from R2"]
async fn readonly_service_token_refuses_hypothesis_creation() -> Result<()> {
    let (world, actors) = lifecycle::World::new("identity-readonly-research").await?;
    let mut ctx = Context::new()?;
    let session = admin(&mut ctx).await?;
    let token = ctx
        .browser_api(
            Method::POST,
            "/api/projects/{slug}/service-accounts/{name}/tokens",
            &format!("{}/service-accounts/agent/tokens", world.base()),
            &session,
            Some(json!({"name":"readonly-research","expires_in_days":1,"scopes":["read"]})),
            201,
        )
        .await?;
    let secret = string(&token["token"])?;
    ctx.api(
        Method::GET,
        "/api/projects/{slug}",
        &world.base(),
        &secret,
        None,
        200,
    )
    .await?;
    let hypothesis: Value =
        serde_json::from_str(include_str!("../../../examples/fixture/hypothesis.json"))?;
    let error = ctx
        .api(
            Method::POST,
            "/api/projects/{slug}/hypotheses",
            &format!("{}/hypotheses", world.base()),
            &secret,
            Some(hypothesis),
            403,
        )
        .await?;
    assert_eq!(error["error"]["code"], "forbidden");
    ctx.finish(&actors.admin, "readonly-research").await
}
fn listed(body: &Value, id: &Value) -> Result<Value> {
    body["items"]
        .as_array()
        .ok_or("listed items absent")?
        .iter()
        .find(|item| item["id"] == *id)
        .cloned()
        .ok_or_else(|| "expected owned token absent".into())
}

#[tokio::test]
#[ignore = "requires the ordinary unrestricted-email conformance profile"]
#[allow(
    clippy::too_many_lines,
    reason = "Token secrecy, scope, bearer precedence and revocation share real minted identities"
)]
async fn personal_tokens_encoding_csrf_scope_precedence_and_revocation() -> Result<()> {
    let mut ctx = Context::new()?;
    let (session, actors) = ctx.admin_actor("identity-pat").await?;
    let readonly = ctx
        .mint(&session, "security-readonly", &["read", "read"])
        .await?;
    let secret = string(&readonly["token"])?;
    opaque(&secret, "cr_pat_")?;
    assert!(
        readonly["display_prefix"] == secret[..12],
        "display prefix differs"
    );
    assert_eq!(readonly["scopes"], json!(["read"]));
    assert!(string(&readonly["expires_at"])? > string(&readonly["created_at"])?);
    let me = ctx
        .api(Method::GET, "/api/me", "/api/me", &secret, None, 200)
        .await?;
    assert_eq!(me["channel"], "api");
    assert_eq!(me["csrf_token"], Value::Null);
    ctx.api(
        Method::GET,
        "/api/projects",
        "/api/projects",
        &secret,
        None,
        200,
    )
    .await?;
    let create = json!({"slug":"forbidden-readonly-project","title":"Forbidden"});
    ctx.api(
        Method::POST,
        "/api/projects",
        "/api/projects",
        &secret,
        Some(create.clone()),
        403,
    )
    .await?;
    let request = ctx
        .h
        .request(Method::POST, "/api/projects")?
        .bearer_auth(&secret)
        .header("cookie", &session.cookie)
        .header("X-CSRF-Token", "incorrect-but-bearer-ignored")
        .json(&create);
    let refused = ctx
        .checked(Method::POST, "/api/projects", request, 403)
        .await?;
    assert_eq!(refused["error"]["code"], "forbidden");
    let request = ctx.h.request(Method::POST, "/api/projects")?
        .bearer_auth(&actors.admin).header("cookie", &session.cookie)
        .header("X-CSRF-Token", "incorrect-but-bearer-ignored")
        .json(&json!({"slug":format!("csrf-bearer-{}",string(&readonly["id"])?),"title":"Bearer writes ignore browser CSRF"}));
    ctx.checked(Method::POST, "/api/projects", request, 201)
        .await?;
    let token_body = json!({"name":"csrf-probe","expires_in_days":1,"scopes":["read"]});
    for csrf in [None, Some("wrong")] {
        let mut request = ctx
            .h
            .request(Method::POST, "/api/tokens")?
            .header("cookie", &session.cookie)
            .json(&token_body);
        if let Some(csrf) = csrf {
            request = request.header("X-CSRF-Token", csrf);
        }
        let error = ctx
            .checked(Method::POST, "/api/tokens", request, 403)
            .await?;
        assert_eq!(error["error"]["code"], "csrf_invalid");
    }
    ctx.api(
        Method::POST,
        "/api/tokens",
        "/api/tokens",
        &actors.admin,
        Some(token_body),
        403,
    )
    .await?;
    for days in [0, 10_000] {
        ctx.browser_api(
            Method::POST,
            "/api/tokens",
            "/api/tokens",
            &session,
            Some(json!({"name":"expiry-bounds","expires_in_days":days,"scopes":["read"]})),
            422,
        )
        .await?;
    }
    for authorization in ["Bearer cr_pat_unknown", "Basic abc", "Bearer ", ""] {
        let request = ctx
            .h
            .request(Method::GET, "/api/me")?
            .header("cookie", &session.cookie)
            .header("Authorization", authorization);
        let error = ctx.checked(Method::GET, "/api/me", request, 401).await?;
        assert_eq!(error["error"]["code"], "unauthenticated");
    }
    let request = ctx
        .h
        .request(Method::GET, "/api/me")?
        .header("Authorization", format!("bEaReR   {secret}  "));
    ctx.checked(Method::GET, "/api/me", request, 200).await?;
    for mutated in [
        secret.replacen("cr_pat_", "cr_svc_", 1),
        secret[7..].to_owned(),
        format!("{secret}x"),
    ] {
        ctx.api(Method::GET, "/api/me", "/api/me", &mutated, None, 401)
            .await?;
    }
    let tokens = ctx
        .browser_api(
            Method::GET,
            "/api/tokens",
            "/api/tokens",
            &session,
            None,
            200,
        )
        .await?;
    let record = listed(&tokens, &readonly["id"])?;
    assert!(record.get("token").is_none());
    assert!(!record["last_used_at"].is_null());
    let path = format!("/api/tokens/{}", string(&readonly["id"])?);
    let first = ctx
        .api(
            Method::DELETE,
            "/api/tokens/{token_id}",
            &path,
            &actors.admin,
            None,
            200,
        )
        .await?;
    let second = ctx
        .api(
            Method::DELETE,
            "/api/tokens/{token_id}",
            &path,
            &actors.admin,
            None,
            200,
        )
        .await?;
    assert_eq!(first["revoked_at"], second["revoked_at"]);
    ctx.api(Method::GET, "/api/me", "/api/me", &secret, None, 401)
        .await?;
    let audit = ctx.h.fetch_audit(&actors.admin, 0).await?;
    assert_eq!(audit.body["items"].as_array().ok_or("audit items absent")?.iter().filter(|item|item["action"]=="token.revoked"&&item["subject_id"]==readonly["id"]).count(),1);
    ctx.finish(&actors.admin, "personal").await
}

#[tokio::test]
#[ignore = "requires the ordinary unrestricted-email conformance profile"]
#[allow(
    clippy::too_many_lines,
    reason = "Service tokens must remain live until their distinct revocation and disable probes"
)]
async fn service_token_scope_minting_revocation_and_disable_idempotency() -> Result<()> {
    let mut ctx = Context::new()?;
    let (world, actors) = ctx.project_fixture("service").await?;
    let session = admin(&mut ctx).await?;
    let base = world.base();
    ctx.api(
        Method::POST,
        "/api/projects/{slug}/service-accounts",
        &format!("{base}/service-accounts"),
        &actors.admin,
        Some(json!({"kind":"agent","name":"security-agent"})),
        201,
    )
    .await?;
    let path = format!("{base}/service-accounts/security-agent/tokens");
    let template = "/api/projects/{slug}/service-accounts/{name}/tokens";
    let body = json!({"name":"readonly","expires_in_days":1,"scopes":["read"]});
    for token in [&actors.admin, &actors.agent] {
        ctx.api(
            Method::POST,
            template,
            &path,
            token,
            Some(body.clone()),
            403,
        )
        .await?;
    }
    let created = ctx
        .browser_api(
            Method::POST,
            template,
            &path,
            &session,
            Some(body.clone()),
            201,
        )
        .await?;
    let secret = string(&created["token"])?;
    opaque(&secret, "cr_svc_")?;
    let me = ctx
        .api(Method::GET, "/api/me", "/api/me", &secret, None, 200)
        .await?;
    assert_eq!(me["kind"], "service");
    assert_eq!(me["service_account"]["name"], "security-agent");
    assert_eq!(me["channel"], "api");
    assert_eq!(me["csrf_token"], Value::Null);
    ctx.api(
        Method::GET,
        "/api/projects/{slug}",
        &base,
        &secret,
        None,
        200,
    )
    .await?;
    ctx.api(
        Method::POST,
        "/api/tokens",
        "/api/tokens",
        &secret,
        Some(body.clone()),
        403,
    )
    .await?;
    for days in [0, 10_000] {
        ctx.browser_api(
            Method::POST,
            template,
            &path,
            &session,
            Some(json!({"name":"bounds","expires_in_days":days,"scopes":["read"]})),
            422,
        )
        .await?;
    }
    let list = ctx
        .api(Method::GET, template, &path, &actors.admin, None, 200)
        .await?;
    let metadata = listed(&list, &created["id"])?;
    assert!(metadata.get("token").is_none());
    assert!(metadata["expires_at"].as_str() > metadata["created_at"].as_str());
    let victim = ctx
        .browser_api(
            Method::POST,
            template,
            &path,
            &session,
            Some(json!({"name":"revocation-victim","expires_in_days":1,"scopes":["read","write"]})),
            201,
        )
        .await?;
    let revoked_path = format!("/api/tokens/{}", string(&victim["id"])?);
    ctx.api(
        Method::DELETE,
        "/api/tokens/{token_id}",
        &revoked_path,
        &actors.admin,
        None,
        200,
    )
    .await?;
    ctx.api(
        Method::GET,
        "/api/me",
        "/api/me",
        &string(&victim["token"])?,
        None,
        401,
    )
    .await?;
    ctx.api(Method::GET, "/api/me", "/api/me", &secret, None, 200)
        .await?;
    let disable = format!("{base}/service-accounts/security-agent/disable");
    let first = ctx
        .api(
            Method::POST,
            "/api/projects/{slug}/service-accounts/{name}/disable",
            &disable,
            &actors.admin,
            Some(json!({"reason":"Security disable test"})),
            200,
        )
        .await?;
    let second = ctx
        .api(
            Method::POST,
            "/api/projects/{slug}/service-accounts/{name}/disable",
            &disable,
            &actors.admin,
            Some(json!({"reason":"Repeat disable"})),
            200,
        )
        .await?;
    assert_eq!(first["disabled_at"], second["disabled_at"]);
    ctx.api(Method::GET, "/api/me", "/api/me", &secret, None, 401)
        .await?;
    ctx.browser_api(Method::POST, template, &path, &session, Some(body), 409)
        .await?;
    let audit = ctx.h.fetch_audit(&actors.admin, 0).await?;
    assert_eq!(
        audit.body["items"]
            .as_array()
            .ok_or("audit items absent")?
            .iter()
            .filter(|item| item["action"] == "service_account.disabled"
                && item["subject_id"] == first["id"])
            .count(),
        1
    );
    ctx.finish(&actors.admin, "service").await
}

#[tokio::test]
#[ignore = "requires the ordinary unrestricted-email conformance profile"]
#[allow(
    clippy::too_many_lines,
    reason = "Membership visibility and foreign cursors require independent human and service identities"
)]
async fn memberships_cursor_privacy_and_impersonation_headers() -> Result<()> {
    let mut ctx = Context::new()?;
    let (world, actors) = ctx.project_fixture("sharing").await?;
    let primary = admin(&mut ctx).await?;
    let alice = ctx
        .login(
            claims("security-alice", "security-alice@conformance.test", true),
            "/",
        )
        .await?;
    let bob = ctx
        .login(
            claims("security-bob", "security-bob@conformance.test", true),
            "/",
        )
        .await?;
    let alice_token = ctx.mint(&alice, "alice-own", &["read", "write"]).await?;
    let bob_token = ctx.mint(&bob, "bob-own", &["read", "write"]).await?;
    let alice_secret = string(&alice_token["token"])?;
    let bob_secret = string(&bob_token["token"])?;
    let base = world.base();
    let second = format!("{}-b", world.project);
    let hidden = format!("{}-hidden", world.project);
    for slug in [&second, &hidden] {
        ctx.api(
            Method::POST,
            "/api/projects",
            "/api/projects",
            &actors.admin,
            Some(json!({"slug":slug,"title":"Visibility fixture"})),
            201,
        )
        .await?;
    }
    ctx.api(
        Method::GET,
        "/api/projects/{slug}",
        &base,
        &alice_secret,
        None,
        404,
    )
    .await?;
    let member_template = "/api/projects/{slug}/members/{user_id}";
    let alice_member = format!("{base}/members/{}", string(&alice.me["user"]["id"])?);
    ctx.api(
        Method::PUT,
        member_template,
        &alice_member,
        &actors.admin,
        Some(json!({"role":"researcher"})),
        200,
    )
    .await?;
    let own = ctx
        .api(
            Method::GET,
            "/api/projects",
            "/api/projects",
            &alice_secret,
            None,
            200,
        )
        .await?;
    assert_eq!(own["items"].as_array().ok_or("projects absent")?.len(), 1);
    assert_eq!(own["items"][0]["slug"], world.project);
    for path in [
        format!("/api/projects/{second}"),
        format!("/api/projects/{second}/members"),
        format!("/api/projects/{hidden}"),
    ] {
        let template = if path.ends_with("/members") {
            "/api/projects/{slug}/members"
        } else {
            "/api/projects/{slug}"
        };
        ctx.api(Method::GET, template, &path, &alice_secret, None, 404)
            .await?;
    }
    ctx.api(
        Method::GET,
        "/api/projects/{slug}",
        &format!("/api/projects/{second}"),
        &actors.agent,
        None,
        404,
    )
    .await?;
    let own_service = ctx
        .api(
            Method::GET,
            "/api/projects",
            "/api/projects",
            &actors.agent,
            None,
            200,
        )
        .await?;
    assert_eq!(
        own_service["items"]
            .as_array()
            .ok_or("service projects absent")?
            .len(),
        1
    );
    assert_eq!(own_service["items"][0]["slug"], world.project);
    let no_more = ctx
        .api(
            Method::GET,
            "/api/projects",
            &format!("/api/projects?before={}", world.project),
            &actors.agent,
            None,
            200,
        )
        .await?;
    assert_eq!(no_more["items"], json!([]));
    assert_eq!(no_more["next_before"], Value::Null);
    ctx.api(
        Method::GET,
        "/api/projects/{slug}/members",
        &format!("{base}/members"),
        &actors.agent,
        None,
        403,
    )
    .await?;
    ctx.api(
        Method::PUT,
        member_template,
        &format!(
            "/api/projects/{second}/members/{}",
            string(&alice.me["user"]["id"])?
        ),
        &actors.admin,
        Some(json!({"role":"viewer"})),
        200,
    )
    .await?;
    ctx.api(
        Method::PUT,
        member_template,
        &format!("{base}/members/{}", string(&bob.me["user"]["id"])?),
        &actors.admin,
        Some(json!({"role":"viewer"})),
        200,
    )
    .await?;
    let shared = ctx
        .api(
            Method::GET,
            "/api/projects",
            "/api/projects?limit=1",
            &alice_secret,
            None,
            200,
        )
        .await?;
    let cursor = string(&shared["next_before"])?;
    let next = ctx
        .api(
            Method::GET,
            "/api/projects",
            &format!("/api/projects?before={cursor}&limit=1"),
            &alice_secret,
            None,
            200,
        )
        .await?;
    assert_eq!(
        next["items"]
            .as_array()
            .ok_or("continued projects absent")?
            .len(),
        1
    );
    assert_ne!(shared["items"][0]["slug"], next["items"][0]["slug"]);
    for page in [&shared, &next] {
        assert!(
            page["items"]
                .as_array()
                .ok_or("projects absent")?
                .iter()
                .all(|item| item["slug"] != hidden)
        );
    }
    let members = ctx
        .api(
            Method::GET,
            "/api/projects/{slug}/members",
            &format!("{base}/members"),
            &bob_secret,
            None,
            200,
        )
        .await?;
    assert!(
        members["items"]
            .as_array()
            .ok_or("members absent")?
            .iter()
            .any(|member| member["user_id"] == alice.me["user"]["id"])
    );
    let second_owned = ctx.mint(&alice, "alice-second", &["read"]).await?;
    for foreign in [
        string(&bob_token["id"])?,
        "00000000-0000-0000-0000-000000000000".into(),
    ] {
        let filtered = ctx
            .api(
                Method::GET,
                "/api/tokens",
                &format!("/api/tokens?before={foreign}"),
                &alice_secret,
                None,
                200,
            )
            .await?;
        assert_eq!(filtered["items"], json!([]));
        assert_eq!(filtered["next_before"], Value::Null);
    }
    let previous = ctx
        .api(
            Method::GET,
            "/api/tokens",
            &format!("/api/tokens?before={}", string(&second_owned["id"])?),
            &alice_secret,
            None,
            200,
        )
        .await?;
    assert_eq!(previous["items"][0]["id"], alice_token["id"]);
    ctx.api(
        Method::DELETE,
        "/api/tokens/{token_id}",
        &format!("/api/tokens/{}", string(&bob_token["id"])?),
        &alice_secret,
        None,
        404,
    )
    .await?;
    let request = ctx
        .h
        .request(Method::GET, "/api/me")?
        .bearer_auth(&alice_secret)
        .header("cookie", &primary.cookie)
        .header("X-User-ID", string(&primary.me["user"]["id"])?)
        .header("X-Forwarded-User", "admin@conformance.test")
        .header("X-Role", "admin");
    let identity = ctx.checked(Method::GET, "/api/me", request, 200).await?;
    assert_eq!(identity["user"]["id"], alice.me["user"]["id"]);
    assert_eq!(identity["user"]["is_admin"], false);
    assert_eq!(identity["channel"], "api");
    let request = ctx
        .h
        .request(Method::GET, "/api/me")?
        .header("X-User-ID", string(&primary.me["user"]["id"])?)
        .header("X-Forwarded-User", "admin@conformance.test")
        .header("X-Role", "admin");
    ctx.checked(Method::GET, "/api/me", request, 401).await?;
    ctx.api(
        Method::GET,
        "/api/users",
        "/api/users?email=admin%40conformance.test",
        &alice_secret,
        None,
        403,
    )
    .await?;
    ctx.api(
        Method::DELETE,
        member_template,
        &format!(
            "/api/projects/{second}/members/{}",
            string(&alice.me["user"]["id"])?
        ),
        &actors.admin,
        None,
        204,
    )
    .await?;
    ctx.api(
        Method::GET,
        "/api/projects/{slug}",
        &format!("/api/projects/{second}"),
        &alice_secret,
        None,
        404,
    )
    .await?;
    ctx.finish(&actors.admin, "visibility").await
}

#[tokio::test]
#[ignore = "requires the ordinary unrestricted-email conformance profile"]
#[allow(
    clippy::too_many_lines,
    reason = "Each adversarial OIDC request uses a fresh browser-bound authorization code"
)]
async fn oidc_browser_binding_return_to_and_claim_verification() -> Result<()> {
    let mut ctx = Context::new()?;
    let (_, actors) = ctx.admin_actor("identity-oidc").await?;
    let start = ctx
        .begin(
            claims("oidc-binding", "binding@conformance.test", true),
            "/hypotheses?x=1",
            json!({}),
        )
        .await?;
    for binding in [None, Some("cr_login=someone-else")] {
        let denied = ctx.callback(&start, binding).await?;
        assert_eq!(denied.status().as_u16(), 400);
        let error: Value = denied.json().await?;
        assert_eq!(error["error"]["code"], "login_failed");
    }
    // take_login_request consumes a state even when a supplied browser binding
    // mismatches; the leaked callback cannot later be used by its owner either.
    assert_eq!(
        ctx.callback(&start, Some(&start.binding))
            .await?
            .status()
            .as_u16(),
        400
    );
    let start = ctx
        .begin(
            claims("oidc-binding", "binding@conformance.test", true),
            "/hypotheses?x=1",
            json!({}),
        )
        .await?;
    let done = ctx.callback(&start, Some(&start.binding)).await?;
    assert_eq!(done.status().as_u16(), 302);
    assert_eq!(done.headers()["location"], "/hypotheses?x=1");
    ctx.session(done).await?;
    let replay = ctx.callback(&start, Some(&start.binding)).await?;
    assert_eq!(replay.status().as_u16(), 400);
    let forged = support::Started {
        binding: start.binding,
        state: "invented-login-state".into(),
        code: "invented-code".into(),
    };
    assert_eq!(
        ctx.callback(&forged, Some(&forged.binding))
            .await?
            .status()
            .as_u16(),
        400
    );
    for (index, return_to) in [
        "//evil.example/steal",
        "/\t/evil.example",
        "/\n/evil.example",
        "/\\evil.example",
        "/x\u{7f}",
        "https://evil.example/",
        "evil.example",
    ]
    .iter()
    .enumerate()
    {
        let start = ctx
            .begin(
                claims(
                    &format!("return-to-{index}"),
                    "redirect@conformance.test",
                    true,
                ),
                return_to,
                json!({}),
            )
            .await?;
        let done = ctx.callback(&start, Some(&start.binding)).await?;
        assert_eq!(done.status().as_u16(), 302);
        assert_eq!(done.headers()["location"], "/");
        ctx.session(done).await?;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    for (extra, options) in [
        (json!({"iss":"https://impostor.invalid/oidc"}), json!({})),
        (json!({"aud":"another-client"}), json!({})),
        (json!({"exp":0}), json!({})),
        (json!({"iat":now+3600}), json!({})),
        (json!({"sub":null}), json!({})),
        (json!({}), json!({"nonce_override":"attacker-nonce"})),
        (json!({}), json!({"bad_signature":true})),
    ] {
        let mut identity = claims("invalid-oidc-claim", "invalid@conformance.test", true);
        identity
            .as_object_mut()
            .ok_or("claims object absent")?
            .extend(extra.as_object().ok_or("claim overrides absent")?.clone());
        let start = ctx.begin(identity, "/", options).await?;
        let denied = ctx.callback(&start, Some(&start.binding)).await?;
        assert_eq!(denied.status().as_u16(), 400);
        let error: Value = denied.json().await?;
        assert_eq!(error["error"]["code"], "login_failed");
    }
    let unverified = ctx
        .login(
            claims("unverified-admin-claim", "admin@conformance.test", false),
            "/",
        )
        .await?;
    assert_eq!(unverified.me["user"]["is_admin"], false);
    assert_eq!(unverified.me["user"]["email_verified"], false);
    let first = ctx
        .login(
            claims("user-key-verified", "CaseLookup@Conformance.Test", true),
            "/",
        )
        .await?;
    let impostor = ctx
        .login(
            claims("user-key-unverified", "caselookup@conformance.test", false),
            "/",
        )
        .await?;
    assert_ne!(first.me["user"]["id"], impostor.me["user"]["id"]);
    assert_eq!(first.me["user"]["email"], "caselookup@conformance.test");
    let verified_only = ctx
        .api(
            Method::GET,
            "/api/users",
            "/api/users?email=CASELOOKUP%40CONFORMANCE.TEST",
            &actors.admin,
            None,
            200,
        )
        .await?;
    assert_eq!(
        verified_only["items"]
            .as_array()
            .ok_or("users absent")?
            .len(),
        1
    );
    assert_eq!(verified_only["items"][0]["id"], first.me["user"]["id"]);
    let again = ctx
        .login(
            claims("user-key-verified", "changed@conformance.test", true),
            "/",
        )
        .await?;
    assert_eq!(again.me["user"]["id"], first.me["user"]["id"]);
    let lookup = ctx
        .api(
            Method::GET,
            "/api/users",
            "/api/users?email=CASELOOKUP%40CONFORMANCE.TEST",
            &actors.admin,
            None,
            200,
        )
        .await?;
    assert_eq!(lookup["items"], json!([]));
    let lookup = ctx
        .api(
            Method::GET,
            "/api/users",
            "/api/users?email=CHANGED%40CONFORMANCE.TEST",
            &actors.admin,
            None,
            200,
        )
        .await?;
    assert!(
        lookup["items"]
            .as_array()
            .ok_or("users absent")?
            .iter()
            .any(|item| item["id"] == first.me["user"]["id"])
    );
    ctx.finish(&actors.admin, "oidc").await
}

#[tokio::test]
#[ignore = "requires explicit allowed-email-domain conformance.test profile"]
async fn restricted_domains_require_exact_domain_and_boolean_verified_claim() -> Result<()> {
    if std::env::var("CANNERY_CONFORMANCE_ALLOWED_EMAIL_DOMAIN")?.as_str() != "conformance.test" {
        return Err("identity domain profile must allow conformance.test".into());
    }
    let mut ctx = Context::new()?;
    for identity in [
        claims("domain-outside", "outside@other.test", true),
        claims("domain-unverified", "inside@conformance.test", false),
        json!({"sub":"domain-string-verified","email":"inside@conformance.test","email_verified":"true"}),
        claims("domain-suffix", "outside@evilconformance.test", true),
    ] {
        let start = ctx.begin(identity, "/", json!({})).await?;
        let denied = ctx.callback(&start, Some(&start.binding)).await?;
        assert_eq!(denied.status().as_u16(), 403);
    }
    let session = admin(&mut ctx).await?;
    let token = ctx
        .mint(&session, "domain-profile-admin", &["read", "write"])
        .await?;
    ctx.finish(&string(&token["token"])?, "domain-profile")
        .await
}

#[tokio::test]
#[ignore = "requires explicit OIDC JWKS decoys profile"]
async fn oidc_jwks_decoys_accept_real_key_and_refuse_forged_signature() -> Result<()> {
    if std::env::var("CANNERY_CONFORMANCE_OIDC_JWKS_MODE")?.as_str() != "decoys" {
        return Err("JWKS decoys profile required".into());
    }
    let mut ctx = Context::new()?;
    let session = admin(&mut ctx).await?;
    let start = ctx
        .begin(
            claims("decoy-forgery", "forgery@conformance.test", true),
            "/",
            json!({"bad_signature":true}),
        )
        .await?;
    assert_eq!(
        ctx.callback(&start, Some(&start.binding))
            .await?
            .status()
            .as_u16(),
        400
    );
    assert!(
        ctx.stats().await?["jwks_requests"]
            .as_u64()
            .ok_or("JWKS request count absent")?
            >= 1
    );
    let token = ctx
        .mint(&session, "jwks-decoys-admin", &["read", "write"])
        .await?;
    ctx.finish(&string(&token["token"])?, "jwks-decoys").await
}

#[tokio::test]
#[ignore = "requires explicit unusable JWKS profile and a fresh relying party"]
async fn oidc_unusable_jwks_refreshes_once_and_refuses_login() -> Result<()> {
    if std::env::var("CANNERY_CONFORMANCE_OIDC_JWKS_MODE")?.as_str() != "unusable" {
        return Err("unusable JWKS profile required".into());
    }
    let ctx = Context::new()?;
    let before = ctx.stats().await?["jwks_requests"]
        .as_u64()
        .ok_or("JWKS request count absent")?;
    let start = ctx
        .begin(
            claims("no-signing-key", "no-key@conformance.test", true),
            "/",
            json!({}),
        )
        .await?;
    let denied = ctx.callback(&start, Some(&start.binding)).await?;
    assert_eq!(denied.status().as_u16(), 400);
    let error: Value = denied.json().await?;
    assert_eq!(error["error"]["code"], "login_failed");
    let after = ctx.stats().await?["jwks_requests"]
        .as_u64()
        .ok_or("JWKS request count absent")?;
    assert_eq!(after - before, 2);
    Ok(())
}

#[tokio::test]
#[ignore = "requires an explicitly selected OIDC logout endpoint profile"]
async fn provider_logout_endpoint_safety_and_session_isolation() -> Result<()> {
    let mode = std::env::var("CANNERY_CONFORMANCE_OIDC_LOGOUT_MODE")?;
    let mut ctx = Context::new()?;
    let primary = admin(&mut ctx).await?;
    let control = ctx
        .mint(&primary, "logout-profile-admin", &["read", "write"])
        .await?;
    let first = ctx
        .login(
            claims("logout-isolated-user", "logout@conformance.test", true),
            "/",
        )
        .await?;
    let second = ctx
        .login(
            claims("logout-isolated-user", "logout@conformance.test", true),
            "/",
        )
        .await?;
    let out = ctx
        .browser_api(
            Method::POST,
            "/auth/logout",
            "/auth/logout",
            &first,
            Some(json!({})),
            200,
        )
        .await?;
    match mode.as_str() {
        "default" => assert!(
            string(&out["logout_url"])?.starts_with(&format!("{}/oidc/session/end?", ctx.oidc))
        ),
        "localhost-http" => assert!(
            string(&out["logout_url"])?.starts_with("http://localhost:3001/oidc/session/end?")
        ),
        "remote-https" => {
            assert!(string(&out["logout_url"])?.starts_with("https://identity.example/logout?"));
        }
        "none" | "remote-http" | "javascript" | "hostless-https" => {
            assert_eq!(out["logout_url"], Value::Null);
        }
        _ => return Err("unknown expected OIDC logout mode".into()),
    }
    ctx.browser_api(Method::GET, "/api/me", "/api/me", &first, None, 401)
        .await?;
    ctx.browser_api(Method::GET, "/api/me", "/api/me", &second, None, 200)
        .await?;
    ctx.api(
        Method::GET,
        "/api/me",
        "/api/me",
        &string(&control["token"])?,
        None,
        200,
    )
    .await?;
    ctx.finish(&string(&control["token"])?, &format!("logout-{mode}"))
        .await
}
