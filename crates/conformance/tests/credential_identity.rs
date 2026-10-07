//! R2-only production credentials survive restart and real PostgreSQL expiry.
#![forbid(unsafe_code)]

#[path = "support/identity_security_support.rs"]
#[allow(
    dead_code,
    reason = "Reuse public identity/project clients without research setup"
)]
mod identity;
#[path = "support/credential_support.rs"]
#[allow(
    dead_code,
    reason = "Reuse private RAM writer and public credential APIs"
)]
mod ram;

use conformance::Result;
use identity::{Context, claims, opaque, string};
use reqwest::Method;
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt};

fn read_envelope() -> Result<Value> {
    let path = ram::envelope_path()?;
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.len() > 16384
    {
        return Err("R2 private envelope permissions or size differ".into());
    }
    let data: Value = serde_json::from_slice(&fs::read(path)?)?;
    let fields = [
        "version", "project", "user", "session", "pat", "service", "control",
    ];
    let object = data.as_object().ok_or("R2 envelope is not an object")?;
    if data["version"] != 2
        || object.len() != fields.len()
        || !fields.iter().all(|key| object.contains_key(*key))
    {
        return Err("R2 envelope version or cases differ".into());
    }
    for (kind, prefix) in [
        ("session", "cr_ses_"),
        ("pat", "cr_pat_"),
        ("service", "cr_svc_"),
        ("control", "cr_pat_"),
    ] {
        opaque(&string(&data[kind]["secret"])?, prefix)?;
    }
    Ok(data)
}

async fn prepare_project(
    ctx: &mut Context,
    admin: &identity::Session,
    control: &str,
) -> Result<Value> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let slug = format!("identity-r2-credential-storage-{nonce}");
    let project = ctx
        .api(
            Method::POST,
            "/api/projects",
            "/api/projects",
            control,
            Some(json!({"slug":slug,"title":"Identity credential persistence"})),
            201,
        )
        .await?;
    ctx.api(
        Method::PUT,
        "/api/projects/{slug}/members/{user_id}",
        &format!(
            "/api/projects/{slug}/members/{}",
            string(&admin.me["user"]["id"])?
        ),
        control,
        Some(json!({"role":"researcher"})),
        200,
    )
    .await?;
    Ok(project)
}

#[tokio::test]
#[ignore = "requires the isolated R2 credential fixture launcher"]
async fn prepare_r2_credential_fixture() -> Result<()> {
    let mut ctx = Context::new()?;
    let admin = ctx
        .login(
            claims("conformance-admin", "admin@conformance.test", true),
            "/",
        )
        .await?;
    let control = ctx
        .mint(&admin, "credential-storage-control", &["read", "write"])
        .await?;
    let control_secret = string(&control["token"])?;
    let project = prepare_project(&mut ctx, &admin, &control_secret).await?;
    let user = ctx
        .login(
            claims(
                "credential-r2-fixture-user",
                "credential-r2-user@conformance.test",
                true,
            ),
            "/",
        )
        .await?;
    let pat = ctx
        .mint(&user, "credential-storage-pat", &["read", "write"])
        .await?;
    let (account, service) =
        ram::service_mint(&mut ctx, &admin, &string(&project["slug"])?).await?;
    let secret = user
        .cookie
        .strip_prefix("cr_session=")
        .ok_or("session cookie format")?;
    let audit = ctx.h.fetch_audit(&control_secret, 0).await?.body;
    let sessions: Vec<_> = audit["items"]
        .as_array()
        .ok_or("audit items absent")?
        .iter()
        .filter(|event| {
            event["action"] == "session.created" && event["actor_user_id"] == user.me["user"]["id"]
        })
        .collect();
    if sessions.len() != 1 {
        return Err("R2 user must have exactly one public session creation".into());
    }
    let envelope = json!({"version":2,
        "project":{"id":project["id"],"slug":project["slug"],"admin_user_id":admin.me["user"]["id"]},
        "user":{"id":user.me["user"]["id"],"subject":"credential-r2-fixture-user"},
        "session":{"id":sessions[0]["subject_id"],"secret":secret},
        "pat":{"id":pat["id"],"secret":pat["token"]},
        "service":{"id":account["id"],"token_id":service["id"],"secret":service["token"]},
        "control":{"id":control["id"],"secret":control["token"]}});
    for (kind, prefix) in [
        ("session", "cr_ses_"),
        ("pat", "cr_pat_"),
        ("service", "cr_svc_"),
        ("control", "cr_pat_"),
    ] {
        opaque(&string(&envelope[kind]["secret"])?, prefix)?;
    }
    ram::write_envelope(&envelope)?;
    ctx.finish(&control_secret, "r2-credential-prepare").await
}

async fn assert_credentials(ctx: &mut Context, data: &Value, status: u16) -> Result<()> {
    let session = ram::session_me(ctx, data, status).await?;
    let pat = ram::me(ctx, &string(&data["pat"]["secret"])?, status).await?;
    let service = ram::me(ctx, &string(&data["service"]["secret"])?, status).await?;
    if status == 200 {
        assert_eq!(session["user"]["id"], data["user"]["id"]);
        assert_eq!(session["channel"], "ui");
        assert_eq!(pat["user"]["id"], data["user"]["id"]);
        assert_eq!(pat["channel"], "api");
        assert_eq!(service["service_account"]["id"], data["service"]["id"]);
        assert_eq!(service["channel"], "api");
    } else {
        for body in [session, pat, service] {
            assert_eq!(body["error"]["code"], "unauthenticated");
        }
    }
    let control = ram::me(ctx, &string(&data["control"]["secret"])?, 200).await?;
    assert_eq!(control["user"]["id"], data["project"]["admin_user_id"]);
    Ok(())
}

#[tokio::test]
#[ignore = "launcher runs after actual API restart and before short expiry edit"]
async fn exercise_restarted_r2_credentials() -> Result<()> {
    let data = read_envelope()?;
    let mut ctx = Context::new()?;
    assert_credentials(&mut ctx, &data, 200).await?;
    let path = format!("/api/projects/{}", string(&data["project"]["slug"])?);
    let project = ctx
        .api(
            Method::GET,
            "/api/projects/{slug}",
            &path,
            &string(&data["service"]["secret"])?,
            None,
            200,
        )
        .await?;
    assert_eq!(project["id"], data["project"]["id"]);
    ctx.finish(
        &string(&data["control"]["secret"])?,
        "r2-credential-restart",
    )
    .await
}

#[tokio::test]
#[ignore = "launcher sets real PostgreSQL expiry after restart proof"]
async fn exercise_r2_credential_expiry() -> Result<()> {
    let data = read_envelope()?;
    let mut ctx = Context::new()?;
    assert_credentials(&mut ctx, &data, 200).await?;
    tokio::time::sleep(std::time::Duration::from_millis(10_500)).await;
    assert_credentials(&mut ctx, &data, 401).await?;
    ctx.finish(&string(&data["control"]["secret"])?, "r2-credential-expiry")
        .await
}
