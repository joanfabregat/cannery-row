//! Source measurement reads on an explicit caller-owned PostgreSQL connection.
use crate::numeric::PgNumeric;
use cannery_core::{
    ids::{AttemptId, ProjectId},
    json::{self, Document, Node},
    timestamps::Timestamp,
};
use chrono::NaiveDateTime;
use num_bigint::BigInt;
use sqlx::PgConnection;

/// Naive timestamps are interpreted by PostgreSQL's current session timezone.
#[derive(Clone, Copy)]
pub enum TimeBound {
    Aware(Timestamp),
    Naive(NaiveDateTime),
}
impl std::fmt::Debug for TimeBound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TimeBound([redacted])")
    }
}
/// Explicit source JSON decoder call profile; no universal native default.
#[derive(Clone, Copy, Debug)]
pub struct JsonContext {
    pub nesting_budget: usize,
}
/// Filters retain duplicates, insertion order, and absent versus empty lists.
pub struct Query {
    pub project_id: ProjectId,
    pub metric: String,
    pub authority: String,
    pub split: Option<String>,
    pub dimensions: Option<Vec<String>>,
    pub filters: Option<Vec<(String, Vec<String>)>>,
    pub tracks: Option<Vec<String>>,
    /// Lossless source TEXT adaptation, when a legacy dashboard supplies tracks.
    pub attempt_states: Option<Vec<String>>,
    pub science_revision: Option<BigInt>,
    pub since: Option<TimeBound>,
    pub until: Option<TimeBound>,
}
impl std::fmt::Debug for Query {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Query([redacted])")
    }
}
impl Query {
    fn validate_text_parameters(&self, measurements: bool) -> Result<(), MetricsError> {
        // Psycopg's text dumper refuses NUL before executing SQL. JSON filter
        // strings are escaped by its Jsonb dumper and rejected by PostgreSQL.
        let bad_common = self
            .tracks
            .iter()
            .chain(self.attempt_states.iter())
            .flatten()
            .any(|value| value.contains('\0'));
        let bad_measurement = measurements
            && (self.metric.contains('\0')
                || self.authority.contains('\0')
                || self
                    .split
                    .as_ref()
                    .is_some_and(|value| value.contains('\0'))
                || self
                    .dimensions
                    .iter()
                    .flatten()
                    .any(|value| value.contains('\0')));
        if bad_common || bad_measurement {
            Err(MetricsError::Database { sqlstate: None })
        } else {
            Ok(())
        }
    }
    #[must_use]
    pub fn new(project_id: ProjectId, metric: String, authority: String) -> Self {
        Self {
            project_id,
            metric,
            authority,
            split: None,
            dimensions: Some(Vec::new()),
            filters: None,
            tracks: None,
            attempt_states: None,
            science_revision: None,
            since: None,
            until: None,
        }
    }
    fn parameters(&self) -> Result<Parameters, MetricsError> {
        let mut keys = self.dimensions.clone().unwrap_or_default();
        keys.sort();
        let filters = self
            .filters
            .as_ref()
            .filter(|f| !f.is_empty())
            .map(|filters| {
                let mut object = serde_json::Map::new();
                for (name, values) in filters {
                    let mut values = values.clone();
                    values.sort();
                    object.insert(name.clone(), serde_json::Value::from(values));
                }
                serde_json::Value::Object(object).to_string()
            });
        let (since_aware, since_naive) = bounds(self.since);
        let (until_aware, until_naive) = bounds(self.until);
        Ok(Parameters {
            authorities: if self.authority == "imported" {
                vec!["imported_artifact".into(), "imported_transcribed".into()]
            } else {
                vec![self.authority.clone()]
            },
            stage: if self.authority == "agent_claim" {
                "agent"
            } else {
                "verification"
            },
            origin: if self.authority.starts_with("imported") {
                "imported"
            } else {
                "live"
            },
            keys,
            filters,
            science: integer(self.science_revision.as_ref())?,
            since_aware,
            since_naive,
            until_aware,
            until_naive,
        })
    }
}
struct Parameters {
    authorities: Vec<String>,
    stage: &'static str,
    origin: &'static str,
    keys: Vec<String>,
    filters: Option<String>,
    science: Option<String>,
    since_aware: Option<Timestamp>,
    since_naive: Option<NaiveDateTime>,
    until_aware: Option<Timestamp>,
    until_naive: Option<NaiveDateTime>,
}
fn bounds(value: Option<TimeBound>) -> (Option<Timestamp>, Option<NaiveDateTime>) {
    match value {
        Some(TimeBound::Aware(v)) => (Some(v), None),
        Some(TimeBound::Naive(v)) => (None, Some(v)),
        None => (None, None),
    }
}
fn integer(value: Option<&BigInt>) -> Result<Option<String>, MetricsError> {
    value
        .map(|v| {
            let s = v.to_string();
            if s.trim_start_matches('-').len() > 131_072 {
                Err(MetricsError::Database { sqlstate: None })
            } else {
                Ok(s)
            }
        })
        .transpose()
}
#[derive(Debug, thiserror::Error)]
pub enum MetricsError {
    #[error("measurement database operation failed")]
    Database { sqlstate: Option<String> },
    #[error("measurement JSON decoding failed")]
    Decode,
}
impl MetricsError {
    fn database(e: &sqlx::Error) -> Self {
        Self::Database {
            sqlstate: e
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .filter(|v| {
                    v.len() == 5
                        && v.bytes()
                            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
                })
                .map(std::borrow::Cow::into_owned),
        }
    }
}
fn document(text: &str, context: JsonContext) -> Result<Document, MetricsError> {
    json::decode_str(text, context.nesting_budget).map_err(|_| MetricsError::Decode)
}
fn optional_document(
    text: Option<String>,
    context: JsonContext,
) -> Result<Option<Document>, MetricsError> {
    text.map(|s| document(&s, context))
        .transpose()
        .map(|d| d.filter(|d| !matches!(d.node(d.root()), Some(Node::Null))))
}

pub struct Point {
    pub id: i64,
    pub attempt_id: AttemptId,
    pub unit_number: i32,
    pub unit_title: String,
    pub unit_state: String,
    pub attempt_sequence: i32,
    pub attempt_state: String,
    pub science_revision: i32,
    pub claimed_at: Timestamp,
    pub submitted_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub track_slug: String,
    pub track_title: String,
    pub metric: String,
    pub split: String,
    pub dimensions: Document,
    pub value: Option<f64>,
    pub missing_reason: Option<String>,
    pub unit: String,
    pub direction: String,
    pub sample_count: Option<PgNumeric>,
    pub control_value: Option<f64>,
    pub uncertainty_method: Option<String>,
    pub uncertainty_lower: Option<f64>,
    pub uncertainty_upper: Option<f64>,
    pub authority: String,
    pub source_ref: Option<String>,
    pub recorded_at: Timestamp,
    pub control: Option<Document>,
    pub project_fields: Option<Document>,
    pub reference_value: Option<f64>,
    pub reference_label: Option<String>,
    pub reference_kind: Option<String>,
    pub reference_ref: Option<String>,
}
impl std::fmt::Debug for Point {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Point([redacted])")
    }
}
#[derive(sqlx::FromRow)]
struct RawPoint {
    id: i64,
    #[sqlx(rename = "attempt_id!: _")]
    attempt_id: AttemptId,
    unit_number: i32,
    unit_title: String,
    unit_state: String,
    attempt_sequence: i32,
    attempt_state: String,
    science_revision: i32,
    #[sqlx(rename = "claimed_at!: _")]
    claimed_at: Timestamp,
    #[sqlx(rename = "submitted_at: _")]
    submitted_at: Option<Timestamp>,
    #[sqlx(rename = "finished_at: _")]
    finished_at: Option<Timestamp>,
    track_slug: String,
    track_title: String,
    metric: String,
    split: String,
    #[sqlx(rename = "dimensions!")]
    dimensions: String,
    value: Option<f64>,
    missing_reason: Option<String>,
    unit: String,
    direction: String,
    #[sqlx(rename = "sample_count: _")]
    sample_count: Option<PgNumeric>,
    control_value: Option<f64>,
    uncertainty_method: Option<String>,
    uncertainty_lower: Option<f64>,
    uncertainty_upper: Option<f64>,
    authority: String,
    source_ref: Option<String>,
    #[sqlx(rename = "recorded_at!: _")]
    recorded_at: Timestamp,
    control: Option<String>,
    project_fields: Option<String>,
    #[sqlx(rename = "reference_value?")]
    reference_value: Option<f64>,
    #[sqlx(rename = "reference_label?")]
    reference_label: Option<String>,
    #[sqlx(rename = "reference_kind?")]
    reference_kind: Option<String>,
    #[sqlx(rename = "reference_ref?")]
    reference_ref: Option<String>,
}
impl RawPoint {
    fn decode(self, c: JsonContext) -> Result<Point, MetricsError> {
        Ok(Point {
            id: self.id,
            attempt_id: self.attempt_id,
            unit_number: self.unit_number,
            unit_title: self.unit_title,
            unit_state: self.unit_state,
            attempt_sequence: self.attempt_sequence,
            attempt_state: self.attempt_state,
            science_revision: self.science_revision,
            claimed_at: self.claimed_at,
            submitted_at: self.submitted_at,
            finished_at: self.finished_at,
            track_slug: self.track_slug,
            track_title: self.track_title,
            metric: self.metric,
            split: self.split,
            dimensions: document(&self.dimensions, c)?,
            value: self.value,
            missing_reason: self.missing_reason,
            unit: self.unit,
            direction: self.direction,
            sample_count: self.sample_count,
            control_value: self.control_value,
            uncertainty_method: self.uncertainty_method,
            uncertainty_lower: self.uncertainty_lower,
            uncertainty_upper: self.uncertainty_upper,
            authority: self.authority,
            source_ref: self.source_ref,
            recorded_at: self.recorded_at,
            control: optional_document(self.control, c)?,
            project_fields: optional_document(self.project_fields, c)?,
            reference_value: self.reference_value,
            reference_label: self.reference_label,
            reference_kind: self.reference_kind,
            reference_ref: self.reference_ref,
        })
    }
}
pub struct Summary {
    pub rows: i64,
    pub measured: i64,
    pub sample_count: PgNumeric,
    pub science_revisions: Vec<i32>,
    pub controls: Document,
}
impl std::fmt::Debug for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Summary([redacted])")
    }
}
#[derive(sqlx::FromRow)]
struct RawSummary {
    #[sqlx(rename = "rows!")]
    rows: i64,
    #[sqlx(rename = "measured!")]
    measured: i64,
    #[sqlx(rename = "sample_count!: _")]
    sample_count: PgNumeric,
    #[sqlx(rename = "science_revisions!")]
    science_revisions: Vec<i32>,
    #[sqlx(rename = "controls!")]
    controls: String,
}

/// Newest measurement first, including the source's unvalidated pagination semantics.
/// # Errors
/// Returns sanitized database, integer conversion, or JSON decoding failures.
pub async fn points(
    conn: &mut PgConnection,
    q: &Query,
    before: Option<&BigInt>,
    limit: Option<&BigInt>,
    context: JsonContext,
) -> Result<Vec<Point>, MetricsError> {
    q.validate_text_parameters(true)?;
    let p = q.parameters()?;
    let before = integer(before)?;
    let limit = integer(limit)?;
    let rows = sqlx::query_file_as!(
        RawPoint,
        "src/sql/points.sql",
        q.project_id as ProjectId,
        &q.metric,
        &p.authorities,
        p.stage,
        q.split.as_deref(),
        q.dimensions.is_none(),
        &p.keys,
        p.filters.as_deref(),
        q.tracks.as_deref(),
        q.attempt_states.as_deref(),
        p.science.as_deref(),
        p.since_aware as Option<Timestamp>,
        p.since_naive as Option<NaiveDateTime>,
        p.until_aware as Option<Timestamp>,
        p.until_naive as Option<NaiveDateTime>,
        before.as_deref(),
        limit.as_deref()
    )
    .fetch_all(conn)
    .await
    .map_err(|e| MetricsError::database(&e))?;
    rows.into_iter().map(|r| r.decode(context)).collect()
}
/// Aggregates over the entire query, independently of point pagination.
/// # Errors
/// Returns sanitized database, integer conversion, or JSON decoding failures.
pub async fn summary(
    conn: &mut PgConnection,
    q: &Query,
    context: JsonContext,
) -> Result<Summary, MetricsError> {
    q.validate_text_parameters(true)?;
    let p = q.parameters()?;
    let r = sqlx::query_file_as!(
        RawSummary,
        "src/sql/summary.sql",
        q.project_id as ProjectId,
        &q.metric,
        &p.authorities,
        p.stage,
        q.split.as_deref(),
        q.dimensions.is_none(),
        &p.keys,
        p.filters.as_deref(),
        q.tracks.as_deref(),
        q.attempt_states.as_deref(),
        p.science.as_deref(),
        p.since_aware as Option<Timestamp>,
        p.since_naive as Option<NaiveDateTime>,
        p.until_aware as Option<Timestamp>,
        p.until_naive as Option<NaiveDateTime>
    )
    .fetch_one(conn)
    .await
    .map_err(|e| MetricsError::database(&e))?;
    Ok(Summary {
        rows: r.rows,
        measured: r.measured,
        sample_count: r.sample_count,
        science_revisions: r.science_revisions,
        controls: document(&r.controls, context)?,
    })
}
/// Failed attempts deliberately ignore metric, split, dimensions and filters.
/// # Errors
/// Returns sanitized database or integer conversion failures.
pub async fn failed_attempts(conn: &mut PgConnection, q: &Query) -> Result<i64, MetricsError> {
    q.validate_text_parameters(false)?;
    let p = q.parameters()?;
    let r = sqlx::query_file!(
        "src/sql/failed.sql",
        q.project_id as ProjectId,
        p.origin,
        q.tracks.as_deref(),
        q.attempt_states.as_deref(),
        p.science.as_deref(),
        p.since_aware as Option<Timestamp>,
        p.since_naive as Option<NaiveDateTime>,
        p.until_aware as Option<Timestamp>,
        p.until_naive as Option<NaiveDateTime>
    )
    .fetch_one(conn)
    .await
    .map_err(|e| MetricsError::database(&e))?;
    Ok(r.count)
}
