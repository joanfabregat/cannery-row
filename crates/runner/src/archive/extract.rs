//! Safe extraction of a commit-bound source archive using native tar/gzip decoders.
use crate::files;
use flate2::bufread::GzDecoder;
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use rustix::fs::{Mode, OFlags};
use std::{
    cell::Cell,
    collections::HashSet,
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    rc::Rc,
};

/// Logical file bytes and members allowed in an extracted tree.
#[derive(Clone, Debug)]
pub struct CodeLimits {
    pub max_tree_bytes: BigInt,
    pub max_files: BigInt,
}
impl Default for CodeLimits {
    fn default() -> Self {
        Self {
            max_tree_bytes: BigInt::from(2_u64 << 30),
            max_files: BigInt::from(100_000),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExtractError {
    #[error("archive is unsafe")]
    Unsafe,
    #[error("archive does not record the requested commit")]
    Commit,
    #[error("archive filesystem operation failed")]
    Io { errno: Option<i32> },
    #[error("archive filesystem value is invalid")]
    Value,
    #[error("archive filesystem text cannot be encoded")]
    Encoding,
    #[error("archive tree inspection failed")]
    Tree(files::TreeError),
}
fn io(error: &io::Error) -> ExtractError {
    ExtractError::Io {
        errno: error.raw_os_error(),
    }
}

// Metadata is read by tar before it exposes a member. Bound that phase so GNU
// long names and PAX records cannot cause unbounded allocation inside the decoder.
const MAX_MEMBER_METADATA: u64 = 1 << 20;
const MAX_EXTRA_METADATA: u64 = 8 << 20;
const MAX_GLOBAL_HEADERS: usize = 1024;
struct BoundedReader<R> {
    inner: R,
    phase: Rc<Cell<u64>>,
    remaining: u64,
}
impl<R: Read> Read for BoundedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let allowed = self
            .remaining
            .min(self.phase.get())
            .min(buffer.len() as u64);
        if allowed == 0 {
            return Err(io::Error::other("archive exceeds resource limit"));
        }
        let size = usize::try_from(allowed)
            .map_err(|_| io::Error::other("archive exceeds resource limit"))?;
        let read = self.inner.read(&mut buffer[..size])?;
        self.remaining -= read as u64;
        self.phase.set(self.phase.get() - read as u64);
        Ok(read)
    }
}
fn parts(value: &str) -> Result<Vec<&str>, ExtractError> {
    if value.starts_with('/') || value.contains(['\\', '\0']) {
        return Err(ExtractError::Unsafe);
    }
    value
        .trim_end_matches('/')
        .split('/')
        .map(|part| {
            if part.is_empty() || matches!(part, "." | "..") {
                Err(ExtractError::Unsafe)
            } else {
                Ok(part)
            }
        })
        .collect()
}
fn link_is_inside(parts: &[&str], link: &str) -> bool {
    if link.is_empty() || link.starts_with('/') || link.contains(['\\', '\0']) {
        return false;
    }
    let mut depth = parts.len() - 1;
    for component in link.split('/') {
        match component {
            "" | "." => {}
            ".." if depth == 0 => return false,
            ".." => depth -= 1,
            _ => depth += 1,
        }
    }
    true
}
/// Extract an acquired archive. Failures leave the destination for caller cleanup.
/// # Errors
/// Rejects malformed archives, unsafe trees, mismatched commits and filesystem errors.
pub fn extract_tree(
    archive: &Path,
    target: &Path,
    commit: &str,
    limits: &CodeLimits,
) -> Result<(), ExtractError> {
    if target.as_os_str().as_bytes().contains(&0) {
        return Err(ExtractError::Value);
    }
    fs::create_dir(target).map_err(|error| io(&error))?;
    if archive.as_os_str().as_bytes().contains(&0) {
        return Err(ExtractError::Value);
    }
    let input = fs::File::open(archive).map_err(|_| ExtractError::Unsafe)?;
    extract_into(input, target, commit, limits)
}
/// Extract into an existing private destination. No archive entry is unpacked automatically.
/// # Errors
/// Rejects malformed archives, unsafe trees, mismatched commits and filesystem errors.
pub fn extract_into<R: Read>(
    input: R,
    target: &Path,
    commit: &str,
    limits: &CodeLimits,
) -> Result<(), ExtractError> {
    if !fs::symlink_metadata(target).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(ExtractError::Unsafe);
    }
    let max_files = limits.max_files.to_u64().ok_or(ExtractError::Unsafe)?;
    let max_bytes = limits.max_tree_bytes.to_u64().ok_or(ExtractError::Unsafe)?;
    let decoded_limit = max_files
        .checked_add(1)
        .and_then(|n| n.checked_mul(1024))
        .and_then(|n| n.checked_add(MAX_EXTRA_METADATA))
        .and_then(|n| n.checked_add(max_bytes))
        .ok_or(ExtractError::Unsafe)?;
    let phase = Rc::new(Cell::new(MAX_MEMBER_METADATA));
    let decoder = GzDecoder::new(BufReader::new(input));
    let bounded = BoundedReader {
        inner: decoder,
        phase: phase.clone(),
        remaining: decoded_limit,
    };
    let mut archive = tar::Archive::new(bounded);
    read_members(&mut archive, target, commit, &phase, max_files, max_bytes)?;
    // Tar iteration ends at its zero marker, before gzip has checked its trailer.
    // Drain bounded zero padding to verify CRC/ISIZE and refuse hidden trailing members.
    let mut reader = archive.into_inner();
    phase.set(MAX_MEMBER_METADATA);
    let mut buffer = [0; 8192];
    loop {
        let count = reader.read(&mut buffer).map_err(|_| ExtractError::Unsafe)?;
        if count == 0 {
            break;
        }
        if buffer[..count].iter().any(|byte| *byte != 0) {
            return Err(ExtractError::Unsafe);
        }
    }
    if !reader
        .inner
        .into_inner()
        .fill_buf()
        .map_err(|_| ExtractError::Unsafe)?
        .is_empty()
    {
        return Err(ExtractError::Unsafe);
    }
    files::check_tree(target).map_err(|error| {
        if error == files::TreeError::Unsafe {
            ExtractError::Unsafe
        } else {
            ExtractError::Tree(error)
        }
    })?;
    Ok(())
}
fn read_members<R: Read>(
    archive: &mut tar::Archive<R>,
    target: &Path,
    commit: &str,
    phase: &Cell<u64>,
    max_files: u64,
    max_bytes: u64,
) -> Result<(), ExtractError> {
    let mut prefix: Option<String> = None;
    let mut seen = HashSet::new();
    let mut count = 0_u64;
    let mut total = 0_u64;
    let mut global_headers = 0;
    let mut recorded_commit = false;
    let mut entries = archive.entries().map_err(|_| ExtractError::Unsafe)?;
    loop {
        phase.set(MAX_MEMBER_METADATA);
        let Some(entry) = entries.next() else {
            break;
        };
        let mut entry = entry.map_err(|_| ExtractError::Unsafe)?;
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() {
            global_headers += 1;
            if global_headers > MAX_GLOBAL_HEADERS || entry.size() > MAX_MEMBER_METADATA {
                return Err(ExtractError::Unsafe);
            }
            recorded_commit |= global_metadata(&mut entry, commit)?;
            continue;
        }
        reject_sparse_metadata(&mut entry)?;
        let raw_path = entry.path_bytes();
        let name = std::str::from_utf8(&raw_path).map_err(|_| ExtractError::Unsafe)?;
        let member_parts = parts(name)?;
        if prefix.is_none() {
            if !recorded_commit {
                return Err(ExtractError::Commit);
            }
            if member_parts.len() != 1 || !kind.is_dir() || entry.size() != 0 {
                return Err(ExtractError::Unsafe);
            }
            prefix = Some(member_parts[0].to_owned());
            continue;
        }
        if Some(member_parts[0]) != prefix.as_deref() {
            return Err(ExtractError::Unsafe);
        }
        let member_parts = &member_parts[1..];
        if member_parts.is_empty() || !seen.insert(member_parts.join("/")) {
            return Err(ExtractError::Unsafe);
        }
        count = count.checked_add(1).ok_or(ExtractError::Unsafe)?;
        if count > max_files {
            return Err(ExtractError::Unsafe);
        }
        let mut parent = target.to_owned();
        for part in &member_parts[..member_parts.len() - 1] {
            parent.push(part);
            if !fs::symlink_metadata(&parent).is_ok_and(|metadata| metadata.is_dir()) {
                return Err(ExtractError::Unsafe);
            }
        }
        let path = parent.join(member_parts[member_parts.len() - 1]);
        if kind.is_dir() {
            if entry.size() != 0 {
                return Err(ExtractError::Unsafe);
            }
            fs::DirBuilder::new()
                .mode(0o755)
                .create(path)
                .map_err(|_| ExtractError::Unsafe)?;
        } else if kind.is_file() {
            let size = entry.size();
            total = total.checked_add(size).ok_or(ExtractError::Unsafe)?;
            if total > max_bytes {
                return Err(ExtractError::Unsafe);
            }
            phase.set(size);
            write_file(&mut entry, &path)?;
        } else if kind.is_symlink() {
            if entry.size() != 0 {
                return Err(ExtractError::Unsafe);
            }
            let link = entry.link_name_bytes().ok_or(ExtractError::Unsafe)?;
            let link = std::str::from_utf8(&link).map_err(|_| ExtractError::Unsafe)?;
            if !link_is_inside(member_parts, link) {
                return Err(ExtractError::Unsafe);
            }
            std::os::unix::fs::symlink(PathBuf::from(link), path)
                .map_err(|_| ExtractError::Unsafe)?;
        } else {
            return Err(ExtractError::Unsafe);
        }
    }
    if prefix.is_none() {
        return Err(ExtractError::Unsafe);
    }
    Ok(())
}
fn reject_sparse_metadata<R: Read>(entry: &mut tar::Entry<'_, R>) -> Result<(), ExtractError> {
    if let Some(extensions) = entry.pax_extensions().map_err(|_| ExtractError::Unsafe)? {
        for extension in extensions {
            if extension
                .map_err(|_| ExtractError::Unsafe)?
                .key()
                .map_err(|_| ExtractError::Unsafe)?
                .starts_with("GNU.sparse.")
            {
                return Err(ExtractError::Unsafe);
            }
        }
    }
    Ok(())
}
fn global_metadata<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    commit: &str,
) -> Result<bool, ExtractError> {
    let mut recorded_commit = false;
    if let Some(extensions) = entry.pax_extensions().map_err(|_| ExtractError::Unsafe)? {
        for extension in extensions {
            let extension = extension.map_err(|_| ExtractError::Unsafe)?;
            let key = extension.key().map_err(|_| ExtractError::Unsafe)?;
            if key == "comment" {
                if extension.value().map_err(|_| ExtractError::Unsafe)? != commit {
                    return Err(ExtractError::Commit);
                }
                recorded_commit = true;
            } else if matches!(key, "path" | "linkpath" | "size") || key.starts_with("GNU.sparse.")
            {
                // Global structural overrides are unsupported by the native reader.
                return Err(ExtractError::Unsafe);
            }
        }
    }
    Ok(recorded_commit)
}
fn write_file<R: Read>(entry: &mut tar::Entry<'_, R>, path: &Path) -> Result<(), ExtractError> {
    let mode = entry.header().mode().map_err(|_| ExtractError::Unsafe)?;
    let fd = rustix::fs::open(
        path,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )
    .map_err(|_| ExtractError::Unsafe)?;
    let mut file = fs::File::from(fd);
    let copied = std::io::copy(&mut *entry, &mut file).map_err(|_| ExtractError::Unsafe)?;
    if copied != entry.size() {
        return Err(ExtractError::Unsafe);
    }
    file.flush().map_err(|_| ExtractError::Unsafe)?;
    drop(file);
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if mode & 0o111 != 0 { 0o555 } else { 0o444 }),
    )
    .map_err(|_| ExtractError::Unsafe)
}
