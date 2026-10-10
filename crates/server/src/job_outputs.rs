// SPDX-License-Identifier: AGPL-3.0-only
//! What a job's completion must carry, from the job's pinned science
//! revision and inputs: the artifact roles its manifest needs and, for a
//! verify job, the values its verification report must name. The claim,
//! the verifier's context bundle and the completion checks share them, so
//! what a performer is told is what the server checks.
use crate::api_models::{ArtifactRoleOut, JobOutputGuide, VerificationExpected};
use cannery_jobs::repo::{Job, Performer, Phase};
use serde_json::Value;
use sqlx::PgConnection;
use std::{collections::BTreeSet, fmt::Write as _};

/// One artifact role a science revision requires, with what goes in it
/// when the revision says.
#[derive(Clone)]
pub(crate) struct RequiredRole {
    pub(crate) role: String,
    pub(crate) description: Option<String>,
}

/// The roles `required_artifact_roles.<kind>` (`attempt` or `verify`)
/// names: each entry is a role name, or `{role, description}`.
pub(crate) fn required_roles(science: &Value, kind: &str) -> Vec<RequiredRole> {
    science["required_artifact_roles"][kind]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| match entry {
            Value::String(role) => Some(RequiredRole {
                role: role.clone(),
                description: None,
            }),
            Value::Object(fields) => {
                fields
                    .get("role")
                    .and_then(Value::as_str)
                    .map(|role| RequiredRole {
                        role: role.to_owned(),
                        description: fields
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    })
            }
            _ => None,
        })
        .collect()
}

/// The role names alone, in the revision's order.
pub(crate) fn names(roles: &[RequiredRole]) -> Vec<&str> {
    roles.iter().map(|role| role.role.as_str()).collect()
}

/// What follows a colon: a Markdown list of the roles, each with its
/// description, or ` none`.
pub(crate) fn role_list(roles: &[RequiredRole]) -> String {
    if roles.is_empty() {
        return String::from(" none\n");
    }
    let mut list = String::from("\n\n");
    for role in roles {
        let _ = write!(list, "- `{}`", role.role);
        if let Some(description) = &role.description {
            let _ = write!(list, ": {}", description.trim());
        }
        list.push('\n');
    }
    list
}

/// The roles as the claim states them.
pub(crate) fn role_models(roles: &[RequiredRole]) -> Vec<ArtifactRoleOut> {
    roles
        .iter()
        .map(|role| ArtifactRoleOut {
            role: role.role.clone(),
            description: role.description.clone(),
        })
        .collect()
}

/// `"a", "b" and "c"`, for messages that give the expected values.
pub(crate) fn quoted_list(values: &[&str]) -> String {
    let quoted: Vec<String> = values.iter().map(|value| format!("\"{value}\"")).collect();
    match quoted.as_slice() {
        [] => String::from("none"),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

/// What a verify job's report must name and its manifest hold.
pub(crate) struct VerifyExpectations {
    pub(crate) performer: Performer,
    pub(crate) science_revision: String,
    /// The run's `provenance.source_revision`, as the run gave it.
    pub(crate) source_revision: Value,
    /// A runner's registered policy revision; for an agent verify job, the
    /// pinned science revision, which registers agent verification.
    pub(crate) policy_revision: String,
    /// The unit's pinned control: its id and revision.
    pub(crate) control: Option<(String, String)>,
    /// The dataset revisions `provenance.dataset_revision` may name; none
    /// when the scorer reads no registered dataset, and it is then left out.
    pub(crate) datasets: BTreeSet<String>,
    pub(crate) roles: Vec<RequiredRole>,
    /// The run's `claims`, as the run gave them.
    pub(crate) claims: Value,
}

impl VerifyExpectations {
    /// From the job's specification, its science revision, the run's front
    /// matter and its pinned control (`{id, revision}`), as JSON values.
    pub(crate) fn new(
        job: &Job,
        spec: &Value,
        science: &Value,
        run: &Value,
        control: Option<&Value>,
    ) -> Option<Self> {
        let science_revision = job.science_revision.to_string();
        let policy_revision = match job.performer {
            Performer::Runner => spec["verifier"]["revision"].as_str()?.to_owned(),
            Performer::Agent => science_revision.clone(),
        };
        let control = match control.filter(|control| !control.is_null()) {
            Some(control) => Some((text(&control["id"])?, text(&control["revision"])?)),
            None => None,
        };
        Some(Self {
            performer: job.performer,
            science_revision,
            source_revision: run["provenance"]["source_revision"].clone(),
            policy_revision,
            control,
            datasets: scored_datasets(spec, science)?,
            roles: required_roles(science, "verify"),
            claims: run["claims"].clone(),
        })
    }

    /// The dataset revisions, sorted, as text.
    pub(crate) fn dataset_list(&self) -> Vec<&str> {
        self.datasets.iter().map(String::as_str).collect()
    }

    /// The values as the claim states them.
    pub(crate) fn model(&self) -> VerificationExpected {
        VerificationExpected {
            policy_revision: self.policy_revision.clone(),
            science_revision: self.science_revision.clone(),
            source_revision: self.source_revision.as_str().map(str::to_owned),
            control_revision: self.control.as_ref().map(|(_, revision)| revision.clone()),
            dataset_revisions: self.datasets.iter().cloned().collect(),
        }
    }
}

fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// The dataset revisions the job's scorer reads; held-out label datasets
/// alone when it reads any. None when the specification is malformed.
pub(crate) fn scored_datasets(spec: &Value, science: &Value) -> Option<BTreeSet<String>> {
    let inputs = &spec["inputs"];
    let scorer = spec["steps"]
        .as_array()?
        .iter()
        .map(|v| &v["manifest"])
        .find(|v| v["spec"]["role"] == "scorer")?;
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
                .and_then(Value::as_str)?;
            if let Some(pinned) = inputs["datasets"]
                .as_array()?
                .iter()
                .find(|v| v["id"] == name)
            {
                let revision = pinned["revision"].as_str()?.to_owned();
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
    Some(if held.is_empty() { read } else { held })
}

/// Keeps a completed job's or a submitted run's outputs: the uploads its
/// manifest lists, by storage identity, besides the transcript the server
/// seals and imported records. An upload the manifest leaves out is not
/// an output. Keeps everything when the manifest lists no objects.
pub(crate) fn keep_listed(
    manifest: &cannery_attempts::model::StoredJson,
    artifacts: &mut Vec<cannery_attempts::model::Artifact>,
) {
    let content = match manifest {
        cannery_attempts::model::StoredJson::Value(document) => {
            cannery_core::json::to_value(document).unwrap_or_default()
        }
        cannery_attempts::model::StoredJson::SqlNull => Value::Null,
    };
    let Some(objects) = content["objects"].as_array() else {
        return;
    };
    let listed: BTreeSet<(&str, &str, &str)> = objects
        .iter()
        .filter_map(|object| {
            let storage = &object["storage"];
            Some((
                storage["backend"].as_str()?,
                storage["bucket"].as_str()?,
                storage["key"].as_str()?,
            ))
        })
        .collect();
    artifacts.retain(|artifact| {
        artifact.role == "transcript"
            || artifact.origin != cannery_attempts::model::Origin::Live
            || listed.contains(&(
                artifact.backend.as_str(),
                artifact.bucket.as_str(),
                artifact.key.as_str(),
            ))
    });
}

/// The JSON contexts these reads decode stored documents with.
const DEPTH: usize = cannery_core::json::MAX_DEPTH;

/// A verify job's expectations, read from the database: its pinned science
/// revision, its run's front matter and its pinned control.
pub(crate) async fn load_verify(conn: &mut PgConnection, job: &Job) -> Option<VerifyExpectations> {
    let spec = cannery_core::json::to_value(&job.spec).ok()?;
    let (_, science) = crate::context_bundle::pinned_science(conn, job.attempt_id)
        .await
        .ok()?;
    let id = uuid::Uuid::parse_str(spec["inputs"]["run"]["ref"].as_str()?).ok()?;
    let (run, _) = cannery_attempts::repo::Repository::new(
        &mut *conn,
        cannery_attempts::model::JsonContext {
            encode_nesting_budget: DEPTH,
            decode_nesting_budget: DEPTH,
        },
    )
    .get_evidence_by_id(job.attempt_id, cannery_attempts::model::EvidenceId(id))
    .await
    .ok()??;
    let run = match run {
        cannery_attempts::model::StoredJson::Value(run) => {
            cannery_core::json::to_value(&run).ok()?
        }
        cannery_attempts::model::StoredJson::SqlNull => Value::Null,
    };
    let control = cannery_research::job_baselines::pinned_control(
        &job.spec,
        cannery_research::science::RenderingContext {
            nesting_budget: DEPTH,
        },
    )
    .ok()?
    .map(|control| cannery_core::json::to_value(&control))
    .transpose()
    .ok()?;
    VerifyExpectations::new(job, &spec, &science, &run, control.as_ref())
}

/// What `complete_job` takes for a claimed job: its document's contract,
/// whether it carries a manifest, the roles that manifest needs and, for a
/// verify job, the values the report must name.
pub(crate) async fn guide(conn: &mut PgConnection, job: &Job) -> JobOutputGuide {
    match job.phase {
        Phase::Verify => {
            if let Some(expected) = load_verify(conn, job).await {
                return JobOutputGuide {
                    document: String::from("verification"),
                    manifest: true,
                    required_roles: role_models(&expected.roles),
                    expected: Some(expected.model()),
                };
            }
            // A job whose specification names no scorer or run states the
            // roles alone; its completion is refused with the details.
            let roles = crate::context_bundle::pinned_science(conn, job.attempt_id)
                .await
                .map(|(_, science)| required_roles(&science, "verify"))
                .unwrap_or_default();
            JobOutputGuide {
                document: String::from("verification"),
                manifest: true,
                required_roles: role_models(&roles),
                expected: None,
            }
        }
        Phase::Document => JobOutputGuide {
            document: String::from("writeup"),
            manifest: false,
            required_roles: vec![],
            expected: None,
        },
        Phase::Decide => JobOutputGuide {
            document: String::from("decision"),
            manifest: false,
            required_roles: vec![],
            expected: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn roles_are_names_or_described_objects() {
        let science = json!({"required_artifact_roles":{"attempt":["candidate",{"role":"log","description":"The training log."}],"verify":[]}});
        let roles = required_roles(&science, "attempt");
        assert_eq!(names(&roles), ["candidate", "log"]);
        assert_eq!(roles[0].description, None);
        assert_eq!(roles[1].description.as_deref(), Some("The training log."));
        assert_eq!(
            role_list(&roles),
            "\n\n- `candidate`\n- `log`: The training log.\n"
        );
        assert!(required_roles(&science, "verify").is_empty());
        assert_eq!(role_list(&[]), " none\n");
    }

    fn artifact(
        role: &str,
        key: &str,
        origin: cannery_attempts::model::Origin,
    ) -> cannery_attempts::model::Artifact {
        cannery_attempts::model::Artifact {
            id: cannery_attempts::model::ArtifactId(uuid::Uuid::nil()),
            attempt_id: cannery_core::ids::AttemptId(uuid::Uuid::nil()),
            role: role.to_owned(),
            backend: String::from("local"),
            bucket: String::from("b"),
            key: key.to_owned(),
            generation: None,
            size_bytes: 1,
            sha256: "a".repeat(64),
            media_type: String::from("text/plain"),
            verified_at: cannery_core::timestamps::Timestamp(
                chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z").unwrap_or_default(),
            ),
            job_id: None,
            interface: None,
            content_validated: None,
            origin,
            source_ref: None,
            uri: None,
        }
    }

    #[test]
    fn only_listed_uploads_are_outputs() {
        use cannery_attempts::model::{Origin, StoredJson};
        let manifest = StoredJson::Value(std::sync::Arc::new(
            cannery_core::json::from_value(json!({"objects":[{"role":"evidence","storage":{"backend":"local","bucket":"b","key":"listed"}}]}))
                .unwrap_or_else(|_| unreachable!()),
        ));
        let mut artifacts = vec![
            artifact("evidence", "listed", Origin::Live),
            artifact("evidence", "stray", Origin::Live),
            artifact("transcript", "transcript", Origin::Live),
        ];
        keep_listed(&manifest, &mut artifacts);
        let keys: Vec<&str> = artifacts.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(keys, ["listed", "transcript"]);
        // A manifest without objects leaves the artifacts as they are.
        let empty = StoredJson::Value(std::sync::Arc::new(
            cannery_core::json::from_value(json!({})).unwrap_or_else(|_| unreachable!()),
        ));
        let mut artifacts = vec![artifact("evidence", "stray", Origin::Live)];
        keep_listed(&empty, &mut artifacts);
        assert_eq!(artifacts.len(), 1);
    }

    #[test]
    fn quoted_lists_read_as_alternatives() {
        assert_eq!(quoted_list(&[]), "none");
        assert_eq!(quoted_list(&["r1"]), "\"r1\"");
        assert_eq!(quoted_list(&["a", "b", "c"]), "\"a\", \"b\" or \"c\"");
    }
}
