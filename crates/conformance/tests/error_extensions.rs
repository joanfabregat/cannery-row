//! Typed boundary validation and browser restrictions across write routes.
#![forbid(unsafe_code)]
#![allow(clippy::too_many_arguments, clippy::too_many_lines)]
#![allow(
    dead_code,
    reason = "Boundary scenarios share bootstrap helpers with the broader matrix"
)]
include!("support/error_matrix.rs");

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn write_route_validation_and_browser_restrictions() -> Result<()> {
    let mut world = World::new()?;
    let admin = world
        .login("conformance-admin", "admin@conformance.test")
        .await?;
    let (token, admin_token_id) = world
        .token(&admin, "write-validation", &["read", "write"])
        .await?;
    let project = unique()?;
    world
        .api(
            Method::POST,
            "/api/projects",
            "/api/projects",
            None,
            Some(&token),
            Some(json!({"slug":project,"title":"Boundary validation"})),
            201,
        )
        .await?;
    world
        .api(
            Method::PUT,
            "/api/projects/{slug}/members/{user_id}",
            &format!("/api/projects/{project}/members/{}", admin.id),
            None,
            Some(&token),
            Some(json!({"role":"researcher"})),
            200,
        )
        .await?;
    let requirements: Value =
        serde_json::from_str(include_str!("../fixtures/coverage-requirements.json"))?;
    for operation in requirements["operations"]
        .as_array()
        .ok_or("missing operations")?
    {
        let method = string(operation, "method")?;
        if !["POST", "PUT"].contains(&method)
            || !operation["required_statuses"]
                .as_array()
                .is_some_and(|statuses| statuses.contains(&json!(422)))
        {
            continue;
        }
        let template = string(operation, "path")?;
        if template.starts_with("/api/uploads/") || template.starts_with("/api/job-uploads/") {
            continue;
        }
        let path = template
            .replace("{slug}", &project)
            .replace("{kind}", "science")
            .replace("{name}", "missing")
            .replace("{track_slug}", "missing")
            .replace("{number}", "invalid")
            .replace("{sequence}", "invalid")
            .replace("{revision}", "invalid")
            .replace("{job_id}", "invalid")
            .replace("{case_id}", "invalid")
            .replace("{comment_id}", "invalid")
            .replace("{user_id}", "invalid")
            .replace("{token_id}", "invalid");
        let checked = world
            .api(
                Method::from_bytes(method.as_bytes())?,
                template,
                &path,
                None,
                Some(&token),
                Some(json!({"unexpected":true})),
                422,
            )
            .await?;
        assert_code(&checked, "validation_failed");
    }
    // A browser session mutation must carry its own CSRF token.
    let response = world
        .harness
        .request(Method::POST, "/auth/logout")?
        .header(header::COOKIE, &admin.cookie)
        .send()
        .await?;
    let checked = world
        .harness
        .check_response(Method::POST, "/auth/logout", response, 403)
        .await?;
    assert_code(&checked.body, "csrf_invalid");
    // A user cannot revoke a different user's personal token.
    let other = world
        .login("write-other", "write-other@conformance.test")
        .await?;
    let (other_token, _) = world.token(&other, "other", &["read", "write"]).await?;
    let checked = world
        .api(
            Method::DELETE,
            "/api/tokens/{token_id}",
            &format!("/api/tokens/{admin_token_id}"),
            None,
            Some(&other_token),
            None,
            404,
        )
        .await?;
    assert_code(&checked, "not_found");
    let (readonly, _) = world.token(&other, "readonly", &["read"]).await?;
    let checked = world
        .api(
            Method::DELETE,
            "/api/tokens/{token_id}",
            &format!("/api/tokens/{admin_token_id}"),
            None,
            Some(&readonly),
            None,
            403,
        )
        .await?;
    assert_code(&checked, "forbidden");
    let checked = world
        .api(
            Method::DELETE,
            "/api/tokens/{token_id}",
            "/api/tokens/00000000-0000-0000-0000-000000000000",
            None,
            Some(&token),
            None,
            404,
        )
        .await?;
    assert_code(&checked, "not_found");
    world.export()?;
    Ok(())
}
