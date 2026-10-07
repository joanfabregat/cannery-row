//! Production-created opaque credentials survive restart and genuine elapsed expiry.
//! Secret values and digests are held only in the launcher's private RAM envelope.
#[path = "support/identity_security_support.rs"]
#[allow(dead_code, reason = "Reuse public OIDC and token APIs")]
mod identity;
#[path = "support/lifecycle_support.rs"]
#[allow(dead_code, reason = "Reuse public research preparation")]
mod lifecycle;
#[path = "support/recovery_support.rs"]
#[allow(dead_code, reason = "Reuse public submission preparation")]
mod recovery;
#[path = "support/credential_support.rs"]
mod support;
use conformance::Result;
use identity::{Context, claims, opaque, string};
use lifecycle::{ATTEMPT, Call, JOB, Lease, World, sha};
use reqwest::Method;
use serde_json::{Value, json};

#[tokio::test]
#[ignore = "requires isolated credential fixture launcher; lease TTL60"]
#[allow(
    clippy::too_many_lines,
    reason = "Keep public secret preparation and private envelope assembly together"
)]
async fn prepare_credential_fixture() -> Result<()> {
    if std::env::var("CANNERY_LEASES_TTL_SECONDS")?.as_str() != "60" {
        return Err("credential fixture requires --lease-ttl-seconds 60".into());
    }
    let (mut world, actors) = World::new("credential-storage").await?;
    let project = world
        .api(Call::get(
            "/api/projects/{slug}",
            world.base(),
            &actors.admin,
        ))
        .await?
        .body;
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
    let user = ctx
        .login(
            claims(
                "credential-fixture-user",
                "credential-fixture-user@conformance.test",
                true,
            ),
            "/",
        )
        .await?;
    let pat = ctx
        .mint(&user, "credential-storage-pat", &["read", "write"])
        .await?;
    let (account, service) = support::service_mint(&mut ctx, &admin, &world.project).await?;
    let service_secret = string(&service["token"])?;
    let number = world.queue(&actors, "Credential lease storage").await?;
    let claimed = world
        .api(Call::post(
            "/api/projects/{slug}/claims",
            format!("{}/claims", world.base()),
            &service_secret,
            json!({"hypothesis":number}),
            201,
        ))
        .await?
        .body;
    let lease = Lease::attempt(&claimed)?;
    // A second, normally submitted attempt creates the separate tester job.
    let submitted = recovery::submit(&mut world, &actors).await?;
    let job = world.claim_job(&actors, false, false).await?;
    let tester = support::me(&mut ctx, &actors.tester, 200).await?;
    let bytes = b"credential fixture verified log\n";
    let grant=world.api(Call::post(&format!("{JOB}/uploads"),format!("{}/uploads",world.job_path(&job)?),&actors.tester,json!({"role":"step_log","path":"fixture-scorer/step_log/credential.log","size_bytes":bytes.len(),"sha256":sha(bytes),"media_type":"text/plain"}),201).lease(&job)?).await?.body;
    let secret = user
        .cookie
        .strip_prefix("cr_session=")
        .ok_or("session cookie format")?;
    let events = ctx
        .h
        .fetch_audit(&string(&control["token"])?, 0)
        .await?
        .body;
    let sessions: Vec<_> = events["items"]
        .as_array()
        .ok_or("audit items")?
        .iter()
        .filter(|e| e["action"] == "session.created" && e["actor_user_id"] == user.me["user"]["id"])
        .collect();
    if sessions.len() != 1 {
        return Err("fixture user must have exactly one public session creation".into());
    }
    let upload_url = reqwest::Url::parse(&string(&grant["upload_url"])?)?;
    let upload_id = upload_url
        .path()
        .rsplit('/')
        .next()
        .ok_or("upload ID absent")?;
    let envelope = json!({"version":1,
        "project":{"id":project["id"],"slug":world.project,"admin_user_id":admin.me["user"]["id"]},
        "user":{"id":user.me["user"]["id"],"subject":"credential-fixture-user"},
        "session":{"id":sessions[0]["subject_id"],"secret":secret},
        "pat":{"id":pat["id"],"secret":pat["token"]},
        "service":{"id":account["id"],"token_id":service["id"],"secret":service["token"]},
        "lease":{"attempt_id":lease.document["id"],"number":number,"secret":lease.token,"generation":lease.generation},
        "job":{"id":job.document["job_id"],"attempt_id":job.document["attempt_id"],"number":submitted.document["number"],"service_id":tester["service_account"]["id"],"secret":job.token,"generation":job.generation,"worker_secret":actors.tester},
        "upload":{"id":upload_id,"secret":grant["headers"]["X-Upload-Token"]},
        "control":{"id":control["id"],"secret":control["token"]}
    });
    for (kind, prefix) in [
        ("session", "cr_ses_"),
        ("pat", "cr_pat_"),
        ("service", "cr_svc_"),
        ("lease", "cr_lease_"),
        ("job", "cr_job_"),
        ("upload", "cr_upl_"),
    ] {
        opaque(&string(&envelope[kind]["secret"])?, prefix)?;
    }
    support::write_envelope(&envelope)?;
    world
        .finish_coverage(&actors.admin, "credential-prepare")
        .await?;
    ctx.finish(&string(&control["token"])?, "credential-prepare")
        .await
}

async fn assert_credentials(ctx: &mut Context, data: &Value, status: u16) -> Result<()> {
    let session = support::session_me(ctx, data, status).await?;
    let pat = support::me(ctx, &string(&data["pat"]["secret"])?, status).await?;
    let service = support::me(ctx, &string(&data["service"]["secret"])?, status).await?;
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
    let control = support::me(ctx, &string(&data["control"]["secret"])?, 200).await?;
    assert_eq!(control["user"]["id"], data["project"]["admin_user_id"]);
    Ok(())
}
#[tokio::test]
#[ignore = "launcher runs after actual API restart and before expiry fixture edit"]
async fn exercise_restarted_credentials() -> Result<()> {
    let data = support::read_envelope()?;
    let mut ctx = Context::new()?;
    assert_credentials(&mut ctx, &data, 200).await?;
    // The same stored attempt lease also authenticates after restart.
    let path = format!(
        "/api/projects/{}/hypotheses/{}/attempts/1/heartbeat",
        string(&data["project"]["slug"])?,
        data["lease"]["number"]
    );
    let request = ctx
        .h
        .request(Method::POST, &path)?
        .bearer_auth(string(&data["service"]["secret"])?)
        .header("X-Lease-Token", string(&data["lease"]["secret"])?)
        .header("X-Lease-Generation", "1")
        .json(&json!({}));
    ctx.checked(Method::POST, &format!("{ATTEMPT}/heartbeat"), request, 200)
        .await?;
    let path = format!(
        "/api/projects/{}/jobs/{}/heartbeat",
        string(&data["project"]["slug"])?,
        string(&data["job"]["id"])?
    );
    let request = ctx
        .h
        .request(Method::POST, &path)?
        .bearer_auth(string(&data["job"]["worker_secret"])?)
        .header("X-Lease-Token", string(&data["job"]["secret"])?)
        .header("X-Lease-Generation", "1")
        .json(&json!({}));
    ctx.checked(Method::POST, &format!("{JOB}/heartbeat"), request, 200)
        .await?;
    let path = format!("/api/job-uploads/{}", string(&data["upload"]["id"])?);
    let request = ctx
        .h
        .request(Method::PUT, &path)?
        .header("X-Upload-Token", string(&data["upload"]["secret"])?)
        .body(b"credential fixture verified log\n".to_vec());
    let artifact = ctx
        .checked(Method::PUT, "/api/job-uploads/{upload_id}", request, 201)
        .await?;
    assert_eq!(artifact["role"], "step_log");
    assert_eq!(
        artifact["size_bytes"],
        b"credential fixture verified log\n".len()
    );
    assert_eq!(
        artifact["sha256"],
        sha(b"credential fixture verified log\n")
    );
    ctx.finish(&string(&data["control"]["secret"])?, "credential-restart")
        .await
}
#[tokio::test]
#[ignore = "launcher supplies short expiry after restart proof; genuine elapsed time"]
async fn exercise_credential_expiry() -> Result<()> {
    let data = support::read_envelope()?;
    let mut ctx = Context::new()?;
    assert_credentials(&mut ctx, &data, 200).await?;
    tokio::time::sleep(std::time::Duration::from_millis(10_500)).await;
    assert_credentials(&mut ctx, &data, 401).await?;
    ctx.finish(&string(&data["control"]["secret"])?, "credential-expiry")
        .await
}
