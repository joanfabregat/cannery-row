//! Source intent: `test_read_side.py` (24), `test_attention.py` (7), `test_comparisons.py` (4).
//! DB-forced replacement records/backfill/corrupt dashboard/object injection and single-pool
//! streaming instrumentation remain explicit gaps; this suite never seeds or alters SQL rows.
#[path = "support/import_support.rs"]
#[allow(dead_code, reason = "reuse the approved CLI/OIDC fixture bootstrap")]
mod import_support;
#[path = "support/lifecycle_support.rs"]
#[allow(
    dead_code,
    reason = "reuse public API lifecycle setup without unrelated lease assertions"
)]
mod lifecycle_support;
#[path = "support/read_side_support.rs"]
mod support;

use conformance::Result;
use import_support::{HYPOTHESIS, REPORT, World, string};
use lifecycle_support::{Actors, Call, World as LiveWorld};
use reqwest::Method;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use support::{items, query, walk};

async fn imported() -> Result<World> {
    let world = World::new().await?;
    let bundle = world.bundle("read-side", &world.slug)?;
    world.run(&bundle, &world.slug, Some(&world.science), &[], 0)?;
    Ok(world)
}

#[tokio::test]
#[ignore = "requires URL API/OIDC and actual import CLI"]
#[allow(
    clippy::too_many_lines,
    reason = "keep the populated report/catalog and pagination checks in one fixture"
)]
async fn historical_read_models_filter_page_and_order_attention() -> Result<()> {
    let mut world = imported().await?;
    let base = world.base();
    let token = world.ana.read_token.clone();
    let reports = query(
        &mut world,
        "/api/projects/{slug}/reports",
        &format!("{base}/reports"),
        &token,
        &[],
        200,
    )
    .await?;
    // The list contains claimed live sheets only; historical reports are read by attempt.
    assert_eq!(items(&reports)?.len(), 0);
    for (key, value, count) in [
        ("track", "lexical", 0),
        ("track", "dense", 0),
        ("hypothesis", "1", 0),
        ("hypothesis", "4", 0),
    ] {
        let filtered = query(
            &mut world,
            "/api/projects/{slug}/reports",
            &format!("{base}/reports"),
            &token,
            &[(key, value.into())],
            200,
        )
        .await?;
        assert_eq!(items(&filtered)?.len(), count);
    }
    let report = world
        .get(REPORT, &format!("{base}/hypotheses/1/attempts/1/report"))
        .await?;
    assert_eq!(report["origin"], "imported");
    assert_eq!(report["report"]["written_at"], "2026-09-28");
    let catalog = query(
        &mut world,
        "/api/projects/{slug}/metrics",
        &format!("{base}/metrics"),
        &token,
        &[],
        200,
    )
    .await?;
    assert_eq!(catalog["science_revision"], 1);
    assert_eq!(catalog["metrics"][0]["key"], "mrr");
    assert_eq!(catalog["metrics"][0]["direction"], "higher");
    assert_eq!(
        catalog,
        query(
            &mut world,
            "/api/projects/{slug}/metrics",
            &format!("{base}/metrics"),
            &token,
            &[("science_revision", "1".into())],
            200
        )
        .await?
    );
    query(
        &mut world,
        "/api/projects/{slug}/metrics",
        &format!("{base}/metrics"),
        &token,
        &[("science_revision", "9".into())],
        404,
    )
    .await?;
    historical_metrics(&mut world).await?;
    historical_attention(&mut world).await?;
    for (template, path, params) in [
        (
            "/api/projects/{slug}/members",
            format!("{base}/members"),
            vec![],
        ),
        (
            "/api/projects/{slug}/tracks",
            format!("{base}/tracks"),
            vec![],
        ),
        (
            "/api/projects/{slug}/hypotheses",
            format!("{base}/hypotheses"),
            vec![],
        ),
        (
            "/api/projects/{slug}/review-cases",
            format!("{base}/review-cases"),
            vec![],
        ),
        (
            "/api/projects/{slug}/reports",
            format!("{base}/reports"),
            vec![],
        ),
        (
            "/api/projects/{slug}/metrics/query",
            format!("{base}/metrics/query"),
            vec![
                ("metric", "mrr".into()),
                ("authority", "imported".into()),
                ("all_slices", "true".into()),
            ],
        ),
        (
            "/api/projects/{slug}/comparisons",
            format!("{base}/comparisons"),
            vec![("origin", "all".into())],
        ),
    ] {
        let mut full = params.clone();
        full.push(("limit", "200".into()));
        let all = query(&mut world, template, &path, &token, &full, 200).await?;
        assert_eq!(
            walk(&mut world, template, &path, &token, &params).await?,
            *items(&all)?
        );
        let default = query(&mut world, template, &path, &token, &params, 200).await?;
        assert_eq!(items(&default)?.len(), items(&all)?.len().min(50));
        for limit in ["0", "201"] {
            let mut bad = params.clone();
            bad.push(("limit", limit.into()));
            query(&mut world, template, &path, &token, &bad, 422).await?;
        }
    }
    support::finish(&mut world, "historical").await
}

#[allow(
    clippy::too_many_lines,
    reason = "keep historical authority/default and comparison filtering checks together"
)]
async fn historical_metrics(world: &mut World) -> Result<()> {
    let base = world.base();
    let token = world.ana.read_token.clone();
    let path = format!("{base}/metrics/query");
    let template = "/api/projects/{slug}/metrics/query";
    let default = query(
        world,
        template,
        &path,
        &token,
        &[("metric", "mrr".into())],
        200,
    )
    .await?;
    assert_eq!(items(&default)?.len(), 0);
    assert_eq!(default["context"]["authority"], "tester_verified");
    let all = query(
        world,
        template,
        &path,
        &token,
        &[
            ("metric", "mrr".into()),
            ("authority", "imported".into()),
            ("all_slices", "true".into()),
        ],
        200,
    )
    .await?;
    assert_eq!(items(&all)?.len(), 10);
    assert!(items(&all)?.iter().all(|point| matches!(
        point["authority"].as_str(),
        Some("imported_artifact" | "imported_transcribed")
    )));
    assert_eq!(
        items(&all)?
            .iter()
            .filter(|point| point["value"].is_null())
            .count(),
        1
    );
    let french = query(
        world,
        template,
        &path,
        &token,
        &[
            ("metric", "mrr".into()),
            ("authority", "imported".into()),
            ("filter", "language:fr".into()),
        ],
        200,
    )
    .await?;
    assert!(
        items(&french)?
            .iter()
            .all(|point| point["dimensions"] == json!({"language":"fr"}))
    );
    for (key, value) in [("filter", "fr"), ("authority", "self_reported")] {
        query(
            world,
            template,
            &path,
            &token,
            &[("metric", "mrr".into()), (key, value.into())],
            422,
        )
        .await?;
    }
    let path = format!("{base}/comparisons");
    let template = "/api/projects/{slug}/comparisons";
    let default = query(world, template, &path, &token, &[], 200).await?;
    assert_eq!(items(&default)?.len(), 0);
    let imported = query(
        world,
        template,
        &path,
        &token,
        &[("origin", "all".into())],
        200,
    )
    .await?;
    assert_eq!(items(&imported)?.len(), 1);
    let point = &imported["items"][0];
    assert_eq!(point["value"], 0.71);
    assert_eq!(
        point["reference"],
        json!({"value":0.68,"label":"base camp","kind":"baseline","ref":"base-camp"})
    );
    assert_eq!(point["hypothesis"], 1);
    assert_eq!(point["dimensions"], json!({}));
    for (key, value, count) in [
        ("track", "dense", 0),
        ("verdict", "fail", 0),
        ("verdict", "pass", 1),
        ("metric", "ndcg", 0),
    ] {
        let filtered = query(
            world,
            template,
            &path,
            &token,
            &[("origin", "all".into()), (key, value.into())],
            200,
        )
        .await?;
        assert_eq!(items(&filtered)?.len(), count);
    }
    let newest = string(&point["recorded_at"])?;
    let until = query(
        world,
        template,
        &path,
        &token,
        &[("origin", "all".into()), ("until", newest.clone())],
        200,
    )
    .await?;
    assert_eq!(items(&until)?.len(), 0);
    let since = query(
        world,
        template,
        &path,
        &token,
        &[("origin", "all".into()), ("since", newest)],
        200,
    )
    .await?;
    assert_eq!(items(&since)?.len(), 1);
    Ok(())
}

async fn historical_attention(world: &mut World) -> Result<()> {
    let base = world.base();
    let token = world.ana.read_token.clone();
    let template = "/api/projects/{slug}/attention";
    let path = format!("{base}/attention");
    let summary = query(world, template, &path, &token, &[], 200).await?;
    assert_eq!(
        summary["pending_counts"],
        json!({"draft":1,"result":1,"failure":0})
    );
    let pending = summary["pending_reviews"]
        .as_array()
        .ok_or("pending absent")?;
    assert_eq!(
        pending
            .iter()
            .map(|row| (&row["kind"], &row["hypothesis"]))
            .collect::<Vec<_>>(),
        vec![(&json!("result"), &json!(6)), (&json!("draft"), &json!(7))]
    );
    assert!(pending[0]["opened_at"].as_str() < pending[1]["opened_at"].as_str());
    assert_eq!(summary["running_count"], 0);
    assert_eq!(summary["stalled_evaluation_count"], 0);
    let limited = query(
        world,
        template,
        &path,
        &token,
        &[("limit", "1".into())],
        200,
    )
    .await?;
    assert_eq!(limited["pending_counts"], summary["pending_counts"]);
    assert_eq!(
        limited["pending_reviews"]
            .as_array()
            .ok_or("pending absent")?
            .len(),
        1
    );
    assert_eq!(
        limited["recent_outcomes"]
            .as_array()
            .ok_or("outcomes absent")?
            .len(),
        1
    );
    query(
        world,
        template,
        &path,
        &token,
        &[("limit", "51".into())],
        422,
    )
    .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires URL API/OIDC and populated import fixtures"]
async fn comments_mentions_search_privacy_and_hostile_cursors() -> Result<()> {
    let mut alpha = imported().await?;
    let mut beta = imported().await?;
    let viewer_id = alpha.ana.id.clone();
    let member_id = alpha.ben.id.clone();
    support::role(&mut alpha, &viewer_id, "viewer").await?;
    support::role(&mut alpha, &member_id, "member").await?;
    comment_history_mentions(&mut alpha, &mut beta).await?;
    search_privacy(&mut alpha, &beta).await?;
    search_cursors(&mut alpha, &beta).await?;
    support::finish(&mut alpha, "comments-search").await?;
    support::finish(&mut beta, "private-project").await
}

#[allow(
    clippy::too_many_lines,
    reason = "keep one comment's creation, edit history, privacy and backlink mutation ordered"
)]
async fn comment_history_mentions(alpha: &mut World, beta: &mut World) -> Result<()> {
    let base = alpha.base();
    let member = alpha.ben.token.clone();
    let viewer = alpha.ana.token.clone();
    let template = "/api/projects/{slug}/hypotheses/{number}/comments";
    let path = format!("{base}/hypotheses/7/comments");
    let mention = format!(
        "Compare #2, {}#4, {}#1, #999 and this very #7. A \u{1}forged\u{2} mark and zanzibar.",
        alpha.slug, beta.slug
    );
    let created = alpha
        .api(
            Method::POST,
            template,
            &path,
            &member,
            Some(&json!({"body_markdown":mention})),
            201,
        )
        .await?;
    assert_eq!(created["revision"], 1);
    assert!(created["edited_at"].is_null());
    for (token, status) in [(&viewer, 403), (&beta.ana.token, 404)] {
        alpha
            .api(
                Method::POST,
                template,
                &path,
                token,
                Some(&json!({"body_markdown":"refused"})),
                status,
            )
            .await?;
    }
    alpha
        .api(
            Method::POST,
            template,
            &path,
            &member,
            Some(&json!({"body_markdown":" \n "})),
            422,
        )
        .await?;
    let reference = format!("{}#7", alpha.slug);
    for number in [2, 4] {
        let hyp = alpha
            .get(HYPOTHESIS, &format!("{base}/hypotheses/{number}"))
            .await?;
        assert!(
            hyp["backlinks"]
                .as_array()
                .ok_or("backlinks absent")?
                .iter()
                .any(|link| link["ref"] == reference)
        );
    }
    let beta_hyp = beta
        .get(HYPOTHESIS, &format!("{}/hypotheses/1", beta.base()))
        .await?;
    assert!(
        !beta_hyp["backlinks"]
            .as_array()
            .ok_or("backlinks absent")?
            .iter()
            .any(|link| link["ref"] == reference)
    );
    let own = alpha
        .get(HYPOTHESIS, &format!("{base}/hypotheses/7"))
        .await?;
    assert!(
        !own["backlinks"]
            .as_array()
            .ok_or("backlinks absent")?
            .iter()
            .any(|link| link["ref"] == reference)
    );
    let search = query(
        alpha,
        "/api/search",
        "/api/search",
        &viewer,
        &[("q", "zanzibar".into()), ("kind", "comment".into())],
        200,
    )
    .await?;
    let hit = items(&search)?.first().ok_or("comment search absent")?;
    let snippet = string(&hit["snippet"])?;
    assert_eq!(snippet.matches('\u{1}').count(), 1);
    assert_eq!(snippet.matches('\u{2}').count(), 1);
    assert!(snippet.contains("\u{1}zanzibar\u{2}"));
    assert!(snippet.contains("forged mark"));
    let comment = format!("{base}/comments/{}", string(&created["id"])?);
    let template = "/api/projects/{slug}/comments/{comment_id}";
    let edited = alpha
        .api(
            Method::PUT,
            template,
            &comment,
            &member,
            Some(&json!({"expected_revision":1,"body_markdown":"Only #4 now."})),
            200,
        )
        .await?;
    assert_eq!(edited["revision"], 2);
    assert!(!edited["edited_at"].is_null());
    let stale = alpha
        .api(
            Method::PUT,
            template,
            &comment,
            &member,
            Some(&json!({"expected_revision":1,"body_markdown":"late"})),
            409,
        )
        .await?;
    assert_eq!(stale["error"]["code"], "stale_revision");
    alpha
        .api(
            Method::PUT,
            template,
            &comment,
            &viewer,
            Some(&json!({"expected_revision":2,"body_markdown":"hijacked"})),
            403,
        )
        .await?;
    let unchanged = alpha
        .api(
            Method::PUT,
            template,
            &comment,
            &member,
            Some(&json!({"expected_revision":2,"body_markdown":"Only #4 now."})),
            200,
        )
        .await?;
    assert_eq!(unchanged["revision"], 2);
    let revisions = query(
        alpha,
        "/api/projects/{slug}/comments/{comment_id}/revisions",
        &format!("{comment}/revisions"),
        &viewer,
        &[],
        200,
    )
    .await?;
    assert_eq!(
        items(&revisions)?
            .iter()
            .map(|row| row["revision"].as_u64())
            .collect::<Vec<_>>(),
        vec![Some(1), Some(2)]
    );
    let hyp = alpha
        .get(HYPOTHESIS, &format!("{base}/hypotheses/2"))
        .await?;
    assert!(
        !hyp["backlinks"]
            .as_array()
            .ok_or("backlinks absent")?
            .iter()
            .any(|link| link["ref"] == reference)
    );
    assert_eq!(alpha.get(template, &comment).await?, edited);
    Ok(())
}

async fn search_privacy(alpha: &mut World, beta: &World) -> Result<()> {
    let viewer = alpha.ana.read_token.clone();
    let outsider = beta.ana.read_token.clone();
    for params in [
        vec![],
        vec![("q", "BM25".into())],
        vec![("q", "#1".into())],
        vec![("q", format!("{}#1", beta.slug))],
        vec![("project", beta.slug.clone())],
    ] {
        let body = query(alpha, "/api/search", "/api/search", &viewer, &params, 200).await?;
        assert!(items(&body)?.iter().all(|row| row["project"] == alpha.slug));
        assert!(!serde_json::to_string(&body)?.contains(&beta.slug));
        let mut scoped: Vec<_> = params
            .iter()
            .filter(|(key, _)| *key != "project")
            .cloned()
            .collect();
        scoped.push(("project", alpha.slug.clone()));
        let outsider_body =
            query(alpha, "/api/search", "/api/search", &outsider, &scoped, 200).await?;
        assert_eq!(items(&outsider_body)?.len(), 0);
        assert_eq!(outsider_body["total"], 0);
        for (_, facet) in outsider_body["facets"].as_object().ok_or("facets absent")? {
            assert_eq!(*facet, json!({}));
        }
    }
    let bare = query(
        alpha,
        "/api/search",
        "/api/search",
        &viewer,
        &[("q", "#1".into())],
        200,
    )
    .await?;
    assert_eq!(items(&bare)?.len(), 1);
    assert_eq!(bare["items"][0]["ref"], format!("{}#1", alpha.slug));
    let scoped = query(
        alpha,
        "/api/search",
        "/api/search",
        &viewer,
        &[("q", format!("{}#1", alpha.slug))],
        200,
    )
    .await?;
    assert_eq!(items(&bare)?, items(&scoped)?);
    let all = query(
        alpha,
        "/api/search",
        "/api/search",
        &viewer,
        &[("limit", "200".into())],
        200,
    )
    .await?;
    assert_eq!(
        walk(alpha, "/api/search", "/api/search", &viewer, &[]).await?,
        *items(&all)?
    );
    let unique: BTreeSet<_> = items(&all)?
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<_, _>>()?;
    assert_eq!(unique.len(), items(&all)?.len());
    let rejected = query(
        alpha,
        "/api/search",
        "/api/search",
        &viewer,
        &[("decision", "reject".into())],
        200,
    )
    .await?;
    assert!(
        items(&rejected)?
            .iter()
            .all(|row| row["decision"] == "reject")
    );
    let past = query(
        alpha,
        "/api/search",
        "/api/search",
        &viewer,
        &[("until", "2000-01-01T00:00:00Z".into())],
        200,
    )
    .await?;
    assert_eq!(items(&past)?.len(), 0);
    Ok(())
}

async fn search_cursors(alpha: &mut World, beta: &World) -> Result<()> {
    let viewer = alpha.ana.read_token.clone();
    for raw in [
        "[0.5,9223372036854775808]",
        "[0.5,18446744073709551616]",
        "[0.5,0]",
        "[0.5,-1]",
        "[0.5,1.5]",
        "[0.5,true]",
        "[true,1]",
        "[1e400,1]",
        "[0.5]",
        "[0.5,1,2]",
        "{\"score\":0.5,\"id\":1}",
        "\"ab\"",
        "[NaN,1]",
        "[Infinity,1]",
    ] {
        query(
            alpha,
            "/api/search",
            "/api/search",
            &viewer,
            &[("before", support::cursor(raw.as_bytes()))],
            422,
        )
        .await?;
    }
    for cursor in [
        support::cursor(&[255, 254]),
        "not a cursor!".into(),
        "a".into(),
    ] {
        query(
            alpha,
            "/api/search",
            "/api/search",
            &viewer,
            &[("before", cursor)],
            422,
        )
        .await?;
    }
    query(
        alpha,
        "/api/search",
        "/api/search",
        &viewer,
        &[("before", support::cursor(b"[0.5,9223372036854775807]"))],
        200,
    )
    .await?;
    for (key, value) in [
        ("limit", "0".into()),
        ("limit", "201".into()),
        ("q", "x".repeat(501)),
    ] {
        query(
            alpha,
            "/api/search",
            "/api/search",
            &viewer,
            &[(key, value)],
            422,
        )
        .await?;
    }
    let path = format!("{}/members", alpha.base());
    let template = "/api/projects/{slug}/members";
    for (id, status) in [
        (beta.ana.id.clone(), 422),
        ("00000000-0000-0000-0000-000000000001".into(), 422),
        (alpha.ben.id.clone(), 200),
    ] {
        query(alpha, template, &path, &viewer, &[("before", id)], status).await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires URL API/OIDC, uploads and public tester/evaluator completions"]
async fn verified_metrics_reference_disagreement_dashboard_and_attention() -> Result<()> {
    let (mut world, actors) = LiveWorld::new("read-side").await?;
    let (first, lease) = support::submit_live(
        &mut world,
        &actors,
        "Synonyms for French requêtes",
        "Quokkazanzibar report; compare lexical evidence.",
    )
    .await?;
    let tested_outputs = support::complete_live(
        &mut world,
        &actors,
        &lease,
        "Fixture control (base-camp fixture-r1)",
    )
    .await?;
    let initial = support::live_get(&mut world, &actors, "/metrics/query?metric=mrr").await?;
    assert_eq!(items(&initial)?.len(), 1);
    assert_eq!(
        initial["items"][0]["reference"],
        json!({"value":0.625,"label":"Fixture control (base-camp fixture-r1)","kind":"baseline","ref":"base-camp"})
    );
    let (second, lease) = support::submit_live(
        &mut world,
        &actors,
        "Second published reference",
        "Une requête française; comparison against the first run.",
    )
    .await?;
    support::complete_live(&mut world, &actors, &lease, "Base camp, as published").await?;
    live_metrics(&mut world, &actors, first, second).await?;
    live_comparisons(&mut world, &actors, first, second).await?;
    live_dashboard(&mut world, &actors).await?;
    live_attention(&mut world, &actors, first, second).await?;
    live_search(&mut world, &actors, first).await?;
    configured_dashboard(&mut world, &actors).await?;
    live_roles_assets(&mut world, &actors, first, &tested_outputs).await?;
    live_pages(&mut world, &actors, first).await?;
    world.finish_coverage(&actors.admin, "read-side-live").await
}

#[allow(
    clippy::too_many_lines,
    reason = "keep the maintained populated-list inventory visible in one table"
)]
async fn live_pages(world: &mut LiveWorld, actors: &Actors, number: i64) -> Result<()> {
    let base = world.base();
    let comment = world
        .api(Call::post(
            "/api/projects/{slug}/hypotheses/{number}/comments",
            format!("{base}/hypotheses/{number}/comments"),
            &actors.admin,
            json!({"body_markdown":"first pagination comment"}),
            201,
        ))
        .await?
        .body;
    let comment_id = string(&comment["id"])?;
    let mut edit = Call::post(
        "/api/projects/{slug}/comments/{comment_id}",
        format!("{base}/comments/{comment_id}"),
        &actors.admin,
        json!({"expected_revision":1,"body_markdown":"second revision"}),
        200,
    );
    edit.method = Method::PUT;
    world.api(edit).await?;
    for (template, path, params) in [
        ("/api/projects", "/api/projects".into(), vec![]),
        ("/api/tokens", "/api/tokens".into(), vec![]),
        (
            "/api/users",
            "/api/users".into(),
            vec![("email", "admin@conformance.test".into())],
        ),
        (
            "/api/projects/{slug}/service-accounts",
            format!("{base}/service-accounts"),
            vec![],
        ),
        (
            "/api/projects/{slug}/service-accounts/{name}/tokens",
            format!("{base}/service-accounts/agent/tokens"),
            vec![],
        ),
        (
            "/api/projects/{slug}/config/{kind}",
            format!("{base}/config/science"),
            vec![],
        ),
        (
            "/api/projects/{slug}/tracks/{track_slug}/history",
            format!("{base}/tracks/lexical/history"),
            vec![],
        ),
        (
            "/api/projects/{slug}/producers",
            format!("{base}/producers"),
            vec![],
        ),
        (
            "/api/projects/{slug}/hypotheses/{number}/revisions",
            format!("{base}/hypotheses/{number}/revisions"),
            vec![],
        ),
        (
            "/api/projects/{slug}/hypotheses/{number}/attempts",
            format!("{base}/hypotheses/{number}/attempts"),
            vec![],
        ),
        (
            "/api/projects/{slug}/hypotheses/{number}/attempts/{sequence}/jobs",
            format!("{base}/hypotheses/{number}/attempts/1/jobs"),
            vec![],
        ),
        (
            "/api/projects/{slug}/attempts",
            format!("{base}/attempts"),
            vec![],
        ),
        (
            "/api/projects/{slug}/reports",
            format!("{base}/reports"),
            vec![],
        ),
        (
            "/api/projects/{slug}/hypotheses/{number}/comments",
            format!("{base}/hypotheses/{number}/comments"),
            vec![],
        ),
        (
            "/api/projects/{slug}/comments/{comment_id}/revisions",
            format!("{base}/comments/{comment_id}/revisions"),
            vec![],
        ),
        (
            "/api/projects/{slug}/metrics/query",
            format!("{base}/metrics/query"),
            vec![("metric", "mrr".into()), ("all_slices", "true".into())],
        ),
        (
            "/api/projects/{slug}/comparisons",
            format!("{base}/comparisons"),
            vec![],
        ),
        (
            "/api/search",
            "/api/search".into(),
            vec![("project", world.project.clone())],
        ),
        (
            "/api/search",
            "/api/search".into(),
            vec![
                ("project", world.project.clone()),
                ("q", "reference".into()),
            ],
        ),
    ] {
        support::live_page_check(world, &actors.admin, template, &path, &params).await?;
    }
    Ok(())
}

async fn live_search(world: &mut LiveWorld, actors: &Actors, first: i64) -> Result<()> {
    for (query_text, kind) in [
        ("synonims", "hypothesis"),
        ("requêtes", "hypothesis"),
        ("quokkazanzibar", "report"),
    ] {
        let response = world
            .h
            .request(Method::GET, "/api/search")?
            .bearer_auth(&actors.agent)
            .query(&[
                ("q", query_text),
                ("kind", kind),
                ("project", world.project.as_str()),
            ])
            .send()
            .await?;
        let found = world
            .h
            .check_response(Method::GET, "/api/search", response, 200)
            .await?
            .body;
        assert!(items(&found)?.iter().any(|row| row["hypothesis"] == first));
        let scores: Vec<_> = items(&found)?
            .iter()
            .map(|row| row["score"].as_f64().ok_or("score absent"))
            .collect::<std::result::Result<_, _>>()?;
        assert!(scores.windows(2).all(|pair| pair[0] >= pair[1]));
    }
    let body = support::live_get(
        world,
        actors,
        &format!("/hypotheses/{first}/attempts/1/report"),
    )
    .await?;
    let actor = string(&body["author"]["id"])?;
    let response = world
        .h
        .request(Method::GET, "/api/search")?
        .bearer_auth(&actors.admin)
        .query(&[
            ("actor", actor.as_str()),
            ("kind", "report"),
            ("project", world.project.as_str()),
            ("limit", "200"),
        ])
        .send()
        .await?;
    let found = world
        .h
        .check_response(Method::GET, "/api/search", response, 200)
        .await?
        .body;
    assert_eq!(items(&found)?.len(), 3);
    assert_eq!(found["facets"]["actor"][&actor], 3);
    Ok(())
}

async fn configured_dashboard(world: &mut LiveWorld, actors: &Actors) -> Result<()> {
    let views = json!({"views":[{"id":"french-table","title":"French MRR","chart":"table","metric":"mrr","split":"dev","group_by":["track"],"filters":{"language":["fr"],"track":["lexical"]},"baseline":"base-camp"}]});
    world
        .api(Call::post(
            "/api/projects/{slug}/config/{kind}",
            format!("{}/config/dashboard", world.base()),
            &actors.admin,
            views,
            201,
        ))
        .await?;
    let configured = support::live_get(world, actors, "/dashboard").await?;
    assert_eq!(configured["derived"], false);
    assert_eq!(configured["dashboard_revision"], 1);
    let french = support::live_get(world, actors, "/dashboard/views/french-table").await?;
    assert_eq!(
        french["warnings"],
        json!(["the evaluator verdicts of one point reported different references; it shows none"])
    );
    assert_eq!(french["series"].as_array().ok_or("series absent")?.len(), 2);
    for series in french["series"].as_array().ok_or("series absent")? {
        assert_eq!(series["group"], json!({"track":"lexical"}));
        assert_eq!(
            series["points"][0]["value"],
            if series["science_revision"] == 1 {
                2.0
            } else {
                1.0
            }
        );
    }
    let mut missing = Call::get(
        "/api/projects/{slug}/dashboard/views/{view_id}",
        format!(
            "{}/dashboard/views/mrr-by-track?dashboard_revision=1",
            world.base()
        ),
        &actors.admin,
    );
    missing.status = 404;
    world.api(missing).await?;
    world.api(Call::post("/api/projects/{slug}/config/{kind}",format!("{}/config/dashboard",world.base()),&actors.admin,json!({"views":[{"id":"invalid","title":"Invalid","chart":"table","metric":"mrr","filters":{"nope":["x"]}}]}),422)).await?;
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "keep role-specific downloads, caching and cross-project identity checks together"
)]
async fn live_roles_assets(
    world: &mut LiveWorld,
    actors: &Actors,
    number: i64,
    outputs: &[Value],
) -> Result<()> {
    let reader = World::new().await?;
    for (id, role) in [(&reader.ana.id, "viewer"), (&reader.ben.id, "member")] {
        let mut call = Call::post(
            "/api/projects/{slug}/members/{user_id}",
            format!("{}/members/{id}", world.base()),
            &actors.admin,
            json!({"role":role}),
            200,
        );
        call.method = Method::PUT;
        world.api(call).await?;
    }
    let attention = support::live_get(world, actors, "/attention").await?;
    for token in [
        &reader.ana.token,
        &reader.ben.token,
        &actors.agent,
        &actors.tester,
        &actors.evaluator,
    ] {
        let body = world
            .api(Call::get(
                "/api/projects/{slug}/attention",
                format!("{}/attention", world.base()),
                token,
            ))
            .await?
            .body;
        assert_eq!(body, attention);
    }
    let report_path = format!("{}/hypotheses/{number}/attempts/1/report", world.base());
    let report = world
        .api(Call::get(
            &format!("{}/report", lifecycle_support::ATTEMPT),
            report_path.clone(),
            &reader.ana.token,
        ))
        .await?
        .body;
    assert_eq!(
        report["tester"]["measurements"]
            .as_array()
            .ok_or("measurements absent")?
            .len(),
        3
    );
    assert_eq!(report["evaluation"]["verdict"], "pass");
    let asset = &report["assets"][0];
    let id = string(&asset["id"])?;
    let path = format!("{}/artifacts/{id}", world.base());
    let template = "/api/projects/{slug}/artifacts/{artifact_id}";
    let first = world
        .api(Call::get(template, path.clone(), &reader.ana.token))
        .await?;
    assert_eq!(first.raw_body, support::FIGURE);
    assert_eq!(
        first.headers["content-disposition"],
        "attachment; filename=\"figure.png\""
    );
    assert_eq!(first.headers["content-type"], "image/png");
    assert_eq!(first.headers["x-content-type-options"], "nosniff");
    assert_eq!(
        first.headers["content-security-policy"],
        "default-src 'none'; sandbox"
    );
    let etag = first.headers["etag"].to_str()?.to_owned();
    assert_eq!(
        etag,
        format!("\"{}\"", lifecycle_support::sha(support::FIGURE))
    );
    for (value, status) in [
        (etag.clone(), 304),
        (format!("W/{etag}"), 304),
        (format!("\"other\", {etag}"), 304),
        ("*".into(), 304),
        ("\"other\"".into(), 200),
    ] {
        let response = world
            .h
            .request(Method::GET, &path)?
            .bearer_auth(&reader.ana.token)
            .header("If-None-Match", value)
            .send()
            .await?;
        let response = world
            .h
            .check_response(Method::GET, template, response, status)
            .await?;
        if status == 304 {
            assert_eq!(response.raw_body.len(), 0);
            assert_eq!(response.headers["etag"], etag);
        }
    }
    let attempt =
        support::live_get(world, actors, &format!("/hypotheses/{number}/attempts/1")).await?;
    let artifacts = attempt["artifacts"].as_array().ok_or("artifacts absent")?;
    // AttemptDetail excludes job outputs; retain their actual upload-response IDs.
    assert!(
        outputs
            .iter()
            .any(|artifact| artifact["role"] == "step_log")
    );
    for artifact in artifacts
        .iter()
        .chain(outputs.iter())
        .filter(|artifact| artifact["role"] != "report_asset")
    {
        let path = format!("{}/artifacts/{}", world.base(), string(&artifact["id"])?);
        for token in [
            &reader.ana.token,
            &actors.agent,
            &actors.tester,
            &actors.evaluator,
        ] {
            let mut call = Call::get(template, path.clone(), token);
            call.status = 403;
            assert_eq!(world.api(call).await?.body["error"]["code"], "forbidden");
        }
        assert_eq!(
            world
                .api(Call::get(template, path, &reader.ben.token))
                .await?
                .status,
            200
        );
    }
    let (mut beta, beta_actors) = LiveWorld::new("read-side-other").await?;
    let (number, _) =
        support::submit_live(&mut beta, &beta_actors, "Only beta", "Private beta report.").await?;
    let attempt = support::live_get(
        &mut beta,
        &beta_actors,
        &format!("/hypotheses/{number}/attempts/1"),
    )
    .await?;
    let candidate = attempt["artifacts"]
        .as_array()
        .ok_or("artifacts absent")?
        .iter()
        .find(|artifact| artifact["role"] == "candidate")
        .ok_or("candidate absent")?;
    let candidate = string(&candidate["id"])?;
    let path = format!("{}/artifacts/{candidate}", beta.base());
    let response = beta
        .h
        .request(Method::GET, &path)?
        .bearer_auth(&reader.ana.token)
        .header("If-None-Match", "*")
        .send()
        .await?;
    beta.h
        .check_response(Method::GET, template, response, 404)
        .await?;
    let mut membership = Call::post(
        "/api/projects/{slug}/members/{user_id}",
        format!("{}/members/{}", beta.base(), reader.ana.id),
        &beta_actors.admin,
        json!({"role":"member"}),
        200,
    );
    membership.method = Method::PUT;
    beta.api(membership).await?;
    let downloaded = beta
        .api(Call::get(template, path, &reader.ana.token))
        .await?;
    assert_eq!(
        downloaded.raw_body,
        include_bytes!("../../../examples/fixture/candidate.json")
    );
    let mut crossed = Call::get(
        template,
        format!("{}/artifacts/{candidate}", world.base()),
        &reader.ana.token,
    );
    crossed.status = 404;
    world.api(crossed).await?;
    let other = beta
        .api(Call::get(
            "/api/projects/{slug}/attention",
            format!("{}/attention", beta.base()),
            &reader.ana.token,
        ))
        .await?
        .body;
    assert_eq!(other["pending_counts"]["result"], 0);
    assert_eq!(
        support::live_get(world, actors, "/attention").await?,
        attention
    );
    beta.finish_coverage(&beta_actors.admin, "read-side-other")
        .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires URL API/OIDC, actual upload/submission and PostgreSQL search"]
async fn report_at_size_cap_remains_searchable() -> Result<()> {
    let (mut world, actors) = LiveWorld::new("read-side-cap").await?;
    let mut science: Value =
        serde_json::from_str(include_str!("../../../examples/fixture/science.json"))?;
    science["limits"]["report_max_bytes"] = json!(1024 * 1024);
    world
        .api(Call::post(
            "/api/projects/{slug}/config/{kind}",
            format!("{}/config/science", world.base()),
            &actors.admin,
            science,
            201,
        ))
        .await?;
    let mut report = "Quokkazanzibar opens the report. ".to_owned();
    for index in 0..117_000 {
        use std::fmt::Write;
        write!(report, "w{index:07} ")?;
    }
    report.truncate(1024 * 1024);
    let report = report.trim().to_owned();
    assert!(report.len() > 1024 * 1024 - 16);
    let (number, _) = support::submit_live(&mut world, &actors, "Report cap case", &report).await?;
    let body = support::live_get(
        &mut world,
        &actors,
        &format!("/hypotheses/{number}/attempts/1/report"),
    )
    .await?;
    assert_eq!(body["report"]["body_markdown"], report);
    let response = world
        .h
        .request(Method::GET, "/api/search")?
        .bearer_auth(&actors.agent)
        .query(&[("q", "quokkazanzibar"), ("project", world.project.as_str())])
        .send()
        .await?;
    let found = world
        .h
        .check_response(Method::GET, "/api/search", response, 200)
        .await?
        .body;
    assert_eq!(items(&found)?.len(), 1);
    assert_eq!(found["items"][0]["kind"], "report");
    assert_eq!(found["items"][0]["hypothesis"], number);
    world.finish_coverage(&actors.admin, "read-side-cap").await
}

async fn live_metrics(
    world: &mut LiveWorld,
    actors: &Actors,
    first: i64,
    second: i64,
) -> Result<()> {
    let overall = support::live_get(world, actors, "/metrics/query?metric=mrr").await?;
    assert_eq!(items(&overall)?.len(), 2);
    assert_eq!(overall["context"]["authority"], "tester_verified");
    assert_eq!(overall["context"]["sample_count"], 8);
    assert_eq!(overall["context"]["failed_attempts"], 0);
    assert_eq!(
        items(&overall)?
            .iter()
            .map(|point| point["hypothesis"].as_i64())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([Some(first), Some(second)])
    );
    assert!(
        items(&overall)?
            .iter()
            .all(|point| point["dimensions"] == json!({})
                && point["value"] == 1.0
                && point["control_value"] == 0.625)
    );
    let labels: BTreeSet<_> = items(&overall)?
        .iter()
        .map(|point| point["reference"]["label"].as_str())
        .collect();
    assert_eq!(
        labels,
        BTreeSet::from([
            Some("Fixture control (base-camp fixture-r1)"),
            Some("Base camp, as published")
        ])
    );
    let all = support::live_get(world, actors, "/metrics/query?metric=mrr&all_slices=true").await?;
    assert_eq!(items(&all)?.len(), 6);
    assert!(
        items(&all)?
            .iter()
            .all(|point| point["authority"] == "tester_verified" && point["value"] != 0.9)
    );
    let language = support::live_get(
        world,
        actors,
        "/metrics/query?metric=mrr&dimensions=language",
    )
    .await?;
    assert_eq!(items(&language)?.len(), 4);
    let french = support::live_get(
        world,
        actors,
        "/metrics/query?metric=mrr&filter=language:fr",
    )
    .await?;
    assert_eq!(items(&french)?.len(), 2);
    assert!(
        items(&french)?
            .iter()
            .all(|point| point["dimensions"] == json!({"language":"fr"})
                && point["control_value"] == 0.5)
    );
    let claims = support::live_get(
        world,
        actors,
        "/metrics/query?metric=mrr&authority=agent_claim&dimensions=language",
    )
    .await?;
    assert_eq!(items(&claims)?.len(), 2);
    assert!(
        items(&claims)?
            .iter()
            .all(|point| point["value"] == 0.9 && point["authority"] == "agent_claim")
    );
    let overall_claims = support::live_get(
        world,
        actors,
        "/metrics/query?metric=mrr&authority=agent_claim",
    )
    .await?;
    assert_eq!(items(&overall_claims)?.len(), 0);
    Ok(())
}

async fn live_comparisons(
    world: &mut LiveWorld,
    actors: &Actors,
    first: i64,
    second: i64,
) -> Result<()> {
    let all = support::live_get(world, actors, "/comparisons?limit=200").await?;
    assert_eq!(items(&all)?.len(), 6);
    let ids: Vec<_> = items(&all)?.iter().map(|row| row["id"].as_i64()).collect();
    let mut sorted = ids.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(ids, sorted);
    let overall = support::live_get(world, actors, "/comparisons?overall=true").await?;
    assert_eq!(items(&overall)?.len(), 2);
    let french = support::live_get(
        world,
        actors,
        "/comparisons?dimensions=language&filter=language:fr",
    )
    .await?;
    assert_eq!(items(&french)?.len(), 2);
    assert!(
        items(&french)?
            .iter()
            .all(|row| row["reference"]["value"] == 0.5)
    );
    assert_eq!(
        items(&all)?
            .iter()
            .map(|row| row["hypothesis"].as_i64())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([Some(first), Some(second)])
    );
    for row in items(&all)? {
        assert_eq!(row["metric"], "mrr");
        assert_eq!(row["split"], "dev");
        assert_eq!(row["source"], "tester");
        assert_eq!(row["value"], 1.0);
        assert_eq!(row["verdict"], "pass");
        assert_eq!(row["policy_revision"], "fixture-policy-1");
    }
    for filter in ["verdict=fail", "metric=ndcg", "track=missing-track"] {
        assert_eq!(
            items(&support::live_get(world, actors, &format!("/comparisons?{filter}")).await?)?
                .len(),
            0
        );
    }
    let base = world.base();
    for token in [&actors.agent, &actors.evaluator] {
        let body = world
            .api(Call::get(
                "/api/projects/{slug}/comparisons",
                format!("{base}/comparisons"),
                token,
            ))
            .await?
            .body;
        assert_eq!(body, all);
    }
    let cases = support::live_get(world, actors, "/review-cases?kind=result&state=pending").await?;
    let case = items(&cases)?.first().ok_or("pending result absent")?;
    let path = format!("{base}/review-cases/{}/decisions", string(&case["id"])?);
    let refused=world.api(Call::post("/api/projects/{slug}/review-cases/{case_id}/decisions",path,&actors.evaluator,json!({"review_case_id":case["id"],"evidence_revision":case["subject_revision"],"action":"promote","reason":"cannot decide my own evaluation"}),403)).await?.body;
    assert_eq!(refused["error"]["code"], "forbidden");
    Ok(())
}

async fn live_dashboard(world: &mut LiveWorld, actors: &Actors) -> Result<()> {
    let table = support::live_get(world, actors, "/dashboard/views/mrr-by-track").await?;
    assert_eq!(table["aggregation"], "mean");
    let series = table["series"].as_array().ok_or("series absent")?;
    assert_eq!(series.len(), 1);
    let point = &series[0]["points"][0];
    assert_eq!(point["count"], 2);
    assert_eq!(point["value"], 1.0);
    assert!(point["control_value"].is_null());
    assert!(point["reference_label"].is_null());
    assert_eq!(
        table["warnings"],
        json!(["the evaluator verdicts of one point reported different references; it shows none"])
    );
    let timeline = support::live_get(world, actors, "/dashboard/views/mrr-timeline").await?;
    assert!(timeline["aggregation"].is_null());
    let derived = support::live_get(world, actors, "/dashboard").await?;
    assert_eq!(derived["derived"], true);
    assert!(derived["dashboard_revision"].is_null());
    let mut science: Value =
        serde_json::from_str(include_str!("../../../examples/fixture/science.json"))?;
    science["metrics"][0]["aggregation"] = json!("sum");
    world
        .api(Call::post(
            "/api/projects/{slug}/config/{kind}",
            format!("{}/config/science", world.base()),
            &actors.admin,
            science,
            201,
        ))
        .await?;
    let (_, attempt) = support::submit_live(
        world,
        actors,
        "New science registry",
        "Pinned revision two.",
    )
    .await?;
    support::complete_live(
        world,
        actors,
        &attempt,
        "Fixture control (base-camp fixture-r1)",
    )
    .await?;
    let table = support::live_get(world, actors, "/dashboard/views/mrr-by-track").await?;
    assert_eq!(table["aggregation"], "sum");
    let series = table["series"].as_array().ok_or("series absent")?;
    assert_eq!(series.len(), 2);
    assert_eq!(
        series
            .iter()
            .map(|row| row["science_revision"].as_u64())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([Some(1), Some(2)])
    );
    let old = series
        .iter()
        .find(|row| row["science_revision"] == 1)
        .ok_or("old revision absent")?;
    assert_eq!(old["points"][0]["value"], 2.0);
    assert_eq!(old["points"][0]["count"], 2);
    let new = series
        .iter()
        .find(|row| row["science_revision"] == 2)
        .ok_or("new revision absent")?;
    assert_eq!(new["points"][0]["count"], 1);
    assert_eq!(new["points"][0]["control_value"], 0.625);
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "keep corrected outcomes and ordered result/failure/draft/running transitions together"
)]
async fn live_attention(
    world: &mut LiveWorld,
    actors: &Actors,
    first: i64,
    second: i64,
) -> Result<()> {
    let before = support::live_get(world, actors, "/attention").await?;
    assert_eq!(
        before["pending_counts"],
        json!({"draft":0,"result":3,"failure":0})
    );
    let pending = before["pending_reviews"]
        .as_array()
        .ok_or("pending absent")?;
    assert_eq!(pending[0]["hypothesis"], first);
    assert_eq!(pending[1]["hypothesis"], second);
    let cases = support::live_get(world, actors, "/review-cases?kind=result&state=pending").await?;
    let case = items(&cases)?
        .iter()
        .find(|case| case["hypothesis"] == first)
        .ok_or("first result absent")?;
    let path = format!(
        "{}/review-cases/{}/decisions",
        world.base(),
        string(&case["id"])?
    );
    let body = json!({"review_case_id":case["id"],"evidence_revision":case["subject_revision"],"action":"promote","reason":"Both languages hold."});
    let promoted = world
        .api(Call::post(
            "/api/projects/{slug}/review-cases/{case_id}/decisions",
            path.clone(),
            &actors.admin,
            body,
            201,
        ))
        .await?
        .body;
    let decision = &promoted["decisions"][0]["id"];
    world.api(Call::post("/api/projects/{slug}/review-cases/{case_id}/decisions",path,&actors.admin,json!({"review_case_id":case["id"],"evidence_revision":case["subject_revision"],"action":"reject","reason":"A rerun shows a loss.","supersedes":decision}),201)).await?;
    let attention = support::live_get(world, actors, "/attention").await?;
    let outcomes = attention["recent_outcomes"]
        .as_array()
        .ok_or("outcomes absent")?;
    let about: Vec<_> = outcomes
        .iter()
        .filter(|row| row["hypothesis"] == first)
        .collect();
    assert_eq!(about.len(), 1);
    assert_eq!(about[0]["action"], "reject");
    assert_eq!(about[0]["reason"], "A rerun shows a loss.");
    let number = world.queue(actors, "Bare running hypothesis").await?;
    let lease = world.claim(actors, number, false).await?;
    let running = support::live_get(world, actors, "/attention").await?;
    assert_eq!(running["running_count"], 1);
    assert_eq!(running["running"][0]["hypothesis"], number);
    assert_eq!(running["running"][0]["state"], "claimed");
    let search = world
        .h
        .request(Method::GET, "/api/search")?
        .bearer_auth(&actors.admin)
        .query(&[("q", format!("{}#{number}.1", world.project))])
        .send()
        .await?;
    let search = world
        .h
        .check_response(Method::GET, "/api/search", search, 200)
        .await?
        .body;
    assert_eq!(items(&search)?.len(), 1);
    assert_eq!(search["items"][0]["kind"], "attempt");
    world
        .api(
            Call::post(
                &format!("{}/release", lifecycle_support::ATTEMPT),
                format!("{}/release", world.attempt_path(&lease)),
                &actors.agent,
                json!({"reason":"Out of memory."}),
                200,
            )
            .lease(&lease)?,
        )
        .await?;
    let mut draft: Value =
        serde_json::from_str(include_str!("../../../examples/fixture/hypothesis.json"))?;
    draft["title"] = json!("Late pending idea");
    world
        .api(Call::post(
            "/api/projects/{slug}/hypotheses",
            format!("{}/hypotheses", world.base()),
            &actors.agent,
            draft,
            201,
        ))
        .await?;
    let summary = support::live_get(world, actors, "/attention").await?;
    assert_eq!(
        summary["pending_counts"],
        json!({"draft":1,"result":2,"failure":1})
    );
    assert_eq!(summary["recent_failures"][0]["hypothesis"], number);
    assert_eq!(
        summary["recent_failures"][0]["hypothesis_state"],
        "awaiting_human_review"
    );
    let pending = summary["pending_reviews"]
        .as_array()
        .ok_or("pending absent")?;
    assert_eq!(pending.last().ok_or("pending empty")?["kind"], "draft");
    assert_eq!(pending[pending.len() - 2]["kind"], "failure");
    for pair in pending.windows(2) {
        assert!(pair[0]["opened_at"].as_str() <= pair[1]["opened_at"].as_str());
    }
    Ok(())
}
