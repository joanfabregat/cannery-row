//! Semantic binding of completed records to verified job inputs and outputs.
#![allow(
    clippy::many_single_char_names,
    reason = "Explicit borrowed transaction and request contexts"
)]
use crate::{
    attempt_lease_routes::{Failure, internal},
    job_completion_routes::{JobLifecycleContext, invalid},
    job_lifecycle as flow,
    requests::RequestContext,
};
use cannery_attempts::{
    model::{Artifact, Attempt, EvidenceId, StoredJson},
    repo::Repository,
};
use cannery_core::{
    contracts::instance::{self, ProjectValidationFailure},
    json::Document,
};
use cannery_jobs::repo::{Job, Stage};
use serde_json::{Value, json};
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
    evidence: &Value,
    s: &JobLifecycleContext,
    r: &RequestContext,
) -> Result<(), Failure> {
    let Some(schema) = science.get("result_extensions").filter(|v| !v.is_null()) else {
        return Ok(());
    };
    let schema = Arc::new(flow::document(schema, &s.flow, r)?);
    let content = flow::document(evidence.get("extensions").unwrap_or(&json!({})), &s.flow, r)?;
    match instance::validate_project_fields(&schema, &content) {
        Ok(()) => Ok(()),
        Err(ProjectValidationFailure::Document(errors)) => {
            let path = errors
                .first()
                .and_then(|v| v.path.as_utf8())
                .unwrap_or_default();
            Err(invalid(
                &format!("/evidence/extensions{path}"),
                "result extension violates its pinned schema",
            ))
        }
        Err(_) => Err(internal(r, "result extension schema")),
    }
}
fn measurements(science: &Value, evidence: &Value, r: &RequestContext) -> Result<(), Failure> {
    for (i, m) in evidence["measurements"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let path = format!("/evidence/measurements/{i}");
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
    // Each explicitly required dimension slice must appear in the record.
    for metric in science["metrics"].as_array().into_iter().flatten() {
        for slice in metric["required_slices"].as_array().into_iter().flatten() {
            let splits = slice
                .get("splits")
                .and_then(Value::as_array)
                .filter(|v| !v.is_empty())
                .or_else(|| metric["splits"].as_array());
            for required in slice["values"].as_array().into_iter().flatten() {
                for split in splits.into_iter().flatten() {
                    if !evidence["measurements"]
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
                        return Err(invalid(
                            "/evidence/measurements",
                            "required slices are missing",
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn comparisons(
    science: &Value,
    assessment: &Value,
    tested: &Value,
    r: &RequestContext,
) -> Result<(), Failure> {
    cannery_research::comparisons::check(science, assessment, tested).map_err(|error| match error {
        cannery_research::comparisons::ComparisonError::Invalid { path, message } => {
            invalid(&path, message)
        }
        cannery_research::comparisons::ComparisonError::Shape => {
            internal(r, "comparison input shape")
        }
    })
}
#[allow(
    clippy::too_many_arguments,
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
) -> Result<(Document, Option<Document>), Failure> {
    let completion = flow::value(d, &s.flow, r)?;
    if completion["job_id"] != j.id.to_string() {
        return Err(invalid("/job_id", format!("this is job {}", j.id)));
    }
    let evidence = &completion["evidence"];
    let stage = j.stage.as_str();
    if evidence["stage"] != stage {
        return Err(invalid(
            "/evidence/stage",
            format!(
                "a {} job completes with {stage}-stage evidence",
                if j.stage == Stage::Tester {
                    "test"
                } else {
                    "evaluation"
                }
            ),
        ));
    }
    if evidence["status"] != "completed" {
        return Err(invalid(
            "/evidence/status",
            "report a failed run through the job's failure",
        ));
    }
    if evidence["attempt_id"] != a.id.to_string() {
        return Err(invalid(
            "/evidence/attempt_id",
            format!("this job runs attempt {}", a.id),
        ));
    }
    let spec = flow::value(&j.spec, &s.flow, r)?;
    let service = &spec[stage];
    if evidence["producer"] != json!({"kind":"service","id":service["id"]}) {
        return Err(invalid("/evidence/producer", "not the registered service"));
    }
    let inputs = &spec["inputs"];
    let reference = if j.stage == Stage::Tester {
        &inputs["claimed_sheet"]
    } else {
        array(inputs, "evidence", r)?
            .first()
            .ok_or_else(|| internal(r, "evaluation tester input"))?
    };
    let id = uuid::Uuid::parse_str(string(reference, "ref", r)?)
        .map_err(|_| internal(r, "job evidence reference"))?;
    let tested = Repository::new(c, s.flow.attempts)
        .get_evidence_by_id(a.id, EvidenceId(id))
        .await
        .map_err(|_| internal(r, "job verified evidence"))?
        .ok_or_else(|| internal(r, "job verified evidence missing"))?;
    let StoredJson::Value(tested) = tested.0 else {
        return Err(internal(r, "job verified evidence shape"));
    };
    let tested = flow::value(&tested, &s.flow, r)?;
    let raw = flow::science(c, a, &s.flow, r).await?;
    let science = flow::value(&raw.content, &s.flow, r)?;
    let provenance = &evidence["provenance"];
    let pinned_revision = j.science_revision.to_string();
    if provenance["science_revision"].as_str() != Some(pinned_revision.as_str()) {
        return Err(invalid(
            "/evidence/provenance/science_revision",
            "must match the job's pinned science revision",
        ));
    }
    if provenance["source_revision"] != tested["provenance"]["source_revision"] {
        return Err(invalid(
            "/evidence/provenance/source_revision",
            "must match the verified source revision",
        ));
    }
    if j.stage == Stage::Tester {
        let control = cannery_research::job_baselines::pinned_control(&j.spec, s.flow.rendering)
            .map_err(|_| internal(r, "job control"))?;
        if let Some(control) = control {
            let control = flow::value(&control, &s.flow, r)?;
            if provenance["control_revision"] != control["revision"] {
                return Err(invalid(
                    "/evidence/provenance/control_revision",
                    "must match the job's pinned control",
                ));
            }
        }
        if let Some(revision) = service.get("revision")
            && provenance["tester_revision"] != *revision
        {
            return Err(invalid(
                "/evidence/provenance/tester_revision",
                "must match the registered tester",
            ));
        }
        let scorer = array(&spec, "steps", r)?
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
        let allowed = if held.is_empty() { read } else { held };
        if !provenance["dataset_revision"]
            .as_str()
            .is_some_and(|v| allowed.contains(v))
        {
            return Err(invalid(
                "/evidence/provenance/dataset_revision",
                "must match a scored dataset revision",
            ));
        }
        measurements(&science, evidence, r)?;
        extensions(&science, evidence, s, r)?;
    } else {
        for field in ["dataset_revision", "control_revision"] {
            if provenance.get(field) != tested["provenance"].get(field) {
                return Err(invalid(
                    &format!("/evidence/provenance/{field}"),
                    "must match the verified tester provenance",
                ));
            }
        }
        let assessment = &evidence["assessment"];
        if assessment["policy_revision"] != service["revision"] {
            return Err(invalid(
                "/evidence/assessment/policy_revision",
                "must match the registered evaluator's policy revision",
            ));
        }
        for (i, v) in array(assessment, "evidence", r)?.iter().enumerate() {
            if !array(inputs, "evidence", r)?
                .iter()
                .any(|known| known["ref"] == v["ref"] && known["sha256"] == v["sha256"])
            {
                return Err(invalid(
                    &format!("/evidence/assessment/evidence/{i}"),
                    "not a verified evidence record of this job's inputs",
                ));
            }
        }
        let mut gates = BTreeSet::new();
        for gate in array(assessment, "gates", r)? {
            if !gates.insert(string(gate, "id", r)?) {
                return Err(invalid(
                    "/evidence/assessment/gates",
                    "each gate is assessed once",
                ));
            }
        }
        // A policy step may cite any of the job's pinned tester records. Bind
        // comparisons to that complete set, just as the evaluator worker does.
        let mut measurements = tested["measurements"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for reference in array(inputs, "evidence", r)?.iter().skip(1) {
            let id = uuid::Uuid::parse_str(string(reference, "ref", r)?)
                .map_err(|_| internal(r, "job evidence reference"))?;
            let verified = Repository::new(c, s.flow.attempts)
                .get_evidence_by_id(a.id, EvidenceId(id))
                .await
                .map_err(|_| internal(r, "job verified evidence"))?
                .ok_or_else(|| internal(r, "job verified evidence missing"))?;
            let StoredJson::Value(verified) = verified.0 else {
                return Err(internal(r, "job verified evidence shape"));
            };
            let verified = flow::value(&verified, &s.flow, r)?;
            if let Some(values) = verified.get("measurements") {
                measurements.extend(
                    values
                        .as_array()
                        .ok_or_else(|| internal(r, "job verified measurement shape"))?
                        .iter()
                        .cloned(),
                );
            }
        }
        comparisons(
            &science,
            assessment,
            &json!({"measurements":measurements}),
            r,
        )?;
        if evidence.get("extensions").is_some() {
            extensions(&science, evidence, s, r)?;
        }
    }
    let artifacts = Repository::new(c, s.flow.attempts)
        .list_job_artifacts(j.id)
        .await
        .map_err(|_| internal(r, "completion verified artifacts"))?;
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
    if j.stage == Stage::Tester {
        for role in science["required_artifact_roles"]["tester"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if !role.as_str().is_some_and(|v| roles.contains(v)) {
                return Err(invalid("/manifest", "the manifest lacks required roles"));
            }
        }
    }
    for (i, role) in evidence["artifact_roles"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        if !role.as_str().is_some_and(|v| roles.contains(v)) {
            return Err(invalid(
                &format!("/evidence/artifact_roles/{i}"),
                "the manifest has no object with this role",
            ));
        }
    }
    Ok((
        flow::document(evidence, &s.flow, r)?,
        manifest
            .map(|v| flow::document(v, &s.flow, r))
            .transpose()?,
    ))
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
            &json!({}),
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
