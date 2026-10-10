//! Authored policy contracts and application semantics, independent of interpreter oracles.
use cannery_core::json::{self, Document, DocumentBuilder, Node};
use cannery_runner::{
    cli_depth::{JSON_CONTAINERS, PolicyEntryPoint},
    policy::{self, ErrorKind, Policy},
};
use serde_json::{Value, json};
use std::{error::Error, sync::Arc};

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;
fn stock() -> Result<Value> {
    Ok(serde_json::from_str(include_str!(
        "../../../examples/fixture/policy.json"
    ))?)
}
fn step() -> Result<Value> {
    Ok(serde_json::from_str(include_str!(
        "../../../examples/fixture/policy-step.json"
    ))?)
}
fn document(value: &Value) -> Result<Arc<Document>> {
    Ok(Arc::new(json::decode(
        &serde_json::to_vec(value)?,
        JSON_CONTAINERS,
    )?))
}
fn parse(value: &Value, caller: PolicyEntryPoint) -> Result<Policy> {
    Ok(policy::parse(document(value)?, caller, JSON_CONTAINERS)?)
}

#[test]
fn application_stock_and_step_policies_load_at_both_entry_points() -> Result {
    for caller in [
        PolicyEntryPoint::Evaluator,
        PolicyEntryPoint::RunnerVerifyKind,
    ] {
        let Policy::Stock(policy) = parse(&stock()?, caller)? else {
            return Err("wrong stock policy kind".into());
        };
        assert_eq!(policy.verifier_id, "cannery-runner");
        assert_eq!(policy.revision, "fixture-policy-1");
        assert!(policy.default_control.is_some());
        let Policy::Step(policy) = parse(&step()?, caller)? else {
            return Err("wrong step policy kind".into());
        };
        assert_eq!(policy.verifier_id, "cannery-runner");
        assert_eq!(policy.name, "fixture-policy");
    }
    Ok(())
}

#[test]
fn step_envelope_and_manifest_refuse_invalid_application_shapes() -> Result {
    for caller in [
        PolicyEntryPoint::Evaluator,
        PolicyEntryPoint::RunnerVerifyKind,
    ] {
        for (pointer, value) in [
            ("/schema_version", json!("0.1")),
            ("/verifier", json!({"id":"cannery-runner"})),
            ("/verifier/revision", json!("has spaces")),
            ("/step/spec/role", json!("evaluator")),
            ("/step", Value::Null),
            ("/step/spec/role", json!("producer")),
            ("/step/spec/inputs/artifacts/0/from", json!("attempt")),
            ("/step/spec/outputs/artifacts/0/name", json!("assessment")),
        ] {
            let mut value_document = step()?;
            *value_document
                .pointer_mut(pointer)
                .ok_or("authored pointer")? = value;
            let error = policy::parse(document(&value_document)?, caller, JSON_CONTAINERS)
                .err()
                .ok_or("invalid policy accepted")?;
            assert_eq!(error.kind, ErrorKind::Configuration, "{pointer}");
            assert!(!error.paths.is_empty(), "{pointer}");
        }
        let mut value = step()?;
        value["unknown"] = json!("private-sentinel");
        let error = policy::parse(document(&value)?, caller, JSON_CONTAINERS)
            .err()
            .ok_or("unknown envelope field accepted")?;
        assert_eq!(error.kind, ErrorKind::Configuration);
        assert!(error.paths.iter().any(|path| path == "/unknown"));
        assert!(!error.to_string().contains("private-sentinel"));
    }
    Ok(())
}

#[test]
fn stock_policy_preserves_duplicate_and_interval_guards() -> Result {
    let original = stock()?;
    for field in ["gates", "baselines"] {
        let mut value = original.clone();
        let entries = value[field].as_array_mut().ok_or("fixture array")?;
        entries.push(entries[0].clone());
        let error = policy::parse(
            document(&value)?,
            PolicyEntryPoint::Evaluator,
            JSON_CONTAINERS,
        )
        .err()
        .ok_or("duplicate policy entry accepted")?;
        assert_eq!(error.kind, ErrorKind::Configuration);
        assert!(error.paths.iter().any(|path| path == &format!("/{field}")));
    }
    let mut value = original.clone();
    value["baselines"][0]["measurements"][0]["uncertainty"] =
        json!({"method":"bootstrap","lower":0.9,"upper":1.0});
    assert!(
        policy::parse(
            document(&value)?,
            PolicyEntryPoint::Evaluator,
            JSON_CONTAINERS
        )
        .is_err()
    );
    let mut value = original;
    value["gates"] = json!([]);
    assert!(
        policy::parse(
            document(&value)?,
            PolicyEntryPoint::Evaluator,
            JSON_CONTAINERS
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn policy_validation_refuses_documents_beyond_native_depth() -> Result {
    let mut builder = DocumentBuilder::new();
    let mut root = builder.push(Node::Null)?;
    for _ in 0..=JSON_CONTAINERS {
        root = builder.push(Node::Array(vec![root]))?;
    }
    let document = Arc::new(builder.finish(root)?);
    for caller in [
        PolicyEntryPoint::Evaluator,
        PolicyEntryPoint::RunnerVerifyKind,
    ] {
        let error = policy::parse_policy(Arc::clone(&document), caller, JSON_CONTAINERS)
            .err()
            .ok_or("deep policy accepted")?;
        assert_eq!(error.kind, ErrorKind::Recursion);
    }
    Ok(())
}

#[test]
fn filesystem_loader_preserves_dispatch_and_refuses_invalid_json() -> Result {
    use cannery_runner::{launcher::PosixPath, policy::FilePolicyLoader};
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let directory = Directory(std::env::temp_dir().join(format!(
        "cannery-policy-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    )));
    fs::create_dir(&directory.0)?;
    let path = directory.0.join("policy.json");
    let path_text = path
        .to_str()
        .ok_or("test directory must be UTF-8")?
        .to_owned();
    let position = PosixPath::new(&path_text);
    let loader = FilePolicyLoader {
        entry_point: PolicyEntryPoint::RunnerVerifyKind,
        repr_nesting_budget: JSON_CONTAINERS,
    };
    assert!(loader.load_policy(&position).is_err());
    fs::write(&path, serde_json::to_vec(&stock()?)?)?;
    assert!(matches!(loader.load_policy(&position)?, Policy::Stock(_)));
    fs::write(&path, serde_json::to_vec(&step()?)?)?;
    assert!(matches!(loader.load_policy(&position)?, Policy::Step(_)));
    for bytes in [
        b"\xff".as_slice(),
        br#""\ud800""#.as_slice(),
        b"NaN".as_slice(),
        b"Infinity".as_slice(),
        b"{private-sentinel".as_slice(),
    ] {
        fs::write(&path, bytes)?;
        let error = loader
            .load_policy(&position)
            .err()
            .ok_or("invalid policy file accepted")?;
        assert!(!error.to_string().contains("private-sentinel"));
    }
    Ok(())
}
