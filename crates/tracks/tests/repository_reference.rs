//! Actual PostgreSQL comparisons against unchanged Python tracks.repo.
#![forbid(unsafe_code)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::{
    db::DatabaseOptions,
    ids::{ProjectId, TrackId, UserId},
    json::{self, Document},
};
use cannery_tracks::repo::{
    self, CreateTrack, JsonContext, RowLock, Track, TrackError, TrackMode, TrackState, UpdateTrack,
};
use num_bigint::BigInt;
use serde_json::{Value, json as value};
use sqlx::{Connection, PgConnection};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const CONTEXT: JsonContext = JsonContext {
    encode_nesting_budget: 100,
    decode_nesting_budget: 100,
};
fn fixture() -> Result<Value> {
    Ok(serde_json::from_str(runtime_reference!(
        "/../../crates/tracks/tests/fixtures/tracks_repository_reference.json"
    ))?)
}
async fn connect() -> Result<PgConnection> {
    let url = std::env::var("CANNERY_TRACKS_TEST_DATABASE_URL")?;
    let options = DatabaseOptions::parse(&url)?;
    if !options
        .connect_options()
        .get_database()
        .is_some_and(|name| {
            name.strip_prefix("conformance_").is_some_and(|suffix| {
                suffix.len() == 24
                    && suffix
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        })
    {
        return Err("tracks tests require guarded disposable database".into());
    }
    Ok(options.connect(None).await?)
}
async fn owner(c: &mut PgConnection) -> Result<(ProjectId, UserId, ProjectId)> {
    let user:UserId=sqlx::query_scalar("INSERT INTO users(issuer,subject) VALUES('https://tracks.invalid',gen_random_uuid()::text) RETURNING id").fetch_one(&mut *c).await?;
    let mut ids = Vec::new();
    for _ in 0..2 {
        let id:ProjectId=sqlx::query_scalar("INSERT INTO projects(slug,title,created_by) VALUES('track-'||replace(gen_random_uuid()::text,'-',''),'Tracks fixture',$1) RETURNING id").bind(user).fetch_one(&mut *c).await?;
        ids.push(id);
    }
    Ok((ids[0], user, ids[1]))
}
async fn seed(c: &mut PgConnection, p: ProjectId, u: UserId, slug: &str) -> Result<Track> {
    let slug = String::from(slug);
    let title = String::from("Seed");
    let description = String::new();
    repo::create_track(
        c,
        CreateTrack {
            project_id: p,
            slug: &slug,
            title: &title,
            description: &description,
            producer: None,
            mode: TrackMode::Agent,
            workflow: None,
            created_by: u,
        },
        CONTEXT,
    )
    .await?
    .ok_or_else(|| "fixture duplicate".into())
}
fn document(recipe: &Value, name: &str) -> Result<Option<Document>> {
    recipe[name]
        .as_str()
        .map(|s| json::decode_str(s, 100))
        .transpose()
        .map_err(Into::into)
}
fn integer(recipe: &Value, name: &str) -> Result<Option<BigInt>> {
    if let Some(power) = recipe[format!("{name}_power")].as_u64() {
        return Ok(Some(BigInt::from(10_u8).pow(u32::try_from(power)?)));
    }
    recipe[name]
        .as_str()
        .map(str::parse)
        .transpose()
        .map_err(Into::into)
}
fn text(recipe: &Value, name: &str, default: &str) -> String {
    String::from(recipe[name].as_str().unwrap_or(default))
}
fn json_projection(document: Option<&Document>) -> Result<String> {
    Ok(document.map_or_else(
        || Ok("null".to_owned()),
        |d| json::encode_ascii_pretty(d, 100),
    )?)
}
async fn projection(c: &mut PgConnection, row: &Track, p: ProjectId, u: UserId) -> Result<Value> {
    let stored:(Option<String>,Option<String>,bool,bool,bool)=sqlx::query_as("SELECT producer::text,workflow::text,created_at=now(),updated_at=now(),created_at<=updated_at FROM tracks WHERE id=$1").bind(row.id).fetch_one(c).await?;
    Ok(
        value!({"project_matches":row.project_id==p,"creator_matches":row.created_by==u,"slug":row.slug.as_utf8(),"title":row.title.as_utf8(),"description":row.description.as_utf8(),"producer":json_projection(row.producer.as_ref())?,"workflow":json_projection(row.workflow.as_ref())?,"storage_producer":stored.0,"storage_workflow":stored.1,"created_in_transaction":stored.2,"updated_in_transaction":stored.3,"time_ordered":stored.4,"utc":row.created_at.isoformat().ends_with("+00:00")&&row.updated_at.isoformat().ends_with("+00:00"),"state":row.state.as_str(),"mode":row.mode.as_str(),"revision":row.revision}),
    )
}
fn error_projection(error: &TrackError) -> Value {
    match error {
        TrackError::TextEncoding => value!({"error":"UnicodeEncodeError"}),
        _ => value!({"sqlstate":error.sqlstate()}),
    }
}

// Keep the source recipe dispatcher together for direct per-operation comparisons.
#[allow(clippy::too_many_lines)]
async fn operation(
    c: &mut PgConnection,
    recipe: &Value,
    p: ProjectId,
    u: UserId,
    foreign: ProjectId,
    first: TrackId,
) -> Result<std::result::Result<Value, TrackError>> {
    let outcome = match recipe["action"].as_str().ok_or("missing action")? {
        "create" => {
            let producer = document(recipe, "producer")?;
            let workflow = document(recipe, "workflow")?;
            let slug = text(recipe, "slug", "new");
            let title = text(recipe, "title", "Created");
            let description = text(recipe, "description", "");
            let mode = TrackMode::try_from(recipe["mode"].as_str().unwrap_or("agent"))?;
            match repo::create_track(
                c,
                CreateTrack {
                    project_id: if recipe["missing_project"] == true {
                        ProjectId("00000000-0000-0000-0000-000000000000".parse()?)
                    } else {
                        p
                    },
                    slug: &slug,
                    title: &title,
                    description: &description,
                    producer: producer.as_ref(),
                    mode,
                    workflow: workflow.as_ref(),
                    created_by: if recipe["missing_user"] == true {
                        UserId("00000000-0000-0000-0000-000000000000".parse()?)
                    } else {
                        u
                    },
                },
                CONTEXT,
            )
            .await
            {
                Ok(Some(row)) => Ok(projection(c, &row, p, u).await?),
                Ok(None) => Ok(Value::Null),
                Err(e) => Err(e),
            }
        }
        "update" => {
            let producer = document(recipe, "producer")?;
            let workflow = document(recipe, "workflow")?;
            let revision = integer(recipe, "revision")?.ok_or("missing revision")?;
            let title = text(recipe, "title", "Changed");
            let description = String::new();
            let state = TrackState::try_from(recipe["state"].as_str().unwrap_or("active"))?;
            let mode = TrackMode::try_from(recipe["mode"].as_str().unwrap_or("agent"))?;
            match repo::update_track(
                c,
                if recipe["missing_track"] == true {
                    TrackId("00000000-0000-0000-0000-000000000000".parse()?)
                } else {
                    first
                },
                UpdateTrack {
                    expected_revision: &revision,
                    title: &title,
                    description: &description,
                    producer: producer.as_ref(),
                    state,
                    mode,
                    workflow: workflow.as_ref(),
                },
                CONTEXT,
            )
            .await
            {
                Ok(Some(row)) => Ok(projection(c, &row, p, u).await?),
                Ok(None) => Ok(Value::Null),
                Err(e) => Err(e),
            }
        }
        "list" => {
            let state = recipe["state"].as_str().map(String::from);
            let after = recipe["after"].as_str().map(String::from);
            let limit = integer(recipe, "limit")?;
            match repo::list_tracks(
                c,
                p,
                state.as_ref(),
                after.as_ref(),
                limit.as_ref(),
                CONTEXT,
            )
            .await
            {
                Ok(rows) => {
                    let mut projections = Vec::new();
                    for row in rows {
                        projections.push(projection(c, &row, p, u).await?);
                    }
                    Ok(value!(projections))
                }
                Err(e) => Err(e),
            }
        }
        "get" | "id" | "stored-null" => {
            if recipe["action"] == "stored-null" {
                sqlx::query("UPDATE tracks SET producer='null'::jsonb WHERE id=$1")
                    .bind(first)
                    .execute(&mut *c)
                    .await?;
            }
            let row = if recipe["action"] == "get" {
                repo::get_track(
                    c,
                    if recipe["foreign"] == true {
                        foreign
                    } else {
                        p
                    },
                    &String::from("a"),
                    None,
                    CONTEXT,
                )
                .await
            } else {
                repo::get_track_by_id(
                    c,
                    if recipe["missing_track"] == true {
                        TrackId("00000000-0000-0000-0000-000000000000".parse()?)
                    } else {
                        first
                    },
                    CONTEXT,
                )
                .await
            };
            match row {
                Ok(Some(row)) => Ok(projection(c, &row, p, u).await?),
                Ok(None) => Ok(Value::Null),
                Err(e) => Err(e),
            }
        }
        "count" => {
            for (index, state) in [
                "draft",
                "queued",
                "active",
                "awaiting_human_review",
                "promoted",
                "rejected",
                "inconclusive",
                "declined",
                "failed",
                "cancelled",
            ]
            .into_iter()
            .enumerate()
            {
                sqlx::query("INSERT INTO hypotheses(project_id,number,track_id,state,title,created_by_user,approved_revision,approved_at) VALUES($1,$2,$3,$4,'fixture',$5,1,now())").bind(p).bind(i32::try_from(index+1)?).bind(first).bind(state).bind(u).execute(&mut *c).await?;
            }
            repo::count_open_hypotheses(c, first)
                .await
                .map(|count| value!(count))
        }
        _ => return Err("unknown action".into()),
    };
    Ok(outcome)
}

#[tokio::test]
#[ignore = "requires a disposable fully migrated PostgreSQL database"]
async fn actual_source_repository_comparisons() -> Result<()> {
    let reference = fixture()?;
    let mut c = connect().await?;
    for recipe in reference["cases"].as_array().ok_or("missing corpus")? {
        if recipe["supplemental"] == true {
            let rejected = if recipe["action"] == "create" {
                TrackMode::try_from(recipe["mode"].as_str().ok_or("missing mode")?).is_err()
            } else {
                TrackState::try_from(recipe["state"].as_str().ok_or("missing state")?).is_err()
            };
            assert!(rejected);
            assert_eq!(recipe["outcome"]["sqlstate"], "23514");
            continue;
        }
        let mut transaction = c.begin().await?;
        let (p, u, foreign) = owner(&mut transaction).await?;
        let mut first = None;
        for slug in ["a", "a-1", "b"] {
            let row = seed(&mut transaction, p, u, slug).await?;
            if first.is_none() {
                first = Some(row.id);
            }
        }
        let first = first.ok_or("missing fixture track")?;
        let foreign_row = seed(&mut transaction, foreign, u, "a").await?;
        sqlx::query("UPDATE tracks SET title='Foreign' WHERE id=$1")
            .bind(foreign_row.id)
            .execute(&mut *transaction)
            .await?;
        if recipe["insert_max"] == true {
            sqlx::query("UPDATE tracks SET revision=2147483647 WHERE id=$1")
                .bind(first)
                .execute(&mut *transaction)
                .await?;
        }
        let mut savepoint = transaction.begin().await?;
        let outcome = operation(&mut savepoint, recipe, p, u, foreign, first).await?;
        let mut actual = match outcome {
            Ok(v) => {
                savepoint.commit().await?;
                value!({"value":v})
            }
            Err(e) => {
                savepoint.rollback().await?;
                error_projection(&e)
            }
        };
        let remaining = repo::list_tracks(&mut transaction, p, None, None, None, CONTEXT).await?;
        actual["remaining"] = value!(
            remaining
                .iter()
                .map(|row| value!([
                    row.slug.as_utf8(),
                    row.revision,
                    row.state.as_str(),
                    row.mode.as_str()
                ]))
                .collect::<Vec<_>>()
        );
        let mut expected = recipe["outcome"].clone();
        if recipe["name"] == "create-numbers" {
            // This model projection uses serde's exponent spelling. Actual
            // PostgreSQL JSONB text and all stored rows still compare exactly.
            let source = expected["value"]["producer"]
                .as_str()
                .ok_or("source numeric projection missing")?;
            assert!(source.contains("\"tiny\": 1e-07"));
            expected["value"]["producer"] =
                value!(source.replace("\"tiny\": 1e-07", "\"tiny\": 1e-7"));
        }
        if recipe["name"] == "create-unicode" {
            assert_eq!(
                expected["value"]["producer"],
                value!("{\n  \"text\": \"\\u00e9\\ud83d\\ude00\"\n}")
            );
            expected["value"]["producer"] = value!("{\n  \"text\": \"é😀\"\n}");
        }
        if recipe["name"] == "update-producer-fields" {
            // Only the model's pretty JSON spelling changes. JSONB text,
            // revisions, remaining rows and savepoint outcomes stay exact.
            assert_eq!(
                expected["value"]["producer"],
                value!("{\n  \"float\": 1.0,\n  \"label\": \"\\u00e9\\ud83d\\ude00\"\n}")
            );
            expected["value"]["producer"] = value!("{\n  \"float\": 1.0,\n  \"label\": \"é😀\"\n}");
        }
        assert_eq!(actual, expected, "{}", recipe["name"]);
        transaction.rollback().await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires a disposable fully migrated PostgreSQL database"]
async fn source_locks_transactions_and_compare_replace() -> Result<()> {
    let reference = fixture()?;
    let mut a = connect().await?;
    let mut b = connect().await?;
    let (p, u, _) = owner(&mut a).await?;
    let row = seed(&mut a, p, u, "locked").await?;
    let slug = String::from("locked");
    let mut matrix = Vec::new();
    for (held, requested, expected) in [
        (RowLock::Share, RowLock::Share, None),
        (RowLock::Share, RowLock::Update, Some("55P03")),
        (RowLock::Update, RowLock::Share, Some("55P03")),
        (RowLock::Update, RowLock::Update, Some("55P03")),
    ] {
        let mut one = a.begin().await?;
        repo::get_track(&mut one, p, &slug, Some(held), CONTEXT)
            .await?
            .ok_or("track missing")?;
        let mut two = b.begin().await?;
        sqlx::query("SET LOCAL lock_timeout='100ms'")
            .execute(&mut *two)
            .await?;
        let result = repo::get_track(&mut two, p, &slug, Some(requested), CONTEXT).await;
        if let Some(code) = expected {
            let error = result.err().ok_or("lock unexpectedly acquired")?;
            assert_eq!(error.sqlstate(), Some(code));
            assert!(std::error::Error::source(&error).is_none());
            matrix.push(value!({"held":lock_name(held),"requested":lock_name(requested),"sqlstate":error.sqlstate()}));
        } else {
            result?.ok_or("shared track missing")?;
            matrix.push(
                value!({"held":lock_name(held),"requested":lock_name(requested),"acquired":true}),
            );
        }
        two.rollback().await?;
        one.rollback().await?;
    }
    assert_eq!(value!(matrix), reference["locking"]["matrix"]);
    // A rolled-back update is invisible; committed unchanged update still increments.
    let revision = BigInt::from(1);
    let mut tx = a.begin().await?;
    let updated = unchanged_update(&mut tx, row.id, &revision)
        .await?
        .ok_or("update missing")?;
    assert_eq!(
        value!(updated.revision),
        reference["locking"]["rollback_update_revision"]
    );
    tx.rollback().await?;
    assert_eq!(
        i64::from(
            repo::get_track_by_id(&mut b, row.id, CONTEXT)
                .await?
                .ok_or("row missing")?
                .revision
        ),
        reference["locking"]["revision_after_rollback"]
            .as_i64()
            .ok_or("missing revision")?
    );
    let updated = unchanged_update(&mut a, row.id, &revision)
        .await?
        .ok_or("update missing")?;
    assert_eq!(
        value!(updated.revision),
        reference["locking"]["unchanged_committed_revision"]
    );
    assert_eq!(
        value!(unchanged_update(&mut b, row.id, &revision).await?.is_none()),
        reference["locking"]["stale_is_none"]
    );
    Ok(())
}

async fn unchanged_update(
    connection: &mut PgConnection,
    id: TrackId,
    revision: &BigInt,
) -> Result<Option<Track>> {
    let title = String::from("Seed");
    let description = String::new();
    Ok(repo::update_track(
        connection,
        id,
        UpdateTrack {
            expected_revision: revision,
            title: &title,
            description: &description,
            producer: None,
            state: TrackState::Active,
            mode: TrackMode::Agent,
            workflow: None,
        },
        CONTEXT,
    )
    .await?)
}

fn lock_name(lock: RowLock) -> &'static str {
    match lock {
        RowLock::Share => "share",
        RowLock::Update => "update",
    }
}

#[test]
fn errors_never_retain_native_diagnostics() {
    let error = TrackError::from(sqlx::Error::Protocol("synthetic-private-value".to_owned()));
    assert!(std::error::Error::source(&error).is_none());
    assert_eq!(error.sqlstate(), None);
    assert!(!format!("{error:?} {error}").contains("synthetic-private-value"));
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
