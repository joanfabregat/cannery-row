//! Identity/project parity runs independently of R3 research routes and R4 MCP.
#![forbid(unsafe_code)]
#[path = "support/identity_security_support.rs"]
#[allow(
    dead_code,
    reason = "Reuse actual OIDC/browser helpers without research fixtures"
)]
mod identity;
#[path = "support/r2_identity_projects_support.rs"]
mod support;

use conformance::Result;
use identity::{opaque, string};
use reqwest::Method;
use serde_json::{Value, json};
use support::{World, code, items};

#[tokio::test]
#[ignore = "requires the ordinary unrestricted-email conformance profile; R2 HTTP only"]
async fn identity_projects_memberships_services_and_audit_via_http() -> Result<()> {
    let mut world = World::new().await?;
    world.memberships().await?;
    project_access(&mut world).await?;
    browser_credentials(&mut world).await?;
    let (account, service) = service_accounts(&mut world).await?;
    cleanup(&mut world, &account, &service).await?;
    audit(&mut world, &account).await?;
    world.ctx.finish(&world.admin.token, "r2-projects").await
}

async fn project_access(world: &mut World) -> Result<()> {
    let base = world.base();
    let conflict = world
        .ctx
        .api(
            Method::POST,
            "/api/projects",
            "/api/projects",
            &world.admin.token,
            Some(json!({"slug":world.slug,"title":"Duplicate"})),
            409,
        )
        .await?;
    code(&conflict, "conflict");
    for (path, token) in [
        (base.clone(), &world.outsider.token),
        (
            "/api/projects/no-such-conformance-project".to_owned(),
            &world.admin.token,
        ),
    ] {
        let error = world
            .ctx
            .api(Method::GET, "/api/projects/{slug}", &path, token, None, 404)
            .await?;
        code(&error, "not_found");
    }
    let request = world.ctx.h.request(Method::GET, &base)?;
    let error = world
        .ctx
        .checked(Method::GET, "/api/projects/{slug}", request, 401)
        .await?;
    code(&error, "unauthenticated");
    let visible = world
        .ctx
        .api(
            Method::GET,
            "/api/projects/{slug}",
            &base,
            &world.viewer.token,
            None,
            200,
        )
        .await?;
    assert_eq!(visible["id"], world.project["id"]);
    assert_eq!(visible["role"], "viewer");
    world
        .ctx
        .api(
            Method::GET,
            "/api/projects",
            "/api/projects?limit=0",
            &world.researcher.token,
            None,
            422,
        )
        .await?;
    let projects = world
        .ctx
        .api(
            Method::GET,
            "/api/projects",
            "/api/projects",
            &world.researcher.token,
            None,
            200,
        )
        .await?;
    assert_eq!(items(&projects)?.len(), 1);
    assert_eq!(items(&projects)?[0]["slug"], world.slug);
    user_lookup_and_administration(world).await
}

async fn user_lookup_and_administration(world: &mut World) -> Result<()> {
    let lookup = world
        .ctx
        .api(
            Method::GET,
            "/api/users",
            "/api/users?email=conformance.test",
            &world.admin.token,
            None,
            200,
        )
        .await?;
    // User lookup matches a complete verified email, not a domain substring.
    assert_eq!(items(&lookup)?, &[] as &[Value]);
    for human in [
        &world.admin,
        &world.researcher,
        &world.viewer,
        &world.outsider,
    ] {
        let email = string(&human.session.me["user"]["email"])?;
        let lookup = world
            .ctx
            .api(
                Method::GET,
                "/api/users",
                &format!("/api/users?email={}", email.to_ascii_uppercase()),
                &world.admin.token,
                None,
                200,
            )
            .await?;
        assert_eq!(items(&lookup)?.len(), 1);
        assert_eq!(items(&lookup)?[0]["id"], human.session.me["user"]["id"]);
    }
    world
        .ctx
        .api(
            Method::GET,
            "/api/users",
            "/api/users?email=conformance.test",
            &world.researcher.token,
            None,
            403,
        )
        .await?;
    world
        .ctx
        .api(
            Method::POST,
            "/api/projects",
            "/api/projects",
            &world.researcher.token,
            Some(json!({"slug":"r2-nonadmin-denied","title":"Forbidden"})),
            403,
        )
        .await?;
    Ok(())
}

async fn browser_credentials(world: &mut World) -> Result<()> {
    let token_body = json!({"name":"cannot-mint","expires_in_days":1,"scopes":["read"]});
    let tokens = world
        .ctx
        .browser_api(
            Method::GET,
            "/api/tokens",
            "/api/tokens",
            &world.researcher.session,
            None,
            200,
        )
        .await?;
    assert!(
        items(&tokens)?
            .iter()
            .any(|token| token["id"] == world.readonly["id"])
    );
    assert!(
        items(&tokens)?
            .iter()
            .all(|token| token.get("token").is_none())
    );
    world
        .ctx
        .api(
            Method::POST,
            "/api/tokens",
            "/api/tokens",
            &world.researcher.token,
            Some(token_body.clone()),
            403,
        )
        .await?;
    world
        .ctx
        .browser_api(
            Method::POST,
            "/api/tokens",
            "/api/tokens",
            &world.researcher.session,
            Some(json!({"name":"bad","expires_in_days":0,"scopes":["read"]})),
            422,
        )
        .await?;
    let request = world
        .ctx
        .h
        .request(Method::POST, "/api/tokens")?
        .header("cookie", &world.researcher.session.cookie)
        .json(&token_body);
    let error = world
        .ctx
        .checked(Method::POST, "/api/tokens", request, 403)
        .await?;
    code(&error, "csrf_invalid");
    Ok(())
}

async fn service_accounts(world: &mut World) -> Result<(Value, String)> {
    let base = world.base();
    let path = format!("{base}/service-accounts");
    let mut agent = Value::Null;
    for (name, kind) in [
        ("codex", "agent"),
        ("tester", "tester"),
        ("stock-evaluator", "evaluator"),
        ("experimenter", "experimenter"),
    ] {
        let account = world
            .ctx
            .api(
                Method::POST,
                "/api/projects/{slug}/service-accounts",
                &path,
                &world.admin.token,
                Some(json!({"name":name,"kind":kind})),
                201,
            )
            .await?;
        assert_eq!(account["name"], name);
        assert_eq!(account["kind"], kind);
        assert_eq!(account["project"], world.slug);
        assert_eq!(account["disabled_at"], Value::Null);
        if kind == "agent" {
            agent = account;
        }
    }
    let list = world
        .ctx
        .api(
            Method::GET,
            "/api/projects/{slug}/service-accounts",
            &path,
            &world.admin.token,
            None,
            200,
        )
        .await?;
    assert_eq!(
        items(&list)?
            .iter()
            .map(|item| item["name"].as_str().unwrap_or(""))
            .collect::<Vec<_>>(),
        vec!["codex", "experimenter", "stock-evaluator", "tester"]
    );
    let conflict = world
        .ctx
        .api(
            Method::POST,
            "/api/projects/{slug}/service-accounts",
            &path,
            &world.admin.token,
            Some(json!({"name":"codex","kind":"agent"})),
            409,
        )
        .await?;
    code(&conflict, "conflict");
    let token_path = format!("{path}/codex/tokens");
    let created = world
        .ctx
        .browser_api(
            Method::POST,
            "/api/projects/{slug}/service-accounts/{name}/tokens",
            &token_path,
            &world.admin.session,
            Some(json!({"name":"agent","expires_in_days":1,"scopes":["read","write"]})),
            201,
        )
        .await?;
    let secret = string(&created["token"])?;
    opaque(&secret, "cr_svc_")?;
    let list = world
        .ctx
        .api(
            Method::GET,
            "/api/projects/{slug}/service-accounts/{name}/tokens",
            &token_path,
            &world.admin.token,
            None,
            200,
        )
        .await?;
    assert_eq!(items(&list)?.len(), 1);
    assert_eq!(items(&list)?[0]["id"], created["id"]);
    assert!(items(&list)?[0].get("token").is_none());
    let me = world
        .ctx
        .api(Method::GET, "/api/me", "/api/me", &secret, None, 200)
        .await?;
    assert_eq!(me["service_account"]["id"], agent["id"]);
    Ok((agent, secret))
}

async fn cleanup(world: &mut World, account: &Value, service: &str) -> Result<()> {
    let path = format!("{}/service-accounts/codex/disable", world.base());
    let disabled = world
        .ctx
        .api(
            Method::POST,
            "/api/projects/{slug}/service-accounts/{name}/disable",
            &path,
            &world.admin.token,
            Some(json!({"reason":"Verify revocation"})),
            200,
        )
        .await?;
    assert_eq!(disabled["id"], account["id"]);
    assert!(!disabled["disabled_at"].is_null());
    world
        .ctx
        .api(Method::GET, "/api/me", "/api/me", service, None, 401)
        .await?;
    let revoke = format!("/api/tokens/{}", string(&world.readonly["id"])?);
    let first = world
        .ctx
        .browser_api(
            Method::DELETE,
            "/api/tokens/{token_id}",
            &revoke,
            &world.researcher.session,
            None,
            200,
        )
        .await?;
    let second = world
        .ctx
        .browser_api(
            Method::DELETE,
            "/api/tokens/{token_id}",
            &revoke,
            &world.researcher.session,
            None,
            200,
        )
        .await?;
    assert_eq!(first["revoked_at"], second["revoked_at"]);
    world
        .ctx
        .api(
            Method::GET,
            "/api/me",
            "/api/me",
            &string(&world.readonly["token"])?,
            None,
            401,
        )
        .await?;
    world
        .ctx
        .api(
            Method::DELETE,
            "/api/projects/{slug}/members/{user_id}",
            &format!(
                "{}/members/{}",
                world.base(),
                string(&world.viewer.session.me["user"]["id"])?
            ),
            &world.admin.token,
            None,
            204,
        )
        .await?;
    world
        .ctx
        .api(
            Method::GET,
            "/api/projects/{slug}",
            &world.base(),
            &world.viewer.token,
            None,
            404,
        )
        .await?;
    logout(world).await
}

async fn logout(world: &mut World) -> Result<()> {
    world
        .ctx
        .browser_api(
            Method::POST,
            "/auth/logout",
            "/auth/logout",
            &world.outsider.session,
            None,
            200,
        )
        .await?;
    world
        .ctx
        .browser_api(
            Method::GET,
            "/api/me",
            "/api/me",
            &world.outsider.session,
            None,
            401,
        )
        .await?;
    Ok(())
}

async fn audit(world: &mut World, account: &Value) -> Result<()> {
    let audit = world.ctx.h.fetch_audit(&world.admin.token, 0).await?;
    let events = items(&audit.body)?;
    let actions = [
        "user.created",
        "user.admin_granted",
        "session.created",
        "session.ended",
        "token.created",
        "token.revoked",
        "project.created",
        "membership.set",
        "membership.removed",
        "service_account.created",
        "service_account.disabled",
    ];
    for action in actions {
        assert!(
            events.iter().any(|event| event["action"] == action),
            "missing R2 audit action {action}"
        );
    }
    assert!(
        events.iter().all(|event| event["action"]
            .as_str()
            .is_some_and(|action| actions.contains(&action))),
        "R2 fixture unexpectedly produced another domain's audit action"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["action"] == "token.revoked"
                && event["subject_id"] == world.readonly["id"])
            .count(),
        1
    );
    let project = events
        .iter()
        .find(|event| {
            event["action"] == "project.created" && event["subject_id"] == world.project["id"]
        })
        .ok_or("project audit absent")?;
    assert_eq!(project["actor_kind"], "user");
    assert_eq!(
        project["actor_user_id"],
        world.admin.session.me["user"]["id"]
    );
    assert_eq!(project["via_channel"], "api");
    assert_eq!(project["project_id"], world.project["id"]);
    let revoked = events
        .iter()
        .find(|event| {
            event["action"] == "token.revoked" && event["subject_id"] == world.readonly["id"]
        })
        .ok_or("revocation audit absent")?;
    assert_eq!(
        revoked["actor_user_id"],
        world.researcher.session.me["user"]["id"]
    );
    assert_eq!(revoked["via_channel"], "ui");
    let disabled = events
        .iter()
        .find(|event| {
            event["action"] == "service_account.disabled" && event["subject_id"] == account["id"]
        })
        .ok_or("disable audit absent")?;
    assert_eq!(
        disabled["actor_user_id"],
        world.admin.session.me["user"]["id"]
    );
    assert_eq!(disabled["via_channel"], "api");
    Ok(())
}
