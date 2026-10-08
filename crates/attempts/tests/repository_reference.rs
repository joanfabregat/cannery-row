#![allow(clippy::expect_used, clippy::panic)]
#![forbid(unsafe_code)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_attempts::{model::*, repo::*};
use cannery_core::{
    ids::*,
    json::{self, Document},
    principal::{Channel, Principal, UserPrincipal, Via},
    timestamps::Timestamp,
};
use num_bigint::BigInt;
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection, Row};
use std::{collections::BTreeSet, str::FromStr};
use uuid::Uuid;
fn owned_url() -> String {
    let url = std::env::var("CANNERY_ATTEMPTS_TEST_DATABASE_URL").expect("owned fixture URL");
    let options = sqlx::postgres::PgConnectOptions::from_str(&url)
        .unwrap_or_else(|_| panic!("requires valid disposable PostgreSQL options"));
    let database = options
        .get_database()
        .expect("requires explicit disposable database");
    assert!(
        database.len() == 36
            && database.starts_with("conformance_")
            && database[12..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "requires guarded disposable database"
    );
    url
}
fn id(n: u64) -> Uuid {
    Uuid::from_u128(
        u128::from(n / 1_000_000_000_000) * 0x1000_0000_0000
            + u128::from_str_radix(&format!("{n:012}"), 16).expect("fixture UUID"),
    )
}
fn number(recipe: &Value, key: &str, default: i64) -> BigInt {
    if let Some(power) = recipe.get(format!("{key}_power")).and_then(Value::as_u64) {
        BigInt::from(10).pow(u32::try_from(power).expect("fixture exponent"))
    } else {
        recipe.get(key).and_then(Value::as_str).map_or_else(
            || BigInt::from(default),
            |s| BigInt::from_str(s).expect("fixture integer"),
        )
    }
}
fn flag(recipe: &Value, key: &str) -> bool {
    recipe.get(key).and_then(Value::as_bool).unwrap_or(false)
}
fn text<'a>(recipe: &'a Value, key: &str, default: &'a str) -> &'a str {
    recipe.get(key).and_then(Value::as_str).unwrap_or(default)
}
fn target(recipe: &Value, key: &str, default: u64) -> u64 {
    recipe.get(key).and_then(Value::as_u64).unwrap_or(default)
}
fn context() -> JsonContext {
    JsonContext {
        encode_nesting_budget: 9994,
        decode_nesting_budget: 9994,
    }
}
fn document(text: &str) -> Document {
    json::decode(text.as_bytes(), 9994).expect("fixture JSON")
}
fn principal() -> Principal {
    Principal::User(UserPrincipal {
        user_id: UserId(id(1)),
        email: None,
        display_name: None,
        is_admin: false,
        via: Via {
            channel: Channel::Cli,
            client: Some("fixture".into()),
        },
        scopes: BTreeSet::new(),
        session_id: None,
        csrf_token: None,
    })
}
fn stored(value: &StoredJson) -> Value {
    match value {
        StoredJson::SqlNull => Value::Null,
        StoredJson::Value(value) => serde_json::from_str(
            &json::encode_ascii_pretty(value, 9994).expect("decoded stored JSON"),
        )
        .expect("stored fixture JSON"),
    }
}
fn timestamp(value: Timestamp, now: Timestamp, field: &str) -> Value {
    if [
        "lease_expires_at",
        "started_at",
        "submitted_at",
        "finished_at",
        "expires_at",
        "urls_expire_at",
        "deadline",
    ]
    .contains(&field)
    {
        json!({"clock_delta_microseconds":(value.0-now.0).num_microseconds().expect("fixture timestamp range").to_string()})
    } else {
        json!(value.isoformat())
    }
}
fn hex_bytes(value: &[u8]) -> String {
    use std::fmt::Write;
    value.iter().fold(String::new(), |mut output, byte| {
        write!(output, "{byte:02x}").expect("string write");
        output
    })
}
fn bytes(value: &[u8]) -> Value {
    json!({"bytes_hex":hex_bytes(value)})
}
trait Project {
    fn project(&self, now: Timestamp) -> Value;
}
async fn invoke(
    repository: &mut Repository<'_>,
    recipe: &Value,
    now: Timestamp,
) -> Result<Value, AttemptError> {
    let missing = flag(recipe, "missing");
    let attempt = AttemptId(id(if missing {
        0
    } else {
        target(recipe, "target", 21)
    }));
    let hypothesis = HypothesisId(id(if missing {
        0
    } else {
        target(recipe, "hypothesis", 13)
    }));
    let project = ProjectId(id(if missing { 0 } else { 2 }));
    let upload = UploadId(id(if missing {
        0
    } else if flag(recipe, "job") {
        52
    } else {
        51
    }));
    let principal = principal();
    let payload = document(text(
        recipe,
        "payload",
        "{\"ordered\":1,\"unicode\":\"é😀\",\"float\":1.0}",
    ));
    let digest = "0".repeat(64);
    let ttl = number(recipe, "ttl", 60);
    let one = BigInt::from(1);
    let size = BigInt::from(17);
    match text(recipe,"action","") {
 "pick_claimable"=> {let num=recipe.get("number").map(|_|number(recipe,"number",0));let skip=recipe.get("skip").map(|v|v.as_array().expect("skip").iter().map(|v|v.as_str().expect("skip").to_owned()).collect::<Vec<_>>()).unwrap_or_default();Ok(json!(repository.pick_claimable(project,num.as_ref(),None,text(recipe,"mode","agent"),&skip).await?.map(|id|id.0.to_string())))},
 "create_attempt"=> {
            let workflow=recipe.get("workflow_payload").map_or_else(||Some(document("{\"steps\":[]}")), |value|value.as_str().map(document));
            let id=repository.create_attempt(CreateAttempt{hypothesis_id:hypothesis,science_revision:&one,producer:&payload,token_hash:&[9;32],ttl_seconds:&ttl,principal:&principal,workflow:workflow.as_ref(),deadline_seconds:Some(&BigInt::from(600))}).await?;
            if flag(recipe,"read_after"){Ok(repository.get_attempt_by_id(id,false).await?.expect("new attempt").project(now))}else{Ok(json!(id.0.to_string()))}
        },
 "get_attempt"=>Ok(repository.get_attempt(project,&BigInt::from(i32::from(!missing)),&one,flag(recipe,"lock")).await?.map_or(Value::Null,|value|value.project(now))),
 "get_attempt_by_id"=>Ok(repository.get_attempt_by_id(attempt,flag(recipe,"lock")).await?.map_or(Value::Null,|value|value.project(now))),
 "list_attempts"=>Ok(Value::Array(repository.list_attempts(HypothesisId(id(if missing{0}else{11})),None,Some(&number(recipe,"limit",50))).await?.iter().map(|value|value.project(now)).collect())),
 "list_project_attempts"=> {let states=recipe.get("states").map(|v|v.as_array().expect("states").iter().map(|v|v.as_str().expect("state").to_owned()).collect::<Vec<_>>());let before=recipe.get("before").map(|_|AttemptId(id(target(recipe,"before",0))));Ok(Value::Array(repository.list_project_attempts(project,states.as_deref(),None,before,&number(recipe,"limit",50)).await?.iter().map(|value|value.project(now)).collect()))},
 "extend_lease"=> {repository.extend_lease(attempt,&ttl).await?;Ok(Value::Null)},
 "mark_running"=> {repository.mark_running(attempt).await?;Ok(Value::Null)},
 "end_lease"=> {repository.end_lease(attempt,"submitted").await?;Ok(Value::Null)},
 "move_attempt"=> {repository.move_attempt(attempt,"testing","evaluating").await?;Ok(Value::Null)},
 "reopen_attempt"=> {repository.reopen_attempt(attempt,"testing").await?;Ok(Value::Null)},
 "pin_track"=>Ok(repository.pin_track(hypothesis).await?.project(now)),
 "approved_project_fields"=>Ok(stored(&repository.approved_project_fields(hypothesis).await?)),
 "approved_control"=>Ok(repository.approved_control(hypothesis,981).await?.map_or(Value::Null,|value|json!({"id":value.id.as_utf8().expect("fixture control"),"revision":value.revision.as_utf8().expect("fixture control")}))),
 "create_upload"=>Ok(repository.create_upload(CreateUpload{attempt_id:AttemptId(id(21)),lease_generation:&one,token_hash:&[9;32],role:"result",backend:"s3",bucket:"fixture",key:text(recipe,"key","new-upload"),declared_size:&size,declared_sha256:&digest,media_type:"application/json",ttl_minutes:&number(recipe,"ttl",1),max_stream_seconds:&BigInt::from(60),job_id:flag(recipe,"job").then(||JobId(id(41))),interface:flag(recipe,"job").then_some("metrics"),transfer:"stream",multipart_upload_id:None,part_size:None,slot:recipe.get("slot").and_then(Value::as_str)}).await?.map_or(Value::Null,|value|value.project(now))),
 "count_open_uploads"=>Ok(json!(repository.count_open_uploads(attempt,None).await?)),
 "get_upload"=>Ok(repository.get_upload(upload,flag(recipe,"lock")).await?.map_or(Value::Null,|value|value.project(now))),
 "begin_receiving"=>Ok(json!(repository.begin_receiving(upload).await?)),
 "abandon_receiving"=> {repository.abandon_receiving(upload).await?;Ok(Value::Null)},
 "finish_upload"=> {repository.finish_upload(upload,"failed",Some(&payload),true).await?;Ok(Value::Null)},
 "object_deleted"=> {repository.object_deleted(upload).await?;Ok(Value::Null)},
 "record_urls"=> {repository.record_urls(upload,&ttl).await?;Ok(Value::Null)},
 "list_refused_uploads"=>Ok(Value::Array(repository.list_refused_uploads(JobId(id(if missing{0}else{41}))).await?.iter().map(|value|value.project(now)).collect())),
 "add_artifact"=> {let upload=repository.get_upload(upload,false).await?.ok_or(AttemptError::Invariant)?;Ok(repository.add_artifact(AddArtifact{project_id:ProjectId(id(2)),upload:&upload,size_bytes:&size,sha256:&digest,generation:Some("generation"),content_validated:Some(true)}).await?.project(now))},
 "list_artifacts"=>Ok(Value::Array(repository.list_artifacts(attempt).await?.iter().map(|value|value.project(now)).collect())),
 "get_artifact"=>Ok(repository.get_artifact(project,ArtifactId(id(61))).await?.map_or(Value::Null,|value|value.project(now))),
 "get_artifact_by_key"=>Ok(repository.get_artifact_by_key(project,"s3","fixture","artifact-existing").await?.map_or(Value::Null,|value|value.project(now))),
 "list_artifacts_by_role"=>Ok(Value::Array(repository.list_artifacts_by_role(attempt,"legacy role").await?.iter().map(|value|value.project(now)).collect())),
 "list_job_artifacts"=>Ok(Value::Array(repository.list_job_artifacts(JobId(id(if missing{0}else{41}))).await?.iter().map(|value|value.project(now)).collect())),
 "add_manifest"=>Ok(repository.add_manifest(attempt,text(recipe,"stage","agent"),&payload,&digest).await?.project(now)),
 "get_manifest"=>Ok(repository.get_manifest(AttemptId(id(22)),ManifestId(id(if missing{0}else{31}))).await?.map_or(Value::Null,|value|value.project(now))),
 "add_evidence"=>Ok(json!(repository.add_evidence(AddEvidence{project_id:ProjectId(id(2)),attempt_id:AttemptId(id(22)),stage:"tester",status:text(recipe,"status","completed"),content:&payload,sha256:&digest,manifest_id:Some(ManifestId(id(31))),principal:&principal}).await?.0.to_string())),
 "get_evidence_by_id"=>Ok(repository.get_evidence_by_id(AttemptId(id(22)),EvidenceId(id(if missing{0}else{32}))).await?.map_or(Value::Null,|(content,sha)|json!([stored(&content),sha]))),
 "get_evidence"=>Ok(repository.get_evidence(attempt,"tester").await?.map_or(Value::Null,|(id,content,sha)|json!([id.0.to_string(),stored(&content),sha]))),
 "record_failure"|"requeue_failed"|"is_claimant"=> {let attempt=repository.get_attempt_by_id(attempt,false).await?.ok_or(AttemptError::Invariant)?;match text(recipe,"action","") {"is_claimant"=>Ok(json!(is_claimant(&principal,&attempt))),"record_failure"=>Ok(json!(repository.record_failure(RecordFailure{project_id:ProjectId(id(2)),attempt:&attempt,stage:"agent",code:text(recipe,"code","fixture_failure"),reason:text(recipe,"reason","é reason"),details:&payload,log_refs:None,from_state:recipe.get("from_state").and_then(Value::as_str)}).await?.0.to_string())),_=>{repository.requeue_failed(RequeueFailed{attempt:&attempt,code:text(recipe,"code","fixture_failure"),reason:text(recipe,"reason","é reason"),details:&payload,log_refs:None}).await?;Ok(Value::Null)}}},
 "automatic_requeues"=>Ok(json!(repository.automatic_requeues(hypothesis).await?)),
 "list_failures"=> {let ids=if flag(recipe,"empty"){Vec::new()}else{vec![attempt,AttemptId(id(22))]};let mut value=serde_json::Map::new();for (id,failures) in repository.list_failures(&ids).await? {value.insert(id.0.to_string(),Value::Array(failures.iter().map(|value|value.project(now)).collect()));}Ok(Value::Object(value))},
 "last_failure_code"=>Ok(json!(repository.last_failure_code(attempt).await?)),
 _=>panic!("unknown controlled fixture")
 }
}
async fn configure(connection: &mut PgConnection, recipe: &Value) {
    if let Some(state) = recipe.get("state").and_then(Value::as_str) {
        sqlx::query("UPDATE attempts SET state=$1,lease_token_hash=CASE WHEN $2 IN ('claimed','running') THEN decode(repeat('07',32),'hex') END,lease_expires_at=CASE WHEN $3 IN ('claimed','running') THEN now()+interval '60 seconds' END WHERE id=$4").bind(state).bind(state).bind(state).bind(id(target(recipe,"target",21))).execute(&mut *connection).await.expect("fixture state");
    }
    if let Some(value) = recipe.get("approved_value").and_then(Value::as_str) {
        let key = if text(recipe, "action", "") == "approved_project_fields" {
            "project_fields"
        } else {
            "control"
        };
        let content =
            json!({key:serde_json::from_str::<Value>(value).expect("approved fixture JSON")});
        sqlx::query("INSERT INTO hypothesis_revisions(hypothesis_id,revision,content,science_revision,author_user,via_channel) VALUES($1,2,$2::jsonb,1,$3,'api')").bind(id(13)).bind(content.to_string()).bind(id(1)).execute(&mut *connection).await.expect("fixture revision");
        sqlx::query("UPDATE hypotheses SET revision=2,approved_revision=2 WHERE id=$1")
            .bind(id(13))
            .execute(&mut *connection)
            .await
            .expect("fixture approval");
    }
    if flag(recipe, "expire") {
        sqlx::query("UPDATE uploads SET expires_at=now()-interval '1 second',transfer=CASE WHEN $1 THEN 'single' ELSE 'stream' END WHERE id=$2").bind(flag(recipe,"direct")).bind(id(51)).execute(&mut *connection).await.expect("fixture expiry");
    }
    if flag(recipe, "refused") {
        sqlx::query("UPDATE uploads SET state='failed',completed_at=now(),refusal='[{\"code\":\"bad\",\"path\":\"/value\"}]'::jsonb WHERE id=$1").bind(id(52)).execute(&mut *connection).await.expect("fixture refusal");
    }
}
// Keep the complete fixed-column snapshot protocol together for source review.
#[allow(clippy::too_many_lines)]
async fn snapshot(connection: &mut PgConnection) -> Value {
    let mut values = serde_json::Map::new();
    let excluded = vec![
        "lease_expires_at",
        "started_at",
        "submitted_at",
        "finished_at",
        "expires_at",
        "urls_expire_at",
        "deadline",
        "updated_at",
        "receiving_since",
        "completed_at",
    ];
    for table in [
        "hypotheses",
        "attempts",
        "uploads",
        "artifacts",
        "manifests",
        "phase_outputs",
        "attempt_failures",
        "review_cases",
    ] {
        let query = format!("SELECT (to_jsonb(t) - $1::text[])::text FROM {table} t ORDER BY id");
        let rows = sqlx::query_scalar::<_, String>(&query)
            .bind(&excluded)
            .fetch_all(&mut *connection)
            .await
            .expect("fixed snapshot query");
        values.insert(
            table.into(),
            Value::Array(
                rows.iter()
                    .map(|v| serde_json::from_str(v).expect("snapshot JSON"))
                    .collect(),
            ),
        );
    }
    let rows=sqlx::query("SELECT id,extract(epoch FROM lease_expires_at-now())::text,started_at=now(),submitted_at=now(),finished_at=now(),extract(epoch FROM deadline-now())::text FROM attempts ORDER BY id").fetch_all(&mut *connection).await.expect("attempt clocks");
    values.insert(
        "attempt_clock_relations".into(),
        Value::Array(
            rows.iter()
                .map(|row| {
                    json!([
                        row.get::<Uuid, _>(0).to_string(),
                        row.get::<Option<String>, _>(1),
                        row.get::<Option<bool>, _>(2),
                        row.get::<Option<bool>, _>(3),
                        row.get::<Option<bool>, _>(4),
                        row.get::<Option<String>, _>(5)
                    ])
                })
                .collect(),
        ),
    );
    let rows=sqlx::query("SELECT id,extract(epoch FROM expires_at-now())::text,receiving_since=now(),completed_at=now(),extract(epoch FROM urls_expire_at-now())::text,object_pending_delete FROM uploads ORDER BY id").fetch_all(&mut *connection).await.expect("upload clocks");
    values.insert(
        "upload_clock_relations".into(),
        Value::Array(
            rows.iter()
                .map(|row| {
                    json!([
                        row.get::<Uuid, _>(0).to_string(),
                        row.get::<Option<String>, _>(1),
                        row.get::<Option<bool>, _>(2),
                        row.get::<Option<bool>, _>(3),
                        row.get::<Option<String>, _>(4),
                        row.get::<bool, _>(5)
                    ])
                })
                .collect(),
        ),
    );
    for (label, query) in [
        (
            "attempt_json_text",
            "SELECT id,producer::text,imported::text,workflow::text,claimed_at::text FROM attempts ORDER BY id",
        ),
        (
            "upload_json_text",
            "SELECT id,refusal::text,created_at::text FROM uploads ORDER BY id",
        ),
        (
            "manifest_json_text",
            "SELECT id,content::text,created_at::text FROM manifests ORDER BY id",
        ),
        (
            "evidence_json_text",
            "SELECT id,front_matter::text,created_at::text FROM phase_outputs ORDER BY id",
        ),
        (
            "failure_json_text",
            "SELECT id,details::text,log_refs::text,created_at::text FROM attempt_failures ORDER BY id",
        ),
        (
            "artifact_creation_text",
            "SELECT id,verified_at::text FROM artifacts ORDER BY id",
        ),
        (
            "review_creation_text",
            "SELECT id,opened_at::text FROM review_cases ORDER BY id",
        ),
    ] {
        let rows = sqlx::query(query)
            .fetch_all(&mut *connection)
            .await
            .expect("fixed text projection");
        values.insert(
            label.into(),
            Value::Array(
                rows.iter()
                    .map(|row| {
                        let mut values = vec![json!(row.get::<Uuid, _>(0).to_string())];
                        for index in 1..row.len() {
                            values.push(json!(row.get::<Option<String>, _>(index)));
                        }
                        Value::Array(values)
                    })
                    .collect(),
            ),
        );
    }
    Value::Object(values)
}
fn outcome(result: Result<Value, AttemptError>) -> Value {
    match result {
        Ok(value) => json!({"value":value}),
        Err(AttemptError::Database { sqlstate }) => json!({"sqlstate":sqlstate}),
        Err(error) => {
            json!({"error":match error {AttemptError::Invariant=>"Invariant",AttemptError::Conflict=>"Conflict",AttemptError::StaleLease=>"StaleLease",AttemptError::SourceIntegerLimit|AttemptError::TextNul=>"ValueError",AttemptError::SourceRecursion=>"RecursionError",AttemptError::Encoding=>"UnicodeEncodeError",AttemptError::ControlShape=>"TypeError",AttemptError::ControlKey=>"KeyError",_=>panic!("unexpected sanitized native error")}})
        }
    }
}
#[tokio::test]
#[ignore = "requires freshly Rust-migrated isolated PostgreSQL fixture"]
#[allow(
    clippy::too_many_lines,
    reason = "sequential source and native corpus checks include full transaction storage assertions"
)]
async fn actual_attempts_reference() {
    let url = owned_url();
    let reference: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/attempts/tests/fixtures/attempts_reference.json"
    ))
    .expect("frozen actual source corpus");
    let cases = reference["cases"].as_array().expect("cases");
    assert_eq!(reference["count"].as_u64(), Some(cases.len() as u64));
    assert!(cases.len() > 200, "nonvacuous full operation corpus");
    for recipe in cases {
        let mut connection = PgConnection::connect(&url)
            .await
            .expect("fixture connection");
        sqlx::query("SET TIME ZONE 'UTC'")
            .execute(&mut connection)
            .await
            .expect("fixture UTC profile");
        sqlx::query("BEGIN")
            .execute(&mut connection)
            .await
            .expect("fixture transaction");
        sqlx::raw_sql(include_str!("fixtures/seed.sql"))
            .execute(&mut connection)
            .await
            .expect("isolated fixture seed");
        configure(&mut connection, recipe).await;
        let now: Timestamp = sqlx::query_scalar("SELECT now()")
            .fetch_one(&mut connection)
            .await
            .expect("transaction clock");
        sqlx::query("SAVEPOINT action")
            .execute(&mut connection)
            .await
            .expect("savepoint");
        let initial = snapshot(&mut connection).await;
        let mut expected = recipe["outcome"].clone();
        let native = recipe["native_profile"].as_str();
        let mut observed = if native == Some("json-decode-refusal") {
            let payload = recipe["payload"].as_str().expect("JSON payload");
            assert!(matches!(
                payload,
                "NaN" | "Infinity" | "{\"text\":\"\\ud800\"}"
            ));
            assert!(json::decode(payload.as_bytes(), 9994).is_err());
            assert_eq!(expected["sqlstate"], "22P02");
            expected
                .as_object_mut()
                .expect("source outcome")
                .remove("sqlstate");
            expected["error"] = json!("JsonDecode");
            json!({"error":"JsonDecode"})
        } else {
            let result = invoke(
                &mut Repository::new(&mut connection, context()),
                recipe,
                now,
            )
            .await;
            if result.is_err() {
                sqlx::query("ROLLBACK TO SAVEPOINT action")
                    .execute(&mut connection)
                    .await
                    .expect("action rollback");
            }
            if native == Some("checked-integer") {
                let key = match recipe["action"].as_str().expect("action") {
                    "list_attempts" | "list_project_attempts" => "limit",
                    "pick_claimable" => "number",
                    "extend_lease" | "record_urls" | "create_attempt" | "create_upload" => "ttl",
                    _ => panic!("unknown checked integer recipe"),
                };
                let value = recipe[key].as_str().expect("integer input");
                if recipe["action"] == "create_upload" {
                    assert!(value.parse::<i32>().is_err());
                } else {
                    assert!(value.parse::<i64>().is_err());
                }
                assert!(matches!(
                    expected["sqlstate"].as_str(),
                    Some("22003" | "22008" | "42883")
                ));
                assert!(matches!(
                    result,
                    Err(AttemptError::Database { sqlstate: None })
                ));
                expected["sqlstate"] = Value::Null;
            } else {
                assert!(native.is_none(), "unknown native recipe");
            }
            outcome(result)
        };
        observed["storage"] = snapshot(&mut connection).await;
        if native.is_some() {
            assert_eq!(
                observed["storage"], initial,
                "native refusal changed raw storage"
            );
        }
        if recipe["name"] == "approved_control-rendering" {
            assert_eq!(recipe["action"], "approved_control");
            let control: Value =
                serde_json::from_str(recipe["approved_value"].as_str().expect("control JSON"))
                    .expect("authored control JSON");
            assert_eq!(
                control,
                json!({"id": [1, true, null], "revision": {"b": 2}})
            );
            // Source str() uses Python repr. Check this finite source observation directly,
            // then require complete equivalent values and exact native serde serialization.
            assert_eq!(
                expected["value"],
                json!({"id": "[1, True, None]", "revision": "{'b': 2}"})
            );
            for field in ["id", "revision"] {
                let rendered = observed["value"][field]
                    .as_str()
                    .expect("rendered control field");
                let decoded: Value = serde_json::from_str(rendered).expect("native control JSON");
                assert_eq!(decoded, control[field], "control {field} semantic value");
                let serialized =
                    serde_json::to_string(&control[field]).expect("control serialization");
                assert_eq!(rendered, serialized, "control {field} native formatting");
                expected["value"][field] = json!(serialized);
            }
            assert_eq!(
                observed["storage"], initial,
                "control projection changed raw storage"
            );
        }
        assert_eq!(
            observed,
            expected,
            "recipe {}",
            recipe["name"].as_str().expect("recipe label")
        );
        sqlx::query("ROLLBACK")
            .execute(&mut connection)
            .await
            .expect("fixture rollback");
    }
}

impl Project for Attempt {
    fn project(&self, now: Timestamp) -> Value {
        json!({"mode":self.mode().as_str(),
        "id":self.id.0.to_string(),
        "project_id":self.project_id.0.to_string(),
        "hypothesis_id":self.hypothesis_id.0.to_string(),
        "hypothesis_number":self.hypothesis_number,
        "sequence":self.sequence,
        "state":self.state.as_str(),
        "hypothesis_revision":self.hypothesis_revision,
        "science_revision":self.science_revision,
        "track_id":self.track_id.0.to_string(),
        "track_slug":self.track_slug,
        "producer":stored(&self.producer),
        "claimed_by_user":self.claimed_by_user.map(|v|v.0.to_string()),
        "claimed_by_service":self.claimed_by_service.map(|v|v.0.to_string()),
        "via_channel":self.via_channel,
        "via_client":self.via_client,
        "predecessor_id":self.predecessor_id.map(|v|v.0.to_string()),
        "lease_generation":self.lease_generation,
        "lease_token_hash":self.lease_token_hash.as_ref().map(|v|bytes(v)),
        "lease_expires_at":self.lease_expires_at.map(|v|timestamp(v,now,"lease_expires_at")),
        "claimed_at":timestamp(self.claimed_at,now,"claimed_at"),
        "started_at":self.started_at.map(|v|timestamp(v,now,"started_at")),
        "submitted_at":self.submitted_at.map(|v|timestamp(v,now,"submitted_at")),
        "finished_at":self.finished_at.map(|v|timestamp(v,now,"finished_at")),
        "origin":self.origin.as_str(),
        "source_ref":self.source_ref,
        "imported":stored(&self.imported),
        "workflow":stored(&self.workflow),
        "deadline":self.deadline.map(|v|timestamp(v,now,"deadline")),
        })
    }
}

impl Project for Upload {
    fn project(&self, now: Timestamp) -> Value {
        json!({
        "id":self.id.0.to_string(),
        "attempt_id":self.attempt_id.0.to_string(),
        "lease_generation":self.lease_generation,
        "token_hash":bytes(&self.token_hash),
        "role":self.role,
        "backend":self.backend,
        "bucket":self.bucket,
        "key":self.key,
        "declared_size":self.declared_size,
        "declared_sha256":self.declared_sha256,
        "media_type":self.media_type,
        "state":self.state.as_str(),
        "expires_at":timestamp(self.expires_at,now,"expires_at"),
        "job_id":self.job_id.map(|v|v.0.to_string()),
        "interface":self.interface,
        "transfer":self.transfer.as_str(),
        "multipart_upload_id":self.multipart_upload_id,
        "part_size":self.part_size,
        "urls_expire_at":self.urls_expire_at.map(|v|timestamp(v,now,"urls_expire_at")),
        })
    }
}

impl Project for Artifact {
    fn project(&self, now: Timestamp) -> Value {
        json!({
        "id":self.id.0.to_string(),
        "attempt_id":self.attempt_id.0.to_string(),
        "role":self.role,
        "backend":self.backend,
        "bucket":self.bucket,
        "key":self.key,
        "generation":self.generation,
        "size_bytes":self.size_bytes,
        "sha256":self.sha256,
        "media_type":self.media_type,
        "verified_at":timestamp(self.verified_at,now,"verified_at"),
        "job_id":self.job_id.map(|v|v.0.to_string()),
        "interface":self.interface,
        "content_validated":self.content_validated,
        "origin":self.origin.as_str(),
        "source_ref":self.source_ref,
        "uri":self.uri,
        })
    }
}

impl Project for Manifest {
    fn project(&self, now: Timestamp) -> Value {
        json!({
        "id":self.id.0.to_string(),
        "attempt_id":self.attempt_id.0.to_string(),
        "stage":self.stage.as_str(),
        "content":stored(&self.content),
        "sha256":self.sha256,
        "created_at":timestamp(self.created_at,now,"created_at"),
        })
    }
}

impl Project for Failure {
    fn project(&self, now: Timestamp) -> Value {
        json!({
        "id":self.id.0.to_string(),
        "stage":self.stage.as_str(),
        "code":self.code,
        "reason":self.reason,
        "details":stored(&self.details),
        "created_at":timestamp(self.created_at,now,"created_at"),
        "requeued":self.requeued,
        "log_refs":stored(&self.log_refs),
        })
    }
}

impl Project for RefusedUpload {
    fn project(&self, _now: Timestamp) -> Value {
        json!({
        "id":self.id.0.to_string(),
        "role":self.role,
        "key":self.key,
        "interface":self.interface,
        "refusal":stored(&self.refusal),
        })
    }
}

impl Project for TrackPin {
    fn project(&self, _now: Timestamp) -> Value {
        json!({
        "slug":self.slug,
        "state":self.state.as_str(),
        "producer":stored(&self.producer),
        "mode":self.mode.as_str(),
        "workflow":stored(&self.workflow),
        })
    }
}

#[tokio::test]
#[ignore = "requires freshly Rust-migrated isolated PostgreSQL fixture"]
// One ordered, two-connection witness preserves the actual lock/commit sequence.
#[allow(clippy::too_many_lines)]
async fn zz_actual_attempt_locking_reference() {
    let url = owned_url();
    let reference: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/attempts/tests/fixtures/attempts_reference.json"
    ))
    .expect("actual source concurrency");
    let mut control = PgConnection::connect(&url)
        .await
        .expect("fixture control connection");
    sqlx::raw_sql(include_str!("fixtures/seed.sql"))
        .execute(&mut control)
        .await
        .expect("isolated committed fixture");
    let mut first = PgConnection::connect(&url)
        .await
        .expect("fixture first connection");
    let mut second = PgConnection::connect(&url)
        .await
        .expect("fixture second connection");
    sqlx::query("BEGIN")
        .persistent(false)
        .execute(&mut first)
        .await
        .expect("first transaction");
    sqlx::query("BEGIN")
        .persistent(false)
        .execute(&mut second)
        .await
        .expect("second transaction");
    let picked = Repository::new(&mut first, context())
        .pick_claimable(ProjectId(id(2)), None, None, "agent", &[])
        .await
        .expect("first locked claim")
        .expect("first claim");
    let skipped = Repository::new(&mut second, context())
        .pick_claimable(ProjectId(id(2)), None, None, "agent", &[])
        .await
        .expect("skip locked claim")
        .expect("second claim");
    sqlx::query("ROLLBACK")
        .persistent(false)
        .execute(&mut first)
        .await
        .expect("rollback first");
    sqlx::query("ROLLBACK")
        .persistent(false)
        .execute(&mut second)
        .await
        .expect("rollback second");
    sqlx::query("BEGIN")
        .persistent(false)
        .execute(&mut first)
        .await
        .expect("upload transaction");
    sqlx::query("BEGIN")
        .persistent(false)
        .execute(&mut second)
        .await
        .expect("waiter transaction");
    Repository::new(&mut first, context())
        .get_upload(UploadId(id(51)), true)
        .await
        .expect("lock upload")
        .expect("owned upload");
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .persistent(false)
        .fetch_one(&mut second)
        .await
        .expect("waiter identity");
    let waiter = tokio::spawn(async move {
        let result = Repository::new(&mut second, context())
            .begin_receiving(UploadId(id(51)))
            .await;
        (second, result)
    });
    wait_for_native_lock(&mut control, pid).await;
    let received = Repository::new(&mut first, context())
        .begin_receiving(UploadId(id(51)))
        .await
        .expect("winning stream");
    sqlx::query("COMMIT")
        .persistent(false)
        .execute(&mut first)
        .await
        .expect("commit winner");
    let (mut second, result) = waiter.await.expect("waiter settlement");
    let other_received = result.expect("waiting stream conditional update");
    sqlx::query("COMMIT")
        .persistent(false)
        .execute(&mut second)
        .await
        .expect("commit waiter");
    sqlx::query("BEGIN")
        .persistent(false)
        .execute(&mut first)
        .await
        .expect("lease transaction");
    sqlx::query("BEGIN")
        .persistent(false)
        .execute(&mut second)
        .await
        .expect("lease waiter transaction");
    Repository::new(&mut first, context())
        .get_attempt_by_id(AttemptId(id(21)), true)
        .await
        .expect("lock attempt")
        .expect("owned attempt");
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .persistent(false)
        .fetch_one(&mut second)
        .await
        .expect("lease waiter identity");
    let waiter = tokio::spawn(async move {
        let result = Repository::new(&mut second, context())
            .end_lease(AttemptId(id(21)), "submitted")
            .await;
        (second, result)
    });
    wait_for_native_lock(&mut control, pid).await;
    Repository::new(&mut first, context())
        .end_lease(AttemptId(id(21)), "submitted")
        .await
        .expect("winning lease end");
    sqlx::query("COMMIT")
        .persistent(false)
        .execute(&mut first)
        .await
        .expect("commit lease winner");
    let (mut second, result) = waiter.await.expect("lease waiter settlement");
    assert!(
        matches!(result, Err(AttemptError::StaleLease)),
        "late lease transition rejected"
    );
    sqlx::query("ROLLBACK")
        .persistent(false)
        .execute(&mut second)
        .await
        .expect("rollback lease waiter");
    let observed = json!({"pick":[picked.0.to_string(),skipped.0.to_string()],"receiving":[received,other_received],"lease_loser":"StaleLease"});
    assert_eq!(observed, reference["concurrency"]);
}
async fn wait_for_native_lock(connection: &mut PgConnection, pid: i32) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let event: Option<String> =
                sqlx::query_scalar("SELECT wait_event_type FROM pg_stat_activity WHERE pid=$1")
                    .bind(pid)
                    .persistent(false)
                    .fetch_one(&mut *connection)
                    .await
                    .expect("owned session lock observation");
            if event.as_deref() == Some("Lock") {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("actual blocked PostgreSQL operation");
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
