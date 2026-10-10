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
                    "does not match the verified object",
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
            .ok_or_else(|| invalid(&format!("{path}/metric"), "unknown metric"))?;
        if !array(metric, "splits", r)?.contains(&m["split"]) {
            return Err(invalid(
                &format!("{path}/split"),
                "unregistered metric split",
            ));
        }
        for field in ["unit", "direction"] {
            if m[field] != metric[field] {
                return Err(invalid(
                    &format!("{path}/{field}"),
                    "does not match the registered metric",
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
                        "unregistered metric dimension",
                    )
                })?;
            if let Some(allowed) = registered.get("values").and_then(Value::as_array)
                && !allowed.contains(value)
            {
                return Err(invalid(
                    &format!("{path}/dimensions/{name}"),
                    "unregistered dimension value",
                ));
            }
        }
    }
    // Each explicitly required dimension slice must appear in the report.
    for metric in science["metrics"].as_array().into_iter().flatten() {
        for slice in metric["required_slices"].as_array().into_iter().flatten() {
            let splits = slice
                .get("splits")
                .and_then(Value::as_array)
                .filter(|v| !v.is_empty())
                .or_else(|| metric["splits"].as_array());
            for required in slice["values"].as_array().into_iter().flatten() {
                for split in splits.into_iter().flatten() {
                    if !report["measurements"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|m| {
                            m["metric"] == metric["key"]
                                && m["split"] == *split
                                && m["dimensions"].as_object().is_some_and(|dimensions| {
                                    dimensions.len() == 1
                                        && slice["dimension"].as_str().is_some_and(|name| {
                                            dimensions.get(name) == Some(required)
                                        })
                                })
                        })
                    {
                        return Err(invalid("/measurements", "required slices are missing"));
                    }
                }
            }
        }
    }
    Ok(())
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

/// The dataset revisions the job's scorer reads; held-out label datasets alone when it reads any.
fn scored_datasets(
    spec: &Value,
    science: &Value,
    r: &RequestContext,
) -> Result<BTreeSet<String>, Failure> {
    let inputs = &spec["inputs"];
    let scorer = array(spec, "steps", r)?
        .iter()
        .map(|v| &v["manifest"])
        .find(|v| v["spec"]["role"] == "scorer")
        .ok_or_else(|| internal(r, "job scorer missing"))?;
    let mut read = BTreeSet::new();
    let mut held = BTreeSet::new();
    for artifact in scorer["spec"]["inputs"]["artifacts"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if artifact["from"] == "dataset" {
            let name = artifact
                .get("id")
                .or_else(|| artifact.get("name"))
                .and_then(Value::as_str)
                .ok_or_else(|| internal(r, "scorer dataset input"))?;
            if let Some(pinned) = array(inputs, "datasets", r)?
                .iter()
                .find(|v| v["id"] == name)
            {
                let revision = string(pinned, "revision", r)?.to_owned();
                read.insert(revision.clone());
                if science["datasets"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|v| v["id"] == name && v["held_out_labels"] == true)
                {
                    held.insert(revision);
                }
            }
        }
    }
    Ok(if held.is_empty() { read } else { held })
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
    let pinned_revision = j.science_revision.to_string();
    if provenance["science_revision"].as_str() != Some(pinned_revision.as_str()) {
        return Err(invalid(
            "/provenance/science_revision",
            "must match the job's pinned science revision",
        ));
    }
    if provenance["source_revision"] != run["provenance"]["source_revision"] {
        return Err(invalid(
            "/provenance/source_revision",
            "must match the run's source revision",
        ));
    }
    let control = cannery_research::job_baselines::pinned_control(&j.spec, s.flow.rendering)
        .map_err(|_| internal(r, "job control"))?
        .map(|control| flow::value(&control, &s.flow, r))
        .transpose()?;
    if let Some(control) = &control
        && provenance["control_revision"] != control["revision"]
    {
        return Err(invalid(
            "/provenance/control_revision",
            "must match the job's pinned control",
        ));
    }
    let scored = scored_datasets(&spec, &science, r)?;
    let dataset = provenance.get("dataset_revision").and_then(Value::as_str);
    if dataset.map_or(!scored.is_empty(), |revision| !scored.contains(revision)) {
        return Err(invalid(
            "/provenance/dataset_revision",
            "must match a scored dataset revision",
        ));
    }
    measurements(&science, &report, r)?;
    extensions(&science, &report, s, r)?;
    if j.performer == Performer::Runner && report["policy_revision"] != spec["verifier"]["revision"]
    {
        return Err(invalid(
            "/policy_revision",
            "must match the registered verifier's policy revision",
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
                "does not match this attempt",
            ));
        }
        manifest_objects(manifest, &artifacts)?
    } else {
        BTreeSet::new()
    };
    for role in science["required_artifact_roles"]["verify"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if !role.as_str().is_some_and(|v| roles.contains(v)) {
            return Err(invalid("/manifest", "the manifest lacks required roles"));
        }
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
