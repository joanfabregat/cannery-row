use std::fmt;

use num_bigint::BigInt;
use num_traits::ToPrimitive;

use super::{SettingsError, SettingsIssue};

/// Checked signed 64-bit settings integers, with explicit consumer conversions.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SettingsInt(pub(crate) BigInt);

impl SettingsInt {
    #[must_use]
    pub fn as_bigint(&self) -> &BigInt {
        &self.0
    }

    /// # Errors
    /// Returns a field-naming error when the consumer's signed range is exceeded.
    pub fn to_i64(&self, field: &str) -> Result<i64, SettingsError> {
        self.0.to_i64().ok_or_else(|| conversion_error(field))
    }

    /// # Errors
    /// Returns a field-naming error for negative integers or unsigned range overflow.
    pub fn to_u64(&self, field: &str) -> Result<u64, SettingsError> {
        self.0.to_u64().ok_or_else(|| conversion_error(field))
    }

    /// # Errors
    /// Returns a field-naming error for negative integers or platform size overflow.
    pub fn to_usize(&self, field: &str) -> Result<usize, SettingsError> {
        self.0.to_usize().ok_or_else(|| conversion_error(field))
    }
}

fn conversion_error(field: &str) -> SettingsError {
    SettingsError::Invalid {
        issues: vec![SettingsIssue::new(
            field,
            "value exceeds this consumer's supported range",
        )],
    }
}

impl From<i64> for SettingsInt {
    fn from(value: i64) -> Self {
        Self(BigInt::from(value))
    }
}

impl fmt::Display for SettingsInt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl fmt::Debug for SettingsInt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SettingsInt(..)")
    }
}

/// Secrets are deliberately neither Serialize nor Deserialize.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(pub(crate) String);

impl Secret {
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret(\"**********\")")
    }
}

#[derive(Clone)]
pub struct DatabaseSettings {
    pub url: String,
    pub pool_min_size: SettingsInt,
    pub pool_max_size: SettingsInt,
    /// `url` connects to `url`; `managed` runs a private server.
    pub provider: DatabaseProvider,
    /// Managed only: the server's data directory (default: per platform).
    pub data_dir: Option<String>,
    /// Managed: a directory with existing `postgres` and `initdb` binaries,
    /// used instead of the bundled ones. With `url`, only where
    /// `cannery db dump` finds `pg_dump`.
    pub postgres_bin_dir: Option<String>,
    /// Managed only: where the bundled PostgreSQL is unpacked.
    pub postgres_cache_dir: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatabaseProvider {
    Url,
    Managed,
}

#[derive(Clone)]
pub struct ServerSettings {
    pub public_base_url: String,
}

#[derive(Clone)]
pub struct AuthSettings {
    pub oidc_issuer: Option<String>,
    pub oidc_client_id: Option<String>,
    pub oidc_client_secret: Option<Secret>,
    pub oidc_scopes: String,
    pub allowed_email_domains: Vec<String>,
    pub bootstrap_admin_emails: Vec<String>,
    pub session_ttl_hours: SettingsInt,
    pub cookie_secure: bool,
    pub personal_token_max_days: SettingsInt,
}

impl AuthSettings {
    #[must_use]
    pub fn oidc_enabled(&self) -> bool {
        self.oidc_issuer.is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageBackend {
    Local,
    S3,
}

impl StorageBackend {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::S3 => "s3",
        }
    }
}

#[derive(Clone)]
pub struct StorageSettings {
    pub backend: StorageBackend,
    pub local_root: String,
    pub bucket: String,
    pub upload_ttl_minutes: SettingsInt,
    pub max_object_bytes: SettingsInt,
    pub max_stream_seconds: SettingsInt,
    pub validate_json_max_bytes: SettingsInt,
    pub max_concurrent_validations: SettingsInt,
    pub s3_endpoint: Option<String>,
    pub s3_public_endpoint: Option<String>,
    pub s3_region: String,
    pub s3_access_key_id: Option<String>,
    pub s3_secret_access_key: Option<Secret>,
    pub s3_path_style: bool,
    pub s3_prefix: String,
    pub s3_presign_ttl_seconds: SettingsInt,
    pub s3_multipart_threshold_bytes: SettingsInt,
    pub s3_part_size_bytes: SettingsInt,
}

#[derive(Clone)]
pub struct LeaseSettings {
    pub ttl_seconds: SettingsInt,
    pub job_ttl_seconds: SettingsInt,
    pub job_overhead_seconds: SettingsInt,
    pub stalled_verification_seconds: SettingsInt,
}

#[derive(Clone)]
pub struct SweepSettings {
    pub enabled: bool,
    pub interval_seconds: f64,
    pub batch_size: SettingsInt,
    pub batches_per_run: SettingsInt,
    pub timeout_seconds: f64,
    pub connect_timeout_seconds: SettingsInt,
    pub lock_timeout_seconds: f64,
    pub statement_timeout_seconds: f64,
}

#[derive(Clone)]
pub struct McpSettings {
    pub max_request_bytes: SettingsInt,
    pub max_result_bytes: SettingsInt,
}

#[derive(Clone)]
pub struct WebSettings {
    pub dist_dir: Option<String>,
}

#[derive(Clone)]
pub struct TestingSettings {
    pub enabled: bool,
}

#[derive(Clone)]
pub struct Settings {
    pub database: DatabaseSettings,
    pub server: ServerSettings,
    pub auth: AuthSettings,
    pub storage: StorageSettings,
    pub leases: LeaseSettings,
    pub sweeps: SweepSettings,
    pub mcp: McpSettings,
    pub web: WebSettings,
    pub testing: TestingSettings,
}

// Every section may contain user-provided credentials, even in URL or path fields.
macro_rules! redacted_debug {
    ($($section:ty),+ $(,)?) => {$ (
        impl fmt::Debug for $section {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.debug_struct(stringify!($section)).finish_non_exhaustive()
            }
        }
    )+};
}
redacted_debug!(
    Settings,
    DatabaseSettings,
    ServerSettings,
    AuthSettings,
    StorageSettings,
    LeaseSettings,
    SweepSettings,
    McpSettings,
    WebSettings,
    TestingSettings
);
