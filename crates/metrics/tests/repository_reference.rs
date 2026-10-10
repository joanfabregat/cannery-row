//! Complete source query projections, including failure outcomes, on actual PostgreSQL.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use cannery_core::{ids::ProjectId, json, timestamps::Timestamp};
use cannery_metrics::{
    numeric::PgNumeric,
    repo::{self, JsonContext, MetricsError, Point, Query, TimeBound},
};
use chrono::NaiveDateTime;
use num_bigint::{BigInt, Sign};
use serde_json::{Value, json};
use sqlx::{Column, Executor, PgConnection, Row};
use std::{env, error::Error, str::FromStr};

fn reference() -> Value {
    let path = env::var("CANNERY_METRICS_REFERENCE").expect("actual source reference required");
    serde_json::from_slice(&std::fs::read(path).expect("source reference readable"))
        .expect("source reference JSON")
}
async fn connection() -> Result<PgConnection, Box<dyn Error>> {
    let dsn = env::var("CANNERY_METRICS_TEST_DATABASE_URL")?;
    let options = cannery_core::db::DatabaseOptions::parse(&dsn)?;
    let mut c = options
        .connect(None)
        .await
        .map_err(|_| "metrics fixture connection failed")?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&mut c)
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
    c.execute("SET TIME ZONE 'UTC'").await?;
    let seeded: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projects)")
        .fetch_one(&mut c)
        .await?;
    if !seeded {
        sqlx::raw_sql(include_str!("fixtures/seed.sql"))
            .execute(&mut c)
            .await?;
    }
    Ok(c)
}
fn numeric(value: &PgNumeric) -> Value {
    match value {
        PgNumeric::Finite { coefficient, scale } => {
            json!({"decimal":{"sign":u8::from(coefficient.sign()==Sign::Minus),"digits":coefficient.magnitude().to_string(),"exponent":(-i32::from(*scale)).to_string()}})
        }
        PgNumeric::NaN => json!({"decimal":{"sign":0,"digits":"","exponent":"n"}}),
        PgNumeric::PositiveInfinity => json!({"decimal":{"sign":0,"digits":"0","exponent":"F"}}),
        PgNumeric::NegativeInfinity => json!({"decimal":{"sign":1,"digits":"0","exponent":"F"}}),
    }
}
fn document(value: &json::Document) -> Value {
    serde_json::from_str(&json::encode_ascii_pretty(value, 1000).expect("fixture JSON projection"))
        .expect("fixture JSON")
}
fn optional_document(value: Option<&json::Document>) -> Value {
    value.map_or(Value::Null, document)
}
fn datetime(value: Timestamp) -> Value {
    json!({"datetime":value.isoformat()})
}
fn optional_datetime(value: Option<Timestamp>) -> Value {
    value.map_or(Value::Null, datetime)
}
fn float(value: Option<f64>) -> Value {
    value.map_or(Value::Null, |v| {
        if v.is_finite() {
            json!(v)
        } else {
            json!({"float_bits":format!("{:016x}",v.to_bits())})
        }
    })
}
fn point(p: &Point) -> Value {
    json!({
    "id":p.id,"attempt_id":p.attempt_id.to_string(),"hypothesis_number":p.hypothesis_number,"hypothesis_title":p.hypothesis_title,"hypothesis_state":p.hypothesis_state,"attempt_sequence":p.attempt_sequence,"attempt_state":p.attempt_state,"science_revision":p.science_revision,
    "claimed_at":datetime(p.claimed_at),"submitted_at":optional_datetime(p.submitted_at),"finished_at":optional_datetime(p.finished_at),"track_slug":p.track_slug,"track_title":p.track_title,"metric":p.metric,"split":p.split,"dimensions":document(&p.dimensions),"value":float(p.value),"missing_reason":p.missing_reason,"unit":p.unit,"direction":p.direction,"sample_count":p.sample_count.as_ref().map(numeric),"control_value":float(p.control_value),"uncertainty_method":p.uncertainty_method,"uncertainty_lower":float(p.uncertainty_lower),"uncertainty_upper":float(p.uncertainty_upper),"authority":p.authority,"source_ref":p.source_ref,"recorded_at":datetime(p.recorded_at),"control":optional_document(p.control.as_ref()),"project_fields":optional_document(p.project_fields.as_ref()),"reference_value":float(p.reference_value),"reference_label":p.reference_label,"reference_kind":p.reference_kind,"reference_ref":p.reference_ref
    })
}
fn strings(value: &Value) -> Option<Vec<String>> {
    if value.is_null() {
        None
    } else {
        Some(
            value
                .as_array()
                .expect("string list recipe")
                .iter()
                .map(|v| v.as_str().expect("string recipe").to_owned())
                .collect(),
        )
    }
}
fn integer(value: &Value) -> Option<BigInt> {
    if value.is_null() {
        None
    } else if let Some(exponent) = value.get("power10") {
        Some(BigInt::from(10u8).pow(u32::try_from(exponent.as_u64().unwrap()).unwrap()))
    } else {
        Some(value.to_string().parse().expect("integer recipe"))
    }
}
fn bound(value: &Value) -> Option<TimeBound> {
    value.as_str().map(|s| {
        Timestamp::from_str(s).map_or_else(
            |_| {
                TimeBound::Naive(
                    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").expect("naive recipe"),
                )
            },
            TimeBound::Aware,
        )
    })
}
fn query(r: &Value) -> Query {
    let mut q = Query::new(
        ProjectId::from_str("00000000-0000-0000-0000-000000000002").unwrap(),
        r["metric"].as_str().unwrap_or("score").into(),
        r["authority"].as_str().unwrap_or("tester_verified").into(),
    );
    q.split = r["split"].as_str().map(str::to_owned);
    if let Some(d) = r.get("dimensions") {
        q.dimensions = strings(d);
    }
    q.filters = r["filters"].as_object().map(|f| {
        f.iter()
            .map(|(k, v)| (k.clone(), strings(v).expect("filter recipe")))
            .collect()
    });
    q.tracks = strings(&r["tracks"]);
    q.attempt_states = strings(&r["attempt_states"]);
    q.science_revision = integer(&r["science_revision"]);
    q.since = bound(&r["since"]);
    q.until = bound(&r["until"]);
    q
}
fn compare_result<T>(
    actual: Result<T, MetricsError>,
    expected: &Value,
    project: impl FnOnce(T) -> Value,
    name: &str,
    operation: &str,
) {
    match actual {
        Ok(value) => assert_eq!(project(value), expected["ok"], "{name}/{operation}"),
        Err(MetricsError::Database { sqlstate }) => {
            assert!(
                expected["error"]["class"].is_string(),
                "{name}/{operation}: unexpected native failure"
            );
            assert_eq!(
                json!(sqlstate),
                expected["error"]["sqlstate"],
                "{name}/{operation}: SQLSTATE"
            );
        }
        Err(MetricsError::Decode) => panic!("{name}/{operation}: unexpected JSON decode failure"),
    }
}
#[tokio::test]
#[ignore = "requires source-seeded isolated migrated PostgreSQL fixture"]
async fn queries_match_actual_source() -> Result<(), Box<dyn Error>> {
    let fixture = reference();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 75);
    let mut c = connection().await?;
    // Verify the checked-query cache against the actual migrated PostgreSQL
    // schema after adding explicit casts for source parameter OIDs.
    for (statement, metadata) in [
        (
            include_str!("../src/sql/points.sql"),
            include_str!(
                "../../../.sqlx/query-484dca16323b85511b72f610e37f807104bef4ca5d8c67ac7d4c08551a309679.json"
            ),
        ),
        (
            include_str!("../src/sql/summary.sql"),
            include_str!(
                "../../../.sqlx/query-b37b480ef7429dcd630e9c18a33758711411f6c5c1eae7b7c78a947f6a2038b4.json"
            ),
        ),
        (
            include_str!("../src/sql/failed.sql"),
            include_str!(
                "../../../.sqlx/query-90ed4a33dd81ddaabbb985cf0936b59c0da9243905a4d5bbc2bcc2aa65367ffc.json"
            ),
        ),
    ] {
        let metadata: Value = serde_json::from_str(metadata)?;
        assert_eq!(metadata["query"], statement);
        let description = c.describe(statement).await?;
        let columns: Vec<Value> = description
            .columns()
            .iter()
            .map(|column| {
                json!({
                    "ordinal": column.ordinal(), "name": column.name(),
                "type_info": format!("{:?}", **column.type_info()),
                })
            })
            .collect();
        let parameters: Vec<String> = description
            .parameters()
            .ok_or("parameter types required")?
            .left()
            .ok_or("PostgreSQL parameter types required")?
            .iter()
            .map(|parameter| format!("{:?}", **parameter))
            .collect();
        let nullable: Vec<Option<bool>> = (0..columns.len())
            .map(|index| description.nullable(index))
            .collect();
        assert_eq!(
            json!({"columns":columns,"parameters":{"Left":parameters},"nullable":nullable}),
            metadata["describe"]
        );
    }
    for case in cases {
        let r = &case["recipe"];
        let name = r["name"].as_str().unwrap();
        let q = query(r);
        let expected = &case["outcomes"];
        let before = integer(&r["before"]);
        let limit = integer(r.get("limit").unwrap_or(&json!(50)));
        compare_result(
            repo::points(
                &mut c,
                &q,
                before.as_ref(),
                limit.as_ref(),
                JsonContext {
                    nesting_budget: 1000,
                },
            )
            .await,
            &expected["points"],
            |p| Value::Array(p.iter().map(point).collect()),
            name,
            "points",
        );
        compare_result(
            repo::summary(
                &mut c,
                &q,
                JsonContext {
                    nesting_budget: 1000,
                },
            )
            .await,
            &expected["summary"],
            |s| json!({"rows":s.rows,"measured":s.measured,"sample_count":numeric(&s.sample_count),"science_revisions":s.science_revisions,"controls":document(&s.controls)}),
            name,
            "summary",
        );
        compare_result(
            repo::failed_attempts(&mut c, &q).await,
            &expected["failed_attempts"],
            |n| json!(n),
            name,
            "failed_attempts",
        );
    }
    Ok(())
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL fixture"]
async fn numeric_binary_text_and_binding_match_source() -> Result<(), Box<dyn Error>> {
    let fixture = reference();
    let mut c = connection().await?;
    for case in fixture["numeric"].as_array().unwrap() {
        let text = case["input"].as_str().unwrap();
        let value: PgNumeric = sqlx::query_scalar("SELECT $1::text::numeric")
            .bind(text)
            .fetch_one(&mut c)
            .await?;
        assert_eq!(numeric(&value), case["value"]);
        let rebound: PgNumeric = sqlx::query_scalar("SELECT $1::numeric")
            .bind(&value)
            .fetch_one(&mut c)
            .await?;
        assert_eq!(numeric(&rebound), case["value"]);
        let bytes: Vec<u8> = sqlx::query_scalar("SELECT numeric_send($1::numeric)")
            .bind(&value)
            .fetch_one(&mut c)
            .await?;
        let mut hex = String::new();
        for byte in bytes {
            use std::fmt::Write;
            write!(hex, "{byte:02x}").expect("writing to String");
        }
        assert_eq!(json!(hex), case["wire"]);
        let row = c
            .fetch_one(format!("SELECT '{text}'::numeric AS n").as_str())
            .await?;
        let textual: PgNumeric = row.try_get("n")?;
        assert_eq!(numeric(&textual), case["value"]);
    }
    Ok(())
}

/// Session-dependent selection is separate from the unresolved row-offset decoder.
#[tokio::test]
#[ignore = "requires isolated PostgreSQL fixture"]
async fn naive_and_aware_filters_use_current_session_timezone() -> Result<(), Box<dyn Error>> {
    let fixture = reference();
    let mut c = connection().await?;
    let cases = fixture["timezone_filters"].as_array().unwrap();
    assert_eq!(cases.len(), 9);
    for case in cases {
        sqlx::query("SELECT set_config('TimeZone',$1,false)")
            .bind(case["zone"].as_str().unwrap())
            .execute(&mut c)
            .await?;
        let mut q = query(&json!({}));
        q.since = bound(&case["since"]);
        let rows = repo::points(
            &mut c,
            &q,
            None,
            Some(&BigInt::from(50)),
            JsonContext {
                nesting_budget: 1000,
            },
        )
        .await?;
        assert_eq!(
            json!(rows.iter().map(|p| p.id).collect::<Vec<_>>()),
            case["ids"]
        );
        let summary = repo::summary(
            &mut c,
            &q,
            JsonContext {
                nesting_budget: 1000,
            },
        )
        .await?;
        assert_eq!(json!(summary.rows), case["summary_rows"]);
        assert_eq!(
            json!(repo::failed_attempts(&mut c, &q).await?),
            case["failed_attempts"]
        );
    }
    Ok(())
}
