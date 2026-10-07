//! Read service credentials without exposing their contents in diagnostics.
use cannery_core::principal::Secret;
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TokenFileError {
    #[error("credential file cannot be read")]
    Io,
    #[error("credential file is not regular")]
    NotRegular,
    #[error("credential file grants group or other permissions")]
    Permissions,
    #[error("credential file is not UTF-8")]
    Encoding,
    #[error("credential file is empty")]
    Empty,
}

/// Recognize Unicode whitespace with the standard Rust character predicate.
#[must_use]
pub fn is_whitespace(cp: u32) -> bool {
    char::from_u32(cp).is_some_and(char::is_whitespace)
}

/// Follow the source's `stat`, then separate strict UTF-8 file read.
/// Owner permissions are unrestricted; group/other permissions must be zero.
/// Symlinks are followed, the owner is not checked, and a BOM is not stripped.
///
/// # Errors
/// Returns a sanitized category without paths, contents, or native diagnostics.
pub fn read_token_file(path: &Path) -> Result<Secret, TokenFileError> {
    let metadata = fs::metadata(path).map_err(|_| TokenFileError::Io)?;
    if !metadata.is_file() {
        return Err(TokenFileError::NotRegular);
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(TokenFileError::Permissions);
    }
    let bytes = fs::read(path).map_err(|_| TokenFileError::Io)?;
    let text = String::from_utf8(bytes).map_err(|_| TokenFileError::Encoding)?;
    // Path.read_text uses universal-newline translation before strip.
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let token = text.trim_matches(|c: char| is_whitespace(u32::from(c)));
    if token.is_empty() {
        return Err(TokenFileError::Empty);
    }
    Ok(Secret::new(token.to_owned()))
}
