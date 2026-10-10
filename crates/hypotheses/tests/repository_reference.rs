#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::{
    ids::{AttemptId, HypothesisId, ProjectId, ReviewCaseId, ServiceAccountId, TrackId, UserId},
    json::{self, Document},
    principal::{
        Channel, Principal, ServiceKind, ServicePrincipal, UserPrincipal, Via, all_scopes,
    },
};
use cannery_hypotheses::repo::*;
use num_bigint::BigInt;
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection};
use std::{collections::BTreeMap, error::Error};
use uuid::Uuid;
type Result<T> = std::result::Result<T, Box<dyn Error>>;
const CONTEXT: JsonContext = JsonContext {
    encode_nesting_budget: 80,
    decode_nesting_budget: 80,
};
struct Projection {
    aliases: BTreeMap<Uuid, String>,
}
impl Projection {
    fn new() -> Self {
        Self {
            aliases: [
                (1, "user"),
                (2, "project"),
                (3, "track"),
                (4, "service"),
                (5, "comment"),
                (6, "attempt"),
                (7, "report"),
                (8, "imported"),
            ]
            .into_iter()
            .map(|(id, name)| (Uuid::from_u128(id), name.to_owned()))
            .collect(),
        }
    }
    fn id(&self, id: Uuid) -> Value {
        json!(
            self.aliases
                .get(&id)
                .cloned()
                .unwrap_or_else(|| id.to_string())
        )
    }
}
fn text_value(v: &String) -> Result<Value> {
    Ok(json!(v.as_utf8().ok_or("fixture text invalid")?))
}
fn document(v: &Document) -> Result<Value> {
    Ok(serde_json::from_str(&json::encode_ascii_pretty(
        v,
        CONTEXT.encode_nesting_budget,
    )?)?)
}
fn opt<T>(v: Option<&T>, f: impl FnOnce(&T) -> Result<Value>) -> Result<Value> {
    v.map(f).transpose().map(|v| v.unwrap_or(Value::Null))
}
fn hypothesis(p: &Projection, r: &Hypothesis) -> Result<Value> {
    Ok(
        json!({"id":p.id(r.id.0),"project_id":p.id(r.project_id.0),"number":json!(r.number),"track_id":p.id(r.track_id.0),"track_slug":text_value(&r.track_slug)?,"track_mode":json!(r.track_mode.as_str()),"state":json!(r.state.as_str()),"revision":json!(r.revision),"approved_revision":json!(r.approved_revision),"title":text_value(&r.title)?,"created_by_user":r.created_by_user.map_or(Value::Null, |id|p.id(id.0)),"created_by_service":r.created_by_service.map_or(Value::Null, |id|p.id(id.0)),"created_at":json!("@transaction-time"),"updated_at":json!("@transaction-time"),"approved_at":r.approved_at.map_or(Value::Null, |_|json!("@transaction-time")),"origin":json!(r.origin.as_str()),"source_ref":opt(r.source_ref.as_ref(),text_value)?,"external_id":opt(r.external_id.as_ref(),text_value)?,"imported":opt(r.imported.as_ref(),document)?}),
    )
}
fn revision(p: &Projection, r: &Revision) -> Result<Value> {
    Ok(
        json!({"revision":json!(r.revision),"content":document(&r.content)?,"science_revision":json!(r.science_revision),"author_user":r.author_user.map_or(Value::Null, |id|p.id(id.0)),"author_service":r.author_service.map_or(Value::Null, |id|p.id(id.0)),"via_channel":text_value(&r.via_channel.as_text())?,"via_client":opt(r.via_client.as_ref(),text_value)?,"created_at":json!("@transaction-time"),"origin":json!(r.origin.as_str())}),
    )
}
fn link(p: &Projection, r: &Link) -> Result<Value> {
    Ok(
        json!({"kind":json!(match r.kind{LinkKind::Mention=>"mention",LinkKind::Relation(k)=>k.as_str()}),"hypothesis_id":p.id(r.hypothesis_id.0),"project_id":p.id(r.project_id.0),"project_slug":text_value(&r.project_slug)?,"number":json!(r.number),"title":text_value(&r.title)?,"state":json!(r.state.as_str())}),
    )
}
fn reviewcase(p: &Projection, r: &ReviewCase) -> Result<Value> {
    Ok(
        json!({"id":p.id(r.id.0),"kind":json!(r.kind.as_str()),"subject_revision":json!(r.subject_revision),"state":json!(r.state.as_str()),"opened_at":json!("@transaction-time"),"resolved_at":r.resolved_at.map_or(Value::Null, |_|json!("@transaction-time")),"origin":json!(r.origin.as_str()),"source_ref":opt(r.source_ref.as_ref(),text_value)?}),
    )
}
fn decision(p: &Projection, r: &Decision) -> Result<Value> {
    Ok(
        json!({"id":p.id(r.id.0),"review_case_id":p.id(r.review_case_id.0),"action":json!(r.action.as_str()),"subject_revision":json!(r.subject_revision),"reason":text_value(&r.reason)?,"actor_user_id":p.id(r.actor_user_id.0),"via_channel":text_value(&r.via_channel.as_text())?,"via_client":opt(r.via_client.as_ref(),text_value)?,"decided_at":json!("@transaction-time"),"supersedes":r.supersedes.map_or(Value::Null, |id|p.id(id.0)),"origin":json!(r.origin.as_str()),"source_ref":opt(r.source_ref.as_ref(),text_value)?}),
    )
}
const USER: UserId = UserId(Uuid::from_u128(1));
const PROJECT: ProjectId = ProjectId(Uuid::from_u128(2));
const TRACK: TrackId = TrackId(Uuid::from_u128(3));
const SERVICE: ServiceAccountId = ServiceAccountId(Uuid::from_u128(4));
const MISSING: HypothesisId = HypothesisId(Uuid::nil());
fn user() -> UserPrincipal {
    UserPrincipal {
        user_id: USER,
        email: None,
        display_name: None,
        is_admin: false,
        via: Via {
            channel: Channel::Api,
            client: Some("fixture".into()),
        },
        scopes: all_scopes(),
        session_id: None,
        csrf_token: None,
    }
}
fn service() -> Principal {
    Principal::Service(ServicePrincipal {
        service_account_id: SERVICE,
        project_id: PROJECT,
        kind: ServiceKind::Agent,
        name: "fixture".into(),
        via: Via {
            channel: Channel::Cli,
            client: None,
        },
        scopes: all_scopes(),
    })
}
fn python(s: &str) -> String {
    String::from(s)
}
fn many<T>(rows: &[T], projection: impl Fn(&T) -> Result<Value>) -> Result<Value> {
    rows.iter()
        .map(projection)
        .collect::<Result<Vec<_>>>()
        .map(Value::Array)
}
// Hypotheses come from plan approval, outside this crate; the fixture writes
// the rows an approved plan would.
async fn insert_hypothesis(
    c: &mut PgConnection,
    number: i32,
    title: &str,
    principal: &Principal,
) -> Result<HypothesisId> {
    let (user, service) = match principal {
        Principal::User(u) => (Some(u.user_id), None),
        Principal::Service(s) => (None, Some(s.service_account_id)),
    };
    Ok(sqlx::query_scalar("INSERT INTO hypotheses(project_id,number,track_id,state,revision,approved_revision,title,created_by_user,created_by_service,approved_at) VALUES($1,$2,$3,'queued',1,1,$4,$5,$6,now()) RETURNING id")
        .bind(PROJECT).bind(number).bind(TRACK).bind(title).bind(user).bind(service)
        .fetch_one(&mut *c).await?)
}
async fn insert_revision(
    c: &mut PgConnection,
    hypothesis: HypothesisId,
    revision: i32,
    content: &str,
    science_revision: i32,
    principal: &Principal,
) -> Result<()> {
    let (user, service) = match principal {
        Principal::User(u) => (Some(u.user_id), None),
        Principal::Service(s) => (None, Some(s.service_account_id)),
    };
    let via = match principal {
        Principal::User(_) => "api",
        Principal::Service(_) => "cli",
    };
    sqlx::query("INSERT INTO hypothesis_revisions(hypothesis_id,revision,content,science_revision,author_user,author_service,via_channel,via_client) VALUES($1,$2,$3::jsonb,$4,$5,$6,$7,$8)")
        .bind(hypothesis).bind(revision).bind(content).bind(science_revision).bind(user).bind(service).bind(via)
        .bind(matches!(principal, Principal::User(_)).then_some("fixture"))
        .execute(&mut *c).await?;
    Ok(())
}
// A result case on an attempt of `hypothesis`, as verification opens it.
async fn insert_result_case(
    c: &mut PgConnection,
    hypothesis: HypothesisId,
) -> Result<ReviewCaseId> {
    let attempt = Uuid::from_u128(11);
    let evidence = Uuid::from_u128(12);
    sqlx::query("INSERT INTO attempts(id,project_id,hypothesis_id,sequence,state,hypothesis_revision,science_revision,track_id,claimed_by_user,via_channel,lease_generation) VALUES($1,$2,$3,1,'verified',1,1,$4,$5,'api',0)").bind(attempt).bind(PROJECT).bind(hypothesis).bind(TRACK).bind(USER).execute(&mut *c).await?;
    sqlx::query("INSERT INTO phase_outputs(id,project_id,attempt_id,stage,status,front_matter,sha256,producer_user,via_channel) VALUES($1,$2,$3,'verification','completed','{}','fixture',$4,'api')").bind(evidence).bind(PROJECT).bind(attempt).bind(USER).execute(&mut *c).await?;
    Ok(ReviewCaseId(sqlx::query_scalar("INSERT INTO review_cases(project_id,hypothesis_id,attempt_id,kind,subject_revision,evidence_id) VALUES($1,$2,$3,'decision',1,$4) RETURNING id").bind(PROJECT).bind(hypothesis).bind(attempt).bind(evidence).fetch_one(&mut *c).await?))
}
async fn seed(c: &mut PgConnection) -> Result<()> {
    sqlx::query("SET plan_cache_mode=force_generic_plan")
        .execute(&mut *c)
        .await?;
    sqlx::query(
        "INSERT INTO users(id,issuer,subject) VALUES($1,'https://hypotheses.invalid','fixture')",
    )
    .bind(USER)
    .execute(&mut *c)
    .await?;
    sqlx::query("INSERT INTO projects(id,slug,title,created_by) VALUES($1,'project','Fixture',$2)")
        .bind(PROJECT)
        .bind(USER)
        .execute(&mut *c)
        .await?;
    sqlx::query(
        "INSERT INTO tracks(id,project_id,slug,title,created_by) VALUES($1,$2,'track','Track',$3)",
    )
    .bind(TRACK)
    .bind(PROJECT)
    .bind(USER)
    .execute(&mut *c)
    .await?;
    sqlx::query("INSERT INTO service_accounts(id,project_id,kind,name,created_by) VALUES($1,$2,'agent','fixture',$3)").bind(SERVICE).bind(PROJECT).bind(USER).execute(&mut *c).await?;
    Ok(())
}
// Snapshot exact PostgreSQL row text; numeric representation normalization never touches storage.
async fn raw_storage(connection: &mut PgConnection) -> Result<Value> {
    let mut result = serde_json::Map::new();
    for table in [
        "projects",
        "hypotheses",
        "hypothesis_revisions",
        "hypothesis_relations",
        "mentions",
        "review_cases",
        "decisions",
        "idempotency_keys",
    ] {
        let query =
            format!("SELECT row_to_json(t)::text FROM {table} t ORDER BY row_to_json(t)::text");
        let rows: Vec<String> = sqlx::query_scalar(&query)
            .fetch_all(&mut *connection)
            .await?;
        result.insert(table.into(), json!(rows));
    }
    Ok(Value::Object(result))
}

fn source_float_tokens(value: &mut Value) -> Result<()> {
    match value {
        Value::Number(number) if number.to_string().contains(['.', 'e', 'E']) => {
            // Source JSON emitted these as binary floats. Integer tokens stay arbitrary precision.
            let float: f64 = serde_json::from_str(&number.to_string())?;
            *number = serde_json::Number::from_f64(float).ok_or("nonfinite source float")?;
        }
        Value::Array(values) => {
            for value in values {
                source_float_tokens(value)?;
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                source_float_tokens(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn require_native_error(recipe: &Value, error: &HypothesisError) {
    let valid = match recipe["native_error"].as_str() {
        Some("integer_binding") => matches!(error, HypothesisError::IntegerBinding),
        Some("integer_encoding") => matches!(
            error,
            HypothesisError::Encode(json::EncodeError::IntegerLimit)
        ),
        Some("json_syntax") => matches!(
            error,
            HypothesisError::Decode(json::DecodeError::Syntax { .. })
        ),
        _ => false,
    };
    assert!(
        valid,
        "recipe {}: wrong native refusal {error:?}",
        recipe["name"]
    );
}

// One sequential scenario preserves the source transaction and observation order.
#[allow(clippy::too_many_lines, clippy::many_single_char_names)]
async fn scenario(c: &mut PgConnection, recipes: &[Value]) -> Result<Value> {
    seed(c).await?;
    let zero = BigInt::from(0);
    let mut p = Projection::new();
    let human = Principal::User(user());
    let agent = service();
    let mut out = serde_json::Map::new();
    let n = next_number(c, PROJECT).await?;
    let first = insert_hypothesis(c, n, "First é😀", &human).await?;
    p.aliases.insert(first.0, "first".into());
    let second_number = next_number(c, PROJECT).await?;
    let second = insert_hypothesis(c, second_number, "Second", &agent).await?;
    p.aliases.insert(second.0, "second".into());
    out.insert(
        "numbers".into(),
        json!([
            n,
            get_hypothesis_by_id(c, second, CONTEXT)
                .await?
                .ok_or("missing second")?
                .number
        ]),
    );
    let h = get_hypothesis(c, PROJECT, &1.into(), false, CONTEXT)
        .await?
        .ok_or("missing first")?;
    let h2 = get_hypothesis_by_id(c, second, CONTEXT)
        .await?
        .ok_or("missing second")?;
    out.insert(
        "created".into(),
        json!([hypothesis(&p, &h)?, hypothesis(&p, &h2)?]),
    );
    out.insert(
        "missing".into(),
        json!([
            get_hypothesis(c, PROJECT, &zero, false, CONTEXT)
                .await?
                .is_some()
                .then_some(true),
            get_hypothesis(c, PROJECT, &zero, true, CONTEXT)
                .await?
                .is_some()
                .then_some(true),
            get_hypothesis_by_id(c, MISSING, CONTEXT)
                .await?
                .is_some()
                .then_some(true),
            get_revision(c, first, &1.into(), CONTEXT)
                .await?
                .is_some()
                .then_some(true),
            pending_case(c, first, CaseKind::Decision, CONTEXT)
                .await?
                .is_some()
                .then_some(true)
        ]),
    );
    insert_revision(
        c,
        first,
        1,
        r#"{"title":"First é😀","negative":-0.0,"float":1.0,"tiny":1e-7}"#,
        1,
        &human,
    )
    .await?;
    let bumped: i32 = sqlx::query_scalar(
        "UPDATE hypotheses SET revision=revision+1,title='Changed',updated_at=now() WHERE id=$1 RETURNING revision",
    )
    .bind(first)
    .fetch_one(&mut *c)
    .await?;
    out.insert("bumped".into(), json!(bumped));
    insert_revision(c, first, 2, r#"{"title":"Changed"}"#, 5, &agent).await?;
    out.insert(
        "revision".into(),
        revision(
            &p,
            &get_revision(c, first, &1.into(), CONTEXT)
                .await?
                .ok_or("missing revision")?,
        )?,
    );
    out.insert(
        "revisions".into(),
        many(&list_revisions(c, first, None, None, CONTEXT).await?, |r| {
            revision(&p, r)
        })?,
    );
    out.insert(
        "revisions-after".into(),
        many(
            &list_revisions(c, first, Some(&1.into()), Some(&1.into()), CONTEXT).await?,
            |r| revision(&p, r),
        )?,
    );
    out.insert(
        "revisions-zero".into(),
        many(
            &list_revisions(c, first, None, Some(&zero), CONTEXT).await?,
            |r| revision(&p, r),
        )?,
    );
    set_state(c, first, HypothesisState::Queued, Some(&2.into())).await?;
    let approved = get_hypothesis(c, PROJECT, &1.into(), true, CONTEXT)
        .await?
        .ok_or("missing approved")?;
    set_state(c, first, HypothesisState::Active, None).await?;
    let current = get_hypothesis_by_id(c, first, CONTEXT)
        .await?
        .ok_or("missing current")?;
    out.insert(
        "approval-preserved".into(),
        json!(approved.approved_at == current.approved_at && current.approved_revision == Some(2)),
    );
    out.insert("state".into(), hypothesis(&p, &current)?);
    for (name, states, track, before, limit) in [
        ("all", None, None, None, 10),
        ("empty", Some(vec![]), None, None, 10),
        (
            "active",
            Some(vec![HypothesisState::Active]),
            None,
            None,
            10,
        ),
        ("track", None, Some("track"), None, 10),
        ("wrong-track", None, Some("missing"), None, 10),
        ("before", None, None, Some(2), 10),
        ("limit", None, None, None, 1),
        ("zero", None, None, None, 0),
    ] {
        let track = track.map(python);
        let before = before.map(BigInt::from);
        out.insert(
            format!("list-{name}"),
            many(
                &list_hypotheses(
                    c,
                    PROJECT,
                    ListHypotheses {
                        states: states.as_deref(),
                        track_slug: track.as_ref(),
                        before: before.as_ref(),
                        limit: &limit.into(),
                    },
                    CONTEXT,
                )
                .await?,
                |r| hypothesis(&p, r),
            )?,
        );
    }
    replace_relations(
        c,
        first,
        [
            (RelationKind::Supersedes, second),
            (RelationKind::RelatedTo, second),
            (RelationKind::DerivedFrom, second),
            (RelationKind::RelatedTo, second),
        ],
    )
    .await?;
    replace_mentions(c, MentionSource::Hypothesis(second), [first, first]).await?;
    replace_mentions(c, MentionSource::Hypothesis(first), [first]).await?;
    sqlx::query("INSERT INTO comments(id,project_id,hypothesis_id,author_user,body_markdown) VALUES($1,$2,$3,$4,'Fixture')").bind(Uuid::from_u128(5)).bind(PROJECT).bind(second).bind(USER).execute(&mut *c).await?;
    sqlx::query("INSERT INTO attempts(id,project_id,hypothesis_id,sequence,state,hypothesis_revision,science_revision,track_id,claimed_by_user,via_channel,lease_generation) VALUES($1,$2,$3,1,'cancelled',1,1,$4,$5,'api',0)").bind(Uuid::from_u128(6)).bind(PROJECT).bind(second).bind(TRACK).bind(USER).execute(&mut *c).await?;
    sqlx::query("INSERT INTO phase_outputs(id,project_id,attempt_id,stage,status,front_matter,sha256,producer_user,via_channel) VALUES($1,$2,$3,'agent','completed','{}','fixture',$4,'api')").bind(Uuid::from_u128(7)).bind(PROJECT).bind(Uuid::from_u128(6)).bind(USER).execute(&mut *c).await?;
    for source in [
        MentionSource::Comment(CommentId(Uuid::from_u128(5))),
        MentionSource::Report(EvidenceId(Uuid::from_u128(7))),
        MentionSource::Attempt(AttemptId(Uuid::from_u128(6))),
    ] {
        replace_mentions(c, source, [first]).await?;
    }
    out.insert(
        "outgoing".into(),
        many(&outgoing_relations(c, first, CONTEXT).await?, |r| {
            link(&p, r)
        })?,
    );
    out.insert(
        "backlinks-first".into(),
        many(&backlinks(c, first, CONTEXT).await?, |r| link(&p, r))?,
    );
    out.insert(
        "backlinks-second".into(),
        many(&backlinks(c, second, CONTEXT).await?, |r| link(&p, r))?,
    );
    let refs = resolve_refs(
        c,
        [
            (PROJECT, 1.into()),
            (PROJECT, 2.into()),
            (PROJECT, 1.into()),
            (PROJECT, 99.into()),
            (ProjectId(Uuid::nil()), 1.into()),
        ],
    )
    .await?;
    out.insert(
        "refs".into(),
        json!(
            refs.into_iter()
                .map(|((project, n), h)| json!([p.id(project.0), n, p.id(h.0)]))
                .collect::<Vec<_>>()
        ),
    );
    out.insert("refs-empty".into(), json!({}));
    assert!(resolve_refs(c, Vec::new()).await?.is_empty());
    let opened = pending_case(c, first, CaseKind::Decision, CONTEXT)
        .await?
        .is_some()
        .then_some(true);
    out.insert("opened-before".into(), json!(opened));
    let case = insert_result_case(c, first).await?;
    p.aliases.insert(case.0, "case".into());
    p.aliases
        .insert(Uuid::from_u128(11), "first-attempt".into());
    p.aliases
        .insert(Uuid::from_u128(12), "first-verification".into());
    let opened = pending_case(c, first, CaseKind::Decision, CONTEXT)
        .await?
        .ok_or("missing pending")?;
    out.insert("pending".into(), reviewcase(&p, &opened)?);
    let decided = record_decision(
        c,
        RecordDecision {
            case_id: opened.id,
            action: DecisionAction::Inconclusive,
            subject_revision: &1.into(),
            reason: &python("Not enough data"),
            principal: &user(),
            supersedes: None,
            document: None,
        },
        CONTEXT,
    )
    .await?;
    p.aliases.insert(decided.id.0, "decision".into());
    out.insert("decision".into(), decision(&p, &decided)?);
    let before_case = list_cases(c, first, CONTEXT).await?.remove(0);
    let corrected = record_decision(
        c,
        RecordDecision {
            case_id: opened.id,
            action: DecisionAction::Reject,
            subject_revision: &1.into(),
            reason: &python("Correction"),
            principal: &user(),
            supersedes: Some(decided.id),
            document: None,
        },
        CONTEXT,
    )
    .await?;
    p.aliases.insert(corrected.id.0, "correction".into());
    out.insert("correction".into(), decision(&p, &corrected)?);
    let cases = list_cases(c, first, CONTEXT).await?;
    out.insert(
        "resolved-preserved".into(),
        json!(before_case.resolved_at == cases[0].resolved_at),
    );
    out.insert("cases".into(), many(&cases, |r| reviewcase(&p, r))?);
    out.insert(
        "pending-resolved".into(),
        json!(
            pending_case(c, first, CaseKind::Decision, CONTEXT)
                .await?
                .is_some()
                .then_some(true)
        ),
    );
    out.insert(
        "decisions".into(),
        many(
            &list_decisions(c, &[opened.id, opened.id], CONTEXT).await?,
            |r| decision(&p, r),
        )?,
    );
    out.insert(
        "decisions-empty".into(),
        many(&list_decisions(c, &[], CONTEXT).await?, |r| decision(&p, r))?,
    );
    {
        let mut tx = c.begin().await?;
        out.insert(
            "rollback-number".into(),
            json!(next_number(&mut tx, PROJECT).await?),
        );
        replace_relations(&mut tx, first, []).await?;
        replace_mentions(&mut tx, MentionSource::Hypothesis(second), []).await?;
        tx.rollback().await?;
    }
    out.insert(
        "after-rollback-number".into(),
        json!(next_number(c, PROJECT).await?),
    );
    out.insert(
        "after-rollback-outgoing".into(),
        many(&outgoing_relations(c, first, CONTEXT).await?, |r| {
            link(&p, r)
        })?,
    );
    let mut imported = Vec::new();
    for (i, shape) in [
        "{}",
        r#"{"null":null}"#,
        r#"{"list":[]}"#,
        r#"{"decimal":1.0}"#,
    ]
    .into_iter()
    .enumerate()
    {
        let ident = HypothesisId(Uuid::from_u128(100 + u128::try_from(i)?));
        p.aliases.insert(ident.0, format!("imported-{i}"));
        let index = i32::try_from(i)?;
        sqlx::query("INSERT INTO hypotheses(id,project_id,number,track_id,title,created_by_user,origin,source_ref,external_id,imported,created_at,updated_at,state,approved_revision,approved_at) VALUES($1,$2,$3,$4,'Imported',$5,'imported','fixture','historical-'||$6::text,$7::jsonb,'2001-02-03T04:05:06.123456Z','2001-02-03T04:05:06.123456Z','queued',1,'2001-02-03T04:05:06.123456Z')").bind(ident).bind(PROJECT).bind(100+index).bind(TRACK).bind(USER).bind(index.to_string()).bind(shape).execute(&mut *c).await?;
        let row = get_hypothesis_by_id(c, ident, CONTEXT)
            .await?
            .ok_or("missing imported")?;
        assert_eq!(
            row.created_at.0.to_rfc3339(),
            "2001-02-03T04:05:06.123456+00:00"
        );
        imported.push(hypothesis(&p, &row)?);
    }
    out.insert("imported".into(), json!(imported));
    let historical = HypothesisId(Uuid::from_u128(100));
    sqlx::query("INSERT INTO hypothesis_revisions(hypothesis_id,revision,content,science_revision,author_user,via_channel,origin,source_ref) VALUES($1,1,'{}'::jsonb,1,$2,'historical','imported','fixture')")
        .bind(historical).bind(USER).execute(&mut *c).await?;
    out.insert(
        "historical-channel".into(),
        opt(
            get_revision(c, historical, &1.into(), CONTEXT)
                .await?
                .as_ref(),
            |row| revision(&p, row),
        )?,
    );
    let mut results = Vec::new();
    for recipe in recipes {
        if recipe["supplemental"].as_bool() == Some(true) {
            continue;
        }
        let before = raw_storage(c).await?;
        let mut tx = c.begin().await?;
        let value = observe_recipe(&mut tx, recipe, first, opened.id, &p).await;
        if recipe["native_error"].is_string() {
            let Err(error) = &value else {
                return Err("unsupported input accepted".into());
            };
            require_native_error(recipe, error);
            assert_eq!(
                raw_storage(&mut tx).await?,
                before,
                "refusal {} mutated storage",
                recipe["name"]
            );
        }
        tx.rollback().await?;
        assert_eq!(
            raw_storage(c).await?,
            before,
            "recipe {} rollback changed storage",
            recipe["name"]
        );
        results.push(match value{Ok(v)=>json!({"name":recipe["name"],"value":v}),Err(e)=>json!({"name":recipe["name"],"error":match &e{HypothesisError::Invariant=>"invariant",HypothesisError::Database{sqlstate:Some(_)}=>"server",HypothesisError::Database{sqlstate:None}|HypothesisError::IntegerBinding|HypothesisError::IntegerArrayRendering=>"driver",_=>"encode"},"sqlstate":e.sqlstate()})});
    }
    out.insert("recipes".into(), json!(results));
    out.insert(
        "delete-insert-rollback".into(),
        many(&outgoing_relations(c, first, CONTEXT).await?, |r| {
            link(&p, r)
        })?,
    );
    Ok(Value::Object(out))
}
// A single dispatch mirrors the frozen source recipes without changing failures.
#[allow(clippy::too_many_lines)]
async fn observe_recipe(
    c: &mut PgConnection,
    r: &Value,
    hid: HypothesisId,
    cid: ReviewCaseId,
    p: &Projection,
) -> std::result::Result<Value, HypothesisError> {
    let action = r["action"].as_str().ok_or(HypothesisError::CorruptData)?;
    let mut n = if let Some(power) = r["power"].as_u64() {
        BigInt::from(10).pow(u32::try_from(power).map_err(|_| HypothesisError::CorruptData)?)
    } else {
        r["number"]
            .as_str()
            .unwrap_or("1")
            .parse::<BigInt>()
            .map_err(|_| HypothesisError::CorruptData)?
    };
    if r["negative"].as_bool() == Some(true) {
        n = -n;
    }
    let projection_error = |_: Box<dyn Error>| HypothesisError::CorruptData;
    match action {
        "get" | "get-lock" => opt(
            get_hypothesis(c, PROJECT, &n, action == "get-lock", CONTEXT)
                .await?
                .as_ref(),
            |row| hypothesis(p, row),
        )
        .map_err(projection_error),
        "revision" => opt(get_revision(c, hid, &n, CONTEXT).await?.as_ref(), |row| {
            revision(p, row)
        })
        .map_err(projection_error),
        "list-before" => many(
            &list_hypotheses(
                c,
                PROJECT,
                ListHypotheses {
                    states: None,
                    track_slug: None,
                    before: Some(&n),
                    limit: &10.into(),
                },
                CONTEXT,
            )
            .await?,
            |row| hypothesis(p, row),
        )
        .map_err(projection_error),
        "revisions-after" => many(
            &list_revisions(c, hid, Some(&n), None, CONTEXT).await?,
            |row| revision(p, row),
        )
        .map_err(projection_error),
        "refs" => Ok(json!(
            resolve_refs(c, [(PROJECT, n)])
                .await?
                .into_iter()
                .map(|((project, no), h)| json!([project.to_string(), no, h.to_string()]))
                .collect::<Vec<_>>()
        )),
        "approve" | "state-missing" => {
            set_state(
                c,
                if action == "state-missing" {
                    MISSING
                } else {
                    hid
                },
                HypothesisState::Queued,
                if action == "approve" { Some(&n) } else { None },
            )
            .await?;
            Ok(Value::Null)
        }
        "decision" | "decision-fk" | "decision-blank" => decision(
            p,
            &record_decision(
                c,
                RecordDecision {
                    case_id: if action == "decision-fk" {
                        ReviewCaseId(Uuid::nil())
                    } else {
                        cid
                    },
                    action: DecisionAction::Promote,
                    subject_revision: &n,
                    reason: &python(if action == "decision-blank" {
                        " "
                    } else {
                        "Fixture"
                    }),
                    principal: &user(),
                    supersedes: None,
                    document: None,
                },
                CONTEXT,
            )
            .await?,
        )
        .map_err(projection_error),
        "next-missing" => Ok(json!(next_number(c, ProjectId(Uuid::nil())).await?)),
        "next-overflow" => {
            sqlx::query("UPDATE projects SET next_hypothesis_number=2147483647 WHERE id=$1")
                .bind(PROJECT)
                .execute(&mut *c)
                .await?;
            Ok(json!(next_number(c, PROJECT).await?))
        }
        "relations-self" | "relations-fk" => {
            replace_relations(
                c,
                hid,
                [(
                    RelationKind::RelatedTo,
                    if action == "relations-fk" {
                        MISSING
                    } else {
                        hid
                    },
                )],
            )
            .await?;
            Ok(Value::Null)
        }
        "mentions-fk" => {
            replace_mentions(c, MentionSource::Hypothesis(hid), [MISSING]).await?;
            Ok(Value::Null)
        }
        "list-negative" => many(
            &list_hypotheses(
                c,
                PROJECT,
                ListHypotheses {
                    states: None,
                    track_slug: None,
                    before: None,
                    limit: &(-1).into(),
                },
                CONTEXT,
            )
            .await?,
            |row| hypothesis(p, row),
        )
        .map_err(projection_error),
        "revisions-negative" => many(
            &list_revisions(c, hid, None, Some(&(-1).into()), CONTEXT).await?,
            |row| revision(p, row),
        )
        .map_err(projection_error),
        _ => Err(HypothesisError::CorruptData),
    }
}

#[tokio::test]
#[ignore = "requires guarded isolated Rust-migrated fixture database"]
async fn repository_matches_frozen_python() -> Result<()> {
    let url = std::env::var("HYPOTHESES_NATIVE_DATABASE_URL")?;
    if !url.contains("dbname=hypotheses_fixture_native_")
        && !url.contains("/hypotheses_fixture_native_")
    {
        return Err("unguarded native database".into());
    }
    let mut c = PgConnection::connect(&url).await?;
    let f: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/hypotheses/tests/fixtures/hypotheses_repository_reference.json"
    ))?;
    let recipes = f["recipes"].as_array().ok_or("recipes absent")?;
    let mut expected = f["observations"].clone();
    expected["recipes"]
        .as_array_mut()
        .ok_or("recipes absent")?
        .retain(|v| {
            !recipes
                .iter()
                .any(|r| r["name"] == v["name"] && r["supplemental"].as_bool() == Some(true))
        });
    for record in expected["recipes"].as_array_mut().ok_or("recipes absent")? {
        let recipe = recipes
            .iter()
            .find(|recipe| recipe["name"] == record["name"])
            .ok_or("recipe absent")?;
        if let Some(kind) = recipe["native_error"].as_str() {
            *record = json!({"name": recipe["name"], "error": if kind == "integer_binding" { "driver" } else { "encode" }, "sqlstate": null});
        }
    }
    expected["warmup"] = f["native_warmup"].clone();
    // The server version string depends on the PostgreSQL build, not on the code.
    expected
        .as_object_mut()
        .ok_or("observations absent")?
        .remove("server-version");
    source_float_tokens(&mut expected)?;
    let mut observed = scenario(&mut c, recipes).await?;
    observed["concurrency"] = concurrency(&url).await?;
    observed["warmup"] = warmup(&url).await?;
    for (key, actual) in observed.as_object().ok_or("observations absent")? {
        if key == "recipes" {
            for (native, source) in actual
                .as_array()
                .ok_or("recipes absent")?
                .iter()
                .zip(expected[key].as_array().ok_or("recipes absent")?)
            {
                assert_eq!(native, source, "recipe {}", native["name"]);
            }
        }
        assert_eq!(actual, &expected[key], "observation {key}");
    }
    assert_eq!(observed, expected);
    Ok(())
}
async fn require_blocked(connection: &mut PgConnection, pid: i32) -> Result<bool> {
    for _ in 0..100 {
        let blocked: Option<bool> =
            sqlx::query_scalar("SELECT wait_event_type='Lock' FROM pg_stat_activity WHERE pid=$1")
                .bind(pid)
                .fetch_optional(&mut *connection)
                .await?
                .flatten();
        if blocked == Some(true) {
            return Ok(true);
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    Err("fixture concurrency did not block".into())
}
async fn concurrency(url: &str) -> Result<Value> {
    let mut first = PgConnection::connect(url).await?;
    let mut second = PgConnection::connect(url).await?;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut second)
        .await?;
    let original = get_hypothesis(&mut first, PROJECT, &1.into(), false, CONTEXT)
        .await?
        .ok_or("missing concurrency hypothesis")?;
    let alternate = TrackId(Uuid::from_u128(9));
    sqlx::query("INSERT INTO tracks(id,project_id,slug,title,created_by,mode,workflow) VALUES($1,$2,'alternate','Alternate',$3,'workflow','{}')").bind(alternate).bind(PROJECT).bind(USER).execute(&mut first).await?;
    let mut held = first.begin().await?;
    sqlx::query("SELECT id FROM hypotheses WHERE id=$1 FOR UPDATE")
        .bind(original.id)
        .fetch_one(&mut *held)
        .await?;
    let reader = tokio::spawn(async move {
        let row = get_hypothesis(&mut second, PROJECT, &1.into(), true, CONTEXT).await?;
        Ok::<_, HypothesisError>((second, row))
    });
    let row_waited = require_blocked(&mut held, pid).await?;
    sqlx::query("UPDATE hypotheses SET track_id=$1 WHERE id=$2")
        .bind(alternate)
        .bind(original.id)
        .execute(&mut *held)
        .await?;
    held.commit().await?;
    let (mut second, joined) = reader.await??;
    let joined = joined.ok_or("missing joined hypothesis")?;
    let mut held = first.begin().await?;
    let first_number = next_number(&mut held, PROJECT).await?;
    let allocator = tokio::spawn(async move { next_number(&mut second, PROJECT).await });
    let allocator_waited = require_blocked(&mut held, pid).await?;
    held.commit().await?;
    let second_number = allocator.await??;
    let mut held = first.begin().await?;
    let rolled_back = next_number(&mut held, PROJECT).await?;
    held.rollback().await?;
    let reused = next_number(&mut first, PROJECT).await?;
    Ok(
        json!({"row_waited":row_waited,"joined_track_slug":text_value(&joined.track_slug)?,"joined_track_mode":joined.track_mode.as_str(),"allocator_waited":allocator_waited,"allocated":[first_number,second_number],"rollback_reused":[rolled_back,reused]}),
    )
}
async fn warmup(url: &str) -> Result<Value> {
    let mut observations = Vec::new();
    for mode in ["auto", "force_generic_plan"] {
        for threshold in [5, 0] {
            for action in [
                "before-missing",
                "after-missing",
                "approve-missing",
                "refs-missing",
            ] {
                let mut connection = PgConnection::connect(url).await?;
                sqlx::query(if mode == "auto" {
                    "SET plan_cache_mode=auto"
                } else {
                    "SET plan_cache_mode=force_generic_plan"
                })
                .execute(&mut connection)
                .await?;
                let mut outcomes = Vec::new();
                for step in 0..16 {
                    let number = if step < 7 {
                        BigInt::from(1)
                    } else {
                        BigInt::from(10).pow(19)
                    };
                    let result = match action {
                        "before-missing" => list_hypotheses(
                            &mut connection,
                            ProjectId(Uuid::nil()),
                            ListHypotheses {
                                states: None,
                                track_slug: None,
                                before: Some(&number),
                                limit: &10.into(),
                            },
                            CONTEXT,
                        )
                        .await
                        .map(|_| ()),
                        "after-missing" => {
                            list_revisions(&mut connection, MISSING, Some(&number), None, CONTEXT)
                                .await
                                .map(|_| ())
                        }
                        "approve-missing" => {
                            set_state(
                                &mut connection,
                                MISSING,
                                HypothesisState::Queued,
                                Some(&number),
                            )
                            .await
                        }
                        "refs-missing" => {
                            resolve_refs(&mut connection, [(ProjectId(Uuid::nil()), number)])
                                .await
                                .map(|_| ())
                        }
                        _ => return Err("unknown controlled warmup".into()),
                    };
                    if step >= 7 {
                        assert!(
                            matches!(&result, Err(HypothesisError::IntegerBinding)),
                            "warmup {mode}/{threshold}/{action}/{step}: expected checked integer refusal"
                        );
                    }
                    outcomes.push(match result {
                        Ok(()) => Some("ok".to_owned()),
                        Err(error) => error.sqlstate().map(str::to_owned),
                    });
                }
                observations.push(
                    json!({"mode":mode,"threshold":threshold,"action":action,"outcomes":outcomes}),
                );
            }
        }
    }
    Ok(json!(observations))
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;

#[test]
fn source_float_representation_keeps_integers_and_storage_exact() -> Result<()> {
    let mut value: Value = serde_json::from_str(
        r#"{"tiny":1e-07,"large":9007199254740993,"stored":"{\"tiny\": 0.0000001}"}"#,
    )?;
    source_float_tokens(&mut value)?;
    assert_eq!(value["tiny"], json!(0.000_000_1_f64));
    assert_eq!(value["large"].to_string(), "9007199254740993");
    assert_eq!(value["stored"], "{\"tiny\": 0.0000001}");
    Ok(())
}

#[test]
fn native_json_rejects_surrogates_and_nonfinite_literals() {
    for text in [r#"{"x":"\ud800"}"#, "NaN", "Infinity", "-Infinity"] {
        assert!(
            matches!(
                json::decode_str(text, CONTEXT.decode_nesting_budget),
                Err(json::DecodeError::Syntax { .. })
            ),
            "accepted {text}"
        );
    }
    assert!(
        json::decode_str(
            r#"{"x":"\ud83d\ude00","nul":"\u0000"}"#,
            CONTEXT.decode_nesting_budget
        )
        .is_ok()
    );
}
