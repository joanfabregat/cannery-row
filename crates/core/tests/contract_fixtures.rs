//! The published contract examples under tests/fixtures/contracts: every valid
//! example satisfies its schema, and every invalid one breaks it where its
//! `reason` says when that reason is a JSON Pointer.
#![allow(clippy::unwrap_used, clippy::panic, reason = "Fixture assertions")]
use cannery_core::{
    contracts::{
        ContractKind, ContractValidator,
        phases::{Phase, PhaseSchemas},
    },
    front_matter::Limits,
    json,
};
use serde_json::Value;
use std::{fs, path::Path};

const NESTING: usize = 128;

struct Schemas {
    contracts: ContractValidator,
    phases: PhaseSchemas,
}

impl Schemas {
    /// The violation paths of `document` against the named contract.
    fn paths(&self, name: &str, document: &Value) -> Vec<String> {
        if name == "verification" {
            return self
                .phases
                .violations(Phase::Verification, document)
                .into_iter()
                .map(|violation| violation.path)
                .collect();
        }
        let kind = ContractKind::from_name(name).unwrap_or_else(|| panic!("{name}"));
        let document = json::from_value(document.clone()).unwrap();
        self.contracts
            .document_violations(kind, &document)
            .unwrap()
            .into_iter()
            .map(|violation| violation.path)
            .collect()
    }
}

fn examples(directory: &Path) -> Vec<(String, Result<Value, ()>)> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut examples: Vec<_> = entries
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .map(|path| {
            let bytes = fs::read(&path).unwrap();
            let value = json::decode(&bytes, NESTING)
                .map_err(|_| ())
                .and_then(|document| json::to_value(&document).map_err(|_| ()));
            (path.display().to_string(), value)
        })
        .collect();
    examples.sort_by(|left, right| left.0.cmp(&right.0));
    examples
}

#[test]
fn published_examples_match_their_schemas() {
    let schemas = Schemas {
        contracts: ContractValidator::new().unwrap(),
        phases: PhaseSchemas::new().unwrap(),
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/contracts");
    let mut contracts: Vec<_> = fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    contracts.sort();
    let (mut valid, mut invalid) = (0, 0);
    for name in &contracts {
        for (path, value) in examples(&root.join(name).join("valid")) {
            let value = value.unwrap_or_else(|()| panic!("{path} is not JSON"));
            let paths = schemas.paths(name, &value);
            assert!(paths.is_empty(), "{path} breaks {name} at {paths:?}");
            valid += 1;
        }
        for (path, example) in examples(&root.join(name).join("invalid")) {
            invalid += 1;
            // A non-finite number is refused before any schema applies.
            let Ok(example) = example else { continue };
            let reason = example["reason"].as_str().unwrap_or_default().to_owned();
            let paths = schemas.paths(name, &example["document"]);
            assert!(!paths.is_empty(), "{path} satisfies {name}");
            if reason.starts_with('/') {
                assert!(
                    paths
                        .iter()
                        .any(|found| found.starts_with(&reason)
                            || reason.starts_with(found.as_str())),
                    "{path}: expected {reason}, found {paths:?}"
                );
            }
        }
    }
    assert!(valid > 0 && invalid > 0);
}

#[test]
fn completion_examples_carry_valid_reports_and_writeups() {
    let phases = PhaseSchemas::new().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/contracts/job_completion/valid");
    let completions = examples(&root);
    assert_ne!(completions.len(), 0);
    for (path, completion) in completions {
        let completion = completion.unwrap();
        let text = completion["document"].as_str().unwrap();
        // A document job is completed with a write-up, a verify job with its report.
        let phase = if path.ends_with("writeup.json") {
            Phase::Writeup
        } else {
            Phase::Verification
        };
        assert!(
            phases.parse(phase, text, Limits::default()).is_ok(),
            "{path}"
        );
    }
}

#[test]
fn decision_examples_carry_valid_decision_documents() {
    let phases = PhaseSchemas::new().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/contracts/human_decision/valid");
    let mut documents = 0;
    for (path, decision) in examples(&root) {
        if let Some(text) = decision.unwrap()["document"].as_str() {
            let parsed = phases.parse(Phase::Decision, text, Limits::default());
            assert!(
                parsed.is_ok_and(|document| !document.body.trim().is_empty()),
                "{path}"
            );
            documents += 1;
        }
    }
    assert_ne!(documents, 0);
}
