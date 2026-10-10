//! Fixed, credential-free PostgreSQL text observations for conformance builds.
use crate::{AppState, authentication::authenticate, errors::ApiError, requests::RequestContext};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
};
use cannery_core::{
    errors::{DomainError, ErrorCode},
    json::Node,
    principal::Scope,
};
use serde_json::{Map, Value, json};
use sqlx::{Connection, PgConnection, Row};

const ROW_LIMIT: usize = 2000;

pub(super) async fn head() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [
            (axum::http::header::ALLOW, "GET"),
            (axum::http::header::CONTENT_LENGTH, "31"),
        ],
        Json(json!({"detail":"Method Not Allowed"})),
    )
        .into_response()
}
struct Projection {
    entity: &'static str,
    query: &'static str,
    comparable: &'static [&'static str],
    captured: &'static [&'static str],
}
// Match tests/conformance/hooks.py. Only the project UUID and remaining row
// allowance are bound; callers cannot supply SQL, identifiers or column names.
const PROJECTIONS: &[Projection] = &[
    Projection {
        entity: "config",
        query: r"SELECT ARRAY[kind,
               revision::text] AS key,
               content::text AS content,
               science_revision::text AS science_revision,
               created_at::text AS created_at
           FROM config_revisions
           WHERE project_id = $1
           ORDER BY key LIMIT $2",
        comparable: &["content", "science_revision"],
        captured: &["created_at"],
    },
    Projection {
        entity: "track",
        query: r"SELECT ARRAY[t.slug] AS key,
               t.producer::text AS producer,
               t.workflow::text AS workflow,
               t.created_at::text AS created_at,
               t.updated_at::text AS updated_at
           FROM tracks t
           WHERE t.project_id = $1 AND EXISTS (SELECT 1
           FROM import_entries i
           WHERE i.project_id = t.project_id AND i.kind = 'track' AND i.key = t.slug)
           ORDER BY key LIMIT $2",
        comparable: &["producer", "workflow", "created_at", "updated_at"],
        captured: &[],
    },
    Projection {
        entity: "unit",
        query: r"SELECT ARRAY[external_id,
               number::text] AS key,
               imported::text AS imported,
               created_at::text AS created_at,
               updated_at::text AS updated_at,
               approved_at::text AS approved_at
           FROM units
           WHERE project_id = $1 AND origin = 'imported'
           ORDER BY key LIMIT $2",
        comparable: &["imported", "created_at", "updated_at", "approved_at"],
        captured: &[],
    },
    Projection {
        entity: "unit_revision",
        query: r"SELECT ARRAY[h.external_id,
               h.number::text,
               r.revision::text] AS key,
               r.content::text AS content,
               r.created_at::text AS created_at,
               r.science_revision::text AS science_revision
           FROM unit_revisions r JOIN units h ON h.id = r.unit_id
           WHERE h.project_id = $1 AND r.origin = 'imported'
           ORDER BY key LIMIT $2",
        comparable: &["content", "created_at", "science_revision"],
        captured: &[],
    },
    Projection {
        entity: "attempt",
        query: r"SELECT ARRAY[h.external_id,
               h.number::text,
               a.sequence::text] AS key,
               a.imported::text AS imported,
               a.producer::text AS producer,
               a.workflow::text AS workflow,
               a.claimed_at::text AS claimed_at,
               a.started_at::text AS started_at,
               a.submitted_at::text AS submitted_at,
               a.finished_at::text AS finished_at
           FROM attempts a JOIN units h ON h.id = a.unit_id
           WHERE a.project_id = $1 AND a.origin = 'imported'
           ORDER BY key LIMIT $2",
        comparable: &[
            "imported",
            "producer",
            "workflow",
            "claimed_at",
            "started_at",
            "submitted_at",
            "finished_at",
        ],
        captured: &[],
    },
    Projection {
        entity: "policy",
        query: r"SELECT ARRAY[id,
               revision] AS key,
               content::text AS content,
               source_ref::text AS source_ref,
               created_at::text AS created_at
           FROM historical_policies
           WHERE project_id = $1
           ORDER BY key LIMIT $2",
        comparable: &["content", "source_ref"],
        captured: &["created_at"],
    },
    Projection {
        entity: "import_entry",
        query: r"SELECT ARRAY[kind,
               key] AS key,
               content::text AS content,
               sha256::text AS sha256,
               bundle_sha256::text AS bundle_sha256,
               imported_at::text AS imported_at
           FROM import_entries
           WHERE project_id = $1
           ORDER BY key LIMIT $2",
        comparable: &["content", "sha256", "bundle_sha256"],
        captured: &["imported_at"],
    },
    Projection {
        entity: "report",
        query: r"SELECT ARRAY[h.external_id,
               h.number::text,
               a.sequence::text] AS key,
               (r.front_matter ->> 'written_on')::date::text AS written_on,
               (r.front_matter ->> 'written_at')::timestamptz::text AS written_at,
               r.body AS body_markdown,
               r.sha256::text AS sha256,
               r.source_ref::text AS source_ref,
               r.created_at::text AS created_at
           FROM phase_outputs r JOIN attempts a ON a.id = r.attempt_id JOIN units h ON h.id = a.unit_id
           WHERE a.project_id = $1 AND r.stage = 'writeup'
           ORDER BY key LIMIT $2",
        comparable: &[
            "written_on",
            "written_at",
            "body_markdown",
            "sha256",
            "source_ref",
        ],
        captured: &["created_at"],
    },
    Projection {
        entity: "review_case",
        query: r"SELECT ARRAY[h.external_id,
               h.number::text,
               coalesce(a.sequence::text,
               ''),
               c.kind,
               c.subject_revision::text] AS key,
               c.opened_at::text AS opened_at,
               c.resolved_at::text AS resolved_at
           FROM review_cases c JOIN units h ON h.id = c.unit_id LEFT JOIN attempts a ON a.id = c.attempt_id
           WHERE c.project_id = $1 AND c.origin = 'imported'
           ORDER BY key LIMIT $2",
        comparable: &["opened_at", "resolved_at"],
        captured: &[],
    },
    Projection {
        entity: "decision",
        query: r"SELECT ARRAY[h.external_id,
               h.number::text,
               coalesce(a.sequence::text,
               ''),
               c.kind,
               c.subject_revision::text,
               d.action] AS key,
               d.decided_at::text AS decided_at,
               d.origin::text AS origin,
               d.source_ref::text AS source_ref,
               d.via_channel::text AS via_channel,
               d.via_client::text AS via_client
           FROM decisions d JOIN review_cases c ON c.id = d.review_case_id JOIN units h ON h.id = c.unit_id LEFT JOIN attempts a ON a.id = c.attempt_id
           WHERE c.project_id = $1 AND d.origin = 'imported'
           ORDER BY key LIMIT $2",
        comparable: &[
            "decided_at",
            "origin",
            "source_ref",
            "via_channel",
            "via_client",
        ],
        captured: &[],
    },
    Projection {
        entity: "measurement",
        query: r"SELECT ARRAY[h.external_id,
               h.number::text,
               a.sequence::text,
               e.stage,
               e.revision::text,
               m.metric,
               m.split,
               m.dimensions::text] AS key,
               m.dimensions::text AS dimensions,
               m.recorded_at::text AS recorded_at,
               m.authority::text AS authority,
               m.source_ref::text AS source_ref,
               m.value::text AS value,
               m.control_value::text AS control_value,
               m.uncertainty_lower::text AS uncertainty_lower,
               m.uncertainty_upper::text AS uncertainty_upper
           FROM measurements m JOIN attempts a ON a.id = m.attempt_id JOIN units h ON h.id = a.unit_id JOIN phase_outputs e ON e.id = m.evidence_id
           WHERE m.project_id = $1 AND e.origin = 'imported'
           ORDER BY key LIMIT $2",
        comparable: &[
            "dimensions",
            "recorded_at",
            "authority",
            "source_ref",
            "value",
            "control_value",
            "uncertainty_lower",
            "uncertainty_upper",
        ],
        captured: &[],
    },
    Projection {
        entity: "comparison",
        query: r"SELECT ARRAY[h.external_id,
               h.number::text,
               a.sequence::text,
               e.stage,
               e.revision::text,
               c.metric,
               c.split,
               c.dimensions::text] AS key,
               c.dimensions::text AS dimensions,
               c.recorded_at::text AS recorded_at,
               c.policy_revision::text AS policy_revision,
               c.value::text AS value,
               c.reference_value::text AS reference_value
           FROM comparisons c JOIN attempts a ON a.id = c.attempt_id JOIN units h ON h.id = a.unit_id JOIN phase_outputs e ON e.id = c.evidence_id
           WHERE c.project_id = $1 AND e.origin = 'imported'
           ORDER BY key LIMIT $2",
        comparable: &[
            "dimensions",
            "recorded_at",
            "policy_revision",
            "value",
            "reference_value",
        ],
        captured: &[],
    },
    Projection {
        entity: "evidence",
        query: r"SELECT ARRAY[h.external_id,
               h.number::text,
               a.sequence::text,
               e.stage,
               e.revision::text] AS key,
               e.front_matter::text AS content,
               e.sha256::text AS sha256,
               e.created_at::text AS created_at
           FROM phase_outputs e JOIN attempts a ON a.id = e.attempt_id JOIN units h ON h.id = a.unit_id
           WHERE e.project_id = $1 AND e.origin = 'imported' AND e.stage <> 'writeup'
           ORDER BY key LIMIT $2",
        comparable: &["created_at"],
        captured: &["content", "sha256"],
    },
];

#[derive(Debug)]
enum ObservationError {
    Database(sqlx::Error),
    Missing,
    Overflow,
    NotFixture,
}
impl From<sqlx::Error> for ObservationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

pub(super) async fn project_storage(
    State(state): State<AppState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Result<Response, ApiError> {
    let mut authenticated = authenticate(&state, &context, &headers, &Method::GET).await?;
    if let Err(error) = crate::validation::validate_parameter_node(
        &Node::String(String::from(&slug)),
        crate::validation::Parameter::Slug,
    ) {
        return Err(error
            .domain_error()
            .map_err(|_| context.internal("validation location encoding"))?
            .into());
    }
    authenticated.principal.require_admin(false)?;
    authenticated.principal.require_scope(Scope::Read)?;
    match observe(&mut authenticated.connection, &slug).await {
        Ok(value) => Ok(Json(value).into_response()),
        Err(ObservationError::Missing) => Err(DomainError::new(ErrorCode::NotFound, "project not found").into()),
        Err(ObservationError::Overflow) => Ok((StatusCode::CONFLICT, Json(json!({"error":{"code":"storage_projection_overflow","message":"storage projection exceeds its row bound","details":null}}))).into_response()),
        Err(ObservationError::NotFixture) => Err(DomainError::new(ErrorCode::Forbidden, "storage observation requires a conformance database").into()),
        Err(ObservationError::Database(error)) => {
            drop(error); // Never expose driver diagnostics or stored values.
            Err(context.internal("conformance storage observation"))
        }
    }
}

async fn observe(connection: &mut PgConnection, slug: &str) -> Result<Value, ObservationError> {
    let mut transaction = connection.begin().await?;
    let result = read_snapshot(&mut transaction, slug).await;
    // Explicitly settle every ordinary failure. Cancellation also retains SQLx's
    // transaction Drop rollback before this physical connection can be reused.
    match result {
        Ok(value) => {
            transaction.commit().await?;
            Ok(value)
        }
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(error)
        }
    }
}

async fn read_snapshot(
    connection: &mut PgConnection,
    slug: &str,
) -> Result<Value, ObservationError> {
    for statement in [
        "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY",
        "SET LOCAL TIME ZONE 'UTC'",
        "SET LOCAL DateStyle = 'ISO, YMD'",
        "SET LOCAL statement_timeout = '30s'",
    ] {
        sqlx::query(statement).execute(&mut *connection).await?;
    }
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&mut *connection)
        .await?;
    if !database.strip_prefix("conformance_").is_some_and(|suffix| {
        suffix.len() == 24
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    }) {
        return Err(ObservationError::NotFixture);
    }
    let project: Option<uuid::Uuid> = sqlx::query_scalar("SELECT id FROM projects WHERE slug = $1")
        .bind(slug)
        .fetch_optional(&mut *connection)
        .await?;
    let project = project.ok_or(ObservationError::Missing)?;
    let session = sqlx::query("SELECT current_setting('TimeZone') AS timezone, current_setting('DateStyle') AS date_style, current_setting('transaction_read_only') AS read_only")
        .fetch_one(&mut *connection).await?;
    let session = json!({"timezone":session.try_get::<String,_>("timezone")?, "date_style":session.try_get::<String,_>("date_style")?, "read_only":session.try_get::<String,_>("read_only")?});
    let mut rows = Vec::new();
    let mut counts = Map::new();
    for projection in PROJECTIONS {
        let limit =
            i64::try_from(ROW_LIMIT - rows.len() + 1).map_err(|_| ObservationError::Overflow)?;
        let records = sqlx::query(projection.query)
            .bind(project)
            .bind(limit)
            .fetch_all(&mut *connection)
            .await?;
        if rows.len() + records.len() > ROW_LIMIT {
            return Err(ObservationError::Overflow);
        }
        counts.insert(projection.entity.into(), json!(records.len()));
        for record in records {
            let mut values = Map::new();
            let mut captured = Map::new();
            for (names, output) in [
                (projection.comparable, &mut values),
                (projection.captured, &mut captured),
            ] {
                for &name in names {
                    output.insert(
                        name.into(),
                        json!(record.try_get::<Option<String>, _>(name)?),
                    );
                }
            }
            let key: Vec<Option<String>> = record.try_get("key")?;
            rows.push(
                json!({"entity":projection.entity,"key":key,"values":values,"captured":captured}),
            );
        }
    }
    Ok(json!({"version":1,"project":slug,"session":session,"counts":counts,"rows":rows}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::{Executor, postgres::PgPoolOptions};
    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    #[tokio::test]
    async fn installation_requires_testing_configuration() -> TestResult {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;
        let mut settings = cannery_core::settings::load_settings(
            None,
            &std::collections::BTreeMap::from([(
                "CANNERY_DATABASE_URL".into(),
                "postgresql://postgres:fixture@127.0.0.1:1/conformance_000000000000000000000000"
                    .into(),
            )]),
        )?;
        for enabled in [false, true] {
            settings.testing.enabled = enabled;
            let (app, state) = crate::application(settings.clone())?;
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/__conformance/storage/projects/fixture")
                        .method(Method::POST)
                        .body(Body::empty())?,
                )
                .await?;
            assert_eq!(
                response.status(),
                if enabled {
                    StatusCode::METHOD_NOT_ALLOWED
                } else {
                    StatusCode::NOT_FOUND
                }
            );
            if enabled {
                let response = app
                    .oneshot(
                        Request::builder()
                            .method(Method::HEAD)
                            .uri("/__conformance/storage/projects/fixture")
                            .body(Body::empty())?,
                    )
                    .await?;
                assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
                assert_eq!(response.headers()[axum::http::header::ALLOW], "GET");
                assert_eq!(response.headers()[axum::http::header::CONTENT_LENGTH], "31");
            }
            state.pool.close().await;
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires fresh migrated nonce-owned CANNERY_STORAGE_HOOK_DATABASE_URL"]
    async fn storage_snapshot_bounds_and_settlement() -> TestResult {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&std::env::var("CANNERY_STORAGE_HOOK_DATABASE_URL")?)
            .await?;
        let mut connection = pool.acquire().await?;
        let name: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&mut *connection)
            .await?;
        let suffix = name
            .strip_prefix("conformance_")
            .ok_or("owned database required")?;
        if suffix.len() != 24
            || !suffix
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err("owned database required".into());
        }
        let version: String = sqlx::query_scalar("SHOW server_version_num")
            .fetch_one(&mut *connection)
            .await?;
        assert_eq!(version, "170011");
        sqlx::raw_sql(r#"
            INSERT INTO users(id,issuer,subject) VALUES('00000000-0000-0000-0000-000000000001','storage-hook.fixture','owner');
            INSERT INTO projects(id,slug,title,created_by) VALUES('00000000-0000-0000-0000-000000000010','storage-proof','Storage','00000000-0000-0000-0000-000000000001');
            INSERT INTO config_revisions(project_id,kind,revision,content,created_by) VALUES('00000000-0000-0000-0000-000000000010','science',1,'{"raw":123456789012345678901234567890,"null":null,"text":"é"}','00000000-0000-0000-0000-000000000001');
        "#).execute(&mut *connection).await?;
        let before: String = sqlx::query_scalar("SELECT content::text FROM config_revisions")
            .fetch_one(&mut *connection)
            .await?;
        let projection = observe(&mut connection, "storage-proof")
            .await
            .map_err(|error| format!("{error:?}"))?;
        assert_eq!(
            projection["session"],
            json!({"timezone":"UTC","date_style":"ISO, YMD","read_only":"on"})
        );
        assert_eq!(projection["counts"].as_object().ok_or("counts")?.len(), 13);
        assert_eq!(projection["rows"][0]["values"]["content"], before);
        assert_eq!(
            projection["rows"][0]["values"]["science_revision"],
            Value::Null
        );
        assert_eq!(
            projection["rows"][0]["captured"]
                .as_object()
                .ok_or("captured")?
                .len(),
            1
        );
        // Verify server enforcement, not merely the returned session label.
        let mut transaction = connection.begin().await?;
        read_snapshot(&mut transaction, "storage-proof")
            .await
            .map_err(|error| format!("{error:?}"))?;
        let rejected = transaction
            .execute("UPDATE projects SET title = 'Forbidden'")
            .await
            .err()
            .ok_or("read-only snapshot must refuse mutation")?;
        assert_eq!(
            rejected
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("25006")
        );
        transaction.rollback().await?;
        assert!(matches!(
            observe(&mut connection, "absent").await,
            Err(ObservationError::Missing)
        ));
        assert_eq!(connection.execute("SELECT 1").await?.rows_affected(), 1);
        sqlx::raw_sql(r"INSERT INTO config_revisions(project_id,kind,revision,content,created_by) SELECT '00000000-0000-0000-0000-000000000010','science',revision,'{}','00000000-0000-0000-0000-000000000001' FROM generate_series(2,2000) revision").execute(&mut *connection).await?;
        assert_eq!(
            observe(&mut connection, "storage-proof")
                .await
                .map_err(|error| format!("{error:?}"))?["rows"]
                .as_array()
                .ok_or("rows")?
                .len(),
            ROW_LIMIT
        );
        sqlx::raw_sql(r"INSERT INTO config_revisions(project_id,kind,revision,content,created_by) VALUES('00000000-0000-0000-0000-000000000010','science',2001,'{}','00000000-0000-0000-0000-000000000001')").execute(&mut *connection).await?;
        assert!(matches!(
            observe(&mut connection, "storage-proof").await,
            Err(ObservationError::Overflow)
        ));
        assert_eq!(connection.execute("SELECT 1").await?.rows_affected(), 1);
        let after: String =
            sqlx::query_scalar("SELECT content::text FROM config_revisions WHERE revision = 1")
                .fetch_one(&mut *connection)
                .await?;
        assert_eq!(before, after);
        drop(connection);
        pool.close().await;
        Ok(())
    }
}
