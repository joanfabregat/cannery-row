use crate::{CHUNK, Error, ObjectHead, StoredObject, hex};
use futures_util::{Stream, StreamExt};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

pub struct LocalStore {
    pub bucket: String,
    root: PathBuf,
    staging: PathBuf,
}
/// Owns only the exact unpublished staging path; cancellation cannot skip unlink.
struct StagedFileCleanup(Option<PathBuf>);
impl Drop for StagedFileCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}
impl std::fmt::Debug for LocalStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LocalStore(<redacted>)")
    }
}
fn resolve(path: &Path) -> Result<PathBuf, Error> {
    enum Work {
        Component(OsString),
        Finish(PathBuf),
    }
    if path.as_os_str().as_bytes().contains(&0) {
        return Err(Error::InvalidKey);
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut pending: Vec<_> = absolute
        .components()
        .rev()
        .map(|c| Work::Component(c.as_os_str().to_owned()))
        .collect();
    let mut links: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();
    let mut resolved = PathBuf::new();
    while let Some(work) = pending.pop() {
        let Work::Component(component) = work else {
            if let Work::Finish(link) = work {
                links.insert(link, Some(resolved.clone()));
            }
            continue;
        };
        match Path::new(&component).components().next() {
            Some(Component::RootDir) => {
                resolved = PathBuf::from("/");
                continue;
            }
            Some(Component::ParentDir) => {
                resolved.pop();
                continue;
            }
            Some(Component::CurDir) | None => continue,
            _ => resolved.push(&component),
        }
        let Ok(metadata) = fs::symlink_metadata(&resolved) else {
            continue;
        };
        if !metadata.file_type().is_symlink() {
            continue;
        }
        if let Some(cached) = links.get(&resolved) {
            if let Some(target) = cached {
                resolved = target.clone();
            }
            continue;
        }
        let Ok(target) = fs::read_link(&resolved) else {
            continue;
        };
        let link = resolved.clone();
        links.insert(link.clone(), None);
        resolved.pop();
        pending.push(Work::Finish(link));
        pending.extend(
            target
                .components()
                .rev()
                .map(|c| Work::Component(c.as_os_str().to_owned())),
        );
    }
    Ok(resolved)
}
fn generation(metadata: &fs::Metadata) -> String {
    format!(
        "{}:{}",
        metadata.ino(),
        i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec())
    )
}
fn read_chunk(file: &mut File, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut count = 0;
    while count < buffer.len() {
        match file.read(&mut buffer[count..]) {
            Ok(0) => break,
            Ok(read) => count += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(count)
}
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| Error::Worker)?
}
impl LocalStore {
    /// # Errors
    /// Returns a filesystem error when root resolution fails.
    pub fn new(root: impl AsRef<Path>, bucket: &str) -> Result<Self, Error> {
        let root = resolve(root.as_ref())?.join(bucket);
        Ok(Self {
            bucket: bucket.to_owned(),
            staging: root.join(".staging"),
            root,
        })
    }
    #[must_use]
    pub const fn backend(&self) -> &'static str {
        "local"
    }
    fn path(&self, key: &str) -> Result<PathBuf, Error> {
        if key.contains('\0') {
            return Err(Error::InvalidKey);
        }
        let path = resolve(&self.root.join(key))?;
        let relative = path
            .strip_prefix(&self.root)
            .map_err(|_| Error::InvalidKey)?;
        let first = relative.components().next().ok_or(Error::InvalidKey)?;
        if first.as_os_str().to_string_lossy().starts_with('.') {
            return Err(Error::InvalidKey);
        }
        Ok(path)
    }
    /// # Errors
    /// Returns source-classified storage, key, stream, or SDK range errors.
    pub async fn write_new<S>(
        &self,
        key: &str,
        mut chunks: S,
        max_bytes: impl Into<num_bigint::BigInt>,
    ) -> Result<StoredObject, Error>
    where
        S: Stream<Item = Result<Vec<u8>, Error>> + Unpin,
    {
        let max_bytes = max_bytes.into();
        let final_path = self.path(key)?;
        let staging = self.staging.clone();
        blocking(move || {
            fs::create_dir_all(staging)?;
            Ok(())
        })
        .await?;
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(|_| Error::Worker)?;
        random[6] = (random[6] & 15) | 64;
        random[8] = (random[8] & 63) | 128;
        let staged = self
            .staging
            .join(uuid::Uuid::from_bytes(random).simple().to_string());
        let create = staged.clone();
        let (mut file, mut cleanup_guard) = blocking(move || {
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&create)?;
            // Ownership starts only after create_new succeeds; a collision is
            // someone else's path and must never be unlinked by this guard.
            let guard = StagedFileCleanup(Some(create));
            Ok((file, guard))
        })
        .await?;
        let result = async {
            let mut size = 0u64;
            let mut digest = Sha256::new();
            while let Some(chunk) = chunks.next().await {
                let chunk = chunk?;
                size = size
                    .checked_add(chunk.len() as u64)
                    .ok_or(Error::IntegerRange)?;
                if num_bigint::BigInt::from(size) > max_bytes {
                    return Err(Error::ObjectTooLarge);
                }
                digest.update(&chunk);
                file = blocking(move || {
                    file.write_all(&chunk)?;
                    Ok(file)
                })
                .await?;
            }
            let root = self.root.clone();
            let temporary = staged.clone();
            let generation = blocking(move || {
                file.flush()?;
                file.sync_all()?;
                drop(file);
                let parent = final_path.parent().ok_or(Error::InvalidKey)?;
                fs::create_dir_all(parent)?;
                match fs::hard_link(temporary, &final_path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        return Err(Error::ObjectExists);
                    }
                    Err(e) => return Err(e.into()),
                }
                let mut dir = parent;
                loop {
                    File::open(dir)?.sync_all()?;
                    if dir == root {
                        break;
                    }
                    dir = dir.parent().ok_or(Error::InvalidKey)?;
                }
                Ok(generation(&fs::metadata(final_path)?))
            })
            .await?;
            Ok(StoredObject {
                size_bytes: size,
                sha256: hex(digest.finalize()),
                generation: Some(generation),
            })
        }
        .await;
        let cleanup = blocking(move || match fs::remove_file(staged) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        })
        .await;
        cleanup?;
        cleanup_guard.0 = None;
        result
    }
    /// # Errors
    /// Returns source-classified storage, key, stream, or SDK range errors.
    pub async fn head(&self, key: &str) -> Result<Option<ObjectHead>, Error> {
        let path = self.path(key)?;
        blocking(move || match fs::metadata(path) {
            Ok(m) if m.is_file() => Ok(Some(ObjectHead {
                size_bytes: m.len(),
                generation: Some(generation(&m)),
                sha256: None,
            })),
            Ok(_) => Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        })
        .await
    }
    /// # Errors
    /// Returns source-classified storage, key, stream, or SDK range errors.
    pub async fn stat(&self, key: &str) -> Result<Option<StoredObject>, Error> {
        let path = self.path(key)?;
        blocking(move || {
            match fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => {}
                Ok(_) => return Ok(None),
                Err(error) if matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory)
                    || matches!(error.raw_os_error(), Some(code) if code == nix::libc::ELOOP || code == nix::libc::EBADF) => return Ok(None),
                Err(error) => return Err(error.into()),
            }
            let mut file = File::open(&path)?;
            let mut buffer = vec![0; CHUNK];
            let mut digest = Sha256::new();
            let mut size = 0;
            loop {
                let count = read_chunk(&mut file, &mut buffer)?;
                if count == 0 {
                    break;
                }
                size += count as u64;
                digest.update(&buffer[..count]);
            }
            drop(file);
            Ok(Some(StoredObject {
                size_bytes: size,
                sha256: hex(digest.finalize()),
                generation: Some(generation(&fs::metadata(path)?)),
            }))
        })
        .await
    }
    /// # Errors
    /// Returns source-classified storage, key, stream, or SDK range errors.
    pub async fn read(&self, key: &str) -> Result<LocalReader, Error> {
        let path = self.path(key)?;
        let file = blocking(move || Ok(File::open(path)?)).await?;
        Ok(LocalReader { file: Some(file) })
    }
    /// # Errors
    /// Returns source-classified storage, key, stream, or SDK range errors.
    pub async fn delete(&self, key: &str, expected: Option<&str>) -> Result<(), Error> {
        let path = self.path(key)?;
        let expected = expected.map(str::to_owned);
        blocking(move || {
            if let Some(expected) = expected {
                match fs::metadata(&path) {
                    Ok(m) if generation(&m) == expected => {}
                    Ok(_) => return Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                    Err(e) => return Err(e.into()),
                }
            }
            match fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            }
        })
        .await
    }
    /// # Errors
    /// Returns source-classified storage, key, stream, or SDK range errors.
    #[allow(clippy::cast_precision_loss)] // Source compares floating-point POSIX seconds.
    pub async fn sweep_staging(&self, older_than_seconds: f64) -> Result<u64, Error> {
        let path = self.staging.clone();
        blocking(move || {
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_err(|_| Error::IntegerRange)?
                .as_secs_f64();
            let cutoff = now - older_than_seconds;
            let entries = match fs::read_dir(path) {
                Ok(v) => v,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
                Err(e) => return Err(e.into()),
            };
            let mut removed = 0;
            for entry in entries {
                let entry = entry?;
                match fs::metadata(entry.path()) {
                    Ok(m)
                        if m.is_file()
                            && (m.mtime() as f64 + m.mtime_nsec() as f64 / 1e9) < cutoff =>
                    {
                        match fs::remove_file(entry.path()) {
                            Ok(()) => removed += 1,
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                            Err(e) => return Err(e.into()),
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                    _ => {}
                }
            }
            Ok(removed)
        })
        .await
    }
}
pub struct LocalReader {
    file: Option<File>,
}
impl LocalReader {
    /// # Errors
    /// Returns source-classified storage, key, stream, or SDK range errors.
    pub async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, Error> {
        let Some(mut file) = self.file.take() else {
            return Ok(None);
        };
        let (file, bytes) = blocking(move || {
            let mut bytes = vec![0; CHUNK];
            let count = read_chunk(&mut file, &mut bytes)?;
            bytes.truncate(count);
            Ok((file, bytes))
        })
        .await?;
        if bytes.is_empty() {
            Ok(None)
        } else {
            self.file = Some(file);
            Ok(Some(bytes))
        }
    }
}
