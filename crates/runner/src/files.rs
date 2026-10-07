//! Source tree inspection; this does not confine concurrent filesystem mutations.
use crate::paths::{self, PathError};
use num_bigint::BigInt;
use rustix::fs::{Access, AtFlags, CWD, accessat};
use std::{
    fs,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

/// Static failures retain neither source paths nor native diagnostic payloads.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TreeError {
    #[error("the runner tree is unsafe or unreadable")]
    Unsafe,
    #[error("tree filesystem operation failed")]
    Io { errno: Option<i32> },
    #[error("tree path resolution failed")]
    Path(#[from] PathError),
}
fn io_error(error: &std::io::Error) -> TreeError {
    TreeError::Io {
        errno: error.raw_os_error(),
    }
}

// os.walk classifies entries using a following is_dir check, suppressing its errors.
fn entries(directory: &Path) -> std::io::Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mut directories = Vec::new();
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if fs::metadata(&path).is_ok_and(|metadata| metadata.is_dir()) {
            directories.push(path);
        } else {
            files.push(path);
        }
    }
    Ok((directories, files))
}

/// Check source tree links and readable directories; count non-link file bytes.
/// A root symlink is followed as in os.walk; nested directory symlinks are not.
/// Relative dangling links are accepted when their resolved target stays inside
/// the root. Cycles and absolute links are refused.
/// # Errors
/// Scanning/access/refused links yield Unsafe. Direct lstat/readlink errors stay Io.
pub fn check_tree(path: &Path) -> Result<BigInt, TreeError> {
    let root = paths::resolve(path, false).map_err(|error| match error {
        PathError::InvalidNul => TreeError::Path(error),
        _ => TreeError::Unsafe,
    })?;
    let mut pending = vec![path.to_owned()];
    let mut total = BigInt::from(0);
    while let Some(directory) = pending.pop() {
        let (directories, files) = entries(&directory).map_err(|_| TreeError::Unsafe)?;
        for member in directories.iter().chain(&files) {
            let metadata = fs::symlink_metadata(member).map_err(|error| io_error(&error))?;
            if metadata.file_type().is_symlink() {
                let target = fs::read_link(member).map_err(|error| io_error(&error))?;
                if target.is_absolute() {
                    return Err(TreeError::Unsafe);
                }
                let destination = member.parent().ok_or(TreeError::Unsafe)?.join(target);
                let destination =
                    paths::resolve(&destination, false).map_err(|_| TreeError::Unsafe)?;
                if !destination.starts_with(&root) {
                    return Err(TreeError::Unsafe);
                }
            } else if metadata.is_dir() {
                // Empty flags match Python's real-ID access check, including ACLs.
                if accessat(
                    CWD,
                    member,
                    Access::READ_OK | Access::EXEC_OK,
                    AtFlags::empty(),
                )
                .is_err()
                {
                    return Err(TreeError::Unsafe);
                }
            } else {
                total += fs::symlink_metadata(member)
                    .map_err(|error| io_error(&error))?
                    .len();
            }
        }
        for child in directories.into_iter().rev() {
            if !fs::symlink_metadata(&child).is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                pending.push(child);
            }
        }
    }
    Ok(total)
}

/// Cache scan size differs from `check_tree`: file-classified links count by lstat.
/// Traversal and lstat failures are ignored; directory symlinks are never followed.
/// # Errors
/// An embedded NUL is a source value failure, not a suppressed filesystem error.
pub fn tree_size(path: &Path) -> Result<BigInt, TreeError> {
    if path.as_os_str().as_bytes().contains(&0) {
        return Err(PathError::InvalidNul.into());
    }
    let mut pending = vec![path.to_owned()];
    let mut total = BigInt::from(0);
    while let Some(directory) = pending.pop() {
        let Ok((directories, files)) = entries(&directory) else {
            continue;
        };
        for member in files {
            if let Ok(metadata) = fs::symlink_metadata(member) {
                total += metadata.len();
            }
        }
        for child in directories.into_iter().rev() {
            if !fs::symlink_metadata(&child).is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                pending.push(child);
            }
        }
    }
    Ok(total)
}
