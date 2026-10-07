//! Source cache lifecycle. Filesystem mutation is not confined against concurrent writers.
use crate::{
    files::{TreeError, check_tree, tree_size},
    removal::remove_tree,
};

use num_bigint::BigInt;
use rustix::fs::{AtFlags, CWD, FlockOperation, Timestamps, UTIME_NOW, flock, utimensat};
use std::os::unix::fs::DirBuilderExt;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

pub const DEFAULT_MAX_BYTES: u64 = 20 << 30;
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CacheError {
    #[error("cache size cap must be positive")]
    InvalidCap,
    #[error("cache repository is invalid")]
    Repository,
    #[error("cache commit is invalid")]
    Commit,
    #[error("cache key is invalid")]
    Key,
    #[error("cache filesystem path value is invalid")]
    PathValue,
    #[error("another runner holds the cache root")]
    Busy,
    #[error("cache filesystem operation failed")]
    Io { errno: Option<i32> },
    #[error("cache tree was refused")]
    Tree(#[from] TreeError),
    #[error("cache state lock failed")]
    State,
    #[error("cache entropy failed")]
    Entropy,
}
fn io(error: &std::io::Error) -> CacheError {
    CacheError::Io {
        errno: error.raw_os_error(),
    }
}
fn errno(error: rustix::io::Errno) -> CacheError {
    CacheError::Io {
        errno: Some(error.raw_os_error()),
    }
}
/// Logging is explicit; no default stderr output or path/credential diagnostics.
pub trait CacheLog: Send + Sync {
    /// Called under the state mutex; a frontend must not reenter this cache.
    /// # Errors
    /// Source stderr failures propagate after filesystem/index effects.
    fn event(&self, event: CacheEvent) -> Result<(), CacheError>;
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CacheEvent {
    Evicted(usize),
    OverCap { bytes: BigInt, cap: BigInt },
}
#[derive(Clone)]
struct Entry {
    size: BigInt,
    used: f64,
    order: BigInt,
}
#[derive(Default)]
struct State {
    entries: BTreeMap<Key, Entry>,
    held: BTreeMap<Key, BigInt>,
    next: BigInt,
    lock: Option<File>,
}
// Rust Path equality merges '/' and '//'; Python PurePosixPath does not.
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
struct Key(Vec<u8>);
impl Key {
    fn new(path: &Path) -> Self {
        Self(lexical(path).into_os_string().into_vec())
    }
    fn path(&self) -> PathBuf {
        PathBuf::from(std::ffi::OsString::from_vec(self.0.clone()))
    }
}
impl State {
    fn insert(&mut self, path: PathBuf, size: BigInt, used: f64) {
        let path = Key(path.into_os_string().into_vec());
        let order = self.entries.get(&path).map_or_else(
            || {
                let order = self.next.clone();
                self.next += 1;
                order
            },
            |entry| entry.order.clone(),
        );
        self.entries.insert(path, Entry { size, used, order });
    }
    fn total(&self) -> BigInt {
        self.entries.values().map(|entry| &entry.size).sum()
    }
    fn hold(&mut self, path: &Path) {
        *self.held.entry(Key::new(path)).or_default() += 1;
    }
    fn unhold(&mut self, path: &Path) {
        let key = Key::new(path);
        let count = self.held.entry(key.clone()).or_default();
        *count -= 1;
        if *count <= BigInt::from(0) {
            self.held.remove(&key);
        }
    }
}
/// Observational state, in source dictionary insertion order. Paths may contain native bytes.
#[derive(Clone)]
pub struct CacheEntry {
    pub path: PathBuf,
    pub size: BigInt,
    pub used: f64,
}
pub struct CacheRoot {
    root: PathBuf,
    max_bytes: BigInt,
    state: Mutex<State>,
    log: Arc<dyn CacheLog>,
}
impl std::fmt::Debug for CacheRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CacheRoot([redacted])")
    }
}
fn lexical(path: &Path) -> PathBuf {
    let bytes = path.as_os_str().as_bytes();
    let count = bytes.iter().take_while(|&&b| b == b'/').count();
    let mut result = vec![
        b'/';
        if count == 2 {
            2
        } else {
            usize::from(count > 0)
        }
    ];
    for part in bytes
        .split(|&b| b == b'/')
        .filter(|part| !part.is_empty() && *part != b".")
    {
        if !result.is_empty() && !result.ends_with(b"/") {
            result.push(b'/');
        }
        result.extend_from_slice(part);
    }
    if result.is_empty() {
        result.push(b'.');
    }
    PathBuf::from(std::ffi::OsString::from_vec(result))
}
// tempfile.mkdtemp returns abspath since Python 3.12, without resolving links.
fn abspath(path: &Path) -> Result<PathBuf, CacheError> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|error| io(&error))?
            .join(path)
    };
    let bytes = absolute.as_os_str().as_bytes();
    let leading = bytes.iter().take_while(|&&byte| byte == b'/').count();
    let mut parts: Vec<&[u8]> = Vec::new();
    for part in bytes.split(|&byte| byte == b'/') {
        match part {
            b"" | b"." => {}
            b".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    let mut result = vec![b'/'; if leading == 2 { 2 } else { 1 }];
    for part in parts {
        if !result.ends_with(b"/") {
            result.push(b'/');
        }
        result.extend_from_slice(part);
    }
    Ok(PathBuf::from(std::ffi::OsString::from_vec(result)))
}
fn regex_body(value: &str) -> Vec<u32> {
    value
        .strip_suffix('\n')
        .unwrap_or(value)
        .chars()
        .map(u32::from)
        .collect()
}
fn hex_name(value: &str, length: usize) -> bool {
    let body = regex_body(value);
    body.len() == length
        && body
            .iter()
            .all(|&c| (48..=57).contains(&c) || (97..=102).contains(&c))
}
fn name_text(value: &String) -> Result<String, CacheError> {
    value.as_utf8().ok_or(CacheError::Repository)
}
fn native_hex_name(path: &Path, length: usize) -> bool {
    path.file_name().is_some_and(|name| {
        let data = name.as_bytes();
        let data = data.strip_suffix(b"\n").unwrap_or(data);
        data.len() == length
            && data.iter().all(u8::is_ascii_hexdigit)
            && data.iter().all(|b| !b.is_ascii_uppercase())
    })
}
#[allow(clippy::cast_precision_loss)]
fn mtime(path: &Path) -> Result<f64, CacheError> {
    let stat = fs::metadata(path).map_err(|error| io(&error))?;
    Ok(stat.mtime() as f64 + stat.mtime_nsec() as f64 * 1e-9)
}
fn touch(path: &Path) -> Result<f64, CacheError> {
    let now = rustix::fs::Timespec {
        tv_sec: 0,
        tv_nsec: UTIME_NOW,
    };
    utimensat(
        CWD,
        path,
        &Timestamps {
            last_access: now,
            last_modification: now,
        },
        AtFlags::empty(),
    )
    .map_err(errno)?;
    mtime(path)
}
fn lookup(state: &mut State, path: &Path) -> Result<bool, CacheError> {
    let key = Key::new(path);
    let Some(entry) = state.entries.get_mut(&key) else {
        return Ok(false);
    };
    if !path.is_dir() {
        state.entries.remove(&key);
        return Ok(false);
    }
    entry.used = touch(path)?;
    Ok(true)
}
fn children(path: &Path) -> Result<Vec<PathBuf>, CacheError> {
    fs::read_dir(path)
        .map_err(|error| io(&error))?
        .map(|entry| entry.map(|entry| entry.path()).map_err(|error| io(&error)))
        .collect()
}
impl CacheRoot {
    /// No default logging sink: callers explicitly choose their frontend.
    /// # Errors
    /// Rejects a nonpositive arbitrary-precision cap.
    pub fn new(root: &Path, max_bytes: BigInt, log: Arc<dyn CacheLog>) -> Result<Self, CacheError> {
        if max_bytes < BigInt::from(1) {
            return Err(CacheError::InvalidCap);
        }
        Ok(Self {
            root: lexical(root),
            max_bytes,
            state: Mutex::new(State::default()),
            log,
        })
    }
    fn state(&self) -> Result<MutexGuard<'_, State>, CacheError> {
        self.state.lock().map_err(|_| CacheError::State)
    }
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// Create layout, acquire real nonblocking flock, clear tmp, then reindex.
    /// The descriptor remains owned if indexing fails, matching source recovery.
    /// # Errors
    /// Busy is distinct from ordinary filesystem failure.
    pub fn open(&self) -> Result<(), CacheError> {
        let mut state = self.state()?;
        if self.root.as_os_str().as_bytes().contains(&0) {
            return Err(CacheError::PathValue);
        }
        for part in ["code", "setup", "tmp"] {
            fs::create_dir_all(self.root.join(part)).map_err(|error| io(&error))?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.root.join(".lock"))
            .map_err(|error| io(&error))?;
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(rustix::io::Errno::WOULDBLOCK) => return Err(CacheError::Busy),
            Err(error) => return Err(errno(error)),
        }
        state.lock = Some(file);
        for path in children(&self.root.join("tmp"))? {
            remove_tree(&path);
        }
        state.entries.clear();
        state.next = BigInt::from(0);
        for path in self.scan()? {
            let used = mtime(&path)?;
            state.insert(path.clone(), tree_size(&path)?, used);
        }
        Ok(())
    }
    /// # Errors
    /// A poisoned state lock remains an explicit internal failure.
    pub fn close(&self) -> Result<(), CacheError> {
        self.state()?.lock = None;
        Ok(())
    }
    fn scan(&self) -> Result<Vec<PathBuf>, CacheError> {
        let mut found: Vec<_> = children(&self.root.join("setup"))?
            .into_iter()
            .filter(|path| native_hex_name(path, 64))
            .collect();
        // Python Path.glob suppresses scanning errors and follows intermediate links.
        for owner in children(&self.root.join("code")).unwrap_or_default() {
            if !owner.is_dir() {
                continue;
            }
            for repo in children(&owner).unwrap_or_default() {
                if repo.is_dir() {
                    found.extend(
                        children(&repo)
                            .unwrap_or_default()
                            .into_iter()
                            .filter(|path| native_hex_name(path, 40)),
                    );
                }
            }
        }
        Ok(found
            .into_iter()
            .filter(|path| path.is_dir() && !path.is_symlink())
            .collect())
    }
    /// # Errors
    /// Source regex failures are reported without echoing input text.
    pub fn code_path(&self, repo: &String, commit: &String) -> Result<PathBuf, CacheError> {
        let repo = name_text(&repo.lowercase())?;
        let Some((owner, name)) = repo.split_once('/') else {
            return Err(CacheError::Repository);
        };
        let valid = |value: &str| {
            let body = value.strip_suffix('\n').unwrap_or(value);
            !body.is_empty()
                && body.len() <= 100
                && body
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        };
        if !valid(owner) || !valid(name) || [".", ".."].contains(&name) {
            return Err(CacheError::Repository);
        }
        if !hex_name(commit, 40) {
            return Err(CacheError::Commit);
        }
        let commit = commit.as_utf8().ok_or(CacheError::Commit)?;
        Ok(lexical(
            &self.root.join("code").join(owner).join(name).join(commit),
        ))
    }
    /// # Errors
    /// Rejects source key regex failures; a single trailing LF is accepted.
    pub fn setup_path(&self, key: &String) -> Result<PathBuf, CacheError> {
        if !hex_name(key, 64) {
            return Err(CacheError::Key);
        }
        Ok(self
            .root
            .join("setup")
            .join(key.as_utf8().ok_or(CacheError::Key)?))
    }
    /// Atomically create an owned directory using the source eight-character alphabet.
    /// # Errors
    /// Entropy, path, and filesystem failures remain distinct.
    pub fn staging(&self, prefix: &Path) -> Result<PathBuf, CacheError> {
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
        if prefix.as_os_str().as_bytes().contains(&0) {
            return Err(CacheError::PathValue);
        }
        for _ in 0..238_328 {
            // frozen Python 3.13.11 tempfile.TMP_MAX, not a new policy limit
            let mut random = [0u8; 8];
            getrandom::fill(&mut random).map_err(|_| CacheError::Entropy)?;
            let mut name = prefix.as_os_str().as_bytes().to_vec();
            name.extend(
                random
                    .into_iter()
                    .map(|byte| ALPHABET[usize::from(byte) % ALPHABET.len()]),
            );
            let path = self
                .root
                .join("tmp")
                .join(std::ffi::OsString::from_vec(name));
            if path.as_os_str().as_bytes().contains(&0) {
                return Err(CacheError::PathValue);
            }
            let result = fs::DirBuilder::new().mode(0o700).create(&path);
            match result {
                Ok(()) => return abspath(&path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(io(&error)),
            }
        }
        Err(CacheError::Io { errno: Some(17) })
    }
    /// # Errors
    /// Filesystem errors during touch are propagated.
    pub fn lookup(&self, path: &Path) -> Result<bool, CacheError> {
        lookup(&mut *self.state()?, &lexical(path))
    }
    /// # Errors
    /// Lookup precedes the hold, atomically under one mutex.
    pub fn acquire(&self, path: &Path) -> Result<bool, CacheError> {
        let mut state = self.state()?;
        let path = lexical(path);
        if !lookup(&mut state, &path)? {
            return Ok(false);
        }
        state.hold(&path);
        Ok(true)
    }
    /// # Errors
    /// Poisoned mutex only; unknown paths are allowed.
    pub fn hold(&self, path: &Path) -> Result<(), CacheError> {
        self.state()?.hold(&lexical(path));
        Ok(())
    }
    /// # Errors
    /// Eviction errors propagate after all requested decrements.
    pub fn release(&self, paths: &[PathBuf]) -> Result<(), CacheError> {
        let mut state = self.state()?;
        for path in paths {
            state.unhold(&lexical(path));
        }
        self.evict_locked(&mut state)?;
        Ok(())
    }
    /// Check before locking; atomically rename, touch, then protect through eviction.
    /// An existing nonempty directory reuses the old entry without eviction.
    /// # Errors
    /// Tree refusal precedes destination creation; filesystem failures preserve source partial state.
    pub fn publish(&self, staging: &Path, path: &Path, hold: bool) -> Result<bool, CacheError> {
        let size = check_tree(staging)?;
        let mut state = self.state()?;
        let path = lexical(path);
        let parent = path.parent().unwrap_or(Path::new("."));
        if parent.as_os_str().as_bytes().contains(&0) {
            return Err(CacheError::PathValue);
        }
        fs::create_dir_all(parent).map_err(|error| io(&error))?;
        if path.as_os_str().as_bytes().contains(&0) {
            return Err(CacheError::PathValue);
        }
        if let Err(error) = fs::rename(staging, &path) {
            if !path.is_dir() {
                return Err(io(&error));
            }
            remove_tree(staging);
            if !state.entries.contains_key(&Key::new(&path)) {
                state.insert(path.clone(), tree_size(&path)?, mtime(&path)?);
            }
            lookup(&mut state, &path)?;
            if hold {
                state.hold(&path);
            }
            return Ok(false);
        }
        let used = touch(&path)?;
        state.insert(path.clone(), size, used);
        state.hold(&path);
        let result = self.evict_locked(&mut state);
        if !hold || result.is_err() {
            state.unhold(&path);
        }
        result?;
        Ok(true)
    }
    pub fn discard(&self, staging: &Path) {
        remove_tree(staging);
    }
    /// # Errors
    /// Poisoned mutex only.
    pub fn total_bytes(&self) -> Result<BigInt, CacheError> {
        Ok(self.state()?.total())
    }
    /// # Errors
    /// Filesystem failure after removing the index entry is preserved.
    pub fn evict(&self) -> Result<Vec<PathBuf>, CacheError> {
        self.evict_locked(&mut *self.state()?)
    }
    fn evict_locked(&self, state: &mut State) -> Result<Vec<PathBuf>, CacheError> {
        let mut candidates: Vec<_> = state
            .entries
            .iter()
            .filter(|(path, _)| !state.held.contains_key(*path))
            .map(|(path, entry)| (path.clone(), entry.used, entry.order.clone()))
            .collect();
        candidates.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.2.cmp(&b.2)));
        let mut total = state.total();
        let mut evicted = Vec::new();
        for (path, _, _) in candidates {
            if total <= self.max_bytes {
                break;
            }
            if let Some(entry) = state.entries.remove(&path) {
                total -= entry.size;
            }
            let path = path.path();
            let doomed = self.staging(Path::new("evicted-"))?;
            match fs::rename(&path, doomed.join("entry")) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(io(&error)),
            }
            remove_tree(&doomed);
            evicted.push(path);
        }
        if !evicted.is_empty() {
            self.log.event(CacheEvent::Evicted(evicted.len()))?;
        }
        if total > self.max_bytes {
            self.log.event(CacheEvent::OverCap {
                bytes: total,
                cap: self.max_bytes.clone(),
            })?;
        }
        Ok(evicted)
    }
    /// Source insertion order and actual floating filesystem mtimes, without text rewriting.
    /// # Errors
    /// Poisoned mutex only.
    pub fn entries(&self) -> Result<Vec<CacheEntry>, CacheError> {
        let state = self.state()?;
        let mut entries: Vec<_> = state.entries.iter().collect();
        entries.sort_by(|a, b| a.1.order.cmp(&b.1.order));
        Ok(entries
            .into_iter()
            .map(|(path, entry)| CacheEntry {
                path: path.path(),
                size: entry.size.clone(),
                used: entry.used,
            })
            .collect())
    }
    /// # Errors
    /// Poisoned mutex only. Lossless byte-key order; both '/' and '//' can coexist.
    pub fn holds(&self) -> Result<Vec<(PathBuf, BigInt)>, CacheError> {
        Ok(self
            .state()?
            .held
            .iter()
            .map(|(key, count)| (key.path(), count.clone()))
            .collect())
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
