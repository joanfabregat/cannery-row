//! Native filesystem canonicalization with an explicit missing-destination policy.
use std::{
    ffi::OsString,
    fs,
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PathError {
    #[error("path contains a NUL")]
    InvalidNul,
    #[error("path cannot be encoded by the filesystem")]
    Encoding,
    #[error("path traverses a non-directory")]
    NotDirectory,
    #[error("path filesystem operation failed")]
    Io { errno: Option<i32> },
}
fn io_error(error: &std::io::Error) -> PathError {
    if error.kind() == std::io::ErrorKind::NotADirectory {
        PathError::NotDirectory
    } else {
        PathError::Io {
            errno: error.raw_os_error(),
        }
    }
}

/// Convert UTF-8 application text to a native path.
/// # Errors
/// Rejects NUL before filesystem access.
pub fn from_text(value: &str) -> Result<PathBuf, PathError> {
    if value.as_bytes().contains(&0) {
        return Err(PathError::InvalidNul);
    }
    Ok(PathBuf::from(value))
}

/// Canonicalize existing paths using the operating system's link limits.
/// Non-strict mode allows missing destinations by canonicalizing the longest
/// existing ancestor, then normalizing its missing suffix lexically. It does
/// not hide permission, non-directory or symlink-loop errors.
/// # Errors
/// Rejects NUL and propagates native filesystem errors.
pub fn resolve(value: &Path, strict: bool) -> Result<PathBuf, PathError> {
    if value.as_os_str().as_bytes().contains(&0) {
        return Err(PathError::InvalidNul);
    }
    match fs::canonicalize(value) {
        Ok(path) => return Ok(path),
        Err(error) if !strict && error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(&error)),
    }
    let mut ancestor = if value.is_absolute() {
        value.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|error| io_error(&error))?
            .join(value)
    };
    let mut suffix: Vec<OsString> = Vec::new();
    loop {
        match fs::canonicalize(&ancestor) {
            Ok(mut path) => {
                for part in suffix.into_iter().rev() {
                    for component in Path::new(&part).components() {
                        match component {
                            Component::ParentDir => {
                                path.pop();
                            }
                            Component::Normal(name) => path.push(name),
                            Component::CurDir => {}
                            Component::RootDir | Component::Prefix(_) => {
                                return Err(PathError::Encoding);
                            }
                        }
                    }
                }
                return match fs::canonicalize(&path) {
                    Ok(resolved) => Ok(resolved),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path),
                    Err(error) => Err(io_error(&error)),
                };
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let part = ancestor
                    .components()
                    .next_back()
                    .ok_or_else(|| io_error(&error))?;
                suffix.push(part.as_os_str().to_owned());
                if !ancestor.pop() {
                    return Err(io_error(&error));
                }
            }
            Err(error) => return Err(io_error(&error)),
        }
    }
}

/// Resolve UTF-8 application text with the same native path policy.
/// # Errors
/// Rejects NUL and propagates native filesystem errors.
pub fn resolve_text(value: &str, strict: bool) -> Result<PathBuf, PathError> {
    resolve(&from_text(value)?, strict)
}
