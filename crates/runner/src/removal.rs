//! Frozen runner cleanup effects with descriptor-verified directory traversal.
use crate::paths::PathError;
use rustix::{
    fd::{AsFd, BorrowedFd, OwnedFd},
    fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags, Stat, fstat, openat, statat, unlinkat},
    io::Errno,
};
use std::{
    ffi::OsString,
    fs,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::PermissionsExt,
    },
    path::{Path, PathBuf},
    sync::Arc,
};

/// Set a non-link object's mode to exactly 0700, suppressing OS failures.
/// The source checks for a link before chmod; it does not recurse.
/// # Errors
/// Embedded NUL remains a source value error rather than a suppressed OS error.
pub fn make_writable(path: &Path) -> Result<(), PathError> {
    if path.as_os_str().as_bytes().contains(&0) {
        return Err(PathError::InvalidNul);
    }
    if !fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

#[derive(Clone)]
enum Parent {
    Cwd,
    Directory(Arc<OwnedFd>),
}
impl Parent {
    fn fd(&self) -> BorrowedFd<'_> {
        match self {
            Self::Cwd => CWD,
            Self::Directory(fd) => fd.as_ref().as_fd(),
        }
    }
}
enum Task {
    Enter {
        parent: Parent,
        name: PathBuf,
        path: PathBuf,
        inner: bool,
        cached_stat: Option<Box<Stat>>,
    },
    Remove {
        parent: Parent,
        name: PathBuf,
        path: PathBuf,
        inner: bool,
    },
    Close(Arc<OwnedFd>),
}
#[derive(Clone, Copy)]
enum Retry {
    None,
    Unlink,
    Directory,
}
fn unlock(path: &Path, retry: Retry) {
    let _ = make_writable(path.parent().unwrap_or(Path::new("")));
    let _ = make_writable(path);
    match retry {
        Retry::None => {}
        Retry::Unlink => {
            let _ = fs::remove_file(path);
        }
        Retry::Directory => {
            let _ = fs::remove_dir(path);
        }
    }
}
fn failure(error: Errno, path: &Path, inner: bool, retry: Retry) {
    if error != Errno::NOENT || !inner {
        unlock(path, retry);
    }
}
fn read_entries(fd: &OwnedFd) -> std::io::Result<Vec<(OsString, FileType)>> {
    // scandir(fd) duplicates the descriptor; reopening '.' adds an access check
    // and changes the source's permission-recovery effects on hardlinked files.
    let mut dir = Dir::new(fd.try_clone()?)?;
    let mut entries = Vec::new();
    while let Some(entry) = dir.read() {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if name != b"." && name != b".." {
            entries.push((OsString::from_vec(name.to_vec()), entry.file_type()));
        }
    }
    Ok(entries)
}
fn enter(
    parent: Parent,
    name: PathBuf,
    path: &Path,
    inner: bool,
    cached_stat: Option<Box<Stat>>,
    tasks: &mut Vec<Task>,
) {
    let original = match cached_stat.map_or_else(
        || statat(parent.fd(), &name, AtFlags::SYMLINK_NOFOLLOW),
        |stat| Ok(*stat),
    ) {
        Ok(stat) => stat,
        Err(error) => {
            failure(error, path, inner, Retry::None);
            return;
        }
    };
    let fd = match openat(
        parent.fd(),
        &name,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => Arc::new(fd),
        Err(error) => {
            failure(error, path, inner, Retry::None);
            return;
        }
    };
    let same = match fstat(fd.as_ref()) {
        Ok(stat) => original.st_dev == stat.st_dev && original.st_ino == stat.st_ino,
        Err(error) => {
            failure(error, path, inner, Retry::None);
            return;
        }
    };
    if !same {
        unlock(path, Retry::None);
        return;
    }
    tasks.push(Task::Remove {
        parent,
        name,
        path: path.to_owned(),
        inner,
    });
    tasks.push(Task::Close(fd.clone()));
    let Ok(entries) = read_entries(fd.as_ref()) else {
        unlock(path, Retry::None);
        return;
    };
    for (name, kind) in entries {
        let fullname = path.join(&name);
        let (directory, cached_stat) = if kind == FileType::Unknown {
            match statat(fd.as_ref(), &name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => {
                    let directory = FileType::from_raw_mode(stat.st_mode) == FileType::Directory;
                    (directory, directory.then_some(Box::new(stat)))
                }
                Err(Errno::NOENT) => continue,
                Err(_) => (false, None),
            }
        } else {
            (kind == FileType::Directory, None)
        };
        if directory {
            tasks.push(Task::Enter {
                parent: Parent::Directory(fd.clone()),
                name: PathBuf::from(name),
                path: fullname,
                inner: true,
                cached_stat,
            });
        } else if let Err(error) = unlinkat(fd.as_ref(), &name, AtFlags::empty()) {
            failure(error, &fullname, true, Retry::Unlink);
        }
    }
}
fn remove_pass(path: &Path) {
    let mut tasks = vec![Task::Enter {
        parent: Parent::Cwd,
        name: path.to_owned(),
        path: path.to_owned(),
        inner: false,
        cached_stat: None,
    }];
    while let Some(task) = tasks.pop() {
        match task {
            Task::Enter {
                parent,
                name,
                path,
                inner,
                cached_stat,
            } => enter(parent, name, &path, inner, cached_stat, &mut tasks),
            Task::Remove {
                parent,
                name,
                path,
                inner,
            } => {
                if let Err(error) = unlinkat(parent.fd(), &name, AtFlags::REMOVEDIR) {
                    failure(error, &path, inner, Retry::Directory);
                }
            }
            Task::Close(fd) => drop(fd),
        }
    }
}
fn unlock_walk(path: &Path) {
    let mut pending = vec![path.to_owned()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        let directories: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| fs::metadata(p).is_ok_and(|m| m.is_dir()))
            .collect();
        for member in &directories {
            let _ = make_writable(member);
        }
        for member in directories.into_iter().rev() {
            if !fs::symlink_metadata(&member).is_ok_and(|m| m.file_type().is_symlink()) {
                pending.push(member);
            }
        }
    }
}
/// Remove a directory with the source's three passes and permission recovery.
/// Top-level links/files/FIFOs can remain, exactly as in the source; this helper
/// does not report a suppressed filesystem failure as successful deletion.
/// Directory descent verifies lstat/open/fstat identities and uses directory
/// descriptors for child operations. Nested symlink targets are not traversed.
pub fn remove_tree(path: &Path) {
    for _ in 0..3 {
        if fs::symlink_metadata(path).is_err() {
            return;
        }
        remove_pass(path);
        if fs::symlink_metadata(path).is_err() {
            return;
        }
        let _ = make_writable(path);
        unlock_walk(path);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn cached_directory_identity_refuses_replacement_before_descent() {
        let base = std::env::temp_dir().join(format!(
            "cannery-removal-identity-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&base).unwrap();
        let fixture = Fixture(base);
        let tree = fixture.0.join("tree");
        let candidate = tree.join("candidate");
        fs::create_dir(&tree).unwrap();
        fs::create_dir(&candidate).unwrap();
        // Model the cached no-follow stat that Python obtains for DT_UNKNOWN.
        let original = statat(CWD, &candidate, AtFlags::SYMLINK_NOFOLLOW).unwrap();
        fs::rename(&candidate, fixture.0.join("original")).unwrap();
        fs::create_dir(&candidate).unwrap();
        let outside = fixture.0.join("outside");
        fs::write(&outside, b"owned outside inode").unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&outside, candidate.join("inside")).unwrap();
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o500)).unwrap();
        let mut tasks = Vec::new();
        enter(
            Parent::Cwd,
            candidate.clone(),
            &candidate,
            true,
            Some(Box::new(original)),
            &mut tasks,
        );
        assert!(
            candidate.join("inside").exists(),
            "replacement was traversed"
        );
        assert_eq!(
            fs::metadata(&candidate).unwrap().permissions().mode() & 0o7777,
            0o700
        );
        remove_tree(&tree);
        assert!(!tree.exists());
        assert_eq!(
            fs::metadata(&outside).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        assert_eq!(fs::read(&outside).unwrap(), b"owned outside inode");
    }
}
