#[path = "support/import_support.rs"]
mod import_support;

use conformance::Result;
use import_support::{ATTEMPT, CONFIG, HYPOTHESIS, PROJECT, REPORT, World, edit, string};
use reqwest::Method;
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::Path};

#[tokio::test]
#[ignore = "requires URL-driven API, OIDC, database and actual CLI fixtures"]
async fn cli_import_history_transactions_and_permissions() -> Result<()> {
    let mut world = World::new().await?;
    let slug = world.slug.clone();
    let bundle = world.bundle("original", &slug)?;
    let creator = world.ana.read_token.clone();
    let (before, _) = world.audit(0).await?;
    let refused = world.run(&bundle, &slug, None, &[], 2)?;
    refused.assert_problem(
        "the project has no science revision",
        "--science",
        "",
        "the project has no science revision; give one",
    );
    let dry = world.run(&bundle, &slug, Some(&world.science), &["--dry-run"], 0)?;
    assert!(dry.stdout.contains("would import"));
    assert!(dry.stdout.contains("dry run: nothing was written"));
    world
        .api(Method::GET, PROJECT, &world.base(), &creator, None, 404)
        .await?;
    let (_, rolled_back) = world.audit(before).await?;
    assert!(
        !rolled_back
            .iter()
            .any(|row| row["new_state"]["slug"] == slug)
    );

    let imported = world.run(&bundle, &slug, Some(&world.science), &[], 0)?;
    assert!(
        imported
            .stdout
            .contains("1 policies, 2 tracks, 7 hypotheses, 6 attempts, 5 decisions, 1 reports")
    );
    assert!(imported.stdout.contains("imported_artifact: 7"));
    assert!(imported.stdout.contains("imported_transcribed: 3"));
    let project = world.get(PROJECT, &world.base()).await?;
    let project_id = project["id"].clone();
    let (cursor, rows) = world.audit(before).await?;
    check_import_audit(&rows, &project_id, &imported.stdout)?;
    check_history(&mut world, &bundle).await?;
    check_scope(&mut world).await?;
    check_replays(&mut world, &bundle, &project_id, cursor).await?;
    check_existing_project(&mut world).await?;
    world.finish()?;
    Ok(())
}

fn check_import_audit(rows: &[Value], project_id: &Value, output: &str) -> Result<()> {
    let events: Vec<_> = rows
        .iter()
        .filter(|row| {
            row["project_id"] == *project_id
                && row["action"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("import."))
        })
        .collect();
    assert_eq!(events.len(), 15);
    let actions: BTreeSet<_> = events
        .iter()
        .map(|row| row["action"].as_str().ok_or("audit action absent"))
        .collect::<std::result::Result<_, _>>()?;
    assert_eq!(
        actions,
        BTreeSet::from([
            "import.project_created",
            "import.membership",
            "import.science_registered",
            "import.policy",
            "import.track",
            "import.hypothesis",
            "import.completed"
        ])
    );
    let digest = output
        .split_whitespace()
        .nth(1)
        .ok_or("CLI bundle digest missing")?
        .strip_suffix(':')
        .ok_or("CLI bundle digest separator missing")?;
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    for (action, count) in [
        ("import.membership", 2),
        ("import.track", 2),
        ("import.hypothesis", 7),
    ] {
        assert_eq!(
            events
                .iter()
                .filter(|event| event["action"] == action)
                .count(),
            count
        );
    }
    let completed = events
        .iter()
        .find(|event| event["action"] == "import.completed")
        .ok_or("completion event absent")?;
    for (field, value) in [
        ("policies", 1),
        ("tracks", 2),
        ("hypotheses", 7),
        ("attempts", 6),
        ("decisions", 5),
        ("reports", 1),
    ] {
        assert_eq!(completed["new_state"][field], value);
    }
    for event in events {
        assert_eq!(event["actor_kind"], "system");
        assert!(event["actor_user_id"].is_null());
        assert!(event["actor_service_id"].is_null());
        assert_eq!(event["via_channel"], "cli");
        assert_eq!(event["via_client"], "cannery import");
        assert_eq!(event["new_state"]["bundle_sha256"], digest);
        // Audit timestamps describe the import, while the read models below retain 2025 dates.
        assert!(string(&event["occurred_at"])?.as_str() > "2026-01-01");
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "Check the complete imported HTTP read model together"
)]
async fn check_history(world: &mut World, bundle: &Path) -> Result<()> {
    let base = world.base();
    let member = world.ana.token.clone();
    let memberships = world
        .get("/api/projects/{slug}/members", &format!("{base}/members"))
        .await?;
    let members = memberships["items"].as_array().ok_or("members missing")?;
    assert_eq!(members.len(), 2);
    for identity in [&world.ana, &world.ben] {
        assert!(
            members
                .iter()
                .any(|row| row["user_id"] == identity.id && row["role"] == "researcher")
        );
    }
    let science = world
        .get(
            "/api/projects/{slug}/config/{kind}/latest",
            &format!("{base}/config/science/latest"),
        )
        .await?;
    assert_eq!(science["revision"], 1);
    assert_eq!(science["created_by"], world.ana.id);
    assert_eq!(
        science["content"],
        serde_json::from_slice::<Value>(&fs::read(&world.science)?)?
    );
    let hypotheses = world
        .get(
            "/api/projects/{slug}/hypotheses",
            &format!("{base}/hypotheses?limit=100"),
        )
        .await?;
    let states = [
        "promoted",
        "rejected",
        "inconclusive",
        "failed",
        "declined",
        "awaiting_human_review",
        "draft",
    ];
    assert_eq!(
        hypotheses["items"]
            .as_array()
            .ok_or("hypotheses missing")?
            .len(),
        7
    );
    for (offset, state) in states.iter().enumerate() {
        let number = offset + 1;
        let detail = world
            .get(HYPOTHESIS, &format!("{base}/hypotheses/{number}"))
            .await?;
        assert_eq!(detail["state"], *state);
        assert_eq!(detail["origin"], "imported");
        assert_eq!(detail["external_id"], format!("H-{number:03}"));
        assert_eq!(
            detail["source_ref"],
            format!("hypotheses/H-{number:03}.yaml")
        );
        assert!(detail["document"].get("imported").is_none());
        if number == 1 {
            assert!(string(&detail["created_at"])?.starts_with("2025-01-08"));
            assert_eq!(
                detail["imported"]["claim"],
                "Lowering k1 to 0.9 raises dev MRR over the base camp on both languages."
            );
            assert_eq!(
                detail["imported"]["control"],
                json!({"baseline":"base-camp"})
            );
            assert_eq!(detail["reviews"][0]["origin"], "imported");
            let decision = &detail["reviews"][0]["decisions"][0];
            assert_eq!(decision["origin"], "imported");
            assert_eq!(decision["actor_user_id"], world.ana.id);
            assert_eq!(decision["via_channel"], "cli");
            assert_eq!(decision["via_client"], "cannery import");
            assert_eq!(decision["source_ref"], "notebook/2025-01.md:60@3f9c2ab");
            assert!(string(&decision["decided_at"])?.starts_with("2025-01-11"));
        }
    }
    let attempts = world
        .get(
            "/api/projects/{slug}/hypotheses/{number}/attempts",
            &format!("{base}/hypotheses/2/attempts"),
        )
        .await?;
    assert_eq!(
        attempts["items"]
            .as_array()
            .ok_or("attempts missing")?
            .iter()
            .map(|row| (&row["state"], &row["origin"]))
            .collect::<Vec<_>>(),
        vec![
            (&json!("failed"), &json!("imported")),
            (&json!("rejected"), &json!("imported"))
        ]
    );
    let attempt = world
        .api(
            Method::GET,
            ATTEMPT,
            &format!("{base}/hypotheses/1/attempts/1"),
            &member,
            None,
            200,
        )
        .await?;
    assert_eq!(
        attempt["imported"],
        json!({"label":"seed-1","config":"configs/bm25-k1-0.9.yaml","source_revision":"8d1e0f4"})
    );
    assert!(string(&attempt["started_at"])?.starts_with("2025-01-09T14:00:00"));
    assert!(string(&attempt["finished_at"])?.starts_with("2025-01-09T15:12:00"));
    let report = world
        .get(REPORT, &format!("{base}/hypotheses/1/attempts/1/report"))
        .await?;
    assert_eq!(report["origin"], "imported");
    assert_eq!(report["author"], json!({"kind":"import","id":null}));
    assert_eq!(
        report["report"],
        json!({"body_markdown":fs::read_to_string(bundle.join("reports/H-001/seed-1.md"))?,"kind":"retrospective","author":format!("A research agent, from the notebook and the run metrics; reviewed by {}",world.ana.email),"written_at":"2026-09-28","origin":"imported","source_ref":"reports/H-001/seed-1.md"})
    );
    assert_eq!(report["evaluation"]["verdict"], "pass");
    assert_eq!(report["evaluation"]["policy_revision"], "release-gate@v1");
    assert!(
        report["tester"]["measurements"]
            .as_array()
            .ok_or("report measurements missing")?
            .iter()
            .all(|row| row["authority"] == "imported_artifact")
    );
    assert_eq!(
        world
            .get(REPORT, &format!("{base}/hypotheses/2/attempts/2/report"))
            .await?["report"],
        json!({})
    );
    assert_eq!(
        world
            .get("/api/projects/{slug}/reports", &format!("{base}/reports"))
            .await?["items"],
        json!([])
    );
    let artifact = &attempt["artifacts"][0];
    assert_eq!(artifact["origin"], "imported");
    assert_eq!(artifact["storage"]["backend"], "external");
    assert_eq!(
        artifact["uri"],
        "gs://retrieval-history/runs/h001-seed-1/metrics.json"
    );
    world
        .api(
            Method::GET,
            "/api/projects/{slug}/artifacts/{artifact_id}",
            &format!("{base}/artifacts/{}", string(&artifact["id"])?),
            &member,
            None,
            409,
        )
        .await?;
    let verified = world
        .get(
            "/api/projects/{slug}/metrics/query",
            &format!("{base}/metrics/query?metric=mrr"),
        )
        .await?;
    assert_eq!(verified["items"], json!([]));
    assert_eq!(verified["context"]["failed_attempts"], 0);
    let imported = world
        .get(
            "/api/projects/{slug}/metrics/query",
            &format!("{base}/metrics/query?metric=mrr&authority=imported&all_slices=true"),
        )
        .await?;
    let points = imported["items"].as_array().ok_or("measurements missing")?;
    assert_eq!(points.len(), 10);
    assert_eq!(imported["context"]["failed_attempts"], 2);
    let point = points
        .iter()
        .find(|row| row["attempt_ref"] == "#1.1" && row["dimensions"] == json!({}))
        .ok_or("overall historical measurement missing")?;
    assert_eq!(point["authority"], "imported_artifact");
    assert_eq!(point["value"], 0.71);
    assert_eq!(point["reference"]["value"], 0.68);
    assert!(string(&point["source_ref"])?.ends_with("metrics.json#/mrr/overall"));
    assert!(string(&point["recorded_at"])?.starts_with("2025-01-09T15:12:00"));
    for origin in ["live", "imported", "all"] {
        let comparisons = world
            .get(
                "/api/projects/{slug}/comparisons",
                &format!("{base}/comparisons?origin={origin}"),
            )
            .await?;
        let items = comparisons["items"]
            .as_array()
            .ok_or("comparisons missing")?;
        assert_eq!(items.len(), usize::from(origin != "live"));
        if let Some(item) = items.first() {
            assert_eq!(item["origin"], "imported");
            assert_eq!(item["policy_revision"], "release-gate@v1");
        }
    }
    for track in ["dense", "lexical"] {
        let history = world
            .get(
                "/api/projects/{slug}/tracks/{track_slug}/history",
                &format!("{base}/tracks/{track}/history"),
            )
            .await?;
        let event = history["items"]
            .as_array()
            .ok_or("track history absent")?
            .iter()
            .find(|row| row["action"] == "import.track")
            .ok_or("import track history missing")?;
        assert_eq!(event["actor_kind"], "system");
        assert_eq!(event["via_channel"], "cli");
        assert_eq!(event["via_client"], "cannery import");
        let detail = world
            .get(
                "/api/projects/{slug}/tracks/{track_slug}",
                &format!("{base}/tracks/{track}"),
            )
            .await?;
        assert_eq!(detail["mode"], "agent");
        assert_eq!(detail["revision"], 1);
        if track == "lexical" {
            assert_eq!(detail["description"], "BM25 and its tuning.");
            assert!(string(&detail["created_at"])?.starts_with("2025-01-06"));
        }
    }
    let attention = world
        .get(
            "/api/projects/{slug}/attention",
            &format!("{base}/attention"),
        )
        .await?;
    for section in ["pending_reviews", "recent_outcomes", "recent_failures"] {
        let entries = attention[section]
            .as_array()
            .ok_or("attention section missing")?;
        assert_ne!(entries.as_slice(), &[] as &[Value]);
        assert!(entries.iter().all(|row| row["origin"] == "imported"));
    }
    let search = world
        .get(
            "/api/search",
            &format!("/api/search?q={}%232.1", world.slug),
        )
        .await?;
    let matches = search["items"].as_array().ok_or("search items missing")?;
    assert_ne!(matches.as_slice(), &[] as &[Value]);
    assert!(matches.iter().all(|row| row["origin"] == "imported"));
    Ok(())
}

async fn check_scope(world: &mut World) -> Result<()> {
    let base = world.base();
    let science: Value = serde_json::from_slice(&fs::read(&world.science)?)?;
    // A global admin's read-only PAT and a researcher's write PAT both lack config authority.
    for token in [world.admin.read_token.clone(), world.ana.token.clone()] {
        world
            .api(
                Method::POST,
                CONFIG,
                &format!("{base}/config/science"),
                &token,
                Some(&science),
                403,
            )
            .await?;
    }
    let read = world.ana.read_token.clone();
    world
        .api(
            Method::GET,
            "/api/projects/{slug}/config/{kind}/{revision}",
            &format!("{base}/config/science/1"),
            &read,
            None,
            200,
        )
        .await?;
    assert_eq!(
        world.get(CONFIG, &format!("{base}/config/science")).await?["items"]
            .as_array()
            .ok_or("config revisions missing")?
            .len(),
        1
    );
    Ok(())
}

async fn check_replays(
    world: &mut World,
    bundle: &Path,
    project_id: &Value,
    cursor: u64,
) -> Result<()> {
    let slug = world.slug.clone();
    let snapshot = world.snapshot().await?;
    let replay = world.run(bundle, &slug, Some(&world.science), &[], 0)?;
    assert!(
        replay
            .stdout
            .contains("already imported, nothing to do (11 unchanged entries)")
    );
    world.unchanged(project_id, cursor, &snapshot).await?;
    let changed = world.bundle("changed", &slug)?;
    edit(
        &changed.join("hypotheses/H-003.yaml"),
        "Not enough measured to decide",
        "Private changed wording",
    )?;
    edit(
        &changed.join("tracks/lexical.yaml"),
        "BM25 and its tuning.",
        "BM25.",
    )?;
    let refused = world.run(&changed, &slug, None, &[], 2)?;
    refused.assert_problem(
        "tracks/lexical.yaml: /description:",
        "tracks/lexical.yaml",
        "/description",
        "differs from the immutable imported entry",
    );
    refused.assert_problem(
        "hypotheses/H-003.yaml: /decision/reason:",
        "hypotheses/H-003.yaml",
        "/decision/reason",
        "differs from the immutable imported entry",
    );
    assert!(!refused.stderr.contains("Private changed wording"));
    world.unchanged(project_id, cursor, &snapshot).await?;
    let missing = world.bundle("missing", &slug)?;
    fs::remove_file(missing.join("hypotheses/H-005.yaml"))?;
    fs::remove_file(missing.join("hypotheses/H-007.yaml"))?;
    let refused = world.run(&missing, &slug, None, &[], 2)?;
    for key in ["hypothesis H-005", "hypothesis H-007"] {
        refused.assert_problem(
            key,
            key,
            "",
            "imported previously and missing; allow_missing keeps it",
        );
    }
    world.unchanged(project_id, cursor, &snapshot).await?;
    let allowed = world.run(&missing, &slug, None, &["--allow-missing"], 0)?;
    assert!(
        allowed
            .stdout
            .contains("missing from the bundle, kept as imported: hypothesis H-005")
    );
    assert!(
        allowed
            .stdout
            .contains("missing from the bundle, kept as imported: hypothesis H-007")
    );
    world.unchanged(project_id, cursor, &snapshot).await?;
    let changed_science = world.directory.join("changed-science.json");
    let mut science: Value = serde_json::from_slice(&fs::read(&world.science)?)?;
    science["baselines"][0]["description"] = json!("Another description.");
    fs::write(&changed_science, serde_json::to_vec(&science)?)?;
    let refused = world.run(bundle, &slug, Some(&changed_science), &[], 2)?;
    refused.assert_problem(
        "already has science revision 1",
        "--science",
        "",
        "differs from the current revision; register changes through the API",
    );
    world.unchanged(project_id, cursor, &snapshot).await?;
    let appended = world.bundle("appended", &slug)?;
    let content = fs::read_to_string(appended.join("hypotheses/H-005.yaml"))?
        .replace("id: H-005", "id: H-008")
        .replace("created_at: 2025-02-17", "created_at: 2025-04-01")
        .replace("decided_at: 2025-02-18", "decided_at: 2025-04-02");
    fs::write(appended.join("hypotheses/H-008.yaml"), content)?;
    let imported = world.run(&appended, &slug, None, &[], 0)?;
    assert!(imported.stdout.contains("1 hypotheses"));
    assert!(imported.stdout.contains("11 entries unchanged"));
    let hyp = world
        .get(HYPOTHESIS, &format!("{}/hypotheses/8", world.base()))
        .await?;
    assert_eq!(hyp["external_id"], "H-008");
    assert_eq!(hyp["state"], "declined");
    assert!(string(&hyp["created_at"])?.starts_with("2025-04-01"));
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "Keep refusal and successful retry on the same pre-existing project together"
)]
async fn check_existing_project(world: &mut World) -> Result<()> {
    let slug = format!("{}-existing", world.slug);
    let base = format!("/api/projects/{slug}");
    let admin = world.admin.token.clone();
    let science: Value = serde_json::from_slice(&fs::read(&world.science)?)?;
    world.api(Method::POST, "/api/projects", "/api/projects", &admin, Some(&json!({"slug":slug,"title":"Keep existing title","description":"Keep existing description"})), 201).await?;
    for (id, role) in [
        (world.ana.id.clone(), "researcher"),
        (world.ben.id.clone(), "member"),
    ] {
        world
            .api(
                Method::PUT,
                "/api/projects/{slug}/members/{user_id}",
                &format!("{base}/members/{id}"),
                &admin,
                Some(&json!({"role":role})),
                200,
            )
            .await?;
    }
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
    let bundle = world.bundle("existing", &slug)?;
    let (cursor, _) = world.audit(0).await?;
    let refused = world.run(&bundle, &slug, None, &[], 2)?;
    refused.assert_located_problem(
        "hypotheses/H-002.yaml: /decision/decided_by: not a researcher of the project",
    );
    assert_eq!(
        world
            .get(
                "/api/projects/{slug}/hypotheses",
                &format!("{base}/hypotheses")
            )
            .await?["items"],
        json!([])
    );
    let (_, rows) = world.audit(cursor).await?;
    assert!(!rows.iter().any(|row| {
        row["action"]
            .as_str()
            .is_some_and(|s| s.starts_with("import."))
    }));
    world
        .api(
            Method::PUT,
            "/api/projects/{slug}/members/{user_id}",
            &format!("{base}/members/{}", world.ben.id),
            &admin,
            Some(&json!({"role":"researcher"})),
            200,
        )
        .await?;
    let (cursor, _) = world.audit(cursor).await?;
    let imported = world.run(&bundle, &slug, Some(&world.science), &[], 0)?;
    assert!(!imported.stdout.contains("project: created"));
    assert!(!imported.stdout.contains("science revision 1: registered"));
    let project = world.get(PROJECT, &base).await?;
    assert_eq!(project["title"], "Keep existing title");
    assert_eq!(project["description"], "Keep existing description");
    let (_, rows) = world.audit(cursor).await?;
    let events: Vec<_> = rows
        .iter()
        .filter(|row| row["project_id"] == project["id"])
        .collect();
    assert_eq!(events.len(), 11);
    assert!(!events.iter().any(|row| {
        [
            "import.project_created",
            "import.membership",
            "import.science_registered",
        ]
        .iter()
        .any(|action| row["action"] == *action)
    }));
    assert_eq!(
        world
            .get(
                "/api/projects/{slug}/hypotheses",
                &format!("{base}/hypotheses?limit=100")
            )
            .await?["items"]
            .as_array()
            .ok_or("existing hypotheses missing")?
            .len(),
        7
    );
    assert_eq!(
        world.get(CONFIG, &format!("{base}/config/science")).await?["items"]
            .as_array()
            .ok_or("existing science missing")?
            .len(),
        1
    );
    Ok(())
}
