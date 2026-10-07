//! Required code-acquisition and filesystem seams; no implicit cancellation backend.
use crate::{
    archive,
    cache::{CacheError, CacheLog, CacheRoot},
    cancellation::CancellationEvent,
    files::TreeError,
    setup,
};

use num_bigint::BigInt;
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
};

pub type CodeFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, CodeError>> + Send + 'a>>;
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CodeError {
    #[error("code repository is not allowed")]
    CodeNotAllowed,
    #[error("code acquisition failed")]
    RunnerError,
    #[error("code tree was refused")]
    InvalidCode,
    #[error("GitHub acquisition failed")]
    GitHub,
    #[error("code callback failed")]
    Callback,
    #[error("code cache operation failed")]
    Cache(#[from] CacheError),
    #[error("code archive operation failed")]
    Archive(archive::ExtractError),
    #[error("code operation was cancelled")]
    Cancelled,
    #[error("code executor returned an incompatible receipt")]
    Receipt,
    #[error("code slot state is unavailable")]
    State,
}
#[derive(Clone, Debug)]
pub struct CodeLimits {
    pub max_archive_bytes: BigInt,
    pub max_tree_bytes: BigInt,
    pub max_files: BigInt,
}
impl Default for CodeLimits {
    fn default() -> Self {
        Self {
            max_archive_bytes: BigInt::from(512_u64 << 20),
            max_tree_bytes: BigInt::from(2_u64 << 30),
            max_files: BigInt::from(100_000),
        }
    }
}
#[derive(Clone)]
enum Signal {
    Event(CancellationEvent),
    Setup(setup::Cancellation),
}
/// Explicit caller signal. It neither selects nor implements thread cancellation.
#[derive(Clone)]
pub struct CodeCancellation(Signal);
impl CodeCancellation {
    #[must_use]
    pub fn from_event(event: CancellationEvent) -> Self {
        Self(Signal::Event(event))
    }
    #[must_use]
    pub fn from_setup(signal: setup::Cancellation) -> Self {
        Self(Signal::Setup(signal))
    }
    #[must_use]
    pub fn requested(&self) -> bool {
        match &self.0 {
            Signal::Event(v) => v.is_set(),
            Signal::Setup(v) => v.requested(),
        }
    }
    /// Await the caller's signal without substituting task-abort semantics.
    pub async fn cancelled(&self) {
        match &self.0 {
            Signal::Event(event) => event.wait().await,
            Signal::Setup(signal) => signal.clone().cancelled().await,
        }
    }
    fn check(&self) -> Result<(), CodeError> {
        if self.requested() {
            Err(CodeError::Cancelled)
        } else {
            Ok(())
        }
    }
    fn cleanup() -> Self {
        Self::from_event(CancellationEvent::new())
    }
}
/// Implementations retain neither token nor request diagnostics in errors and
/// settle their own download/callback work before acknowledging cancellation.
/// Dropping a callback future must leave no detached writes to its staging path.
/// A backend that cannot guarantee that must provide an outer settlement owner;
/// this trait does not silently select one for production.
pub trait GitHubSource: Send + Sync {
    fn verify_commit<'a>(
        &'a self,
        repo: &'a str,
        commit: &'a str,
        cancel: CodeCancellation,
    ) -> CodeFuture<'a, ()>;
    fn download_tarball<'a>(
        &'a self,
        repo: &'a str,
        commit: &'a str,
        path: PathBuf,
        max_bytes: BigInt,
        cancel: CodeCancellation,
    ) -> CodeFuture<'a, ()>;
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationKind {
    Open,
    Close,
    Acquire,
    Staging,
    Extract,
    Publish,
    Discard,
    Release,
}
enum Action {
    Open,
    Close,
    Acquire(PathBuf),
    Staging,
    Extract {
        archive: PathBuf,
        target: PathBuf,
        commit: String,
        limits: CodeLimits,
    },
    Publish {
        tree: PathBuf,
        path: PathBuf,
    },
    Discard(PathBuf),
    Release(PathBuf),
}
/// Fixed native operation; no arbitrary callback/script is executable here.
pub struct FsOperation {
    cache: Arc<CacheRoot>,
    executor: Arc<dyn FsExecutor>,
    action: Action,
}
/// Execution is mandatory. A future abandoned while a worker is running must
/// let that worker settle and drop its receipt, rather than losing it. Receipts
/// own completed effects before they are returned to an async caller.
pub trait FsExecutor: Send + Sync {
    fn execute(
        &self,
        operation: FsOperation,
        cancel: CodeCancellation,
    ) -> CodeFuture<'_, FsReceipt>;
    /// Execute cleanup without blocking an async worker; preserve submission
    /// order and the order of operations within each submitted batch.
    /// Wait for this executor's already-running operations on the cache before
    /// applying cleanup. Raw future abandonment must not race a pending publish
    /// or extraction with removal of its staging directory.
    fn detach(&self, operations: Vec<FsOperation>);
}
enum Payload {
    Unit,
    Bool(bool),
    Path(PathBuf),
}
enum Cleanup {
    Close,
    Discard(PathBuf),
    Release(PathBuf),
}
/// A successful effect is armed until explicitly transferred/settled. Debug
/// exposes no path or callback payload. Native callbacks must return this receipt.
pub struct FsReceipt {
    payload: Payload,
    cleanup: Option<Cleanup>,
    cache: Arc<CacheRoot>,
    executor: Arc<dyn FsExecutor>,
}
impl FsReceipt {
    fn unit(&self) -> Result<(), CodeError> {
        if matches!(self.payload, Payload::Unit) {
            Ok(())
        } else {
            Err(CodeError::Receipt)
        }
    }
    fn boolean(&self) -> Result<bool, CodeError> {
        if let Payload::Bool(v) = self.payload {
            Ok(v)
        } else {
            Err(CodeError::Receipt)
        }
    }
    fn path(&self) -> Result<PathBuf, CodeError> {
        if let Payload::Path(v) = &self.payload {
            Ok(v.clone())
        } else {
            Err(CodeError::Receipt)
        }
    }
    fn transfer(&mut self) {
        self.cleanup = None;
    }
}
impl Drop for FsReceipt {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            let action = match cleanup {
                Cleanup::Close => Action::Close,
                Cleanup::Discard(p) => Action::Discard(p),
                Cleanup::Release(p) => Action::Release(p),
            };
            self.executor.detach(vec![FsOperation {
                cache: self.cache.clone(),
                executor: self.executor.clone(),
                action,
            }]);
        }
    }
}
impl FsOperation {
    #[must_use]
    pub fn kind(&self) -> OperationKind {
        match self.action {
            Action::Open => OperationKind::Open,
            Action::Close => OperationKind::Close,
            Action::Acquire(_) => OperationKind::Acquire,
            Action::Staging => OperationKind::Staging,
            Action::Extract { .. } => OperationKind::Extract,
            Action::Publish { .. } => OperationKind::Publish,
            Action::Discard(_) => OperationKind::Discard,
            Action::Release(_) => OperationKind::Release,
        }
    }
    /// Execute on the executor's chosen filesystem context. No implicit thread
    /// backend exists. Source ordinary partial failures are not rolled back.
    /// # Errors
    /// Returns static native source categories; successful effects carry ownership.
    pub fn run(self) -> Result<FsReceipt, CodeError> {
        let mut cleanup = None;
        let payload = match self.action {
            Action::Open => {
                self.cache.open()?;
                cleanup = Some(Cleanup::Close);
                Payload::Unit
            }
            Action::Close => {
                self.cache.close()?;
                Payload::Unit
            }
            Action::Acquire(path) => {
                let acquired = self.cache.acquire(&path)?;
                if acquired {
                    cleanup = Some(Cleanup::Release(path));
                }
                Payload::Bool(acquired)
            }
            Action::Staging => {
                let path = self.cache.staging(std::path::Path::new("code-"))?;
                cleanup = Some(Cleanup::Discard(path.clone()));
                Payload::Path(path)
            }
            Action::Extract {
                archive,
                target,
                commit,
                limits,
            } => {
                archive::extract_tree(
                    &archive,
                    &target,
                    &commit,
                    &archive::CodeLimits {
                        max_tree_bytes: limits.max_tree_bytes,
                        max_files: limits.max_files,
                    },
                )
                .map_err(extract_error)?;
                Payload::Unit
            }
            Action::Publish { tree, path } => {
                self.cache.publish(&tree, &path, true).map_err(|e| {
                    if e == CacheError::Tree(TreeError::Unsafe) {
                        CodeError::InvalidCode
                    } else {
                        CodeError::Cache(e)
                    }
                })?;
                cleanup = Some(Cleanup::Release(path));
                Payload::Unit
            }
            Action::Discard(path) => {
                self.cache.discard(&path);
                Payload::Unit
            }
            Action::Release(path) => {
                self.cache.release(&[path])?;
                Payload::Unit
            }
        };
        Ok(FsReceipt {
            payload,
            cleanup,
            cache: self.cache,
            executor: self.executor,
        })
    }
}
fn extract_error(error: archive::ExtractError) -> CodeError {
    match error {
        archive::ExtractError::Unsafe => CodeError::InvalidCode,
        archive::ExtractError::Commit => CodeError::RunnerError,
        error => CodeError::Archive(error),
    }
}
/// A store shares the cache, not a cold-miss mutex. Every successful call
/// transfers one hold to its caller, which must release that path.
pub struct CodeStore {
    pub cache: Arc<CacheRoot>,
    github: Arc<dyn GitHubSource>,
    executor: Arc<dyn FsExecutor>,
    limits: CodeLimits,
    allowed_repos: Option<Vec<String>>,
}
impl CodeStore {
    #[must_use]
    pub fn new(
        cache: Arc<CacheRoot>,
        github: Arc<dyn GitHubSource>,
        executor: Arc<dyn FsExecutor>,
        limits: CodeLimits,
        allowed_repos: Option<Vec<String>>,
    ) -> Self {
        Self {
            cache,
            github,
            executor,
            limits,
            allowed_repos,
        }
    }
    async fn execute(
        &self,
        action: Action,
        cancel: CodeCancellation,
    ) -> Result<FsReceipt, CodeError> {
        self.executor
            .execute(
                FsOperation {
                    cache: self.cache.clone(),
                    executor: self.executor.clone(),
                    action,
                },
                cancel,
            )
            .await
    }
    /// # Errors
    /// Preserves allowlist/path/acquire/staging ordering, then verify/download,
    /// extraction, publication and finally-discard. Finally errors override.
    pub async fn tree(
        &self,
        repo: &String,
        commit: &String,
        cancel: CodeCancellation,
    ) -> Result<PathBuf, CodeError> {
        if self
            .allowed_repos
            .as_ref()
            .is_some_and(|list| !list.contains(&repo.lowercase()))
        {
            return Err(CodeError::CodeNotAllowed);
        }
        let path = self.cache.code_path(repo, commit)?;
        let mut acquired = self
            .execute(Action::Acquire(path.clone()), cancel.clone())
            .await?;
        if acquired.boolean()? {
            cancel.check()?;
            acquired.transfer();
            return Ok(path);
        }
        cancel.check()?;
        let mut staging = self.execute(Action::Staging, cancel.clone()).await?;
        let staging_path = staging.path()?;
        let mut publication = None;
        let result: Result<(), CodeError> = async {
            cancel.check()?;
            let download = async {
                self.github
                    .verify_commit(repo, commit, cancel.clone())
                    .await?;
                cancel.check()?;
                self.github
                    .download_tarball(
                        repo,
                        commit,
                        staging_path.join("archive.tar.gz"),
                        self.limits.max_archive_bytes.clone(),
                        cancel.clone(),
                    )
                    .await
            }
            .await;
            download.map_err(|error| {
                if error == CodeError::GitHub {
                    CodeError::RunnerError
                } else {
                    error
                }
            })?;
            cancel.check()?;
            self.execute(
                Action::Extract {
                    archive: staging_path.join("archive.tar.gz"),
                    target: staging_path.join("tree"),
                    commit: commit.clone(),
                    limits: self.limits.clone(),
                },
                cancel.clone(),
            )
            .await?
            .unit()?;
            cancel.check()?;
            publication = Some(
                self.execute(
                    Action::Publish {
                        tree: staging_path.join("tree"),
                        path: path.clone(),
                    },
                    cancel.clone(),
                )
                .await?,
            );
            publication.as_ref().ok_or(CodeError::Receipt)?.unit()?;
            cancel.check()?;
            Ok(())
        }
        .await;
        let cleanup = self
            .execute(Action::Discard(staging_path), CodeCancellation::cleanup())
            .await
            .and_then(|v| v.unit());
        staging.transfer();
        // The publication receipt still owns this hold. On failure its
        // drop releases it; ownership transfers only with a returned path.
        cleanup?;
        result?;
        if let Some(hold) = &mut publication {
            hold.transfer();
        }
        Ok(path)
    }
}
/// Native typed fields corresponding to the source `CodeSlot`'s consumed config.
pub struct SlotConfig {
    pub root: Option<PathBuf>,
    pub max_bytes: BigInt,
    pub allowed_repos: Option<Vec<String>>,
}

fn setup_error(error: CodeError) -> setup::SetupError {
    use archive::ExtractError;
    use setup::SetupError;
    match error {
        CodeError::CodeNotAllowed => SetupError::CodeNotAllowed,
        CodeError::InvalidCode
        | CodeError::Archive(ExtractError::Unsafe | ExtractError::Tree(TreeError::Unsafe)) => {
            SetupError::InvalidCode
        }
        CodeError::RunnerError | CodeError::GitHub | CodeError::Archive(ExtractError::Commit) => {
            SetupError::RunnerError
        }
        CodeError::Cancelled => SetupError::Cancelled,
        CodeError::Archive(ExtractError::Value)
        | CodeError::Cache(
            CacheError::InvalidCap
            | CacheError::Repository
            | CacheError::Commit
            | CacheError::Key
            | CacheError::PathValue,
        ) => SetupError::Value,
        CodeError::Archive(ExtractError::Encoding) => SetupError::Encoding,
        CodeError::Archive(
            ExtractError::Io { errno } | ExtractError::Tree(TreeError::Io { errno }),
        )
        | CodeError::Cache(CacheError::Io { errno }) => SetupError::Io { errno },
        CodeError::Cache(error) => SetupError::Cache(error),
        CodeError::Archive(ExtractError::Tree(TreeError::Path(error))) => match error {
            crate::paths::PathError::Encoding => SetupError::Encoding,
            crate::paths::PathError::InvalidNul => SetupError::Value,
            crate::paths::PathError::NotDirectory => SetupError::Io { errno: Some(20) },
            crate::paths::PathError::Io { errno } => SetupError::Io { errno },
        },
        // Receipt/state failures are required-adapter failures rather than values.
        CodeError::Callback | CodeError::Receipt | CodeError::State => SetupError::Callback,
    }
}
impl setup::CodeSource for CodeStore {
    fn tree<'a>(
        &'a self,
        code: &'a setup::CodeRef,
        cancel: setup::Cancellation,
    ) -> setup::SetupFuture<'a, PathBuf> {
        Box::pin(async move {
            CodeStore::tree(
                self,
                &code.repo,
                &code.commit,
                CodeCancellation::from_setup(cancel),
            )
            .await
            .map_err(setup_error)
        })
    }
}
pub struct CodeSlot {
    config: SlotConfig,
    github: Arc<dyn GitHubSource>,
    executor: Arc<dyn FsExecutor>,
    log: Arc<dyn CacheLog>,
    opening: tokio::sync::Mutex<()>,
    store: Mutex<Option<Arc<CodeStore>>>,
}
impl CodeSlot {
    #[must_use]
    pub fn new(
        config: SlotConfig,
        github: Arc<dyn GitHubSource>,
        executor: Arc<dyn FsExecutor>,
        log: Arc<dyn CacheLog>,
    ) -> Self {
        Self {
            config,
            github,
            executor,
            log,
            opening: tokio::sync::Mutex::new(()),
            store: Mutex::new(None),
        }
    }
    /// # Errors
    /// Cache contention remains `Cache(Busy)` for an actionable native error;
    /// ordinary open I/O failures become `RunnerError`.
    pub async fn get(&self, cancel: CodeCancellation) -> Result<Arc<CodeStore>, CodeError> {
        let _opening = self.opening.lock().await;
        cancel.check()?;
        if let Some(store) = self.store.lock().map_err(|_| CodeError::State)?.clone() {
            return Ok(store);
        }
        let root = self.config.root.as_ref().ok_or(CodeError::RunnerError)?;
        let cache = Arc::new(CacheRoot::new(
            root,
            self.config.max_bytes.clone(),
            self.log.clone(),
        )?);
        let operation = FsOperation {
            cache: cache.clone(),
            executor: self.executor.clone(),
            action: Action::Open,
        };
        let mut opened = self
            .executor
            .execute(operation, cancel.clone())
            .await
            .map_err(|error| match error {
                CodeError::Cache(CacheError::Io { .. }) => CodeError::RunnerError,
                other => other,
            })?;
        opened.unit()?;
        cancel.check()?;
        let store = Arc::new(CodeStore::new(
            cache,
            self.github.clone(),
            self.executor.clone(),
            CodeLimits::default(),
            self.config.allowed_repos.clone(),
        ));
        *self.store.lock().map_err(|_| CodeError::State)? = Some(store.clone());
        opened.transfer();
        Ok(store)
    }
    /// # Errors
    /// Close failures leave the installed store. No opening mutex is acquired:
    /// this retains the source get/close races, including concurrent closes.
    pub async fn close(&self) -> Result<(), CodeError> {
        let store = self.store.lock().map_err(|_| CodeError::State)?.clone();
        if let Some(store) = store {
            store
                .execute(Action::Close, CodeCancellation::cleanup())
                .await?
                .unit()?;
            *self.store.lock().map_err(|_| CodeError::State)? = None;
        }
        Ok(())
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
