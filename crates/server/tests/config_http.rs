//! Real migrated PostgreSQL integration for configuration publication ordering.
#![forbid(unsafe_code)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request},
};
use cannery_core::{contracts::ContractValidator, principal::Scope, settings::load_settings};
use cannery_identity::{
    models::TokenKind,
    repo::{self, LoginUser, NewToken},
    secrets::{self, PERSONAL_PREFIX},
};
use cannery_research::{config_repo::JsonContext, science::RenderingContext};
use cannery_server::{
    application_with_config_context, config_routes::ConfigContext, config_wire::ResponseContext,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{collections::BTreeMap, error::Error, sync::Arc};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

async fn person(pool: &PgPool, subject: &str, admin: bool) -> Result<String> {
    let mut conn = pool.acquire().await?;
    let user = repo::upsert_login_user(
        &mut conn,
        LoginUser {
            issuer: "https://config-http.fixture",
            subject,
            email: Some(subject),
            email_verified: true,
            display_name: Some(subject),
            make_admin: admin,
        },
    )
    .await?
    .user;
    let secret = secrets::new_secret(PERSONAL_PREFIX)?;
    repo::create_token(
        &mut conn,
        NewToken {
            secret_digest: secret.digest(),
            display_prefix: secret.display_prefix(),
            kind: TokenKind::Personal,
            user_id: Some(user.id),
            service_account_id: None,
            name: "config fixture",
            scopes: &[Scope::Read, Scope::Write],
            expires_in_days: 1,
        },
    )
    .await?;
    Ok(secret.plaintext().expose().to_owned())
}
async fn fixture() -> Result<(Router, PgPool)> {
    let uri = std::env::var("CANNERY_CONFIG_TEST_DATABASE_URL")?;
    let settings = load_settings(
        None,
        &BTreeMap::from([("CANNERY_DATABASE_URL".into(), uri)]),
    )?;
    // These separately measured operation contexts are supplied explicitly. The
    // production meta-runtime/cache binding remains a coordinated integration.
    let context = ConfigContext {
        contracts: ContractValidator::new()?,
        validation_walk_budget: 967,
        repr_budget: 9992,

        rendering: RenderingContext {
            nesting_budget: 967,
        },
        repository: JsonContext {
            encode_nesting_budget: 9990,
            decode_nesting_budget: 9992,
        },
        response: ResponseContext {
            inferred_nesting_budget: 255,
        },
    };
    let (app, state) = application_with_config_context(settings, Arc::new(context))?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&state.pool)
        .await
        .map_err(|_| "fixture guard failed")?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("isolated config fixture required")?;
    if suffix.len() != 24
        || !suffix
            .bytes()
            .all(|v| v.is_ascii_digit() || matches!(v, b'a'..=b'f'))
    {
        return Err("isolated config fixture required".into());
    }
    Ok((app, state.pool))
}
async fn call(
    app: &Router,
    method: Method,
    path: &str,
    token: Option<&str>,
    raw: Option<&[u8]>,
) -> Result<(u16, Value)> {
    let (status, response, _) =
        call_with_response_json(app, method, path, token, raw, false).await?;
    Ok((status, response))
}
async fn call_with_response_json(
    app: &Router,
    method: Method,
    path: &str,
    token: Option<&str>,
    raw: Option<&[u8]>,
    diagnostics_only: bool,
) -> Result<(u16, Value, Vec<u8>)> {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    if raw.is_some() {
        request = request.header("content-type", "application/json");
    }
    let response = app
        .clone()
        .oneshot(request.body(raw.map_or_else(Body::empty, |v| Body::from(v.to_vec())))?)
        .await?;
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    if status == 500 {
        assert_eq!(bytes.as_ref(), b"Internal Server Error");
        return Ok((status, Value::Null, bytes.to_vec()));
    }
    Ok((
        status,
        if bytes.is_empty() || (diagnostics_only && status != 422) {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)?
        },
        bytes.to_vec(),
    ))
}
async fn expect(
    app: &Router,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<&Value>,
    status: u16,
) -> Result<Value> {
    let bytes = body.map(serde_json::to_vec).transpose()?;
    let (actual, result) = call(app, method, path, token, bytes.as_deref()).await?;
    assert_eq!(actual, status, "configuration HTTP status for {path}");
    Ok(result)
}
fn science() -> Result<Value> {
    let mut doc: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/contracts/science_revision/valid/stock_evaluator.json"
    ))?;
    doc["default_producer"] = json!({"name":"sparse-producer","revision":1});
    Ok(doc)
}
fn dashboard() -> Result<Value> {
    Ok(serde_json::from_str(include_str!(
        "../../../tests/fixtures/contracts/dashboard_views/valid/tracks_vs_control.json"
    ))?)
}
fn validator() -> Result<Value> {
    Ok(serde_json::from_str(include_str!(
        "../../../tests/fixtures/contracts/step_manifest/valid/validator.json"
    ))?)
}
fn violation_path(value: &Value) -> Option<&str> {
    value["error"]["details"][0]["path"].as_str()
}
fn assert_revision_refusal(response: &Value) {
    assert_eq!(
        response,
        &json!({"error": {
            "code": "validation_failed", "message": "request validation failed",
            "details": [{"path": "path/revision", "message": "Input should be a signed 64-bit integer"}]
        }})
    );
}
fn assert_json_refusal(response: &Value, raw: &[u8]) -> Result<()> {
    let error = serde_json::from_slice::<Value>(raw)
        .err()
        .ok_or("native JSON depth probe unexpectedly accepted")?;
    assert_eq!(
        response,
        &json!({"error": {
            "code": "validation_failed", "message": "request validation failed",
            "details": [{"path": format!("body/{}", error.column()), "message": "JSON decode error"}]
        }})
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires uniquely owned migrated PostgreSQL"]
#[allow(
    clippy::too_many_lines,
    reason = "Complete source publication order and transactional effects are checked together"
)]
async fn config_publication_reads_semantics_and_bounded_json_refusal() -> Result<()> {
    let (app, pool) = fixture().await?;
    let admin = person(&pool, "admin-config", true).await?;
    let outsider = person(&pool, "outsider-config", false).await?;
    expect(
        &app,
        Method::POST,
        "/api/projects",
        Some(&admin),
        Some(&json!({"slug":"alpha","title":"Alpha","tracks":[{"slug":"main","title":"Main"}]})),
        201,
    )
    .await?;
    let base = "/api/projects/alpha/config";
    expect(
        &app,
        Method::GET,
        &format!("{base}/science/latest"),
        Some(&admin),
        None,
        404,
    )
    .await?;
    expect(
        &app,
        Method::POST,
        &format!("{base}/dashboard"),
        Some(&admin),
        Some(&dashboard()?),
        409,
    )
    .await?;
    let doc = science()?;
    let first = expect(
        &app,
        Method::POST,
        &format!("{base}/science"),
        Some(&admin),
        Some(&doc),
        201,
    )
    .await?;
    assert_eq!(first["revision"], 1);
    assert_eq!(first["science_revision"], Value::Null);
    assert_eq!(first["content"], doc);
    assert!(
        first["created_at"]
            .as_str()
            .ok_or("created timestamp")?
            .ends_with('Z')
    );
    expect(
        &app,
        Method::POST,
        &format!("{base}/science"),
        Some(&outsider),
        Some(&doc),
        403,
    )
    .await?;
    expect(
        &app,
        Method::GET,
        &format!("{base}/science/latest"),
        Some(&outsider),
        None,
        404,
    )
    .await?;
    let mut repeated = doc.clone();
    let metric = repeated["metrics"][0].clone();
    repeated["metrics"]
        .as_array_mut()
        .ok_or("metric list")?
        .push(metric);
    let refused = expect(
        &app,
        Method::POST,
        &format!("{base}/science"),
        Some(&admin),
        Some(&repeated),
        422,
    )
    .await?;
    assert_eq!(violation_path(&refused), Some("/metrics"));
    let mut repeated = doc.clone();
    let baseline = repeated["baselines"][0].clone();
    repeated["baselines"]
        .as_array_mut()
        .ok_or("baseline list")?
        .push(baseline);
    let refused = expect(
        &app,
        Method::POST,
        &format!("{base}/science"),
        Some(&admin),
        Some(&repeated),
        422,
    )
    .await?;
    assert_eq!(violation_path(&refused), Some("/baselines"));
    let mut validated = doc.clone();
    validated["validators"] = json!([validator()?]);
    validated["interfaces"][0]["validator"] = json!("run-validator");
    expect(
        &app,
        Method::POST,
        &format!("{base}/science"),
        Some(&admin),
        Some(&validated),
        201,
    )
    .await?;
    let mut unknown = validated.clone();
    unknown["interfaces"][0]["validator"] = json!("other-validator");
    let refused = expect(
        &app,
        Method::POST,
        &format!("{base}/science"),
        Some(&admin),
        Some(&unknown),
        422,
    )
    .await?;
    assert_eq!(violation_path(&refused), Some("/interfaces/0/validator"));
    let mut duplicate = validated.clone();
    duplicate["validators"] = json!([validator()?, validator()?]);
    let refused = expect(
        &app,
        Method::POST,
        &format!("{base}/science"),
        Some(&admin),
        Some(&duplicate),
        422,
    )
    .await?;
    assert_eq!(violation_path(&refused), Some("/validators"));
    let mut mismatch = validated.clone();
    mismatch["validators"][0]["spec"]["inputs"]["artifacts"][0]["interface"] =
        json!("per-query-results/v1");
    let refused = expect(
        &app,
        Method::POST,
        &format!("{base}/science"),
        Some(&admin),
        Some(&mismatch),
        422,
    )
    .await?;
    assert_eq!(violation_path(&refused), Some("/interfaces/0/validator"));
    let mut deadline = validated.clone();
    deadline["validators"][0]["spec"]["activeDeadlineSeconds"] = json!(1_000_000);
    let refused = expect(
        &app,
        Method::POST,
        &format!("{base}/science"),
        Some(&admin),
        Some(&deadline),
        422,
    )
    .await?;
    assert_eq!(
        violation_path(&refused),
        Some("/validators/0/spec/activeDeadlineSeconds")
    );
    let mut unknown = dashboard()?;
    unknown["views"][0]["metric"] = json!("recall");
    expect(
        &app,
        Method::POST,
        &format!("{base}/dashboard"),
        Some(&admin),
        Some(&unknown),
        422,
    )
    .await?;
    let mut dimension = dashboard()?;
    dimension["views"][0]["group_by"] = json!(["country"]);
    expect(
        &app,
        Method::POST,
        &format!("{base}/dashboard"),
        Some(&admin),
        Some(&dimension),
        422,
    )
    .await?;
    let published = expect(
        &app,
        Method::POST,
        &format!("{base}/dashboard"),
        Some(&admin),
        Some(&dashboard()?),
        201,
    )
    .await?;
    assert_eq!(published["science_revision"], 2);
    let listed = expect(
        &app,
        Method::GET,
        &format!("{base}/science?limit=1"),
        Some(&admin),
        None,
        200,
    )
    .await?;
    assert_eq!(listed["items"][0]["revision"], 2);
    assert_eq!(listed["next_before"], 2);
    let listed = expect(
        &app,
        Method::GET,
        &format!("{base}/science?before=2"),
        Some(&admin),
        None,
        200,
    )
    .await?;
    assert_eq!(listed["items"][0]["revision"], 1);
    assert_eq!(listed["next_before"], Value::Null);
    expect(
        &app,
        Method::GET,
        &format!("{base}/science/-1"),
        Some(&admin),
        None,
        404,
    )
    .await?;
    let overflow = expect(
        &app,
        Method::GET,
        &format!("{base}/science/9223372036854775808"),
        Some(&admin),
        None,
        422,
    )
    .await?;
    assert_revision_refusal(&overflow);
    expect(
        &app,
        Method::GET,
        &format!("{base}/science?before=0&limit=0"),
        Some(&admin),
        None,
        422,
    )
    .await?;
    // Syntax precedes authentication; model errors precede admin/schema/project checks.
    assert_eq!(
        call(
            &app,
            Method::POST,
            "/api/projects/missing/config/invalid",
            None,
            Some(b"{")
        )
        .await?
        .0,
        422
    );
    assert_eq!(
        call(
            &app,
            Method::POST,
            "/api/projects/missing/config/invalid",
            None,
            Some(b"[]")
        )
        .await?
        .0,
        401
    );
    let invalid = expect(
        &app,
        Method::POST,
        "/api/projects/missing/config/invalid",
        Some(&outsider),
        Some(&json!([])),
        422,
    )
    .await?;
    assert_eq!(invalid["error"]["details"][0]["path"], "path/kind");
    assert_eq!(invalid["error"]["details"][1]["path"], "body");
    expect(
        &app,
        Method::POST,
        "/api/projects/missing/config/science",
        Some(&admin),
        Some(&json!({})),
        422,
    )
    .await?;
    expect(
        &app,
        Method::POST,
        "/api/projects/missing/config/science",
        Some(&admin),
        Some(&doc),
        404,
    )
    .await?;
    // Native JSON depth refusal occurs before publication or audit writes.
    let mut deep = doc.clone();
    deep["hypothesis_fields"] = json!({"const":"FIXED-DEEP-PROBE"});
    let shallow = serde_json::to_string(&deep)?;
    let raw = shallow.replace(
        "\"FIXED-DEEP-PROBE\"",
        &format!("{}0{}", "[".repeat(254), "]".repeat(254)),
    );
    let count_before: i64 = sqlx::query_scalar("SELECT count(*) FROM config_revisions")
        .fetch_one(&pool)
        .await?;
    let (status, response) = call(
        &app,
        Method::POST,
        &format!("{base}/science"),
        Some(&admin),
        Some(raw.as_bytes()),
    )
    .await?;
    assert_eq!(status, 422);
    assert_json_refusal(&response, raw.as_bytes())?;
    let count_after: i64 = sqlx::query_scalar("SELECT count(*) FROM config_revisions")
        .fetch_one(&pool)
        .await?;
    assert_eq!(count_after, count_before);
    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_events WHERE action LIKE 'config.%' AND project_id=(SELECT id FROM projects WHERE slug='alpha') ORDER BY seq",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        actions,
        [
            "config.science_created",
            "config.science_created",
            "config.dashboard_created"
        ]
    );
    // The latest stored ordinary publication remains unchanged after refusal.
    let content:String=sqlx::query_scalar("SELECT content::text FROM config_revisions WHERE kind='science' AND project_id=(SELECT id FROM projects WHERE slug='alpha') ORDER BY revision DESC LIMIT 1").fetch_one(&pool).await?;
    assert_eq!(serde_json::from_str::<Value>(&content)?, validated);
    pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires uniquely owned migrated PostgreSQL"]
async fn config_concurrent_revisions_audit_rollback_and_immutability() -> Result<()> {
    let (app, pool) = fixture().await?;
    let admin = person(&pool, "admin-config-concurrency", true).await?;
    expect(
        &app,
        Method::POST,
        "/api/projects",
        Some(&admin),
        Some(&json!({"slug":"concurrent","title":"Concurrent","tracks":[{"slug":"main","title":"Main"}]})),
        201,
    )
    .await?;
    let path = "/api/projects/concurrent/config/science";
    let doc = science()?;
    let attempts = (0..8).map(|_| expect(&app, Method::POST, path, Some(&admin), Some(&doc), 201));
    let responses = futures_util::future::join_all(attempts).await;
    let mut revisions = responses
        .into_iter()
        .map(|v| {
            v.map(|v| v["revision"].as_i64().ok_or("revision"))
                .and_then(|v| v.map_err(Into::into))
        })
        .collect::<Result<Vec<_>>>()?;
    revisions.sort_unstable();
    assert_eq!(revisions, (1..=8).collect::<Vec<_>>());
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action='config.science_created' AND project_id=(SELECT id FROM projects WHERE slug='concurrent')").fetch_one(&pool).await?;
    assert_eq!(count, 8);
    // This transactionally scoped fixture trigger injects only the owned project's
    // config audit failure, proving row+audit atomicity rather than authorization.
    let mut conn = pool.acquire().await?;
    sqlx::raw_sql("CREATE FUNCTION conformance_config_audit_failure() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='config.science_created' AND NEW.project_id=(SELECT id FROM projects WHERE slug='concurrent') THEN RAISE EXCEPTION 'fixture audit failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER conformance_config_audit_failure BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION conformance_config_audit_failure()").execute(&mut *conn).await?;
    drop(conn);
    expect(&app, Method::POST, path, Some(&admin), Some(&doc), 500).await?;
    let latest = expect(
        &app,
        Method::GET,
        &format!("{path}/latest"),
        Some(&admin),
        None,
        200,
    )
    .await?;
    assert_eq!(latest["revision"], 8);
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM config_revisions WHERE project_id=(SELECT id FROM projects WHERE slug='concurrent')").fetch_one(&pool).await?;
    assert_eq!(count, 8);
    let mut conn = pool.acquire().await?;
    sqlx::raw_sql("DROP TRIGGER conformance_config_audit_failure ON audit_events; DROP FUNCTION conformance_config_audit_failure()").execute(&mut *conn).await?;
    for statement in [
        "UPDATE config_revisions SET content='{}' WHERE project_id=(SELECT id FROM projects WHERE slug='concurrent')",
        "DELETE FROM config_revisions WHERE project_id=(SELECT id FROM projects WHERE slug='concurrent')",
    ] {
        let error = sqlx::raw_sql(statement)
            .execute(&mut *conn)
            .await
            .err()
            .ok_or("append-only revision trigger must refuse mutation")?;
        assert_eq!(
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("P0001")
        );
    }
    drop(conn);
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM config_revisions WHERE project_id=(SELECT id FROM projects WHERE slug='concurrent')").fetch_one(&pool).await?;
    assert_eq!(count, 8);
    expect(&app, Method::POST, path, Some(&admin), Some(&doc), 201).await?;
    let latest = expect(
        &app,
        Method::GET,
        &format!("{path}/latest"),
        Some(&admin),
        None,
        200,
    )
    .await?;
    assert_eq!(latest["revision"], 9);
    pool.close().await;
    Ok(())
}

fn source_projection(value: &Value) -> Result<Value> {
    if let Some(error) = value.get("error") {
        let paths: Vec<Value> = error["details"].as_array().map_or_else(Vec::new, |items| {
            items.iter().map(|v| v["path"].clone()).collect()
        });
        return Ok(json!({"error":error["code"],"count":paths.len(),"paths":paths}));
    }
    if let Some(items) = value.get("items") {
        let items = items
            .as_array()
            .ok_or("source page")?
            .iter()
            .map(source_projection)
            .collect::<Result<Vec<_>>>()?;
        return Ok(json!({"items":items,"next_before":value["next_before"]}));
    }
    if !value["created_at"]
        .as_str()
        .ok_or("source timestamp")?
        .ends_with('Z')
    {
        return Err("source timestamp shape".into());
    }
    Ok(
        json!({"kind":value["kind"],"revision":value["revision"],"science_revision":value["science_revision"],"content":value["content"]}),
    )
}
fn unhex(value: &str) -> Result<Vec<u8>> {
    value
        .as_bytes()
        .chunks(2)
        .map(|chunk| Ok(u8::from_str_radix(std::str::from_utf8(chunk)?, 16)?))
        .collect()
}

#[tokio::test]
#[ignore = "requires uniquely owned migrated PostgreSQL"]
#[allow(
    clippy::too_many_lines,
    reason = "Keep corpus setup, ordered source requests and persisted checks together"
)]
async fn configuration_matches_real_python_http_and_persisted_reference() -> Result<()> {
    let reference: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/server/tests/fixtures/config_http_reference.json"
    ))?;
    assert_eq!(reference["python"], "3.13.11");
    assert_eq!(reference["pydantic"], "2.13.5");
    let cases = reference["cases"]
        .as_array()
        .ok_or("HTTP reference cases")?;
    assert_eq!(reference["count"], 31);
    assert_eq!(cases.len(), 31);
    for (id, boundary) in [
        ("overflow-revision", "signed_i64_revision"),
        ("model-failure-after-commit", "json_nesting"),
    ] {
        assert_eq!(
            cases
                .iter()
                .filter(|case| case["id"] == id && case["native_boundary"] == boundary)
                .count(),
            1
        );
    }
    assert_eq!(
        cases
            .iter()
            .filter(|case| case["native_boundary"].is_null())
            .count(),
        29
    );
    let (app, pool) = fixture().await?;
    let admin = person(&pool, "admin-config-matrix", true).await?;
    expect(
        &app,
        Method::POST,
        "/api/projects",
        Some(&admin),
        Some(&json!({"slug":"matrix","title":"Matrix","tracks":[{"slug":"main","title":"Main"}]})),
        201,
    )
    .await?;
    let mut roles = BTreeMap::from([("admin", admin.clone())]);
    for role in ["researcher", "viewer", "outsider"] {
        let token = person(&pool, &format!("config-matrix-{role}"), false).await?;
        if role != "outsider" {
            let user = expect(&app, Method::GET, "/api/me", Some(&token), None, 200).await?;
            let id = user["user"]["id"].as_str().ok_or("fixture user")?;
            expect(
                &app,
                Method::PUT,
                &format!("/api/projects/matrix/members/{id}"),
                Some(&admin),
                Some(&json!({"role":role})),
                200,
            )
            .await?;
        }
        roles.insert(role, token);
    }
    for kind in ["agent", "tester"] {
        expect(
            &app,
            Method::POST,
            "/api/projects/matrix/service-accounts",
            Some(&admin),
            Some(&json!({"kind":kind,"name":kind})),
            201,
        )
        .await?;
        // Match source seed_world's isolated token provisioning. Public token
        // issuance requires a browser session; this corpus uses bearer fixtures.
        let mut connection = pool.acquire().await?;
        let project = cannery_projects::repo::get_project_by_slug(&mut connection, "matrix")
            .await?
            .ok_or("fixture project")?;
        let account = repo::get_service_account_by_name(&mut connection, project.id, kind)
            .await?
            .ok_or("fixture service account")?;
        let secret = secrets::new_secret(secrets::SERVICE_PREFIX)?;
        repo::create_token(
            &mut connection,
            NewToken {
                secret_digest: secret.digest(),
                display_prefix: secret.display_prefix(),
                kind: TokenKind::Service,
                user_id: None,
                service_account_id: Some(account.id),
                name: "matrix fixture",
                scopes: &[Scope::Read, Scope::Write],
                expires_in_days: 1,
            },
        )
        .await?;
        roles.insert(kind, secret.plaintext().expose().to_owned());
    }
    for case in cases {
        let method = Method::from_bytes(
            case["method"]
                .as_str()
                .ok_or("reference method")?
                .as_bytes(),
        )?;
        let path = case["path"].as_str().ok_or("reference path")?;
        let role = case["role"].as_str().ok_or("reference role")?;
        let bytes = case["body_hex"].as_str().map(unhex).transpose()?;
        let (status, body) = call(
            &app,
            method,
            path,
            roles.get(role).map(String::as_str),
            bytes.as_deref(),
        )
        .await?;
        match case["native_boundary"].as_str() {
            Some("signed_i64_revision") => {
                assert_eq!(case["id"], "overflow-revision");
                assert_eq!(case["status"], 500);
                assert_eq!(case["output"], Value::Null);
                assert_eq!(status, 422);
                assert_revision_refusal(&body);
            }
            Some("json_nesting") => {
                assert_eq!(case["id"], "model-failure-after-commit");
                assert_eq!(case["status"], 500);
                assert_eq!(case["output"], Value::Null);
                assert_eq!(status, 422);
                assert_json_refusal(&body, bytes.as_deref().ok_or("JSON depth body")?)?;
            }
            None if case["native_boundary"].is_null() => {
                assert_eq!(
                    u64::from(status),
                    case["status"].as_u64().ok_or("reference status")?,
                    "source case {}",
                    case["id"]
                );
                let actual = if status == 500 {
                    Value::Null
                } else {
                    source_projection(&body)?
                };
                if case["id"] == "schema-before-project" {
                    assert_eq!(
                        case["output"],
                        json!({"error":"validation_failed","paths":["","","","","","","","","","",""],"count":11})
                    );
                    assert_eq!(
                        actual,
                        json!({"error":"validation_failed","paths":[""],"count":1})
                    );
                } else {
                    assert_eq!(actual, case["output"], "source case {}", case["id"]);
                }
            }
            _ => return Err("unknown native configuration boundary".into()),
        }
    }
    // Preserve the source's post-commit failure independently of native refusal:
    // its final probe adds exactly one science revision and one audit event.
    let native_rows = reference["native_revisions"]
        .as_array()
        .ok_or("native revisions")?;
    let source_rows = reference["revisions"]
        .as_array()
        .ok_or("source revisions")?;
    assert_eq!(source_rows.len(), native_rows.len() + 1);
    assert_eq!(&source_rows[..native_rows.len()], native_rows.as_slice());
    let last_revision = native_rows.last().ok_or("ordinary publication")?[1]
        .as_i64()
        .ok_or("ordinary revision")?;
    assert_eq!(
        source_rows.last(),
        Some(&json!(["science", last_revision + 1, null]))
    );
    let mut source_actions = reference["native_actions"]
        .as_array()
        .ok_or("native actions")?
        .clone();
    source_actions.push(json!("config.science_created"));
    assert_eq!(reference["actions"], Value::Array(source_actions));
    let rows:Vec<(String,i32,Option<i32>)>=sqlx::query_as("SELECT kind,revision,science_revision FROM config_revisions WHERE project_id=(SELECT id FROM projects WHERE slug='matrix') ORDER BY kind,revision").fetch_all(&pool).await?;
    assert_eq!(serde_json::to_value(rows)?, reference["native_revisions"]);
    let actions:Vec<String>=sqlx::query_scalar("SELECT action FROM audit_events WHERE project_id=(SELECT id FROM projects WHERE slug='matrix') AND action LIKE 'config.%' ORDER BY seq").fetch_all(&pool).await?;
    assert_eq!(serde_json::to_value(actions)?, reference["native_actions"]);
    pool.close().await;
    Ok(())
}
