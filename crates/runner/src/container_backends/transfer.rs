//! Bounded tar transfer with explicit destinations and no following archive links.
use super::{Limits, RuntimeError};
use crate::launcher::Mount;
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, symlink},
    path::{Component, Path, PathBuf},
};
struct Bounded<W> {
    inner: W,
    remaining: u64,
}
impl<W: Write> Write for Bounded<W> {
    fn write(&mut self, value: &[u8]) -> io::Result<usize> {
        if u64::try_from(value.len()).map_or(true, |n| n > self.remaining) {
            return Err(io::Error::other("transfer limit"));
        }
        let size = self.inner.write(value)?;
        self.remaining -= size as u64;
        Ok(size)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
pub(super) fn spool(root: &Path, name: &str) -> Result<(PathBuf, File), RuntimeError> {
    let path = root
        .parent()
        .ok_or(RuntimeError::Configuration)?
        .join(format!(".cr-{}-{name}.tar", super::nonce()?));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    Ok((path, file))
}
pub(super) fn pack(
    root: &Path,
    mounts: &[Mount],
    file: File,
    limits: &Limits,
) -> Result<(), RuntimeError> {
    let writer = Bounded {
        inner: file,
        remaining: limits.input_bytes,
    };
    let mut archive = tar::Builder::new(writer);
    archive.follow_symlinks(false);
    let mut queue = vec![
        (root.join("job.json"), PathBuf::from("job.json")),
        (root.join("inputs"), PathBuf::from("inputs")),
        (root.join("outputs"), PathBuf::from("outputs")),
    ];
    for mount in mounts {
        let name = if mount.target().text().equals_utf8("/cr/code") {
            "code"
        } else {
            "cache"
        };
        let metadata = fs::symlink_metadata(mount.source())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(RuntimeError::Integrity);
        }
        queue.push((mount.source().to_path_buf(), PathBuf::from(name)));
    }
    let mut members = 0;
    while let Some((source, target)) = queue.pop() {
        members += 1;
        if members > limits.files {
            return Err(RuntimeError::Integrity);
        }
        let metadata = fs::symlink_metadata(&source)?;
        if metadata.is_dir() {
            archive.append_dir(&target, &source)?;
            let mut children = fs::read_dir(&source)?.collect::<Result<Vec<_>, _>>()?;
            children.sort_by_key(std::fs::DirEntry::file_name);
            for child in children.into_iter().rev() {
                queue.push((child.path(), target.join(child.file_name())));
            }
        } else if metadata.is_file() || metadata.file_type().is_symlink() {
            archive.append_path_with_name(&source, &target)?;
        } else {
            return Err(RuntimeError::Integrity);
        }
    }
    archive.finish()?;
    Ok(())
}
fn safe(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.components().all(|p| matches!(p, Component::Normal(_)))
}
fn parent(root: &Path, path: &Path) -> Result<PathBuf, RuntimeError> {
    let mut current = root.to_path_buf();
    for part in path
        .parent()
        .ok_or(RuntimeError::InvalidOutput)?
        .components()
    {
        if let Component::Normal(part) = part {
            current.push(part);
        } else {
            return Err(RuntimeError::InvalidOutput);
        }
        match fs::symlink_metadata(&current) {
            Ok(value) if value.is_dir() && !value.file_type().is_symlink() => {}
            Ok(_) => return Err(RuntimeError::InvalidOutput),
            Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir(&current)?,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(root.join(path))
}
/// Extraction never calls tar's generic unpack. Only named outputs/cache are written;
/// links are preserved as links and can never become subsequent write parents.
pub(super) fn unpack(
    archive: &Path,
    root: &Path,
    mounts: &[Mount],
    limits: &Limits,
) -> Result<(), RuntimeError> {
    if fs::metadata(archive)?.len() > limits.output_bytes {
        return Err(RuntimeError::InvalidOutput);
    }
    let input = File::open(archive)?;
    let mut archive = tar::Archive::new(input);
    let mut members = 0;
    let mut written = 0u64;
    let mut regular = BTreeSet::new();
    for entry in archive.entries().map_err(|_| RuntimeError::InvalidOutput)? {
        let mut entry = entry.map_err(|_| RuntimeError::InvalidOutput)?;
        members += 1;
        if members > limits.files {
            return Err(RuntimeError::InvalidOutput);
        }
        let path = entry
            .path()
            .map_err(|_| RuntimeError::InvalidOutput)?
            .into_owned();
        if !safe(&path) {
            return Err(RuntimeError::InvalidOutput);
        }
        let mut parts = path.components();
        let first = parts.next().ok_or(RuntimeError::InvalidOutput)?;
        let remaining = parts.as_path();
        let destination = match first {
            Component::Normal(name) if name == "outputs" => root.join("outputs"),
            Component::Normal(name) if name == "cache" => mounts
                .iter()
                .find(|m| m.target().text().equals_utf8("/cr/cache") && !m.read_only())
                .ok_or(RuntimeError::InvalidOutput)?
                .source()
                .to_path_buf(),
            _ => return Err(RuntimeError::InvalidOutput),
        };
        let metadata = fs::symlink_metadata(&destination)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(RuntimeError::InvalidOutput);
        }
        if remaining.as_os_str().is_empty() {
            continue;
        }
        let target = parent(&destination, remaining)?;
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            match fs::symlink_metadata(&target) {
                Ok(v) if v.is_dir() && !v.file_type().is_symlink() => {}
                Ok(_) => return Err(RuntimeError::InvalidOutput),
                Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir(&target)?,
                Err(e) => return Err(e.into()),
            }
        } else if kind.is_file() {
            written = written
                .checked_add(entry.size())
                .ok_or(RuntimeError::InvalidOutput)?;
            if written > limits.output_bytes {
                return Err(RuntimeError::InvalidOutput);
            }
            if fs::symlink_metadata(&target)
                .is_ok_and(|v| v.file_type().is_symlink() || !v.is_file())
            {
                return Err(RuntimeError::InvalidOutput);
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&target)?;
            io::copy(&mut entry, &mut file).map_err(|_| RuntimeError::InvalidOutput)?;
            regular.insert(path);
        } else if kind.is_symlink() {
            let link = entry
                .link_name()
                .map_err(|_| RuntimeError::InvalidOutput)?
                .ok_or(RuntimeError::InvalidOutput)?;
            if fs::symlink_metadata(&target).is_ok() {
                return Err(RuntimeError::InvalidOutput);
            }
            symlink(&link, &target)?;
        } else if kind.is_hard_link() {
            let link = entry
                .link_name()
                .map_err(|_| RuntimeError::InvalidOutput)?
                .ok_or(RuntimeError::InvalidOutput)?;
            if !safe(&link) || !regular.contains(link.as_ref()) {
                return Err(RuntimeError::InvalidOutput);
            }
            let link = link
                .strip_prefix(first.as_os_str())
                .map_err(|_| RuntimeError::InvalidOutput)?;
            let source = parent(&destination, link)?;
            if !fs::symlink_metadata(&source)?.is_file() {
                return Err(RuntimeError::InvalidOutput);
            }
            fs::hard_link(source, target)?;
        }
    }
    Ok(())
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "Filesystem integrity fixture assertions"
)]
mod tests {
    use super::*;
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "cr-transfer-{}",
                super::super::nonce().expect("nonce")
            ));
            fs::create_dir(&path).expect("root");
            fs::create_dir(path.join("outputs")).expect("outputs");
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn limits() -> Limits {
        Limits {
            input_bytes: 1 << 20,
            output_bytes: 1 << 20,
            files: 10,
            log_bytes: 100,
            scheduling: std::time::Duration::from_secs(1),
            transfer_idle: std::time::Duration::from_secs(1),
            cleanup: std::time::Duration::from_secs(1),
        }
    }
    fn archive(root: &Path, entries: &[(&str, tar::EntryType, &str)]) -> PathBuf {
        let path = root.join("archive.tar");
        let mut builder = tar::Builder::new(File::create(&path).expect("archive"));
        for (name, kind, value) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o600);
            header.set_entry_type(*kind);
            let bytes = if kind.is_file() {
                value.as_bytes()
            } else {
                &[]
            };
            header.set_size(bytes.len() as u64);
            if kind.is_symlink() || kind.is_hard_link() {
                header.set_link_name(value).expect("link");
            }
            header.set_cksum();
            builder
                .append_data(&mut header, name, bytes)
                .expect("entry");
        }
        builder.finish().expect("finish");
        path
    }
    #[test]
    fn extraction_preserves_links_without_following_them() {
        let root = Directory::new();
        let path = archive(
            &root.0,
            &[
                ("outputs/file", tar::EntryType::Regular, "body"),
                ("outputs/hard", tar::EntryType::Link, "outputs/file"),
                ("outputs/link", tar::EntryType::Symlink, "file"),
            ],
        );
        unpack(&path, &root.0, &[], &limits()).expect("safe extraction");
        assert_eq!(
            fs::read(root.0.join("outputs/hard")).expect("file"),
            b"body"
        );
        assert_eq!(
            fs::read_link(root.0.join("outputs/link")).expect("link"),
            Path::new("file")
        );
    }
    #[test]
    fn extraction_rejects_link_parents_destinations_and_unknown_roots() {
        let root = Directory::new();
        let path = archive(
            &root.0,
            &[
                ("outputs/escape", tar::EntryType::Symlink, "/"),
                ("outputs/escape/forbidden", tar::EntryType::Regular, "body"),
            ],
        );
        assert_eq!(
            unpack(&path, &root.0, &[], &limits()),
            Err(RuntimeError::InvalidOutput)
        );
        assert!(!root.0.join("forbidden").exists());
        let path = archive(
            &root.0,
            &[("inputs/overwrite", tar::EntryType::Regular, "body")],
        );
        assert_eq!(
            unpack(&path, &root.0, &[], &limits()),
            Err(RuntimeError::InvalidOutput)
        );
        fs::remove_dir_all(root.0.join("outputs")).expect("owned outputs");
        symlink("/", root.0.join("outputs")).expect("fixture");
        let path = archive(
            &root.0,
            &[("outputs/forbidden", tar::EntryType::Regular, "body")],
        );
        assert_eq!(
            unpack(&path, &root.0, &[], &limits()),
            Err(RuntimeError::InvalidOutput)
        );
    }
    #[test]
    fn extraction_enforces_archive_and_member_limits() {
        let root = Directory::new();
        let path = archive(
            &root.0,
            &[
                ("outputs/one", tar::EntryType::Regular, "one"),
                ("outputs/two", tar::EntryType::Regular, "two"),
            ],
        );
        let mut cap = limits();
        cap.files = 1;
        assert_eq!(
            unpack(&path, &root.0, &[], &cap),
            Err(RuntimeError::InvalidOutput)
        );
        cap = limits();
        cap.output_bytes = 1;
        assert_eq!(
            unpack(&path, &root.0, &[], &cap),
            Err(RuntimeError::InvalidOutput)
        );
    }
}
