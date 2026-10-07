//! Transaction-scoped job transitions shared by submission and worker controllers.
//! Authorization and physical connection ownership remain the caller's responsibility.
#![allow(
    clippy::many_single_char_names,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "Borrowed transaction flow keeps explicit caller contexts and ordered mutations together"
)]
use crate::{
    attempt_lease_routes::{Failure, domain, internal},
    requests::RequestContext,
};
use cannery_attempts::{
    model::{Attempt, Manifest},
    repo::{RecordFailure, Repository},
};
use cannery_core::{
    audit::{self, Attribution, Record},
    errors::ErrorCode,
    ids::JobId,
    json::{self, Document},
    principal::Principal,
};
use cannery_jobs::repo::{self as jobs, Job, Stage};
use cannery_research::{
    config_repo,
    science::{RenderingContext, Science},
};
use num_bigint::BigInt;
use serde_json::{Value, json};
use sqlx::PgConnection;

/// Explicit profiles for caller-owned transactions; this does not install a pool policy.
#[derive(Clone)]
pub struct Context {
    pub jobs: jobs::JsonContext,
    pub attempts: cannery_attempts::model::JsonContext,
    pub config: config_repo::JsonContext,
    pub hypotheses: cannery_hypotheses::repo::JsonContext,
    pub rendering: RenderingContext,
}
pub(crate) fn document(
    value: &Value,
    profile: &Context,
    r: &RequestContext,
) -> Result<Document, Failure> {
    let bytes = serde_json::to_vec(value).map_err(|_| internal(r, "job document encoding"))?;
    json::decode(&bytes, profile.jobs.encode_nesting_budget)
        .map_err(|_| internal(r, "job document construction"))
}
pub(crate) fn value(
    document: &Document,
    profile: &Context,
    r: &RequestContext,
) -> Result<Value, Failure> {
    let text = json::encode_ascii_pretty(document, profile.jobs.encode_nesting_budget)
        .map_err(|_| internal(r, "job document rendering"))?;
    serde_json::from_str(&text).map_err(|_| internal(r, "job document typed representation"))
}
pub(crate) async fn science(
    c: &mut PgConnection,
    a: &Attempt,
    s: &Context,
    r: &RequestContext,
) -> Result<config_repo::ConfigRevision, Failure> {
    config_repo::get_revision(
        c,
        a.project_id,
        config_repo::Kind::Science,
        Some(&BigInt::from(a.science_revision)),
        s.config,
    )
    .await
    .map_err(|_| internal(r, "job pinned science"))?
    .ok_or_else(|| internal(r, "job pinned science missing"))
}
pub(crate) async fn no_evaluator_reason(
    c: &mut PgConnection,
    a: &Attempt,
    s: &Context,
    r: &RequestContext,
) -> Result<Option<String>, Failure> {
    let raw = science(c, a, s, r).await?;
    let science = Science::new(raw.revision.into(), &raw.content, s.rendering)
        .map_err(|_| internal(r, "job science construction"))?;
    Ok(science
        .legacy_problem()
        .map_err(|_| internal(r, "job evaluator registration"))?
        .map(|_| {
            format!(
                "science revision {} has no evaluator; built-in gates moved to the stock evaluator",
                raw.revision
            )
        }))
}
#[allow(
    clippy::too_many_arguments,
    reason = "Caller owns identity, transaction and request diagnostics"
)]
pub(crate) async fn event(
    c: &mut PgConnection,
    p: &Principal,
    a: &Attempt,
    action: &str,
    subject: &str,
    id: &str,
    prior: Option<&Value>,
    new: &Value,
    reason: Option<&str>,
    r: &RequestContext,
) -> Result<(), Failure> {
    event_as(
        c,
        Attribution::Principal(p),
        a,
        action,
        subject,
        id,
        prior,
        new,
        reason,
        r,
    )
    .await
}
#[allow(
    clippy::too_many_arguments,
    reason = "Audit state and capability identity are explicit"
)]
pub(crate) async fn event_as(
    c: &mut PgConnection,
    actor: Attribution<'_>,
    a: &Attempt,
    action: &str,
    subject: &str,
    id: &str,
    prior: Option<&Value>,
    new: &Value,
    reason: Option<&str>,
    r: &RequestContext,
) -> Result<(), Failure> {
    audit::record(
        c,
        actor,
        Record {
            action,
            subject_type: subject,
            subject_id: id,
            project_id: Some(a.project_id),
            prior_state: prior,
            new_state: Some(new),
            reason,
            idempotency_key: None,
        },
    )
    .await
    .map(|_| ())
    .map_err(|_| internal(r, "job lifecycle audit"))
}
#[allow(
    dead_code,
    reason = "Submission composition installs this Principal wrapper"
)]
pub(crate) async fn record_job_created(
    c: &mut PgConnection,
    p: &Principal,
    a: &Attempt,
    j: &Job,
    s: &Context,
    r: &RequestContext,
) -> Result<(), Failure> {
    record_job_created_as(c, Attribution::Principal(p), a, j, s, r).await
}
pub(crate) async fn record_job_created_as(
    c: &mut PgConnection,
    actor: Attribution<'_>,
    a: &Attempt,
    j: &Job,
    s: &Context,
    r: &RequestContext,
) -> Result<(), Failure> {
    let spec = value(&j.spec, s, r)?;
    let steps = spec
        .get("steps")
        .and_then(Value::as_array)
        .map(|v| {
            v.iter()
                .map(|v| json!({"name":v["name"],"revision":v["revision"]}))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    event_as(c,actor,a,"job.created","job",&j.id.to_string(),None,&json!({"state":j.state.as_str(),"stage":j.stage.as_str(),"run_number":j.run_number,"attempt_id":j.attempt_id.to_string(),"service":j.tester_id,"origin":j.origin.as_str(),"previous_run_id":j.previous_run_id.map(|v|v.to_string()),"steps":steps}),None,r).await
}
#[allow(
    dead_code,
    reason = "Submission composition installs this Principal wrapper"
)]
pub(crate) async fn fail_without_evaluator(
    c: &mut PgConnection,
    p: &Principal,
    a: &Attempt,
    stage: Stage,
    reason: &str,
    s: &Context,
    r: &RequestContext,
) -> Result<(), Failure> {
    fail_without_evaluator_as(c, Attribution::Principal(p), a, stage, reason, s, r).await
}
#[allow(
    clippy::too_many_arguments,
    reason = "Explicit capability attribution and transaction"
)]
pub(crate) async fn fail_without_evaluator_as(
    c: &mut PgConnection,
    actor: Attribution<'_>,
    a: &Attempt,
    stage: Stage,
    reason: &str,
    s: &Context,
    r: &RequestContext,
) -> Result<(), Failure> {
    let details = document(&json!({"science_revision":a.science_revision}), s, r)?;
    Repository::new(c, s.attempts)
        .record_failure(RecordFailure {
            project_id: a.project_id,
            attempt: a,
            stage: stage.as_str(),
            code: "no_evaluator",
            reason,
            details: &details,
            log_refs: None,
            from_state: Some(a.state.as_str()),
        })
        .await
        .map_err(|_| internal(r, "job no evaluator failure"))?;
    event_as(
        c,
        actor,
        a,
        "attempt.failed",
        "attempt",
        &a.id.to_string(),
        Some(&json!({"state":a.state.as_str()})),
        &json!({"state":"failed","stage":stage.as_str(),"code":"no_evaluator"}),
        Some(reason),
        r,
    )
    .await
}
pub(crate) fn output_prefix(a: &Attempt, id: JobId, stage: Stage) -> String {
    format!(
        "projects/{}/attempts/{}/{}/{}/",
        a.project_id,
        a.id,
        if stage == Stage::Tester {
            "test-runs"
        } else {
            "evaluation-runs"
        },
        id
    )
}
#[allow(
    clippy::too_many_arguments,
    reason = "Transition inputs and transaction are explicit"
)]
pub(crate) async fn fail_job_run(
    c: &mut PgConnection,
    p: &Principal,
    a: &Attempt,
    j: &Job,
    report: jobs::Failure<'_>,
    details: &Document,
    agent_side: bool,
    s: &Context,
    r: &RequestContext,
) -> Result<(Job, Option<Job>), Failure> {
    fail_job_run_as(
        c,
        Attribution::Principal(p),
        a,
        j,
        report,
        details,
        agent_side,
        s,
        r,
    )
    .await
}
#[allow(
    clippy::too_many_arguments,
    reason = "Caller owns fenced job, transaction and audit attribution"
)]
pub(crate) async fn fail_job_run_as(
    c: &mut PgConnection,
    actor: Attribution<'_>,
    a: &Attempt,
    j: &Job,
    report: jobs::Failure<'_>,
    details: &Document,
    agent_side: bool,
    s: &Context,
    r: &RequestContext,
) -> Result<(Job, Option<Job>), Failure> {
    let stage_state = if j.stage == Stage::Tester {
        "testing"
    } else {
        "evaluating"
    };
    if j.state != jobs::State::Claimed || j.attempt_id != a.id || a.state.as_str() != stage_state {
        return Err(domain(
            ErrorCode::StaleLease,
            "the job is no longer claimed",
        ));
    }
    let failed = jobs::fail_job(
        c,
        j.id,
        jobs::Failure {
            step: report.step,
            code: report.code,
            reason: report.reason,
            logs: report.logs,
        },
        s.jobs,
    )
    .await
    .map_err(|_| internal(r, "job fail transition"))?;
    event_as(
        c,
        actor,
        a,
        "job.failed",
        "job",
        &j.id.to_string(),
        Some(&json!({"state":j.state.as_str()})),
        &json!({"state":"failed","code":report.code,"step":report.step,"run_number":j.run_number}),
        Some(report.reason),
        r,
    )
    .await?;
    if !agent_side {
        if let Some(reason) = no_evaluator_reason(c, a, s, r).await? {
            fail_without_evaluator_as(c, actor, a, j.stage, &reason, s, r).await?;
            return Ok((failed, None));
        }
        let raw = science(c, a, s, r).await?;
        let budget = raw
            .content
            .field(raw.content.root(), "max_auto_retries")
            .map(|id| cannery_research::science::configuration_integer(&raw.content, id))
            .transpose()
            .map_err(|_| internal(r, "job rerun budget"))?
            .unwrap_or_else(|| 1.into());
        let used = jobs::automatic_reruns(c, a.id, j.stage)
            .await
            .map_err(|_| internal(r, "job rerun count"))?;
        if BigInt::from(used) < budget {
            let id = JobId(uuid::Uuid::new_v4());
            let mut spec = value(&j.spec, s, r)?;
            spec["output_prefix"] = json!(output_prefix(a, id, j.stage));
            let spec = document(&spec, s, r)?;
            let rerun = jobs::create_job(
                c,
                jobs::NewJob {
                    id,
                    project_id: a.project_id,
                    attempt_id: a.id,
                    stage: j.stage,
                    science_revision: &BigInt::from(j.science_revision),
                    tester_id: &j.tester_id,
                    spec: &spec,
                    deadline_seconds: &BigInt::from(j.deadline_seconds),
                    origin: jobs::Origin::AutoRetry,
                    previous_run_id: Some(j.id),
                },
                s.jobs,
            )
            .await
            .map_err(|_| internal(r, "job rerun creation"))?;
            record_job_created_as(c, actor, a, &rerun, s, r).await?;
            return Ok((failed, Some(rerun)));
        }
    }
    let runs = jobs::list_jobs(c, a.id, None, None, s.jobs)
        .await
        .map_err(|_| internal(r, "job failure history"))?;
    let mut data = value(details, s, r)?;
    let object = data
        .as_object_mut()
        .ok_or_else(|| internal(r, "job failure details shape"))?;
    object.insert("job_id".into(), json!(j.id.to_string()));
    object.insert("run_number".into(), json!(j.run_number));
    object.insert("step".into(), json!(report.step));
    object.insert("runs".into(),json!(runs.iter().filter(|run|run.stage==j.stage).map(|run|json!({"job_id":run.id.to_string(),"run_number":run.run_number,"error_code":run.error_code})).collect::<Vec<_>>()));
    let details = document(&data, s, r)?;
    let stage = if agent_side {
        "agent"
    } else {
        j.stage.as_str()
    };
    Repository::new(c, s.attempts)
        .record_failure(RecordFailure {
            project_id: a.project_id,
            attempt: a,
            stage,
            code: report.code,
            reason: report.reason,
            details: &details,
            log_refs: Some(report.logs),
            from_state: Some(stage_state),
        })
        .await
        .map_err(|_| internal(r, "job failure review"))?;
    event_as(
        c,
        actor,
        a,
        "attempt.failed",
        "attempt",
        &a.id.to_string(),
        Some(&json!({"state":a.state.as_str()})),
        &json!({"state":"failed","stage":stage,"code":report.code,"job_id":j.id.to_string()}),
        Some(report.reason),
        r,
    )
    .await?;
    Ok((failed, None))
}

/// Queue an evaluator on the same locked attempt and verified tester inputs.
#[allow(
    clippy::too_many_arguments,
    reason = "Explicit transaction and publication inputs"
)]
pub(crate) async fn start_evaluation(
    c: &mut PgConnection,
    p: &Principal,
    a: &Attempt,
    test: &Job,
    evidence: cannery_attempts::model::EvidenceId,
    sha256: &str,
    manifest: &Manifest,
    s: &Context,
    r: &RequestContext,
) -> Result<Option<Job>, Failure> {
    start_evaluation_as(
        c,
        Attribution::Principal(p),
        a,
        test,
        evidence,
        sha256,
        manifest,
        s,
        r,
    )
    .await
}

/// Recovery callers supply genuine system attribution on the same transaction.
pub(crate) async fn start_evaluation_as(
    c: &mut PgConnection,
    actor: Attribution<'_>,
    a: &Attempt,
    test: &Job,
    evidence: cannery_attempts::model::EvidenceId,
    sha256: &str,
    manifest: &Manifest,
    s: &Context,
    r: &RequestContext,
) -> Result<Option<Job>, Failure> {
    if let Some(reason) = no_evaluator_reason(c, a, s, r).await? {
        fail_without_evaluator_as(c, actor, a, Stage::Evaluator, &reason, s, r).await?;
        return Ok(None);
    }
    let raw = science(c, a, s, r).await?;
    let science = Science::new(raw.revision.into(), &raw.content, s.rendering)
        .map_err(|_| internal(r, "evaluation science"))?;
    let registered = value(&raw.content, s, r)?;
    let evaluator = &registered["evaluator"];
    let id = JobId(uuid::Uuid::new_v4());
    let tested = value(&test.spec, s, r)?;
    let revision = cannery_hypotheses::repo::get_revision(
        c,
        a.hypothesis_id,
        &BigInt::from(a.hypothesis_revision),
        s.hypotheses,
    )
    .await
    .map_err(|_| internal(r, "evaluation pinned hypothesis"))?
    .ok_or_else(|| internal(r, "evaluation pinned hypothesis missing"))?;
    let hypothesis = value(&revision.content, s, r)?;
    let control = cannery_research::job_baselines::pinned_control(&test.spec, s.rendering)
        .map_err(|_| internal(r, "evaluation pinned control"))?
        .as_ref()
        .map(|d| value(d, s, r))
        .transpose()?;
    let spec = document(
        &json!({"evaluator":{"id":evaluator["id"],"revision":evaluator["revision"]},"track":a.track_slug,"inputs":{"evidence":[{"ref":evidence.0.to_string(),"sha256":sha256}],"manifest":{"ref":manifest.id.0.to_string(),"sha256":manifest.sha256},"baselines":tested["inputs"]["baselines"],"datasets":tested["inputs"]["datasets"]},"output_prefix":output_prefix(a,id,Stage::Evaluator),"limits":{"max_output_bytes":science.max_output_bytes().map_err(|_|internal(r,"evaluation output budget"))?.to_string().parse::<u64>().map_err(|_|internal(r,"evaluation output budget range"))?},"control":control,"parameters":hypothesis.get("project_fields").filter(|v|!v.is_null()).cloned().unwrap_or_else(||json!({}))}),
        s,
        r,
    )?;
    let deadline = registered["limits"]
        .get("max_deadline_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(3600);
    let evaluator_id = evaluator["id"]
        .as_str()
        .ok_or_else(|| internal(r, "evaluator identity"))?;
    let job = jobs::create_job(
        c,
        jobs::NewJob {
            id,
            project_id: a.project_id,
            attempt_id: a.id,
            stage: Stage::Evaluator,
            science_revision: &BigInt::from(a.science_revision),
            tester_id: evaluator_id,
            spec: &spec,
            deadline_seconds: &BigInt::from(deadline),
            origin: jobs::Origin::Submission,
            previous_run_id: None,
        },
        s.jobs,
    )
    .await
    .map_err(|_| internal(r, "evaluation job creation"))?;
    record_job_created_as(c, actor, a, &job, s, r).await?;
    Ok(Some(job))
}

/// Queue a test job from resolved, pinned producer/scorer steps. The caller holds the attempt lock.
#[allow(
    clippy::too_many_arguments,
    reason = "Explicit submission inputs and context"
)]
#[allow(
    dead_code,
    reason = "Shared submission composition helper; parent installs its caller separately"
)]
pub(crate) async fn create_test_job(
    c: &mut PgConnection,
    a: &Attempt,
    sheet: uuid::Uuid,
    sheet_sha256: &str,
    manifest: &Manifest,
    overhead: &BigInt,
    s: &Context,
    r: &RequestContext,
) -> Result<Job, Failure> {
    let raw = science(c, a, s, r).await?;
    let science = Science::new(raw.revision.into(), &raw.content, s.rendering)
        .map_err(|_| internal(r, "submission science"))?;
    let producer = match &a.producer {
        cannery_attempts::model::StoredJson::Value(d) => Some(d.as_ref()),
        cannery_attempts::model::StoredJson::SqlNull => None,
    };
    let resolved = crate::step_binding::resolve_producer(
        &mut crate::step_binding::PgManifestQuery(c),
        a.project_id,
        &science,
        producer,
        &String::from("/producer"),
        &crate::step_binding::BindingContext {
            rendering: s.rendering,
            decode_budget: s.config.decode_nesting_budget,
            equality: &crate::step_binding::CheckedOutputEquality,
        },
    )
    .await
    .map_err(|_| internal(r, "submission producer binding"))?;
    let scorer_id = science
        .scorer()
        .map_err(|_| internal(r, "submission scorer"))?;
    let mut builder = json::DocumentBuilder::new();
    let root = builder
        .import(&raw.content, scorer_id)
        .map_err(|_| internal(r, "submission scorer copy"))?;
    let scorer = builder
        .finish(root)
        .map_err(|_| internal(r, "submission scorer document"))?;
    let revision = cannery_hypotheses::repo::get_revision(
        c,
        a.hypothesis_id,
        &BigInt::from(a.hypothesis_revision),
        s.hypotheses,
    )
    .await
    .map_err(|_| internal(r, "submission pinned hypothesis"))?
    .ok_or_else(|| internal(r, "submission pinned hypothesis missing"))?;
    let hyp = value(&revision.content, s, r)?;
    // The step contract pins identity only; hypothesis control metadata is not
    // part of the closed pinned_ref schema shared by testers and evaluators.
    let control = hyp
        .get("control")
        .filter(|value| !value.is_null())
        .map(|value| {
            let id = value
                .get("id")
                .ok_or_else(|| internal(r, "tester control identity"))?;
            let revision = value
                .get("revision")
                .ok_or_else(|| internal(r, "tester control revision"))?;
            Ok::<_, crate::attempt_lease_routes::Failure>(json!({"id":id,"revision":revision}))
        })
        .transpose()?;
    let control_document = control.as_ref().map(|v| document(v, s, r)).transpose()?;
    let producer_manifest = value(&resolved.manifest, s, r)?;
    let scorer_manifest = value(&scorer, s, r)?;
    let name = resolved
        .name
        .as_utf8()
        .ok_or_else(|| internal(r, "submission producer name"))?;
    let steps = document(
        &json!([{"name":name,"revision":resolved.revision.to_string().parse::<i64>().map_err(|_|internal(r,"submission producer revision range"))?,"manifest":producer_manifest},{"name":scorer_manifest["metadata"]["name"],"revision":a.science_revision,"manifest":scorer_manifest}]),
        s,
        r,
    )?;
    let baselines = cannery_research::job_baselines::staged_baselines(
        &science,
        &steps,
        control_document.as_ref(),
        s.rendering,
    )
    .map_err(|_| internal(r, "submission baseline binding"))?;
    let registered = value(&raw.content, s, r)?;
    let tester = registered["tester"].clone();
    let id = JobId(uuid::Uuid::new_v4());
    let datasets=science.datasets.iter().map(|(_,d)|Ok(json!({"id":d.id.as_utf8().ok_or_else(||internal(r,"dataset identity"))?,"revision":d.revision.as_utf8().ok_or_else(||internal(r,"dataset revision"))?}))).collect::<Result<Vec<_>,Failure>>()?;
    let spec = document(
        &json!({"tester":tester,"track":a.track_slug,"steps":value(&steps,s,r)?,"inputs":{"claimed_sheet":{"ref":sheet.to_string(),"sha256":sheet_sha256},"manifest":{"ref":manifest.id.0.to_string(),"sha256":manifest.sha256},"baselines":value(&baselines,s,r)?,"datasets":datasets},"output_prefix":output_prefix(a,id,Stage::Tester),"limits":{"max_output_bytes":registered["limits"]["max_output_bytes"]},"control":control}),
        s,
        r,
    )?;
    let deadline = cannery_research::job_deadlines::workflow_seconds(
        &science,
        &[&resolved.manifest, &scorer],
        overhead,
        s.rendering,
    )
    .map_err(|_| internal(r, "submission deadline"))?;
    let tester_id = registered["tester"]["id"]
        .as_str()
        .ok_or_else(|| internal(r, "tester identity"))?;
    jobs::create_job(
        c,
        jobs::NewJob {
            id,
            project_id: a.project_id,
            attempt_id: a.id,
            stage: Stage::Tester,
            science_revision: &BigInt::from(a.science_revision),
            tester_id,
            spec: &spec,
            deadline_seconds: &deadline,
            origin: jobs::Origin::Submission,
            previous_run_id: None,
        },
        s.jobs,
    )
    .await
    .map_err(|_| internal(r, "submission job creation"))
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
