#![forbid(unsafe_code)]
// Fixture/test setup fails immediately on malformed oracle data or unexpected filesystem state.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::json::{self, Document, Node};
use cannery_runner::job::{self, JobContext, JobError, JobKind};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};
const BUDGET: usize = 128;
static REFERENCE: std::sync::LazyLock<&str> = std::sync::LazyLock::new(|| {
    runtime_reference!("/../../crates/runner/tests/fixtures/runner_job_reference.json")
});
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "cannery-job-reference-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove owned scratch");
    }
}
fn document(text: &str) -> Document {
    json::decode(text.as_bytes(), BUDGET).unwrap()
}
fn pretty(doc: &Document) -> String {
    json::encode_ascii_pretty(doc, BUDGET).unwrap()
}
fn assert_invalid_native_documents(case: &Value) -> bool {
    let invalid = [
        case["claim"]["json"].as_str().unwrap(),
        case["step"]["json"].as_str().unwrap(),
    ]
    .into_iter()
    .chain(case["metrics_json"].as_str())
    .filter(|text| serde_json::from_str::<Value>(text).is_err())
    .collect::<Vec<_>>();
    for text in &invalid {
        assert!(json::decode(text.as_bytes(), BUDGET).is_err());
    }
    !invalid.is_empty()
}
fn keys(doc: &Document, inputs: bool) -> Vec<String> {
    let root = if inputs {
        doc.field(doc.root(), "inputs").unwrap()
    } else {
        doc.root()
    };
    let Node::Object(fields) = doc.node(root).unwrap() else {
        panic!("expected object")
    };
    fields
        .iter()
        .map(|(key, _)| key.as_utf8().unwrap())
        .collect()
}
fn snapshot(root: &Path) -> Vec<Value> {
    fn visit(root: &Path, path: &Path, items: &mut Vec<(String, &'static str)>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            let directory = path.is_dir();
            items.push((
                path.strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned(),
                if directory { "directory" } else { "file" },
            ));
            if directory {
                visit(root, &path, items);
            }
        }
    }
    let mut items = Vec::new();
    if root.exists() {
        visit(root, root, &mut items);
    }
    items.sort();
    items
        .into_iter()
        .map(|(path, kind)| json!({"path":path,"kind":kind}))
        .collect()
}
#[test]
fn all_nineteen_dataset_recipes_are_asserted() {
    let reference: Value = serde_json::from_str(*REFERENCE).unwrap();
    let recipes = reference["datasets"].as_array().unwrap();
    assert_eq!(recipes.len(), 19);
    let mut successes = 0;
    let mut failures = 0;
    for case in recipes {
        let step_json = case["step_json"].as_str().unwrap();
        let pinned_json = case["pinned_json"].as_str().unwrap();
        if serde_json::from_str::<Value>(step_json).is_err()
            || serde_json::from_str::<Value>(pinned_json).is_err()
        {
            assert!(
                json::decode(step_json.as_bytes(), BUDGET).is_err()
                    || json::decode(pinned_json.as_bytes(), BUDGET).is_err()
            );
            continue;
        }
        let step = document(case["step_json"].as_str().unwrap());
        let pinned = document(case["pinned_json"].as_str().unwrap());
        let result = job::declared_datasets(&step, &pinned, BUDGET);
        if case["name"] == "explicit-null-id" || case["name"] == "dict-id-order-is-significant" {
            assert_eq!(pretty(&result.unwrap()), "[]");
            continue;
        }
        if case["name"] == "dict-id-and-list-name-python-repr" {
            let actual = serde_json::from_str::<Value>(&pretty(&result.unwrap())).unwrap();
            assert_eq!(actual[0]["name"], "[\"a'b\",\"\u{2028}\"]");
            assert_eq!(actual[0]["id"], json!({"a":null,"b":false}));
            assert_eq!(actual[0]["revision"], "r1");
            continue;
        }
        let expected = &case["observed"];
        if let Some(exception) = expected["exception"].as_str() {
            assert_eq!(
                result.unwrap_err().python_exception(),
                exception,
                "{}",
                case["name"]
            );
            failures += 1;
        } else {
            let expected = document(expected["result_json"].as_str().unwrap());
            assert_eq!(
                pretty(&result.unwrap()),
                pretty(&expected),
                "{}",
                case["name"]
            );
            successes += 1;
        }
    }
    assert_eq!((successes, failures), (11, 3));
}
#[test]
fn all_thirty_nine_projections_and_contract_writes_are_asserted() {
    let reference: Value = serde_json::from_str(*REFERENCE).unwrap();
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 39);
    let mut written = 0;
    let mut failed = 0;
    for case in cases {
        if assert_invalid_native_documents(case) {
            continue;
        }
        let kind = match case["kind"].as_str().unwrap() {
            "tester" => JobKind::Tester,
            "evaluator" => JobKind::Evaluator,
            "experiment" => JobKind::Experiment,
            _ => panic!("unknown kind"),
        };
        let claim = document(case["claim"]["json"].as_str().unwrap());
        let step = document(case["step"]["json"].as_str().unwrap());
        let metrics = case["metrics_json"].as_str().map(document);
        let context = JobContext {
            kind,
            claim: &claim,
            step: &step,
            metrics: metrics.as_ref(),
            nesting_budget: BUDGET,
        };
        let projected = job::project(&context);
        if let Some(exception) = case["projection"]["exception"].as_str() {
            assert_eq!(
                projected.unwrap_err().python_exception(),
                exception,
                "{}",
                case["name"]
            );
        } else {
            let actual = projected.unwrap();
            let expected = document(case["projection"]["projection_json"].as_str().unwrap());
            assert_eq!(pretty(&actual), pretty(&expected), "{}", case["name"]);
            assert_eq!(
                json!(keys(&actual, false)),
                case["projection"]["ordered_keys"]
            );
            assert_eq!(
                json!(keys(&actual, true)),
                case["projection"]["ordered_input_keys"]
            );
            for marker in [
                "public-worker-credential-exclusion-marker",
                "public-lease-credential-exclusion-marker",
                "public-github-credential-exclusion-marker",
            ] {
                assert!(!pretty(&actual).contains(marker));
            }
        }
        let scratch = Scratch::new();
        let root = scratch.0.join("contract");
        let name = case["name"].as_str().unwrap();
        let missing_root =
            name.ends_with("/missing-root") || name.ends_with("/missing-projection-field-and-root");
        if !missing_root {
            fs::create_dir(&root).unwrap();
        }
        let outputs = vec![
            root.join("outputs/one/nested"),
            root.join("outputs/two"),
            root.join("outputs/three"),
        ];
        if name.ends_with("/job-path-directory") {
            fs::create_dir(root.join("job.json")).unwrap();
        } else if name.ends_with("/output-file-collision") {
            fs::create_dir(root.join("outputs")).unwrap();
            fs::write(&outputs[1], "synthetic existing output file").unwrap();
        } else if name.ends_with("/overwrite-existing") {
            fs::write(root.join("job.json"), "old synthetic job").unwrap();
            fs::create_dir_all(&outputs[0]).unwrap();
        }
        let result = job::write_contract(&context, &root, &outputs);
        let expected = &case["write"];
        if let Some(exception) = expected["exception"].as_str() {
            assert_eq!(result.unwrap_err().python_exception(), exception, "{name}");
            failed += 1;
        } else {
            result.unwrap();
            assert_eq!(expected["result"], "written");
            written += 1;
        }
        assert_eq!(json!(snapshot(&root)), expected["filesystem"], "{name}");
        if let Some(text) = expected["job_text"].as_str() {
            let actual = fs::read(root.join("job.json")).unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&actual).unwrap(),
                serde_json::from_str::<Value>(text).unwrap(),
                "{name}"
            );
            assert_eq!(json!(actual.ends_with(b"\n")), expected["trailing_newline"]);
        }
    }
    assert_eq!((written, failed), (21, 13));
}
#[test]
fn raw_container_rendering_errors_are_sanitized() {
    let step = document(r#"{"manifest":{"spec":{"inputs":{"artifacts":[]}}}}"#);
    let pinned = document(r#"[{"id":{"private":"credential-marker"}}]"#);
    let error = job::declared_datasets(&step, &pinned, 0).unwrap_err();
    assert_eq!(error, JobError::Recursion);
    assert_eq!(error.python_exception(), "RecursionError");
    assert!(!format!("{error:?} {error}").contains("credential-marker"));
    assert!(std::error::Error::source(&error).is_none());
    let native = std::io::Error::other("private-capability-path");
    let safe = JobError::from(native);
    assert!(!format!("{safe:?} {safe}").contains("private-capability-path"));
    assert!(std::error::Error::source(&safe).is_none());
}
#[test]
fn rendering_failure_precedes_file_truncation_and_output_creation() {
    let reference: Value = serde_json::from_str(*REFERENCE).unwrap();
    let case = &reference["cases"][0];
    let claim = document(case["claim"]["json"].as_str().unwrap());
    let step = document(case["step"]["json"].as_str().unwrap());
    let context = JobContext {
        kind: JobKind::Tester,
        claim: &claim,
        step: &step,
        metrics: None,
        nesting_budget: 1,
    };
    let scratch = Scratch::new();
    fs::write(scratch.0.join("job.json"), "pristine").unwrap();
    assert_eq!(
        job::write_contract(&context, &scratch.0, &[scratch.0.join("output")]),
        Err(JobError::Encoding)
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("job.json")).unwrap(),
        "pristine"
    );
    assert!(!scratch.0.join("output").exists());
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
