//! Source-ordered runner configuration loading, independent of launch backends.
use crate::{
    credentials::{TokenFileError, read_token_file},
    launcher::PosixPath,
};
use cannery_core::{
    configuration_toml::{self, Input},
    json::{self, Document, Node, NodeId},
    principal::Secret,
};
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorClass {
    Configuration,
    Io,
    Overflow,
    Encoding,
    Value,
    Recursion,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Read,
    Utf8,
    Json,
    Toml,
    Table,
    Unknown,
    Required,
    Text,
    Integer,
    Seconds,
    Kind,
    Name,
    DuplicateName,
    Token(TokenFileError),
    SharedToken,
    Policy,
    Launcher,
    GpuMode,
    GpuList,
    GpuSyntax,
    GpuDuplicate,
    Repositories,
    Size,
    Path,
}
pub struct ConfigError {
    pub class: ErrorClass,
    pub location: String,
    pub kind: ErrorKind,
}
impl fmt::Debug for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigError")
            .field("class", &self.class)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}
impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "runner configuration {:?}: {:?}", self.class, self.kind)
    }
}
impl std::error::Error for ConfigError {}
fn error(location: &str, kind: ErrorKind) -> ConfigError {
    ConfigError {
        class: ErrorClass::Configuration,
        location: String::from(location),
        kind,
    }
}
fn exceptional(location: &str, class: ErrorClass, kind: ErrorKind) -> ConfigError {
    ConfigError {
        class,
        ..error(location, kind)
    }
}

/// Configurable resolver for native paths, including missing destination suffixes.
pub trait PathResolver {
    /// # Errors
    /// Preserve source OS, filesystem encoding and value failures without values.
    fn resolve(&self, path: &PosixPath) -> Result<PathBuf, ResolutionError>;
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolutionError {
    Io,
    Encoding,
    Value,
}
/// Production adapter resolving UTF-8 configuration paths with native filesystem policy.
#[derive(Clone, Copy, Debug, Default)]
pub struct NativePathResolver;
impl PathResolver for NativePathResolver {
    fn resolve(&self, path: &PosixPath) -> Result<PathBuf, ResolutionError> {
        crate::paths::resolve_text(&path.text(), false).map_err(|error| match error {
            crate::paths::PathError::InvalidNul => ResolutionError::Value,
            crate::paths::PathError::Encoding => ResolutionError::Encoding,
            crate::paths::PathError::NotDirectory | crate::paths::PathError::Io { .. } => {
                ResolutionError::Io
            }
        })
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyLoadError {
    Configuration,
    Encoding,
    Overflow,
    Value,
    Recursion,
}
/// Owned policy payload; parsing/validation belongs to the required policy loader.
pub enum EvaluationPolicy {
    Stock(Arc<Document>),
    Step {
        /// Complete registration envelope, including the evaluator identity.
        document: Arc<Document>,
        needs_data_root: bool,
    },
}
impl fmt::Debug for EvaluationPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Stock(_) => "StockPolicy([redacted])",
            Self::Step { .. } => "StepPolicy([redacted])",
        })
    }
}
pub trait PolicyLoader {
    /// # Errors
    /// Preserve policy configuration refusals versus uncaught source exceptions.
    fn load(&self, path: &PosixPath) -> Result<EvaluationPolicy, PolicyLoadError>;
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LauncherType {
    Local,
    Docker,
    Kubernetes,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobKind {
    Test,
    Eval,
    Experiment,
}
impl JobKind {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Eval => "eval",
            Self::Experiment => "experiment",
        }
    }
}
#[derive(Debug)]
pub enum KindConfig {
    Test,
    Eval(EvaluationPolicy),
    Experiment,
}
impl KindConfig {
    #[must_use]
    pub const fn job_kind(&self) -> JobKind {
        match self {
            Self::Test => JobKind::Test,
            Self::Eval(_) => JobKind::Eval,
            Self::Experiment => JobKind::Experiment,
        }
    }
    #[must_use]
    pub const fn needs_launcher(&self) -> bool {
        !matches!(self, Self::Eval(EvaluationPolicy::Stock(_)))
    }
    #[must_use]
    pub const fn needs_data_root(&self) -> bool {
        match self {
            Self::Eval(EvaluationPolicy::Stock(_)) => false,
            Self::Eval(EvaluationPolicy::Step {
                needs_data_root, ..
            }) => *needs_data_root,
            _ => true,
        }
    }
}
pub struct KindEntry {
    pub name: String,
    pub kind: KindConfig,
    pub token: Secret,
    pub poll_seconds: f64,
    pub concurrency: BigInt,
}
impl fmt::Debug for KindEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KindEntry([redacted])")
    }
}
#[derive(Default)]
pub struct DockerSettings {
    pub docker_host: Option<String>,
    pub docker_user: Option<String>,
    pub docker_job_root_host: Option<PosixPath>,
    pub docker_pids_limit: Option<BigInt>,
    pub docker_tmp_size: Option<String>,
    pub docker_shm_size: Option<String>,
    pub docker_gpu_mode: Option<String>,
    pub docker_gpu_devices: Option<String>,
}
#[derive(Default)]
pub struct KubernetesSettings {
    pub k8s_namespace: Option<String>,
    pub k8s_api_url: Option<String>,
    pub k8s_token_file: Option<PathBuf>,
    pub k8s_ca_file: Option<PathBuf>,
    pub k8s_storage_class: Option<String>,
    pub k8s_volume_size: Option<String>,
    pub k8s_gpu_runtime_class: Option<String>,
    pub k8s_step_user: Option<String>,
    pub k8s_scheduling_timeout: Option<f64>,
    pub k8s_transfer_image: Option<String>,
    pub k8s_tmp_size: Option<String>,
    pub k8s_shm_size: Option<String>,
    pub k8s_max_output_bytes: Option<String>,
    pub k8s_max_output_files: Option<BigInt>,
    pub k8s_exec_idle_timeout: Option<f64>,
}
#[derive(Default)]
pub struct GitHubSettings {
    pub token_file: Option<PathBuf>,
    pub app_id: Option<String>,
    pub app_key_file: Option<PathBuf>,
    pub app_installation_id: Option<String>,
    pub api_url: Option<String>,
    pub allowed_repos: Option<Vec<String>>,
}
pub struct ProcessConfig {
    pub api_url: String,
    pub project: String,
    pub kinds: Vec<KindEntry>,
    pub launcher: Option<LauncherType>,
    pub step_root: Option<PathBuf>,
    pub data_root: Option<PathBuf>,
    pub work_root: Option<PathBuf>,
    pub cache_root: Option<PathBuf>,
    pub cache_max_bytes: Option<String>,
    pub runner_id: Option<String>,
    pub docker: DockerSettings,
    pub kubernetes: KubernetesSettings,
    pub github: GitHubSettings,
}
impl fmt::Debug for ProcessConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProcessConfig([redacted])")
    }
}

const TOP: &[&str] = &[
    "api_url",
    "project",
    "data_root",
    "work_root",
    "cache_root",
    "cache_max_bytes",
    "launcher",
    "github",
    "kinds",
];
const DOCKER: &[&str] = &[
    "docker_host",
    "docker_user",
    "docker_job_root_host",
    "docker_pids_limit",
    "docker_tmp_size",
    "docker_shm_size",
    "docker_gpu_mode",
    "docker_gpu_devices",
];
const K8S: &[&str] = &[
    "k8s_namespace",
    "k8s_api_url",
    "k8s_token_file",
    "k8s_ca_file",
    "k8s_storage_class",
    "k8s_volume_size",
    "k8s_gpu_runtime_class",
    "k8s_step_user",
    "k8s_scheduling_timeout",
    "k8s_transfer_image",
    "k8s_tmp_size",
    "k8s_shm_size",
    "k8s_max_output_bytes",
    "k8s_max_output_files",
    "k8s_exec_idle_timeout",
];
const GITHUB: &[&str] = &[
    "token_file",
    "app_id",
    "app_key_file",
    "app_installation_id",
    "api_url",
    "allowed_repos",
];
const COMMON: &[&str] = &["kind", "name", "token_file", "poll_seconds", "concurrency"];
#[derive(Clone, Copy)]
enum Value<'a> {
    Toml(&'a Input),
    Json(&'a Document, NodeId),
}
impl<'a> Value<'a> {
    fn null(self) -> bool {
        matches!(self,Self::Json(d,id) if matches!(d.node(id),Some(Node::Null)))
    }
    fn text(self) -> Option<String> {
        match self {
            Self::Toml(Input::String(v)) => Some(String::from(v)),
            Self::Json(d, id) => match d.node(id) {
                Some(Node::String(v)) => Some(v.clone()),
                _ => None,
            },
            Self::Toml(_) => None,
        }
    }
    fn integer(self) -> Option<&'a BigInt> {
        match self {
            Self::Toml(Input::Integer(v)) => Some(v),
            Self::Json(d, id) => match d.node(id) {
                Some(Node::Integer(v)) => Some(v),
                _ => None,
            },
            Self::Toml(_) => None,
        }
    }
    fn float(self) -> Option<f64> {
        match self {
            Self::Toml(Input::Float(v)) => Some(*v),
            Self::Json(d, id) => match d.node(id) {
                Some(Node::Float(v)) => Some(*v),
                _ => None,
            },
            Self::Toml(_) => None,
        }
    }
    fn boolean(self) -> Option<bool> {
        match self {
            Self::Toml(Input::Boolean(v)) => Some(*v),
            Self::Json(d, id) => match d.node(id) {
                Some(Node::Bool(v)) => Some(*v),
                _ => None,
            },
            Self::Toml(_) => None,
        }
    }
    fn array(self) -> Option<Vec<Self>> {
        match self {
            Self::Toml(Input::Array(v)) => Some(v.iter().map(Self::Toml).collect()),
            Self::Json(d, id) => match d.node(id) {
                Some(Node::Array(v)) => Some(v.iter().map(|&id| Self::Json(d, id)).collect()),
                _ => None,
            },
            Self::Toml(_) => None,
        }
    }
    fn entries(self) -> Option<Vec<(String, Self)>> {
        match self {
            Self::Toml(Input::Table(v)) => Some(
                v.iter()
                    .map(|(k, v)| (String::from(k), Self::Toml(v)))
                    .collect(),
            ),
            Self::Json(d, id) => match d.node(id) {
                Some(Node::Object(v)) => Some(
                    v.iter()
                        .map(|(k, id)| (k.clone(), Self::Json(d, *id)))
                        .collect(),
                ),
                _ => None,
            },
            Self::Toml(_) => None,
        }
    }
}
struct Table<'a, 'r> {
    items: Vec<(String, Value<'a>)>,
    location: String,
    base: &'r Path,
    paths: &'r dyn PathResolver,
}
impl<'a, 'r> Table<'a, 'r> {
    fn new(
        value: Value<'a>,
        location: &str,
        allowed: &[&str],
        base: &'r Path,
        paths: &'r dyn PathResolver,
    ) -> Result<Self, ConfigError> {
        let items = value
            .entries()
            .ok_or_else(|| error(location, ErrorKind::Table))?;
        let key = items
            .iter()
            .filter(|(k, _)| !allowed.iter().any(|a| k.equals_utf8(a)))
            .map(|(k, _)| k)
            .min();
        if let Some(key) = key {
            return Err(ConfigError {
                location: if location.is_empty() {
                    key.clone()
                } else {
                    format!("{location}.{key}")
                },
                ..error(location, ErrorKind::Unknown)
            });
        }
        Ok(Self {
            items,
            location: location.into(),
            base,
            paths,
        })
    }
    fn at(&self, key: &str) -> String {
        if self.location.is_empty() {
            key.into()
        } else {
            format!("{}.{key}", self.location)
        }
    }
    fn get(&self, key: &str) -> Option<Value<'a>> {
        self.items
            .iter()
            .find(|(k, _)| k.equals_utf8(key))
            .map(|(_, v)| *v)
            .filter(|v| !v.null())
    }
    fn text(&self, key: &str, required: bool) -> Result<Option<String>, ConfigError> {
        let Some(v) = self.get(key) else {
            return if required {
                Err(error(&self.at(key), ErrorKind::Required))
            } else {
                Ok(None)
            };
        };
        v.text()
            .filter(|s| !s.is_empty())
            .map(Some)
            .ok_or_else(|| error(&self.at(key), ErrorKind::Text))
    }
    fn required(&self, key: &str) -> Result<String, ConfigError> {
        self.text(key, true)?
            .ok_or_else(|| error(&self.at(key), ErrorKind::Required))
    }
    fn path(&self, key: &str, required: bool) -> Result<Option<PathBuf>, ConfigError> {
        self.text(key, required)?
            .map(|v| {
                resolve(
                    self.paths,
                    &joined_path(self.base, &v, &self.at(key))?,
                    &self.at(key),
                )
            })
            .transpose()
    }
    fn integer(&self, key: &str) -> Result<Option<BigInt>, ConfigError> {
        let Some(v) = self.get(key) else {
            return Ok(None);
        };
        v.integer()
            .filter(|n| **n >= BigInt::from(1))
            .cloned()
            .map(Some)
            .ok_or_else(|| error(&self.at(key), ErrorKind::Integer))
    }
    fn seconds(&self, key: &str) -> Result<Option<f64>, ConfigError> {
        let Some(v) = self.get(key) else {
            return Ok(None);
        };
        let n = if let Some(n) = v.integer() {
            if n <= &BigInt::from(0) {
                return Err(error(&self.at(key), ErrorKind::Seconds));
            }
            n.to_f64().filter(|n| n.is_finite()).ok_or_else(|| {
                exceptional(&self.at(key), ErrorClass::Overflow, ErrorKind::Seconds)
            })?
        } else {
            v.float()
                .ok_or_else(|| error(&self.at(key), ErrorKind::Seconds))?
        };
        if n.is_finite() && n > 0.0 {
            Ok(Some(n))
        } else {
            Err(error(&self.at(key), ErrorKind::Seconds))
        }
    }
    fn identifier(&self, key: &str) -> Result<Option<String>, ConfigError> {
        if let Some(n) = self.get(key).and_then(Value::integer) {
            return integer_text(n, &self.at(key)).map(Some);
        }
        self.text(key, false)
    }
}
fn integer_text(n: &BigInt, location: &str) -> Result<String, ConfigError> {
    let text = n.to_string();
    // Identifiers cross the JSON protocol boundary: bound their numeric spelling.
    if text.len() > 1024 {
        return Err(exceptional(location, ErrorClass::Value, ErrorKind::Integer));
    }
    Ok(String::from(&text))
}
fn resolve(
    paths: &dyn PathResolver,
    path: &PosixPath,
    location: &str,
) -> Result<PathBuf, ConfigError> {
    paths.resolve(path).map_err(|e| {
        exceptional(
            location,
            match e {
                ResolutionError::Io => ErrorClass::Io,
                ResolutionError::Encoding => ErrorClass::Encoding,
                ResolutionError::Value => ErrorClass::Value,
            },
            ErrorKind::Path,
        )
    })
}
fn filesystem_text(path: &Path, location: &str) -> Result<String, ConfigError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| exceptional(location, ErrorClass::Encoding, ErrorKind::Path))
}
fn joined_path(base: &Path, child: &str, location: &str) -> Result<PosixPath, ConfigError> {
    let joined = base.join(child);
    Ok(PosixPath::new(&filesystem_text(&joined, location)?))
}
fn matches_name(text: &str) -> bool {
    let p = text.as_bytes();
    (1..=63).contains(&p.len())
        && p.first().is_some_and(|v| matches!(v,48..=57|97..=122))
        && p.iter().all(|v| matches!(v,48..=57|97..=122|95|45))
}

#[derive(Default)]
struct ParsedLauncher {
    launcher: Option<LauncherType>,
    step_root: Option<PathBuf>,
    runner_id: Option<String>,
    docker: DockerSettings,
    kubernetes: KubernetesSettings,
}
fn launcher(top: &Table<'_, '_>) -> Result<ParsedLauncher, ConfigError> {
    let Some(value) = top.get("launcher") else {
        return Ok(ParsedLauncher::default());
    };
    let keys: Vec<_> = [&["type", "step_root", "runner_id"][..], DOCKER, K8S].concat();
    let t = Table::new(value, "launcher", &keys, top.base, top.paths)?;
    let kind = match t.required("type")? {
        v if v.equals_utf8("local") => LauncherType::Local,
        v if v.equals_utf8("docker") => LauncherType::Docker,
        v if v.equals_utf8("kubernetes") => LauncherType::Kubernetes,
        _ => return Err(error("launcher.type", ErrorKind::Launcher)),
    };
    let host_root = t.text("docker_job_root_host", false)?;
    let docker = DockerSettings {
        docker_host: t.text("docker_host", false)?,
        docker_user: t.text("docker_user", false)?,
        docker_job_root_host: host_root.map(|v| PosixPath::new(&v)),
        docker_pids_limit: t.integer("docker_pids_limit")?,
        docker_tmp_size: t.text("docker_tmp_size", false)?,
        docker_shm_size: t.text("docker_shm_size", false)?,
        docker_gpu_mode: t.text("docker_gpu_mode", false)?,
        docker_gpu_devices: gpu_key(&t)?,
    };
    if docker
        .docker_gpu_mode
        .as_ref()
        .is_some_and(|v| !v.equals_utf8("nvidia") && !v.equals_utf8("cos"))
    {
        return Err(error("launcher.docker_gpu_mode", ErrorKind::GpuMode));
    }
    let kubernetes = KubernetesSettings {
        k8s_namespace: t.text("k8s_namespace", false)?,
        k8s_api_url: t.text("k8s_api_url", false)?,
        k8s_token_file: t.path("k8s_token_file", false)?,
        k8s_ca_file: t.path("k8s_ca_file", false)?,
        k8s_storage_class: t.text("k8s_storage_class", false)?,
        k8s_volume_size: t.text("k8s_volume_size", false)?,
        k8s_gpu_runtime_class: t.text("k8s_gpu_runtime_class", false)?,
        k8s_step_user: t.text("k8s_step_user", false)?,
        k8s_scheduling_timeout: t.seconds("k8s_scheduling_timeout")?,
        k8s_transfer_image: t.text("k8s_transfer_image", false)?,
        k8s_tmp_size: t.text("k8s_tmp_size", false)?,
        k8s_shm_size: t.text("k8s_shm_size", false)?,
        k8s_max_output_bytes: t.text("k8s_max_output_bytes", false)?,
        k8s_max_output_files: t.integer("k8s_max_output_files")?,
        k8s_exec_idle_timeout: t.seconds("k8s_exec_idle_timeout")?,
    };
    Ok(ParsedLauncher {
        launcher: Some(kind),
        step_root: t.path("step_root", false)?,
        runner_id: t.text("runner_id", false)?,
        docker,
        kubernetes,
    })
}
fn gpu_key(t: &Table<'_, '_>) -> Result<Option<String>, ConfigError> {
    let value = if let Some(items) = t.get("docker_gpu_devices").and_then(Value::array) {
        // Source validates every element before rendering any integer. A later
        // wrong type therefore wins over an earlier decimal-rendering failure.
        if items.iter().any(|v| v.integer().is_none()) {
            return Err(error(&t.at("docker_gpu_devices"), ErrorKind::GpuList));
        }
        let mut strings = Vec::new();
        for v in items {
            let n = v
                .integer()
                .ok_or_else(|| error(&t.at("docker_gpu_devices"), ErrorKind::GpuList))?;
            strings.push(
                integer_text(n, &t.at("docker_gpu_devices"))?
                    .as_utf8()
                    .ok_or_else(|| {
                        exceptional(
                            &t.at("docker_gpu_devices"),
                            ErrorClass::Encoding,
                            ErrorKind::GpuList,
                        )
                    })?,
            );
        }
        Some(String::from(&if strings.is_empty() {
            "none".to_owned()
        } else {
            strings.join(",")
        }))
    } else {
        t.text("docker_gpu_devices", false)?
    };
    if let Some(text) = &value {
        validate_gpu_text(text, &t.at("docker_gpu_devices"))?;
    }
    Ok(value)
}
fn validate_gpu_text(text: &str, location: &str) -> Result<(), ConfigError> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("none") {
        return Ok(());
    }
    let mut seen = BTreeSet::new();
    for part in text.split(',').map(str::trim) {
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(error(location, ErrorKind::GpuSyntax));
        }
        let index = part
            .parse::<u32>()
            .map_err(|_| error(location, ErrorKind::GpuSyntax))?;
        if !seen.insert(index) {
            return Err(error(location, ErrorKind::GpuDuplicate));
        }
    }
    Ok(())
}
fn github(top: &Table<'_, '_>) -> Result<GitHubSettings, ConfigError> {
    let Some(v) = top.get("github") else {
        return Ok(GitHubSettings::default());
    };
    let t = Table::new(v, "github", GITHUB, top.base, top.paths)?;
    let repos = t
        .get("allowed_repos")
        .map(|v| {
            v.array()
                .ok_or_else(|| error("github.allowed_repos", ErrorKind::Repositories))
                .and_then(|a| {
                    a.into_iter()
                        .map(|v| {
                            v.text().ok_or_else(|| {
                                error("github.allowed_repos", ErrorKind::Repositories)
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
        })
        .transpose()?;
    Ok(GitHubSettings {
        token_file: t.path("token_file", false)?,
        app_id: t.identifier("app_id")?,
        app_key_file: t.path("app_key_file", false)?,
        app_installation_id: t.identifier("app_installation_id")?,
        api_url: t.text("api_url", false)?,
        allowed_repos: repos,
    })
}
// Keep plaintext lookup keys private and unavailable through diagnostics.
#[derive(Default)]
struct TokenKinds(BTreeMap<String, JobKind>);
impl fmt::Debug for TokenKinds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TokenKinds([redacted])")
    }
}
impl TokenKinds {
    fn conflicts(&self, token: &Secret, kind: JobKind) -> bool {
        self.0
            .get(token.expose())
            .is_some_and(|previous| *previous != kind)
    }
    fn remember(&mut self, token: &Secret, kind: JobKind) {
        self.0.entry(token.expose().to_owned()).or_insert(kind);
    }
}
fn kinds(top: &Table<'_, '_>, policies: &dyn PolicyLoader) -> Result<Vec<KindEntry>, ConfigError> {
    let list = top
        .get("kinds")
        .and_then(Value::array)
        .filter(|a| !a.is_empty())
        .ok_or_else(|| error("kinds", ErrorKind::Kind))?;
    let mut entries: Vec<KindEntry> = Vec::new();
    let mut names = BTreeSet::new();
    let mut tokens = TokenKinds::default();
    for (index, v) in list.into_iter().enumerate() {
        let location = format!("kinds[{index}]");
        let items = v
            .entries()
            .ok_or_else(|| error(&location, ErrorKind::Table))?;
        let name = items
            .iter()
            .find(|(k, _)| k.equals_utf8("kind"))
            .and_then(|(_, v)| v.text());
        let kind = match name {
            Some(v) if v.equals_utf8("test") => JobKind::Test,
            Some(v) if v.equals_utf8("eval") => JobKind::Eval,
            Some(v) if v.equals_utf8("experiment") => JobKind::Experiment,
            _ => return Err(error(&format!("{location}.kind"), ErrorKind::Kind)),
        };
        let mut keys = COMMON.to_vec();
        if kind == JobKind::Eval {
            keys.push("policy");
        }
        let t = Table::new(v, &location, &keys, top.base, top.paths)?;
        let label = t
            .text("name", false)?
            .unwrap_or_else(|| String::from(kind.name()));
        if !matches_name(&label) {
            return Err(error(&t.at("name"), ErrorKind::Name));
        }
        if !names.insert(label.clone()) {
            return Err(error(&t.at("name"), ErrorKind::DuplicateName));
        }
        let path = t
            .path("token_file", true)?
            .ok_or_else(|| error(&t.at("token_file"), ErrorKind::Required))?;
        let token =
            read_token_file(&path).map_err(|e| error(&t.at("token_file"), ErrorKind::Token(e)))?;
        if tokens.conflicts(&token, kind) {
            return Err(error(&t.at("token_file"), ErrorKind::SharedToken));
        }
        let kind = match kind {
            JobKind::Test => KindConfig::Test,
            JobKind::Experiment => KindConfig::Experiment,
            JobKind::Eval => {
                let p = t
                    .get("policy")
                    .and_then(Value::text)
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| error(&t.at("policy"), ErrorKind::Policy))?;
                let path = joined_path(top.base, &p, &t.at("policy"))?;
                let policy = policies.load(&path).map_err(|e| {
                    exceptional(
                        &t.at("policy"),
                        match e {
                            PolicyLoadError::Configuration => ErrorClass::Configuration,
                            PolicyLoadError::Encoding => ErrorClass::Encoding,
                            PolicyLoadError::Overflow => ErrorClass::Overflow,
                            PolicyLoadError::Value => ErrorClass::Value,
                            PolicyLoadError::Recursion => ErrorClass::Recursion,
                        },
                        ErrorKind::Policy,
                    )
                })?;
                KindConfig::Eval(policy)
            }
        };
        let entry = KindEntry {
            name: label,
            kind,
            token,
            poll_seconds: t.seconds("poll_seconds")?.unwrap_or(10.0),
            concurrency: t.integer("concurrency")?.unwrap_or_else(|| BigInt::from(1)),
        };
        tokens.remember(&entry.token, entry.kind.job_kind());
        entries.push(entry);
    }
    Ok(entries)
}
fn parse(
    value: Value<'_>,
    base: &Path,
    paths: &dyn PathResolver,
    policies: &dyn PolicyLoader,
) -> Result<ProcessConfig, ConfigError> {
    let top = Table::new(value, "", TOP, base, paths)?;
    let ParsedLauncher {
        launcher,
        step_root,
        runner_id,
        docker,
        kubernetes,
    } = launcher(&top)?;
    let cache_max = top.get("cache_max_bytes");
    if cache_max
        .is_some_and(|v| v.text().is_none() && v.integer().is_none() && v.boolean().is_none())
    {
        return Err(error("cache_max_bytes", ErrorKind::Size));
    }
    let api_url = top.required("api_url")?;
    let project = top.required("project")?;
    let kinds = kinds(&top, policies)?;
    let data_root = top.path("data_root", false)?;
    let work_root = top.path("work_root", false)?;
    let cache_root = top.path("cache_root", false)?;
    let cache_max_bytes = cache_max
        .map(|v| {
            if let Some(s) = v.text() {
                Ok(s)
            } else if let Some(n) = v.integer() {
                integer_text(n, "cache_max_bytes")
            } else if let Some(b) = v.boolean() {
                Ok(String::from(if b { "True" } else { "False" }))
            } else {
                Err(error("cache_max_bytes", ErrorKind::Size))
            }
        })
        .transpose()?;
    Ok(ProcessConfig {
        api_url,
        project,
        kinds,
        launcher,
        step_root,
        data_root,
        work_root,
        cache_root,
        cache_max_bytes,
        runner_id,
        docker,
        kubernetes,
        github: github(&top)?,
    })
}
/// Load actual UTF-8 JSON/TOML and tokens, with required policy and path seams.
/// `json_nesting_budget` is an explicit limit capped by the native JSON boundary.
/// # Errors
/// Preserves configuration refusals versus uncaught source exception categories.
pub fn load_config_file(
    path: &Path,
    policies: &dyn PolicyLoader,
    paths: &dyn PathResolver,
    json_nesting_budget: usize,
) -> Result<ProcessConfig, ConfigError> {
    let resolved = resolve(paths, &PosixPath::new(&filesystem_text(path, "")?), "")?;
    let base = resolved.parent().unwrap_or(Path::new("/"));
    let bytes = fs::read(path).map_err(|_| error("", ErrorKind::Read))?;
    let text = String::from_utf8(bytes).map_err(|_| error("", ErrorKind::Utf8))?;
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    if path
        .extension()
        .is_some_and(|s| s.eq_ignore_ascii_case("json"))
    {
        let document = json::decode_str(&text, json_nesting_budget).map_err(|e| match e {
            json::DecodeError::Recursion => exceptional("", ErrorClass::Recursion, ErrorKind::Json),
            json::DecodeError::IntegerLimit => exceptional("", ErrorClass::Value, ErrorKind::Json),
            _ => error("", ErrorKind::Json),
        })?;
        parse(
            Value::Json(&document, document.root()),
            base,
            paths,
            policies,
        )
    } else {
        let table = configuration_toml::document(&text).map_err(|e| match e {
            configuration_toml::InvalidToml::Syntax => error("", ErrorKind::Toml),
        })?;
        parse(Value::Toml(&Input::Table(table)), base, paths, policies)
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;

#[cfg(test)]
mod tests {
    use super::{ErrorKind, joined_path, validate_gpu_text};
    use std::path::Path;

    #[test]
    fn lexical_paths_retain_root_and_absolute_children() {
        for (base, child, expected) in [
            ("/fixture", "file", "/fixture/file"),
            ("/fixture", "/file", "/file"),
            ("/", "file", "/file"),
            ("/fixture", "../file", "/fixture/../file"),
        ] {
            assert_eq!(
                joined_path(Path::new(base), child, "")
                    .map(|path| path.text())
                    .ok()
                    .as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn native_path_text_boundary_and_name_grammar_are_explicit() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};
        let base = PathBuf::from(OsString::from_vec(b"/private-\xff".to_vec()));
        let failure = joined_path(&base, "relative", "data_root");
        assert!(matches!(
            failure,
            Err(super::ConfigError {
                class: super::ErrorClass::Encoding,
                kind: ErrorKind::Path,
                ..
            })
        ));
        assert_eq!(
            joined_path(&base, "/absolute", "data_root")
                .map(|path| path.text())
                .ok()
                .as_deref(),
            Some("/absolute")
        );
        assert!(super::filesystem_text(&base, "").is_err());
        assert!(super::matches_name("valid-name_1"));
        for name in ["name\n", "name\r\n", "é", ""] {
            assert!(!super::matches_name(name));
        }
    }

    #[test]
    fn numeric_identifier_spelling_is_bounded() {
        use num_bigint::BigInt;
        let within = BigInt::from(10u8).pow(1023);
        let beyond = BigInt::from(10u8).pow(1024);
        assert!(super::integer_text(&within, "runner_id").is_ok());
        assert!(super::integer_text(&beyond, "runner_id").is_err());
        assert!(super::integer_text(&-within, "runner_id").is_err());
    }

    #[test]
    fn scheduling_seconds_require_finite_positive_values() -> Result<(), Box<dyn std::error::Error>>
    {
        use cannery_core::json::{DocumentBuilder, Node};
        for seconds in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN, 0.0, -1.0, 0.25] {
            let mut builder = DocumentBuilder::new();
            let root = builder.push(Node::Float(seconds))?;
            let doc = builder.finish(root)?;
            let paths = super::NativePathResolver;
            let table = super::Table {
                items: vec![("seconds".into(), super::Value::Json(&doc, root))],
                location: String::new(),
                base: Path::new("/"),
                paths: &paths,
            };
            assert_eq!(
                table.seconds("seconds").is_ok(),
                seconds.is_finite() && seconds > 0.0
            );
        }
        Ok(())
    }

    #[test]
    fn device_ids_use_ascii_digits_and_bounded_unsigned_integers() {
        for value in ["0", "0,2", " 0, 2 ", "none", " NoNe ", "4294967295"] {
            assert!(validate_gpu_text(value, "devices").is_ok(), "{value}");
        }
        for value in ["", "0,,1", "-1", "+1", "1.5", "٠", "１", "²", "4294967296"] {
            assert!(
                matches!(validate_gpu_text(value, "devices"), Err(error) if error.kind == ErrorKind::GpuSyntax),
                "{value}"
            );
        }
        for value in ["0,0", "0,00"] {
            assert!(
                matches!(validate_gpu_text(value, "devices"), Err(error) if error.kind == ErrorKind::GpuDuplicate)
            );
        }
    }
}
