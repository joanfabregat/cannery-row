//! Actual claim workflow projection against isolated unchanged production reads.
use cannery_attempts::{
    model::JsonContext,
    repo::{AttemptError, Repository},
};
use cannery_core::{
    ids::AttemptId,
    json::{self, Document, Node},
};
use cannery_research::science::{RenderingContext, Science, ScienceError};
use cannery_server::attempt_workflow::{self, Error, WorkflowStep};
use num_bigint::BigInt;
use serde_json::{Value, json};
use sqlx::{Connection, postgres::PgConnectOptions};
use std::collections::BTreeMap;
use std::str::FromStr;
use uuid::Uuid;
type TestError = Box<dyn std::error::Error + Send + Sync>;
fn document(value: &Value, field: &str) -> Result<Document, TestError> {
    Ok(json::decode_str(
        value[field].as_str().ok_or("fixture text")?,
        200,
    )?)
}
fn class(error: &Error) -> &'static str {
    match error {
        Error::Science(error) => error.class(),
        // The repository preserves the source's missing approved-revision assertion.
        Error::Invariant | Error::Database(AttemptError::Invariant) => "AssertionError",
        Error::Database(_) => "DatabaseError",
        Error::Build => "BuildError",
    }
}
async fn raw_storage(connection: &mut sqlx::PgConnection) -> Result<Vec<String>, TestError> {
    let mut rows = vec![];
    for table in [
        "users",
        "projects",
        "tracks",
        "hypotheses",
        "hypothesis_revisions",
        "attempts",
        "jobs",
        "artifacts",
        "attempt_failures",
        "phase_outputs",
        "manifests",
        "review_cases",
        "decisions",
        "audit_events",
        "idempotency_keys",
        "search_documents",
    ] {
        rows.push(sqlx::query_scalar::<_,String>(&format!("SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]')::text FROM {table} t")).fetch_one(&mut *connection).await?);
    }
    Ok(rows)
}
#[tokio::test]
#[ignore = "requires positively selected guarded source/native PostgreSQL child fixtures"]
#[allow(
    clippy::too_many_lines,
    reason = "Keep complete independent fixture observations together"
)]
async fn actual_attempt_workflow_parity() -> Result<(), TestError> {
    let uri = std::env::var("CANNERY_WORKFLOW_DATABASE_URL")?;
    let options = PgConnectOptions::from_str(&uri)?;
    let name = options.get_database().ok_or("owned database required")?;
    let nonce = name
        .strip_prefix("preparation_owner_")
        .ok_or("guarded child required")?;
    if nonce.len() != 24
        || !nonce
            .bytes()
            .all(|value| value.is_ascii_digit() || (b'a'..=b'f').contains(&value))
    {
        return Err("guarded child required".into());
    }
    let source: Value = serde_json::from_str(&std::fs::read_to_string(std::env::var(
        "CANNERY_WORKFLOW_REFERENCE",
    )?)?)?;
    let rendering = RenderingContext {
        nesting_budget: 200,
    };
    let context = JsonContext {
        encode_nesting_budget: 200,
        decode_nesting_budget: 200,
    };
    let mut conn = sqlx::PgConnection::connect_with(&options).await?;
    let observations = source["observations"].as_array().ok_or("observations")?;
    assert_eq!(observations.len(), 214);
    let profiles = observations
        .iter()
        .filter_map(|o| o["recipe"]["native_profile"].as_str())
        .fold(BTreeMap::new(), |mut counts, name| {
            *counts.entry(name).or_insert(0_usize) += 1;
            counts
        });
    assert_eq!(
        profiles,
        BTreeMap::from([
            ("limit-type", 15),
            ("nonfinite-json", 10),
            ("limit-overflow", 6),
            ("missing-deadline", 39)
        ])
    );
    for observation in source["observations"].as_array().ok_or("observations")? {
        let recipe = &observation["recipe"];
        let mut transaction = conn.begin().await?;
        sqlx::raw_sql(include_str!("fixtures/attempt_workflow/seed.sql"))
            .execute(&mut *transaction)
            .await?;
        if recipe["parameters_missing"] == true {
            sqlx::query(
                "UPDATE hypotheses SET state='draft', approved_revision=NULL, approved_at=NULL",
            )
            .execute(&mut *transaction)
            .await?;
        } else if let Some(value) = recipe["parameters_json"].as_str() {
            sqlx::query("INSERT INTO hypothesis_revisions(hypothesis_id,revision,content,science_revision,author_user,via_channel,created_at) VALUES('00000000-0000-0000-0000-000000000011',3,$1::jsonb,1,'00000000-0000-0000-0000-000000000001','api','2001-01-01Z')")
                .bind(format!("{{\"project_fields\":{value}}}"))
                .execute(&mut *transaction)
                .await?;
            sqlx::query("UPDATE hypotheses SET revision=3, approved_revision=3")
                .execute(&mut *transaction)
                .await?;
        }
        let before = raw_storage(&mut transaction).await?;
        let mut repository = Repository::new(&mut transaction, context);
        let mut attempt = repository
            .get_attempt_by_id(
                AttemptId(Uuid::parse_str("00000000-0000-0000-0000-000000000021")?),
                false,
            )
            .await?
            .ok_or("seeded attempt required")?;
        attempt.predecessor_id = recipe["predecessor"]
            .as_u64()
            .map(|id| Uuid::parse_str(&format!("00000000-0000-0000-0000-{id:012}")))
            .transpose()?
            .map(AttemptId);
        if recipe["deadline"] == false {
            attempt.deadline = None;
        }
        if recipe["native_profile"] == "nonfinite-json" {
            assert!(recipe["id"].as_str().ok_or("id")?.starts_with("limit-"));
            let text = recipe["science_json"].as_str().ok_or("science")?;
            assert!(
                text.contains("\"max_output_bytes\": NaN")
                    || text.contains("\"max_output_bytes\": Infinity")
            );
            assert!(
                matches!(
                    json::decode_str(text, 128),
                    Err(json::DecodeError::Syntax { .. })
                ),
                "explicit nonfinite JSON refusal before workflow execution"
            );
            assert_eq!(
                raw_storage(&mut transaction).await?,
                before,
                "nonfinite refusal leaves all workflow storage unchanged"
            );
            transaction.rollback().await?;
            continue;
        }
        let science_document = document(recipe, "science_json")?;
        let control = document(recipe, "control_json")?;
        let raw_steps: Value =
            serde_json::from_str(recipe["steps_json"].as_str().ok_or("steps text")?)?;
        let parsed = raw_steps
            .as_array()
            .ok_or("steps array")?
            .iter()
            .map(|step| {
                Ok::<_, TestError>((
                    String::from(step["name"].as_str().ok_or("step name")?),
                    BigInt::from(step["revision"].as_i64().ok_or("step revision")?),
                    json::decode_str(&step["manifest"].to_string(), 200)?,
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let steps = parsed
            .iter()
            .map(|(name, revision, manifest)| WorkflowStep {
                name,
                revision,
                manifest,
            })
            .collect::<Vec<_>>();
        let control = if matches!(control.node(control.root()), Some(Node::Null)) {
            None
        } else {
            Some(&control)
        };
        let result = match Science::new(BigInt::from(3), &science_document, rendering) {
            Ok(science) => {
                attempt_workflow::run_spec(
                    &mut repository,
                    &attempt,
                    &science,
                    &steps,
                    control,
                    rendering,
                )
                .await
            }
            Err(error) => Err(error.into()),
        };
        if let Some(profile) = recipe["native_profile"].as_str() {
            let Err(error) = result else {
                return Err("declared native refusal returned a spec".into());
            };
            match profile {
                "missing-deadline" => {
                    assert_eq!(recipe["deadline"], false);
                    assert!(matches!(error, Error::Invariant));
                }
                "limit-type" | "limit-overflow" => {
                    let limits = science_document
                        .field(science_document.root(), "limits")
                        .ok_or("limits")?;
                    let limit = science_document
                        .field(limits, "max_output_bytes")
                        .ok_or("max output")?;
                    if profile == "limit-type" {
                        let value = json::to_value(&science_document)?;
                        assert!(
                            [
                                json!(false),
                                json!(1.5),
                                json!(" ١٢\u{2003}"),
                                json!("1_000"),
                                json!("bad")
                            ]
                            .contains(&value["limits"]["max_output_bytes"])
                        );
                        assert!(matches!(
                            science_document.node(limit),
                            Some(Node::Bool(_) | Node::Float(_) | Node::String(_))
                        ));
                        assert!(matches!(error, Error::Science(ScienceError::Type)));
                    } else {
                        let Some(Node::Integer(value)) = science_document.node(limit) else {
                            return Err("overflow must be JSON integer".into());
                        };
                        assert!(matches!(
                            value.to_string().as_str(),
                            "9223372036854775808" | "-9223372036854775809"
                        ));
                        assert!(matches!(error, Error::Science(ScienceError::Overflow)));
                    }
                }
                _ => return Err("unknown native workflow profile".into()),
            }
        } else {
            let actual = match result {
                Ok(document) => {
                    let value = json::to_value(&document)?;
                    assert_eq!(
                        json::encode_ascii_default(&document, 128)?,
                        serde_json::to_string(&value)?,
                        "native standard serde serialization"
                    );
                    json!({"value":value})
                }
                Err(error) => json!({"exception":class(&error)}),
            };
            let expected = if let Some(text) = observation["outcome"]["json"].as_str() {
                json!({"value":serde_json::from_str::<Value>(text)?})
            } else {
                observation["outcome"].clone()
            };
            assert_eq!(
                actual, expected,
                "complete workflow semantics {}",
                recipe["id"]
            );
        }
        assert_eq!(
            raw_storage(&mut transaction).await?,
            before,
            "workflow construction must not change stored application state"
        );
        transaction.rollback().await?;
    }
    conn.close().await?;
    Ok(())
}
