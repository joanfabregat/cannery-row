//! Pure launcher contracts. These helpers do not launch or contact anything.

use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::Zero;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ContractError {
    #[error("invalid resource quantity")]
    Quantity,
    #[error("resource quantity exceeds the 1024-byte input limit")]
    IntegerLimit,
    #[error("image is not pinned by digest")]
    Image,
    #[error("unsupported mount target")]
    MountTarget,
    #[error("mount source is not absolute")]
    MountSource,
    #[error("two mounts share a target")]
    DuplicateMount,
    #[error("path is not under /cr")]
    Path,
    #[error("path cannot be encoded by the filesystem")]
    Encoding,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Outcome {
    pub exit_code: Option<BigInt>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub oom_killed: bool,
    pub output_error: Option<String>,
}
impl Outcome {
    #[must_use]
    pub fn new(exit_code: Option<BigInt>) -> Self {
        Self {
            exit_code,
            timed_out: false,
            cancelled: false,
            oom_killed: false,
            output_error: None,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Resources {
    pub cpu: Option<BigRational>,
    pub memory_bytes: Option<BigInt>,
    pub gpus: BigInt,
}

/// `PurePosixPath` lexical normalization, deliberately retaining `..`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PosixPath {
    anchor: u8,
    parts: Vec<String>,
}
impl PosixPath {
    #[must_use]
    pub fn new(value: &str) -> Self {
        let slashes = value.bytes().take_while(|&byte| byte == b'/').count();
        let anchor = if slashes == 2 {
            2
        } else {
            u8::from(slashes > 0)
        };
        let parts = value
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .map(str::to_owned)
            .collect();
        Self { anchor, parts }
    }
    #[must_use]
    pub fn text(&self) -> String {
        let mut text = "/".repeat(usize::from(self.anchor));
        for (index, part) in self.parts.iter().enumerate() {
            if index > 0 {
                text.push('/');
            }
            text.push_str(part);
        }
        if text.is_empty() {
            text.push('.');
        }
        text
    }
    fn cr_relative(&self) -> Result<&[String], ContractError> {
        if self.anchor == 1 && self.parts.first().is_some_and(|p| p.equals_utf8("cr")) {
            Ok(&self.parts[1..])
        } else {
            Err(ContractError::Path)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mount {
    source: PathBuf,
    target: PosixPath,
    read_only: bool,
}
impl Mount {
    #[must_use]
    pub fn source(&self) -> &Path {
        &self.source
    }
    #[must_use]
    pub const fn target(&self) -> &PosixPath {
        &self.target
    }
    #[must_use]
    pub const fn read_only(&self) -> bool {
        self.read_only
    }
    /// # Errors
    /// Refuses targets outside the two source mount locations, then relative sources.
    pub fn new(source: PathBuf, target: PosixPath, read_only: bool) -> Result<Self, ContractError> {
        let text = target.text();
        if !text.equals_utf8("/cr/code") && !text.equals_utf8("/cr/cache") {
            return Err(ContractError::MountTarget);
        }
        if !source.is_absolute() {
            return Err(ContractError::MountSource);
        }
        Ok(Self {
            source,
            target,
            read_only,
        })
    }
}

/// Typed projection of an already validated manifest's container.
#[derive(Clone, Debug)]
pub struct Manifest {
    pub image: String,
    pub command: Vec<String>,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cpu: Option<String>,
    pub memory: Option<String>,
    pub gpus: Option<String>,
    pub network: Network,
}
#[derive(Clone, Debug)]
pub enum Network {
    None,
    Egress(Vec<String>),
}
#[must_use]
pub fn egress_of(network: &Network) -> Vec<String> {
    match network {
        Network::None => Vec::new(),
        Network::Egress(hosts) => hosts.clone(),
    }
}

#[derive(Clone, Debug)]
pub struct StepSpec {
    pub job_id: String,
    pub label: String,
    pub image: String,
    pub command: Vec<String>,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub resources: Resources,
    pub egress: Vec<String>,
    pub mounts: Vec<Mount>,
    pub workdir: Option<PosixPath>,
    pub open_network: bool,
}
impl StepSpec {
    /// Match source resource conversion and last-wins environment projection.
    /// # Errors
    /// Returns the first failing quantity in CPU, memory, GPU order.
    pub fn from_manifest(
        manifest: &Manifest,
        job_id: String,
        label: String,
    ) -> Result<Self, ContractError> {
        let egress = egress_of(&manifest.network);
        let cpu = manifest
            .cpu
            .as_ref()
            .map(|text| quantity(text))
            .transpose()?;
        let memory_bytes = manifest
            .memory
            .as_ref()
            .map(|text| quantity(text))
            .transpose()?
            .map(|v| v.ceil().to_integer());
        let gpus = manifest
            .gpus
            .as_ref()
            .map(|text| quantity(text))
            .transpose()?
            .map_or_else(BigInt::zero, |v| v.ceil().to_integer());
        let mut env: Vec<(String, String)> = Vec::new();
        for (name, value) in &manifest.env {
            if let Some((_, old)) = env.iter_mut().find(|(key, _)| key == name) {
                old.clone_from(value);
            } else {
                env.push((name.clone(), value.clone()));
            }
        }
        Ok(Self {
            job_id,
            label,
            image: manifest.image.clone(),
            command: manifest.command.clone(),
            args: manifest.args.clone(),
            env,
            resources: Resources {
                cpu,
                memory_bytes,
                gpus,
            },
            egress,
            mounts: Vec::new(),
            workdir: None,
            open_network: false,
        })
    }
    #[must_use]
    pub fn networked(&self) -> bool {
        !self.egress.is_empty() || self.open_network
    }
    /// Check direct-construction mount/workdir constraints in source order.
    /// # Errors
    /// Duplicate mount targets precede invalid workdir errors.
    pub fn validate(&self) -> Result<(), ContractError> {
        for (index, mount) in self.mounts.iter().enumerate() {
            if self.mounts[..index]
                .iter()
                .any(|old| old.target == mount.target)
            {
                return Err(ContractError::DuplicateMount);
            }
        }
        if let Some(workdir) = &self.workdir {
            workdir.cr_relative()?;
        }
        Ok(())
    }
}

/// Parse an ASCII decimal resource quantity using exact rational arithmetic.
/// Inputs are bounded to 1024 bytes, including the unit suffix.
/// # Errors
/// Returns a sanitized syntax or resource input limit error.
pub fn quantity(value: &str) -> Result<BigRational, ContractError> {
    if value.len() > 1024 {
        return Err(ContractError::IntegerLimit);
    }
    let points = value.as_bytes();
    let whole = points
        .iter()
        .take_while(|&&cp| (48..=57).contains(&cp))
        .count();
    if whole == 0 {
        return Err(ContractError::Quantity);
    }
    let mut end = whole;
    let mut fraction = 0;
    if points.get(end) == Some(&46) {
        end += 1;
        fraction = points[end..]
            .iter()
            .take_while(|&&cp| (48..=57).contains(&cp))
            .count();
        if fraction == 0 {
            return Err(ContractError::Quantity);
        }
        end += fraction;
    }
    let suffix = std::str::from_utf8(&points[end..]).map_err(|_| ContractError::Quantity)?;
    let (factor, divisor): (u64, u64) = match suffix {
        "" => (1, 1),
        "m" => (1, 1000),
        "k" => (1000, 1),
        "M" => (1_000_000, 1),
        "G" => (1_000_000_000, 1),
        "T" => (1_000_000_000_000, 1),
        "P" => (1_000_000_000_000_000, 1),
        "Ki" => (1 << 10, 1),
        "Mi" => (1 << 20, 1),
        "Gi" => (1 << 30, 1),
        "Ti" => (1 << 40, 1),
        "Pi" => (1 << 50, 1),
        _ => return Err(ContractError::Quantity),
    };
    let whole_int = BigInt::parse_bytes(&points[..whole], 10).ok_or(ContractError::Quantity)?;
    let denominator =
        BigInt::from(10u8).pow(u32::try_from(fraction).map_err(|_| ContractError::IntegerLimit)?);
    let decimal = if fraction == 0 {
        BigInt::zero()
    } else {
        BigInt::parse_bytes(&points[whole + 1..end], 10).ok_or(ContractError::Quantity)?
    };
    Ok(BigRational::new(
        (whole_int * &denominator + decimal) * factor,
        denominator * divisor,
    ))
}

/// Extract digest and remove a tag in the final repository component only.
/// # Errors
/// Returns `Image` for invalid digest-pinned references.
pub fn pinned_image(value: &str) -> Result<(String, String), ContractError> {
    let (repository, digest) = value.split_once('@').ok_or(ContractError::Image)?;
    if repository.is_empty() || repository.chars().any(char::is_whitespace) {
        return Err(ContractError::Image);
    }
    if digest.len() != 71
        || !digest.starts_with("sha256:")
        || !digest.as_bytes()[7..]
            .iter()
            .all(|cp| matches!(cp,b'0'..=b'9'|b'a'..=b'f'))
    {
        return Err(ContractError::Image);
    }
    let last = repository.rfind('/').map_or(0, |index| index + 1);
    let end = repository[last..]
        .find(':')
        .map_or(repository.len(), |index| last + index);
    Ok((repository[..end].to_owned(), digest.to_owned()))
}

/// Map a lexical UTF-8 `/cr` path into the host root.
/// This is not a traversal-confinement validator: `..` remains intact.
/// # Errors
/// Returns `Path` for a different anchor/root, or `Encoding` for nonfilesystem text.
pub fn host_path(root: &Path, container_path: &str) -> Result<PathBuf, ContractError> {
    let path = PosixPath::new(container_path);
    let mut output = root.to_path_buf();
    for part in path.cr_relative()? {
        output.push(part);
    }
    Ok(output)
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
