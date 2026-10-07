//! Explicit operator policy and concrete container factories.
use super::{RuntimeError, backend::Backend};
use crate::{
    config::ProcessConfig,
    container_backends::{
        Limits,
        docker::{DockerBackend, DockerOptions, GpuMode},
        kubernetes::{KubernetesBackend, KubernetesOptions},
    },
};

use clap::Args;
use num_traits::ToPrimitive;
use std::{path::PathBuf, sync::Arc, time::Duration};

/// Infrastructure acknowledgements are operator controls, outside step manifests.
#[derive(Args, Clone)]
pub struct BackendPolicy {
    /// Allow networked steps only with externally enforced metadata/egress isolation.
    #[arg(long)]
    pub allow_unrestricted_egress: bool,
    /// Acknowledge installed namespace policies for isolated Kubernetes execution.
    #[arg(long)]
    pub k8s_namespace_policy_acknowledged: bool,
    #[arg(long, default_value = "kubernetes.default.svc")]
    pub k8s_api_service_host: String,
    #[arg(long, default_value_t = 443)]
    pub k8s_api_service_port: u16,
    #[arg(long, default_value = "1")]
    pub default_step_cpu: String,
    #[arg(long, default_value = "512Mi")]
    pub default_step_memory: String,
}
impl Default for BackendPolicy {
    fn default() -> Self {
        Self {
            allow_unrestricted_egress: false,
            k8s_namespace_policy_acknowledged: false,
            k8s_api_service_host: "kubernetes.default.svc".to_owned(),
            k8s_api_service_port: 443,
            default_step_cpu: "1".to_owned(),
            default_step_memory: "512Mi".to_owned(),
        }
    }
}
fn text(value: Option<&String>, fallback: &str) -> Result<String, RuntimeError> {
    value.map_or_else(
        || Ok(fallback.to_owned()),
        |value| value.as_utf8().ok_or(RuntimeError::Configuration),
    )
}
fn bytes(value: Option<&String>, fallback: u64) -> Result<u64, RuntimeError> {
    let Some(value) = value else {
        return Ok(fallback);
    };
    crate::launcher::quantity(value)
        .map_err(|_| RuntimeError::Configuration)?
        .ceil()
        .to_integer()
        .to_u64()
        .filter(|value| *value > 0)
        .ok_or(RuntimeError::Configuration)
}
fn integer(value: Option<&num_bigint::BigInt>, fallback: i64) -> Result<i64, RuntimeError> {
    value
        .map_or(Some(fallback), ToPrimitive::to_i64)
        .filter(|value| *value > 0)
        .ok_or(RuntimeError::Configuration)
}
fn user(value: Option<&String>) -> Result<(u32, u32), RuntimeError> {
    let value = text(value, "10001:10001")?;
    let (uid, gid) = value.split_once(':').ok_or(RuntimeError::Configuration)?;
    let parse = |value: &str| {
        value
            .parse::<u32>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or(RuntimeError::Configuration)
    };
    Ok((parse(uid)?, parse(gid)?))
}
fn duration(value: f64) -> Result<Duration, RuntimeError> {
    Duration::try_from_secs_f64(value)
        .ok()
        .filter(|value| !value.is_zero() && value.as_secs() <= 86_400)
        .ok_or(RuntimeError::Configuration)
}
fn limits(output: u64, files: usize, scheduling: f64, idle: f64) -> Result<Limits, RuntimeError> {
    Ok(Limits {
        input_bytes: 16 << 30,
        output_bytes: output,
        files,
        log_bytes: 200 << 20,
        scheduling: duration(scheduling)?,
        transfer_idle: duration(idle)?,
        cleanup: Duration::from_secs(60),
    })
}
fn defaults(policy: &BackendPolicy) -> Result<(u32, i64), RuntimeError> {
    let cpu = crate::launcher::quantity(&String::from(&policy.default_step_cpu))
        .map_err(|_| RuntimeError::Configuration)?;
    let millis = (cpu * num_bigint::BigInt::from(1000))
        .ceil()
        .to_integer()
        .to_u32()
        .filter(|value| *value > 0)
        .ok_or(RuntimeError::Configuration)?;
    let memory = bytes(Some(&String::from(&policy.default_step_memory)), 512 << 20)?;
    Ok((
        millis,
        i64::try_from(memory).map_err(|_| RuntimeError::Configuration)?,
    ))
}
fn identity(config: &ProcessConfig) -> Result<String, RuntimeError> {
    config
        .runner_id
        .as_ref()
        .and_then(String::as_utf8)
        .filter(|value| !value.is_empty())
        .ok_or(RuntimeError::Configuration)
}

/// # Errors
/// Require explicit daemon configuration and finite resources before any claim.
pub fn docker(
    config: &ProcessConfig,
    policy: &BackendPolicy,
) -> Result<Arc<dyn Backend>, RuntimeError> {
    Ok(Arc::new(DockerBackend::new(docker_options(
        config, policy,
    )?)?))
}
fn docker_options(
    config: &ProcessConfig,
    policy: &BackendPolicy,
) -> Result<DockerOptions, RuntimeError> {
    let options = &config.docker;
    let host = text(options.docker_host.as_ref(), "unix:///var/run/docker.sock")?;
    let socket = PathBuf::from(
        host.strip_prefix("unix://")
            .ok_or(RuntimeError::Configuration)?,
    );
    let (uid, gid) = user(options.docker_user.as_ref())?;
    let (default_cpu_millis, default_memory_bytes) = defaults(policy)?;
    let work = config.work_root.clone().unwrap_or_else(std::env::temp_dir);
    let work = crate::paths::resolve(&work, false).map_err(|_| RuntimeError::Configuration)?;
    let host_root = options
        .docker_job_root_host
        .as_ref()
        .map(|path| {
            path.text()
                .as_utf8()
                .map(PathBuf::from)
                .ok_or(RuntimeError::Configuration)
        })
        .transpose()?;
    let gpu_devices = if let Some(value) = &options.docker_gpu_devices {
        let value = value.as_utf8().ok_or(RuntimeError::Configuration)?;
        value
            .split(',')
            .map(|value| {
                value
                    .trim()
                    .strip_prefix("nvidia")
                    .unwrap_or(value.trim())
                    .parse::<u32>()
                    .map_err(|_| RuntimeError::Configuration)
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    let gpu_mode = match text(options.docker_gpu_mode.as_ref(), "nvidia")?.as_str() {
        "nvidia" => GpuMode::Nvidia,
        "cos" => GpuMode::Cos {
            driver_root: PathBuf::from("/var/lib/nvidia"),
        },
        _ => return Err(RuntimeError::Configuration),
    };
    let options = DockerOptions {
        runner_id: identity(config)?,
        socket,
        uid,
        gid,
        job_root: work,
        job_root_host: host_root,
        pids: integer(options.docker_pids_limit.as_ref(), 4096)?,
        tmp_bytes: i64::try_from(bytes(options.docker_tmp_size.as_ref(), 1 << 30)?)
            .map_err(|_| RuntimeError::Configuration)?,
        shm_bytes: i64::try_from(bytes(options.docker_shm_size.as_ref(), 64 << 20)?)
            .map_err(|_| RuntimeError::Configuration)?,
        default_cpu_millis,
        default_memory_bytes,
        gpu_devices,
        gpu_mode,
        allow_unrestricted_egress: policy.allow_unrestricted_egress,
        limits: limits(16 << 30, 100_000, 900.0, 300.0)?,
    };
    options.validate()?;
    Ok(options)
}

/// # Errors
/// Reject ambient credential discovery, unsafe URLs and absent isolation acknowledgement.
pub fn kubernetes(
    config: &ProcessConfig,
    policy: &BackendPolicy,
) -> Result<Arc<dyn Backend>, RuntimeError> {
    let (options, client) = kubernetes_options(config, policy)?;
    Ok(Arc::new(KubernetesBackend::connect(options, client)?))
}
fn kubernetes_options(
    config: &ProcessConfig,
    policy: &BackendPolicy,
) -> Result<(KubernetesOptions, kube::Config), RuntimeError> {
    let options = &config.kubernetes;
    let api = options
        .k8s_api_url
        .as_ref()
        .and_then(String::as_utf8)
        .ok_or(RuntimeError::Configuration)?;
    let url = reqwest::Url::parse(&api).map_err(|_| RuntimeError::Configuration)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(RuntimeError::Configuration);
    }
    let namespace = options
        .k8s_namespace
        .as_ref()
        .and_then(String::as_utf8)
        .ok_or(RuntimeError::Configuration)?;
    let token_file = options
        .k8s_token_file
        .as_ref()
        .ok_or(RuntimeError::Configuration)?;
    crate::credentials::read_token_file(token_file).map_err(|_| RuntimeError::Configuration)?;
    let mut client = kube::Config::new(api.parse().map_err(|_| RuntimeError::Configuration)?);
    client.auth_info.token_file = Some(
        token_file
            .to_str()
            .ok_or(RuntimeError::Configuration)?
            .to_owned(),
    );
    if let Some(path) = &options.k8s_ca_file {
        let metadata = std::fs::metadata(path).map_err(|_| RuntimeError::Configuration)?;
        if !metadata.is_file() || metadata.len() > 4 * 1024 * 1024 {
            return Err(RuntimeError::Configuration);
        }
        client.root_cert_file = Some(path.clone());
    }
    let (uid, gid) = user(options.k8s_step_user.as_ref())?;
    let (default_cpu_millis, default_memory_bytes) = defaults(policy)?;
    let volume_bytes = bytes(options.k8s_volume_size.as_ref(), 10 << 30)?;
    let output = bytes(options.k8s_max_output_bytes.as_ref(), volume_bytes)?;
    let files = usize::try_from(integer(options.k8s_max_output_files.as_ref(), 100_000)?)
        .map_err(|_| RuntimeError::Configuration)?;
    let options = KubernetesOptions {
        runner_id: identity(config)?,
        namespace,
        uid: i64::from(uid),
        gid: i64::from(gid),
        volume_bytes,
        storage_class: options
            .k8s_storage_class
            .as_ref()
            .map(|value| value.as_utf8().ok_or(RuntimeError::Configuration))
            .transpose()?,
        gpu_runtime_class: options
            .k8s_gpu_runtime_class
            .as_ref()
            .map(|value| value.as_utf8().ok_or(RuntimeError::Configuration))
            .transpose()?,
        transfer_image: text(
            options.k8s_transfer_image.as_ref(),
            "docker.io/library/busybox@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e",
        )?,
        tmp_bytes: bytes(options.k8s_tmp_size.as_ref(), 1 << 30)?,
        shm_bytes: bytes(options.k8s_shm_size.as_ref(), 64 << 20)?,
        default_cpu_millis,
        default_memory_bytes,
        api_service_host: policy.k8s_api_service_host.clone(),
        api_service_port: policy.k8s_api_service_port,
        namespace_policy_acknowledged: policy.k8s_namespace_policy_acknowledged,
        allow_unrestricted_egress: policy.allow_unrestricted_egress,
        limits: limits(
            output,
            files,
            options.k8s_scheduling_timeout.unwrap_or(900.0),
            options.k8s_exec_idle_timeout.unwrap_or(300.0),
        )?,
    };
    options.validate()?;
    Ok((options, client))
}

/// Validate local factory inputs without constructing a transport or backend.
pub(crate) fn check(config: &ProcessConfig, policy: &BackendPolicy) -> Result<(), RuntimeError> {
    match config.launcher {
        Some(crate::config::LauncherType::Docker) => {
            docker_options(config, policy)?;
        }
        Some(crate::config::LauncherType::Kubernetes) => {
            kubernetes_options(config, policy)?;
        }
        Some(crate::config::LauncherType::Local) => {
            config
                .step_root
                .as_ref()
                .ok_or(RuntimeError::Configuration)?;
        }
        None => return Err(RuntimeError::Configuration),
    }
    Ok(())
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
