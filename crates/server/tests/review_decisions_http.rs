//! Native decision controller against the immutable production source oracle.
#![forbid(unsafe_code)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use cannery_core::{
    contracts::{
        ContractValidator,
        phases::{Phase, PhaseSchemas},
    },
    json,
    settings::load_settings,
};
use cannery_server::{
    application_with_review_decision_context, review_attention_routes::ReviewAttentionContext,
    review_attention_wire::ResponseContext, review_decision_routes::ReviewDecisionContext,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Acquire, PgPool};
use std::fmt::Write;
use std::{collections::BTreeMap, error::Error, sync::Arc};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
fn fixture() -> Result<Value> {
    Ok(serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/review_decisions_http/reference.json"
    ))?)
}
#[test]
fn ordinary_review_seed_uses_published_verification_and_log_models() -> Result<()> {
    let seed = include_str!("fixtures/review_decisions_http/seed.sql");
    let front_matter = seed
        .split('\'')
        .find(|v| v.starts_with("{\"verdict\":\"pass\""))
        .ok_or("published seed verification report")?;
    let violations = PhaseSchemas::new()?.violations(
        Phase::Verification,
        &serde_json::from_str::<Value>(front_matter)?,
    );
    assert!(
        violations.is_empty(),
        "ordinary fixture must satisfy the published verification contract"
    );
    let logs = seed
        .split('\'')
        .find(|v| v.starts_with("[{\"key\":\"fixture\","))
        .ok_or("published seed logs")?;
    let model: Vec<cannery_server::api_models::LogRef> = serde_json::from_str(logs)?;
    assert_eq!(
        serde_json::to_value(model)?,
        serde_json::from_str::<Value>(logs)?
    );
    Ok(())
}
fn profile() -> Result<ReviewDecisionContext> {
    Ok(ReviewDecisionContext {
        reads: ReviewAttentionContext {
            reviews: cannery_reviews::JsonContext {
                decode_nesting_budget: 80,
            },
            units: cannery_units::repo::JsonContext {
                encode_nesting_budget: 80,
                decode_nesting_budget: 80,
            },
            attempts: cannery_attempts::model::JsonContext {
                encode_nesting_budget: 80,
                decode_nesting_budget: 80,
            },
            response: ResponseContext {
                inferred_nesting_budget: 80,
            },
        },
        contracts: ContractValidator::new()?,
        phases: PhaseSchemas::new()?,
        validation_walk_budget: 80,
        repr_budget: 80,

        request_hash_budget: 80,
        config: cannery_research::config_repo::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
        science_rendering: cannery_research::science::RenderingContext { nesting_budget: 80 },
        jobs: cannery_jobs::repo::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
        audit_encoding_budget: 80,
        lifecycle: std::sync::Arc::new(cannery_server::job_lifecycle::Context {
            jobs: cannery_jobs::repo::JsonContext {
                encode_nesting_budget: 80,
                decode_nesting_budget: 80,
            },
            attempts: cannery_attempts::model::JsonContext {
                encode_nesting_budget: 80,
                decode_nesting_budget: 80,
            },
            config: cannery_research::config_repo::JsonContext {
                encode_nesting_budget: 80,
                decode_nesting_budget: 80,
            },
            units: cannery_units::repo::JsonContext {
                encode_nesting_budget: 80,
                decode_nesting_budget: 80,
            },
            rendering: cannery_research::science::RenderingContext { nesting_budget: 80 },
        }),
    })
}
fn hex(bytes: &[u8]) -> Result<String> {
    let mut out = String::new();
    for b in bytes {
        write!(&mut out, "{b:02x}")?;
    }
    Ok(out)
}
fn instant(s: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|v| v.to_rfc3339_opts(chrono::SecondsFormat::Micros, true))
}
fn projected(v: Value, ids: &BTreeMap<String, String>, clocks: &BTreeMap<String, String>) -> Value {
    match v {
        Value::Array(v) => Value::Array(v.into_iter().map(|v| projected(v, ids, clocks)).collect()),
        Value::Object(v) => Value::Object(
            v.into_iter()
                .map(|(k, v)| (k, projected(v, ids, clocks)))
                .collect(),
        ),
        Value::String(mut s) => {
            if let Some(t) = instant(&s).and_then(|t| clocks.get(&t)) {
                return json!(t);
            }
            for (a, b) in ids {
                s = s.replace(a, b);
            }
            json!(s)
        }
        Value::Number(n) if n.to_string().contains(['.', 'e', 'E']) => n
            .as_f64()
            .and_then(serde_json::Number::from_f64)
            .map_or(Value::Number(n), Value::Number),
        v => v,
    }
}
fn output(v: Value) -> Value {
    if v.get("error")
        .is_some_and(|v| v["code"] == "validation_failed")
    {
        let e = &v["error"];
        return json!({"error":{"code":e["code"],"details":e["details"].as_array().map(|v|v.iter().map(|v|json!({"path":v["path"]})).collect::<Vec<_>>())}});
    }
    v
}
fn native_validation_error(recipe: &Value) -> Result<Value> {
    assert!(matches!(
        recipe["name"].as_str(),
        Some("body-validation" | "auth-invalid-schema")
    ));
    assert_eq!(recipe["role"], "researcher");
    assert_eq!(recipe["number"], 1);
    assert_eq!(recipe["method"], "POST");
    assert_eq!(recipe["status"], 422);
    let ordinary = json!({"action":"retry","evidence_revision":1,"reason":"Reason é😀","review_case_id":"00000000-0000-0000-0000-000000000301"});
    // The message names what the schema expected: the missing case id, then
    // what each of the two input sets (a decision or a failure case) lacks.
    let forms = "missing required property \"review_case_id\"; matches none of the allowed forms: (1) missing required property \"document\"; (2) missing required properties \"evidence_revision\", \"action\", \"reason\"";
    let (body, path, source_count, message) = match recipe["native_validation_profile"].as_str() {
        Some("missing-fields") => (json!({}), "", 4, forms.to_owned()),
        Some("missing-extra") => {
            assert_eq!(recipe["name"], "body-validation");
            (
                json!({"extra":1}),
                "",
                5,
                forms.replace(
                    "; matches",
                    "; unexpected property \"extra\": the schema does not allow it; matches",
                ),
            )
        }
        Some("empty-reason") => {
            assert_eq!(recipe["name"], "body-validation");
            let mut body = ordinary;
            body["reason"] = json!("");
            (
                body,
                "/reason",
                2,
                "does not match the pattern \\S; is shorter than 1 character".to_owned(),
            )
        }
        _ => return Err("unknown native validator profile".into()),
    };
    assert_eq!(recipe["body"], body, "exact maintained-validator recipe");
    assert_eq!(
        recipe["response"],
        json!({"error":{"code":"validation_failed","details":vec![json!({"path":path});source_count]}}),
        "source observations remain exact and independent"
    );
    Ok(
        json!({"error":{"code":"validation_failed","message":"invalid human decision","details":[{"path":path,"message":message}]}}),
    )
}
struct Snapshot {
    ids: BTreeMap<String, String>,
    clocks: BTreeMap<String, String>,
    value: Value,
}
async fn raw_storage(pool: &PgPool) -> Result<Vec<String>> {
    let mut rows = Vec::new();
    for table in [
        "config_revisions",
        "units",
        "unit_revisions",
        "attempts",
        "attempt_failures",
        "phase_outputs",
        "review_cases",
        "decisions",
        "jobs",
        "audit_events",
        "idempotency_keys",
        "search_documents",
    ] {
        rows.push(sqlx::query_scalar::<_, String>(&format!(
            "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]')::text FROM {table} t"
        )).fetch_one(pool).await?);
    }
    Ok(rows)
}
fn request_hash(v: &Value) -> Result<String> {
    // Use the actual native lossless source-sorted envelope helper for verification;
    // the project is the fixed source fixture slug, and all request keys are sorted.
    let body = json::decode(&serde_json::to_vec(&v["decision"])?, 9994)?;
    hex(&cannery_server::review_decision_routes::reference_request_hash("matrix", &body, 80)?)
}
#[allow(
    clippy::too_many_lines,
    reason = "Compare all source storage and clock relations before projection"
)]
async fn snapshot(pool: &PgPool, requests: &BTreeMap<String, Value>) -> Result<Snapshot> {
    let mut rows = json!({});
    for (table, order) in [
        ("config_revisions", "project_id,kind,revision"),
        ("units", "project_id,number"),
        ("unit_revisions", "unit_id,revision"),
        ("attempts", "id"),
        ("attempt_failures", "id"),
        ("phase_outputs", "id"),
        ("review_cases", "id"),
        ("decisions", "review_case_id,decided_at,id"),
        ("jobs", "attempt_id,phase,run_number"),
        ("audit_events", "seq"),
        ("idempotency_keys", "scope,actor,key"),
        ("search_documents", "kind,id"),
    ] {
        rows[table] = sqlx::query_scalar::<_, Value>(&format!(
            "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY {order}),'[]') FROM {table} t"
        ))
        .fetch_one(pool)
        .await?;
    }
    let mut ids = BTreeMap::new();
    let mut clocks = BTreeMap::new();
    for (i, row) in rows["decisions"]
        .as_array()
        .ok_or("decisions")?
        .iter()
        .enumerate()
    {
        let id = row["id"].as_str().ok_or("id")?;
        if !id.starts_with("00000000-") {
            ids.insert(
                id.to_owned(),
                format!(
                    "@decision:{}:{i}",
                    row["review_case_id"].as_str().ok_or("case")?
                ),
            );
        }
    }
    for row in rows["jobs"].as_array().ok_or("jobs")? {
        let id = row["id"].as_str().ok_or("id")?;
        if !id.starts_with("00000000-") {
            ids.insert(
                id.to_owned(),
                format!(
                    "@job:{}:{}:{}",
                    row["attempt_id"].as_str().ok_or("attempt")?,
                    row["phase"].as_str().ok_or("phase")?,
                    row["run_number"]
                ),
            );
        }
    }
    for row in rows["idempotency_keys"].as_array_mut().ok_or("keys")? {
        let digest = row["request_hash"].as_str().ok_or("hash")?;
        let original = requests.get(digest).ok_or("unobserved request hash")?;
        assert_eq!(digest, format!("\\x{}", request_hash(original)?));
        let stable = projected(original.clone(), &ids, &BTreeMap::new());
        if &stable != original {
            row["request_hash"] = json!(format!("@request-hash:{}", request_hash(&stable)?));
        }
    }
    // Match the explicit source table insertion order, never arbitrary object-key order.
    for table in [
        "config_revisions",
        "units",
        "unit_revisions",
        "attempts",
        "attempt_failures",
        "phase_outputs",
        "review_cases",
        "decisions",
        "jobs",
        "audit_events",
        "idempotency_keys",
        "search_documents",
    ] {
        for row in rows[table].as_array().ok_or("table")? {
            for key in [
                "created_at",
                "updated_at",
                "resolved_at",
                "decided_at",
                "occurred_at",
                "finished_at",
            ] {
                if let Some(s) = row[key].as_str()
                    && chrono::DateTime::parse_from_rfc3339(s)
                        .is_ok_and(|v| chrono::Datelike::year(&v) >= 2026)
                {
                    let label = format!("@clock:{}", clocks.len());
                    clocks.entry(instant(s).ok_or("clock")?).or_insert(label);
                }
            }
        }
    }
    let relations:Value=sqlx::query_scalar("SELECT coalesce(jsonb_agg(jsonb_build_array(d.review_case_id,d.id,c.resolved_at<=d.decided_at,h.updated_at>=d.decided_at,(SELECT count(*) FROM audit_events a WHERE a.occurred_at=d.decided_at),c.resolved_at=d.decided_at) ORDER BY d.review_case_id,d.decided_at,d.id),'[]') FROM decisions d JOIN review_cases c ON c.id=d.review_case_id JOIN units h ON h.id=c.unit_id").fetch_one(pool).await?;
    for row in relations.as_array().ok_or("relations")? {
        assert_eq!(row[2], true);
        assert_eq!(row[3], true);
        if !row[1].as_str().ok_or("id")?.starts_with("00000000-") {
            assert!(row[4].as_i64().ok_or("audit count")? >= 2);
        }
    }
    let links:Value=sqlx::query_scalar("SELECT coalesce(jsonb_agg(jsonb_build_array(j.id,j.previous_run_id,j.run_number=p.run_number+1,j.phase=p.phase,(j.performer,j.verifier_id) IS NOT DISTINCT FROM (p.performer,p.verifier_id),j.spec->'inputs'->'run'->>'ref'=(SELECT o.id::text FROM phase_outputs o WHERE o.attempt_id=j.attempt_id AND o.stage='agent' AND o.status='completed' ORDER BY o.revision DESC LIMIT 1),j.created_at>=p.created_at,j.origin='human_retry') ORDER BY j.attempt_id,j.run_number),'[]') FROM jobs j JOIN jobs p ON p.id=j.previous_run_id").fetch_one(pool).await?;
    for row in links.as_array().ok_or("links")? {
        for v in row.as_array().ok_or("link")?.iter().skip(2) {
            assert_eq!(v, true);
        }
    }
    let value = projected(
        json!({"rows":rows,"clock_relations":relations,"job_link_relations":links}),
        &ids,
        &clocks,
    );
    Ok(Snapshot { ids, clocks, value })
}
#[allow(
    clippy::too_many_lines,
    reason = "Keep request transport and complete native refusal envelopes together"
)]
async fn call(
    app: &Router,
    r: &Value,
    ids: &BTreeMap<String, String>,
    requests: &mut BTreeMap<String, Value>,
) -> Result<(u16, Option<String>, Value, String)> {
    let reverse = ids
        .iter()
        .map(|(a, b)| (b.clone(), a.clone()))
        .collect::<BTreeMap<_, _>>();
    let body = projected(r["body"].clone(), &reverse, &BTreeMap::new());
    if r["key"].is_string() && body.is_object() {
        let envelope = json!({"project":"matrix","decision":body});
        requests.insert(format!("\\x{}", request_hash(&envelope)?), envelope);
    }
    let path = r["path"].as_str().map_or_else(
        || {
            format!(
                "/api/projects/matrix/review-cases/00000000-0000-0000-0000-{:012}/decisions",
                300 + r["number"].as_u64().unwrap_or(0)
            )
        },
        str::to_owned,
    );
    let mut request = Request::builder()
        .method(r["method"].as_str().ok_or("method")?)
        .uri(path)
        .header("content-type", "application/json");
    let role = r["role"].as_str().ok_or("role")?;
    if role != "none" {
        request = request.header(
            "authorization",
            format!(
                "Bearer {}track_http_{role}",
                if role.contains("agent") {
                    "cr_svc_"
                } else {
                    "cr_pat_"
                }
            ),
        );
    }
    if let Some(key) = r["key"].as_str() {
        request = request.header("idempotency-key", key);
    }
    let bytes = r["raw"]
        .as_str()
        .map_or_else(|| serde_json::to_vec(&body), |v| Ok(v.as_bytes().to_vec()))?;
    let native_error = if r["native_wire_profile"] == "unpaired-surrogate" {
        assert!(bytes.windows(6).any(|v| v == br"\ud800"));
        let Err(json::DecodeError::Syntax { position }) =
            json::decode(&bytes, cannery_server::body::REST_JSON_NESTING_BUDGET)
        else {
            return Err("surrogate profile must fail native syntax validation".into());
        };
        Some(
            json!({"error":{"code":"validation_failed","message":"request validation failed",
            "details":[{"path":format!("body/{position}"),"message":"JSON decode error"}]}}),
        )
    } else {
        assert!(r["native_wire_profile"].is_null());
        None
    };
    let response = app
        .clone()
        .oneshot(request.body(Body::from(bytes))?)
        .await?;
    let status = response.status().as_u16();
    let allow = response
        .headers()
        .get("allow")
        .map(|v| v.to_str().map(str::to_owned))
        .transpose()?;
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let body = if status == 500 {
        assert_eq!(&bytes[..], b"Internal Server Error");
        Value::Null
    } else if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)?
    };
    if let Some(expected) = native_error {
        assert_eq!(status, 422);
        assert_eq!(body, expected, "complete native syntax envelope");
    }
    if r["native_validation_profile"].is_string() {
        assert_eq!(status, 422);
        assert_eq!(allow, None);
        assert_eq!(
            body,
            native_validation_error(r)?,
            "complete authored native validation envelope"
        );
    }
    match r["native_model_profile"].as_str() {
        Some("strict-integer") => {
            assert_eq!(r["name"], "integer-float-revision");
            assert!(matches!(&r["body"]["evidence_revision"], Value::Number(n) if n.is_f64()));
            assert_eq!(status, 422);
            assert_eq!(
                body,
                json!({"error":{"code":"validation_failed",
                "message":"request does not match the REST contract","details":null}})
            );
        }
        Some("number-length") => {
            let raw = r["raw"].as_str().ok_or("number-length raw body")?;
            assert!(raw.contains(&"9".repeat(4300)));
            assert!(matches!(
                json::decode(
                    raw.as_bytes(),
                    cannery_server::body::REST_JSON_NESTING_BUDGET
                ),
                Err(json::DecodeError::IntegerLimit)
            ));
            assert_eq!(status, 400);
            assert_eq!(
                body,
                json!({"detail":"There was an error parsing the body"})
            );
            assert_eq!(
                &bytes[..],
                br#"{"detail":"There was an error parsing the body"}"#
            );
        }
        Some("integer-key-replay") | None => {}
        Some(_) => return Err("unknown native model profile".into()),
    }
    Ok((status, allow, output(body), hex(&bytes)?))
}
#[allow(clippy::too_many_lines)]
fn compare(
    r: &Value,
    result: (u16, Option<String>, Value, String),
    s: &Snapshot,
    f: &Value,
) -> Result<()> {
    assert_eq!(
        json!(result.0),
        r["status"],
        "status {}; response {}",
        r["name"],
        result.2
    );
    assert_eq!(json!(result.1), r["allow"], "allow {}", r["name"]);
    let mut expected_response = r["response"].clone();
    if r["native_validation_profile"].is_string() {
        expected_response = output(native_validation_error(r)?);
    }
    assert_eq!(
        projected(without_cites(result.2)?, &s.ids, &s.clocks),
        projected(expected_response, &BTreeMap::new(), &BTreeMap::new()),
        "response {}",
        r["name"]
    );
    let key = r["storage"].as_str().ok_or("snapshot key")?;
    let expected = projected(
        f["snapshots"][key].clone(),
        &BTreeMap::new(),
        &BTreeMap::new(),
    );
    assert!(
        s.value == expected,
        "storage {}; first differing path {}",
        r["name"],
        difference(&s.value, &expected).unwrap_or_default()
    );
    if let Some(wire) = r["wire_hex"].as_str() {
        if r["native_response_profile"] == "typed-review-model" && result.0 == 200 {
            assert!(matches!(
                r["name"].as_str(),
                Some("fixed-current-model" | "fixed-current-other-user")
            ));
            let bytes = result
                .3
                .as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| Ok(u8::from_str_radix(std::str::from_utf8(pair)?, 16)?))
                .collect::<Result<Vec<u8>>>()?;
            let model: cannery_server::api_models::cannery_row__reviews__routes__ReviewCaseOut =
                serde_json::from_slice(&bytes)?;
            assert_eq!(
                serde_json::to_vec(&model)?,
                bytes,
                "exact native typed DTO serialization"
            );
            // Full decoded response and every storage field were compared above.
            // Source wire bytes stay recorded independently, including evidence
            // floats/Unicode, whose canonical persisted bytes remain strict.
        } else {
            assert!(
                r["native_response_profile"].is_null()
                    || (r["name"] == "fixed-current-other-user" && result.0 == 404)
            );
            let bytes = result
                .3
                .as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| Ok(u8::from_str_radix(std::str::from_utf8(pair)?, 16)?))
                .collect::<Result<Vec<u8>>>()?;
            assert_eq!(
                hex(&bytes_without_cites(&bytes)?)?,
                wire,
                "wire {}",
                r["name"]
            );
        }
    }
    Ok(())
}

fn difference(actual: &Value, expected: &Value) -> Option<String> {
    let mut pending = vec![(actual, expected, String::new())];
    while let Some((actual, expected, path)) = pending.pop() {
        if actual == expected {
            continue;
        }
        match (actual, expected) {
            (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
                pending.extend(
                    a.iter()
                        .zip(b)
                        .enumerate()
                        .rev()
                        .map(|(i, (a, b))| (a, b, format!("{path}/{i}"))),
                );
            }
            (Value::Object(a), Value::Object(b)) if a.len() == b.len() => {
                for (key, a) in a.iter().rev() {
                    if let Some(b) = b.get(key) {
                        pending.push((a, b, format!("{path}/{key}")));
                    } else {
                        return Some(format!("{path}/{key}"));
                    }
                }
            }
            _ => return Some(path),
        }
    }
    None
}
async fn locked_case_disappeared(
    app: &Router,
    pool: &PgPool,
    recipe: &Value,
    ids: &BTreeMap<String, String>,
    requests: &mut BTreeMap<String, Value>,
) -> Result<(u16, Option<String>, Value, String)> {
    let mut conn = pool.acquire().await?;
    let mut tx = conn.begin().await?;
    sqlx::query(
        "SELECT id FROM attempts WHERE id='00000000-0000-0000-0000-000000002017' FOR UPDATE",
    )
    .execute(&mut *tx)
    .await?;
    let app = app.clone();
    let recipe = recipe.clone();
    let ids = ids.clone();
    let mut ledger = requests.clone();
    let pending = tokio::spawn(async move {
        let result = call(&app, &recipe, &ids, &mut ledger).await?;
        Ok::<_, Box<dyn Error + Send + Sync>>((result, ledger))
    });
    let mut blocked = 0_i64;
    for _ in 0..200 {
        blocked=sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND pid<>pg_backend_pid()").fetch_one(pool).await?;
        if blocked >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(blocked >= 1);
    sqlx::query("UPDATE review_cases SET project_id='00000000-0000-0000-0000-000000000011' WHERE id='00000000-0000-0000-0000-000000000317'").execute(pool).await?;
    tx.commit().await?;
    let (result, ledger) = pending.await??;
    requests.extend(ledger);
    Ok(result)
}
#[tokio::test]
#[ignore = "requires a positively selected artifact and guarded fresh migrated PostgreSQL child"]
#[allow(
    clippy::many_single_char_names,
    clippy::too_many_lines,
    reason = "Source recipe handles and concurrent result pairs"
)]
async fn review_decisions_match_production() -> Result<()> {
    let url = std::env::var("CANNERY_REVIEW_DECISIONS_HTTP_DATABASE_URL")?;
    let settings = load_settings(
        None,
        &BTreeMap::from([("CANNERY_DATABASE_URL".into(), url)]),
    )?;
    let (app, state) = application_with_review_decision_context(settings, Arc::new(profile()?))?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("owned child required")?;
    if suffix.len() != 24 || !suffix.bytes().all(|v| v.is_ascii_hexdigit()) {
        return Err("owned child required".into());
    }
    sqlx::raw_sql(include_str!("fixtures/review_decisions_http/seed.sql"))
        .execute(&state.pool)
        .await?;
    let source = fixture()?;
    let mut f = source.clone();
    let float_case = source["cases"]
        .as_array()
        .ok_or("cases")?
        .iter()
        .find(|r| r["native_model_profile"] == "strict-integer")
        .ok_or("strict integer recipe")?;
    assert_eq!(float_case["name"], "integer-float-revision");
    assert_eq!(float_case["status"], 201);
    let followup = float_case["native_followup"].clone();
    let mut expected_followup = float_case["body"].clone();
    expected_followup["evidence_revision"] = json!(1);
    assert_eq!(
        followup, expected_followup,
        "only the integer representation changes"
    );
    let source_digest = format!(
        "\\x{}",
        request_hash(&json!({"project":"matrix","decision":float_case["body"]}))?
    );
    let authored_integer_envelope = r#"{"decision":{"action":"retry","evidence_revision":1,"reason":"Reason é😀","review_case_id":"00000000-0000-0000-0000-000000000372"},"project":"matrix"}"#;
    assert_eq!(
        serde_json::from_str::<Value>(authored_integer_envelope)?,
        json!({"project":"matrix","decision":followup})
    );
    // Compute the expected digest independently from authored canonical bytes,
    // rather than calling the production request-hash adapter under test.
    let native_digest = format!(
        "\\x{}",
        hex(&Sha256::digest(authored_integer_envelope.as_bytes()))?
    );
    assert_ne!(source_digest, native_digest);
    // The authored valid integer follow-up preserves every source state transition.
    // Only its single declared idempotency digest differs; no other row is projected away.
    let mut adapted_snapshots = 0;
    for snapshot in f["snapshots"]
        .as_object_mut()
        .ok_or("snapshots")?
        .values_mut()
    {
        for row in snapshot["rows"]["idempotency_keys"]
            .as_array_mut()
            .ok_or("keys")?
        {
            if row["key"] == "float-revision" {
                assert_eq!(row["scope"], "review.decide");
                assert_eq!(row["actor"], "user:00000000-0000-0000-0000-000000000002");
                assert_eq!(row["request_hash"], source_digest);
                row["request_hash"] = json!(native_digest);
                adapted_snapshots += 1;
            }
        }
    }
    assert!(adapted_snapshots > 0);
    let recipes = f["cases"].as_array().ok_or("recipes")?;
    assert_eq!(
        recipes
            .iter()
            .filter_map(|r| r["native_validation_profile"].as_str())
            .fold(BTreeMap::new(), |mut counts, name| {
                *counts.entry(name).or_insert(0_usize) += 1;
                counts
            }),
        BTreeMap::from([
            ("missing-fields", 2),
            ("missing-extra", 1),
            ("empty-reason", 1)
        ])
    );
    assert_eq!(
        recipes
            .iter()
            .filter(|r| r["native_response_profile"] == "typed-review-model")
            .count(),
        2
    );
    assert_eq!(
        recipes
            .iter()
            .filter(|r| r["name"] == "reject-any-verdict")
            .map(|r| r["number"].clone())
            .collect::<Vec<_>>(),
        [json!(6)]
    );
    assert_eq!(
        recipes
            .iter()
            .filter(|r| r["native_model_profile"].is_string())
            .count(),
        4,
        "one strict integer refusal, one dependent key replay and two numeric length refusals"
    );
    assert_eq!(
        recipes
            .iter()
            .filter(|r| r["native_wire_profile"].is_string())
            .count(),
        1,
        "corpus must retain the explicit unpaired-surrogate refusal"
    );
    let mut requests = BTreeMap::new();
    let mut ids = BTreeMap::new();
    let mut i = 0;
    while i < recipes.len() {
        let r = &recipes[i];
        if r["name"] == "race-first" {
            sqlx::raw_sql("DROP TRIGGER fixture_audit_failure ON audit_events; DROP FUNCTION fixture_audit_failure()")
                .execute(&state.pool).await?;
            let second = recipes.get(i + 1).ok_or("race replay")?;
            assert_eq!(second["name"], "race-replay");
            let mut conn = state.pool.acquire().await?;
            let mut tx = conn.begin().await?;
            sqlx::query("SELECT id FROM attempts WHERE id='00000000-0000-0000-0000-000000002018' FOR UPDATE").execute(&mut *tx).await?;
            let app_one = app.clone();
            let r_one = r.clone();
            let ids_one = ids.clone();
            let requests_one = requests.clone();
            let one = tokio::spawn(async move {
                let mut ledger = requests_one;
                let result = call(&app_one, &r_one, &ids_one, &mut ledger).await?;
                Ok::<_, Box<dyn Error + Send + Sync>>((result, ledger))
            });
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let app_two = app.clone();
            let r_two = second.clone();
            let ids_two = ids.clone();
            let requests_two = requests.clone();
            let two = tokio::spawn(async move {
                let mut ledger = requests_two;
                let result = call(&app_two, &r_two, &ids_two, &mut ledger).await?;
                Ok::<_, Box<dyn Error + Send + Sync>>((result, ledger))
            });
            let mut blocked = 0_i64;
            for _ in 0..100 {
                blocked=sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND pid<>pg_backend_pid()").fetch_one(&state.pool).await?;
                if blocked >= 2 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert!(blocked >= 2);
            tx.commit().await?;
            let (a, ledger) = one.await??;
            requests.extend(ledger);
            let (b, ledger) = two.await??;
            requests.extend(ledger);
            let stored = snapshot(&state.pool, &requests).await?;
            // Either identical request may acquire the row lock first. Require
            // exactly one creation and one replay, then compare both complete
            // responses and the shared durable snapshot by their outcome.
            assert_eq!(
                [r["status"].as_u64(), second["status"].as_u64()],
                [Some(201), Some(200)]
            );
            let (created, replayed) = if a.0 == 201 { (a, b) } else { (b, a) };
            compare(r, created, &stored, &f)?;
            compare(second, replayed, &stored, &f)?;
            i += 2;
            continue;
        }
        if let Some(setup) = r["setup"].as_str() {
            sqlx::raw_sql(setup).execute(&state.pool).await?;
        }
        if r["name"] == "audit-rollback" {
            sqlx::raw_sql("CREATE FUNCTION fixture_audit_failure() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture rollback'; END $$; CREATE TRIGGER fixture_audit_failure BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION fixture_audit_failure()")
                .execute(&state.pool).await?;
        }
        let before = if r["native_wire_profile"].is_string()
            || r["native_validation_profile"].is_string()
            || matches!(
                r["native_model_profile"].as_str(),
                Some("strict-integer" | "number-length" | "integer-key-replay")
            ) {
            Some((
                snapshot(&state.pool, &requests).await?.value,
                raw_storage(&state.pool).await?,
            ))
        } else {
            None
        };
        let result = if r["name"] == "locked-case-invariant" {
            locked_case_disappeared(&app, &state.pool, r, &ids, &mut requests).await?
        } else {
            call(&app, r, &ids, &mut requests).await?
        };
        let stored = snapshot(&state.pool, &requests).await?;
        if let Some((before, raw_before)) = before {
            let profile = r["native_model_profile"].as_str();
            assert_eq!(
                result.0,
                match profile {
                    Some("number-length") => 400,
                    Some("integer-key-replay") => 200,
                    _ => 422,
                }
            );
            assert_eq!(result.1, None);
            assert_eq!(
                stored.value, before,
                "native rejection must not write storage"
            );
            assert_eq!(
                raw_storage(&state.pool).await?,
                raw_before,
                "all stored row values unchanged"
            );
            if profile == Some("strict-integer") {
                let mut native_recipe = r.clone();
                native_recipe["native_model_profile"] = Value::Null;
                native_recipe["body"] = followup.clone();
                let valid = call(&app, &native_recipe, &ids, &mut requests).await?;
                let stored = snapshot(&state.pool, &requests).await?;
                compare(r, valid, &stored, &f)?;
                ids = stored.ids;
                i += 1;
                continue;
            }
            if profile == Some("integer-key-replay") {
                let mut expected = r.clone();
                expected["status"] = json!(200);
                expected["response"] = float_case["response"].clone();
                compare(&expected, result, &stored, &f)?;
            } else {
                let key = r["storage"].as_str().ok_or("snapshot key")?;
                assert_eq!(
                    stored.value, f["snapshots"][key],
                    "source invalid-body storage remains equivalent"
                );
            }
        } else {
            compare(r, result, &stored, &f)?;
        }
        ids = stored.ids;
        i += 1;
    }
    off_type_verdicts_reject_and_replay(&app, &state.pool).await?;
    state.pool.close().await;
    Ok(())
}

// The promotion-verdict cases leave reports whose verdict is not a string. A
// verification report's front matter is an open map, so a reject records the
// decision and returns the report verbatim; the keyed request then replays.
async fn off_type_verdicts_reject_and_replay(app: &Router, pool: &PgPool) -> Result<()> {
    for number in [7, 9, 10, 11] {
        let case_id = format!("00000000-0000-0000-0000-{:012}", 300 + number);
        let document = format!(
            "---\noutcome: reject\nverification:\n  ref: 00000000-0000-0000-0000-{:012}\n  sha256: \"{}\"\nwriteup: null\n---\nAuthored off-type verdict é😀\n",
            3000 + number,
            "0".repeat(64)
        );
        let recipe = json!({
            "name":"native-off-type-verdict", "number":number,
            "method":"POST", "role":"researcher",
            "key":format!("native-off-type-verdict-{number}"),
            "body":{"review_case_id":case_id,"document":document}
        });
        let mut requests = BTreeMap::new();
        let report: Value =
            sqlx::query_scalar("SELECT front_matter FROM phase_outputs WHERE id=$1::uuid")
                .bind(format!("00000000-0000-0000-0000-{:012}", 3000 + number))
                .fetch_one(pool)
                .await?;
        let verdict = match number {
            7 => Value::Null,
            9 => json!({"legacy":1}),
            10 => json!(7),
            11 => json!([]),
            _ => unreachable!(),
        };
        assert_eq!(report, json!({"verdict":verdict}));
        let created = call(app, &recipe, &BTreeMap::new(), &mut requests).await?;
        assert_eq!(created.0, 201);
        assert_eq!(created.1, None);
        let model: cannery_server::api_models::cannery_row__reviews__routes__ReviewCaseOut =
            serde_json::from_value(created.2.clone())?;
        assert_eq!(model.id, case_id);
        assert_eq!(model.state, "resolved");
        assert_eq!(model.unit_state, "rejected");
        assert_eq!(model.attempt_state.as_deref(), Some("verified"));
        assert_eq!(created.2["verification"]["front_matter"], report);
        assert_eq!(model.decisions.len(), 1);
        assert_eq!(created.2["decisions"][0]["action"], "reject");
        assert_eq!(
            created.2["decisions"][0]["reason"],
            "Authored off-type verdict é😀"
        );
        let committed = raw_storage(pool).await?;
        let replayed = call(app, &recipe, &BTreeMap::new(), &mut requests).await?;
        assert_eq!(replayed.0, 200);
        assert_eq!(replayed.1, created.1);
        assert_eq!(replayed.2, created.2);
        assert_eq!(replayed.3, created.3);
        assert_eq!(raw_storage(pool).await?, committed);
    }
    Ok(())
}
/// A review case response without `cites`, which the frozen corpus
/// predates, after checking it: only a decision case states it, and each
/// citation is null or a `{ref, sha256}` with a 64-hex digest.
fn without_cites(mut value: Value) -> Result<Value> {
    match &mut value {
        Value::Object(fields) => {
            if let Some(cites) = fields.remove("cites") {
                assert_eq!(fields.get("kind"), Some(&json!("decision")), "{cites}");
                let cites = cites.as_object().ok_or("cites object")?;
                assert_eq!(cites.len(), 2, "{cites:?}");
                for key in ["verification", "writeup"] {
                    let document = cites.get(key).ok_or("cited document")?;
                    if !document.is_null() {
                        assert!(document["ref"].is_string(), "{document}");
                        assert!(
                            document["sha256"]
                                .as_str()
                                .is_some_and(|sha| sha.len() == 64
                                    && sha.bytes().all(|b| b.is_ascii_hexdigit())),
                            "{document}"
                        );
                    }
                }
            } else if fields.contains_key("subject_revision") && fields.contains_key("unit_ref") {
                assert_ne!(
                    fields.get("kind"),
                    Some(&json!("decision")),
                    "a decision case states cites"
                );
            }
            for field in fields.values_mut() {
                *field = without_cites(field.take())?;
            }
        }
        Value::Array(items) => {
            for item in items {
                *item = without_cites(item.take())?;
            }
        }
        _ => {}
    }
    Ok(value)
}
/// The response bytes without the `,"cites":{…}` members `without_cites`
/// drops; the object holds no braces inside its strings.
fn bytes_without_cites(bytes: &[u8]) -> Result<Vec<u8>> {
    let marker = b",\"cites\":{";
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(marker) {
            let mut depth = 0_usize;
            let mut j = i + marker.len() - 1;
            loop {
                match bytes.get(j).ok_or("unterminated cites")? {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            i = j + 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    Ok(out)
}
