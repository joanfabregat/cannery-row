//! Authored configuration, backend, policy, path and credential contracts.
use cannery_runner::{
    cli_depth::{JSON_CONTAINERS, PolicyEntryPoint},
    config::{
        self, ErrorKind, JobKind, KindConfig, LauncherType, NativePathResolver, ProcessConfig,
        VerifyPolicy,
    },
    credentials::TokenFileError,
    policy::FilePolicyLoader,
};
use serde_json::{Value, json};
use std::{
    error::Error,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;
struct Directory(PathBuf);
impl Directory {
    fn new() -> Result<Self> {
        let directory = Self(std::env::temp_dir().join(format!(
            "cannery-config-contract-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        )));
        fs::create_dir(&directory.0)?;
        for name in ["data", "work", "cache", "steps"] {
            fs::create_dir(directory.0.join(name))?;
        }
        for (name, value) in [
            ("verify.token", "synthetic-verify-credential"),
            ("experiment.token", "synthetic-experiment-credential"),
            ("decide.token", "synthetic-decide-credential"),
        ] {
            fs::write(directory.0.join(name), value)?;
            fs::set_permissions(directory.0.join(name), fs::Permissions::from_mode(0o600))?;
        }
        fs::write(
            directory.0.join("stock.json"),
            include_bytes!("../../../examples/fixture/policy.json"),
        )?;
        fs::write(
            directory.0.join("step.json"),
            include_bytes!("../../../examples/fixture/policy-step.json"),
        )?;
        fs::write(
            directory.0.join("decider.json"),
            include_bytes!("../../../examples/fixture/decider-step.json"),
        )?;
        Ok(directory)
    }
    fn load(&self, value: &Value) -> Result<ProcessConfig> {
        let path = self.0.join("config.json");
        fs::write(&path, serde_json::to_vec(value)?)?;
        Self::read(&path)
    }
    fn read(path: &std::path::Path) -> Result<ProcessConfig> {
        Ok(config::load_config_file(
            path,
            &FilePolicyLoader {
                entry_point: PolicyEntryPoint::RunnerVerifyKind,
                repr_nesting_budget: JSON_CONTAINERS,
            },
            &NativePathResolver,
            JSON_CONTAINERS,
        )?)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn base() -> Value {
    json!({"api_url":"http://localhost:9010", "project":"fixture", "kinds":[{"kind":"verify", "name":"fixture-verify", "token_file":"verify.token", "policy":"stock.json"}], "launcher":{"type":"local"}})
}

#[test]
fn ordinary_json_and_toml_configurations_load_with_defaults_and_relative_paths() -> Result {
    let directory = Directory::new()?;
    let mut value = base();
    value["data_root"] = json!("data");
    value["work_root"] = json!("work");
    value["cache_root"] = json!("cache");
    value["cache_max_bytes"] = json!("1Gi");
    value["launcher"]["step_root"] = json!("steps");
    let loaded = directory.load(&value)?;
    assert_eq!(loaded.api_url, "http://localhost:9010");
    assert_eq!(loaded.project, "fixture");
    assert_eq!(loaded.launcher, Some(LauncherType::Local));
    assert_eq!(loaded.kinds[0].concurrency.to_string(), "1");
    assert_eq!(loaded.kinds[0].poll_seconds.to_bits(), 10.0_f64.to_bits());
    for (root, name) in [
        (loaded.data_root, "data"),
        (loaded.work_root, "work"),
        (loaded.cache_root, "cache"),
        (loaded.step_root, "steps"),
    ] {
        assert_eq!(root, Some(directory.0.join(name)));
    }
    assert_eq!(loaded.cache_max_bytes.as_deref(), Some("1Gi"));
    let path = directory.0.join("config.toml");
    fs::write(
        &path,
        "api_url = 'http://localhost:9010'\nproject = 'fixture'\n[launcher]\ntype = 'local'\n[[kinds]]\nkind = 'verify'\nname = 'fixture-verify'\ntoken_file = 'verify.token'\npolicy = 'stock.json'\npoll_seconds = 0.5\nconcurrency = 2\n",
    )?;
    let loaded = Directory::read(&path)?;
    assert_eq!(loaded.kinds[0].concurrency.to_string(), "2");
    assert_eq!(loaded.kinds[0].poll_seconds.to_bits(), 0.5_f64.to_bits());
    Ok(())
}

#[test]
fn docker_kubernetes_and_github_settings_keep_supported_application_fields() -> Result {
    let directory = Directory::new()?;
    let mut value = base();
    value["launcher"] = json!({"type":"docker", "docker_host":"unix:///run/podman/podman.sock", "docker_user":"1000:1000", "docker_job_root_host":"/owned/jobs", "docker_pids_limit":100, "docker_tmp_size":"64Mi", "docker_gpu_mode":"nvidia", "docker_gpu_devices":[0,2]});
    let loaded = directory.load(&value)?;
    assert_eq!(loaded.launcher, Some(LauncherType::Docker));
    assert_eq!(loaded.docker.docker_gpu_devices.as_deref(), Some("0,2"));
    assert_eq!(
        loaded.docker.docker_host.as_deref(),
        Some("unix:///run/podman/podman.sock")
    );
    assert_eq!(
        loaded
            .docker
            .docker_pids_limit
            .map(|n| n.to_string())
            .as_deref(),
        Some("100")
    );
    value["launcher"] = json!({"type":"kubernetes", "k8s_namespace":"fixture", "k8s_token_file":"verify.token", "k8s_storage_class":"standard", "k8s_volume_size":"1Gi", "k8s_scheduling_timeout":15, "k8s_max_output_files":100, "k8s_exec_idle_timeout":5});
    value["github"] = json!({"token_file":"experiment.token", "app_id":123, "app_installation_id":"456", "api_url":"https://api.github.com", "allowed_repos":["owner/project"]});
    let loaded = directory.load(&value)?;
    assert_eq!(loaded.launcher, Some(LauncherType::Kubernetes));
    assert_eq!(loaded.kubernetes.k8s_namespace.as_deref(), Some("fixture"));
    assert_eq!(
        loaded.kubernetes.k8s_token_file,
        Some(directory.0.join("verify.token"))
    );
    assert_eq!(loaded.github.app_id.as_deref(), Some("123"));
    assert_eq!(loaded.github.app_installation_id.as_deref(), Some("456"));
    assert_eq!(
        loaded.github.allowed_repos,
        Some(vec!["owner/project".to_owned()])
    );
    Ok(())
}

#[test]
fn verify_kinds_load_stock_and_step_policies_and_experiments_need_none() -> Result {
    let directory = Directory::new()?;
    for (policy, step) in [("stock.json", false), ("step.json", true)] {
        let mut value = base();
        value["kinds"][0]["policy"] = json!(policy);
        let loaded = directory.load(&value)?;
        assert!(match &loaded.kinds[0].kind {
            KindConfig::Verify(VerifyPolicy::Stock(_)) => !step,
            KindConfig::Verify(VerifyPolicy::Step(_)) => step,
            KindConfig::Experiment | KindConfig::Decide(_) => false,
        });
        assert_eq!(loaded.kinds[0].kind.job_kind(), JobKind::Verify);
    }
    let mut value = base();
    value["kinds"] =
        json!([{"kind":"experiment", "name":"experiment", "token_file":"experiment.token"}]);
    let loaded = directory.load(&value)?;
    assert!(matches!(loaded.kinds[0].kind, KindConfig::Experiment));
    let mut value = base();
    value["kinds"][0]["policy"] = Value::Null;
    assert_eq!(
        directory
            .load(&value)
            .err()
            .ok_or("verify kind without a policy accepted")?
            .downcast_ref::<config::ConfigError>()
            .ok_or("config error")?
            .kind,
        ErrorKind::Policy
    );
    let mut value = base();
    value["kinds"] = json!([{"kind":"experiment", "name":"experiment", "token_file":"experiment.token", "policy":"stock.json"}]);
    assert_eq!(
        directory
            .load(&value)
            .err()
            .ok_or("experiment policy accepted")?
            .downcast_ref::<config::ConfigError>()
            .ok_or("config error")?
            .kind,
        ErrorKind::Unknown
    );
    Ok(())
}

#[test]
fn decide_kinds_load_a_decider_registration_and_nothing_else() -> Result {
    let directory = Directory::new()?;
    let decide = |decider: Value| {
        let mut value = base();
        value["kinds"] = json!([{"kind":"decide", "name":"decide", "token_file":"decide.token", "decider": decider}]);
        value
    };
    let loaded = directory.load(&decide(json!("decider.json")))?;
    assert!(matches!(loaded.kinds[0].kind, KindConfig::Decide(_)));
    assert_eq!(loaded.kinds[0].kind.job_kind(), JobKind::Decide);
    // A missing decider, a policy file in its place, or a decider on a
    // verify kind are all refused.
    for (value, expected) in [
        (decide(Value::Null), ErrorKind::Decider),
        (decide(json!("step.json")), ErrorKind::Decider),
        (decide(json!("stock.json")), ErrorKind::Decider),
        (
            {
                let mut value = base();
                value["kinds"][0]["decider"] = json!("decider.json");
                value
            },
            ErrorKind::Unknown,
        ),
    ] {
        assert_eq!(
            directory
                .load(&value)
                .err()
                .ok_or("invalid decide configuration accepted")?
                .downcast_ref::<config::ConfigError>()
                .ok_or("config error")?
                .kind,
            expected
        );
    }
    // The verify and decide kinds of one process hold different tokens.
    let mut value = decide(json!("decider.json"));
    value["kinds"][0]["token_file"] = json!("verify.token");
    value["kinds"]
        .as_array_mut()
        .ok_or("kinds")?
        .push(base()["kinds"][0].clone());
    assert_eq!(
        directory
            .load(&value)
            .err()
            .ok_or("shared token accepted")?
            .downcast_ref::<config::ConfigError>()
            .ok_or("config error")?
            .kind,
        ErrorKind::SharedToken
    );
    Ok(())
}

#[test]
fn configuration_refuses_invalid_constraints_without_exposing_credential_values() -> Result {
    let directory = Directory::new()?;
    for (pointer, value, expected) in [
        ("/project", json!(false), ErrorKind::Text),
        ("/kinds", json!([]), ErrorKind::Kind),
        ("/kinds/0/kind", json!("unknown"), ErrorKind::Kind),
        ("/kinds/0/kind", json!("test"), ErrorKind::Kind),
        ("/kinds/0/kind", json!("eval"), ErrorKind::Kind),
        ("/kinds/0/name", json!("has spaces"), ErrorKind::Name),
        ("/kinds/0/concurrency", json!(0), ErrorKind::Integer),
        ("/kinds/0/concurrency", json!(true), ErrorKind::Integer),
        ("/kinds/0/poll_seconds", json!(-1), ErrorKind::Seconds),
        ("/launcher/type", json!("unknown"), ErrorKind::Launcher),
    ] {
        let mut document = base();
        if pointer.ends_with("concurrency") || pointer.ends_with("poll_seconds") {
            document["kinds"][0][pointer.rsplit('/').next().ok_or("field name")?] = Value::Null;
        }
        *document
            .pointer_mut(pointer)
            .ok_or("authored config pointer")? = value;
        let error = directory
            .load(&document)
            .err()
            .ok_or("invalid config accepted")?;
        let error = error
            .downcast_ref::<config::ConfigError>()
            .ok_or("wrong config error type")?;
        assert_eq!(error.kind, expected, "{pointer}");
        assert!(!format!("{error:?} {error}").contains("synthetic-verify-credential"));
    }
    for (field, value, expected) in [
        ("docker_gpu_devices", json!("١"), ErrorKind::GpuSyntax),
        ("docker_gpu_devices", json!("0,00"), ErrorKind::GpuDuplicate),
        ("docker_gpu_mode", json!("unknown"), ErrorKind::GpuMode),
    ] {
        let mut document = base();
        document["launcher"]["type"] = json!("docker");
        document["launcher"][field] = value;
        let error = directory
            .load(&document)
            .err()
            .ok_or("invalid GPU config accepted")?;
        assert_eq!(
            error
                .downcast_ref::<config::ConfigError>()
                .ok_or("config error")?
                .kind,
            expected
        );
    }
    let mut document = base();
    document["unexpected"] = json!(true);
    assert!(directory.load(&document).is_err());
    let mut document = base();
    document["kinds"] = json!([document["kinds"][0].clone(), document["kinds"][0].clone()]);
    assert_eq!(
        directory
            .load(&document)
            .err()
            .ok_or("duplicate name accepted")?
            .downcast_ref::<config::ConfigError>()
            .ok_or("config error")?
            .kind,
        ErrorKind::DuplicateName
    );
    Ok(())
}

#[test]
fn credential_permissions_kind_separation_and_redaction_are_enforced() -> Result {
    let directory = Directory::new()?;
    let loaded = directory.load(&base())?;
    assert_eq!(
        loaded.kinds[0].token.expose(),
        "synthetic-verify-credential"
    );
    assert!(!format!("{loaded:?} {:?}", loaded.kinds[0]).contains("synthetic-verify-credential"));
    let mut document = base();
    document["kinds"] = json!([document["kinds"][0].clone(), {"kind":"experiment", "name":"experiment", "token_file":"verify.token"}]);
    assert_eq!(
        directory
            .load(&document)
            .err()
            .ok_or("shared credential accepted")?
            .downcast_ref::<config::ConfigError>()
            .ok_or("config error")?
            .kind,
        ErrorKind::SharedToken
    );
    fs::set_permissions(
        directory.0.join("verify.token"),
        fs::Permissions::from_mode(0o644),
    )?;
    assert_eq!(
        directory
            .load(&base())
            .err()
            .ok_or("public credential accepted")?
            .downcast_ref::<config::ConfigError>()
            .ok_or("config error")?
            .kind,
        ErrorKind::Token(TokenFileError::Permissions)
    );
    fs::set_permissions(
        directory.0.join("verify.token"),
        fs::Permissions::from_mode(0o600),
    )?;
    fs::write(directory.0.join("verify.token"), b"\xff")?;
    assert_eq!(
        directory
            .load(&base())
            .err()
            .ok_or("invalid UTF8 credential accepted")?
            .downcast_ref::<config::ConfigError>()
            .ok_or("config error")?
            .kind,
        ErrorKind::Token(TokenFileError::Encoding)
    );
    Ok(())
}

#[test]
fn unsupported_json_values_are_rejected_at_the_native_input_boundary() -> Result {
    let directory = Directory::new()?;
    let path = directory.0.join("config.json");
    for bytes in [
        br#"{"project":"\ud800"}"#.as_slice(),
        b"NaN".as_slice(),
        b"Infinity".as_slice(),
        b"1e999".as_slice(),
    ] {
        fs::write(&path, bytes)?;
        assert!(Directory::read(&path).is_err());
    }
    Ok(())
}
