//! Pure configuration checks performed while preparing a runner process.
use crate::{
    config::{DockerSettings, KubernetesSettings, LauncherType, ProcessConfig},
    credentials::is_whitespace,
    launcher,
};

use num_bigint::BigInt;
use std::collections::HashSet;

/// Source value failures; messages are rendered at the CLI boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PreparationError {
    #[error("invalid runner size")]
    Size,
    #[error("invalid repository allowlist entry")]
    Repository,
    #[error("repository allowlist is empty")]
    EmptyRepositories,
}

/// Parse a bounded native quantity, then truncate its rational value toward zero.
/// # Errors
/// Invalid syntax and the 1024-byte resource limit become a size value error.
pub fn parse_size(text: &str) -> Result<BigInt, PreparationError> {
    launcher::quantity(text)
        .map(|quantity| quantity.to_integer())
        .map_err(|_| PreparationError::Size)
}

fn valid_repository(points: &[u32]) -> bool {
    let Some(slash) = points.iter().position(|&point| point == 47) else {
        return false;
    };
    let owner = &points[..slash];
    let name = &points[slash + 1..];
    let alphanumeric = |point: u32| matches!(point, 48..=57 | 65..=90 | 97..=122);
    (1..=39).contains(&owner.len())
        && alphanumeric(owner[0])
        && owner
            .iter()
            .all(|&point| alphanumeric(point) || point == 45)
        && (1..=100).contains(&name.len())
        && name != [46]
        && name != [46, 46]
        && name
            .iter()
            .all(|&point| alphanumeric(point) || matches!(point, 45 | 46 | 95))
}

/// Split repeated or comma-separated names, strip Python whitespace and lowercase.
/// The returned set retains the source's unordered set semantics.
/// # Errors
/// Returns the first invalid entry's category, or an explicitly empty allowlist.
pub fn allowed_repos(
    given: Option<&[String]>,
) -> Result<Option<HashSet<String>>, PreparationError> {
    let Some(given) = given.filter(|items| !items.is_empty()) else {
        return Ok(None);
    };
    let mut repositories = HashSet::new();
    for item in given {
        for part in item.codepoints().split(|&point| point == 44) {
            let start = part
                .iter()
                .position(|&point| !is_whitespace(point))
                .unwrap_or(part.len());
            let end = part
                .iter()
                .rposition(|&point| !is_whitespace(point))
                .map_or(start, |index| index + 1);
            let points = &part[start..end];
            if points.is_empty() {
                continue;
            }
            if !valid_repository(points) {
                return Err(PreparationError::Repository);
            }
            let repository = cannery_core::text::from_codepoints(points.to_vec())
                .ok_or(PreparationError::Repository)?;
            repositories.insert(repository.lowercase());
        }
    }
    if repositories.is_empty() {
        return Err(PreparationError::EmptyRepositories);
    }
    Ok(Some(repositories))
}

/// Options are reported in the Python dataclass's declaration order.
#[must_use]
pub fn docker_options(settings: &DockerSettings) -> Vec<&'static str> {
    [
        (settings.docker_host.is_some(), "--docker-host"),
        (settings.docker_user.is_some(), "--docker-user"),
        (
            settings.docker_job_root_host.is_some(),
            "--docker-job-root-host",
        ),
        (settings.docker_pids_limit.is_some(), "--docker-pids-limit"),
        (settings.docker_tmp_size.is_some(), "--docker-tmp-size"),
        (settings.docker_shm_size.is_some(), "--docker-shm-size"),
        (settings.docker_gpu_mode.is_some(), "--docker-gpu-mode"),
        (
            settings.docker_gpu_devices.is_some(),
            "--docker-gpu-devices",
        ),
    ]
    .into_iter()
    .filter_map(|(present, option)| present.then_some(option))
    .collect()
}

/// Options are reported in the Python dataclass's declaration order.
#[must_use]
pub fn kubernetes_options(settings: &KubernetesSettings) -> Vec<&'static str> {
    [
        (settings.k8s_namespace.is_some(), "--k8s-namespace"),
        (settings.k8s_api_url.is_some(), "--k8s-api-url"),
        (settings.k8s_token_file.is_some(), "--k8s-token-file"),
        (settings.k8s_ca_file.is_some(), "--k8s-ca-file"),
        (settings.k8s_storage_class.is_some(), "--k8s-storage-class"),
        (settings.k8s_volume_size.is_some(), "--k8s-volume-size"),
        (
            settings.k8s_gpu_runtime_class.is_some(),
            "--k8s-gpu-runtime-class",
        ),
        (settings.k8s_step_user.is_some(), "--k8s-step-user"),
        (
            settings.k8s_scheduling_timeout.is_some(),
            "--k8s-scheduling-timeout",
        ),
        (
            settings.k8s_transfer_image.is_some(),
            "--k8s-transfer-image",
        ),
        (settings.k8s_tmp_size.is_some(), "--k8s-tmp-size"),
        (settings.k8s_shm_size.is_some(), "--k8s-shm-size"),
        (
            settings.k8s_max_output_bytes.is_some(),
            "--k8s-max-output-bytes",
        ),
        (
            settings.k8s_max_output_files.is_some(),
            "--k8s-max-output-files",
        ),
        (
            settings.k8s_exec_idle_timeout.is_some(),
            "--k8s-exec-idle-timeout",
        ),
    ]
    .into_iter()
    .filter_map(|(present, option)| present.then_some(option))
    .collect()
}

/// Flags supplied for a different launcher, in source warning order.
#[must_use]
pub fn ignored_options(config: &ProcessConfig, launcher: LauncherType) -> Vec<&'static str> {
    match launcher {
        LauncherType::Local => {
            let mut options = Vec::new();
            if config.runner_id.is_some() {
                options.push("--runner-id");
            }
            options.extend(docker_options(&config.docker));
            options.extend(kubernetes_options(&config.kubernetes));
            options
        }
        LauncherType::Docker => kubernetes_options(&config.kubernetes),
        LauncherType::Kubernetes => docker_options(&config.docker),
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
