#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_attention::*;
use cannery_core::{
    ids::{AttemptId, HypothesisId, ProjectId, ReviewCaseId},
    json::{self, Document},
    timestamps::Timestamp,
};
use cannery_reviews::{repo::*, *};
use num_bigint::BigInt;
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection};
use uuid::Uuid;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const JSON: JsonContext = JsonContext {
    decode_nesting_budget: 80,
};
fn identity(n: u64) -> Uuid {
    Uuid::parse_str(&format!("00000000-0000-0000-0000-{n:012}")).unwrap_or(Uuid::nil())
}
fn text(t: &String) -> Result<Value> {
    Ok(json!(t.as_utf8().ok_or("invalid fixture text")?))
}
fn doc(d: &Document) -> Result<Value> {
    Ok(serde_json::from_str(&json::encode_ascii_pretty(d, 80)?)?)
}
fn optional<T>(v: Option<&T>, f: impl FnOnce(&T) -> Result<Value>) -> Result<Value> {
    v.map(f).transpose().map(|v| v.unwrap_or(Value::Null))
}
fn time(t: Timestamp, clock: Timestamp) -> Value {
    if t == clock {
        json!("@source-clock")
    } else if t.0.timestamp_micros() == clock.0.timestamp_micros() - 600_000_000 {
        json!("@source-clock-minus-600")
    } else {
        json!(t.0.to_rfc3339())
    }
}
fn many<T>(rows: &[T], f: impl Fn(&T) -> Result<Value>) -> Result<Value> {
    Ok(Value::Array(
        rows.iter().map(f).collect::<Result<Vec<_>>>()?,
    ))
}
include!("support/projections.rs");
fn integer(r: &Value, key: &str, default: i64) -> Result<Option<BigInt>> {
    if let Some(power) = r[format!("{key}_power")].as_u64() {
        return Ok(Some(BigInt::from(10).pow(u32::try_from(power)?)));
    }
    match r.get(key) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.parse()?)),
        None => Ok(Some(default.into())),
        _ => Err("invalid numeric recipe".into()),
    }
}
fn filter(r: &Value, key: &str) -> Option<String> {
    r[key].as_str().map(String::from)
}
fn id(r: &Value, key: &str, default: u64) -> Uuid {
    identity(r[key].as_u64().unwrap_or(default))
}
async fn operation(c: &mut PgConnection, r: &Value, clock: Timestamp) -> Result<Value> {
    let project = ProjectId(id(r, "project", 2));
    let limit = integer(r, "limit", 100)?;
    match r["action"].as_str().ok_or("missing action")? {
        "get" | "get-locked" => optional(
            get_case(
                c,
                project,
                ReviewCaseId(id(r, "target", 301)),
                r["action"] == "get-locked",
            )
            .await?
            .as_ref(),
            |v| canon_case(v, clock),
        ),
        "failure" => optional(
            get_failure(c, FailureId(id(r, "target", 408)), JSON)
                .await?
                .as_ref(),
            |v| canon_failure(v, clock),
        ),
        "list" => {
            let kind = filter(r, "kind");
            let state = filter(r, "state");
            many(
                &cannery_reviews::repo::list_cases(
                    c,
                    project,
                    ListCases {
                        kind: kind.as_ref(),
                        state: state.as_ref(),
                        before: r["before"].as_u64().map(|n| ReviewCaseId(identity(n))),
                        limit: limit.as_ref(),
                    },
                )
                .await?,
                |v| canon_case(v, clock),
            )
        }
        "open" => {
            let revision = integer(r, "revision", 1)?;
            let default_revision = BigInt::from(1);
            let input = |e| OpenDecisionCase {
                project_id: project,
                hypothesis_id: HypothesisId(id(r, "hypothesis", 43)),
                attempt_id: AttemptId(id(r, "attempt", 113)),
                evidence_id: Some(EvidenceId(e)),
                writeup_id: None,
                subject_revision: revision.as_ref().unwrap_or(&default_revision),
            };
            let new = open_decision_case(c, input(id(r, "evidence", 213))).await?;
            if let Some(other) = r["duplicate"].as_u64() {
                open_decision_case(c, input(identity(other))).await?;
            }
            let row = get_case(c, project, new, false)
                .await?
                .ok_or("missing inserted review")?;
            let mut row = canon_case(&row, clock)?;
            row["id"] = json!("@new-case");
            Ok(row)
        }
        "pending" => many(&pending_reviews(c, project, limit.as_ref()).await?, |v| {
            canon_pendingreview(v, clock)
        }),
        "counts" => Ok(Value::Object(
            pending_counts(c, project)
                .await?
                .into_iter()
                .map(|(k, v)| (k.as_str().to_owned(), json!(v)))
                .collect(),
        )),
        "running" => {
            let (total, rows) = running_attempts(c, project, limit.as_ref()).await?;
            Ok(json!([
                total,
                many(&rows, |v| canon_runningattempt(v, clock))?
            ]))
        }
        "outcomes" => many(&recent_outcomes(c, project, limit.as_ref()).await?, |v| {
            canon_outcome(v, clock)
        }),
        "failures" => many(&recent_failures(c, project, limit.as_ref()).await?, |v| {
            canon_recentfailure(v, clock)
        }),
        "stalled" => {
            let seconds = integer(r, "seconds", 600)?;
            let (total, rows) =
                stalled_verifications(c, project, limit.as_ref(), seconds.as_ref()).await?;
            Ok(json!([
                total,
                many(&rows, |v| canon_stalledverification(v, clock))?
            ]))
        }
        _ => Err("unknown recipe".into()),
    }
}
fn error_value(error: &(dyn std::error::Error + 'static)) -> Value {
    let e = error.downcast_ref::<Error>();
    json!({"error":if matches!(e,Some(Error::Invariant)){"invariant"}else if e.and_then(Error::sqlstate).is_some(){"server"}else{"driver"},"sqlstate":e.and_then(Error::sqlstate)})
}

#[tokio::test]
#[ignore = "requires guarded disposable Rust-migrated database"]
async fn repository_matches_frozen_source() -> Result<()> {
    let url = std::env::var("RA_NATIVE_DATABASE_URL")?;
    if !url.starts_with("postgresql://")
        || !url
            .rsplit('/')
            .next()
            .is_some_and(|n| n.starts_with("ra_fixture_"))
    {
        return Err("unguarded native fixture".into());
    }
    let fixture: Value =
        serde_json::from_str(runtime_reference!("/tests/fixtures/reference.json"))?;
    let mut c = PgConnection::connect(&url).await?;
    let mut tx = c.begin().await?;
    sqlx::raw_sql(include_str!("fixtures/seed.sql"))
        .execute(&mut *tx)
        .await?;
    let clock: Timestamp = sqlx::query_scalar("SELECT now()")
        .persistent(false)
        .fetch_one(&mut *tx)
        .await?;
    for (recipe, expected) in fixture["recipes"]
        .as_array()
        .ok_or("missing recipes")?
        .iter()
        .zip(
            fixture["observations"]["recipes"]
                .as_array()
                .ok_or("missing observations")?,
        )
    {
        let mut nested = tx.begin().await?;
        let result = operation(&mut nested, recipe, clock).await;
        nested.rollback().await?;
        let observed = match result {
            Ok(v) => json!({"name":recipe["name"],"value":v}),
            Err(e) => {
                let mut v = error_value(e.as_ref());
                v["name"] = recipe["name"].clone();
                v
            }
        };
        if ["limit", "seconds", "revision"]
            .into_iter()
            .map(|key| integer(recipe, key, 1))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .any(|value| value < BigInt::from(i64::MIN) || value > BigInt::from(i64::MAX))
            && recipe["name"] != "interval-before-limit-driver"
        {
            // Native integer arguments refuse values outside BIGINT before
            // encoding. Keep the source observation and savepoint rollback.
            assert_eq!(
                observed,
                json!({"name":recipe["name"],"error":"driver","sqlstate":null})
            );
        } else {
            assert_eq!(&observed, expected, "recipe {}", recipe["name"]);
        }
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM review_cases")
        .persistent(false)
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(
        json!([count]),
        fixture["observations"]["after-rollback-case-count"]
    );
    tx.commit().await?;
    assert_eq!(
        concurrency(&url).await?,
        fixture["observations"]["concurrency"]
    );
    Ok(())
}
async fn blocked(c: &mut PgConnection, pid: i32) -> Result<bool> {
    for _ in 0..100 {
        let v: Option<bool> =
            sqlx::query_scalar("SELECT wait_event_type='Lock' FROM pg_stat_activity WHERE pid=$1")
                .bind(pid)
                .persistent(false)
                .fetch_optional(&mut *c)
                .await?
                .flatten();
        if v == Some(true) {
            return Ok(true);
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    Err("fixture did not block".into())
}
async fn concurrency(url: &str) -> Result<Value> {
    let mut first = PgConnection::connect(url).await?;
    let mut second = PgConnection::connect(url).await?;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .persistent(false)
        .fetch_one(&mut second)
        .await?;
    let mut held = first.begin().await?;
    sqlx::query("SELECT id FROM review_cases WHERE id=$1 FOR UPDATE")
        .bind(identity(301))
        .persistent(false)
        .fetch_one(&mut *held)
        .await?;
    let reader = tokio::spawn(async move {
        let row = get_case(
            &mut second,
            ProjectId(identity(2)),
            ReviewCaseId(identity(301)),
            true,
        )
        .await?;
        Ok::<_, Error>((second, row))
    });
    let waited = blocked(&mut held, pid).await?;
    sqlx::query("UPDATE hypotheses SET number=90 WHERE id=$1")
        .bind(identity(37))
        .persistent(false)
        .execute(&mut *held)
        .await?;
    held.commit().await?;
    let (mut second, row) = reader.await??;
    let joined = row.ok_or("missing joined case")?;
    let mut held = first.begin().await?;
    let one = BigInt::from(1);
    open_decision_case(
        &mut held,
        OpenDecisionCase {
            project_id: ProjectId(identity(2)),
            hypothesis_id: HypothesisId(identity(43)),
            attempt_id: AttemptId(identity(113)),
            evidence_id: Some(EvidenceId(identity(213))),
            writeup_id: None,
            subject_revision: &one,
        },
    )
    .await?;
    let contender = tokio::spawn(async move {
        let one = BigInt::from(1);
        let id = open_decision_case(
            &mut second,
            OpenDecisionCase {
                project_id: ProjectId(identity(2)),
                hypothesis_id: HypothesisId(identity(43)),
                attempt_id: AttemptId(identity(113)),
                evidence_id: Some(EvidenceId(identity(214))),
                writeup_id: None,
                subject_revision: &one,
            },
        )
        .await?;
        get_case(&mut second, ProjectId(identity(2)), id, false).await
    });
    let unique_waited = blocked(&mut held, pid).await?;
    held.rollback().await?;
    let accepted = contender.await??;
    Ok(
        json!({"case_waited":waited,"joined_number":joined.hypothesis_number,"unique_waited":unique_waited,"rollback_contender_accepted":accepted.is_some()}),
    )
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
