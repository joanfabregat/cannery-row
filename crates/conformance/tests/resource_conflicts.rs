#![forbid(unsafe_code)]
#[path = "support/lifecycle_support.rs"]
#[allow(
    dead_code,
    reason = "Scenario binaries share fixture bootstrap helpers"
)]
mod support;
use conformance::Result;
use reqwest::{Client, Method};
use serde_json::{Value, json};
use support::{ATTEMPT, Call, World, string};
const ABSENT: &str = "00000000-0000-0000-0000-000000000000";
const MISSING_PROJECT: &str = "absent-resource-conflict-project";
struct Browser {
    cookie: String,
    csrf: String,
    user: String,
}
fn cookie(headers: &reqwest::header::HeaderMap, name: &str) -> Result<String> {
    let prefix = format!("{name}=");
    headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|header| header.to_str().ok())
        .filter_map(|header| header.split(';').next())
        .find(|part| part.starts_with(&prefix))
        .map(ToOwned::to_owned)
        .ok_or_else(|| "browser cookie absent".into())
}
async fn browser(base: &str, subject: &str, email: &str) -> Result<Browser> {
    let oidc = std::env::var("CANNERY_CONFORMANCE_OIDC_URL")?;
    let control = std::env::var("CANNERY_TEST_OIDC_CONTROL_TOKEN")?;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let login = client.get(format!("{base}/auth/login")).send().await?;
    assert_eq!(login.status().as_u16(), 302);
    let binding = cookie(login.headers(), "cr_login")?;
    let authorization = login
        .headers()
        .get("location")
        .ok_or("login redirect absent")?
        .to_str()?;
    let approval:Value=client.post(format!("{oidc}/__conformance/approve")).bearer_auth(control).json(&json!({"authorization_url":authorization,"claims":{"sub":subject,"email":email,"email_verified":true,"name":"Resource scenario browser"}})).send().await?.error_for_status()?.json().await?;
    let callback = client
        .get(format!("{base}/auth/callback"))
        .query(&[
            ("state", string(&approval["state"])?),
            ("code", string(&approval["code"])?),
        ])
        .header("cookie", binding)
        .send()
        .await
        .map_err(|_| "OIDC callback transport failed")?;
    assert_eq!(callback.status().as_u16(), 302);
    let session = cookie(callback.headers(), "cr_session")?;
    let me: Value = client
        .get(format!("{base}/api/me"))
        .header("cookie", &session)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(Browser {
        cookie: session,
        csrf: string(&me["csrf_token"])?,
        user: string(&me["user"]["id"])?,
    })
}
async fn browser_request(
    world: &mut World,
    browser: &Browser,
    method: Method,
    template: &str,
    path: &str,
    body: Value,
    status: u16,
) -> Result<Value> {
    let response = world
        .h
        .request(method.clone(), path)?
        .header("cookie", &browser.cookie)
        .header("X-CSRF-Token", &browser.csrf)
        .json(&body)
        .send()
        .await?;
    Ok(world
        .h
        .check_response(method, template, response, status)
        .await?
        .body)
}
async fn request(
    world: &mut World,
    method: Method,
    template: &str,
    path: String,
    token: &str,
    body: Value,
    status: u16,
) -> Result<Value> {
    let mut call = Call::post(template, path, token, body, status);
    call.method = method;
    Ok(world.api(call).await?.body)
}
async fn get_error(
    world: &mut World,
    template: &str,
    path: String,
    token: &str,
    status: u16,
) -> Result<()> {
    let mut call = Call::get(template, path, token);
    call.status = status;
    world.api(call).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
#[allow(
    clippy::too_many_lines,
    reason = "Missing resources and pre-science conflicts share a genuinely empty API-created project"
)]
async fn administrative_missing_resources_and_configuration_prerequisites() -> Result<()> {
    let (mut world, actors) = World::new("resource-admin").await?;
    let base = world.base();
    let admin = browser(
        &world.base_url,
        "conformance-admin",
        "admin@conformance.test",
    )
    .await?;
    let members = "/api/projects/{slug}/members/{user_id}";
    request(
        &mut world,
        Method::PUT,
        members,
        format!("{base}/members/{ABSENT}"),
        &actors.admin,
        json!({"role":"member"}),
        404,
    )
    .await?;
    request(
        &mut world,
        Method::DELETE,
        members,
        format!("{base}/members/{ABSENT}"),
        &actors.admin,
        json!({}),
        404,
    )
    .await?;
    get_error(
        &mut world,
        "/api/projects/{slug}/members",
        format!("{base}/members"),
        &actors.agent,
        403,
    )
    .await?;
    get_error(
        &mut world,
        "/api/tokens",
        "/api/tokens".into(),
        &actors.agent,
        403,
    )
    .await?;
    request(
        &mut world,
        Method::PATCH,
        "/api/projects/{slug}/tracks/{track_slug}",
        format!("{base}/tracks/absent"),
        &actors.admin,
        json!({"expected_revision":1,"title":"Absent track"}),
        404,
    )
    .await?;
    request(
        &mut world,
        Method::POST,
        "/api/projects/{slug}/tracks",
        format!("/api/projects/{MISSING_PROJECT}/tracks"),
        &actors.admin,
        json!({"slug":"probe","title":"Probe"}),
        404,
    )
    .await?;
    let transition = json!({"expected_revision":1,"to_state":"paused","reason":"valid transition on a missing target"});
    request(
        &mut world,
        Method::POST,
        "/api/projects/{slug}/tracks/{track_slug}/transitions",
        format!("{base}/tracks/lexical/transitions"),
        &actors.agent,
        transition.clone(),
        403,
    )
    .await?;
    request(
        &mut world,
        Method::POST,
        "/api/projects/{slug}/tracks/{track_slug}/transitions",
        format!("{base}/tracks/absent/transitions"),
        &actors.admin,
        transition,
        404,
    )
    .await?;
    let empty = format!("{}-empty", world.project);
    request(
        &mut world,
        Method::POST,
        "/api/projects",
        "/api/projects".into(),
        &actors.admin,
        json!({"slug":empty,"title":"Empty configuration prerequisite fixture"}),
        201,
    )
    .await?;
    request(
        &mut world,
        Method::PUT,
        members,
        format!("/api/projects/{empty}/members/{}", admin.user),
        &actors.admin,
        json!({"role":"researcher"}),
        200,
    )
    .await?;
    let science: Value =
        serde_json::from_str(include_str!("../../../examples/fixture/science.json"))?;
    request(
        &mut world,
        Method::POST,
        "/api/projects/{slug}/config/{kind}",
        format!("/api/projects/{MISSING_PROJECT}/config/science"),
        &actors.admin,
        science,
        404,
    )
    .await?;
    let dashboard: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/contracts/dashboard_views/valid/tracks_vs_control.json"
    ))?;
    let conflict = request(
        &mut world,
        Method::POST,
        "/api/projects/{slug}/config/{kind}",
        format!("/api/projects/{empty}/config/dashboard"),
        &actors.admin,
        dashboard,
        409,
    )
    .await?;
    assert_eq!(conflict["error"]["code"], "conflict");
    for (kind, document) in [
        (
            "experiment-steps",
            serde_json::from_str::<Value>(include_str!(
                "../../../examples/fixture/experiments/fixture-experiment.json"
            ))?,
        ),
        (
            "producers",
            serde_json::from_str::<Value>(include_str!(
                "../../../examples/fixture/producers/overlap-producer.json"
            ))?,
        ),
    ] {
        let template = format!("/api/projects/{{slug}}/{kind}");
        request(
            &mut world,
            Method::POST,
            &template,
            format!("/api/projects/{MISSING_PROJECT}/{kind}"),
            &actors.admin,
            document.clone(),
            404,
        )
        .await?;
        request(
            &mut world,
            Method::POST,
            &template,
            format!("/api/projects/{empty}/{kind}"),
            &actors.admin,
            document,
            409,
        )
        .await?;
    }
    let hypothesis: Value =
        serde_json::from_str(include_str!("../../../examples/fixture/hypothesis.json"))?;
    request(
        &mut world,
        Method::POST,
        "/api/projects/{slug}/hypotheses",
        format!("/api/projects/{empty}/hypotheses"),
        &actors.admin,
        hypothesis,
        409,
    )
    .await?;
    world.finish_coverage(&actors.admin, "resource-admin").await
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
#[allow(
    clippy::too_many_lines,
    reason = "Real draft, attempt and comment identities distinguish authorization from lookup conflicts"
)]
async fn hypothesis_comment_review_and_attempt_target_errors() -> Result<()> {
    // Comment creation has no revision precondition or conflict branch:
    // comments/routes.py::_create inserts a new UUID and ignores unknown
    // mentions through resolve_mentions. A 409 belongs to comment edits.
    let (mut world, actors) = World::new("resource-research").await?;
    let base = world.base();
    let hypothesis: Value =
        serde_json::from_str(include_str!("../../../examples/fixture/hypothesis.json"))?;
    request(
        &mut world,
        Method::POST,
        "/api/projects/{slug}/hypotheses",
        format!("{base}/hypotheses"),
        &actors.tester,
        hypothesis.clone(),
        403,
    )
    .await?;
    request(
        &mut world,
        Method::POST,
        "/api/projects/{slug}/hypotheses",
        format!("/api/projects/{MISSING_PROJECT}/hypotheses"),
        &actors.agent,
        hypothesis.clone(),
        404,
    )
    .await?;
    let number = world
        .queue(&actors, "Source backed resource conflicts")
        .await?;
    let path = format!("{base}/hypotheses/{number}");
    let missing = format!("{base}/hypotheses/999999");
    let update = json!({"expected_revision":1,"document":hypothesis});
    request(
        &mut world,
        Method::PUT,
        "/api/projects/{slug}/hypotheses/{number}",
        path.clone(),
        &actors.other_agent,
        update.clone(),
        403,
    )
    .await?;
    request(
        &mut world,
        Method::PUT,
        "/api/projects/{slug}/hypotheses/{number}",
        missing.clone(),
        &actors.agent,
        update,
        404,
    )
    .await?;
    let review =
        json!({"draft_revision":1,"action":"approve","reason":"Valid draft review target probe"});
    let review_template = "/api/projects/{slug}/hypotheses/{number}/draft-review";
    for (target, token, status) in [
        (format!("{path}/draft-review"), &actors.agent, 403),
        (format!("{missing}/draft-review"), &actors.admin, 404),
        (format!("{path}/draft-review"), &actors.admin, 409),
    ] {
        request(
            &mut world,
            Method::POST,
            review_template,
            target,
            token,
            review.clone(),
            status,
        )
        .await?;
    }
    let lease = world.claim(&actors, number, false).await?;
    let attempt_path = world.attempt_path(&lease);
    let comment = json!({"body_markdown":"A source-backed comment fixture."});
    request(
        &mut world,
        Method::POST,
        "/api/projects/{slug}/hypotheses/{number}/comments",
        format!("{missing}/comments"),
        &actors.admin,
        comment.clone(),
        404,
    )
    .await?;
    request(
        &mut world,
        Method::POST,
        &format!("{ATTEMPT}/comments"),
        format!("{path}/attempts/999999/comments"),
        &actors.admin,
        comment.clone(),
        404,
    )
    .await?;
    request(
        &mut world,
        Method::POST,
        &format!("{ATTEMPT}/comments"),
        format!("{attempt_path}/comments"),
        &actors.agent,
        comment.clone(),
        403,
    )
    .await?;
    let created = request(
        &mut world,
        Method::POST,
        "/api/projects/{slug}/hypotheses/{number}/comments",
        format!("{path}/comments"),
        &actors.admin,
        comment,
        201,
    )
    .await?;
    let other = browser(
        &world.base_url,
        "resource-comment-member",
        "resource-member@conformance.test",
    )
    .await?;
    request(
        &mut world,
        Method::PUT,
        "/api/projects/{slug}/members/{user_id}",
        format!("{base}/members/{}", other.user),
        &actors.admin,
        json!({"role":"member"}),
        200,
    )
    .await?;
    let comment_template = "/api/projects/{slug}/comments/{comment_id}";
    let edited =
        json!({"expected_revision":1,"body_markdown":"Different author cannot edit this."});
    browser_request(
        &mut world,
        &other,
        Method::PUT,
        comment_template,
        &format!("{base}/comments/{}", string(&created["id"])?),
        edited.clone(),
        403,
    )
    .await?;
    request(
        &mut world,
        Method::PUT,
        comment_template,
        format!("{base}/comments/{ABSENT}"),
        &actors.admin,
        edited,
        404,
    )
    .await?;
    world
        .api(
            Call::post(
                &format!("{ATTEMPT}/heartbeat"),
                format!("{path}/attempts/999999/heartbeat"),
                &actors.agent,
                json!({}),
                404,
            )
            .lease(&lease)?,
        )
        .await?;
    let mut malformed = Call::get(
        &format!("{ATTEMPT}/inputs/predecessor/{{artifact_id}}"),
        format!(
            "{}/inputs/predecessor/not-a-uuid",
            world.attempt_path(&lease)
        ),
        &actors.agent,
    )
    .lease(&lease)?;
    malformed.status = 422;
    world.api(malformed).await?;
    request(
        &mut world,
        Method::POST,
        "/api/projects/{slug}/review-cases/{case_id}/decisions",
        format!("{base}/review-cases/{ABSENT}/decisions"),
        &actors.admin,
        json!({"review_case_id":ABSENT,"evidence_revision":1,"action":"promote","reason":"Valid decision on missing case"}),
        404,
    ).await?;
    world
        .finish_coverage(&actors.admin, "resource-research")
        .await
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
#[allow(
    clippy::too_many_lines,
    reason = "Enabled and disabled service identities distinguish browser minting constraints"
)]
async fn service_resources_require_admin_browser_minting_and_enabled_accounts() -> Result<()> {
    let (mut world, actors) = World::new("resource-services").await?;
    let base = world.base();
    let admin = browser(
        &world.base_url,
        "conformance-admin",
        "admin@conformance.test",
    )
    .await?;
    let accounts = "/api/projects/{slug}/service-accounts";
    request(
        &mut world,
        Method::POST,
        accounts,
        format!("/api/projects/{MISSING_PROJECT}/service-accounts"),
        &actors.admin,
        json!({"kind":"agent","name":"probe"}),
        404,
    )
    .await?;
    request(
        &mut world,
        Method::POST,
        accounts,
        format!("{base}/service-accounts"),
        &actors.admin,
        json!({"kind":"agent","name":"agent"}),
        409,
    )
    .await?;
    let disable = "/api/projects/{slug}/service-accounts/{name}/disable";
    request(
        &mut world,
        Method::POST,
        disable,
        format!("{base}/service-accounts/agent/disable"),
        &actors.agent,
        json!({"reason":"Forbidden self-disable"}),
        403,
    )
    .await?;
    request(
        &mut world,
        Method::POST,
        disable,
        format!("{base}/service-accounts/absent/disable"),
        &actors.admin,
        json!({"reason":"Missing account"}),
        404,
    )
    .await?;
    let tokens = "/api/projects/{slug}/service-accounts/{name}/tokens";
    get_error(
        &mut world,
        tokens,
        format!("{base}/service-accounts/agent/tokens"),
        &actors.agent,
        403,
    )
    .await?;
    get_error(
        &mut world,
        tokens,
        format!("{base}/service-accounts/absent/tokens"),
        &actors.admin,
        404,
    )
    .await?;
    let body = json!({"name":"probe","expires_in_days":1,"scopes":["read","write"]});
    request(
        &mut world,
        Method::POST,
        tokens,
        format!("{base}/service-accounts/agent/tokens"),
        &actors.admin,
        body.clone(),
        403,
    )
    .await?;
    browser_request(
        &mut world,
        &admin,
        Method::POST,
        tokens,
        &format!("{base}/service-accounts/absent/tokens"),
        body.clone(),
        404,
    )
    .await?;
    request(
        &mut world,
        Method::POST,
        accounts,
        format!("{base}/service-accounts"),
        &actors.admin,
        json!({"kind":"agent","name":"disabled-probe"}),
        201,
    )
    .await?;
    request(
        &mut world,
        Method::POST,
        disable,
        format!("{base}/service-accounts/disabled-probe/disable"),
        &actors.admin,
        json!({"reason":"Disabled minting conflict fixture"}),
        200,
    )
    .await?;
    let conflict = browser_request(
        &mut world,
        &admin,
        Method::POST,
        tokens,
        &format!("{base}/service-accounts/disabled-probe/tokens"),
        body,
        409,
    )
    .await?;
    assert_eq!(conflict["error"]["code"], "conflict");
    world
        .finish_coverage(&actors.admin, "resource-services")
        .await
}
