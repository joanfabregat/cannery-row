#![forbid(unsafe_code)]
#[path = "support/lifecycle_support.rs"]
#[allow(dead_code, reason = "Shared HTTP-only bootstrap")]
mod support;
use conformance::Result;
use reqwest::Method;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use support::{ATTEMPT, Actors, Call, World, object, sha};
use tokio::task::JoinSet;

const DRAFT: &str = "/api/projects/{slug}/hypotheses";
fn hypothesis(track: &str) -> Result<Value> {
    let mut document: Value =
        serde_json::from_str(include_str!("../../../examples/fixture/hypothesis.json"))?;
    document["track"] = json!(track);
    Ok(document)
}
#[tokio::test]
#[ignore = "requires the URL-driven API and OIDC fixture"]
async fn concurrent_drafts_number_per_project_across_tracks() -> Result<()> {
    let (mut world, actors) = World::new("concurrent-numbering").await?;
    world.api(Call::post(
        "/api/projects/{slug}/tracks",format!("{}/tracks",world.base()),&actors.admin,
        json!({"slug":"other","title":"Other track","producer":{"name":"overlap-producer","revision":1}}),201,
    )).await?;
    let mut tasks = JoinSet::new();
    for index in 0..8 {
        let request = world
            .h
            .request(Method::POST, &format!("{}/hypotheses", world.base()))?
            .bearer_auth(&actors.agent)
            .json(&hypothesis(if index % 2 == 0 {
                "lexical"
            } else {
                "other"
            })?);
        tasks.spawn(async move { request.send().await });
    }
    let mut numbers = BTreeSet::new();
    while let Some(response) = tasks.join_next().await {
        let checked = world
            .h
            .check_response(Method::POST, DRAFT, response??, 201)
            .await?;
        numbers.insert(
            checked.body["number"]
                .as_i64()
                .ok_or("draft number absent")?,
        );
    }
    assert_eq!(numbers, (1..=8).collect());
    world
        .finish_coverage(&actors.admin, "concurrent-numbering")
        .await
}

#[tokio::test]
#[ignore = "requires the URL-driven API and OIDC fixture"]
async fn concurrent_idempotency_replays_one_draft_and_binds_its_project() -> Result<()> {
    let (mut world, actors) = World::new("concurrent-idempotency").await?;
    let (other, _) = World::new("concurrent-other").await?;
    let key = "concurrent-shared-draft-key";
    let document = hypothesis("lexical")?;
    let mut tasks = JoinSet::new();
    for _ in 0..4 {
        let request = world
            .h
            .request(Method::POST, &format!("{}/hypotheses", world.base()))?
            .bearer_auth(&actors.admin)
            .header("Idempotency-Key", key)
            .json(&document);
        tasks.spawn(async move { request.send().await });
    }
    let mut statuses = Vec::new();
    let mut numbers = BTreeSet::new();
    while let Some(response) = tasks.join_next().await {
        let response = response??;
        let status = response.status().as_u16();
        assert!([200, 201].contains(&status));
        let checked = world
            .h
            .check_response(Method::POST, DRAFT, response, status)
            .await?;
        statuses.push(status);
        numbers.insert(
            checked.body["number"]
                .as_i64()
                .ok_or("draft number absent")?,
        );
    }
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 200, 200, 201]);
    assert_eq!(numbers, BTreeSet::from([1]));
    let refused = world
        .api(
            Call::post(
                DRAFT,
                format!("{}/hypotheses", other.base()),
                &actors.admin,
                document.clone(),
                409,
            )
            .key(key)?,
        )
        .await?;
    assert_eq!(refused.body["error"]["code"], "conflict");
    let mut changed = document;
    changed["title"] = json!("Different concurrent payload");
    let refused = world
        .api(
            Call::post(
                DRAFT,
                format!("{}/hypotheses", world.base()),
                &actors.admin,
                changed,
                409,
            )
            .key(key)?,
        )
        .await?;
    assert_eq!(refused.body["error"]["code"], "conflict");
    let audit = world.h.fetch_audit(&actors.admin, 0).await?;
    assert_eq!(
        audit.body["items"]
            .as_array()
            .ok_or("audit absent")?
            .iter()
            .filter(|event| event["action"] == "hypothesis.draft_created"
                && event["idempotency_key"] == key)
            .count(),
        1
    );
    world
        .finish_coverage(&actors.admin, "concurrent-idempotency")
        .await
}

#[tokio::test]
#[ignore = "requires the URL-driven API and OIDC fixture"]
async fn concurrent_attempt_claims_issue_one_lease_and_one_audit_event() -> Result<()> {
    let (mut world, actors) = World::new("concurrent-attempt-claim").await?;
    let number = world
        .queue(&actors, "One queued hypothesis for six workers")
        .await?;
    let template = "/api/projects/{slug}/claims";
    let mut tasks = JoinSet::new();
    for token in [&actors.agent, &actors.other_agent, &actors.admin].repeat(2) {
        let request = world
            .h
            .request(Method::POST, &format!("{}/claims", world.base()))?
            .bearer_auth(token)
            .json(&json!({}));
        tasks.spawn(async move { request.send().await });
    }
    let mut statuses = Vec::new();
    let mut winner = Value::Null;
    while let Some(response) = tasks.join_next().await {
        let response = response??;
        let status = response.status().as_u16();
        assert!([201, 409].contains(&status));
        let checked = world
            .h
            .check_response(Method::POST, template, response, status)
            .await?;
        statuses.push(status);
        if status == 201 {
            winner = checked.body["attempt"].clone();
        } else {
            assert_eq!(checked.body["error"]["code"], "nothing_to_claim");
        }
    }
    statuses.sort_unstable();
    assert_eq!(statuses, [201, 409, 409, 409, 409, 409]);
    assert_eq!(winner["lease_generation"], 1);
    assert_eq!(winner["sequence"], 1);
    let listed = world
        .api(Call::get(
            "/api/projects/{slug}/hypotheses/{number}/attempts",
            format!("{}/hypotheses/{number}/attempts", world.base()),
            &actors.admin,
        ))
        .await?;
    let attempts = listed.body["items"]
        .as_array()
        .ok_or("attempt list missing")?;
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0]["id"], winner["id"]);
    let audit = world.h.fetch_audit(&actors.admin, 0).await?;
    assert_eq!(
        audit.body["items"]
            .as_array()
            .ok_or("audit list missing")?
            .iter()
            .filter(
                |event| event["action"] == "attempt.claimed" && event["subject_id"] == winner["id"]
            )
            .count(),
        1
    );
    world
        .finish_coverage(&actors.admin, "concurrent-attempt-claim")
        .await
}

async fn queued_job(world: &mut World, actors: &Actors) -> Result<String> {
    let number = world.queue(actors, "One test job for four workers").await?;
    let lease = world.claim(actors, number, false).await?;
    let path = world.attempt_path(&lease);
    let bytes = include_bytes!("../../../examples/fixture/candidate.json");
    let grant = world.api(Call::post(
        &format!("{ATTEMPT}/uploads"), format!("{path}/uploads"), &actors.agent,
        json!({"role":"candidate","name":"candidate.json","size_bytes":bytes.len(),"sha256":sha(bytes),"media_type":"application/json"}),201,
    ).lease(&lease)?).await?.body;
    let artifact = world.put(&grant, bytes, false).await?;
    let manifest = world.api(Call::post(
        &format!("{ATTEMPT}/manifest"), format!("{path}/manifest"), &actors.agent,
        json!({"schema_version":"0.2","attempt_id":lease.document["id"],"objects":[object(&artifact)]}),201,
    ).lease(&lease)?).await?.body;
    let mut sheet: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/contracts/evidence_envelope/valid/agent.json"
    ))?;
    sheet["attempt_id"] = lease.document["id"].clone();
    sheet["manifest"] = manifest;
    sheet["provenance"]["science_revision"] = json!("1");
    sheet["artifact_roles"] = json!(["candidate"]);
    sheet["measurements"] = json!([{"metric":"mrr","value":0.9,"authority":"agent_claim","unit":"ratio","direction":"higher","split":"dev","dimensions":{"language":"en"}}]);
    world
        .api(
            Call::post(
                &format!("{ATTEMPT}/submission"),
                format!("{path}/submission"),
                &actors.agent,
                sheet,
                201,
            )
            .lease(&lease)?
            .key("concurrent-job-submission")?,
        )
        .await?;
    Ok(path)
}

#[tokio::test]
#[ignore = "requires the URL-driven API and OIDC fixture"]
async fn concurrent_job_claims_issue_one_generation_one_lease() -> Result<()> {
    let (mut world, actors) = World::new("concurrent-job-claim").await?;
    let path = queued_job(&mut world, &actors).await?;
    let template = "/api/projects/{slug}/jobs/claims";
    let mut tasks = JoinSet::new();
    for _ in 0..4 {
        let request = world
            .h
            .request(Method::POST, &format!("{}/jobs/claims", world.base()))?
            .bearer_auth(&actors.tester)
            .json(&json!({}));
        tasks.spawn(async move { request.send().await });
    }
    let mut statuses = Vec::new();
    let mut winner = Value::Null;
    while let Some(response) = tasks.join_next().await {
        let response = response??;
        let status = response.status().as_u16();
        assert!([201, 409].contains(&status));
        let checked = world
            .h
            .check_response(Method::POST, template, response, status)
            .await?;
        statuses.push(status);
        if status == 201 {
            winner = checked.body["job"].clone();
        } else {
            assert_eq!(checked.body["error"]["code"], "conflict");
        }
    }
    statuses.sort_unstable();
    assert_eq!(statuses, [201, 409, 409, 409]);
    assert_eq!(winner["lease"]["generation"], 1);
    let listed = world
        .api(Call::get(
            &format!("{ATTEMPT}/jobs"),
            format!("{path}/jobs"),
            &actors.admin,
        ))
        .await?;
    let jobs = listed.body["items"].as_array().ok_or("job list missing")?;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["id"], winner["job_id"]);
    assert_eq!(jobs[0]["state"], "claimed");
    assert_eq!(jobs[0]["lease_generation"], 1);
    let audit = world.h.fetch_audit(&actors.admin, 0).await?;
    assert_eq!(
        audit.body["items"]
            .as_array()
            .ok_or("audit list missing")?
            .iter()
            .filter(
                |event| event["action"] == "job.claimed" && event["subject_id"] == winner["job_id"]
            )
            .count(),
        1
    );
    world
        .finish_coverage(&actors.admin, "concurrent-job-claim")
        .await
}
