//! Actual production-source outcomes and complete native records on PostgreSQL.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use cannery_comments_reports::{
    RepositoryError,
    comments::{self, Comment, CommentId, CommentRevision},
    reports::{self, EvidenceId, EvidenceRow, ImportedReportRow, JsonContext, ReportRow},
};
use cannery_core::{
    ids::{AttemptId, ProjectId, UnitId, UserId},
    json,
    principal::{Channel, UserPrincipal, Via},
    timestamps::Timestamp,
};
use num_bigint::BigInt;
use serde_json::{Value, json as value};
use sqlx::{Executor, PgConnection, Row};
use std::{collections::BTreeSet, error::Error};
use uuid::Uuid;
fn uid(n: u128) -> Uuid {
    Uuid::from_u128(n)
}
fn integer(v: &Value) -> Option<BigInt> {
    if v.is_null() {
        None
    } else if let Some(power) = v.get("power10") {
        Some(BigInt::from(10u8).pow(u32::try_from(power.as_u64().unwrap()).unwrap()))
    } else {
        Some(v.to_string().parse().unwrap())
    }
}
fn datetime(v: Timestamp) -> Value {
    value!({"datetime":v.isoformat()})
}
fn optional_datetime(v: Option<Timestamp>) -> Value {
    v.map_or(Value::Null, datetime)
}
fn document(v: &json::Document) -> Value {
    serde_json::from_str(&json::encode_ascii_pretty(v, 1000).unwrap()).unwrap()
}
fn comment(c: &Comment) -> Value {
    value!({"id":c.id.0.to_string(),"project_id":c.project_id.to_string(),"unit_id":c.unit_id.to_string(),"unit_number":c.unit_number,"attempt_id":c.attempt_id.map(|i|i.to_string()),"attempt_sequence":c.attempt_sequence,"author_user":c.author_user.to_string(),"body_markdown":c.body_markdown,"revision":c.revision,"created_at":datetime(c.created_at),"edited_at":optional_datetime(c.edited_at)})
}
fn revision(c: &CommentRevision) -> Value {
    value!({"revision":c.revision,"body_markdown":c.body_markdown,"via_channel":c.via_channel,"via_client":c.via_client,"created_at":datetime(c.created_at)})
}
fn report(r: &ReportRow) -> Value {
    value!({"id":r.id.0.to_string(),"attempt_id":r.attempt_id.to_string(),"status":r.status.as_str(),"report":r.report.as_ref().map(document),"created_at":datetime(r.created_at),"producer_user":r.producer_user.map(|i|i.to_string()),"producer_service":r.producer_service.map(|i|i.to_string()),"unit_number":r.unit_number,"unit_title":r.unit_title,"track_slug":r.track_slug,"attempt_sequence":r.attempt_sequence,"attempt_state":r.attempt_state.as_str(),"origin":r.origin.as_str()})
}
fn evidence(r: &EvidenceRow) -> Value {
    value!({"id":r.id.0.to_string(),"stage":r.stage.as_str(),"status":r.status.as_str(),"revision":r.revision,"content":document(&r.content),"producer_user":r.producer_user.map(|i|i.to_string()),"producer_service":r.producer_service.map(|i|i.to_string()),"created_at":datetime(r.created_at),"origin":r.origin.as_str(),"source_ref":r.source_ref})
}
fn imported(r: &ImportedReportRow) -> Value {
    value!({"kind":r.kind.as_str(),"author":r.author,"written_on":r.written_on.map(|d|value!({"date":d.to_string()})),"written_at":optional_datetime(r.written_at),"body_markdown":r.body_markdown,"source_ref":r.source_ref})
}
fn author() -> UserPrincipal {
    UserPrincipal {
        user_id: UserId(uid(1)),
        email: None,
        display_name: None,
        is_admin: false,
        via: Via {
            channel: Channel::Cli,
            client: Some("fixture-é".into()),
        },
        scopes: BTreeSet::new(),
        session_id: None,
        csrf_token: None,
    }
}
#[allow(clippy::too_many_lines)]
async fn operation(conn: &mut PgConnection, r: &Value) -> Result<Value, RepositoryError> {
    let operation = r["operation"].as_str().unwrap();
    let project = ProjectId(uid(2));
    let identifier = CommentId(uid(u128::from(r["id"].as_u64().unwrap_or(300))));
    let attempt = r["attempt"].as_u64().map(|n| AttemptId(uid(u128::from(n))));
    let before = r["before"].as_u64().map(|n| uid(u128::from(n)));
    let limit = integer(r.get("limit").unwrap_or(&value!(50)));
    let context = JsonContext {
        decode_nesting_budget: 1000,
    };
    match operation {
        "get_comment" => {
            Ok(
                comments::get_comment(conn, project, identifier, r["lock"].as_bool().unwrap())
                    .await?
                    .as_ref()
                    .map_or(Value::Null, comment),
            )
        }
        "list_comments" => Ok(Value::Array(
            comments::list_comments(
                conn,
                UnitId(uid(100)),
                attempt,
                before.map(CommentId),
                limit.as_ref(),
            )
            .await?
            .iter()
            .map(comment)
            .collect(),
        )),
        "list_revisions" => Ok(Value::Array(
            comments::list_revisions(
                conn,
                identifier,
                integer(&r["after"]).as_ref(),
                limit.as_ref(),
            )
            .await?
            .iter()
            .map(revision)
            .collect(),
        )),
        "list_reports" => Ok(Value::Array(
            reports::list_reports(
                conn,
                project,
                integer(&r["number"]).as_ref(),
                r["track"].as_str(),
                before.map(EvidenceId),
                limit.as_ref(),
                context,
            )
            .await?
            .iter()
            .map(report)
            .collect(),
        )),
        "latest_evidence" => {
            let records = reports::latest_evidence(conn, attempt.unwrap(), context).await?;
            Ok(Value::Object(
                records
                    .iter()
                    .map(|(stage, r)| (stage.as_str().into(), evidence(r)))
                    .collect(),
            ))
        }
        "result_case_ids" => Ok(value!(
            reports::result_case_ids(conn, attempt.unwrap())
                .await?
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        )),
        "imported_report" => Ok(reports::imported_report(conn, attempt.unwrap())
            .await?
            .as_ref()
            .map_or(Value::Null, imported)),
        "create_comment" | "edit_comment" | "add_revision" => {
            let body = r["body"].as_str().unwrap();
            let author = author();
            let mut identifier = identifier;
            match operation {
                "create_comment" => {
                    identifier = comments::create_comment(
                        conn,
                        project,
                        UnitId(uid(100)),
                        None,
                        body,
                        &author,
                    )
                    .await?;
                }
                "edit_comment" => {
                    comments::get_comment(conn, project, identifier, true).await?;
                    comments::edit_comment(conn, identifier, body).await?;
                }
                _ => {
                    comments::add_revision(
                        conn,
                        identifier,
                        &integer(r.get("revision").unwrap_or(&value!(3))).unwrap(),
                        body,
                        &author,
                    )
                    .await?;
                }
            }
            let record = comments::get_comment(conn, project, identifier, false)
                .await?
                .unwrap();
            let revisions =
                comments::list_revisions(conn, identifier, None, Some(&BigInt::from(50))).await?;
            let clock: Timestamp = sqlx::query_scalar("SELECT transaction_timestamp()")
                .fetch_one(&mut *conn)
                .await
                .map_err(|e| RepositoryError::Database {
                    sqlstate: e
                        .as_database_error()
                        .and_then(sqlx::error::DatabaseError::code)
                        .map(std::borrow::Cow::into_owned),
                })?;
            let mut record = comment(&record);
            if operation == "create_comment" {
                record["id"] = value!("generated");
            }
            let mut revisions: Vec<Value> = revisions.iter().map(revision).collect();
            for record in std::iter::once(&mut record).chain(revisions.iter_mut()) {
                for field in ["created_at", "edited_at"] {
                    if record.get(field) == Some(&datetime(clock)) {
                        record[field] = value!({"database_clock":true});
                    }
                }
            }
            let row=sqlx::query("SELECT title,body,actor_user,actor_service,project_id,unit_id,attempt_id,occurred_at,updated_at,tsv::text FROM search_documents WHERE kind='comment' AND source_id=$1").bind(identifier.0).fetch_optional(conn).await.map_err(|_|RepositoryError::Invariant)?;
            let search = row.map_or(Value::Null, |r| {
                let time = |i| {
                    let value = r.get::<Timestamp, _>(i);
                    if value == clock {
                        value!({"database_clock":true})
                    } else {
                        datetime(value)
                    }
                };
                value!([
                    r.get::<String, _>(0),
                    r.get::<String, _>(1),
                    r.get::<Option<Uuid>, _>(2).map(|u| u.to_string()),
                    r.get::<Option<Uuid>, _>(3).map(|u| u.to_string()),
                    r.get::<Uuid, _>(4).to_string(),
                    r.get::<Option<Uuid>, _>(5).map(|u| u.to_string()),
                    r.get::<Option<Uuid>, _>(6).map(|u| u.to_string()),
                    time(7usize),
                    time(8usize),
                    r.get::<String, _>(9)
                ])
            });
            Ok(value!({"record":record,"revisions":revisions,"search":search}))
        }
        _ => Err(RepositoryError::Invariant),
    }
}
fn error_matches(actual: &RepositoryError, expected: &Value) -> bool {
    let class = expected["class"].as_str().unwrap();
    let sqlstate = expected["sqlstate"].as_str();
    match actual {
        RepositoryError::Database { sqlstate: actual } => actual.as_deref() == sqlstate,
        RepositoryError::Invariant => class == "AssertionError" && sqlstate.is_none(),
        RepositoryError::TextEncoding => class == "DataError" && sqlstate.is_none(),
        RepositoryError::IntegerEncoding => {
            ["error", "OverflowError", "DataError"].contains(&class) && sqlstate.is_none()
        }
        RepositoryError::JsonDecode(_) => {
            ["ValueError", "RecursionError"].contains(&class) && sqlstate.is_none()
        }
        RepositoryError::CorruptData | RepositoryError::PreparationMaintenanceRequired => false,
    }
}
async fn connection() -> Result<PgConnection, Box<dyn Error>> {
    let dsn = std::env::var("CANNERY_COMMENTS_REPORTS_DATABASE_URL")?;
    let mut conn = cannery_core::db::DatabaseOptions::parse(&dsn)?
        .connect(None)
        .await
        .map_err(|_| "connection failed")?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&mut conn)
        .await?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("isolated fixture required")?;
    assert!(
        suffix.len() == 24
            && suffix
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    conn.execute("SET TIME ZONE 'UTC'").await?;
    let seeded: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projects)")
        .fetch_one(&mut conn)
        .await?;
    if !seeded {
        sqlx::raw_sql(include_str!("fixtures/seed.sql"))
            .execute(&mut conn)
            .await?;
    }
    Ok(conn)
}
async fn raw_storage(conn: &mut PgConnection) -> Result<Value, Box<dyn Error>> {
    let mut snapshot = value!({});
    for (table, order) in [
        ("comments", "id"),
        ("comment_revisions", "comment_id,revision"),
        ("mentions", "source_type,source_id,target_id"),
        ("search_documents", "id"),
        ("phase_outputs", "id"),
        ("review_cases", "id"),
        ("audit_events", "seq"),
        ("attempts", "id"),
        ("units", "id"),
    ] {
        snapshot[table] = value!(
            sqlx::query_scalar::<_, String>(&format!(
                "SELECT to_jsonb(t)::text FROM {table} t ORDER BY {order}"
            ))
            .fetch_all(&mut *conn)
            .await?
        );
    }
    Ok(snapshot)
}
#[tokio::test]
#[ignore = "requires actual source reference and a fresh guarded migrated PostgreSQL database"]
#[allow(
    clippy::too_many_lines,
    reason = "sequential corpus replay preserves transaction and storage assertions"
)]
async fn repository_reference() -> Result<(), Box<dyn Error>> {
    let reference: Value = serde_json::from_slice(&std::fs::read(std::env::var(
        "CANNERY_COMMENTS_REPORTS_REFERENCE",
    )?)?)?;
    assert_eq!(reference["source_only"].as_array().unwrap().len(), 4);
    for case in reference["source_only"].as_array().unwrap() {
        assert_eq!(
            case["outcome"]["error"],
            value!({"class":"UnicodeEncodeError","sqlstate":null})
        );
        assert!(
            case["recipe"]["body"].get("python_codepoints").is_some()
                || case["recipe"]["track"].get("python_codepoints").is_some()
        );
    }
    let mut conn = connection().await?;
    for (index, case) in reference["cases"].as_array().unwrap().iter().enumerate() {
        // Every recipe starts from unchanged fixed recovery rows.
        conn.execute("BEGIN").await?;
        let initial = raw_storage(&mut conn).await?;
        let outcome = operation(&mut conn, &case["recipe"]).await;
        if case["native_profile"] == "checked-i64" {
            assert_eq!(case["outcome"]["error"]["sqlstate"], "22003");
            assert!(matches!(outcome, Err(RepositoryError::IntegerEncoding)));
            assert_eq!(
                raw_storage(&mut conn).await?,
                initial,
                "checked prebind refusal changed raw storage"
            );
            conn.execute("ROLLBACK").await?;
            continue;
        }
        conn.execute("ROLLBACK").await?;
        assert_eq!(
            raw_storage(&mut conn).await?,
            initial,
            "recipe transaction cleanup changed raw storage"
        );
        match outcome {
            Ok(value) => assert_eq!(
                value, case["outcome"]["ok"],
                "case {index}: {}",
                case["recipe"]["operation"]
            ),
            Err(error) => assert!(
                error_matches(&error, &case["outcome"]["error"]),
                "case {index}: {error:?} / {}",
                case["outcome"]["error"]
            ),
        }
    }
    compare_plans(&reference).await?;
    compare_locks(&reference).await?;
    compare_dates(&reference).await?;
    compare_json_profiles(&reference).await?;
    let null_report = reports::list_reports(
        &mut conn,
        ProjectId(uid(2)),
        Some(&BigInt::from(2)),
        None,
        None,
        Some(&BigInt::from(50)),
        JsonContext {
            decode_nesting_budget: 9989,
        },
    )
    .await?;
    assert!(matches!(
        null_report[0]
            .report
            .as_ref()
            .and_then(|d| d.node(d.root())),
        Some(json::Node::Null)
    ));
    let array_report = reports::list_reports(
        &mut conn,
        ProjectId(uid(2)),
        Some(&BigInt::from(5)),
        None,
        None,
        Some(&BigInt::from(50)),
        JsonContext {
            decode_nesting_budget: 9989,
        },
    )
    .await?;
    assert!(array_report[0].report.is_none());
    let latest = reports::latest_evidence(
        &mut conn,
        AttemptId(uid(200)),
        JsonContext {
            decode_nesting_budget: 9989,
        },
    )
    .await?;
    assert_eq!(
        latest
            .keys()
            .map(|stage| stage.as_str())
            .collect::<Vec<_>>(),
        ["agent", "verification"]
    );
    compare_storage(&mut conn, &reference).await?;
    Ok(())
}

async fn compare_storage(conn: &mut PgConnection, reference: &Value) -> Result<(), Box<dyn Error>> {
    let rows=sqlx::query("SELECT id::text,body_markdown,revision,created_at::text,edited_at::text FROM comments ORDER BY id").fetch_all(&mut *conn).await?;
    let storage: Vec<Value> = rows
        .iter()
        .map(|r| {
            value!([
                r.get::<String, _>(0),
                r.get::<String, _>(1),
                r.get::<i32, _>(2),
                r.get::<String, _>(3),
                r.get::<Option<String>, _>(4)
            ])
        })
        .collect();
    assert_eq!(value!(storage), reference["storage"]);
    let rows = sqlx::query(
        "SELECT id::text,front_matter::text,created_at::text FROM phase_outputs WHERE stage<>'writeup' ORDER BY id",
    )
    .fetch_all(&mut *conn)
    .await?;
    assert_eq!(
        value!(
            rows.iter()
                .map(|r| value!([
                    r.get::<String, _>(0),
                    r.get::<String, _>(1),
                    r.get::<String, _>(2)
                ]))
                .collect::<Vec<_>>()
        ),
        reference["evidence_storage"]
    );
    let rows=sqlx::query("SELECT comment_id::text,revision,body_markdown,via_channel,via_client,created_at::text FROM comment_revisions ORDER BY comment_id,revision").fetch_all(&mut *conn).await?;
    assert_eq!(
        value!(
            rows.iter()
                .map(|r| value!([
                    r.get::<String, _>(0),
                    r.get::<i32, _>(1),
                    r.get::<String, _>(2),
                    r.get::<String, _>(3),
                    r.get::<Option<String>, _>(4),
                    r.get::<String, _>(5)
                ]))
                .collect::<Vec<_>>()
        ),
        reference["revision_storage"]
    );
    let rows=sqlx::query("SELECT source_type,source_id::text,target_id::text FROM mentions ORDER BY source_type,source_id,target_id").fetch_all(&mut *conn).await?;
    assert_eq!(
        value!(
            rows.iter()
                .map(|r| value!([
                    r.get::<String, _>(0),
                    r.get::<String, _>(1),
                    r.get::<String, _>(2)
                ]))
                .collect::<Vec<_>>()
        ),
        reference["mentions"]
    );
    Ok(())
}

async fn compare_plans(reference: &Value) -> Result<(), Box<dyn Error>> {
    let mut mode = String::new();
    let mut conn = connection().await?;
    for probe in reference["plans"].as_array().unwrap() {
        let next = probe["mode"].as_str().unwrap();
        if next != mode {
            mode = next.to_owned();
            conn = connection().await?;
            sqlx::query("SELECT set_config('plan_cache_mode',$1,false)")
                .bind(&mode)
                .execute(&mut conn)
                .await?;
        }
        let operation_name = probe["operation"].as_str().unwrap();
        let recipe = match operation_name {
            "list_comments" => value!({"operation":operation_name,"limit":probe["integer"]}),
            "list_revisions" => {
                value!({"operation":operation_name,"id":301,"after":probe["integer"]})
            }
            _ => value!({"operation":operation_name,"number":probe["integer"]}),
        };
        for (iteration, expected) in probe["observations"].as_array().unwrap().iter().enumerate() {
            if probe["integer"].as_u64() == Some(9_223_372_036_854_775_808) {
                let initial = raw_storage(&mut conn).await?;
                assert!(matches!(
                    operation(&mut conn, &recipe).await,
                    Err(RepositoryError::IntegerEncoding)
                ));
                assert_eq!(raw_storage(&mut conn).await?, initial);
                assert_eq!(expected["error"]["sqlstate"], "22003");
                continue;
            }
            match operation(&mut conn, &recipe).await {
                Ok(value) => {
                    assert_eq!(value, expected["ok"], "{mode}/{operation_name}/{iteration}");
                }
                Err(error) => assert!(
                    error_matches(&error, &expected["error"]),
                    "{mode}/{operation_name}/{iteration}: {error:?}"
                ),
            }
        }
    }
    Ok(())
}
async fn compare_locks(reference: &Value) -> Result<(), Box<dyn Error>> {
    for probe in reference["locks"].as_array().unwrap() {
        let mut reader = connection().await?;
        let mut writer = connection().await?;
        reader.execute("BEGIN").await?;
        writer.execute("BEGIN").await?;
        writer.execute("SET LOCAL lock_timeout='20ms'").await?;
        let project = ProjectId(probe["project"].as_str().unwrap().parse()?);
        let identifier = CommentId(probe["id"].as_str().unwrap().parse()?);
        let result = comments::get_comment(&mut reader, project, identifier, true).await?;
        assert_eq!(result.is_some(), probe["found"].as_bool().unwrap());
        match comments::edit_comment(&mut writer, CommentId(uid(300)), "Concurrent é edit").await {
            Ok(revision) => assert_eq!(value!(revision), probe["outcome"]["ok"]),
            Err(error) => assert!(error_matches(&error, &probe["outcome"]["error"])),
        }
        writer.execute("ROLLBACK").await?;
        reader.execute("ROLLBACK").await?;
    }
    Ok(())
}
async fn compare_dates(reference: &Value) -> Result<(), Box<dyn Error>> {
    let mut conn = connection().await?;
    for probe in reference["dates"].as_array().unwrap() {
        conn.execute("BEGIN").await?;
        sqlx::query("INSERT INTO phase_outputs(project_id,attempt_id,stage,status,revision,front_matter,body,sha256,via_channel,via_client,origin,source_ref) SELECT project_id,id,'writeup','completed',1,jsonb_build_object('kind','retrospective','author','Date fixture','written_on',$2::date),'Historical report',$3,'cli','cannery import','imported','reports/date.md' FROM attempts WHERE id=$1").bind(uid(208)).bind(probe["input"].as_str().unwrap()).bind("0".repeat(64)).execute(&mut conn).await?;
        match reports::imported_report(&mut conn, AttemptId(uid(208))).await {
            Ok(report) => assert_eq!(
                report.as_ref().map_or(Value::Null, imported),
                probe["outcome"]["ok"]
            ),
            Err(error) => assert!(error_matches(&error, &probe["outcome"]["error"])),
        }
        conn.execute("ROLLBACK").await?;
    }
    Ok(())
}
async fn compare_json_profiles(reference: &Value) -> Result<(), Box<dyn Error>> {
    for name in ["latest_evidence", "list_reports"] {
        let profile = &reference["json_profiles"][name];
        let context = JsonContext {
            decode_nesting_budget: 128,
        };
        let mut conn = connection().await?;
        for probe in profile["records"].as_array().unwrap() {
            let recipe = &probe["recipe"];
            let depth = usize::try_from(recipe["depth"].as_u64().unwrap())?;
            let nested = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
            let stored_document = if name == "list_reports" {
                format!("{{\"report\":{nested}}}")
            } else {
                nested
            };
            let stage = if name == "list_reports" {
                "agent"
            } else {
                "verification"
            };
            conn.execute("BEGIN").await?;
            sqlx::query("INSERT INTO phase_outputs(id,project_id,attempt_id,stage,status,revision,front_matter,sha256,producer_user,via_channel,created_at) VALUES($1,$2,$3,$4,'completed',$8,$5::jsonb,$6,$7,'cli','2024-01-02Z')").bind(uid(9000)).bind(uid(2)).bind(uid(200)).bind(stage).bind(&stored_document).bind("0".repeat(64)).bind(if stage=="agent"{Some(uid(1))}else{None}).bind(if stage=="agent"{4_i32}else{5}).execute(&mut conn).await?;
            let result = if name == "list_reports" {
                reports::list_reports(
                    &mut conn,
                    ProjectId(uid(2)),
                    Some(&BigInt::from(1)),
                    None,
                    None,
                    Some(&BigInt::from(50)),
                    context,
                )
                .await
                .map(|mut rows| rows.remove(0).report.unwrap())
            } else {
                reports::latest_evidence(&mut conn, AttemptId(uid(200)), context)
                    .await
                    .map(|mut rows| rows.remove(&reports::Stage::Verification).unwrap().content)
            };
            match result {
                Ok(doc) => {
                    assert_ne!(
                        probe["native_refusal"], true,
                        "authored serde container boundary"
                    );
                    let mut root = doc.root();
                    let mut count = 0;
                    while let Some(json::Node::Array(children)) = doc.node(root) {
                        count += 1;
                        root = children[0];
                    }
                    let storage: String = sqlx::query_scalar(
                        "SELECT front_matter::text FROM phase_outputs WHERE id=$1",
                    )
                    .bind(uid(9000))
                    .fetch_one(&mut conn)
                    .await?;
                    assert_eq!(
                        value!({"array_depth":count,"storage_text":storage}),
                        probe["outcome"]["ok"]
                    );
                }
                Err(error) => {
                    assert_eq!(probe["native_refusal"], true);
                    assert!(matches!(error, RepositoryError::JsonDecode(_)));
                    let depth = recipe["depth"].as_u64().unwrap();
                    assert!(depth >= 128);
                    let storage: String = sqlx::query_scalar(
                        "SELECT front_matter::text FROM phase_outputs WHERE id=$1",
                    )
                    .bind(uid(9000))
                    .fetch_one(&mut conn)
                    .await?;
                    assert_eq!(value!(storage), probe["outcome"]["ok"]["storage_text"]);
                }
            }
            conn.execute("ROLLBACK").await?;
        }
    }
    Ok(())
}
