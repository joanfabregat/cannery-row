//! Actual PostgreSQL comparisons against the unchanged Python repository.
#![forbid(unsafe_code)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::{
    db::DatabaseOptions,
    ids::{ProjectId, UserId},
    json::{self, encode_ascii_pretty},
};
use cannery_research::config_repo::{self, ConfigError, ConfigRevision, JsonContext, Kind};
use num_bigint::BigInt;
use serde_json::{Value, json as value};
use sqlx::{Connection, PgConnection};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

// The bounded corpus is shallow. This is not a production decoder/encoder cap.
const CONTEXT: JsonContext = JsonContext {
    encode_nesting_budget: 100,
    decode_nesting_budget: 100,
};

async fn connect() -> Result<PgConnection> {
    let url = std::env::var("CANNERY_CONFIG_TEST_DATABASE_URL")?;
    let options = DatabaseOptions::parse(&url)?;
    if !options
        .connect_options()
        .get_database()
        .is_some_and(|name| {
            name.strip_prefix("conformance_").is_some_and(|suffix| {
                suffix.len() == 24 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        })
    {
        return Err("configuration tests require a disposable database".into());
    }
    Ok(options.connect(None).await?)
}

async fn owner(connection: &mut PgConnection) -> Result<(ProjectId, UserId)> {
    let user: UserId = sqlx::query_scalar(
        "INSERT INTO users (issuer,subject) VALUES ('https://config.invalid',gen_random_uuid()::text) RETURNING id",
    ).fetch_one(&mut *connection).await?;
    let project: ProjectId = sqlx::query_scalar(
        "INSERT INTO projects (slug,title,created_by) VALUES ('config-'||gen_random_uuid()::text,'Config fixture',$1) RETURNING id",
    ).bind(user).fetch_one(connection).await?;
    Ok((project, user))
}

async fn create(
    connection: &mut PgConnection,
    project: ProjectId,
    user: UserId,
    kind: Kind,
    content: &str,
    science: Option<&BigInt>,
) -> Result<std::result::Result<ConfigRevision, ConfigError>> {
    let document = json::decode_str(content, CONTEXT.decode_nesting_budget)?;
    let mut transaction = connection.begin().await?;
    sqlx::query("SELECT id FROM projects WHERE id=$1 FOR UPDATE")
        .bind(project)
        .execute(&mut *transaction)
        .await?;
    let row = config_repo::create_revision(
        &mut transaction,
        project,
        kind,
        &document,
        science,
        user,
        CONTEXT,
    )
    .await;
    if row.is_ok() {
        transaction.commit().await?;
    } else {
        transaction.rollback().await?;
    }
    Ok(row)
}

async fn projection(
    connection: &mut PgConnection,
    row: ConfigRevision,
    project: ProjectId,
    user: UserId,
) -> Result<Value> {
    let stored: (String, String) = sqlx::query_as(
        "SELECT content::text,created_at::text FROM config_revisions WHERE project_id=$1 AND kind=$2 AND revision=$3",
    ).bind(project).bind(&row.kind).bind(row.revision).fetch_one(connection).await?;
    Ok(value!({
        "project_matches":row.project_id==project,"creator_matches":row.created_by==user,
        "kind":row.kind,"revision":row.revision,"science_revision":row.science_revision,
        "content":encode_ascii_pretty(&row.content,CONTEXT.encode_nesting_budget)?,
        "created_at":row.created_at.isoformat(),"storage_json":stored.0,"storage_time":stored.1,
    }))
}
fn integer(value: &Value) -> Result<Option<BigInt>> {
    value
        .as_str()
        .map(str::parse)
        .transpose()
        .map_err(Into::into)
}
async fn seed(connection: &mut PgConnection, recipe: &Value) -> Result<(ProjectId, UserId)> {
    let (project, user) = owner(connection).await?;
    for (kind, content, science) in [
        (Kind::Science, "{\"seed\":1}", None),
        (Kind::Science, "{\"seed\":2}", None),
        (Kind::Science, "{\"seed\":3}", None),
        (Kind::Dashboard, "{\"view\":1}", Some(BigInt::from(1))),
    ] {
        create(connection, project, user, kind, content, science.as_ref()).await??;
    }
    if recipe["insert_max"] == true {
        sqlx::query("INSERT INTO config_revisions (project_id,kind,revision,content,created_by) VALUES ($1,'science',2147483647,'{}',$2)")
            .bind(project).bind(user).execute(connection).await?;
    }
    Ok((project, user))
}
fn write_ids(recipe: &Value, project: ProjectId, user: UserId) -> Result<(ProjectId, UserId)> {
    let project = if recipe["missing_project"] == true {
        "00000000-0000-0000-0000-000000000000".parse()?
    } else {
        project
    };
    let user = if recipe["missing_creator"] == true {
        "00000000-0000-0000-0000-000000000000".parse()?
    } else {
        user
    };
    Ok((project, user))
}

#[tokio::test]
#[ignore = "requires a task-owned PostgreSQL database migrated by the shipped binary"]
#[allow(
    clippy::too_many_lines,
    reason = "Keep ordered repository cases and complete persisted outcomes together"
)]
async fn actual_source_repository_results_storage_and_failed_writes_match() -> Result<()> {
    let fixture: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/research/tests/fixtures/config_repository_reference.json"
    ))?;
    let cases = fixture["cases"]
        .as_array()
        .ok_or("missing reference cases")?;
    assert_eq!(cases.len(), 50);
    let mut connection = connect().await?;
    sqlx::query("SET TIME ZONE 'UTC'")
        .execute(&mut connection)
        .await?;
    sqlx::query("ALTER TABLE config_revisions ALTER COLUMN created_at SET DEFAULT '2026-09-30T12:34:56.123456+00:00'::timestamptz")
        .execute(&mut connection).await?;
    for recipe in cases {
        let (project, user) = seed(&mut connection, recipe).await?;
        let (write_project, write_user) = write_ids(recipe, project, user)?;
        let kind = if recipe["kind"] == "science" {
            Kind::Science
        } else {
            Kind::Dashboard
        };
        let observed: std::result::Result<Value, ConfigError> = match recipe["action"].as_str() {
            Some("create") => {
                let science = integer(&recipe["science"])?;
                match create(
                    &mut connection,
                    write_project,
                    write_user,
                    kind,
                    recipe["content"].as_str().ok_or("missing content")?,
                    science.as_ref(),
                )
                .await?
                {
                    Ok(row) => Ok(projection(&mut connection, row, project, user).await?),
                    Err(error) => Err(error),
                }
            }
            Some("get") => {
                let revision = integer(&recipe["revision"])?;
                match config_repo::get_revision(
                    &mut connection,
                    project,
                    kind,
                    revision.as_ref(),
                    CONTEXT,
                )
                .await
                {
                    Ok(Some(row)) => Ok(projection(&mut connection, row, project, user).await?),
                    Ok(None) => Ok(Value::Null),
                    Err(error) => Err(error),
                }
            }
            Some("list") => {
                let before = integer(&recipe["before"])?;
                let limit = integer(&recipe["limit"])?;
                match config_repo::list_revisions(
                    &mut connection,
                    project,
                    kind,
                    before.as_ref(),
                    limit.as_ref(),
                    CONTEXT,
                )
                .await
                {
                    Ok(rows) => {
                        let mut projected = Vec::new();
                        for row in rows {
                            projected.push(projection(&mut connection, row, project, user).await?);
                        }
                        Ok(Value::Array(projected))
                    }
                    Err(error) => Err(error),
                }
            }
            _ => return Err("unknown source recipe".into()),
        };
        let mut outcome = match observed {
            Ok(value) => value!({"value":value}),
            Err(ConfigError::Database { sqlstate }) => value!({"sqlstate":sqlstate}),
            Err(error) => return Err(error.into()),
        };
        let remaining =
            config_repo::list_revisions(&mut connection, project, kind, None, None, CONTEXT)
                .await?;
        outcome["remaining_revisions"] = value!(
            remaining
                .into_iter()
                .map(|row| row.revision)
                .collect::<Vec<_>>()
        );
        let mut expected = recipe["outcome"].clone();
        // Only the in-memory model projection uses native serde spelling.
        // PostgreSQL JSONB text, timestamps and remaining revisions remain exact.
        match recipe["name"].as_str() {
            Some("ordinary") => {
                assert_eq!(
                    expected["value"]["content"],
                    value!(
                        "{\n  \"tiny\": 1e-07,\n  \"float\": 1.0,\n  \"label\": \"science\",\n  \"negative\": 0.0\n}"
                    )
                );
                expected["value"]["content"] = value!(
                    "{\n  \"tiny\": 1e-7,\n  \"float\": 1.0,\n  \"label\": \"science\",\n  \"negative\": 0.0\n}"
                );
            }
            Some("unicode") => {
                assert_eq!(
                    expected["value"]["content"],
                    value!(
                        "{\n  \"text\": \"\\u00e9\",\n  \"astral\": \"\\ud83d\\ude00\",\n  \"escaped\": \"\\ud83d\\ude00\"\n}"
                    )
                );
                expected["value"]["content"] = value!(
                    "{\n  \"text\": \"é\",\n  \"astral\": \"😀\",\n  \"escaped\": \"😀\"\n}"
                );
            }
            Some("max-revision-valid-json") => {
                assert_eq!(expected["sqlstate"], "22003");
                assert_eq!(
                    expected["remaining_revisions"],
                    value!([2_147_483_647, 3, 2, 1])
                );
            }
            _ => {}
        }
        assert_eq!(outcome, expected, "{}", recipe["name"]);
    }
    Ok(())
}
