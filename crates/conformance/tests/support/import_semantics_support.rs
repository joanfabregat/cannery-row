use super::import_support::{CONFIG, PROJECT, World, string};
use conformance::Result;
use reqwest::{Client, Method, header::HeaderMap};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

pub async fn empty_project(world: &mut World) -> Result<()> {
    let admin = world.admin.token.clone();
    let base = world.base();
    world
        .api(
            Method::POST,
            "/api/projects",
            "/api/projects",
            &admin,
            Some(&json!({"slug":world.slug,"title":"Semantic rollback"})),
            201,
        )
        .await?;
    for id in [world.ana.id.clone(), world.ben.id.clone()] {
        world
            .api(
                Method::PUT,
                "/api/projects/{slug}/members/{user_id}",
                &format!("{base}/members/{id}"),
                &admin,
                Some(&json!({"role":"researcher"})),
                200,
            )
            .await?;
    }
    let science: Value = serde_json::from_slice(&fs::read(&world.science)?)?;
    world
        .api(
            Method::POST,
            CONFIG,
            &format!("{base}/config/science"),
            &admin,
            Some(&science),
            201,
        )
        .await?;
    Ok(())
}

pub async fn snapshot(world: &mut World) -> Result<Value> {
    let base = world.base();
    let mut public = Vec::new();
    for (template, path) in [
        (PROJECT, base.clone()),
        (
            "/api/projects/{slug}/members",
            format!("{base}/members?limit=100"),
        ),
        (
            "/api/projects/{slug}/hypotheses",
            format!("{base}/hypotheses?limit=100"),
        ),
        (
            "/api/projects/{slug}/tracks",
            format!("{base}/tracks?limit=100"),
        ),
        (CONFIG, format!("{base}/config/science?limit=100")),
    ] {
        public.push(world.get(template, &path).await?);
    }
    let response = world
        .h
        .request(
            Method::GET,
            &format!("/__conformance/storage/projects/{}", world.slug),
        )?
        .bearer_auth(&world.admin.read_token)
        .send()
        .await?;
    assert_eq!(response.status(), 200);
    let storage: Value = response.json().await?;
    assert_eq!(storage["session"]["read_only"], "on");
    Ok(json!({"public":public,"storage":storage}))
}

pub async fn unchanged(world: &mut World, before: &Value, cursor: u64) -> Result<()> {
    // Same database and no intervening writes: captured now()/UUID columns must also match.
    assert_eq!(&snapshot(world).await?, before);
    let (_, events) = world.audit(cursor).await?;
    assert!(!events.iter().any(|event| {
        event["action"]
            .as_str()
            .is_some_and(|action| action.starts_with("import."))
    }));
    Ok(())
}

pub fn finish(world: &World, name: &str) -> Result<()> {
    let directory = PathBuf::from(std::env::var("CANNERY_CONFORMANCE_COVERAGE_DIR")?);
    fs::create_dir_all(&directory)?;
    fs::write(
        directory.join(format!("{name}.json")),
        serde_json::to_vec_pretty(world.h.coverage())?,
    )?;
    Ok(())
}

pub async fn unverified_user(world: &mut World) -> Result<String> {
    let base = std::env::var("CANNERY_CONFORMANCE_URL")?;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let response = client.get(format!("{base}/auth/login")).send().await?;
    assert_eq!(response.status(), 302);
    let authorization = response
        .headers()
        .get("location")
        .ok_or("login location absent")?
        .to_str()?;
    let binding = cookie(response.headers(), "cr_login")?;
    let email = format!("{}-unverified@conformance.test", world.slug);
    let oidc = std::env::var("CANNERY_CONFORMANCE_OIDC_URL")?;
    let control = std::env::var("CANNERY_TEST_OIDC_CONTROL_TOKEN")?;
    let approved: Value = client.post(format!("{oidc}/__conformance/approve")).bearer_auth(control).json(&json!({"authorization_url":authorization,"claims":{"sub":format!("{}-unverified",world.slug),"email":email,"email_verified":false,"name":"Unverified import actor"}})).send().await?.error_for_status()?.json().await?;
    let response = client
        .get(format!("{base}/auth/callback"))
        .header("cookie", binding)
        .query(&[
            ("state", string(&approved["state"])?),
            ("code", string(&approved["code"])?),
        ])
        .send()
        .await?;
    assert_eq!(response.status(), 302);
    let session = cookie(response.headers(), "cr_session")?;
    let response = client
        .get(format!("{base}/api/me"))
        .header("cookie", session)
        .send()
        .await?;
    let checked = world
        .h
        .check_response(Method::GET, "/api/me", response, 200)
        .await?;
    assert_eq!(checked.body["user"]["email"], email);
    assert_eq!(checked.body["user"]["email_verified"], false);
    Ok(email)
}

fn cookie(headers: &HeaderMap, name: &str) -> Result<String> {
    for value in headers.get_all("set-cookie") {
        let pair = value.to_str()?.split(';').next().ok_or("cookie absent")?;
        if pair.starts_with(&format!("{name}=")) {
            return Ok(pair.to_owned());
        }
    }
    Err("login cookie absent".into())
}
