#![forbid(unsafe_code)]
#[path = "support/identity_security_support.rs"]
#[allow(dead_code, reason = "Shared browser bootstrap covers other profiles")]
mod identity;
#[path = "support/lifecycle_support.rs"]
#[allow(dead_code, reason = "Shared lifecycle support covers other scenarios")]
mod lifecycle;
#[path = "support/runner_security_semantics.rs"]
#[allow(dead_code, reason = "Shared actual CLI fixture covers other suites")]
mod runner;
#[path = "support/mcp_semantics.rs"]
mod support;
use conformance::Result;
use lifecycle::{Call, Lease, World, string};
use reqwest::Method;
use serde_json::{Value, json};

#[tokio::test]
#[ignore = "requires API, browser OIDC and MCP"]
#[allow(
    clippy::too_many_lines,
    reason = "Named-token refusals, replay and attribution share one audited project"
)]
async fn mcp_named_tokens_client_sanitization_and_keyed_drafts() -> Result<()> {
    let (mut world, actors) = World::new("mcp-attribution").await?;
    let mut identity = identity::Context::new()?;
    let browser = support::admin(&mut identity).await?;
    let desk = string(
        &identity
            .mint(&browser, "laptop", &["read", "write"])
            .await?["token"],
    )?;
    let agent =
        support::service_token(&mut identity, &browser, &world.project, "agent", "codex-ci")
            .await?;
    let readonly = string(&identity.mint(&browser, "read-desk", &["read"]).await?["token"])?;
    for name in [
        "a;b",
        "tab\there",
        "bell\u{7}",
        &"x".repeat(101),
        "rtl\u{202e}override",
    ] {
        identity
            .browser_api(
                Method::POST,
                "/api/tokens",
                "/api/tokens",
                &browser,
                Some(json!({"name":name,"expires_in_days":1,"scopes":["read"]})),
                422,
            )
            .await?;
    }
    identity
        .browser_api(
            Method::POST,
            "/api/projects/{slug}/service-accounts/{name}/tokens",
            &format!("{}/service-accounts/agent/tokens", world.base()),
            &browser,
            Some(json!({"name":"ci;token:admin","expires_in_days":1,"scopes":["read"]})),
            422,
        )
        .await?;
    let cursor = world.h.fetch_audit(&actors.admin, 0).await?.body["next_after"]
        .as_u64()
        .ok_or("cursor missing")?;
    let mut document = runner::fixture("examples/fixture/hypothesis.json")?;
    document["title"] = json!("MCP keyed attribution");
    let args = json!({"project":world.project,"document":document,"idempotency_key":"named-draft"});
    let first = support::rpc(
        &world.h,
        &desk,
        "create_draft",
        args.clone(),
        Some("desktop/1.0"),
    )
    .await?;
    let draft = support::success(&first, false)?;
    let replay = support::rpc(
        &world.h,
        &desk,
        "create_draft",
        args.clone(),
        Some("desktop/1.0"),
    )
    .await?;
    support::replay_body(&first, &replay)?;
    let mut changed = args.clone();
    changed["document"]["title"] = json!("Other content");
    support::error(
        &support::rpc(&world.h, &desk, "create_draft", changed, None).await?,
        "conflict",
    );
    support::error(
        &support::rpc(
            &world.h,
            &readonly,
            "create_draft",
            json!({"project":world.project,"document":document}),
            None,
        )
        .await?,
        "forbidden",
    );
    let service = support::success(
        &support::rpc(
            &world.h,
            &agent,
            "create_draft",
            json!({"project":world.project,"document":document}),
            Some("agent/1.0"),
        )
        .await?,
        false,
    )?;
    assert_eq!(
        draft["created_by"],
        json!({"kind":"user","id":browser.me["user"]["id"]})
    );
    assert_eq!(service["created_by"]["kind"], "service");
    let mut comments = Vec::new();
    let long = "Z".repeat(250);
    for (body, ua, expected) in [
        (
            "semicolon",
            Some("evil/1.0; token:admin"),
            "token:laptop; ua:evil/1.0 token:admin".to_owned(),
        ),
        ("no-agent", None, "token:laptop".to_owned()),
        (
            "truncated",
            Some(long.as_str()),
            format!("token:laptop; ua:{}", "Z".repeat(200)),
        ),
        ("blank", Some("   "), "token:laptop".to_owned()),
    ] {
        let comment = support::success(
            &support::rpc(
                &world.h,
                &desk,
                "comment",
                json!({"project":world.project,"number":draft["number"],"body_markdown":body}),
                ua,
            )
            .await?,
            false,
        )?;
        comments.push((comment["id"].clone(), expected));
    }
    let mut forged = args;
    forged
        .as_object_mut()
        .ok_or("args missing")?
        .insert("via_client".into(), json!("token:admin"));
    let malformed = world
        .h
        .rpc(
            Some(&desk),
            "tools/call",
            json!({"name":"create_draft","arguments":forged}),
        )
        .await?;
    assert_eq!(malformed.status(), 200);
    let malformed: Value = malformed.json().await?;
    assert_eq!(malformed["error"]["code"], -32602);
    let forbidden_path = match std::env::var("CANNERY_CONFORMANCE_SERVER_IMPLEMENTATION")?.as_str()
    {
        "rust" => "",
        "python" => "via_client",
        _ => return Err("unknown MCP conformance implementation".into()),
    };
    assert!(
        malformed["error"]["data"]
            .as_array()
            .ok_or("validation details missing")?
            .iter()
            .any(|item| item["path"] == forbidden_path)
    );
    assert!(!serde_json::to_string(&malformed)?.contains("token:admin"));
    let rest = world
        .api(Call::post(
            "/api/projects/{slug}/hypotheses/{number}/comments",
            format!("{}/hypotheses/{}/comments", world.base(), draft["number"]),
            &desk,
            json!({"body_markdown":"REST retains api channel"}),
            201,
        ))
        .await?
        .body;
    let audit = support::audit(&mut world, &actors.admin, cursor).await?;
    let items = audit.as_array().ok_or("audit items missing")?;
    assert_eq!(
        items
            .iter()
            .filter(|row| row["action"] == "hypothesis.draft_created")
            .count(),
        2
    );
    assert_eq!(
        items
            .iter()
            .filter(|row| row["action"] == "comment.created")
            .count(),
        5
    );
    let human = support::rows(&audit, "hypothesis.draft_created", &draft["id"])?;
    assert_eq!(human.len(), 1);
    support::actor(
        human[0],
        "user",
        &browser.me["user"]["id"],
        "mcp",
        "token:laptop; ua:desktop/1.0",
    );
    let service_rows = support::rows(&audit, "hypothesis.draft_created", &service["id"])?;
    assert_eq!(service_rows.len(), 1);
    support::actor(
        service_rows[0],
        "service",
        &service["created_by"]["id"],
        "mcp",
        "token:codex-ci; ua:agent/1.0",
    );
    for (id, client) in comments {
        let rows = support::rows(&audit, "comment.created", &id)?;
        assert_eq!(rows.len(), 1);
        support::actor(rows[0], "user", &browser.me["user"]["id"], "mcp", &client);
    }
    let rows = support::rows(&audit, "comment.created", &rest["id"])?;
    assert_eq!(rows.len(), 1);
    support::actor(
        rows[0],
        "user",
        &browser.me["user"]["id"],
        "api",
        "token:laptop",
    );
    world
        .finish_coverage(&actors.admin, "mcp-attribution")
        .await
}

#[tokio::test]
#[ignore = "requires API, real tester CLI and MCP"]
#[allow(
    clippy::too_many_lines,
    reason = "One real test/evaluation/review lifecycle establishes claim and completion replays"
)]
async fn mcp_job_claim_completion_and_human_decision_replay_metadata() -> Result<()> {
    let mut rig = runner::Rig::new("mcp-replay", runner::producer()?, runner::science()?).await?;
    let number = rig.submit().await?;
    runner::successful_process(&rig.run().await?);
    let tested = rig.job(number).await?;
    if tested["state"] != "completed" {
        let category = match tested["error_reason"].as_str() {
            Some("runner API returned an invalid contract") => "contract",
            Some("runner API returned HTTP 404") => "HTTP404",
            Some("runner API returned HTTP 422") => "HTTP422",
            Some("runner filesystem operation failed") => "filesystem",
            _ => "other",
        };
        return Err(format!(
            "tester workflow did not complete: code={}, step={}, category={category}",
            tested["error_code"].as_str().unwrap_or("absent"),
            tested["error_step"].as_str().unwrap_or("absent")
        )
        .into());
    }
    let mut identity = identity::Context::new()?;
    let browser = support::admin(&mut identity).await?;
    let desk = string(&identity.mint(&browser, "desk", &["read", "write"]).await?["token"])?;
    let evaluator = support::service_token(
        &mut identity,
        &browser,
        &rig.world.project,
        "stock-evaluator",
        "eval-bot",
    )
    .await?;
    let cursor = rig.world.h.fetch_audit(&rig.actors.admin, 0).await?.body["next_after"]
        .as_u64()
        .ok_or("cursor missing")?;
    let claim_args = json!({"project":rig.world.project,"stage":"evaluator","revision":"fixture-policy-1","idempotency_key":"evaluation-claim"});
    let first = support::rpc(
        &rig.world.h,
        &evaluator,
        "claim_job",
        claim_args.clone(),
        Some("eval/1.0"),
    )
    .await?;
    let original = Lease::job(&support::success(&first, false)?)?;
    let replay = support::rpc(
        &rig.world.h,
        &evaluator,
        "claim_job",
        claim_args,
        Some("eval/1.0"),
    )
    .await?;
    let lease = Lease::job(&support::success(&replay, true)?)?;
    assert_eq!(lease.document["job_id"], original.document["job_id"]);
    assert_eq!(lease.generation, original.generation + 1);
    assert!(
        lease.token != original.token,
        "claim replay must rotate opaque lease token"
    );
    support::error(
        &support::rpc(
            &rig.world.h,
            &evaluator,
            "heartbeat_job",
            original.job_args(&rig.world.project),
            Some("eval/1.0"),
        )
        .await?,
        "stale_lease",
    );
    let mut input_args = lease.job_args(&rig.world.project);
    input_args["input"] = json!("evidence");
    let inputs = support::success(
        &support::rpc(
            &rig.world.h,
            &evaluator,
            "get_job_input",
            input_args,
            Some("eval/1.0"),
        )
        .await?,
        false,
    )?;
    assert_eq!(inputs.as_object().ok_or("wrapped list expected")?.len(), 1);
    assert_eq!(
        inputs["items"]
            .as_array()
            .ok_or("evidence list missing")?
            .len(),
        1
    );
    let record = json!({"schema_version":"0.2","attempt_id":lease.document["attempt_id"],"stage":"evaluator","status":"completed","producer":{"kind":"service","id":"stock-evaluator"},"started_at":"2026-09-29T01:00:00Z","finished_at":"2026-09-29T01:00:05Z","provenance":{"source_revision":"4f2a9c1","dataset_revision":"qrels-r1","control_revision":"fixture-r1","science_revision":"2"},"assessment":{"policy_revision":"fixture-policy-1","gates":[{"id":"synthetic-policy","result":"pass"}],"evidence":lease.document["inputs"]["evidence"],"verdict":"pass","reason":"MCP checked pinned tester evidence"}});
    let mut args = lease.job_args(&rig.world.project);
    args["document"] =
        json!({"schema_version":"0.2","job_id":lease.document["job_id"],"evidence":record});
    args["idempotency_key"] = json!("evaluation-complete");
    let done = support::rpc(
        &rig.world.h,
        &evaluator,
        "complete_job",
        args.clone(),
        Some("eval/1.0"),
    )
    .await?;
    assert_eq!(support::success(&done, false)?["state"], "completed");
    let keyed = support::rpc(
        &rig.world.h,
        &evaluator,
        "complete_job",
        args.clone(),
        Some("eval/1.0"),
    )
    .await?;
    support::replay_body(&done, &keyed)?;
    args.as_object_mut()
        .ok_or("args missing")?
        .remove("idempotency_key");
    let repeated = support::rpc(
        &rig.world.h,
        &evaluator,
        "complete_job",
        args,
        Some("eval/1.0"),
    )
    .await?;
    support::replay_body(&done, &repeated)?;
    let cases = support::success(
        &support::rpc(
            &rig.world.h,
            &desk,
            "list_review_cases",
            json!({"project":rig.world.project,"kind":"result"}),
            None,
        )
        .await?,
        false,
    )?;
    let case = cases["items"]
        .as_array()
        .ok_or("cases missing")?
        .first()
        .ok_or("result case missing")?;
    let decision = json!({"project":rig.world.project,"case_id":case["id"],"evidence_revision":case["subject_revision"],"action":"promote","reason":"Reviewed by the browser-minted person","idempotency_key":"human-promote"});
    let forbidden = support::rpc(
        &rig.world.h,
        &evaluator,
        "record_decision",
        decision.clone(),
        Some("eval/1.0"),
    )
    .await?;
    support::error(&forbidden, "forbidden");
    let decided = support::rpc(
        &rig.world.h,
        &desk,
        "record_decision",
        decision.clone(),
        Some("desk/1.0"),
    )
    .await?;
    support::success(&decided, false)?;
    let replay = support::rpc(
        &rig.world.h,
        &desk,
        "record_decision",
        decision,
        Some("desk/1.0"),
    )
    .await?;
    support::replay_body(&decided, &replay)?;
    let audit = support::audit(&mut rig.world, &rig.actors.admin, cursor).await?;
    let completed = support::rows(&audit, "job.completed", &lease.document["job_id"])?;
    assert_eq!(completed.len(), 1);
    let claimed = support::rows(&audit, "job.claimed", &lease.document["job_id"])?;
    assert_eq!(claimed.len(), 1);
    let services = rig
        .world
        .api(Call::get(
            "/api/projects/{slug}/service-accounts",
            format!("{}/service-accounts", rig.world.base()),
            &rig.actors.admin,
        ))
        .await?
        .body;
    let service = services["items"]
        .as_array()
        .ok_or("services missing")?
        .iter()
        .find(|service| service["name"] == "stock-evaluator")
        .ok_or("evaluator missing")?;
    for row in completed.into_iter().chain(claimed) {
        support::actor(
            row,
            "service",
            &service["id"],
            "mcp",
            "token:eval-bot; ua:eval/1.0",
        );
    }
    let decisions = audit
        .as_array()
        .ok_or("audit items missing")?
        .iter()
        .filter(|row| row["action"] == "attempt.decided")
        .collect::<Vec<_>>();
    assert_eq!(decisions.len(), 1);
    let reviews = support::rows(&audit, "review.promote", &case["id"])?;
    assert_eq!(reviews.len(), 1);
    support::actor(
        reviews[0],
        "user",
        &browser.me["user"]["id"],
        "mcp",
        "token:desk; ua:desk/1.0",
    );
    support::actor(
        decisions[0],
        "user",
        &browser.me["user"]["id"],
        "mcp",
        "token:desk; ua:desk/1.0",
    );
    rig.finish("mcp-replay").await
}
