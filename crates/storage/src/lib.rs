#![forbid(unsafe_code)]
//! Production object-store adapters. Callers own request and cancellation lifetimes.
pub mod local;
pub mod s3;
use std::{fmt, io};

/// Caller-owned asynchronous cancellation settlement. Implementations must track
/// submitted work, select its timeout, and drain it after request shutdown.
pub trait CancellationSink: Send + Sync {
    fn submit(&self, work: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>);
}

pub const DOWNLOAD_URL_SECONDS: u64 = 300;
pub const CHUNK: usize = 1 << 20;

#[derive(Clone, PartialEq, Eq)]
pub struct StoredObject {
    pub size_bytes: u64,
    pub sha256: String,
    pub generation: Option<String>,
}
#[derive(Clone, PartialEq, Eq)]
pub struct ObjectHead {
    pub size_bytes: u64,
    pub generation: Option<String>,
    pub sha256: Option<String>,
}
impl fmt::Debug for StoredObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StoredObject(<redacted>)")
    }
}
impl fmt::Debug for ObjectHead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ObjectHead(<redacted>)")
    }
}
pub enum Error {
    InvalidKey,
    InvalidChecksum,
    ObjectExists,
    ObjectTooLarge,
    Io(io::ErrorKind),
    Body(io::ErrorKind),
    Worker,
    Store {
        operation: &'static str,
        code: String,
    },
    Transport {
        operation: &'static str,
        kind: &'static str,
    },
    Credentials,
    PartsRejected(String),
    IntegerRange,
    PresignExpiry,
}
impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store { operation, .. } => {
                write!(f, "Store {{ operation: {operation}, code: <redacted> }}")
            }
            Self::PartsRejected(_) => f.write_str("PartsRejected(<redacted>)"),
            Self::Transport { operation, kind } => {
                write!(f, "Transport {{ operation: {operation}, kind: {kind} }}")
            }
            Self::Io(kind) => write!(f, "Io({kind:?})"),
            Self::Body(kind) => write!(f, "Body({kind:?})"),
            Self::InvalidKey => f.write_str("InvalidKey"),
            Self::InvalidChecksum => f.write_str("InvalidChecksum"),
            Self::ObjectExists => f.write_str("ObjectExists"),
            Self::ObjectTooLarge => f.write_str("ObjectTooLarge"),
            Self::Worker => f.write_str("Worker"),
            Self::Credentials => f.write_str("Credentials"),
            Self::IntegerRange => f.write_str("IntegerRange"),
            Self::PresignExpiry => f.write_str("PresignExpiry"),
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store { operation, .. } => write!(f, "object store {operation} failed"),
            Self::Transport { operation, kind } => {
                write!(f, "object store {operation} failed: {kind}")
            }
            Self::Io(kind) => write!(f, "local object store failed: {kind:?}"),
            Self::Body(kind) => write!(f, "object response body failed: {kind:?}"),
            Self::PartsRejected(_) => f.write_str("multipart parts rejected"),
            _ => write!(f, "{self:?}"),
        }
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}
impl Error {
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        if let Self::Store { code, .. } = self {
            Some(code)
        } else {
            None
        }
    }
    pub(crate) fn missing(&self) -> bool {
        self.code()
            .is_some_and(|code| matches!(code, "404" | "NoSuchKey" | "NotFound" | "NoSuchUpload"))
    }
}
pub(crate) fn hex(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write;
    let mut result = String::with_capacity(bytes.as_ref().len() * 2);
    for byte in bytes.as_ref() {
        let _ = write!(result, "{byte:02x}");
    }
    result
}

pub enum ObjectStore {
    Local(local::LocalStore),
    S3(s3::S3Store),
}
impl ObjectStore {
    #[must_use]
    pub fn presigning(&self) -> Option<&s3::S3Store> {
        match self {
            Self::S3(store) => Some(store),
            Self::Local(_) => None,
        }
    }
}
/// # Errors
/// Returns source-classified storage initialization errors and measured SDK integer range limits.
pub async fn create_store(
    settings: &cannery_core::settings::StorageSettings,
) -> Result<ObjectStore, Error> {
    if settings.backend == cannery_core::settings::StorageBackend::S3 {
        Ok(ObjectStore::S3(s3::S3Store::from_settings(settings).await?))
    } else {
        Ok(ObjectStore::Local(local::LocalStore::new(
            &settings.local_root,
            &settings.bucket,
        )?))
    }
}

pub enum ObjectReader {
    Local(local::LocalReader),
    S3(s3::S3Reader),
}
impl ObjectReader {
    /// # Errors
    /// Returns the source stream's filesystem or response-body error.
    pub async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, Error> {
        match self {
            Self::Local(reader) => reader.next_chunk().await,
            Self::S3(reader) => reader.next_chunk().await,
        }
    }
}
impl ObjectStore {
    #[must_use]
    pub fn backend(&self) -> &'static str {
        match self {
            Self::Local(store) => store.backend(),
            Self::S3(store) => store.backend(),
        }
    }
    #[must_use]
    pub fn bucket(&self) -> &str {
        match self {
            Self::Local(store) => &store.bucket,
            Self::S3(store) => &store.bucket,
        }
    }
    /// # Errors
    /// Returns source-classified key, storage, or input-stream errors; publication is create-only.
    pub async fn write_new<S>(
        &self,
        key: &str,
        chunks: S,
        max_bytes: impl Into<num_bigint::BigInt>,
    ) -> Result<StoredObject, Error>
    where
        S: futures_util::Stream<Item = Result<Vec<u8>, Error>> + Unpin,
    {
        match self {
            Self::Local(store) => store.write_new(key, chunks, max_bytes).await,
            Self::S3(store) => store.write_new(key, chunks, max_bytes).await,
        }
    }
    /// Stream with caller-owned cancellation settlement for unpublished S3 multipart uploads.
    /// # Errors
    /// Returns the same source-classified errors as `write_new`.
    pub async fn write_new_with_cancellation<S>(
        &self,
        key: &str,
        chunks: S,
        max_bytes: impl Into<num_bigint::BigInt>,
        cancellation: std::sync::Arc<dyn CancellationSink>,
    ) -> Result<StoredObject, Error>
    where
        S: futures_util::Stream<Item = Result<Vec<u8>, Error>> + Unpin,
    {
        match self {
            Self::Local(store) => store.write_new(key, chunks, max_bytes).await,
            Self::S3(store) => {
                store
                    .write_new_with_cancellation(key, chunks, max_bytes, cancellation)
                    .await
            }
        }
    }
    /// # Errors
    /// Returns source-classified key or storage errors.
    pub async fn head(&self, key: &str) -> Result<Option<ObjectHead>, Error> {
        match self {
            Self::Local(store) => store.head(key).await,
            Self::S3(store) => store.head(key).await,
        }
    }
    /// # Errors
    /// Returns source-classified key or storage errors while hashing all bytes.
    pub async fn stat(&self, key: &str) -> Result<Option<StoredObject>, Error> {
        match self {
            Self::Local(store) => store.stat(key).await,
            Self::S3(store) => store.stat(key).await,
        }
    }
    /// # Errors
    /// Returns source-classified key or storage errors, including missing objects.
    pub async fn read(&self, key: &str) -> Result<ObjectReader, Error> {
        match self {
            Self::Local(store) => Ok(ObjectReader::Local(store.read(key).await?)),
            Self::S3(store) => Ok(ObjectReader::S3(store.read(key).await?)),
        }
    }
    /// # Errors
    /// Returns source-classified key or storage errors; missing or changed generations are benign.
    pub async fn delete(&self, key: &str, generation: Option<&str>) -> Result<(), Error> {
        match self {
            Self::Local(store) => store.delete(key, generation).await,
            Self::S3(store) => store.delete(key, generation).await,
        }
    }
    /// # Errors
    /// Returns source-classified storage and clock range errors. Caller supplies the S3 sweep clock.
    pub async fn sweep_staging(
        &self,
        older_than_seconds: f64,
        now: std::time::SystemTime,
    ) -> Result<u64, Error> {
        match self {
            Self::Local(store) => store.sweep_staging(older_than_seconds).await,
            Self::S3(store) => store.sweep_staging(older_than_seconds, now).await,
        }
    }
}
#[must_use]
pub fn presigning(store: &ObjectStore) -> Option<&s3::S3Store> {
    store.presigning()
}
