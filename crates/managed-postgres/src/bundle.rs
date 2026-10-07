//! Unpacking an embedded PostgreSQL distribution into the user's cache.
//!
//! The payload is a zstd-compressed tar archive built by
//! `dev/build-postgres-bundle.sh`. It holds only regular files and
//! directories under `bin/`, `lib/` and `share/`, plus a `VERSION` file. It is
//! checked against the SHA-256 digest recorded at build time, unpacked into a
//! temporary sibling directory and renamed into place, so a reader never sees
//! a partial tree.
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
};

/// Upper bound on the unpacked size, far above the real ~20 MB.
const MAX_UNPACKED_BYTES: u64 = 512 << 20;
const MAX_ENTRIES: usize = 10_000;

#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("embedded PostgreSQL bundle does not match its recorded digest")]
    Digest,
    #[error("embedded PostgreSQL bundle is malformed: {0}")]
    Malformed(&'static str),
    #[error("cannot unpack PostgreSQL bundle into {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> BundleError + '_ {
    move |source| BundleError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Lowercase hexadecimal SHA-256 of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

/// Returns the installation directory for `payload` under `cache_root`,
/// unpacking it first if no complete copy exists. The directory is named
/// `<version>-<first 16 hex digits of the digest>`.
///
/// # Errors
/// Refuses a payload whose digest differs from `expected_sha256`, a malformed
/// or unsafe archive, and filesystem failures.
pub fn install(
    payload: &[u8],
    expected_sha256: &str,
    cache_root: &Path,
) -> Result<PathBuf, BundleError> {
    let expected = expected_sha256.trim().to_ascii_lowercase();
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(BundleError::Digest);
    }
    let suffix = format!("-{}", &expected[..16]);
    if let Some(existing) = find_installed(cache_root, &suffix)? {
        return Ok(existing);
    }
    if sha256_hex(payload) != expected {
        return Err(BundleError::Digest);
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(cache_root)
        .map_err(io_error(cache_root))?;
    let staging = cache_root.join(format!(".unpack{suffix}-{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(io_error(&staging))?;
    }
    let result = unpack(payload, &staging).and_then(|()| {
        let version = fs::read_to_string(staging.join("VERSION")).map_err(io_error(&staging))?;
        let version = version.trim();
        if version.is_empty()
            || !version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.')
        {
            return Err(BundleError::Malformed("invalid VERSION"));
        }
        let target = cache_root.join(format!("{version}{suffix}"));
        match fs::rename(&staging, &target) {
            Ok(()) => Ok(target),
            // Another process installed the same bundle first.
            Err(_) if target.join("bin").is_dir() => Ok(target),
            Err(source) => Err(BundleError::Io {
                path: target,
                source,
            }),
        }
    });
    if staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn find_installed(cache_root: &Path, suffix: &str) -> Result<Option<PathBuf>, BundleError> {
    let entries = match fs::read_dir(cache_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(cache_root)(error)),
    };
    for entry in entries {
        let entry = entry.map_err(io_error(cache_root))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with('.') && name.ends_with(suffix) && entry.path().join("bin").is_dir() {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

/// Unpacks `payload` into `destination`, which must not exist.
///
/// # Errors
/// Refuses entries that are not regular files or directories, absolute
/// paths, `..` components, duplicate files and oversized archives.
pub fn unpack(payload: &[u8], destination: &Path) -> Result<(), BundleError> {
    let decoder = ruzstd::decoding::StreamingDecoder::new(payload)
        .map_err(|_| BundleError::Malformed("not a zstd stream"))?;
    unpack_tar(decoder, destination)
}

pub(crate) fn unpack_tar(reader: impl Read, destination: &Path) -> Result<(), BundleError> {
    fs::DirBuilder::new()
        .mode(0o700)
        .create(destination)
        .map_err(io_error(destination))?;
    let mut archive = tar::Archive::new(reader);
    let mut total: u64 = 0;
    let entries = archive
        .entries()
        .map_err(|_| BundleError::Malformed("unreadable archive"))?;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_ENTRIES {
            return Err(BundleError::Malformed("too many entries"));
        }
        let mut entry = entry.map_err(|_| BundleError::Malformed("unreadable entry"))?;
        let relative = safe_relative_path(
            &entry
                .path()
                .map_err(|_| BundleError::Malformed("unreadable entry name"))?,
        )?;
        let target = destination.join(&relative);
        let kind = entry.header().entry_type();
        let mode = entry.header().mode().unwrap_or(0o644);
        if kind.is_dir() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o755)
                .create(&target)
                .map_err(io_error(&target))?;
            continue;
        }
        if !kind.is_file() {
            return Err(BundleError::Malformed("entry is not a file or directory"));
        }
        total = total.saturating_add(entry.size());
        if total > MAX_UNPACKED_BYTES {
            return Err(BundleError::Malformed("archive is too large"));
        }
        if let Some(parent) = target.parent() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o755)
                .create(parent)
                .map_err(io_error(parent))?;
        }
        // create_new refuses to follow or replace anything already there.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(if mode & 0o111 == 0 { 0o644 } else { 0o755 })
            .open(&target)
            .map_err(io_error(&target))?;
        io::copy(&mut (&mut entry).take(MAX_UNPACKED_BYTES), &mut file)
            .map_err(io_error(&target))?;
        file.sync_all().map_err(io_error(&target))?;
    }
    Ok(())
}

/// Accepts only normal relative components (a leading `./` is dropped).
pub(crate) fn safe_relative_path(path: &Path) -> Result<PathBuf, BundleError> {
    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => {
                return Err(BundleError::Malformed("absolute entry path"));
            }
            Component::ParentDir => return Err(BundleError::Malformed("entry path contains ..")),
        }
    }
    if relative.as_os_str().is_empty() {
        return Err(BundleError::Malformed("empty entry path"));
    }
    Ok(relative)
}

#[cfg(test)]
mod tests {
    use super::{BundleError, safe_relative_path, sha256_hex, unpack_tar};
    use std::path::{Path, PathBuf};

    fn scratch(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("cannery-bundle-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    fn header(path: &[u8], kind: tar::EntryType, size: u64) -> tar::Header {
        let mut header = tar::Header::new_gnu();
        // Raw name bytes: the builder's own setters refuse unsafe names.
        header.as_old_mut().name[..path.len()].copy_from_slice(path);
        header.set_entry_type(kind);
        header.set_size(size);
        header.set_mode(0o755);
        header.set_cksum();
        header
    }

    fn archive(entries: &[(&[u8], tar::EntryType, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, kind, data) in entries {
            let header = header(path, *kind, data.len() as u64);
            builder.append(&header, *data).ok();
        }
        builder.into_inner().unwrap_or_default()
    }

    #[test]
    fn relative_paths_are_normalized_and_unsafe_ones_refused() {
        assert_eq!(
            safe_relative_path(Path::new("./bin/postgres")).ok(),
            Some(PathBuf::from("bin/postgres"))
        );
        for unsafe_path in ["/etc/passwd", "bin/../../x", "..", "."] {
            assert!(
                matches!(
                    safe_relative_path(Path::new(unsafe_path)),
                    Err(BundleError::Malformed(_))
                ),
                "{unsafe_path}"
            );
        }
    }

    #[test]
    fn regular_files_and_directories_unpack_with_modes() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let destination = scratch("ok");
        let bytes = archive(&[
            (b"bin/", tar::EntryType::Directory, b""),
            (b"bin/postgres", tar::EntryType::Regular, b"#!binary"),
            (b"VERSION", tar::EntryType::Regular, b"17.11\n"),
        ]);
        unpack_tar(bytes.as_slice(), &destination)?;
        let mode = std::fs::metadata(destination.join("bin/postgres"))?
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
        assert_eq!(
            std::fs::read_to_string(destination.join("VERSION"))?,
            "17.11\n"
        );
        std::fs::remove_dir_all(&destination)?;
        Ok(())
    }

    #[test]
    fn links_traversal_and_duplicates_are_refused() -> Result<(), Box<dyn std::error::Error>> {
        let cases: [(&str, Vec<u8>); 4] = [
            (
                "symlink",
                archive(&[(b"lib/libpq.so", tar::EntryType::Symlink, b"")]),
            ),
            (
                "hardlink",
                archive(&[(b"lib/libpq.so", tar::EntryType::Link, b"")]),
            ),
            (
                "traversal",
                archive(&[(b"../escape", tar::EntryType::Regular, b"x")]),
            ),
            (
                "duplicate",
                archive(&[
                    (b"VERSION", tar::EntryType::Regular, b"1"),
                    (b"VERSION", tar::EntryType::Regular, b"2"),
                ]),
            ),
        ];
        for (name, bytes) in cases {
            let destination = scratch(name);
            assert!(
                unpack_tar(bytes.as_slice(), &destination).is_err(),
                "{name}"
            );
            assert!(
                !destination
                    .parent()
                    .unwrap_or(&destination)
                    .join("escape")
                    .exists()
            );
            std::fs::remove_dir_all(&destination)?;
        }
        Ok(())
    }

    #[test]
    fn install_unpacks_once_into_a_versioned_directory() -> Result<(), Box<dyn std::error::Error>> {
        let cache = scratch("install");
        let tar = archive(&[
            (b"VERSION", tar::EntryType::Regular, b"17.11\n"),
            (b"bin/postgres", tar::EntryType::Regular, b"binary"),
        ]);
        let payload = ruzstd::encoding::compress_to_vec(
            tar.as_slice(),
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        let digest = sha256_hex(&payload);
        let installed = super::install(&payload, &digest, &cache)?;
        assert_eq!(installed, cache.join(format!("17.11-{}", &digest[..16])));
        assert_eq!(std::fs::read(installed.join("bin/postgres"))?, b"binary");
        // A second call finds the complete copy without unpacking again.
        std::fs::write(installed.join("marker"), b"")?;
        assert_eq!(super::install(&payload, &digest, &cache)?, installed);
        assert!(installed.join("marker").exists());
        let leftovers = std::fs::read_dir(&cache)?.count();
        assert_eq!(leftovers, 1);
        std::fs::remove_dir_all(&cache)?;
        Ok(())
    }

    #[test]
    fn install_checks_the_digest_before_unpacking() {
        let cache = scratch("digest");
        let wrong = "0".repeat(64);
        assert!(matches!(
            super::install(b"not a bundle", &wrong, &cache),
            Err(BundleError::Digest)
        ));
        assert!(matches!(
            super::install(b"x", "short", &cache),
            Err(BundleError::Digest)
        ));
        assert!(!cache.exists());
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
