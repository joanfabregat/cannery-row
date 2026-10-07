//! Public track/draft transitions, revision-bound review and relation visibility.
#![allow(clippy::too_many_arguments, clippy::too_many_lines)]
include!("support/research_state_semantics.rs");

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn track_transition_matrix_and_revision_bound_draft_review() -> Result<()> {
    let mut world = World::new()?;
    let slug = unique()?.replace("conformance", "research-state");
    let admin = world
        .login("conformance-admin", "admin@conformance.test")
        .await?;
    let (token, _) = world
        .token(&admin, "research-state", &["read", "write"])
        .await?;
    let base = project(&mut world, &slug, &admin, &token).await?;
    let owner = agent(&mut world, &base, &admin, &token, "state-owner").await?;
    let other = agent(&mut world, &base, &admin, &token, "state-other").await?;
    track(&mut world, &base, &token, "lifecycle").await?;
    let gate = json!({"id":"latency","metric":"latency_ms","split":"dev","statistic":"value","compare":"control","op":"<=","min_delta":0});
    let invalid = world
        .rest(
            Method::POST,
            "/api/projects/{slug}/tracks",
            &format!("{base}/tracks"),
            &token,
            Some(json!({"slug":"gated","title":"No track gates","gates":[gate]})),
            422,
        )
        .await?;
    assert_violation_path(&invalid, "");
    let invalid = world
        .rest(
            Method::PATCH,
            "/api/projects/{slug}/tracks/{track_slug}",
            &format!("{base}/tracks/lifecycle"),
            &token,
            Some(json!({"expected_revision":1,"gates":[gate],"reason":"Evaluator owns gates"})),
            422,
        )
        .await?;
    if std::env::var("CANNERY_CONFORMANCE_SERVER_IMPLEMENTATION").as_deref() == Ok("rust") {
        assert_eq!(invalid["error"]["code"], "validation_failed");
        assert_eq!(
            invalid["error"]["message"],
            "request does not match the REST contract"
        );
        assert_eq!(invalid["error"]["details"], Value::Null);
    } else {
        assert_violation_path(&invalid, "body/gates");
    }
    for (to, revision, reason, status) in [
        ("paused", 1, "because", 200),
        ("paused", 2, "because", 409),
        ("active", 1, "because", 409),
        ("active", 2, "because", 200),
        ("archived", 3, "because", 200),
        ("paused", 4, "because", 409),
        ("archived", 4, "because", 409),
        ("active", 4, "   ", 422),
        ("active", 4, "because", 200),
    ] {
        let result = move_track(
            &mut world,
            &base,
            "lifecycle",
            &token,
            revision,
            to,
            reason,
            status,
        )
        .await?;
        if status == 200 {
            assert_eq!(result["state"], to);
            assert_eq!(result["revision"], revision + 1);
            assert!(result.get("gates").is_none());
        }
    }
    // Exercise the remaining allowed edge paused -> archived, then its sole exit.
    move_track(
        &mut world,
        &base,
        "lifecycle",
        &token,
        5,
        "paused",
        "because",
        200,
    )
    .await?;
    move_track(
        &mut world,
        &base,
        "lifecycle",
        &token,
        6,
        "archived",
        "because",
        200,
    )
    .await?;
    move_track(
        &mut world,
        &base,
        "lifecycle",
        &token,
        7,
        "active",
        "because",
        200,
    )
    .await?;
    let history = world
        .rest(
            Method::GET,
            "/api/projects/{slug}/tracks/{track_slug}/history",
            &format!("{base}/tracks/lifecycle/history"),
            &token,
            None,
            200,
        )
        .await?;
    let changes = rows(&history)?
        .iter()
        .filter(|event| event["action"] == "track.state_changed")
        .collect::<Vec<_>>();
    let transitions = changes
        .iter()
        .map(|event| {
            Ok((
                string(&event["prior_state"], "state")?,
                string(&event["new_state"], "state")?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(
        transitions,
        vec![
            ("active", "paused"),
            ("paused", "active"),
            ("active", "archived"),
            ("archived", "active"),
            ("active", "paused"),
            ("paused", "archived"),
            ("archived", "active")
        ]
    );
    assert!(
        changes
            .iter()
            .all(|event| event["reason"] == "because" && event["via_channel"] == "api")
    );
    track(&mut world, &base, &token, "producer-binding").await?;
    let binding_path = format!("{base}/tracks/producer-binding");
    let producer = json!({"name":"sparse-producer","revision":1});
    world
        .rest(
            Method::PATCH,
            "/api/projects/{slug}/tracks/{track_slug}",
            &binding_path,
            &token,
            Some(json!({"expected_revision":1,"producer":producer})),
            422,
        )
        .await?;
    let bound = world
        .rest(
            Method::PATCH,
            "/api/projects/{slug}/tracks/{track_slug}",
            &binding_path,
            &token,
            Some(json!({"expected_revision":1,"producer":producer,"reason":"Bind producer"})),
            200,
        )
        .await?;
    assert_eq!(bound["producer"], producer);
    let retitled = world
        .rest(
            Method::PATCH,
            "/api/projects/{slug}/tracks/{track_slug}",
            &binding_path,
            &token,
            Some(json!({"expected_revision":2,"title":"Retitled"})),
            200,
        )
        .await?;
    assert_eq!(retitled["producer"], producer);
    let mut alternate = fixture("tests/fixtures/contracts/step_manifest/valid/producer.json")?;
    alternate["metadata"]["name"] = json!("alternate-producer");
    world
        .rest(
            Method::POST,
            "/api/projects/{slug}/producers",
            &format!("{base}/producers"),
            &token,
            Some(alternate),
            201,
        )
        .await?;
    let replacement = json!({"name":"alternate-producer","revision":1});
    world
        .rest(
            Method::PATCH,
            "/api/projects/{slug}/tracks/{track_slug}",
            &binding_path,
            &token,
            Some(json!({"expected_revision":3,"producer":replacement})),
            422,
        )
        .await?;
    let replaced = world
        .rest(
            Method::PATCH,
            "/api/projects/{slug}/tracks/{track_slug}",
            &binding_path,
            &token,
            Some(json!({"expected_revision":3,"producer":replacement,"reason":"Replace producer"})),
            200,
        )
        .await?;
    assert_eq!(replaced["producer"], replacement);
    world
        .rest(
            Method::PATCH,
            "/api/projects/{slug}/tracks/{track_slug}",
            &binding_path,
            &token,
            Some(json!({"expected_revision":4,"producer":null})),
            422,
        )
        .await?;
    let removed = world
        .rest(
            Method::PATCH,
            "/api/projects/{slug}/tracks/{track_slug}",
            &binding_path,
            &token,
            Some(json!({"expected_revision":4,"producer":null,"reason":"Use project default"})),
            200,
        )
        .await?;
    assert!(removed["producer"].is_null());
    let history = world
        .rest(
            Method::GET,
            "/api/projects/{slug}/tracks/{track_slug}/history",
            &format!("{binding_path}/history"),
            &token,
            None,
            200,
        )
        .await?;
    let reasons = rows(&history)?
        .iter()
        .filter(|event| event["action"] == "track.updated")
        .map(|event| event["reason"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        reasons,
        vec![
            json!("Bind producer"),
            Value::Null,
            json!("Replace producer"),
            json!("Use project default")
        ]
    );
    for name in ["busy", "archived", "paused"] {
        track(&mut world, &base, &token, name).await?;
    }
    for (field, value, path) in [
        ("missing_fields", Value::Null, "/project_fields"),
        (
            "metric",
            json!("unregistered-metric"),
            "/plan/primary_metric",
        ),
        ("split", json!(["train"]), "/plan/confirmation_splits/0"),
        ("control", json!("unknown-baseline-revision"), "/control"),
        ("track", json!("missing-track"), "/track"),
        ("unknown", json!(1), ""),
    ] {
        let mut invalid = document("busy", "Invalid references create no draft")?;
        match field {
            "missing_fields" => {
                invalid
                    .as_object_mut()
                    .ok_or("draft document absent")?
                    .remove("project_fields");
            }
            "metric" => invalid["plan"]["primary_metric"] = value,
            "split" => invalid["plan"]["confirmation_splits"] = value,
            "control" => invalid["control"]["revision"] = value,
            "track" => invalid["track"] = value,
            "unknown" => invalid["priority"] = value,
            _ => return Err("unknown reference scenario".into()),
        }
        let error = world
            .rest(
                Method::POST,
                "/api/projects/{slug}/hypotheses",
                &format!("{base}/hypotheses"),
                &token,
                Some(invalid),
                422,
            )
            .await?;
        assert_violation_path(&error, path);
    }
    let empty = world
        .rest(
            Method::GET,
            "/api/projects/{slug}/hypotheses",
            &format!("{base}/hypotheses"),
            &token,
            None,
            200,
        )
        .await?;
    assert!(
        rows(&empty)?.is_empty(),
        "refused reference checks must create no hypotheses"
    );
    let created = create(&mut world, &base, &owner, document("busy", "Owner draft")?).await?;
    let n = number(&created)?;
    move_track(
        &mut world,
        &base,
        "busy",
        &token,
        1,
        "archived",
        "Open draft blocks archive",
        409,
    )
    .await?;
    move_track(
        &mut world,
        &base,
        "archived",
        &token,
        1,
        "archived",
        "Quiet track archived",
        200,
    )
    .await?;
    world
        .rest(
            Method::POST,
            "/api/projects/{slug}/hypotheses",
            &format!("{base}/hypotheses"),
            &token,
            Some(document("archived", "Refused draft")?),
            409,
        )
        .await?;
    world
        .rest(
            Method::PATCH,
            "/api/projects/{slug}/tracks/{track_slug}",
            &format!("{base}/tracks/archived"),
            &token,
            Some(json!({"expected_revision":2,"title":"Refused edit"})),
            409,
        )
        .await?;
    revise(
        &mut world,
        &base,
        n,
        &other,
        1,
        document("busy", "Wrong agent")?,
        403,
    )
    .await?;
    let revised = revise(
        &mut world,
        &base,
        n,
        &owner,
        1,
        document("busy", "Sharper r2")?,
        200,
    )
    .await?;
    assert_eq!(revised["revision"], 2);
    let stale = review(&mut world, &base, n, &token, 1, "approve", 409).await?;
    assert_code(&stale, "stale_revision");
    let approved = review(&mut world, &base, n, &token, 2, "approve", 200).await?;
    assert_eq!(approved["approved_revision"], 2);
    assert_eq!(approved["state"], "queued");
    assert!(!approved["approved_at"].is_null());
    review(&mut world, &base, n, &token, 2, "decline", 409).await?;
    revise(
        &mut world,
        &base,
        n,
        &token,
        2,
        document("busy", "Cannot edit queued")?,
        409,
    )
    .await?;
    let r1 = world
        .rest(
            Method::GET,
            "/api/projects/{slug}/hypotheses/{number}/revisions/{revision}",
            &format!("{base}/hypotheses/{n}/revisions/1"),
            &token,
            None,
            200,
        )
        .await?;
    assert_eq!(r1["document"], created["document"]);
    move_track(
        &mut world,
        &base,
        "paused",
        &token,
        1,
        "paused",
        "Wait for resources",
        200,
    )
    .await?;
    let paused = create(
        &mut world,
        &base,
        &token,
        document("paused", "Paused can review")?,
    )
    .await?;
    let paused = review(
        &mut world,
        &base,
        number(&paused)?,
        &token,
        1,
        "approve",
        200,
    )
    .await?;
    assert_eq!(paused["state"], "queued");
    let requested = create(
        &mut world,
        &base,
        &owner,
        document("busy", "Revision requested")?,
    )
    .await?;
    let n = number(&requested)?;
    let review_path = format!("{base}/hypotheses/{n}/draft-review");
    let template = "/api/projects/{slug}/hypotheses/{number}/draft-review";
    let body = review_body(1, "request_revision");
    let first = world
        .keyed(
            template,
            &review_path,
            &token,
            body.clone(),
            "request-once",
            200,
        )
        .await?;
    assert_eq!(first["state"], "draft");
    assert_eq!(first["reviews"][0]["state"], "resolved");
    review(&mut world, &base, n, &token, 1, "approve", 409).await?;
    let edited = revise(
        &mut world,
        &base,
        n,
        &owner,
        1,
        document("busy", "Required changes r2")?,
        200,
    )
    .await?;
    assert_eq!(edited["revision"], 2);
    let edited = revise(
        &mut world,
        &base,
        n,
        &token,
        2,
        document("busy", "Researcher revision r3")?,
        200,
    )
    .await?;
    let cases = edited["reviews"].as_array().ok_or("draft reviews absent")?;
    assert_eq!(
        cases
            .iter()
            .map(|case| (case["state"].clone(), case["subject_revision"].clone()))
            .collect::<Vec<_>>(),
        vec![(json!("resolved"), json!(1)), (json!("pending"), json!(3))]
    );
    let replay = world
        .keyed(
            template,
            &review_path,
            &token,
            body.clone(),
            "request-once",
            200,
        )
        .await?;
    // Production replay returns current detail, preserving the original decision once.
    assert_eq!(replay, edited);
    assert_ne!(replay, first);
    assert_eq!(replay["revision"], 3);
    let wrong = world
        .keyed(
            template,
            &review_path,
            &token,
            review_body(1, "decline"),
            "request-once",
            409,
        )
        .await?;
    assert_code(&wrong, "conflict");
    let approved = world
        .keyed(
            template,
            &review_path,
            &token,
            review_body(3, "approve"),
            "approve-once",
            200,
        )
        .await?;
    assert_eq!(approved["approved_revision"], 3);
    let replay = world
        .keyed(template, &review_path, &token, body, "request-once", 200)
        .await?;
    assert_eq!(replay, approved);
    let decision_actions = approved["reviews"]
        .as_array()
        .ok_or("reviews absent")?
        .iter()
        .flat_map(|case| case["decisions"].as_array().into_iter().flatten())
        .map(|decision| decision["action"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        decision_actions,
        vec![json!("request_revision"), json!("approve")]
    );
    let declined = create(&mut world, &base, &owner, document("busy", "Out of scope")?).await?;
    let declined = review(
        &mut world,
        &base,
        number(&declined)?,
        &token,
        1,
        "decline",
        200,
    )
    .await?;
    assert_eq!(declined["state"], "declined");
    let audit = world.harness.fetch_audit(&token, 0).await?;
    let events = rows(&audit.body)?;
    for action in ["hypothesis.revision_requested", "hypothesis.draft_approved"] {
        assert_eq!(
            events
                .iter()
                .filter(|event| event["subject_id"] == requested["id"] && event["action"] == action)
                .count(),
            1,
            "key replay must not duplicate {action}"
        );
    }
    world.export()?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires the conformance server and OIDC provider"]
async fn cross_project_relations_mentions_pages_and_editor_preservation() -> Result<()> {
    let mut world = World::new()?;
    let run = unique()?.replace("conformance", "research-relations");
    let a = format!("{run}-a");
    let b = format!("{run}-b");
    let admin = world
        .login("conformance-admin", "admin@conformance.test")
        .await?;
    let (token, _) = world
        .token(&admin, "relation-state", &["read", "write"])
        .await?;
    let alpha = project(&mut world, &a, &admin, &token).await?;
    let beta = project(&mut world, &b, &admin, &token).await?;
    for name in ["main", "other"] {
        track(&mut world, &alpha, &token, name).await?;
    }
    track(&mut world, &beta, &token, "main").await?;
    let editor = world
        .login(
            &format!("{run}-editor"),
            &format!("{run}-editor@conformance.test"),
        )
        .await?;
    member(&mut world, &alpha, &token, &editor.id, "researcher").await?;
    let (editor_token, _) = world
        .token(&editor, "alpha-only", &["read", "write"])
        .await?;
    let viewer = world
        .login(
            &format!("{run}-viewer"),
            &format!("{run}-viewer@conformance.test"),
        )
        .await?;
    member(&mut world, &alpha, &token, &viewer.id, "viewer").await?;
    let (viewer_token, _) = world.token(&viewer, "alpha-viewer", &["read"]).await?;
    let origin = create(&mut world, &alpha, &token, document("main", "Origin")?).await?;
    assert_eq!(number(&origin)?, 1);
    for (track, title) in [
        ("other", "URL anchor target"),
        ("main", "C sharp target"),
        ("other", "Double hash target"),
    ] {
        create(&mut world, &alpha, &token, document(track, title)?).await?;
    }
    let beta_origin = create(&mut world, &beta, &token, document("main", "Beta origin")?).await?;
    assert_eq!(number(&beta_origin)?, 1);
    let relation = json!({"kind":"related_to","hypothesis":{"project":b,"number":1}});
    let mut derived = document("main", "Derived")?;
    derived["rationale"] = json!(format!(
        "See #1 and {b}#1, (#1). Not https://x.test/a#2, C#3, ##4 or #0."
    ));
    derived["relations"] = json!([{"kind":"derived_from","hypothesis":1},relation]);
    let derived_created = create(&mut world, &alpha, &token, derived.clone()).await?;
    assert_eq!(number(&derived_created)?, 5);
    assert_eq!(
        derived_created["relations"]
            .as_array()
            .ok_or("relations absent")?
            .iter()
            .map(|item| item["ref"].clone())
            .collect::<Vec<_>>(),
        vec![json!(format!("{a}#1")), json!(format!("{b}#1"))]
    );
    let mut follow = document("main", "Beta follows alpha")?;
    follow["rationale"] = json!(format!("Builds on {a}#1."));
    create(&mut world, &beta, &token, follow).await?;
    let origin = detail(&mut world, &alpha, 1, &token).await?;
    let visible = origin["backlinks"]
        .as_array()
        .ok_or("backlinks absent")?
        .iter()
        .map(|item| {
            Ok((
                string(item, "kind")?.to_owned(),
                string(item, "ref")?.to_owned(),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(
        visible,
        vec![
            ("derived_from".into(), format!("{a}#5")),
            ("mention".into(), format!("{a}#5")),
            ("mention".into(), format!("{b}#2"))
        ]
    );
    let restricted = detail(&mut world, &alpha, 1, &viewer_token).await?;
    assert_eq!(
        restricted["backlinks"]
            .as_array()
            .ok_or("restricted backlinks absent")?
            .len(),
        2
    );
    assert!(
        restricted["backlinks"]
            .as_array()
            .ok_or("restricted backlinks absent")?
            .iter()
            .all(|item| item["project"] == a)
    );
    for n in [2, 3, 4] {
        let false_anchor = detail(&mut world, &alpha, n, &token).await?;
        assert_eq!(
            false_anchor["backlinks"],
            json!([]),
            "URL/C#/## syntax must not create mentions"
        );
    }
    derived["title"] = json!("Edited without beta access");
    let kept = revise(
        &mut world,
        &alpha,
        5,
        &editor_token,
        1,
        derived.clone(),
        200,
    )
    .await?;
    assert_eq!(kept["revision"], 2);
    derived["relations"]
        .as_array_mut()
        .ok_or("document relations absent")?
        .push(json!({"kind":"related_to","hypothesis":{"project":b,"number":2}}));
    revise(&mut world, &alpha, 5, &editor_token, 2, derived, 422).await?;
    let full = detail(&mut world, &alpha, 5, &token).await?;
    assert_eq!(full["revision"], 2);
    assert_eq!(full["relations"], derived_created["relations"]);
    let mut unreadable = document("main", "No new unreadable relation")?;
    unreadable["relations"] = json!([relation]);
    world
        .rest(
            Method::POST,
            "/api/projects/{slug}/hypotheses",
            &format!("{alpha}/hypotheses"),
            &editor_token,
            Some(unreadable),
            422,
        )
        .await?;
    let mut huge = document("main", "Out-of-range relation")?;
    huge["relations"] = json!([{"kind":"related_to","hypothesis":3_000_000_000_u64}]);
    world
        .rest(
            Method::POST,
            "/api/projects/{slug}/hypotheses",
            &format!("{alpha}/hypotheses"),
            &token,
            Some(huge),
            422,
        )
        .await?;
    world
        .rest(
            Method::GET,
            "/api/projects/{slug}/hypotheses",
            &format!("{alpha}/hypotheses?before=3000000000"),
            &token,
            None,
            422,
        )
        .await?;
    world
        .rest(
            Method::GET,
            "/api/projects/{slug}/hypotheses/{number}",
            &format!("{alpha}/hypotheses/3000000000"),
            &token,
            None,
            404,
        )
        .await?;
    review(&mut world, &alpha, 4, &token, 1, "decline", 200).await?;
    let list_template = "/api/projects/{slug}/hypotheses";
    let page = world
        .rest(
            Method::GET,
            list_template,
            &format!("{alpha}/hypotheses?limit=2"),
            &viewer_token,
            None,
            200,
        )
        .await?;
    assert_eq!(nums(&page)?, vec![5, 4]);
    assert_eq!(page["next_before"], 4);
    let page = world
        .rest(
            Method::GET,
            list_template,
            &format!("{alpha}/hypotheses?limit=2&before=4"),
            &viewer_token,
            None,
            200,
        )
        .await?;
    assert_eq!(nums(&page)?, vec![3, 2]);
    assert_eq!(page["next_before"], 2);
    let page = world
        .rest(
            Method::GET,
            list_template,
            &format!("{alpha}/hypotheses?limit=2&before=2"),
            &viewer_token,
            None,
            200,
        )
        .await?;
    assert_eq!(nums(&page)?, vec![1]);
    assert!(page["next_before"].is_null());
    for (query, expected) in [
        ("track=other", vec![4, 2]),
        ("archived=false", vec![5, 3, 2, 1]),
        ("archived=true", vec![4]),
        ("state=declined", vec![4]),
    ] {
        let listed = world
            .rest(
                Method::GET,
                list_template,
                &format!("{alpha}/hypotheses?{query}"),
                &viewer_token,
                None,
                200,
            )
            .await?;
        assert_eq!(nums(&listed)?, expected);
    }
    let doc = document("main", "Idempotent per actor")?;
    let first = world
        .keyed(
            list_template,
            &format!("{alpha}/hypotheses"),
            &token,
            doc.clone(),
            "same-across-projects",
            201,
        )
        .await?;
    let replay = world
        .keyed(
            list_template,
            &format!("{alpha}/hypotheses"),
            &token,
            doc.clone(),
            "same-across-projects",
            200,
        )
        .await?;
    assert_eq!(first, replay);
    world
        .keyed(
            list_template,
            &format!("{beta}/hypotheses"),
            &token,
            doc.clone(),
            "same-across-projects",
            409,
        )
        .await?;
    let second = world
        .keyed(
            list_template,
            &format!("{alpha}/hypotheses"),
            &editor_token,
            doc,
            "same-across-projects",
            201,
        )
        .await?;
    assert_eq!(number(&second)?, number(&first)? + 1);
    world.harness.fetch_audit(&token, 0).await?;
    // Distinct export names preserve both scenario reports in one suite invocation.
    if let Ok(directory) = std::env::var("CANNERY_CONFORMANCE_COVERAGE_DIR") {
        fs::write(
            PathBuf::from(directory).join("research-relations.json"),
            serde_json::to_vec_pretty(world.harness.coverage())?,
        )?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the conformance server and local CLI workers"]
async fn result_review_gate_constraints_and_keyed_correction_replay() -> Result<()> {
    let coverage = result_review::exercise().await.map_err(without_url)?;
    if let Ok(directory) = std::env::var("CANNERY_CONFORMANCE_COVERAGE_DIR") {
        fs::write(
            PathBuf::from(directory).join("research-result-review.json"),
            serde_json::to_vec_pretty(&coverage)?,
        )?;
    }
    Ok(())
}
