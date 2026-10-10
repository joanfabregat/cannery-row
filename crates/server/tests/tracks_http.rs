//! Actual source HTTP corpus with full transactional track/audit observations.
#![forbid(unsafe_code)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use cannery_core::{contracts::ContractValidator, settings::load_settings};
use cannery_research::science::RenderingContext;
use cannery_server::{
    application_with_track_context, track_routes::TrackContext, track_wire::ResponseContext,
};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::fmt::Write;
use std::{collections::BTreeMap, error::Error, sync::Arc};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
fn fixture() -> Result<Value> {
    Ok(serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/tracks_http/reference.json"
    ))?)
}
fn profile() -> Result<TrackContext> {
    Ok(TrackContext {
        contracts: ContractValidator::new()?,
        validation_walk_budget: 80,
        repr_budget: 80,

        rendering: RenderingContext { nesting_budget: 80 },
        repository: cannery_tracks::repo::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
        config_repository: cannery_research::config_repo::JsonContext {
            encode_nesting_budget: 80,
            decode_nesting_budget: 80,
        },
        audit_encode_budget: 80,
        audit_decode_budget: 80,
        response: ResponseContext {
            inferred_nesting_budget: 80,
        },
    })
}
fn canonical(value: Value, ids: &BTreeMap<String, String>) -> Value {
    match value {
        Value::Number(number) if number.to_string().contains(['.', 'e', 'E']) => number
            .as_f64()
            .and_then(serde_json::Number::from_f64)
            .map_or(Value::Number(number), Value::Number),
        Value::Array(v) => Value::Array(v.into_iter().map(|v| canonical(v, ids)).collect()),
        Value::Object(v) if v.contains_key("error") => {
            let e = &v["error"];
            let mut projected = json!({"code":e["code"],"details":e["details"].as_array().map(|a|a.iter().map(|d|json!({"path":d["path"]})).collect::<Vec<_>>())});
            if e["code"] != "validation_failed" {
                projected["message"] = e["message"].clone();
            }
            json!({"error":projected})
        }
        Value::Object(v) => {
            Value::Object(v.into_iter().map(|(k, v)| (k, canonical(v, ids))).collect())
        }
        Value::String(s) => ids.get(&s).map_or_else(
            || {
                if chrono::DateTime::parse_from_rfc3339(&s.replace(' ', "T"))
                    .or_else(|_| chrono::DateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S%.f%#z"))
                    .is_ok_and(|instant| chrono::Datelike::year(&instant) >= 2026)
                {
                    json!("@clock")
                } else {
                    json!(s)
                }
            },
            |v| json!(v),
        ),
        v => v,
    }
}
fn raw(hex: &str) -> Result<Vec<u8>> {
    hex.as_bytes()
        .chunks(2)
        .map(|v| Ok(u8::from_str_radix(std::str::from_utf8(v)?, 16)?))
        .collect()
}
async fn call(app: &Router, recipe: &Value) -> Result<(u16, Option<String>, Value)> {
    let mut request = Request::builder()
        .method(recipe["method"].as_str().ok_or("method")?)
        .uri(recipe["path"].as_str().ok_or("path")?);
    let role = recipe["role"].as_str().ok_or("role")?;
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
    let body = recipe["body_hex"].as_str().map(raw).transpose()?;
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let r = app
        .clone()
        .oneshot(request.body(body.map_or_else(Body::empty, Body::from))?)
        .await?;
    let status = r.status().as_u16();
    let allow = r
        .headers()
        .get("allow")
        .map(|v| v.to_str().map(str::to_owned))
        .transpose()?;
    let bytes = to_bytes(r.into_body(), usize::MAX).await?;
    let value = if status == 500 {
        assert_eq!(&bytes[..], b"Internal Server Error");
        Value::Null
    } else if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)?
    };
    Ok((status, allow, value))
}
async fn storage(pool: &PgPool) -> Result<(BTreeMap<String, String>, Value, Value)> {
    let ids = sqlx::query("SELECT id::text,slug FROM tracks ORDER BY slug")
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|r| {
            Ok((
                r.try_get::<String, _>(0)?,
                format!("@track:{}", r.try_get::<String, _>(1)?),
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let tracks:Value=sqlx::query_scalar("SELECT coalesce(jsonb_agg(jsonb_build_array(id::text,project_id::text,slug,title,description,producer,mode,workflow,state,revision,created_by::text,created_at::text,updated_at::text) ORDER BY slug),'[]') FROM tracks").fetch_one(pool).await?;
    let audit:Value=sqlx::query_scalar("SELECT coalesce(jsonb_agg(jsonb_build_array(seq,project_id::text,actor_kind,actor_user_id::text,actor_service_id::text,via_channel,via_client,action,subject_type,subject_id,prior_state,new_state,reason,idempotency_key) ORDER BY seq),'[]') FROM audit_events").fetch_one(pool).await?;
    Ok((ids, tracks, audit))
}
fn native_refusal(path: &str, message: &str) -> Value {
    json!({"error":{"code":"validation_failed","message":"request validation failed","details":[{"path":path,"message":message}]}})
}
fn syntax_refusal(bytes: &[u8]) -> Result<Value> {
    let error = serde_json::from_slice::<Value>(bytes)
        .err()
        .ok_or("invalid JSON probe accepted")?;
    Ok(native_refusal(
        &format!("body/{}", error.column()),
        "JSON decode error",
    ))
}
#[tokio::test]
async fn all_six_track_routes_are_installed_and_fail_closed_without_database() -> Result<()> {
    let settings = load_settings(
        None,
        &BTreeMap::from([(
            "CANNERY_DATABASE_URL".into(),
            "postgresql://postgres@127.0.0.1:1/unused".into(),
        )]),
    )?;
    let (app, state) = application_with_track_context(settings, Arc::new(profile()?))?;
    state.pool.close().await;
    for (method, path) in [
        ("POST", "/api/projects/matrix/tracks"),
        ("GET", "/api/projects/matrix/tracks"),
        ("GET", "/api/projects/matrix/tracks/alpha"),
        ("PATCH", "/api/projects/matrix/tracks/alpha"),
        ("POST", "/api/projects/matrix/tracks/alpha/transitions"),
        ("GET", "/api/projects/matrix/tracks/alpha/history"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))?,
            )
            .await?;
        assert_eq!(
            response.status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "{method} {path}"
        );
        let body = to_bytes(response.into_body(), 4096).await?;
        assert_eq!(body.as_ref(), b"Internal Server Error");
    }
    Ok(())
}
#[tokio::test]
#[ignore = "requires guarded uniquely owned migrated PostgreSQL"]
#[allow(
    clippy::too_many_lines,
    reason = "Replay complete source request and recovery fixture order"
)]
async fn tracks_match_production() -> Result<()> {
    let url = std::env::var("CANNERY_TRACK_HTTP_DATABASE_URL")?;
    let settings = load_settings(
        None,
        &BTreeMap::from([("CANNERY_DATABASE_URL".into(), url)]),
    )?;
    let (app, state) = application_with_track_context(settings, Arc::new(profile()?))?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("owned fixture required")?;
    if suffix.len() != 24 || !suffix.bytes().all(|v| v.is_ascii_hexdigit()) {
        return Err("owned fixture required".into());
    }
    sqlx::raw_sql(include_str!("fixtures/tracks_http/seed.sql"))
        .execute(&state.pool)
        .await?;
    let f = fixture()?;
    let cases = f["cases"].as_array().ok_or("cases")?;
    for id in [
        "null-model",
        "list-model",
        "ordered-model",
        "update-coercion",
        "same-title-increments",
        "wide-stale",
        "history-wide",
        "history-wide-missing",
        "raw-invalid-JSON",
    ] {
        assert_eq!(
            cases
                .iter()
                .filter(|recipe| recipe["id"] == id && recipe["native_boundary"].is_string())
                .count(),
            1
        );
    }
    assert_eq!(
        cases
            .iter()
            .filter(|recipe| recipe["native_boundary"].is_string())
            .count(),
        9
    );
    assert_eq!(
        cases
            .iter()
            .filter(|recipe| recipe["native_followup"].is_string())
            .count(),
        2
    );
    for recipe in cases {
        let id = recipe["id"].as_str().ok_or("id")?;
        if id == "producer-missing" {
            let mut d: Value = serde_json::from_str(include_str!(
                "../../../tests/fixtures/contracts/science_revision/valid/runner_verifier.json"
            ))?;
            d["default_producer"] = json!({"name":"sparse-producer","revision":1});
            sqlx::query("INSERT INTO config_revisions(project_id,kind,revision,content,created_by) VALUES('00000000-0000-0000-0000-000000000010','science',1,$1,'00000000-0000-0000-0000-000000000001')").bind(d).execute(&state.pool).await?;
        }
        if id == "bound-accepted" {
            let d: Value = serde_json::from_str(include_str!(
                "../../../tests/fixtures/contracts/step_manifest/valid/producer.json"
            ))?;
            sqlx::query("INSERT INTO producer_manifests(project_id,name,revision,content,created_by) VALUES('00000000-0000-0000-0000-000000000010','sparse-producer',1,$1,'00000000-0000-0000-0000-000000000001')").bind(d).execute(&state.pool).await?;
        }
        if id == "audit-rollback" {
            sqlx::raw_sql("CREATE FUNCTION fixture_reject_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='track.updated' THEN RAISE EXCEPTION 'fixture'; END IF; RETURN NEW; END $$; CREATE TRIGGER fixture_reject BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION fixture_reject_audit()").execute(&state.pool).await?;
        }
        if id == "archive-open" {
            sqlx::raw_sql("INSERT INTO hypotheses(project_id,number,track_id,state,title,created_by_user,approved_revision,approved_at) SELECT '00000000-0000-0000-0000-000000000010',100+i,t.id,h.state,h.state,'00000000-0000-0000-0000-000000000001',1,now() FROM tracks t CROSS JOIN (VALUES(1,'queued'),(2,'queued'),(3,'active'),(4,'awaiting_human_review')) h(i,state) WHERE t.slug='alpha'").execute(&state.pool).await?;
        }
        if id == "archive" {
            sqlx::query("UPDATE hypotheses SET state='cancelled' WHERE number>=100")
                .execute(&state.pool)
                .await?;
        }
        if id == "workflow-accepted" {
            let d: Value =
                serde_json::from_str(include_str!("fixtures/tracks_http/experiment.json"))?;
            sqlx::query("INSERT INTO experiment_manifests(project_id,name,revision,content,created_by) VALUES('00000000-0000-0000-0000-000000000010','experiment',1,$1,'00000000-0000-0000-0000-000000000001')").bind(d).execute(&state.pool).await?;
        }
        if id == "after-rollback" {
            sqlx::raw_sql(
                "DROP TRIGGER fixture_reject ON audit_events; DROP FUNCTION fixture_reject_audit()",
            )
            .execute(&state.pool)
            .await?;
        }
        if id == "recovered-read-80" {
            for (index, value) in [
                (80, json!(null)),
                (
                    81,
                    serde_json::from_str::<Value>(&format!(
                        "{{\"free\":[1{},1e-7,\"é\"]}}",
                        "0".repeat(500)
                    ))?,
                ),
                (82, json!([1, null])),
                (83, json!(4)),
            ] {
                sqlx::query("INSERT INTO tracks(id,project_id,slug,title,producer,created_by,created_at,updated_at) VALUES($1,'00000000-0000-0000-0000-000000000010',$2,'Recovery',$3,'00000000-0000-0000-0000-000000000001','2001-02-03T04:05:06Z','2001-02-03T04:05:06Z')").bind(uuid::Uuid::from_u128(index)).bind(format!("recovered-{index}")).bind(value).execute(&state.pool).await?;
            }
        }
        let initial = storage(&state.pool).await?;
        let (mut status, mut allow, mut output) = call(&app, recipe).await?;
        let boundary = recipe["native_boundary"].as_str();
        if let Some(boundary) = boundary {
            assert_eq!(status, 422, "native status {id}");
            assert_eq!(json!(allow), recipe["allow"], "native Allow {id}");
            assert_eq!(
                storage(&state.pool).await?,
                initial,
                "native refusal changed state {id}"
            );
            let expected = match boundary {
                "dto_shape" => {
                    assert!(["null-model", "list-model", "ordered-model"].contains(&id));
                    assert_eq!(recipe["status"], 422);
                    assert!(
                        serde_json::from_slice::<cannery_server::api_models::TrackUpdate>(&raw(
                            recipe["body_hex"].as_str().ok_or("patch body")?
                        )?)
                        .is_err(),
                        "authored DTO shape probe {id}"
                    );
                    json!({"error":{"code":"validation_failed","message":"request does not match the REST contract","details":null}})
                }
                "expected_revision" => {
                    assert!(
                        ["update-coercion", "same-title-increments", "wide-stale"].contains(&id)
                    );
                    assert_eq!(recipe["status"], if id == "wide-stale" { 409 } else { 200 });
                    assert!(
                        serde_json::from_slice::<cannery_server::api_models::TrackUpdate>(&raw(
                            recipe["body_hex"].as_str().ok_or("patch body")?
                        )?)
                        .is_err(),
                        "authored bounded integer DTO probe {id}"
                    );
                    json!({"error":{"code":"validation_failed","message":"request does not match the REST contract","details":null}})
                }
                "history_before" => {
                    assert!(["history-wide", "history-wide-missing"].contains(&id));
                    assert_eq!(
                        recipe["status"],
                        if id == "history-wide" { 500 } else { 404 }
                    );
                    native_refusal("query/before", "Input should be a signed 64-bit integer")
                }
                "json_syntax" => {
                    assert_eq!(id, "raw-invalid-JSON");
                    assert_eq!(recipe["status"], 422);
                    syntax_refusal(&raw(recipe["body_hex"].as_str().ok_or("syntax body")?)?)?
                }
                _ => return Err("unknown native track boundary".into()),
            };
            assert_eq!(output, expected, "native refusal {id}");
            if let Some(body) = recipe["native_followup"].as_str() {
                // Authored integer revisions and an ordinary reason preserve the
                // source's successful mutation, including its full audit state.
                assert!(["update-coercion", "same-title-increments"].contains(&id));
                let mut followup = recipe.clone();
                followup["body_hex"] = json!(body);
                (status, allow, output) = call(&app, &followup).await?;
                assert_eq!(status, recipe["status"], "follow-up status {id}");
            }
        } else {
            assert!(recipe["native_boundary"].is_null());
            assert_eq!(status, recipe["status"], "status {id}");
        }
        assert_eq!(json!(allow), recipe["allow"], "Allow {id}");
        let (ids, tracks, audit) = storage(&state.pool).await?;
        if boundary.is_none() || recipe["native_followup"].is_string() {
            assert_eq!(
                canonical(output, &ids),
                canonical(recipe["output"].clone(), &ids),
                "output {id}"
            );
        }
        assert_eq!(
            canonical(tracks, &ids),
            canonical(recipe["tracks"].clone(), &ids),
            "tracks {id}"
        );
        assert_eq!(
            canonical(audit, &ids),
            canonical(recipe["audit"].clone(), &ids),
            "audit {id}"
        );
    }
    for profile in f["contention"].as_array().ok_or("contention")? {
        contention(&app, &state.pool, profile).await?;
    }
    let initial = storage(&state.pool).await?;
    for _ in 0..12 {
        let recipe = json!({"method":"GET","path":"/api/projects/matrix/tracks/alpha/history?before=9223372036854775808&limit=2","role":"researcher","body_hex":null});
        let (status, _, output) = call(&app, &recipe).await?;
        assert_eq!(status, 422);
        assert_eq!(
            output,
            native_refusal("query/before", "Input should be a signed 64-bit integer")
        );
        assert_eq!(
            storage(&state.pool).await?,
            initial,
            "repeated history refusal changed state"
        );
    }
    Ok(())
}

#[test]
fn fixed_track_model_serialization() -> Result<()> {
    use cannery_core::{
        ids::{ProjectId, TrackId, UserId},
        json,
        timestamps::Timestamp,
    };
    use cannery_tracks::repo::{Track, TrackMode, TrackState};
    for producer in [
        Value::Null,
        json!({}),
        json!({"name":"producer", "revision":1}),
        json!({"value":1e-7,"label":"é","extra":[1,null]}),
    ] {
        let document = json::decode_str(&serde_json::to_string(&producer)?, json::MAX_DEPTH)?;
        let mut row = Track {
            id: TrackId(uuid::Uuid::from_u128(0x99)),
            project_id: ProjectId(uuid::Uuid::from_u128(10)),
            slug: String::from("wire"),
            title: String::from("Wire é"),
            description: String::from("Line\ntext"),
            producer: Some(document),
            mode: TrackMode::Agent,
            workflow: None,
            state: TrackState::Active,
            revision: 1,
            created_by: UserId(uuid::Uuid::from_u128(1)),
            created_at: Timestamp(
                chrono::DateTime::parse_from_rfc3339("2001-02-03T04:05:06.123456+00:00")?
                    .fixed_offset(),
            ),
            updated_at: Timestamp(
                chrono::DateTime::parse_from_rfc3339("2001-02-03T04:05:07+00:00")?.fixed_offset(),
            ),
        };
        let result = cannery_server::track_wire::track_bytes(
            &row,
            ResponseContext {
                inferred_nesting_budget: json::MAX_DEPTH,
            },
        );
        let result: Value = serde_json::from_slice(&result?)?;
        assert_eq!(
            result,
            json!({"id":row.id.to_string(),"slug":"wire","title":"Wire é","description":"Line\ntext","producer":producer,"mode":"agent","workflow":null,"state":"active","revision":1,"created_at":"2001-02-03T04:05:06.123456Z","updated_at":"2001-02-03T04:05:07Z"})
        );
        for invalid in [json!([1]), json!(4), json!("text")] {
            row.producer = Some(json::decode_str(
                &serde_json::to_string(&invalid)?,
                json::MAX_DEPTH,
            )?);
            assert!(
                cannery_server::track_wire::track_bytes(
                    &row,
                    ResponseContext {
                        inferred_nesting_budget: json::MAX_DEPTH
                    }
                )
                .is_err()
            );
        }
    }
    Ok(())
}
async fn contention(app: &Router, pool: &PgPool, profile: &Value) -> Result<()> {
    use sqlx::Acquire;
    let operation = profile["operation"].as_str().ok_or("operation")?;
    let body = if operation == "update" {
        json!({"expected_revision":6,"title":"Concurrent"})
    } else {
        json!({"expected_revision":7,"to_state":"paused","reason":"Concurrent"})
    };
    let bytes = serde_json::to_vec(&body)?;
    let mut hex = String::new();
    for byte in bytes {
        write!(hex, "{byte:02x}")?;
    }
    let recipe = json!({"method":if operation=="update"{"PATCH"}else{"POST"},"path":format!("/api/projects/matrix/tracks/alpha{}",if operation=="update"{""}else{"/transitions"}),"role":"researcher","body_hex":hex});
    let mut holder = pool.acquire().await?;
    let mut tx = holder.begin().await?;
    sqlx::query("SELECT id FROM tracks WHERE slug='alpha' FOR UPDATE")
        .fetch_one(&mut *tx)
        .await?;
    let tasks = (0..2)
        .map(|_| {
            let app = app.clone();
            let recipe = recipe.clone();
            tokio::spawn(async move { call(&app, &recipe).await })
        })
        .collect::<Vec<_>>();
    let mut waited = false;
    for _ in 0..300 {
        let n:i64=sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%tracks%' AND query NOT LIKE '%pg_stat_activity%'").fetch_one(pool).await?;
        if n >= 2 {
            waited = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    tx.commit().await?;
    let mut statuses = vec![];
    for task in tasks {
        statuses.push(task.await??.0);
    }
    statuses.sort_unstable();
    assert_eq!(json!(waited), profile["waited"]);
    assert_eq!(json!(statuses), profile["statuses"]);
    let (ids, _, _) = storage(pool).await?;
    let recipe = json!({"method":"GET","path":"/api/projects/matrix/tracks/alpha","role":"researcher","body_hex":null});
    let (_, _, row) = call(app, &recipe).await?;
    assert_eq!(row["revision"], profile["revision"]);
    assert_eq!(
        canonical(row, &ids),
        canonical(profile["row"].clone(), &ids)
    );
    let clock:bool=sqlx::query_scalar("SELECT t.updated_at=(SELECT max(a.occurred_at) FROM audit_events a WHERE a.subject_id=t.id::text) FROM tracks t WHERE slug='alpha'").fetch_one(pool).await?;
    assert_eq!(json!(clock), profile["atomic_clock"]);
    Ok(())
}
