//! Real PostgreSQL comparison with unchanged production search queries.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::{ids::ProjectId, timestamps::Timestamp};
use cannery_search::repo::{self, ActorId, Criteria, Hit, SearchError, TimeBound};
use chrono::NaiveDateTime;
use num_bigint::BigInt;
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection, Row};
use std::error::Error;
use uuid::Uuid;

fn uid(value: u64) -> Uuid {
    Uuid::from_u128(u128::from(value))
}
fn integer(recipe: &Value, key: &str, default: Option<i64>) -> Option<BigInt> {
    if let Some(power) = recipe[format!("{key}_power")].as_u64() {
        Some(BigInt::from(10).pow(u32::try_from(power).unwrap()))
    } else {
        recipe.get(key).map_or_else(
            || default.map(BigInt::from),
            |value| value.as_str().map(|value| value.parse().unwrap()),
        )
    }
}
fn outside_native_integer_range(recipe: &Value) -> bool {
    ["ref_number", "ref_sequence", "limit", "after_id"]
        .into_iter()
        .filter(|key| !matches!(*key, "limit" | "after_id") || recipe["only_facets"] != true)
        .filter(|key| *key != "after_id" || recipe["after_score"].is_string())
        .filter_map(|key| integer(recipe, key, None))
        .any(|value| value < BigInt::from(i64::MIN) || value > BigInt::from(i64::MAX))
}
fn native_outcome(recipe: &Value) -> Value {
    let source = &recipe["outcome"];
    if !outside_native_integer_range(recipe) {
        return source.clone();
    }
    // Native BIGINT arguments reject overflow before PostgreSQL sees a query.
    // The frozen source either reports server overflow, rejects its NUMERIC
    // adapter, or ignores an oversized sequence without a reference number.
    match recipe["name"].as_str() {
        Some("ref_sequence-power-4300" | "ref_sequence-power-131071") => {
            assert_eq!(source["hits"], json!([]));
            assert_eq!(source["first"], Value::Null);
            assert_eq!(source["facets"][0], 21);
            assert!(source.get("error").is_none());
        }
        _ => assert!(
            source == &json!({"error":"Database","sqlstate":"22003"})
                || source == &json!({"error":"Database","sqlstate":null}),
            "unexpected source integer refusal: {}",
            recipe["name"]
        ),
    }
    json!({"error":"Database","sqlstate":null})
}
fn strings(recipe: &Value, key: &str) -> Option<Vec<String>> {
    recipe
        .get(key)
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().into())
                .collect()
        })
}
fn time(value: &Value) -> Option<TimeBound> {
    value.as_str().map(|value| {
        if value.ends_with('Z') || value.contains('+') {
            TimeBound::Aware(value.parse::<Timestamp>().unwrap())
        } else {
            TimeBound::Naive(NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S").unwrap())
        }
    })
}
fn criteria(recipe: &Value) -> Criteria {
    let mut c = Criteria::new(recipe.get("readable").map(|value| {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|value| ProjectId(uid(value.as_u64().unwrap())))
            .collect()
    }));
    c.text = recipe["text"].as_str().map(String::from);
    c.ref_project = recipe["ref_project"].as_str().map(str::to_owned);
    c.ref_number = integer(recipe, "ref_number", None);
    c.ref_sequence = integer(recipe, "ref_sequence", None);
    c.projects = strings(recipe, "projects");
    c.kinds = strings(recipe, "kinds");
    c.tracks = strings(recipe, "tracks");
    c.hypothesis_states = strings(recipe, "hypothesis_states");
    c.attempt_states = strings(recipe, "attempt_states");
    c.verdicts = strings(recipe, "verdicts");
    c.decisions = strings(recipe, "decisions");
    c.actors = recipe.get("actors").map(|value| {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|value| ActorId(uid(value.as_u64().unwrap())))
            .collect()
    });
    c.since = time(&recipe["since"]);
    c.until = time(&recipe["until"]);
    c
}
fn hit(value: &Hit) -> Value {
    json!({"id":value.id.0,"kind":value.kind.as_str(),"source_id":value.source_id.0.to_string(),"project":value.project,"track":value.track,"hypothesis_number":value.hypothesis_number,"hypothesis_title":value.hypothesis_title,"attempt_sequence":value.attempt_sequence,"doc_title":value.doc_title,"snippet":value.snippet,"hypothesis_state":value.hypothesis_state.map(repo::HypothesisState::as_str),"attempt_state":value.attempt_state.map(repo::AttemptState::as_str),"verdict":value.verdict,"decision":value.decision.map(repo::Decision::as_str),"actor_user":value.actor_user.map(|value|value.0.to_string()),"actor_service":value.actor_service.map(|value|value.0.to_string()),"occurred_at":value.occurred_at.isoformat(),"origin":value.origin.as_str(),"score":{"float_bits":format!("{:016x}",value.score.to_bits())}})
}
async fn observe(
    conn: &mut PgConnection,
    recipe: &Value,
    fixture: &Value,
) -> Result<Value, SearchError> {
    if let Some(name) = recipe["mutation"].as_str() {
        assert!(
            [
                "comment",
                "track",
                "hypothesis",
                "agent",
                "null_assessment",
                "array_verdict",
                "boolean_verdict",
                "object_verdict",
                "numeric_verdict",
                "scalar_content",
                "comment_delete",
                "no_assessment",
                "decision"
            ]
            .contains(&name)
        );
        sqlx::raw_sql(fixture["mutations"][name].as_str().unwrap())
            .execute(&mut *conn)
            .await
            .map_err(|error| {
                match error
                    .as_database_error()
                    .and_then(sqlx::error::DatabaseError::code)
                {
                    Some(code) => SearchError::Database {
                        sqlstate: Some(code.into_owned()),
                    },
                    None => SearchError::Database { sqlstate: None },
                }
            })?;
    }
    repo::set_fuzziness(conn).await?;
    let c = criteria(recipe);
    if recipe["only_facets"] == true {
        return Ok(json!({"facets":repo::facets(conn,&c).await?}));
    }
    let mut after_id = integer(recipe, "after_id", Some(0));
    let mut score = recipe["after_score"]
        .as_str()
        .map(|value| value.parse::<f64>().unwrap());
    if recipe["warm"] == true {
        let cold = Criteria::new(Some(Vec::new()));
        let integer = BigInt::from(1_u64 << 32);
        for _ in 0..14 {
            repo::search(conn, &cold, Some((0.0, &integer)), Some(&integer)).await?;
        }
    }
    let first = if let Some(page) = recipe["page"].as_i64() {
        let rows = repo::search(conn, &c, None, Some(&BigInt::from(page))).await?;
        if let Some(last) = rows.last() {
            score = Some(last.score);
            after_id = Some(BigInt::from(last.id.0));
        }
        Some(rows.iter().map(|row| row.id.0).collect::<Vec<_>>())
    } else {
        None
    };
    let rows = repo::search(
        conn,
        &c,
        score.map(|score| (score, after_id.as_ref().unwrap())),
        integer(recipe, "limit", Some(200)).as_ref(),
    )
    .await?;
    let facets = repo::facets(conn, &c).await?;
    Ok(json!({"first":first,"hits":rows.iter().map(hit).collect::<Vec<_>>(),"facets":facets}))
}

#[tokio::test]
#[ignore = "requires isolated Rust-migrated PostgreSQL supplied by source launcher"]
async fn actual_python_search_repository_corpus() -> Result<(), Box<dyn Error>> {
    let url = std::env::var("CANNERY_SEARCH_TEST_DATABASE_URL").expect("owned database URI");
    let fixture: Value =
        serde_json::from_str(runtime_reference!("/tests/fixtures/search_reference.json"))?;
    assert_eq!(fixture["python"], "3.13.11");
    assert_eq!(fixture["unicode"], "15.1.0");
    assert_eq!(fixture["count"], 184);
    assert_eq!(
        fixture["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|recipe| outside_native_integer_range(recipe))
            .count(),
        26
    );
    let mut conn = PgConnection::connect(&url)
        .await
        .map_err(|_| "owned database connection failed")?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&mut conn)
        .await?;
    assert!(
        name.len() == 36
            && name.starts_with("conformance_")
            && name[12..]
                .bytes()
                .all(|value| value.is_ascii_hexdigit() && !value.is_ascii_uppercase())
    );
    let empty: bool = sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM projects)")
        .fetch_one(&mut conn)
        .await?;
    assert!(empty);
    sqlx::raw_sql(include_str!("fixtures/seed.sql"))
        .execute(&mut conn)
        .await?;
    drop(conn);
    for recipe in fixture["cases"].as_array().unwrap() {
        // Separate physical connections isolate source preparation recipes.
        let mut conn = PgConnection::connect(&url)
            .await
            .map_err(|_| "owned connection failed")?;
        sqlx::query("SELECT set_config('TimeZone',$1,false)")
            .bind(recipe["timezone"].as_str().unwrap_or("UTC"))
            .execute(&mut conn)
            .await?;
        let mut tx = conn.begin().await?;
        let actual = match observe(&mut tx, recipe, &fixture).await {
            Ok(value) => value,
            Err(SearchError::Database { sqlstate }) => {
                json!({"error":"Database","sqlstate":sqlstate})
            }
            Err(error) => panic!("unexpected sanitized search error: {error}"),
        };
        // Search rejects NUL in its domain filter before querying PostgreSQL.
        assert_eq!(
            actual,
            native_outcome(recipe),
            "search case {}",
            recipe["name"]
        );
        tx.rollback().await?;
    }
    let mut conn = PgConnection::connect(&url)
        .await
        .map_err(|_| "owned connection failed")?;
    sqlx::query("SELECT word_similarity('a','b')")
        .execute(&mut conn)
        .await?;
    let before: String = sqlx::query("SHOW pg_trgm.word_similarity_threshold")
        .fetch_one(&mut conn)
        .await?
        .get(0);
    repo::set_fuzziness(&mut conn).await?;
    let after: String = sqlx::query("SHOW pg_trgm.word_similarity_threshold")
        .fetch_one(&mut conn)
        .await?
        .get(0);
    assert_eq!(json!([[before], [after]]), fixture["autocommit_fuzziness"]);
    Ok(())
}

#[test]
fn native_bigint_argument_boundaries_are_explicit() {
    for key in ["ref_number", "ref_sequence", "limit", "after_id"] {
        for number in [i64::MIN.to_string(), i64::MAX.to_string()] {
            let recipe = json!({key: number, "after_score":"0"});
            assert!(!outside_native_integer_range(&recipe));
        }
        for number in ["-9223372036854775809", "9223372036854775808"] {
            let recipe = json!({key: number, "after_score":"0"});
            assert!(outside_native_integer_range(&recipe));
        }
    }
}

#[test]
fn public_diagnostics_are_redacted() {
    assert_eq!(format!("{:?}", Criteria::new(None)), "Criteria([redacted])");
    assert_eq!(
        SearchError::Database {
            sqlstate: Some("23505".into())
        }
        .to_string(),
        "search database operation failed"
    );
}
