//! Supported application helper cases over guarded migrated PostgreSQL.
#![forbid(unsafe_code)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::{
    db::DatabaseOptions,
    ids::ProjectId,
    json::{self, Document, Node, NodeId},
};
use cannery_research::science::{RenderingContext, Science};
use cannery_server::step_binding::{
    self, BindingContext, CheckedOutputEquality, ManifestQuery, PgManifestQuery, ResolvedStep,
};
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection};
use std::{error::Error, str::FromStr};
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
const PROJECT: &str = "00000000-0000-0000-0000-000000000010";
struct Jsonb(String);
impl sqlx::Type<sqlx::Postgres> for Jsonb {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_name("jsonb")
    }
}
impl sqlx::Encode<'_, sqlx::Postgres> for Jsonb {
    fn encode_by_ref(
        &self,
        b: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> std::result::Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        b.push(1);
        b.extend_from_slice(self.0.as_bytes());
        Ok(sqlx::encode::IsNull::No)
    }
}
fn field(d: &Document, id: NodeId, key: &str) -> Result<NodeId> {
    d.field(id, key).ok_or_else(|| "fixture field".into())
}
fn document(d: &Document, id: NodeId) -> Result<Document> {
    Ok(json::decode_str(
        &json::encode_ascii_pretty_node(d, id, 120)?,
        120,
    )?)
}
fn value(d: &Document, id: NodeId) -> Result<Value> {
    Ok(serde_json::from_str(&json::encode_ascii_pretty_node(
        d, id, 120,
    )?)?)
}
fn selected(d: &Document, key: &str) -> bool {
    !matches!(
        d.field(d.root(), key).and_then(|id| d.node(id)),
        Some(Node::Bool(false))
    )
}
fn optional(d: &Document, key: &str) -> Result<Option<Document>> {
    let id = field(d, d.root(), key)?;
    if matches!(d.node(id), Some(Node::Null)) {
        Ok(None)
    } else {
        Ok(Some(document(d, id)?))
    }
}
fn resolved(s: &ResolvedStep) -> Result<Value> {
    let reference = s.reference()?;
    Ok(
        json!({"ref":value(&reference,reference.root())?,"manifest":value(&s.manifest,s.manifest.root())?}),
    )
}
#[allow(
    clippy::too_many_lines,
    reason = "Source fixture setup, construction and caller order remain explicit"
)]
async fn run(
    conn: &mut PgConnection,
    queries: &mut dyn ManifestQuery,
    d: &Document,
    c: &BindingContext<'_>,
) -> std::result::Result<Value, step_binding::Error> {
    let store = |error: Box<dyn Error + Send + Sync>| {
        drop(error);
        step_binding::Error::Store
    };
    for (kind, key, name) in [
        ("producer", "stored_producer", "sparse-producer"),
        ("experiment", "experiment", "experiment"),
        ("experiment", "experiment2", "finish"),
    ] {
        if let Some(id) = d.field(d.root(), key) {
            let mut raw = json::encode_ascii_pretty_node(d, id, 120)
                .map_err(|_| step_binding::Error::Store)?;
            let mut time = "now".to_owned();
            if kind == "producer" {
                for (key, target) in [("stored_raw", &mut raw), ("stored_time", &mut time)] {
                    if let Some(id) = d.field(d.root(), key) {
                        let Some(Node::String(text)) = d.node(id) else {
                            return Err(step_binding::Error::Store);
                        };
                        *target = text.as_utf8().ok_or(step_binding::Error::Store)?;
                    }
                }
            }
            let query = if kind == "producer" {
                "INSERT INTO producer_manifests(project_id,name,revision,content,created_by,created_at) VALUES($1::text::uuid,$2,1,$3,'00000000-0000-0000-0000-000000000001',$4::timestamptz)"
            } else {
                "INSERT INTO experiment_manifests(project_id,name,revision,content,created_by,created_at) VALUES($1::text::uuid,$2,1,$3,'00000000-0000-0000-0000-000000000001',$4::timestamptz)"
            };
            sqlx::query(query)
                .bind(PROJECT)
                .bind(name)
                .bind(Jsonb(raw))
                .bind(time)
                .execute(&mut *conn)
                .await
                .map_err(|_| step_binding::Error::Store)?;
        }
    }
    let get = |key| {
        field(d, d.root(), key)
            .and_then(|id| document(d, id))
            .map_err(store)
    };
    let operation = value(d, field(d, d.root(), "operation").map_err(store)?).map_err(store)?;
    let science_doc = get("science")?;
    let science = if operation == "track"
        && matches!(science_doc.node(science_doc.root()), Some(Node::Null))
    {
        None
    } else {
        Some(Science::new(1.into(), &science_doc, c.rendering)?)
    };
    let binding = optional(d, "binding").map_err(store)?;
    let workflow = get("workflow")?;
    let producer = get("producer")?;
    let project = ProjectId::from_str(PROJECT).map_err(|_| step_binding::Error::Store)?;
    let path = value(d, field(d, d.root(), "path").map_err(store)?).map_err(store)?;
    let path = String::from(path.as_str().ok_or(step_binding::Error::Store)?);
    if operation == "track" {
        if binding.is_some() {
            let view = science.as_ref().ok_or_else(|| {
                cannery_research::science::ScienceError::Validation {
                    path: String::from("/producer"),
                    message: "missing science",
                }
            })?;
            step_binding::resolve_producer(
                queries,
                project,
                view,
                binding.as_ref(),
                &String::from("/producer"),
                c,
            )
            .await?;
        }
        if !matches!(workflow.node(workflow.root()), Some(Node::Null)) {
            let view = science.as_ref().ok_or_else(|| {
                cannery_research::science::ScienceError::Validation {
                    path: String::from("/workflow"),
                    message: "missing science",
                }
            })?;
            let bound = step_binding::resolve_producer(
                queries,
                project,
                view,
                binding.as_ref(),
                &String::from("/producer"),
                c,
            )
            .await?;
            step_binding::resolve_workflow(
                queries,
                project,
                view,
                &workflow,
                &String::from("/workflow"),
                Some(&bound.manifest),
                c,
            )
            .await?;
        }
        return Ok(Value::Null);
    }
    let view = science
        .as_ref()
        .ok_or(cannery_research::science::ScienceError::Attribute)?;
    if operation == "producer" {
        return resolved(
            &step_binding::resolve_producer(queries, project, view, binding.as_ref(), &path, c)
                .await?,
        )
        .map_err(store);
    }
    let bound = if selected(d, "use_producer") && selected(d, "resolve_producer") {
        Some(
            step_binding::resolve_producer(
                queries,
                project,
                view,
                binding.as_ref(),
                &String::from("/producer"),
                c,
            )
            .await?,
        )
    } else {
        None
    };
    let producer = if !selected(d, "use_producer") {
        None
    } else if let Some(bound) = &bound {
        Some(&*bound.manifest)
    } else if matches!(producer.node(producer.root()), Some(Node::Null)) {
        None
    } else {
        Some(&producer)
    };
    let steps =
        step_binding::resolve_workflow(queries, project, view, &workflow, &path, producer, c)
            .await?;
    Ok(Value::Array(
        steps
            .iter()
            .map(resolved)
            .collect::<Result<Vec<_>>>()
            .map_err(store)?,
    ))
}
#[tokio::test]
#[ignore = "requires guarded migrated isolated PostgreSQL"]
async fn step_bindings_match_source() -> Result<()> {
    let fixture = json::decode(
        runtime_reference!("/tests/fixtures/step_binding/reference.json").as_bytes(),
        120,
    )?;
    let options = DatabaseOptions::parse(&std::env::var("CANNERY_STEP_BINDING_DATABASE_URL")?)?;
    let mut conn = PgConnection::connect_with(options.connect_options()).await?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&mut conn)
        .await?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("guarded database required")?;
    if suffix.len() != 24 || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("guarded database required".into());
    }
    sqlx::raw_sql(include_str!("fixtures/tracks_http/seed.sql"))
        .execute(&mut conn)
        .await?;
    let mut lookup = PgConnection::connect_with(options.connect_options()).await?;
    assert_native_nul_rollback(&mut lookup).await?;
    let mut queries = PgManifestQuery(&mut lookup);
    let equality = CheckedOutputEquality;
    let context = BindingContext {
        rendering: RenderingContext {
            nesting_budget: 100,
        },
        decode_budget: 120,
        equality: &equality,
    };
    let Some(Node::Array(cases)) = fixture.node(field(&fixture, fixture.root(), "cases")?) else {
        return Err("fixture cases".into());
    };
    assert_eq!(cases.len(), 160);
    for required in [
        "workflow-valid",
        "producer-default",
        "producer-explicit",
        "name-lookup-and-storage-refusal",
        "repeated-revision-lookups",
        "duplicate-output",
        "nonfinal-sheet",
        "required-roles",
        "workflow-input",
        "refs-win-producer-use",
        "earlier-output-raw-interface",
        "track-producer-first",
    ] {
        assert!(
            cases.iter().any(|id| {
                fixture
                    .field(*id, "name")
                    .and_then(|id| fixture.node(id))
                    .is_some_and(|node| matches!(node, Node::String(name) if name == required))
            }),
            "missing application case {required}"
        );
    }
    for (index, id) in cases.iter().enumerate() {
        sqlx::raw_sql("TRUNCATE producer_manifests,experiment_manifests")
            .execute(&mut conn)
            .await?;
        let recipe = document(&fixture, *id)?;
        let expected = value(&recipe, field(&recipe, recipe.root(), "outcome")?)?;
        let actual = match run(&mut conn, &mut queries, &recipe, &context).await {
            Ok(value) => json!({"category":"ok","value":value}),
            Err(step_binding::Error::Science(
                cannery_research::science::ScienceError::Validation { path, .. },
            )) => {
                json!({"category":"Validation","paths":[path.as_utf8().ok_or("fixture path encoding")?]})
            }
            Err(step_binding::Error::Science(error)) => json!({"category":error.class()}),
            Err(step_binding::Error::Store) => json!({"category":"Store"}),
            Err(step_binding::Error::Validation(value)) => {
                json!({"category":"Validation","paths":[value.path.as_utf8().ok_or("fixture path encoding")?]})
            }
        };
        let mut expected = expected;
        expected
            .as_object_mut()
            .ok_or("outcome object")?
            .remove("source_class");
        let nul_name = recipe
            .field(recipe.root(), "binding")
            .and_then(|id| recipe.field(id, "name"))
            .and_then(|id| recipe.node(id))
            .is_some_and(|node| matches!(node, Node::String(name) if name == "\0"));
        if nul_name {
            // Both adapters refuse NUL; PostgreSQL reports native text rejection as 22021.
            assert!(matches!(
                expected["category"].as_str(),
                Some("Store" | "ValueError")
            ));
            assert_eq!(actual, json!({"category":"Store"}));
            expected = json!({"category":"Store"});
        }
        assert_eq!(actual, expected, "recipe {index}");
    }
    println!("{} actual step binding outcomes matched", cases.len());
    lookup.close().await?;
    Ok(())
}

async fn assert_native_nul_rollback(connection: &mut PgConnection) -> Result<()> {
    let mut transaction = connection.begin().await?;
    let error = sqlx::query_scalar::<_, String>("SELECT $1::text")
        .bind("\0")
        .fetch_one(&mut *transaction)
        .await
        .err()
        .ok_or("PostgreSQL unexpectedly accepted NUL text")?;
    assert_eq!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("22021")
    );
    transaction.rollback().await?;
    let ready: i32 = sqlx::query_scalar("SELECT 1").fetch_one(connection).await?;
    assert_eq!(ready, 1);
    Ok(())
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
