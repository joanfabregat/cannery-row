// SPDX-License-Identifier: AGPL-3.0-only
//! The per-phase schema registry: the JSON Schema of each phase output's front
//! matter, served whole so an editor or an agent can validate a document
//! before submitting it.
//!
//! A published phase schema references other published schemas
//! (`common.schema.json`, `evidence_envelope.schema.json`) by relative URI. The
//! registry bundles each one as a compound schema document: every schema it
//! reaches, directly or not, is embedded under `$defs` with its own `$id`, so
//! the document validates on its own with any JSON Schema 2020-12 validator.
use super::{ContractError, ContractViolation, formats, schema_errors};
use crate::front_matter::{self, FrontMatterError, Limits};
use jsonschema::Validator;
use serde_json::Value;
use std::collections::BTreeMap;

/// A phase whose output documents have a published front matter schema.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Phase {
    /// A project's brief: its goal, domain, constraints and conventions.
    Brief,
    /// A run: its claims, provenance and manifest, with run notes as the body.
    Run,
    /// A verification report: the verdict, its gates and the verified measurements.
    Verification,
    /// A unit's write-up: its summary, the attempts it covers and the
    /// verification report it reports on; or an imported retrospective report.
    Writeup,
    /// A unit's decision: its outcome and the verification report and
    /// write-up it cites, with the reason as the body.
    Decision,
    /// A concern about a track's plan: its kind and the unit or attempt
    /// it comes from, with the argument as the body.
    Concern,
}

impl Phase {
    pub const ALL: [Self; 6] = [
        Self::Brief,
        Self::Run,
        Self::Verification,
        Self::Writeup,
        Self::Decision,
        Self::Concern,
    ];

    /// The phase named in a URL or a tool argument; unknown names are absent.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|phase| phase.name() == name)
    }

    /// The phase's name, which is also its schema's file name stem.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Brief => "brief",
            Self::Run => "run",
            Self::Verification => "verification",
            Self::Writeup => "writeup",
            Self::Decision => "decision",
            Self::Concern => "concern",
        }
    }
}

/// Why a phase document was refused.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PhaseDocumentError {
    #[error(transparent)]
    FrontMatter(#[from] FrontMatterError),
    #[error("front matter does not satisfy the phase schema")]
    Invalid(Vec<ContractViolation>),
}

struct Entry {
    document: Value,
    validator: Validator,
}

/// The bundled schema and its compiled validator for every phase.
pub struct PhaseSchemas {
    phases: BTreeMap<Phase, Entry>,
}

impl PhaseSchemas {
    /// # Errors
    /// Rejects a published schema that is missing, invalid or cannot be bundled.
    pub fn new() -> Result<Self, ContractError> {
        let mut phases = BTreeMap::new();
        for phase in Phase::ALL {
            let document = super::published::bundled(phase.name())?;
            let validator = formats::options()
                .build(&document)
                .map_err(|_| ContractError::Schema)?;
            phases.insert(
                phase,
                Entry {
                    document,
                    validator,
                },
            );
        }
        Ok(Self { phases })
    }

    /// The phase's front matter schema, self-contained.
    #[must_use]
    pub fn schema(&self, phase: Phase) -> &Value {
        &self.phases[&phase].document
    }

    /// Where `front_matter` breaks the phase schema, by JSON Pointer; empty
    /// when it is valid. No value is disclosed.
    #[must_use]
    pub fn violations(&self, phase: Phase, front_matter: &Value) -> Vec<ContractViolation> {
        schema_errors(&self.phases[&phase].validator, front_matter)
    }

    /// Parse a phase document and validate its front matter.
    /// # Errors
    /// Refuses a document that is not Markdown with YAML front matter within
    /// `limits`, or whose front matter breaks the phase schema.
    pub fn parse(
        &self,
        phase: Phase,
        text: &str,
        limits: Limits,
    ) -> Result<front_matter::Document, PhaseDocumentError> {
        let document = front_matter::parse(text, limits)?;
        let violations = self.violations(phase, &Value::Object(document.front_matter.clone()));
        if violations.is_empty() {
            Ok(document)
        } else {
            Err(PhaseDocumentError::Invalid(violations))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

    fn schemas() -> Result<PhaseSchemas> {
        Ok(PhaseSchemas::new()?)
    }

    fn accepts(schemas: &PhaseSchemas, phase: Phase, value: &Value) -> bool {
        schemas.violations(phase, value).is_empty()
    }

    #[test]
    fn phase_names_round_trip() {
        for phase in Phase::ALL {
            assert_eq!(Phase::from_name(phase.name()), Some(phase));
        }
        assert_eq!(Phase::from_name("plan"), None);
        assert_eq!(Phase::from_name("Run"), None);
    }

    #[test]
    fn brief_front_matter_is_checked() -> Result {
        let schemas = schemas()?;
        let document = schemas.parse(
            Phase::Brief,
            "---\ntitle: Herring counts\ngoal: Count herring in sonar images within 5%.\n---\n## Domain\n\nSonar.\n",
            Limits::default(),
        )?;
        assert_eq!(document.front_matter["title"], "Herring counts");
        assert_eq!(document.body, "## Domain\n\nSonar.\n");
        assert!(accepts(
            &schemas,
            Phase::Brief,
            &json!({"title": "T", "goal": "One line,\nthen another."})
        ));
        for invalid in [
            json!({"title": "T"}),
            json!({"goal": "G"}),
            json!({"title": " ", "goal": "G"}),
            json!({"title": "T", "goal": ""}),
            json!({"title": "T", "goal": "One paragraph.\n\nThen a second."}),
            json!({"title": "T", "goal": "One paragraph.\n \nThen a second."}),
            json!({"title": "x".repeat(201), "goal": "G"}),
            json!({"title": "T", "goal": "G", "owner": "A"}),
        ] {
            assert!(!accepts(&schemas, Phase::Brief, &invalid), "{invalid}");
        }
        assert!(
            schemas.schema(Phase::Brief)["$defs"]
                .get("common.schema.json")
                .is_some()
        );
        Ok(())
    }

    #[test]
    fn bundled_schemas_are_self_contained() -> Result {
        let schemas = schemas()?;
        for phase in Phase::ALL {
            let document = schemas.schema(phase);
            let text = serde_json::to_string(document)?;
            // Every reference resolves inside the document: compiling it with
            // no registry and no retrieval succeeds.
            jsonschema::options()
                .with_draft(jsonschema::Draft::Draft202012)
                .offline()
                .build(document)?;
            assert!(text.contains("SPDX-License-Identifier: Apache-2.0"));
        }
        let run = schemas.schema(Phase::Run);
        assert!(run["$defs"]["evidence_envelope.schema.json"].is_object());
        assert!(run["$defs"]["common.schema.json"].is_object());
        assert!(
            schemas.schema(Phase::Writeup)["$defs"]
                .get("evidence_envelope.schema.json")
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn writeup_front_matter_is_checked() -> Result {
        let schemas = schemas()?;
        let document = schemas.parse(
            Phase::Writeup,
            "---\nkind: retrospective\nauthor: A. Researcher\nwritten_on: 2024-03-01\n---\n# Seed 1\n",
            Limits::default(),
        )?;
        assert_eq!(document.body, "# Seed 1\n");
        assert!(accepts(
            &schemas,
            Phase::Writeup,
            &json!({"kind": "retrospective", "author": "A", "written_at": "2024-03-01T10:00:00.5+00:00"})
        ));
        for invalid in [
            json!({"kind": "retrospective", "author": "A"}),
            json!({"kind": "retrospective", "author": "A", "written_on": "2024-03-01", "written_at": "2024-03-01T10:00:00Z"}),
            json!({"kind": "contemporary", "author": "A", "written_on": "2024-03-01"}),
            json!({"kind": "retrospective", "author": " ", "written_on": "2024-03-01"}),
            json!({"kind": "retrospective", "author": "A", "written_on": "2024-02-30"}),
            json!({"kind": "retrospective", "author": "A", "written_on": "2024-03-01", "extra": 1}),
        ] {
            assert!(!accepts(&schemas, Phase::Writeup, &invalid), "{invalid}");
        }
        assert!(matches!(
            schemas.parse(
                Phase::Writeup,
                "---\nkind: retrospective\n---\n",
                Limits::default()
            ),
            Err(PhaseDocumentError::Invalid(_))
        ));
        assert!(matches!(
            schemas.parse(Phase::Writeup, "no front matter", Limits::default()),
            Err(PhaseDocumentError::FrontMatter(
                FrontMatterError::MissingFrontMatter
            ))
        ));
        let sha = "a".repeat(64);
        let written = schemas.parse(
            Phase::Writeup,
            &format!(
                "---\nsummary: The bigram model beats the control on every language.\nattempts: [1, 2]\nverification: {{ref: 3f1c2a9e-7b4d-4e0a-9c55-1d2e3f4a5b6c, sha256: \"{sha}\"}}\n---\n\n# What was done\n"
            ),
            Limits::default(),
        )?;
        assert_eq!(written.body, "\n# What was done\n");
        assert!(accepts(
            &schemas,
            Phase::Writeup,
            &json!({"summary": "Stopped after the run failed twice.", "attempts": [1], "verification": null})
        ));
        for invalid in [
            json!({"summary": "Two\nsentences.", "attempts": [1], "verification": null}),
            json!({"summary": " ", "attempts": [1], "verification": null}),
            json!({"summary": "S.", "attempts": [], "verification": null}),
            json!({"summary": "S.", "attempts": [0], "verification": null}),
            json!({"summary": "S.", "attempts": [1, 1], "verification": null}),
            json!({"summary": "S.", "attempts": [1]}),
            json!({"summary": "S.", "attempts": [1], "verification": {"ref": "x"}}),
            json!({"summary": "S.", "attempts": [1], "verification": null, "author": "A"}),
        ] {
            assert!(!accepts(&schemas, Phase::Writeup, &invalid), "{invalid}");
        }
        Ok(())
    }

    #[test]
    fn decision_front_matter_is_checked() -> Result {
        let schemas = schemas()?;
        let sha = "b".repeat(64);
        let document = schemas.parse(
            Phase::Decision,
            &format!(
                "---\noutcome: promote\nverification: {{ref: v1, sha256: \"{sha}\"}}\nwriteup: null\n---\n\nAll gates pass.\n"
            ),
            Limits::default(),
        )?;
        assert_eq!(document.body, "\nAll gates pass.\n");
        for invalid in [
            json!({"outcome": "promoted", "verification": null, "writeup": null}),
            json!({"outcome": "failed", "verification": null}),
            json!({"outcome": "failed", "verification": null, "writeup": null, "reason": "r"}),
            json!({"outcome": "reject", "verification": {"ref": "v1"}, "writeup": null}),
        ] {
            assert!(!accepts(&schemas, Phase::Decision, &invalid), "{invalid}");
        }
        Ok(())
    }

    #[test]
    fn concern_front_matter_is_checked() -> Result {
        let schemas = schemas()?;
        let document = schemas.parse(
            Phase::Concern,
            "---\nkind: wrong_assumption\nunit: 12\nattempt: 2\n---\n\nThe tokenizer is not the bottleneck.\n",
            Limits::default(),
        )?;
        assert_eq!(document.front_matter["kind"], "wrong_assumption");
        assert!(accepts(&schemas, Phase::Concern, &json!({"kind": "other"})));
        for invalid in [
            json!({}),
            json!({"kind": "question"}),
            json!({"kind": "blocker", "attempt": 1}),
            json!({"kind": "blocker", "unit": 0}),
            json!({"kind": "blocker", "unit": "#12"}),
            json!({"kind": "blocker", "track": "t"}),
        ] {
            assert!(!accepts(&schemas, Phase::Concern, &invalid), "{invalid}");
        }
        Ok(())
    }

    #[test]
    fn verification_reports_need_a_verdict_their_gates_support() -> Result {
        let schemas = schemas()?;
        let report = |verdict: &str, gate: &str| {
            json!({
                "verdict": verdict,
                "reason": "the gate decided",
                "policy_revision": "policy-1",
                "gates": [{"id": "quality", "result": gate}],
                "measurements": [{
                    "metric": "mrr", "value": 0.5, "authority": "tester_verified",
                    "unit": "ratio", "direction": "higher", "split": "dev"
                }],
                "provenance": {"source_revision": "abc", "science_revision": "1"}
            })
        };
        assert!(accepts(
            &schemas,
            Phase::Verification,
            &report("pass", "pass")
        ));
        assert!(accepts(
            &schemas,
            Phase::Verification,
            &report("fail", "fail")
        ));
        assert!(accepts(
            &schemas,
            Phase::Verification,
            &report("inconclusive", "unknown")
        ));
        assert!(!accepts(
            &schemas,
            Phase::Verification,
            &report("pass", "unknown")
        ));
        let mut claimed = report("fail", "fail");
        claimed["measurements"][0]["authority"] = json!("agent_claim");
        assert!(!accepts(&schemas, Phase::Verification, &claimed));
        let mut stray = report("fail", "fail");
        stray["stage"] = json!("verification");
        assert!(!accepts(&schemas, Phase::Verification, &stray));
        assert!(!accepts(&schemas, Phase::Run, &report("pass", "pass")));
        assert!(!accepts(&schemas, Phase::Verification, &json!({})));
        Ok(())
    }
}
