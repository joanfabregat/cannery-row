#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;

use cannery_runner::{
    config::{DockerSettings, GitHubSettings, KubernetesSettings, LauncherType, ProcessConfig},
    launcher::PosixPath,
    preparation,
};
use num_bigint::BigInt;
use serde_json::{Value, json};
use std::{error::Error, path::PathBuf};

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

fn text(value: &Value) -> Result<String> {
    let points = value
        .as_array()
        .ok_or("fixture codepoints missing")?
        .iter()
        .map(|point| {
            point
                .as_u64()
                .and_then(|point| u32::try_from(point).ok())
                .ok_or("invalid fixture point")
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    cannery_core::text::from_codepoints(points).ok_or_else(|| "invalid fixture text".into())
}

fn assert_lone_surrogate(value: &Value) -> Result {
    assert!(
        value
            .as_array()
            .ok_or("missing native codepoints")?
            .iter()
            .any(|point| point
                .as_u64()
                .is_some_and(|point| (0xd800..=0xdfff).contains(&point)))
    );
    assert!(text(value).is_err());
    Ok(())
}

fn configuration(value: &Value) -> Result<ProcessConfig> {
    let mut config = ProcessConfig {
        api_url: String::from("fixture"),
        project: String::from("fixture"),
        kinds: Vec::new(),
        launcher: None,
        step_root: None,
        data_root: None,
        work_root: None,
        cache_root: None,
        cache_max_bytes: None,
        runner_id: value["runner_id"]
            .as_bool()
            .ok_or("missing runner id presence")?
            .then(String::new),
        docker: DockerSettings::default(),
        kubernetes: KubernetesSettings::default(),
        github: GitHubSettings::default(),
    };
    for field in value["supplied"]
        .as_array()
        .ok_or("missing supplied fields")?
    {
        let empty = || Some(String::new());
        let zero = || Some(BigInt::from(0));
        let path = || Some(PathBuf::from("fixture"));
        match field.as_str().ok_or("invalid supplied field")? {
            "docker_host" => config.docker.docker_host = empty(),
            "docker_user" => config.docker.docker_user = empty(),
            "docker_job_root_host" => {
                config.docker.docker_job_root_host = Some(PosixPath::new(&String::from("fixture")));
            }
            "docker_pids_limit" => config.docker.docker_pids_limit = zero(),
            "docker_tmp_size" => config.docker.docker_tmp_size = empty(),
            "docker_shm_size" => config.docker.docker_shm_size = empty(),
            "docker_gpu_mode" => config.docker.docker_gpu_mode = empty(),
            "docker_gpu_devices" => config.docker.docker_gpu_devices = empty(),
            "k8s_namespace" => config.kubernetes.k8s_namespace = empty(),
            "k8s_api_url" => config.kubernetes.k8s_api_url = empty(),
            "k8s_token_file" => config.kubernetes.k8s_token_file = path(),
            "k8s_ca_file" => config.kubernetes.k8s_ca_file = path(),
            "k8s_storage_class" => config.kubernetes.k8s_storage_class = empty(),
            "k8s_volume_size" => config.kubernetes.k8s_volume_size = empty(),
            "k8s_gpu_runtime_class" => config.kubernetes.k8s_gpu_runtime_class = empty(),
            "k8s_step_user" => config.kubernetes.k8s_step_user = empty(),
            "k8s_scheduling_timeout" => config.kubernetes.k8s_scheduling_timeout = Some(0.0),
            "k8s_transfer_image" => config.kubernetes.k8s_transfer_image = empty(),
            "k8s_tmp_size" => config.kubernetes.k8s_tmp_size = empty(),
            "k8s_shm_size" => config.kubernetes.k8s_shm_size = empty(),
            "k8s_max_output_bytes" => config.kubernetes.k8s_max_output_bytes = empty(),
            "k8s_max_output_files" => config.kubernetes.k8s_max_output_files = zero(),
            "k8s_exec_idle_timeout" => config.kubernetes.k8s_exec_idle_timeout = Some(0.0),
            _ => return Err("unknown fixture field".into()),
        }
    }
    Ok(config)
}

#[test]
fn matches_actual_python_preparation_checks() -> Result {
    let fixture: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/runner/tests/fixtures/preparation_reference.json"
    ))?;
    let cases = fixture["cases"].as_array().ok_or("missing fixture cases")?;
    for (index, case) in cases.iter().enumerate() {
        if case["kind"] == "size" {
            match text(&case["input"]) {
                Err(_) => {
                    assert_lone_surrogate(&case["input"])?;
                    continue;
                }
                Ok(input) if input.len() > 1024 || input.ends_with('\n') => {
                    assert!(preparation::parse_size(&input).is_err());
                    continue;
                }
                Ok(_) => {}
            }
        }
        if case["kind"] == "repos"
            && let Some(items) = case["input"].as_array()
        {
            let invalid = items
                .iter()
                .filter(|item| text(item).is_err())
                .collect::<Vec<_>>();
            if !invalid.is_empty() {
                for item in invalid {
                    assert_lone_surrogate(item)?;
                }
                continue;
            }
        }
        let actual = match case["kind"].as_str().ok_or("missing fixture kind")? {
            "size" => match preparation::parse_size(&text(&case["input"])?) {
                Ok(size) => json!({"ok": format!("{size:#x}")}),
                Err(error) => json!({"error": format!("{error:?}")}),
            },
            "repos" => {
                let given = case["input"]
                    .as_array()
                    .map(|items| items.iter().map(text).collect::<Result<Vec<_>>>())
                    .transpose()?;
                if given.as_ref().is_some_and(|items| {
                    items
                        .iter()
                        .any(|item| item.chars().any(|ch| ('\u{1c}'..='\u{1f}').contains(&ch)))
                }) {
                    assert!(matches!(
                        preparation::allowed_repos(given.as_deref()),
                        Err(preparation::PreparationError::Repository)
                    ));
                    continue;
                }
                match preparation::allowed_repos(given.as_deref()) {
                    Ok(repos) => {
                        let output = repos.map(|repos| {
                            let mut points: Vec<_> = repos
                                .into_iter()
                                .map(|repo| repo.codepoints().clone())
                                .collect();
                            points.sort();
                            points
                        });
                        json!({"ok": output})
                    }
                    Err(error) => json!({"error": format!("{error:?}")}),
                }
            }
            "ignored" => {
                let launcher = match case["input"]["launcher"]
                    .as_str()
                    .ok_or("missing launcher")?
                {
                    "local" => LauncherType::Local,
                    "docker" => LauncherType::Docker,
                    "kubernetes" => LauncherType::Kubernetes,
                    _ => return Err("unknown fixture launcher".into()),
                };
                json!({"ok": preparation::ignored_options(&configuration(&case["input"])?, launcher)})
            }
            _ => return Err("unknown fixture kind".into()),
        };
        assert_eq!(actual, case["result"], "preparation fixture {index}");
    }
    assert_eq!(cases.len(), 309);
    Ok(())
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
