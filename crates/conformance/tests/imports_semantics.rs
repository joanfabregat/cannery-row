#[path = "support/import_support.rs"]
#[allow(
    dead_code,
    reason = "reuse import bootstrap without duplicating its unrelated history assertions"
)]
mod import_support;
#[path = "support/import_semantics_support.rs"]
mod support;

use conformance::Result;
use import_support::{ATTEMPT, HYPOTHESIS, World, edit};
use reqwest::Method;
use serde_json::{Value, json};
use std::fs;

#[tokio::test]
#[ignore = "requires API/OIDC, real import CLI and read-only storage fixture"]
async fn semantic_import_refusals_roll_back_exact_storage_and_public_state() -> Result<()> {
    let mut world = World::new().await?;
    support::empty_project(&mut world).await?;
    let unverified = support::unverified_user(&mut world).await?;
    let before = support::snapshot(&mut world).await?;
    let (cursor, _) = world.audit(0).await?;
    let slug = world.slug.clone();
    let bundle = world.bundle("dry", &slug)?;
    let dry = world.run(&bundle, &slug, None, &["--dry-run"], 0)?;
    for line in [
        "lexical: 6",
        "dense: 1",
        "awaiting_human_review: 1",
        "imported_artifact: 7",
        "imported_transcribed: 3",
        "measurements with a missing value: 1",
        "dry run: nothing was written",
    ] {
        assert!(dry.stdout.contains(line));
    }
    support::unchanged(&mut world, &before, cursor).await?;
    check_date_errors(&mut world, &before, cursor).await?;
    check_semantic_errors(&mut world, &before, cursor).await?;
    for (label, file, old, new, expected) in [
        (
            "missing-decider",
            "hypotheses/H-002.yaml",
            format!("decided_by: {}", world.ben.email),
            "decided_by: nobody-secret@example.org".into(),
            "hypotheses/H-002.yaml: /decision/decided_by: no user has this verified email",
        ),
        (
            "unverified-decider",
            "hypotheses/H-002.yaml",
            format!("decided_by: {}", world.ben.email),
            format!("decided_by: {unverified}"),
            "hypotheses/H-002.yaml: /decision/decided_by: no user has this verified email",
        ),
        (
            "missing-creator",
            "project.yaml",
            format!("created_by: {}", world.ana.email),
            "created_by: creator-secret@example.org".into(),
            "project.yaml: /created_by: no user has this verified email",
        ),
        (
            "unverified-creator",
            "project.yaml",
            format!("created_by: {}", world.ana.email),
            format!("created_by: {unverified}"),
            "project.yaml: /created_by: no user has this verified email",
        ),
    ] {
        let bundle = world.bundle(label, &slug)?;
        edit(&bundle.join(file), &old, &new)?;
        let refused = world.run(&bundle, &slug, None, &[], 2)?;
        refused.assert_located_problem(expected);
        assert!(!refused.stderr.contains("secret@example.org"));
        assert!(!refused.stderr.contains(&unverified));
        support::unchanged(&mut world, &before, cursor).await?;
    }
    world.run(&bundle, &slug, None, &[], 0)?;
    let imported = support::snapshot(&mut world).await?;
    let (cursor, _) = world.audit(cursor).await?;
    let report = world.bundle("changed-report", &slug)?;
    let path = report.join("reports/H-001/seed-1.md");
    fs::write(
        &path,
        format!("{}\nSecret-ish late edit.\n", fs::read_to_string(&path)?),
    )?;
    let refused = world.run(&report, &slug, None, &[], 2)?;
    refused.assert_located_problem("hypotheses/H-001.yaml: /attempts/0/report: the report file differs from the imported one (updates are not supported)");
    assert!(!refused.stderr.contains("Secret-ish"));
    support::unchanged(&mut world, &imported, cursor).await?;
    check_appended_decider_role(&mut world).await?;
    support::finish(&world, "import-semantics-errors")
}

async fn check_appended_decider_role(world: &mut World) -> Result<()> {
    let slug = world.slug.clone();
    let admin = world.admin.token.clone();
    world
        .api(
            Method::PUT,
            "/api/projects/{slug}/members/{user_id}",
            &format!("{}/members/{}", world.base(), world.ben.id),
            &admin,
            Some(&json!({"role":"member"})),
            200,
        )
        .await?;
    let bundle = world.bundle("appended-role", &slug)?;
    let content =
        fs::read_to_string(bundle.join("hypotheses/H-004.yaml"))?.replace("id: H-004", "id: H-008");
    fs::write(bundle.join("hypotheses/H-008.yaml"), content)?;
    let before = support::snapshot(world).await?;
    let (cursor, _) = world.audit(0).await?;
    let refused = world.run(&bundle, &slug, None, &[], 2)?;
    refused.assert_located_problem(
        "hypotheses/H-008.yaml: /decision/decided_by: not a researcher of the project",
    );
    support::unchanged(world, &before, cursor).await
}

async fn check_date_errors(world: &mut World, before: &Value, cursor: u64) -> Result<()> {
    let slug = world.slug.clone();
    let bundle = world.bundle("inverted", &slug)?;
    let h001 = bundle.join("hypotheses/H-001.yaml");
    edit(
        &h001,
        "evaluated_at: 2025-01-10",
        "evaluated_at: 2025-01-09T15:00:00Z",
    )?;
    edit(&h001, "decided_at: 2025-01-11", "decided_at: 2025-01-08")?;
    edit(
        &bundle.join("hypotheses/H-005.yaml"),
        "decided_at: 2025-02-18",
        "decided_at: 2025-02-16",
    )?;
    let refused = world.run(&bundle, &slug, None, &[], 2)?;
    for problem in [
        "hypotheses/H-001.yaml: /attempts/0/verdict/evaluated_at: before the attempt finished",
        "hypotheses/H-001.yaml: /decision/decided_at: before the verdict it decides on",
        "hypotheses/H-005.yaml: /decision/decided_at: before the hypothesis was created",
    ] {
        refused.assert_located_problem(problem);
    }
    support::unchanged(world, before, cursor).await?;
    for (label, written) in [
        ("early-date", "2025-01-08"),
        ("early-instant", "2025-01-09T15:00:00Z"),
    ] {
        let bundle = world.bundle(label, &slug)?;
        edit(
            &bundle.join("hypotheses/H-001.yaml"),
            "written_at: 2026-09-28",
            &format!("written_at: {written}"),
        )?;
        let refused = world.run(&bundle, &slug, None, &[], 2)?;
        refused.assert_located_problem(
            "hypotheses/H-001.yaml: /attempts/0/report/written_at: before the attempt finished",
        );
        support::unchanged(world, before, cursor).await?;
    }
    Ok(())
}

async fn check_semantic_errors(world: &mut World, before: &Value, cursor: u64) -> Result<()> {
    let slug = world.slug.clone();
    let bundle = world.bundle("semantic", &slug)?;
    edit(
        &bundle.join("hypotheses/H-001.yaml"),
        "- metric: mrr\n        split: dev\n        value: 0.71",
        "- metric: ndcg_secret\n        split: dev\n        value: 0.71",
    )?;
    edit(
        &bundle.join("hypotheses/H-003.yaml"),
        "policy: release-gate",
        "policy: unknown-gate",
    )?;
    edit(
        &bundle.join("hypotheses/H-006.yaml"),
        "h006-seed-1/metrics.json#/mrr/en",
        "h999/metrics.json#/mrr/en",
    )?;
    edit(
        &bundle.join("hypotheses/H-002.yaml"),
        "action: reject",
        "action: promote",
    )?;
    let refused = world.run(&bundle, &slug, None, &[], 2)?;
    for problem in [
        "hypotheses/H-001.yaml: /attempts/0/measurements/0/metric: not a metric of the science revision",
        "hypotheses/H-002.yaml: /decision/action: does not lead to the hypothesis's state",
        "hypotheses/H-003.yaml: /attempts/0/verdict/policy: no policy of the bundle has this id",
        "hypotheses/H-006.yaml: /attempts/0/measurements/1/source: names no artifact of the bundle (URI and SHA-256)",
    ] {
        refused.assert_located_problem(problem);
    }
    assert!(!refused.stderr.contains("ndcg_secret"));
    assert!(!refused.stderr.contains("unknown-gate"));
    assert!(!refused.stderr.contains("h999"));
    support::unchanged(world, before, cursor).await
}

#[tokio::test]
#[ignore = "requires API/OIDC and real import CLI"]
async fn imported_history_accepts_live_draft_and_result_decisions() -> Result<()> {
    for verdict in ["pass", "fail"] {
        let mut world = World::new().await?;
        let slug = world.slug.clone();
        let bundle = world.bundle("boundary", &slug)?;
        if verdict == "fail" {
            edit(
                &bundle.join("hypotheses/H-006.yaml"),
                "result: pass",
                "result: fail",
            )?;
        }
        world.run(&bundle, &slug, Some(&world.science), &[], 0)?;
        let base = world.base();
        let token = world.ana.token.clone();
        let before = world
            .get(HYPOTHESIS, &format!("{base}/hypotheses/7"))
            .await?;
        world
            .api(
                Method::POST,
                "/api/projects/{slug}/hypotheses/{number}/draft-review",
                &format!("{base}/hypotheses/7/draft-review"),
                &token,
                Some(&json!({"draft_revision":1,"action":"approve","reason":"worth running now"})),
                200,
            )
            .await?;
        let draft = world
            .get(HYPOTHESIS, &format!("{base}/hypotheses/7"))
            .await?;
        assert_eq!(draft["state"], "queued");
        assert_eq!(draft["approved_revision"], 1);
        assert_eq!(draft["origin"], "imported");
        assert_eq!(draft["created_at"], before["created_at"]);
        assert_eq!(draft["reviews"][0]["decisions"][0]["origin"], "live");
        decide_result(&mut world, verdict).await?;
        world.audit(0).await?;
        support::finish(&world, &format!("import-boundary-{verdict}"))?;
    }
    Ok(())
}

async fn decide_result(world: &mut World, verdict: &str) -> Result<()> {
    let base = world.base();
    let token = world.ana.token.clone();
    let before = world
        .get(ATTEMPT, &format!("{base}/hypotheses/6/attempts/1"))
        .await?;
    let cases = world
        .get(
            "/api/projects/{slug}/review-cases",
            &format!("{base}/review-cases?kind=result&state=pending"),
        )
        .await?;
    let items = cases["items"].as_array().ok_or("review cases absent")?;
    assert_eq!(items.len(), 1);
    let case = &items[0];
    assert_eq!(case["evaluation"]["assessment"]["verdict"], verdict);
    assert_eq!(case["origin"], "imported");
    let path = format!(
        "{base}/review-cases/{}/decisions",
        import_support::string(&case["id"])?
    );
    let body = json!({"review_case_id":case["id"],"evidence_revision":case["subject_revision"],"reason":"decided in Cannery Row","action":"promote"});
    let refused_snapshot = if verdict == "fail" {
        Some(support::snapshot(world).await?)
    } else {
        None
    };
    let result = world
        .api(
            Method::POST,
            "/api/projects/{slug}/review-cases/{case_id}/decisions",
            &path,
            &token,
            Some(&body),
            if verdict == "pass" { 201 } else { 422 },
        )
        .await?;
    if verdict == "fail" {
        assert_eq!(result["error"]["code"], "validation_failed");
        assert_eq!(result["error"]["details"][0]["path"], "/action");
        assert_eq!(
            support::snapshot(world).await?,
            refused_snapshot.ok_or("refusal snapshot absent")?
        );
        let mut body = body;
        body["action"] = json!("reject");
        world
            .api(
                Method::POST,
                "/api/projects/{slug}/review-cases/{case_id}/decisions",
                &path,
                &token,
                Some(&body),
                201,
            )
            .await?;
    }
    let state = if verdict == "pass" {
        "promoted"
    } else {
        "rejected"
    };
    let hypothesis = world
        .get(HYPOTHESIS, &format!("{base}/hypotheses/6"))
        .await?;
    let attempt = world
        .get(ATTEMPT, &format!("{base}/hypotheses/6/attempts/1"))
        .await?;
    assert_eq!(hypothesis["state"], state);
    assert_eq!(hypothesis["origin"], "imported");
    let reviews = hypothesis["reviews"]
        .as_array()
        .ok_or("hypothesis reviews absent")?;
    let review = reviews
        .iter()
        .find(|review| review["id"] == case["id"])
        .ok_or("decided imported case absent")?;
    assert_eq!(review["origin"], "imported");
    assert_eq!(review["opened_at"], case["opened_at"]);
    assert_eq!(review["decisions"][0]["origin"], "live");
    assert_eq!(review["decisions"][0]["via_channel"], "api");
    assert_eq!(attempt["state"], state);
    assert_eq!(attempt["origin"], "imported");
    for field in ["claimed_at", "started_at", "submitted_at", "finished_at"] {
        assert!(attempt.get(field).is_some());
        assert_eq!(attempt[field], before[field]);
    }
    Ok(())
}
