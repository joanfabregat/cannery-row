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
    model::Attempt,
    repo::{RecordFailure, Repository},
};
use cannery_core::{
    audit::{self, Attribution, Record},
    errors::ErrorCode,
    ids::JobId,
    json::{self, Document},
    principal::Principal,
};
use cannery_jobs::repo::{self as jobs, Job, Performer, Phase};
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
    pub units: cannery_units::repo::JsonContext,
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
/// The audit record of a queued job: its identity, steps and who may perform it.
pub(crate) fn created_state(j: &Job, spec: &Value) -> Value {
    let steps = spec
        .get("steps")
        .and_then(Value::as_array)
        .map(|v| {
            v.iter()
                .map(|v| json!({"name":v["name"],"revision":v["revision"]}))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({"state":j.state.as_str(),"phase":j.phase.as_str(),"performer":j.performer.as_str(),"run_number":j.run_number,"attempt_id":j.attempt_id.to_string(),"verifier":j.verifier_id,"origin":j.origin.as_str(),"previous_run_id":j.previous_run_id.map(|v|v.to_string()),"steps":steps})
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
    event_as(
        c,
        actor,
        a,
        "job.created",
        "job",
        &j.id.to_string(),
        None,
        &created_state(j, &spec),
        None,
        r,
    )
    .await
}
pub(crate) fn output_prefix(a: &Attempt, id: JobId) -> String {
    format!(
        "projects/{}/attempts/{}/verify-runs/{}/",
        a.project_id, a.id, id
    )
}

/// The run record a verify job reads: the attempt's latest run front matter and its manifest.
pub(crate) struct RunInputs {
    pub run: uuid::Uuid,
    pub run_sha256: String,
    pub manifest: uuid::Uuid,
    pub manifest_sha256: String,
}
/// The attempt's latest completed run record, pinned by the canonical digest of its front matter.
pub(crate) async fn run_inputs(
    c: &mut PgConnection,
    a: &Attempt,
    s: &Context,
    r: &RequestContext,
) -> Result<Option<RunInputs>, Failure> {
    let row = sqlx::query!(
        r#"SELECT e.id::text AS "id!", e.front_matter::text AS "front_matter!", m.id::text AS "manifest!", m.sha256 AS "manifest_sha256!"
           FROM phase_outputs e JOIN manifests m ON m.id = e.manifest_id
           WHERE e.attempt_id = ($1::text)::uuid AND e.stage = 'agent' AND e.status = 'completed'
           ORDER BY e.revision DESC LIMIT 1"#,
        a.id.0.to_string()
    )
    .fetch_optional(&mut *c)
    .await
    .map_err(|_| internal(r, "verify job run record"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let front_matter = json::decode(row.front_matter.as_bytes(), s.jobs.decode_nesting_budget)
        .map_err(|_| internal(r, "verify job run front matter"))?;
    let run_sha256 =
        crate::manifest_routes::canonical_sha256(&front_matter, s.jobs.encode_nesting_budget)
            .map_err(|_| internal(r, "verify job run digest"))?;
    let id = |text: &str| {
        uuid::Uuid::parse_str(text).map_err(|_| internal(r, "verify job run record id"))
    };
    Ok(Some(RunInputs {
        run: id(&row.id)?,
        run_sha256,
        manifest: id(&row.manifest)?,
        manifest_sha256: row.manifest_sha256,
    }))
}

/// Queue a verify job from the pinned science, the resolved producer and scorer steps and the
/// attempt's run record. The caller holds the attempt lock.
pub(crate) async fn create_verify_job(
    c: &mut PgConnection,
    a: &Attempt,
    run: &RunInputs,
    overhead: &BigInt,
    origin: jobs::Origin,
    previous: Option<JobId>,
    s: &Context,
    r: &RequestContext,
) -> Result<Job, Failure> {
    let raw = science(c, a, s, r).await?;
    let science = Science::new(raw.revision.into(), &raw.content, s.rendering)
        .map_err(|_| internal(r, "verify science"))?;
    let registered = value(&raw.content, s, r)?;
    let verify = &registered["verify"];
    let performer = match verify["performer"].as_str() {
        Some("runner") => Performer::Runner,
        Some("agent") => Performer::Agent,
        _ => return Err(internal(r, "verify performer")),
    };
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
    .map_err(|_| internal(r, "verify producer binding"))?;
    let scorer_id = science.scorer().map_err(|_| internal(r, "verify scorer"))?;
    let mut builder = json::DocumentBuilder::new();
    let root = builder
        .import(&raw.content, scorer_id)
        .map_err(|_| internal(r, "verify scorer copy"))?;
    let scorer = builder
        .finish(root)
        .map_err(|_| internal(r, "verify scorer document"))?;
    let revision =
        cannery_units::repo::get_revision(c, a.unit_id, &BigInt::from(a.unit_revision), s.units)
            .await
            .map_err(|_| internal(r, "verify pinned unit"))?
            .ok_or_else(|| internal(r, "verify pinned unit missing"))?;
    let hyp = value(&revision.content, s, r)?;
    // The step contract pins identity only; unit control metadata is not
    // part of the closed pinned_ref schema of a verify job.
    let control = hyp
        .get("control")
        .filter(|value| !value.is_null())
        .map(|value| {
            let id = value
                .get("id")
                .ok_or_else(|| internal(r, "verify control identity"))?;
            let revision = value
                .get("revision")
                .ok_or_else(|| internal(r, "verify control revision"))?;
            Ok::<_, Failure>(json!({"id":id,"revision":revision}))
        })
        .transpose()?;
    let control_document = control.as_ref().map(|v| document(v, s, r)).transpose()?;
    let producer_manifest = value(&resolved.manifest, s, r)?;
    let scorer_manifest = value(&scorer, s, r)?;
    let name = resolved
        .name
        .as_utf8()
        .ok_or_else(|| internal(r, "verify producer name"))?;
    let steps = document(
        &json!([{"name":name,"revision":resolved.revision.to_string().parse::<i64>().map_err(|_|internal(r,"verify producer revision range"))?,"manifest":producer_manifest},{"name":scorer_manifest["metadata"]["name"],"revision":a.science_revision,"manifest":scorer_manifest}]),
        s,
        r,
    )?;
    let baselines = cannery_research::job_baselines::staged_baselines(
        &science,
        &steps,
        control_document.as_ref(),
        s.rendering,
    )
    .map_err(|_| internal(r, "verify baseline binding"))?;
    let id = JobId(uuid::Uuid::new_v4());
    let datasets=science.datasets.iter().map(|(_,d)|Ok(json!({"id":d.id.as_utf8().ok_or_else(||internal(r,"dataset identity"))?,"revision":d.revision.as_utf8().ok_or_else(||internal(r,"dataset revision"))?}))).collect::<Result<Vec<_>,Failure>>()?;
    let max_output_bytes = science
        .max_output_bytes()
        .map_err(|_| internal(r, "verify output budget"))?
        .to_string()
        .parse::<u64>()
        .map_err(|_| internal(r, "verify output budget range"))?;
    let parameters = hyp
        .get("project_fields")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut spec = json!({"performer":performer.as_str(),"track":a.track_slug,"steps":value(&steps,s,r)?,"inputs":{"run":{"ref":run.run.to_string(),"sha256":run.run_sha256},"manifest":{"ref":run.manifest.to_string(),"sha256":run.manifest_sha256},"baselines":value(&baselines,s,r)?,"datasets":datasets},"output_prefix":output_prefix(a,id),"limits":{"max_output_bytes":max_output_bytes},"control":control,"parameters":parameters});
    let allowance = registered["limits"]
        .get("max_deadline_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(3600);
    let (verifier, deadline) = if performer == Performer::Runner {
        spec["verifier"] =
            json!({"id":verify["verifier"]["id"],"revision":verify["verifier"]["revision"]});
        let workflow = cannery_research::job_deadlines::workflow_seconds(
            &science,
            &[&resolved.manifest, &scorer],
            overhead,
            s.rendering,
        )
        .map_err(|_| internal(r, "verify deadline"))?;
        (
            Some(
                verify["verifier"]["id"]
                    .as_str()
                    .ok_or_else(|| internal(r, "verifier identity"))?,
            ),
            workflow + allowance,
        )
    } else {
        (None, overhead + allowance)
    };
    let spec = document(&spec, s, r)?;
    jobs::create_job(
        c,
        jobs::NewJob {
            id,
            project_id: a.project_id,
            attempt_id: a.id,
            phase: Phase::Verify,
            performer,
            science_revision: &BigInt::from(a.science_revision),
            verifier_id: verifier,
            spec: &spec,
            deadline_seconds: &deadline,
            origin,
            previous_run_id: previous,
        },
        s.jobs,
    )
    .await
    .map_err(|_| internal(r, "verify job creation"))
}

/// Where an automatic rerun starts: the failed step, and the verified outputs of the steps
/// before it, from this run or from the outputs it already reused.
async fn resume(
    c: &mut PgConnection,
    a: &Attempt,
    j: &Job,
    spec: &Value,
    step: Option<&str>,
    s: &Context,
    r: &RequestContext,
) -> Result<Option<Value>, Failure> {
    let Some(step) = step else {
        return Ok(None);
    };
    let names = spec["steps"]
        .as_array()
        .map(|steps| {
            steps
                .iter()
                .filter_map(|step| step["name"].as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let before = names
        .iter()
        .position(|name| *name == step)
        .map_or(names.as_slice(), |index| &names[..index]);
    if before.is_empty() {
        return Ok(None);
    }
    let mut outputs = spec["resume"]["outputs"]
        .as_array()
        .map(|outputs| {
            outputs
                .iter()
                .filter(|output| {
                    output["step"]
                        .as_str()
                        .is_some_and(|name| before.contains(&name))
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let prefix = spec["output_prefix"].as_str().unwrap_or_default();
    let artifacts = Repository::new(c, s.attempts)
        .list_job_artifacts(j.id)
        .await
        .map_err(|_| internal(r, "verify rerun outputs"))?;
    for artifact in artifacts
        .iter()
        .filter(|artifact| artifact.attempt_id == a.id)
    {
        let Some(path) = artifact.key.strip_prefix(prefix) else {
            continue;
        };
        let mut segments = path.splitn(3, '/');
        let (Some(owner), Some(name)) = (segments.next(), segments.next()) else {
            continue;
        };
        if !before.contains(&owner) || name != artifact.role {
            continue;
        }
        let mut output = json!({"step":owner,"name":name,"key":artifact.key,"size_bytes":artifact.size_bytes,"sha256":artifact.sha256,"media_type":artifact.media_type});
        if let Some(interface) = &artifact.interface {
            output["interface"] = json!(interface);
        }
        outputs.push(output);
    }
    Ok(Some(json!({"from_step":step,"outputs":outputs})))
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
    if j.state != jobs::State::Claimed || j.attempt_id != a.id || a.state.as_str() != "verifying" {
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
        let raw = science(c, a, s, r).await?;
        let budget = raw
            .content
            .field(raw.content.root(), "max_auto_retries")
            .map(|id| cannery_research::science::configuration_integer(&raw.content, id))
            .transpose()
            .map_err(|_| internal(r, "job rerun budget"))?
            .unwrap_or_else(|| 1.into());
        let used = jobs::automatic_reruns(c, a.id, j.phase)
            .await
            .map_err(|_| internal(r, "job rerun count"))?;
        if BigInt::from(used) < budget {
            let id = JobId(uuid::Uuid::new_v4());
            let mut spec = value(&j.spec, s, r)?;
            let resume = resume(c, a, j, &spec, report.step, s, r).await?;
            let object = spec
                .as_object_mut()
                .ok_or_else(|| internal(r, "job rerun specification"))?;
            object.insert("output_prefix".into(), json!(output_prefix(a, id)));
            if let Some(resume) = resume {
                object.insert("resume".into(), resume);
            } else {
                object.remove("resume");
            }
            let spec = document(&spec, s, r)?;
            let rerun = jobs::create_job(
                c,
                jobs::NewJob {
                    id,
                    project_id: a.project_id,
                    attempt_id: a.id,
                    phase: j.phase,
                    performer: j.performer,
                    science_revision: &BigInt::from(j.science_revision),
                    verifier_id: j.verifier_id.as_deref(),
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
    object.insert("runs".into(),json!(runs.iter().filter(|run|run.phase==j.phase).map(|run|json!({"job_id":run.id.to_string(),"run_number":run.run_number,"error_code":run.error_code})).collect::<Vec<_>>()));
    let details = document(&data, s, r)?;
    let stage = if agent_side { "agent" } else { "verify" };
    Repository::new(c, s.attempts)
        .record_failure(RecordFailure {
            project_id: a.project_id,
            attempt: a,
            stage,
            code: report.code,
            reason: report.reason,
            details: &details,
            log_refs: Some(report.logs),
            from_state: Some("verifying"),
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

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
