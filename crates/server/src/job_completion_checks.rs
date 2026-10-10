//! Semantic binding of a verification report to the verify job's pinned inputs and outputs.
#![allow(
    clippy::many_single_char_names,
    reason = "Explicit borrowed transaction and request contexts"
)]
use crate::{
    attempt_lease_routes::{Failure, failure, internal},
    errors::ApiError,
    job_completion_routes::{JobLifecycleContext, invalid},
    job_lifecycle as flow,
    requests::RequestContext,
};
use cannery_attempts::{
    model::{Artifact, Attempt, EvidenceId, StoredJson},
    repo::Repository,
};
use cannery_core::{
    contracts::{
        instance::{self, ProjectValidationFailure},
        phases::Phase,
    },
    errors::{DomainError, ErrorCode},
    front_matter::{self, Limits},
    json::Document,
};
use cannery_jobs::repo::{Job, Performer};
use serde_json::{Value, json};
use sha2::Digest as _;
use sqlx::PgConnection;
use std::{collections::BTreeSet, sync::Arc};

fn string<'a>(v: &'a Value, key: &str, r: &RequestContext) -> Result<&'a str, Failure> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| internal(r, "job pinned input shape"))
}
fn array<'a>(v: &'a Value, key: &str, r: &RequestContext) -> Result<&'a [Value], Failure> {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| internal(r, "job input list shape"))
}
pub(crate) fn manifest_objects(
    manifest: &Value,
    artifacts: &[Artifact],
) -> Result<BTreeSet<String>, Failure> {
    let mut seen = BTreeSet::new();
    let mut roles = BTreeSet::new();
    for (i, obj) in manifest["objects"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let storage = &obj["storage"];
        let identity = (
            storage["backend"].as_str(),
            storage["bucket"].as_str(),
            storage["key"].as_str(),
        );
        let path = format!("/manifest/objects/{i}");
        if !seen.insert(identity) {
            return Err(invalid(&path, "object listed twice"));
        }
        let found = artifacts
            .iter()
            .find(|a| {
                Some(a.backend.as_str()) == identity.0
                    && Some(a.bucket.as_str()) == identity.1
                    && Some(a.key.as_str()) == identity.2
            })
            .ok_or_else(|| invalid(&path, "not a verified upload of this job"))?;
        for (field, expected) in [
            ("role", json!(found.role)),
            ("size_bytes", json!(found.size_bytes)),
            ("sha256", json!(found.sha256)),
            ("media_type", json!(found.media_type)),
        ] {
            if obj[field] != expected {
                return Err(invalid(
                    &format!("{path}/{field}"),
                    format!("must be the verified object's {field}, {expected}"),
                ));
            }
        }
        roles.insert(found.role.clone());
    }
    Ok(roles)
}
fn extensions(
    science: &Value,
    report: &Value,
    s: &JobLifecycleContext,
    r: &RequestContext,
) -> Result<(), Failure> {
    let Some(schema) = science.get("result_extensions").filter(|v| !v.is_null()) else {
        return Ok(());
    };
    let schema = Arc::new(flow::document(schema, &s.flow, r)?);
    let content = flow::document(report.get("extensions").unwrap_or(&json!({})), &s.flow, r)?;
    match instance::validate_project_fields(&schema, &content) {
        Ok(()) => Ok(()),
        Err(ProjectValidationFailure::Document(errors)) => {
            let path = errors
                .first()
                .and_then(|v| v.path.as_utf8())
                .unwrap_or_default();
            Err(invalid(
                &format!("/extensions{path}"),
                "result extension violates its pinned schema",
            ))
        }
        Err(_) => Err(internal(r, "result extension schema")),
    }
}
/// The values of a JSON array, or each item's `key` field, as a readable
/// list of alternatives (`"a", "b" or "c"`).
fn listed(values: &Value, key: &str) -> String {
    let texts: Vec<String> = values
        .as_array()
        .into_iter()
        .flatten()
        .map(|value| if key.is_empty() { value } else { &value[key] })
        .map(|value| {
            value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned)
        })
        .collect();
    crate::job_outputs::quoted_list(&texts.iter().map(String::as_str).collect::<Vec<_>>())
}
#[allow(
    clippy::too_many_lines,
    reason = "Each measurement check names what it expected"
)]
fn measurements(science: &Value, report: &Value, r: &RequestContext) -> Result<(), Failure> {
    for (i, m) in report["measurements"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let path = format!("/measurements/{i}");
        let metric = array(science, "metrics", r)?
            .iter()
            .find(|v| v["key"] == m["metric"])
            .ok_or_else(|| {
                invalid(
                    &format!("{path}/metric"),
                    format!(
                        "unknown metric: the registered metrics are {}",
                        listed(&science["metrics"], "key")
                    ),
                )
            })?;
        if !array(metric, "splits", r)?.contains(&m["split"]) {
            return Err(invalid(
                &format!("{path}/split"),
                format!(
                    "unregistered metric split: {} has {}",
                    m["metric"],
                    listed(&metric["splits"], "")
                ),
            ));
        }
        for field in ["unit", "direction"] {
            if m[field] != metric[field] {
                return Err(invalid(
                    &format!("{path}/{field}"),
                    format!("must be the registered metric's {field}, {}", metric[field]),
                ));
            }
        }
        for (name, value) in m["dimensions"].as_object().into_iter().flatten() {
            let registered = metric["dimensions"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|d| d["name"].as_str() == Some(name))
                .ok_or_else(|| {
                    invalid(
                        &format!("{path}/dimensions/{name}"),
                        format!(
                            "unregistered metric dimension: {} has {}",
                            m["metric"],
                            listed(&metric["dimensions"], "name")
                        ),
                    )
                })?;
            if let Some(allowed) = registered.get("values")
                && allowed
                    .as_array()
                    .is_some_and(|allowed| !allowed.contains(value))
            {
                return Err(invalid(
                    &format!("{path}/dimensions/{name}"),
                    format!(
                        "unregistered dimension value: {name} takes {}",
                        listed(allowed, "")
                    ),
                ));
            }
        }
    }
    // Each explicitly required dimension slice must appear in the report.
    for slice in required_slices(science) {
        if !report["measurements"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|m| slice.matches(m))
        {
            return Err(invalid(
                "/measurements",
                format!(
                    "required slices are missing: no measurement of {} on split {} with only {} = {}",
                    slice.metric["key"], slice.split, slice.dimension, slice.value
                ),
            ));
        }
    }
    Ok(())
}

/// A slice a report must measure: `metric` on `split`, with `dimension` =
/// `value` as its only dimension.
pub(crate) struct RequiredSlice<'a> {
    pub(crate) metric: &'a Value,
    pub(crate) split: &'a Value,
    pub(crate) dimension: &'a Value,
    pub(crate) value: &'a Value,
}

impl RequiredSlice<'_> {
    /// Whether the measurement covers this slice.
    pub(crate) fn matches(&self, m: &Value) -> bool {
        m["metric"] == self.metric["key"]
            && m["split"] == *self.split
            && m["dimensions"].as_object().is_some_and(|dimensions| {
                dimensions.len() == 1
                    && self
                        .dimension
                        .as_str()
                        .is_some_and(|name| dimensions.get(name) == Some(self.value))
            })
    }
}

/// Every required slice of the science revision's metrics, in registry
/// order: a slice without its own splits applies to each of the metric's.
pub(crate) fn required_slices(science: &Value) -> Vec<RequiredSlice<'_>> {
    let mut slices = Vec::new();
    for metric in science["metrics"].as_array().into_iter().flatten() {
        for slice in metric["required_slices"].as_array().into_iter().flatten() {
            let splits = slice
                .get("splits")
                .and_then(Value::as_array)
                .filter(|v| !v.is_empty())
                .or_else(|| metric["splits"].as_array());
            for value in slice["values"].as_array().into_iter().flatten() {
                for split in splits.into_iter().flatten() {
                    slices.push(RequiredSlice {
                        metric,
                        split,
                        dimension: &slice["dimension"],
                        value,
                    });
                }
            }
        }
    }
    slices
}

fn comparisons(science: &Value, report: &Value, r: &RequestContext) -> Result<(), Failure> {
    cannery_research::comparisons::check(science, report, report).map_err(|error| match error {
        cannery_research::comparisons::ComparisonError::Invalid { path, message } => {
            invalid(&path, message)
        }
        cannery_research::comparisons::ComparisonError::Shape => {
            internal(r, "comparison input shape")
        }
    })
}

/// A checked verification report: its front matter, its body and the manifest of its outputs.
pub(crate) struct Report {
    pub front_matter: Document,
    pub body: String,
    pub manifest: Option<Document>,
    /// The SHA-256 of the report text, as sent.
    pub sha256: String,
}

#[allow(
    clippy::too_many_lines,
    reason = "Pinned input checks stay in publication order"
)]
pub(crate) async fn completion(
    c: &mut PgConnection,
    a: &Attempt,
    j: &Job,
    d: &Document,
    s: &JobLifecycleContext,
    r: &RequestContext,
) -> Result<Report, Failure> {
    let completion = flow::value(d, &s.flow, r)?;
    if completion["job_id"] != j.id.to_string() {
        return Err(invalid("/job_id", format!("this is job {}", j.id)));
    }
    let text = completion["document"]
        .as_str()
        .ok_or_else(|| internal(r, "verification report text"))?;
    let parsed = front_matter::parse(text, Limits::default())
        .map_err(|error| invalid("/document", error.to_string()))?;
    let report = Value::Object(parsed.front_matter);
    let violations = s.phases.violations(Phase::Verification, &report);
    if !violations.is_empty() {
        let details: Vec<_> = violations
            .into_iter()
            .map(|value| json!({"path":value.path,"message":value.message}))
            .collect();
        return Err(failure(ApiError::from(
            DomainError::new(
                ErrorCode::ValidationFailed,
                "invalid verification report front matter",
            )
            .with_details(json!(details)),
        )));
    }
    let spec = flow::value(&j.spec, &s.flow, r)?;
    let inputs = &spec["inputs"];
    let id = uuid::Uuid::parse_str(string(&inputs["run"], "ref", r)?)
        .map_err(|_| internal(r, "job run reference"))?;
    let run = Repository::new(c, s.flow.attempts)
        .get_evidence_by_id(a.id, EvidenceId(id))
        .await
        .map_err(|_| internal(r, "job run record"))?
        .ok_or_else(|| internal(r, "job run record missing"))?;
    let StoredJson::Value(run) = run.0 else {
        return Err(internal(r, "job run record shape"));
    };
    let run = flow::value(&run, &s.flow, r)?;
    let raw = flow::science(c, a, &s.flow, r).await?;
    let science = flow::value(&raw.content, &s.flow, r)?;
    let limit = science["limits"]["report_max_bytes"]
        .as_u64()
        .ok_or_else(|| internal(r, "verification report limit"))?;
    if u64::try_from(parsed.body.len()).map_or(true, |bytes| bytes > limit) {
        return Err(invalid(
            "/document",
            "the report body exceeds the configured byte limit",
        ));
    }
    let provenance = &report["provenance"];
    let control = cannery_research::job_baselines::pinned_control(&j.spec, s.flow.rendering)
        .map_err(|_| internal(r, "job control"))?
        .map(|control| flow::value(&control, &s.flow, r))
        .transpose()?;
    let expected =
        crate::job_outputs::VerifyExpectations::new(j, &spec, &science, &run, control.as_ref())
            .ok_or_else(|| internal(r, "job pinned expectations"))?;
    if provenance["science_revision"].as_str() != Some(expected.science_revision.as_str()) {
        return Err(invalid(
            "/provenance/science_revision",
            format!(
                "must be the job's pinned science revision, \"{}\"",
                expected.science_revision
            ),
        ));
    }
    if provenance["source_revision"] != expected.source_revision {
        return Err(invalid(
            "/provenance/source_revision",
            format!(
                "must be the run's source revision, {}",
                expected.source_revision
            ),
        ));
    }
    // One form everywhere: the control's bare revision, as the job pins it.
    if let Some((id, revision)) = &expected.control
        && provenance["control_revision"].as_str() != Some(revision.as_str())
    {
        return Err(invalid(
            "/provenance/control_revision",
            format!(
                "must be \"{revision}\", the revision of the unit's pinned control {id} (the bare revision, not \"{id}@{revision}\")"
            ),
        ));
    }
    let scored = expected.dataset_list();
    let dataset = provenance.get("dataset_revision").and_then(Value::as_str);
    if dataset.map_or(!scored.is_empty(), |revision| !scored.contains(&revision)) {
        return Err(invalid(
            "/provenance/dataset_revision",
            if scored.is_empty() {
                String::from(
                    "the job's scorer reads no registered dataset: leave dataset_revision out",
                )
            } else {
                format!(
                    "must be a dataset revision the job's scorer reads: {}",
                    crate::job_outputs::quoted_list(&scored)
                )
            },
        ));
    }
    measurements(&science, &report, r)?;
    extensions(&science, &report, s, r)?;
    if report["policy_revision"].as_str() != Some(expected.policy_revision.as_str()) {
        return Err(invalid(
            "/policy_revision",
            match j.performer {
                Performer::Runner => format!(
                    "must be the registered verifier's policy revision, \"{}\"",
                    expected.policy_revision
                ),
                Performer::Agent => format!(
                    "must be \"{}\": an agent verify job's policy revision is its pinned science revision, which registers agent verification",
                    expected.policy_revision
                ),
            },
        ));
    }
    let mut gates = BTreeSet::new();
    for gate in array(&report, "gates", r)? {
        if !gates.insert(string(gate, "id", r)?) {
            return Err(invalid("/gates", "each gate is reported once"));
        }
    }
    comparisons(&science, &report, r)?;
    // The manifest lists this job's verified uploads and the earlier outputs it reused.
    let mut artifacts = Repository::new(c, s.flow.attempts)
        .list_job_artifacts(j.id)
        .await
        .map_err(|_| internal(r, "completion verified artifacts"))?;
    let reused: BTreeSet<&str> = spec["resume"]["outputs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|output| output["key"].as_str())
        .collect();
    if !reused.is_empty() {
        artifacts.extend(
            Repository::new(c, s.flow.attempts)
                .list_artifacts(a.id)
                .await
                .map_err(|_| internal(r, "completion reused artifacts"))?
                .into_iter()
                .filter(|artifact| reused.contains(artifact.key.as_str())),
        );
    }
    let manifest = completion.get("manifest").filter(|v| !v.is_null());
    let roles = if let Some(manifest) = manifest {
        if manifest["attempt_id"] != a.id.to_string() {
            return Err(invalid(
                "/manifest/attempt_id",
                format!("must be the job's attempt, \"{}\"", a.id),
            ));
        }
        manifest_objects(manifest, &artifacts)?
    } else {
        BTreeSet::new()
    };
    let required = crate::job_outputs::names(&expected.roles);
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|role| !roles.contains(*role))
        .collect();
    if !missing.is_empty() {
        let message = format!(
            "the manifest lacks the required artifact role{} {}: a verify job's completion must list an upload of each role the science revision requires ({})",
            if missing.len() == 1 { "" } else { "s" },
            missing.join(", "),
            required.join(", ")
        );
        return Err(failure(ApiError::from(
            DomainError::new(ErrorCode::ValidationFailed, message.clone()).with_details(json!([{
                "path": "/manifest",
                "message": message,
                "missing_roles": missing,
                "required_roles": required,
            }])),
        )));
    }
    for (i, role) in report["artifact_roles"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        if !role.as_str().is_some_and(|v| roles.contains(v)) {
            return Err(invalid(
                &format!("/artifact_roles/{i}"),
                "the manifest has no object with this role",
            ));
        }
    }
    Ok(Report {
        front_matter: flow::document(&report, &s.flow, r)?,
        body: parsed.body,
        sha256: format!("{:x}", sha2::Sha256::digest(text.as_bytes())),
        manifest: manifest
            .map(|v| flow::document(v, &s.flow, r))
            .transpose()?,
    })
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{http::StatusCode, response::IntoResponse};

    #[test]
    fn duplicate_comparison_slices_ignore_object_key_order()
    -> Result<(), Box<dyn std::error::Error>> {
        let science = json!({"metrics":[{"key":"mrr","splits":["dev"],"dimensions":[{"name":"language"},{"name":"region"}]}]});
        let first: Value = serde_json::from_str(
            r#"{"metric":"mrr","split":"dev","dimensions":{"language":"en","region":"us"},"source":"evaluator","value":0.9}"#,
        )?;
        let second: Value = serde_json::from_str(
            r#"{"metric":"mrr","split":"dev","dimensions":{"region":"us","language":"en"},"source":"evaluator","value":0.8}"#,
        )?;
        let Err(error) = comparisons(
            &science,
            &json!({"comparisons":[first,second]}),
            &RequestContext::background(),
        ) else {
            return Err("duplicate comparison slice accepted".into());
        };
        assert_eq!(
            error.into_response().status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        Ok(())
    }
}
