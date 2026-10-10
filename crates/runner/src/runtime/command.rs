//! Runner CLI is independent of the installation/server database settings.
use super::{
    RuntimeError,
    backend::LocalBackend,
    http::ApiClient,
    verify::{AppliedPolicy, Verifier},
    worker::Resources,
};
use crate::{
    config::{self, ProcessConfig},
    launcher::PosixPath,
};

use clap::{Args, ValueEnum};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Copy, ValueEnum)]
pub enum Launcher {
    Local,
    Docker,
    Kubernetes,
}
#[derive(Args)]
pub struct RunnerArgs {
    #[command(flatten)]
    pub backend_policy: super::container_command::BackendPolicy,
    #[arg(long)]
    pub config: Option<PathBuf>,
    #[arg(long)]
    pub once: bool,
    /// Validate configuration and local credentials without contacting any service.
    #[arg(long, conflicts_with = "once")]
    pub check_config: bool,
    #[arg(long, value_enum)]
    pub launcher: Option<Launcher>,
    #[arg(long)]
    pub unisolated_local: bool,
    #[arg(long)]
    pub token_file: Option<PathBuf>,
    /// The verifier's policy: a stock configuration or a policy step document.
    #[arg(long)]
    pub policy: Option<PathBuf>,
    #[arg(long)]
    pub api_url: Option<String>,
    #[arg(long)]
    pub project: Option<String>,
    #[arg(long)]
    pub data_root: Option<PathBuf>,
    #[arg(long)]
    pub step_root: Option<PathBuf>,
    #[arg(long)]
    pub work_root: Option<PathBuf>,
    #[arg(long)]
    pub poll_seconds: Option<f64>,
    #[arg(long)]
    pub cache_root: Option<PathBuf>,
    #[arg(long)]
    pub cache_max_bytes: Option<String>,
    #[arg(long)]
    pub runner_id: Option<String>,
    #[arg(long)]
    pub github_token_file: Option<PathBuf>,
    #[arg(long)]
    pub github_app_id: Option<String>,
    #[arg(long)]
    pub github_app_key_file: Option<PathBuf>,
    #[arg(long)]
    pub github_app_installation_id: Option<String>,
    #[arg(long)]
    pub github_api_url: Option<String>,
    #[arg(long)]
    pub github_allowed_repos: Vec<String>,
    #[arg(long)]
    pub docker_host: Option<String>,
    #[arg(long)]
    pub docker_user: Option<String>,
    #[arg(long)]
    pub docker_job_root_host: Option<PathBuf>,
    #[arg(long)]
    pub docker_pids_limit: Option<i64>,
    #[arg(long)]
    pub docker_tmp_size: Option<String>,
    #[arg(long)]
    pub docker_shm_size: Option<String>,
    #[arg(long)]
    pub docker_gpu_mode: Option<String>,
    #[arg(long)]
    pub docker_gpu_devices: Option<String>,
    #[arg(long)]
    pub k8s_namespace: Option<String>,
    #[arg(long)]
    pub k8s_api_url: Option<String>,
    #[arg(long)]
    pub k8s_token_file: Option<PathBuf>,
    #[arg(long)]
    pub k8s_ca_file: Option<PathBuf>,
    #[arg(long)]
    pub k8s_storage_class: Option<String>,
    #[arg(long)]
    pub k8s_volume_size: Option<String>,
    #[arg(long)]
    pub k8s_gpu_runtime_class: Option<String>,
    #[arg(long)]
    pub k8s_step_user: Option<String>,
    #[arg(long)]
    pub k8s_scheduling_timeout: Option<f64>,
    #[arg(long)]
    pub k8s_transfer_image: Option<String>,
    #[arg(long)]
    pub k8s_tmp_size: Option<String>,
    #[arg(long)]
    pub k8s_shm_size: Option<String>,
    #[arg(long)]
    pub k8s_max_output_bytes: Option<String>,
    #[arg(long)]
    pub k8s_max_output_files: Option<i64>,
    #[arg(long)]
    pub k8s_exec_idle_timeout: Option<f64>,
}
fn text(value: Option<String>) -> Option<String> {
    value.map(|value| String::from(&value))
}
fn resolve(path: &Path) -> Result<PathBuf, RuntimeError> {
    crate::paths::resolve(path, false).map_err(|_| RuntimeError::Configuration)
}
impl RunnerArgs {
    fn overrides(&self) -> bool {
        self.launcher.is_some()
            || self.unisolated_local
            || self.token_file.is_some()
            || self.policy.is_some()
            || self.api_url.is_some()
            || self.project.is_some()
            || self.data_root.is_some()
            || self.step_root.is_some()
            || self.work_root.is_some()
            || self.poll_seconds.is_some()
            || self.cache_root.is_some()
            || self.cache_max_bytes.is_some()
            || self.runner_id.is_some()
            || self.github_token_file.is_some()
            || self.github_app_id.is_some()
            || self.github_app_key_file.is_some()
            || self.github_app_installation_id.is_some()
            || self.github_api_url.is_some()
            || !self.github_allowed_repos.is_empty()
            || self.docker_host.is_some()
            || self.docker_user.is_some()
            || self.docker_job_root_host.is_some()
            || self.docker_pids_limit.is_some()
            || self.docker_tmp_size.is_some()
            || self.docker_shm_size.is_some()
            || self.docker_gpu_mode.is_some()
            || self.docker_gpu_devices.is_some()
            || self.k8s_namespace.is_some()
            || self.k8s_api_url.is_some()
            || self.k8s_token_file.is_some()
            || self.k8s_ca_file.is_some()
            || self.k8s_storage_class.is_some()
            || self.k8s_volume_size.is_some()
            || self.k8s_gpu_runtime_class.is_some()
            || self.k8s_step_user.is_some()
            || self.k8s_scheduling_timeout.is_some()
            || self.k8s_transfer_image.is_some()
            || self.k8s_tmp_size.is_some()
            || self.k8s_shm_size.is_some()
            || self.k8s_max_output_bytes.is_some()
            || self.k8s_max_output_files.is_some()
            || self.k8s_exec_idle_timeout.is_some()
    }
    /// # Errors
    /// Config files replace field overrides; execution/preflight and operator policy remain flags.
    /// Token files retain strict permissions.
    // Keep the complete CLI-to-configuration field mapping together for review.
    #[allow(clippy::too_many_lines)]
    pub fn load(self) -> Result<(ProcessConfig, bool), RuntimeError> {
        let once = self.once;
        if let Some(path) = &self.config {
            if self.overrides() {
                return Err(RuntimeError::Configuration);
            }
            let loader = crate::policy::FilePolicyLoader {
                entry_point: crate::cli_depth::PolicyEntryPoint::RunnerVerifyKind,
                repr_nesting_budget: 512,
            };
            return config::load_config_file(
                path,
                &loader,
                &config::NativePathResolver,
                crate::cli_depth::JSON_CONTAINERS,
            )
            .map(|config| (config, once))
            .map_err(|_| RuntimeError::Configuration);
        }
        if self.unisolated_local
            && self
                .launcher
                .is_some_and(|launcher| !matches!(launcher, Launcher::Local))
        {
            return Err(RuntimeError::Configuration);
        }
        let launcher = if self.unisolated_local {
            Launcher::Local
        } else {
            self.launcher.ok_or(RuntimeError::Configuration)?
        };
        let api = self.api_url.ok_or(RuntimeError::Configuration)?;
        let project = self.project.ok_or(RuntimeError::Configuration)?;
        let token_file = self
            .token_file
            .or_else(|| {
                std::env::var("CANNERY_RUNNER_TOKEN_FILE")
                    .ok()
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
            })
            .ok_or(RuntimeError::Configuration)?;
        let token = crate::credentials::read_token_file(&token_file)
            .map_err(|_| RuntimeError::Configuration)?;
        let poll = self.poll_seconds.unwrap_or(10.0);
        if !poll.is_finite() || poll < 0.0 || std::time::Duration::try_from_secs_f64(poll).is_err()
        {
            return Err(RuntimeError::Configuration);
        }
        // Without a configuration file the runner is one verify kind.
        let policy = self.policy.ok_or(RuntimeError::Configuration)?;
        let loader = crate::policy::FilePolicyLoader {
            entry_point: crate::cli_depth::PolicyEntryPoint::RunnerVerifyKind,
            repr_nesting_budget: 512,
        };
        let policy = config::PolicyLoader::load(
            &loader,
            &PosixPath::new(&resolve(&policy)?.to_string_lossy()),
        )
        .map_err(|_| RuntimeError::Configuration)?;
        Ok((
            ProcessConfig {
                api_url: String::from(&api),
                project: String::from(&project),
                kinds: vec![config::KindEntry {
                    name: String::from("verify"),
                    kind: config::KindConfig::Verify(policy),
                    token,
                    poll_seconds: poll,
                    concurrency: 1.into(),
                }],
                launcher: Some(match launcher {
                    Launcher::Local => config::LauncherType::Local,
                    Launcher::Docker => config::LauncherType::Docker,
                    Launcher::Kubernetes => config::LauncherType::Kubernetes,
                }),
                data_root: Some(resolve(
                    &self.data_root.ok_or(RuntimeError::Configuration)?,
                )?),
                step_root: self.step_root.map(|path| resolve(&path)).transpose()?,
                work_root: self.work_root.map(|path| resolve(&path)).transpose()?,
                cache_root: self.cache_root.map(|path| resolve(&path)).transpose()?,
                cache_max_bytes: text(self.cache_max_bytes),
                runner_id: text(self.runner_id),
                docker: config::DockerSettings {
                    docker_host: text(self.docker_host),
                    docker_user: text(self.docker_user),
                    docker_job_root_host: self
                        .docker_job_root_host
                        .map(|path| PosixPath::new(&path.to_string_lossy())),
                    docker_pids_limit: self.docker_pids_limit.map(Into::into),
                    docker_tmp_size: text(self.docker_tmp_size),
                    docker_shm_size: text(self.docker_shm_size),
                    docker_gpu_mode: text(self.docker_gpu_mode),
                    docker_gpu_devices: text(self.docker_gpu_devices),
                },
                kubernetes: config::KubernetesSettings {
                    k8s_namespace: text(self.k8s_namespace),
                    k8s_api_url: text(self.k8s_api_url),
                    k8s_token_file: self.k8s_token_file,
                    k8s_ca_file: self.k8s_ca_file,
                    k8s_storage_class: text(self.k8s_storage_class),
                    k8s_volume_size: text(self.k8s_volume_size),
                    k8s_gpu_runtime_class: text(self.k8s_gpu_runtime_class),
                    k8s_step_user: text(self.k8s_step_user),
                    k8s_scheduling_timeout: self.k8s_scheduling_timeout,
                    k8s_transfer_image: text(self.k8s_transfer_image),
                    k8s_tmp_size: text(self.k8s_tmp_size),
                    k8s_shm_size: text(self.k8s_shm_size),
                    k8s_max_output_bytes: text(self.k8s_max_output_bytes),
                    k8s_max_output_files: self.k8s_max_output_files.map(Into::into),
                    k8s_exec_idle_timeout: self.k8s_exec_idle_timeout,
                },
                github: config::GitHubSettings {
                    token_file: self.github_token_file,
                    app_id: text(self.github_app_id),
                    app_key_file: self.github_app_key_file,
                    app_installation_id: text(self.github_app_installation_id),
                    api_url: text(self.github_api_url),
                    allowed_repos: (!self.github_allowed_repos.is_empty())
                        .then(|| self.github_allowed_repos.iter().map(String::from).collect()),
                },
            },
            once,
        ))
    }
}
/// # Errors
/// Preflight returns before transport or state creation; invalid factories fail closed.
pub async fn run(args: RunnerArgs) -> Result<i32, RuntimeError> {
    let check = args.check_config;
    let policy = args.backend_policy.clone();
    let (config, once) = args.load()?;
    if check {
        ApiClient::check_root(
            &config
                .api_url
                .as_utf8()
                .ok_or(RuntimeError::Configuration)?,
        )?;
        config
            .project
            .as_utf8()
            .ok_or(RuntimeError::Configuration)?;
        for entry in &config.kinds {
            if !entry.poll_seconds.is_finite()
                || std::time::Duration::try_from_secs_f64(entry.poll_seconds.max(0.0)).is_err()
            {
                return Err(RuntimeError::Configuration);
            }
            config
                .data_root
                .as_ref()
                .ok_or(RuntimeError::Configuration)?;
            if let config::KindConfig::Verify(policy) = &entry.kind {
                applied(policy)?;
            }
        }
        super::container_command::check(&config, &policy)?;
        super::provision::CodeProvisioner::check_config(&config)?;
        return Ok(0);
    }
    run_config_with_policy(config, once, &policy).await
}
/// Parse a verify kind's policy as the worker applies it; a policy step must
/// also be a valid step manifest.
fn applied(policy: &config::VerifyPolicy) -> Result<AppliedPolicy, RuntimeError> {
    let entry_point = crate::cli_depth::PolicyEntryPoint::RunnerVerifyKind;
    Ok(match policy {
        config::VerifyPolicy::Stock(document) => AppliedPolicy::Stock(Arc::new(
            crate::policy::parse_policy(document.clone(), entry_point, 256)
                .map_err(|_| RuntimeError::Configuration)?,
        )),
        config::VerifyPolicy::Step(document) => {
            let step = crate::policy::parse_step_policy(document.clone(), entry_point, 256)
                .map_err(|_| RuntimeError::Configuration)?;
            let validator = cannery_core::contracts::ContractValidator::new()
                .map_err(|_| RuntimeError::Configuration)?;
            if !validator.is_valid(
                cannery_core::contracts::ContractKind::StepManifest,
                &step.document,
            ) {
                return Err(RuntimeError::Configuration);
            }
            AppliedPolicy::Step(Arc::new(step))
        }
    })
}
/// # Errors
/// Validate all factories before claiming work; installed commands share this path.
pub async fn run_config(config: ProcessConfig, once: bool) -> Result<i32, RuntimeError> {
    run_config_with_policy(
        config,
        once,
        &super::container_command::BackendPolicy::default(),
    )
    .await
}
/// Every kind runs steps: one launcher backend and code provisioner serve them all.
async fn run_config_with_policy(
    config: ProcessConfig,
    once: bool,
    policy: &super::container_command::BackendPolicy,
) -> Result<i32, RuntimeError> {
    let backend = match config.launcher {
        Some(config::LauncherType::Local) => Arc::new(LocalBackend::new(
            std::env::current_exe()?,
            PathBuf::from("python3"),
            config
                .step_root
                .clone()
                .ok_or(RuntimeError::Configuration)?,
            true,
        )?) as Arc<dyn super::backend::Backend>,
        Some(config::LauncherType::Docker) => super::container_command::docker(&config, policy)?,
        Some(config::LauncherType::Kubernetes) => {
            super::container_command::kubernetes(&config, policy)?
        }
        None => return Err(RuntimeError::Configuration),
    };
    let provisioner = Arc::new(super::provision::CodeProvisioner::new(
        &config,
        backend.clone(),
    )?) as Arc<dyn super::worker::Provisioner>;
    let client = ApiClient::new(
        &config
            .api_url
            .as_utf8()
            .ok_or(RuntimeError::Configuration)?,
    )?;
    let project = config
        .project
        .as_utf8()
        .ok_or(RuntimeError::Configuration)?;
    let data_root = config.data_root.ok_or(RuntimeError::Configuration)?;
    let work_root = config.work_root.unwrap_or_else(std::env::temp_dir);
    let mut entries = vec![];
    let mut workers = vec![];
    for entry in config.kinds {
        entries.push(crate::worker_process::Entry {
            name: entry.name,
            poll_seconds: entry.poll_seconds,
            concurrency: entry.concurrency,
        });
        let resources = Resources {
            client: client.clone(),
            token: entry.token,
            project: project.clone(),
            data_root: data_root.clone(),
            work_root: work_root.clone(),
            backend: backend.clone(),
            provisioner: provisioner.clone(),
            validator: Arc::new(super::validation::NativeOutputValidator::runtime_policy()),
        };
        let worker: Arc<dyn super::process::RuntimeWorker> = match &entry.kind {
            config::KindConfig::Experiment => {
                Arc::new(super::experiment::Experiment::new(resources))
            }
            config::KindConfig::Verify(policy) => {
                Arc::new(Verifier::new(resources, applied(policy)?)?)
            }
        };
        workers.push(worker);
    }
    super::process::run(entries, workers, backend, provisioner, once).await
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
