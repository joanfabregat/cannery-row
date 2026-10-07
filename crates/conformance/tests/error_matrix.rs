//! Reachable authorization, resource and validation errors, without DB seeding.
#![allow(clippy::too_many_arguments, clippy::too_many_lines)]
include!("support/error_matrix.rs");

const NIL: &str = "00000000-0000-0000-0000-000000000000";
fn expand(template: &str, project: &str) -> String {
    let mut path = template
        .replace("{slug}", project)
        .replace("{kind}", "science")
        .replace("{track_slug}", "matrix-track")
        .replace("{name}", "missing");
    for parameter in ["number", "sequence", "revision"] {
        path = path.replace(&format!("{{{parameter}}}"), "1");
    }
    for parameter in [
        "user_id",
        "token_id",
        "upload_id",
        "job_id",
        "artifact_id",
        "case_id",
        "comment_id",
    ] {
        path = path.replace(&format!("{{{parameter}}}"), NIL);
    }
    path = path.replace("{view_id}", "missing");
    match template {
        "/api/users" => format!("{path}?email=matrix@conformance.test"),
        "/api/projects/{slug}/metrics/query" => format!("{path}?metric=ndcg_at_10"),
        _ => path,
    }
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn authentication_validation_and_optimistic_conflict_matrix() -> Result<()> {
    let mut world = World::new()?;
    let requirements: Value =
        serde_json::from_str(include_str!("../fixtures/coverage-requirements.json"))?;
    let operations = requirements["operations"]
        .as_array()
        .ok_or("operation inventory absent")?;
    // Authentication does not require a resource to exist or a privileged fixture.
    for operation in operations {
        if !operation["required_statuses"]
            .as_array()
            .is_some_and(|statuses| statuses.contains(&json!(401)))
        {
            continue;
        }
        let method = Method::from_bytes(string(operation, "method")?.as_bytes())?;
        let template = string(operation, "path")?;
        let path = expand(template, "matrix-missing");
        let body = if method == Method::GET || method == Method::DELETE {
            None
        } else {
            Some(json!({}))
        };
        let response = world
            .api(method, template, &path, None, None, body, 401)
            .await?;
        assert_code(&response, "unauthenticated");
    }
    let run = unique()?.replace("conformance", "matrix");
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
    let (admin_token, _) = world
        .token(&admin, "matrix-admin", &["read", "write"])
        .await?;
    let (researcher_token, _) = world
        .token(&researcher, "matrix-researcher", &["read", "write"])
        .await?;
    let (viewer_token, _) = world
        .token(&viewer, "matrix-viewer", &["read", "write"])
        .await?;
    let (readonly, _) = world
        .token(&researcher, "matrix-readonly", &["read"])
        .await?;
    let base = format!("/api/projects/{run}");
    world
        .api(
            Method::POST,
            "/api/projects",
            "/api/projects",
            None,
            Some(&admin_token),
            Some(json!({"slug":run,"title":"Error matrix"})),
            201,
        )
        .await?;
    for (session, role) in [(&researcher, "researcher"), (&viewer, "viewer")] {
        world
            .api(
                Method::PUT,
                "/api/projects/{slug}/members/{user_id}",
                &format!("{base}/members/{}", session.id),
                None,
                Some(&admin_token),
                Some(json!({"role":role})),
                200,
            )
            .await?;
    }
    for operation in operations {
        let template = string(operation, "path")?;
        if operation["method"] != "GET"
            || template.contains("/inputs/")
            || !operation["required_statuses"]
                .as_array()
                .is_some_and(|statuses| statuses.contains(&json!(404)))
        {
            continue;
        }
        let path = expand(template, "matrix-no-such-project");
        assert_code(
            &world
                .api(
                    Method::GET,
                    template,
                    &path,
                    None,
                    Some(&admin_token),
                    None,
                    404,
                )
                .await?,
            "not_found",
        );
    }
    for (method, template, path, body) in [
        (
            Method::POST,
            "/api/projects",
            "/api/projects".into(),
            json!({"slug":"forbidden-project","title":"Forbidden"}),
        ),
        (
            Method::PUT,
            "/api/projects/{slug}/members/{user_id}",
            format!("{base}/members/{}", researcher.id),
            json!({"role":"viewer"}),
        ),
        (
            Method::DELETE,
            "/api/projects/{slug}/members/{user_id}",
            format!("{base}/members/{}", researcher.id),
            json!({}),
        ),
        (
            Method::POST,
            "/api/projects/{slug}/config/{kind}",
            format!("{base}/config/science"),
            json!({}),
        ),
        (
            Method::POST,
            "/api/projects/{slug}/producers",
            format!("{base}/producers"),
            json!({}),
        ),
        (
            Method::POST,
            "/api/projects/{slug}/experiment-steps",
            format!("{base}/experiment-steps"),
            json!({}),
        ),
        (
            Method::POST,
            "/api/projects/{slug}/service-accounts",
            format!("{base}/service-accounts"),
            json!({"name":"forbidden","kind":"agent"}),
        ),
        (
            Method::POST,
            "/api/projects/{slug}/tracks",
            format!("{base}/tracks"),
            json!({"slug":"forbidden","title":"Forbidden"}),
        ),
        (
            Method::POST,
            "/api/projects/{slug}/claims",
            format!("{base}/claims"),
            json!({}),
        ),
        (
            Method::POST,
            "/api/projects/{slug}/jobs/claims",
            format!("{base}/jobs/claims"),
            json!({}),
        ),
    ] {
        assert_code(
            &world
                .api(
                    method,
                    template,
                    &path,
                    None,
                    Some(&viewer_token),
                    Some(body),
                    403,
                )
                .await?,
            "forbidden",
        );
    }
    for (template, path) in [
        (
            "/api/projects/{slug}/service-accounts",
            format!("{base}/service-accounts"),
        ),
        (
            "/api/projects/{slug}/service-accounts/{name}/tokens",
            format!("{base}/service-accounts/missing/tokens"),
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
                    403,
                )
                .await?,
            "forbidden",
        );
    }
    // Missing or malformed typed inputs yield 422; plain string resources do not.
    for (template, path) in [
        (
            "/api/projects/{slug}/tracks",
            format!("{base}/tracks?limit=0"),
        ),
        (
            "/api/projects/{slug}/tracks/{track_slug}/history",
            format!("{base}/tracks/matrix-track/history?limit=0"),
        ),
        (
            "/api/projects/{slug}/hypotheses",
            format!("{base}/hypotheses?limit=0"),
        ),
        (
            "/api/projects/{slug}/producers",
            format!("{base}/producers?limit=0"),
        ),
        (
            "/api/projects/{slug}/review-cases",
            format!("{base}/review-cases?limit=0"),
        ),
        (
            "/api/projects/{slug}/reports",
            format!("{base}/reports?limit=0"),
        ),
        (
            "/api/projects/{slug}/comparisons",
            format!("{base}/comparisons?limit=0"),
        ),
        (
            "/api/projects/{slug}/hypotheses/{number}/comments",
            format!("{base}/hypotheses/1/comments?limit=0"),
        ),
        (
            "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/comments",
            format!("{base}/hypotheses/1/attempts/1/comments?limit=0"),
        ),
        ("/api/projects", "/api/projects?limit=0".into()),
        ("/api/tokens", "/api/tokens?limit=0".into()),
        ("/api/users", "/api/users?email=x".into()),
        (
            "/api/projects/{slug}/members",
            format!("{base}/members?limit=0"),
        ),
        (
            "/api/projects/{slug}/service-accounts",
            format!("{base}/service-accounts?limit=0"),
        ),
        (
            "/api/projects/{slug}/service-accounts/{name}/tokens",
            format!("{base}/service-accounts/missing/tokens?limit=0"),
        ),
        (
            "/api/projects/{slug}/config/{kind}",
            format!("{base}/config/not-a-kind"),
        ),
        (
            "/api/projects/{slug}/config/{kind}/latest",
            format!("{base}/config/not-a-kind/latest"),
        ),
        (
            "/api/projects/{slug}/config/{kind}/{revision}",
            format!("{base}/config/science/not-an-integer"),
        ),
        (
            "/api/projects/{slug}/hypotheses/{number}",
            format!("{base}/hypotheses/not-an-integer"),
        ),
        (
            "/api/projects/{slug}/hypotheses/{number}/revisions",
            format!("{base}/hypotheses/not-an-integer/revisions"),
        ),
        (
            "/api/projects/{slug}/hypotheses/{number}/revisions/{revision}",
            format!("{base}/hypotheses/1/revisions/not-an-integer"),
        ),
        (
            "/api/projects/{slug}/producers/{name}/{revision}",
            format!("{base}/producers/missing/not-an-integer"),
        ),
        (
            "/api/projects/{slug}/experiment-steps",
            format!("{base}/experiment-steps?limit=0"),
        ),
        (
            "/api/projects/{slug}/experiment-steps/{name}/{revision}",
            format!("{base}/experiment-steps/missing/not-an-integer"),
        ),
        (
            "/api/projects/{slug}/attempts",
            format!("{base}/attempts?limit=0"),
        ),
        (
            "/api/projects/{slug}/hypotheses/{number}/attempts",
            format!("{base}/hypotheses/not-an-integer/attempts"),
        ),
        (
            "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}",
            format!("{base}/hypotheses/1/attempts/not-an-integer"),
        ),
        (
            "/api/projects/{slug}/jobs/{job_id}",
            format!("{base}/jobs/not-a-uuid"),
        ),
        (
            "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/jobs",
            format!("{base}/hypotheses/1/attempts/not-an-integer/jobs"),
        ),
        (
            "/api/projects/{slug}/review-cases/{case_id}",
            format!("{base}/review-cases/not-a-uuid"),
        ),
        (
            "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/report",
            format!("{base}/hypotheses/1/attempts/not-an-integer/report"),
        ),
        (
            "/api/projects/{slug}/artifacts/{artifact_id}",
            format!("{base}/artifacts/not-a-uuid"),
        ),
        (
            "/api/projects/{slug}/comments/{comment_id}",
            format!("{base}/comments/not-a-uuid"),
        ),
        (
            "/api/projects/{slug}/comments/{comment_id}/revisions",
            format!("{base}/comments/not-a-uuid/revisions"),
        ),
        (
            "/api/projects/{slug}/metrics",
            format!("{base}/metrics?science_revision=0"),
        ),
        (
            "/api/projects/{slug}/metrics/query",
            format!("{base}/metrics/query?metric=INVALID!"),
        ),
        (
            "/api/projects/{slug}/dashboard",
            format!("{base}/dashboard?dashboard_revision=0"),
        ),
        (
            "/api/projects/{slug}/dashboard/views/{view_id}",
            format!("{base}/dashboard/views/missing?dashboard_revision=0"),
        ),
        (
            "/api/projects/{slug}/attention",
            format!("{base}/attention?limit=0"),
        ),
        ("/api/search", "/api/search?limit=0".into()),
    ] {
        assert_code(
            &world
                .api(
                    Method::GET,
                    template,
                    &path,
                    None,
                    Some(&admin_token),
                    None,
                    422,
                )
                .await?,
            "validation_failed",
        );
    }
    for (method, template, path, body) in [
        (
            Method::DELETE,
            "/api/tokens/{token_id}",
            "/api/tokens/not-a-uuid".into(),
            None,
        ),
        (
            Method::DELETE,
            "/api/projects/{slug}/members/{user_id}",
            format!("{base}/members/not-a-uuid"),
            None,
        ),
        (
            Method::PUT,
            "/api/projects/{slug}/members/{user_id}",
            format!("{base}/members/not-a-uuid"),
            Some(json!({"role":"viewer"})),
        ),
    ] {
        assert_code(
            &world
                .api(method, template, &path, None, Some(&admin_token), body, 422)
                .await?,
            "validation_failed",
        );
    }
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
    world
        .api(
            Method::POST,
            "/api/projects/{slug}/producers",
            &format!("{base}/producers"),
            None,
            Some(&admin_token),
            Some(fixture(
                "tests/fixtures/contracts/step_manifest/valid/producer.json",
            )?),
            201,
        )
        .await?;
    let track_body = json!({"slug":"matrix-track","title":"Matrix track"});
    world
        .api(
            Method::POST,
            "/api/projects/{slug}/tracks",
            &format!("{base}/tracks"),
            None,
            Some(&researcher_token),
            Some(track_body.clone()),
            201,
        )
        .await?;
    assert_code(
        &world
            .api(
                Method::POST,
                "/api/projects/{slug}/tracks",
                &format!("{base}/tracks"),
                None,
                Some(&researcher_token),
                Some(track_body),
                409,
            )
            .await?,
        "conflict",
    );
    let track_path = format!("{base}/tracks/matrix-track");
    for body in [
        json!({"expected_revision":1}),
        json!({"expected_revision":1,"mode":"workflow"}),
        json!({"expected_revision":1,"alien":true}),
    ] {
        assert_code(
            &world
                .api(
                    Method::PATCH,
                    "/api/projects/{slug}/tracks/{track_slug}",
                    &track_path,
                    None,
                    Some(&researcher_token),
                    Some(body),
                    422,
                )
                .await?,
            "validation_failed",
        );
    }
    assert_code(
        &world
            .api(
                Method::PATCH,
                "/api/projects/{slug}/tracks/{track_slug}",
                &track_path,
                None,
                Some(&readonly),
                Some(json!({"expected_revision":1,"title":"Denied"})),
                403,
            )
            .await?,
        "forbidden",
    );
    assert_code(
        &world
            .api(
                Method::POST,
                "/api/projects/{slug}/tracks/{track_slug}/transitions",
                &format!("{track_path}/transitions"),
                None,
                Some(&researcher_token),
                Some(json!({"to_state":"active","expected_revision":1,"reason":"Already active"})),
                409,
            )
            .await?,
        "conflict",
    );
    let mut draft = fixture("tests/fixtures/contracts/hypothesis/valid/minimal.json")?;
    draft["track"] = json!("matrix-track");
    draft["project_fields"] = json!({"architecture":"hybrid"});
    let created = world
        .api(
            Method::POST,
            "/api/projects/{slug}/hypotheses",
            &format!("{base}/hypotheses"),
            None,
            Some(&researcher_token),
            Some(draft.clone()),
            201,
        )
        .await?;
    let number = created["number"].as_u64().ok_or("draft omitted number")?;
    let hypothesis_path = format!("{base}/hypotheses/{number}");
    assert_code(
        &world
            .api(
                Method::PUT,
                "/api/projects/{slug}/hypotheses/{number}",
                &hypothesis_path,
                None,
                Some(&researcher_token),
                Some(json!({"expected_revision":0,"document":draft})),
                409,
            )
            .await?,
        "stale_revision",
    );
    assert_code(
        &world
            .api(
                Method::POST,
                "/api/projects/{slug}/hypotheses/{number}/draft-review",
                &format!("{hypothesis_path}/draft-review"),
                None,
                Some(&researcher_token),
                Some(json!({"draft_revision":0,"action":"approve","reason":"Invalid revision"})),
                422,
            )
            .await?,
        "validation_failed",
    );
    assert_code(
        &world
            .api(
                Method::POST,
                "/api/projects/{slug}/hypotheses/{number}/comments",
                &format!("{hypothesis_path}/comments"),
                None,
                Some(&viewer_token),
                Some(json!({"body_markdown":"No permission"})),
                403,
            )
            .await?,
        "forbidden",
    );
    let invalid = world
        .api(
            Method::POST,
            "/api/projects/{slug}/hypotheses/{number}/comments",
            &format!("{hypothesis_path}/comments"),
            None,
            Some(&researcher_token),
            Some(json!({"body_markdown":" "})),
            422,
        )
        .await?;
    assert_violation_path(&invalid, "body/body_markdown");
    world.export()?;
    Ok(())
}
