use super::{import_support, lifecycle_support};
use conformance::Result;
use lifecycle_support::{ATTEMPT, Actors, Call, JOB, Lease, World, object, sha};
use reqwest::Method;
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::PathBuf};
pub const FIGURE: &[u8] = b"\x89PNG\r\n\x1a\nfixture figure";

pub fn items(body: &Value) -> Result<&Vec<Value>> {
    body["items"]
        .as_array()
        .ok_or_else(|| "items absent".into())
}

pub async fn query(
    world: &mut import_support::World,
    template: &str,
    path: &str,
    token: &str,
    params: &[(&str, String)],
    status: u16,
) -> Result<Value> {
    let response = world
        .h
        .request(Method::GET, path)?
        .bearer_auth(token)
        .query(params)
        .send()
        .await?;
    let checked = world
        .h
        .check_response(Method::GET, template, response, status)
        .await?;
    if status == 422 {
        assert_eq!(checked.body["error"]["code"], "validation_failed");
    }
    Ok(checked.body)
}

pub async fn walk(
    world: &mut import_support::World,
    template: &str,
    path: &str,
    token: &str,
    params: &[(&str, String)],
) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    let mut cursor: Option<String> = None;
    let mut seen = BTreeSet::new();
    for _ in 0..300 {
        let mut query_params = params.to_vec();
        query_params.push(("limit", "1".into()));
        if let Some(before) = cursor {
            query_params.push(("before", before));
        }
        let body = query(world, template, path, token, &query_params, 200).await?;
        assert!(items(&body)?.len() <= 1);
        rows.extend(items(&body)?.iter().cloned());
        if body["next_before"].is_null() {
            return Ok(rows);
        }
        let next = match &body["next_before"] {
            Value::String(value) => value.clone(),
            Value::Number(value) => value.to_string(),
            _ => return Err("unexpected cursor type".into()),
        };
        assert!(seen.insert(next.clone()), "pagination cursor repeated");
        assert_eq!(items(&body)?.len(), 1);
        cursor = Some(next);
    }
    Err("pagination exceeded bounded fixture size".into())
}

pub async fn role(world: &mut import_support::World, id: &str, role: &str) -> Result<()> {
    let admin = world.admin.token.clone();
    world
        .api(
            Method::PUT,
            "/api/projects/{slug}/members/{user_id}",
            &format!("{}/members/{id}", world.base()),
            &admin,
            Some(&json!({"role":role})),
            200,
        )
        .await?;
    Ok(())
}

pub async fn finish(world: &mut import_support::World, label: &str) -> Result<()> {
    world.audit(0).await?;
    let directory = PathBuf::from(std::env::var("CANNERY_CONFORMANCE_COVERAGE_DIR")?);
    fs::create_dir_all(&directory)?;
    fs::write(
        directory.join(format!("read-side-{label}.json")),
        serde_json::to_vec_pretty(world.h.coverage())?,
    )?;
    Ok(())
}

pub fn cursor(bytes: &[u8]) -> String {
    // URL-safe unpadded Base64, solely to transport the source-backed hostile cursor cases.
    const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for shift in (0..4).take(chunk.len() + 1) {
            out.push(char::from(DIGITS[((n >> (18 - 6 * shift)) & 63) as usize]));
        }
    }
    out
}

pub async fn live_get(world: &mut World, actors: &Actors, suffix: &str) -> Result<Value> {
    let path = format!("{}{}", world.base(), suffix);
    let bare = suffix.split('?').next().ok_or("suffix absent")?;
    let segments: Vec<_> = bare
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let template = match segments.as_slice() {
        ["dashboard", "views", _] => "/dashboard/views/{view_id}",
        ["hypotheses", _, "attempts", _] => "/hypotheses/{number}/attempts/{sequence}",
        ["hypotheses", _, "attempts", _, "report"] => {
            "/hypotheses/{number}/attempts/{sequence}/report"
        }
        _ => bare,
    };
    Ok(world
        .api(Call::get(
            &format!("/api/projects/{{slug}}{template}"),
            path,
            &actors.admin,
        ))
        .await?
        .body)
}

pub async fn submit_live(
    world: &mut World,
    actors: &Actors,
    title: &str,
    report: &str,
) -> Result<(i64, Lease)> {
    let number = world.queue(actors, title).await?;
    let lease = world.claim(actors, number, false).await?;
    let path = world.attempt_path(&lease);
    let bytes = include_bytes!("../../../../examples/fixture/candidate.json");
    let grant = world.api(Call::post(&format!("{ATTEMPT}/uploads"), format!("{path}/uploads"), &actors.agent, json!({"role":"candidate","name":"candidate.json","size_bytes":bytes.len(),"sha256":sha(bytes),"media_type":"application/json"}), 201).lease(&lease)?).await?.body;
    let artifact = world.put(&grant, bytes, false).await?;
    let grant=world.api(Call::post(&format!("{ATTEMPT}/uploads"),format!("{path}/uploads"),&actors.agent,json!({"role":"report_asset","name":"figure.png","size_bytes":FIGURE.len(),"sha256":sha(FIGURE),"media_type":"image/png"}),201).lease(&lease)?).await?.body;
    let figure = world.put(&grant, FIGURE, false).await?;
    let manifest = world.api(Call::post(&format!("{ATTEMPT}/manifest"), format!("{path}/manifest"), &actors.agent, json!({"schema_version":"0.2","attempt_id":lease.document["id"],"objects":[object(&artifact),object(&figure)]}), 201).lease(&lease)?).await?.body;
    let mut sheet: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/contracts/evidence_envelope/valid/agent.json"
    ))?;
    sheet["attempt_id"] = lease.document["id"].clone();
    sheet["manifest"] = manifest;
    sheet["provenance"]["science_revision"] = json!(lease.document["science_revision"].to_string());
    sheet["artifact_roles"] = json!(["candidate", "report_asset"]);
    sheet["measurements"] = json!([{"metric":"mrr","value":0.9,"authority":"agent_claim","unit":"ratio","direction":"higher","split":"dev","dimensions":{"language":"en"}}]);
    sheet["report"]["body_markdown"] = json!(report);
    world
        .api(
            Call::post(
                &format!("{ATTEMPT}/submission"),
                format!("{path}/submission"),
                &actors.agent,
                sheet,
                201,
            )
            .lease(&lease)?,
        )
        .await?;
    Ok((number, lease))
}

pub async fn complete_live(
    world: &mut World,
    actors: &Actors,
    attempt: &Lease,
    label: &str,
) -> Result<Vec<Value>> {
    if std::env::var("CANNERY_CONFORMANCE_STALLED_EVALUATION_SECONDS")? != "0" {
        return Err("read-side suite requires --stalled-evaluation-seconds 0 profile".into());
    }
    assert_eq!(
        live_get(world, actors, "/attention").await?["stalled_evaluation_count"],
        0
    );
    let test = world.claim_job(actors, false, false).await?;
    assert_eq!(test.document["attempt_id"], attempt.document["id"]);
    let revision = attempt.document["science_revision"].to_string();
    let measurements:Vec<Value> = [(json!({}),1.0,0.625,4),(json!({"language":"en"}),1.0,0.75,2),(json!({"language":"fr"}),1.0,0.5,2)].into_iter().map(|(dimensions,value,control,samples)| json!({"metric":"mrr","value":value,"control_value":control,"sample_count":samples,"authority":"tester_verified","unit":"ratio","direction":"higher","split":"dev","dimensions":dimensions})).collect();
    let evidence = json!({"schema_version":"0.2","attempt_id":attempt.document["id"],"stage":"tester","status":"completed","producer":{"kind":"service","id":"cannery-runner"},"started_at":"2026-09-29T00:00:00Z","finished_at":"2026-09-29T00:01:00Z","provenance":{"source_revision":"4f2a9c1","tester_revision":"runner-fixture-1","dataset_revision":"qrels-r1","control_revision":"fixture-r1","science_revision":revision},"observations":"Scored 4 queries independently","measurements":measurements,"discrepancies":[],"artifact_roles":["evidence"]});
    let bytes = serde_json::to_vec(&evidence)?;
    let path = world.job_path(&test)?;
    let grant = world.api(Call::post(&format!("{JOB}/uploads"), format!("{path}/uploads"), &actors.tester, json!({"role":"evidence","path":"fixture-scorer/evidence/evidence.json","size_bytes":bytes.len(),"sha256":sha(&bytes),"media_type":"application/json"}),201).lease(&test)?).await?.body;
    let artifact = world.put(&grant, &bytes, true).await?;
    let bytes = b"scored four queries\n";
    let grant = world.api(Call::post(&format!("{JOB}/uploads"),format!("{path}/uploads"),&actors.tester,json!({"role":"step_log","path":"fixture-scorer/step_log/fixture-scorer.log","size_bytes":bytes.len(),"sha256":sha(bytes),"media_type":"text/plain"}),201).lease(&test)?).await?.body;
    let log = world.put(&grant, bytes, true).await?;
    let bytes = b"{\"query\":\"q1\",\"language\":\"en\",\"reciprocal_rank\":1}\n";
    let grant=world.api(Call::post(&format!("{JOB}/uploads"),format!("{path}/uploads"),&actors.tester,json!({"role":"per_query_results","path":"fixture-scorer/per_query_results/scores.jsonl","size_bytes":bytes.len(),"sha256":sha(bytes),"media_type":"application/jsonl"}),201).lease(&test)?).await?.body;
    let results = world.put(&grant, bytes, true).await?;
    complete(world,&test,&actors.tester,json!({"schema_version":"0.2","job_id":test.document["job_id"],"evidence":evidence,"manifest":{"schema_version":"0.2","attempt_id":attempt.document["id"],"objects":[object(&artifact),object(&log),object(&results)]}})).await?;
    let waiting = live_get(world, actors, "/attention").await?;
    assert_eq!(waiting["stalled_evaluation_count"], 1);
    let stalled = &waiting["stalled_evaluations"][0];
    let number = attempt.document["number"]
        .as_i64()
        .ok_or("attempt number absent")?;
    assert_eq!(stalled["attempt_ref"], format!("#{number}.1"));
    assert_eq!(stalled["evaluator"], "stock-evaluator");
    assert_eq!(stalled["revision"], "fixture-policy-1");
    assert_eq!(
        stalled["message"],
        format!(
            "evaluation for #{number} waits for evaluator stock-evaluator revision fixture-policy-1"
        )
    );
    let evaluation = world.claim_job(actors, true, false).await?;
    assert_eq!(evaluation.document["attempt_id"], attempt.document["id"]);
    let comparisons:Vec<Value> = [(json!({}),0.625),(json!({"language":"en"}),0.75),(json!({"language":"fr"}),0.5)].into_iter().map(|(dimensions,value)|json!({"metric":"mrr","split":"dev","dimensions":dimensions,"value":1.0,"source":"tester","reference":{"value":value,"label":label,"kind":"baseline","ref":"base-camp"}})).collect();
    let record = json!({"schema_version":"0.2","attempt_id":attempt.document["id"],"stage":"evaluator","status":"completed","producer":{"kind":"service","id":"stock-evaluator"},"started_at":"2026-09-29T01:00:00Z","finished_at":"2026-09-29T01:00:05Z","provenance":{"source_revision":"4f2a9c1","dataset_revision":"qrels-r1","control_revision":"fixture-r1","science_revision":revision},"assessment":{"policy_revision":"fixture-policy-1","gates":[{"id":"synthetic-policy","result":"pass"}],"comparisons":comparisons,"evidence":evaluation.document["inputs"]["evidence"],"verdict":"pass","reason":"Verified comparison against published reference"}});
    complete(
        world,
        &evaluation,
        &actors.evaluator,
        json!({"schema_version":"0.2","job_id":evaluation.document["job_id"],"evidence":record}),
    )
    .await?;
    assert_eq!(
        live_get(world, actors, "/attention").await?["stalled_evaluation_count"],
        0
    );
    Ok(vec![artifact, log, results])
}

async fn complete(world: &mut World, lease: &Lease, token: &str, body: Value) -> Result<()> {
    let response = world
        .h
        .request(
            Method::POST,
            &format!("{}/completion", world.job_path(lease)?),
        )?
        .bearer_auth(token)
        .header("X-Lease-Token", &lease.token)
        .header("X-Lease-Generation", lease.generation.to_string())
        .json(&body)
        .send()
        .await?;
    let status = response.status().as_u16();
    if status != 200 {
        let body: Value = response.json().await?;
        let paths: Vec<_> = body["error"]["details"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|detail| detail["path"].as_str())
            .collect();
        return Err(format!(
            "job completion status {status}, code {:?}, paths {paths:?}",
            body["error"]["code"].as_str()
        )
        .into());
    }
    world
        .h
        .check_response(Method::POST, &format!("{JOB}/completion"), response, 200)
        .await?;
    Ok(())
}

pub async fn live_page_check(
    world: &mut World,
    token: &str,
    template: &str,
    path: &str,
    params: &[(&str, String)],
) -> Result<()> {
    async fn get(
        world: &mut World,
        token: &str,
        template: &str,
        path: &str,
        params: &[(&str, String)],
        status: u16,
    ) -> Result<Value> {
        let response = world
            .h
            .request(Method::GET, path)?
            .bearer_auth(token)
            .query(params)
            .send()
            .await?;
        Ok(world
            .h
            .check_response(Method::GET, template, response, status)
            .await?
            .body)
    }
    fn comparable(rows: &[Value]) -> Vec<Value> {
        rows.iter()
            .cloned()
            .map(|mut row| {
                if let Some(object) = row.as_object_mut() {
                    object.remove("last_used_at");
                }
                row
            })
            .collect()
    }
    // Token reads move last_used_at; source pytest intentionally excludes that field too.
    let mut full = params.to_vec();
    full.push(("limit", "200".into()));
    let all = get(world, token, template, path, &full, 200).await?;
    assert!(
        !items(&all)?.is_empty(),
        "pagination fixture empty for {template}"
    );
    let mut before: Option<String> = None;
    let mut seen = BTreeSet::new();
    let mut found = Vec::new();
    for _ in 0..300 {
        let mut query = params.to_vec();
        query.push(("limit", "1".into()));
        if let Some(cursor) = before {
            query.push(("before", cursor));
        }
        let page = get(world, token, template, path, &query, 200).await?;
        assert!(items(&page)?.len() <= 1);
        found.extend(items(&page)?.iter().cloned());
        if page["next_before"].is_null() {
            break;
        }
        let cursor = match &page["next_before"] {
            Value::String(value) => value.clone(),
            Value::Number(value) => value.to_string(),
            _ => return Err("unexpected list cursor".into()),
        };
        assert!(seen.insert(cursor.clone()));
        before = Some(cursor);
    }
    assert_eq!(comparable(&found), comparable(items(&all)?));
    let default = get(world, token, template, path, params, 200).await?;
    assert_eq!(items(&default)?.len(), items(&all)?.len().min(50));
    for limit in ["0", "201"] {
        let mut query = params.to_vec();
        query.push(("limit", limit.into()));
        let refused = get(world, token, template, path, &query, 422).await?;
        assert_eq!(refused["error"]["code"], "validation_failed");
    }
    Ok(())
}
