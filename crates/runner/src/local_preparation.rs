//! Synchronous local preparation. This deliberately adds no isolation or rollback.
use crate::launcher::{Mount, host_path};
use rustix::fs::{AtFlags, CWD, Mode, Timespec, Timestamps, XattrFlags, utimensat};
use std::{
    ffi::{CString, OsStr},
    fs::{self, File, Metadata},
    io,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopyFailure {
    Os { errno: Option<i32> },
    NamedPipe,
    SameFile,
    InvalidValue,
}
/// Paths are available for source-compatible diagnostics, but never in Debug.
pub struct CopyIssue {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub failure: CopyFailure,
}
impl std::fmt::Debug for CopyIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CopyIssue")
            .field("failure", &self.failure)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, thiserror::Error)]
pub enum PreparationError {
    #[error("local preparation filesystem operation failed")]
    Io { errno: Option<i32> },
    #[error("local preparation path is invalid")]
    Value,
    #[error("local preparation copy failed")]
    Copy(Vec<CopyIssue>),
    #[error("local preparation exceeds the directory depth limit")]
    DepthLimit,
    #[error("local preparation requires an unavailable native metadata capability")]
    Capability,
}
/// Native metadata operations supported by the caller's filesystem adapter.
#[derive(Clone, Copy, Debug)]
pub struct MetadataCapabilities {
    pub symlink_chmod: bool,
    pub symlink_chflags: bool,
}
/// Native directory traversal policy. The source root has depth zero.
/// Callers may impose a smaller limit; the maximum supported depth is 128.
#[derive(Clone, Copy, Debug)]
pub struct CopyLimits {
    pub max_depth: usize,
}
const MAX_COPY_DEPTH: usize = 128;
impl Default for CopyLimits {
    fn default() -> Self {
        Self {
            max_depth: MAX_COPY_DEPTH,
        }
    }
}
impl CopyLimits {
    fn check(self, depth: usize) -> Result<(), PreparationError> {
        if depth > self.max_depth.min(MAX_COPY_DEPTH) {
            Err(PreparationError::DepthLimit)
        } else {
            Ok(())
        }
    }
}
fn os_error(error: io::Error) -> PreparationError {
    if error.kind() == io::ErrorKind::Unsupported && error.raw_os_error().is_none() {
        return PreparationError::Capability;
    }
    let result = PreparationError::Io {
        errno: error.raw_os_error(),
    };
    drop(error);
    result
}
fn failure(error: PreparationError) -> CopyFailure {
    let result = match &error {
        PreparationError::Io { errno } => CopyFailure::Os { errno: *errno },
        PreparationError::Value => CopyFailure::InvalidValue,
        PreparationError::Copy(_) | PreparationError::DepthLimit | PreparationError::Capability => {
            CopyFailure::Os { errno: None }
        }
    };
    drop(error);
    result
}
fn native_error(error: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}
fn validate(path: &Path) -> Result<(), PreparationError> {
    if path.as_os_str().as_bytes().contains(&0) {
        Err(PreparationError::Value)
    } else {
        Ok(())
    }
}

/// Required inputs to the actual `_prepare` slice. No async policy is selected.
pub struct LocalPreparation {
    pub step_root: PathBuf,
    pub root: PathBuf,
    pub code_dir: PathBuf,
    pub mounts: Vec<Mount>,
    pub copy: bool,
    pub limits: CopyLimits,
    pub metadata: MetadataCapabilities,
}
impl std::fmt::Debug for LocalPreparation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LocalPreparation([redacted])")
    }
}
impl LocalPreparation {
    /// # Errors
    /// Preserves mkdir, copy and mount failure order and already completed effects.
    pub fn prepare(&self) -> Result<(), PreparationError> {
        let temporary = self.root.join("tmp");
        validate(&temporary)?;
        if let Err(error) = fs::create_dir(&temporary)
            && !temporary.is_dir()
        {
            return Err(os_error(error));
        }
        if self.copy {
            copy_tree(
                &self.step_root,
                &self.code_dir,
                false,
                true,
                self.limits,
                self.metadata,
            )?;
        }
        for mount in &self.mounts {
            let target = host_path(&self.root, &mount.target().text())
                .map_err(|_| PreparationError::Value)?;
            if mount.read_only() {
                copy_tree(
                    mount.source(),
                    &target,
                    true,
                    false,
                    self.limits,
                    self.metadata,
                )?;
            } else {
                validate(mount.source())?;
                validate(&target)?;
                symlink(mount.source(), &target).map_err(os_error)?;
            }
        }
        Ok(())
    }
}
fn ignored(name: &OsStr) -> bool {
    let bytes = name.as_bytes();
    bytes == b"__pycache__" || bytes.ends_with(b".pyc")
}
/// Source copytree defaults: scan first, create destination, process scan order,
/// copy directory metadata last, then report the ordered aggregate.
/// # Errors
/// Directory scan/creation errors are direct; entry/metadata errors aggregate.
struct Frame {
    source: PathBuf,
    destination: PathBuf,
    entries: std::vec::IntoIter<fs::DirEntry>,
    errors: Vec<CopyIssue>,
    depth: usize,
    cached_metadata: Option<fs::Metadata>,
}
fn enter_tree(
    source: &Path,
    destination: &Path,
    depth: usize,
    limits: CopyLimits,
) -> Result<Frame, PreparationError> {
    limits.check(depth)?;
    validate(source)?;
    let entries: Vec<_> = fs::read_dir(source)
        .map_err(os_error)?
        .collect::<Result<_, _>>()
        .map_err(os_error)?;
    validate(destination)?;
    if destination.exists() {
        return Err(PreparationError::Io {
            errno: Some(rustix::io::Errno::EXIST.raw_os_error()),
        });
    }
    if let Some(parent) = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(os_error)?;
    }
    // Do not let create_dir_all silently accept a destination created between
    // the existence check and mkdir. Source's final mkdir refuses it atomically.
    fs::create_dir(destination).map_err(os_error)?;
    Ok(Frame {
        source: source.to_owned(),
        destination: destination.to_owned(),
        entries: entries.into_iter(),
        errors: Vec::new(),
        depth,
        cached_metadata: None,
    })
}
/// Copy contents and metadata in source traversal order, preserving partial effects.
/// # Errors
/// Reports direct scan/create errors, ordered aggregate errors, or an explicit
/// native depth-limit failure before opening or creating an over-depth directory.
pub fn copy_tree(
    source: &Path,
    destination: &Path,
    preserve_links: bool,
    ignore_bytecode: bool,
    limits: CopyLimits,
    capabilities: MetadataCapabilities,
) -> Result<(), PreparationError> {
    let mut stack = vec![enter_tree(source, destination, 0, limits)?];
    while let Some(mut frame) = stack.pop() {
        if let Some(entry) = frame.entries.next() {
            if ignore_bytecode && ignored(&entry.file_name()) {
                stack.push(frame);
                continue;
            }
            let src = entry.path();
            let dst = frame.destination.join(entry.file_name());
            let result = (|| {
                let kind = entry.file_type().map_err(os_error)?;
                if kind.is_symlink() && preserve_links {
                    let target = fs::read_link(&src).map_err(os_error)?;
                    symlink(target, &dst).map_err(os_error)?;
                    let metadata = fs::symlink_metadata(&src).map_err(os_error)?;
                    copy_stat(&src, &dst, &metadata, false, capabilities).map_err(os_error)?;
                    Ok(None)
                } else if src.is_dir() {
                    enter_child(&src, &dst, frame.depth, kind.is_symlink(), limits).map(Some)
                } else {
                    copy_file(&src, &dst, capabilities)?;
                    Ok(None)
                }
            })();
            match result {
                Ok(Some(child)) => {
                    stack.push(frame);
                    stack.push(child);
                }
                Ok(None) => stack.push(frame),
                Err(error @ (PreparationError::DepthLimit | PreparationError::Capability)) => {
                    return Err(error);
                }
                Err(PreparationError::Copy(mut errors)) => {
                    frame.errors.append(&mut errors);
                    stack.push(frame);
                }
                Err(error) => {
                    frame.errors.push(CopyIssue {
                        source: src,
                        destination: dst,
                        failure: failure(error),
                    });
                    stack.push(frame);
                }
            }
        } else {
            if let Err(error) = frame
                .cached_metadata
                .map_or_else(|| fs::metadata(&frame.source), Ok)
                .and_then(|metadata| {
                    copy_stat(
                        &frame.source,
                        &frame.destination,
                        &metadata,
                        true,
                        capabilities,
                    )
                })
            {
                let error = os_error(error);
                if matches!(error, PreparationError::Capability) {
                    return Err(error);
                }
                frame.errors.push(CopyIssue {
                    source: frame.source,
                    destination: frame.destination,
                    failure: failure(error),
                });
            }
            if let Some(parent) = stack.last_mut() {
                parent.errors.append(&mut frame.errors);
            } else if !frame.errors.is_empty() {
                return Err(PreparationError::Copy(frame.errors));
            }
        }
    }
    Ok(())
}
fn enter_child(
    source: &Path,
    destination: &Path,
    parent_depth: usize,
    link: bool,
    limits: CopyLimits,
) -> Result<Frame, PreparationError> {
    let depth = parent_depth
        .checked_add(1)
        .ok_or(PreparationError::DepthLimit)?;
    limits.check(depth)?;
    // DirEntry.is_dir caches followed-link stat before recursive scandir;
    // ordinary directories first obtain their stat at the final phase.
    let cached = if link {
        Some(fs::metadata(source).map_err(os_error)?)
    } else {
        None
    };
    let mut frame = enter_tree(source, destination, depth, limits)?;
    frame.cached_metadata = cached;
    Ok(frame)
}
fn copy_file(
    source: &Path,
    destination: &Path,
    capabilities: MetadataCapabilities,
) -> Result<(), PreparationError> {
    let stat = fs::metadata(source).ok();
    let dst_stat = fs::metadata(destination).ok();
    if stat
        .as_ref()
        .zip(dst_stat.as_ref())
        .is_some_and(|(a, b)| a.dev() == b.dev() && a.ino() == b.ino())
    {
        return Err(PreparationError::Copy(vec![CopyIssue {
            source: source.to_owned(),
            destination: destination.to_owned(),
            failure: CopyFailure::SameFile,
        }]));
    }
    if stat
        .as_ref()
        .is_some_and(|s| s.mode() & 0o170_000 == 0o010_000)
        || dst_stat
            .as_ref()
            .is_some_and(|s| s.mode() & 0o170_000 == 0o010_000)
    {
        return Err(PreparationError::Copy(vec![CopyIssue {
            source: source.to_owned(),
            destination: destination.to_owned(),
            failure: CopyFailure::NamedPipe,
        }]));
    }
    let mut input = File::open(source).map_err(os_error)?;
    let mut output = File::create(destination).map_err(os_error)?;
    io::copy(&mut input, &mut output).map_err(os_error)?;
    drop(output);
    drop(input);
    let metadata = stat
        .map_or_else(|| fs::metadata(source), Ok)
        .map_err(os_error)?;
    copy_stat(source, destination, &metadata, true, capabilities).map_err(os_error)
}
fn copy_stat(
    source: &Path,
    destination: &Path,
    metadata: &Metadata,
    follow: bool,
    capabilities: MetadataCapabilities,
) -> io::Result<()> {
    let times = Timestamps {
        last_access: Timespec {
            tv_sec: metadata.atime(),
            tv_nsec: metadata.atime_nsec(),
        },
        last_modification: Timespec {
            tv_sec: metadata.mtime(),
            tv_nsec: metadata.mtime_nsec(),
        },
    };
    utimensat(
        CWD,
        destination,
        &times,
        if follow {
            AtFlags::empty()
        } else {
            AtFlags::SYMLINK_NOFOLLOW
        },
    )
    .map_err(native_error)?;
    copy_xattrs(source, destination, follow)?;
    if follow {
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(metadata.mode() & 0o7777),
        )?;
    } else if capabilities.symlink_chmod {
        // MetadataExt uses u32 on every Unix target, but rustix's mode_t is
        // u16 on macOS. The mask preserves all permission and special bits.
        #[allow(clippy::useless_conversion)] // The conversion is required on macOS.
        let mode = (metadata.mode() & 0o7777)
            .try_into()
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        match rustix::fs::chmodat(
            CWD,
            destination,
            Mode::from_bits_retain(mode),
            AtFlags::SYMLINK_NOFOLLOW,
        ) {
            // Python suppresses unsupported nofollow chmod implementations.
            Err(rustix::io::Errno::OPNOTSUPP) => {}
            result => result.map_err(native_error)?,
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt as _;
        // The native adapter declares whether nofollow chflags is required.
        // The locked safe API has chflags, not lchflags/fchflags.
        if !follow && capabilities.symlink_chflags {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        let flags = nix::sys::stat::FileFlag::from_bits_retain(metadata.st_flags());
        if follow
            && let Err(error) = nix::unistd::chflags(destination, flags)
            && error != nix::errno::Errno::EOPNOTSUPP
        {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
    }
    Ok(())
}
// Python suppresses exactly these xattr errno classes, not all failures.
fn xattr_ignored(error: rustix::io::Errno, setting: bool) -> bool {
    matches!(
        error,
        rustix::io::Errno::NOTSUP | rustix::io::Errno::NODATA | rustix::io::Errno::INVAL
    ) || (setting && matches!(error, rustix::io::Errno::PERM | rustix::io::Errno::ACCESS))
}
fn growing(
    mut action: impl FnMut(&mut [u8]) -> rustix::io::Result<usize>,
) -> rustix::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        match action(&mut bytes) {
            Ok(size) => {
                if bytes.is_empty() && size != 0 {
                    bytes.resize(size, 0);
                } else {
                    bytes.truncate(size);
                    return Ok(bytes);
                }
            }
            Err(rustix::io::Errno::RANGE) => {
                let size = bytes
                    .len()
                    .max(1)
                    .checked_mul(2)
                    .ok_or(rustix::io::Errno::NOMEM)?;
                bytes.resize(size, 0);
            }
            Err(error) => return Err(error),
        }
    }
}
fn copy_xattrs(source: &Path, destination: &Path, follow: bool) -> io::Result<()> {
    let names = match growing(|buffer| {
        if follow {
            rustix::fs::listxattr(source, buffer)
        } else {
            rustix::fs::llistxattr(source, buffer)
        }
    }) {
        Ok(names) => names,
        Err(error) if xattr_ignored(error, false) => return Ok(()),
        Err(error) => return Err(native_error(error)),
    };
    for name in names
        .split(|&byte| byte == 0)
        .filter(|name| !name.is_empty())
    {
        let name = CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let result = (|| {
            let value = growing(|buffer| {
                if follow {
                    rustix::fs::getxattr(source, &name, buffer)
                } else {
                    rustix::fs::lgetxattr(source, &name, buffer)
                }
            })?;
            if follow {
                rustix::fs::setxattr(destination, &name, &value, XattrFlags::empty())
            } else {
                rustix::fs::lsetxattr(destination, &name, &value, XattrFlags::empty())
            }
        })();
        if let Err(error) = result
            && !xattr_ignored(error, true)
        {
            return Err(native_error(error));
        }
    }
    Ok(())
}

/// An application MUST supply its preparation execution/cancellation policy.
/// This module supplies no detached-thread or settlement default.
pub trait PreparationExecutor: Send + Sync {
    fn execute(
        &self,
        preparation: LocalPreparation,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), PreparationError>> + Send + '_>,
    >;
}
#[derive(Clone)]
pub struct LocalLauncher {
    backend: crate::local_process::LocalProcessBackend,
    step_root: PathBuf,
}
impl LocalLauncher {
    #[must_use]
    pub fn new(backend: crate::local_process::LocalProcessBackend, step_root: PathBuf) -> Self {
        Self { backend, step_root }
    }
    #[must_use]
    pub fn prepare(&self, step: crate::launcher::StepSpec, job_dir: PathBuf) -> LocalStep {
        LocalStep {
            launcher: self.clone(),
            step,
            job_dir,
        }
    }
    pub fn start(&self) -> std::future::Ready<()> {
        std::future::ready(())
    }
    pub fn release_job(&self, _job_id: &String) -> std::future::Ready<()> {
        std::future::ready(())
    }
    pub fn aclose(&self) -> std::future::Ready<()> {
        std::future::ready(())
    }
}
pub struct LocalStep {
    launcher: LocalLauncher,
    step: crate::launcher::StepSpec,
    job_dir: PathBuf,
}
/// Paths after a caller-selected preparation adapter has completed.
pub struct ReadyLocalStep {
    backend: crate::local_process::LocalProcessBackend,
    step: crate::launcher::StepSpec,
    root: PathBuf,
    code_dir: PathBuf,
}
impl LocalStep {
    /// # Errors
    /// Reports filesystem preparation failures before starting or opening logs.
    pub async fn prepare_with(
        &self,
        executor: &dyn PreparationExecutor,
        limits: CopyLimits,
        metadata: MetadataCapabilities,
    ) -> Result<ReadyLocalStep, PreparationError> {
        let code_dir = sibling_code_dir(&self.job_dir)?;
        executor
            .execute(LocalPreparation {
                step_root: self.launcher.step_root.clone(),
                root: self.job_dir.clone(),
                code_dir: code_dir.clone(),
                mounts: self.step.mounts.clone(),
                copy: self.step.workdir.is_none(),
                limits,
                metadata,
            })
            .await?;
        let code_dir = match &self.step.workdir {
            Some(path) => {
                host_path(&self.job_dir, &path.text()).map_err(|_| PreparationError::Value)?
            }
            None => code_dir,
        };
        Ok(ReadyLocalStep {
            backend: self.launcher.backend.clone(),
            step: self.step.clone(),
            root: self.job_dir.clone(),
            code_dir,
        })
    }
    pub fn cleanup(&self) -> std::future::Ready<()> {
        std::future::ready(())
    }
}
impl ReadyLocalStep {
    /// Supervision retains owned process settlement independently of this wrapper.
    #[must_use]
    pub fn run(
        &self,
        log_path: PathBuf,
        deadline: crate::local_process::Deadline,
        cancel: crate::cancellation::CancellationEvent,
    ) -> crate::local_process::RunHandle {
        self.backend.run_prepared(
            crate::local_process::PreparedRequest {
                command: self.step.command.clone(),
                args: self.step.args.clone(),
                env: self.step.env.clone(),
                root: self.root.clone(),
                code_dir: self.code_dir.clone(),
                log_path,
                deadline_seconds: deadline,
            },
            cancel,
        )
    }
    #[must_use]
    pub fn code_dir(&self) -> &Path {
        &self.code_dir
    }
}
fn sibling_code_dir(path: &Path) -> Result<PathBuf, PreparationError> {
    use std::os::unix::ffi::OsStringExt;
    let bytes = path.as_os_str().as_bytes();
    let slashes = bytes.iter().take_while(|&&byte| byte == b'/').count();
    let anchor = if slashes == 2 {
        2
    } else {
        usize::from(slashes > 0)
    };
    let mut parts: Vec<_> = bytes
        .split(|&byte| byte == b'/')
        .filter(|part| !part.is_empty() && *part != b".")
        .collect();
    let name = parts.pop().ok_or(PreparationError::Value)?;
    let mut result = vec![b'/'; anchor];
    for part in parts {
        if result.len() > anchor {
            result.push(b'/');
        }
        result.extend_from_slice(part);
    }
    if result.len() > anchor {
        result.push(b'/');
    }
    result.extend_from_slice(name);
    result.extend_from_slice(b".code");
    Ok(PathBuf::from(std::ffi::OsString::from_vec(result)))
}
