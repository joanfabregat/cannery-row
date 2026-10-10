use std::collections::BTreeMap;
use std::net::IpAddr;

use num_bigint::BigInt;
use num_traits::ToPrimitive;

use super::input::{Input, Table};
use super::{
    AuthSettings, DatabaseProvider, DatabaseSettings, ENV_PREFIX, LeaseSettings, McpSettings,
    S3_MAX_PARTS, Secret, ServerSettings, Settings, SettingsError, SettingsInt, SettingsIssue,
    StorageBackend, StorageSettings, SweepSettings, TestingSettings, WebSettings,
};

pub(super) const SECTIONS: &[(&str, &[&str])] = &[
    (
        "database",
        &[
            "url",
            "pool_min_size",
            "pool_max_size",
            "provider",
            "data_dir",
            "postgres_bin_dir",
            "postgres_cache_dir",
        ],
    ),
    ("server", &["public_base_url"]),
    (
        "auth",
        &[
            "oidc_issuer",
            "oidc_client_id",
            "oidc_client_secret",
            "oidc_scopes",
            "allowed_email_domains",
            "bootstrap_admin_emails",
            "session_ttl_hours",
            "cookie_secure",
            "personal_token_max_days",
        ],
    ),
    (
        "storage",
        &[
            "backend",
            "local_root",
            "bucket",
            "upload_ttl_minutes",
            "max_object_bytes",
            "max_stream_seconds",
            "validate_json_max_bytes",
            "max_concurrent_validations",
            "s3_endpoint",
            "s3_public_endpoint",
            "s3_region",
            "s3_access_key_id",
            "s3_secret_access_key",
            "s3_path_style",
            "s3_prefix",
            "s3_presign_ttl_seconds",
            "s3_multipart_threshold_bytes",
            "s3_part_size_bytes",
        ],
    ),
    (
        "leases",
        &[
            "ttl_seconds",
            "job_ttl_seconds",
            "job_overhead_seconds",
            "stalled_verification_seconds",
        ],
    ),
    (
        "sweeps",
        &[
            "enabled",
            "interval_seconds",
            "batch_size",
            "batches_per_run",
            "timeout_seconds",
            "connect_timeout_seconds",
            "lock_timeout_seconds",
            "statement_timeout_seconds",
        ],
    ),
    ("mcp", &["max_request_bytes", "max_result_bytes"]),
    ("web", &["dist_dir"]),
    ("testing", &["enabled"]),
];

pub(super) fn apply_environment(
    data: &mut Table,
    environment: &BTreeMap<String, String>,
) -> Result<(), SettingsError> {
    for (section, fields) in SECTIONS {
        for field in *fields {
            let name = format!("{ENV_PREFIX}{section}_{field}").to_uppercase();
            if let Some(value) = nonblank(environment, &name) {
                let input = data
                    .entry((*section).to_owned())
                    .or_insert_with(|| Input::Table(Table::new()));
                let Input::Table(table) = input else {
                    return Err(SettingsError::Section { section });
                };
                table.insert((*field).to_owned(), Input::String(value.to_owned()));
            }
        }
    }
    if let Some(Input::Table(storage)) = data.get_mut("storage")
        && matches!(storage.get("backend"), Some(Input::String(backend)) if backend == "s3")
    {
        for (field, names) in [
            ("s3_access_key_id", &["AWS_ACCESS_KEY_ID"][..]),
            ("s3_secret_access_key", &["AWS_SECRET_ACCESS_KEY"][..]),
            (
                "s3_endpoint",
                &["AWS_ENDPOINT_URL_S3", "AWS_ENDPOINT_URL"][..],
            ),
            ("s3_region", &["AWS_REGION", "AWS_DEFAULT_REGION"][..]),
            ("bucket", &["S3_BUCKET"][..]),
            ("s3_path_style", &["S3_FORCE_PATH_STYLE"][..]),
        ] {
            if !storage.contains_key(field)
                && let Some(value) = names.iter().find_map(|name| nonblank(environment, name))
            {
                storage.insert(field.to_owned(), Input::String(value.to_owned()));
            }
        }
    }
    Ok(())
}

fn nonblank<'a>(environment: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    environment
        .get(name)
        .filter(|value| !value.trim().is_empty())
        .map(String::as_str)
}

struct Reader<'a> {
    section: &'static str,
    table: Option<&'a Table>,
    issues: &'a mut Vec<SettingsIssue>,
}

impl<'a> Reader<'a> {
    fn new(section: &'static str, data: &'a Table, issues: &'a mut Vec<SettingsIssue>) -> Self {
        let table = match data.get(section) {
            Some(Input::Table(table)) => Some(table),
            Some(_) => {
                issues.push(SettingsIssue::new(
                    section,
                    "Input should be a valid dictionary",
                ));
                None
            }
            None => None,
        };
        Self {
            section,
            table,
            issues,
        }
    }

    fn value(&self, field: &str) -> Option<&Input> {
        self.table.and_then(|table| table.get(field))
    }
    fn issue(&mut self, field: &str, reason: &str) {
        self.issues.push(SettingsIssue {
            location: vec![self.section.to_owned(), field.to_owned()],
            reason: reason.to_owned(),
        });
    }

    fn string(&mut self, field: &str, default: &str) -> String {
        match self.value(field) {
            None => default.to_owned(),
            Some(Input::String(value)) => value.clone(),
            Some(_) => {
                self.issue(field, "Input should be a valid string");
                default.to_owned()
            }
        }
    }

    fn optional(&mut self, field: &str) -> Option<String> {
        match self.value(field) {
            None => None,
            Some(Input::String(value)) => Some(value.clone()),
            Some(_) => {
                self.issue(field, "Input should be a valid string");
                None
            }
        }
    }

    fn secret(&mut self, field: &str) -> Option<Secret> {
        self.optional(field).map(Secret)
    }

    fn integer(
        &mut self,
        field: &str,
        default: i64,
        minimum: Option<i64>,
        maximum: Option<i64>,
    ) -> SettingsInt {
        let value = match self.value(field) {
            None => BigInt::from(default),
            Some(input) => {
                if let Some(value) = integer(input) {
                    value
                } else {
                    self.issue(field, "Input should be a valid integer");
                    return SettingsInt::from(default);
                }
            }
        };
        if minimum.is_some_and(|minimum| value < BigInt::from(minimum)) {
            self.issue(
                field,
                if minimum == Some(1) {
                    "Input should be greater than 0"
                } else {
                    "Input should be greater than or equal to the lower bound"
                },
            );
        }
        if maximum.is_some_and(|maximum| value > BigInt::from(maximum)) {
            self.issue(
                field,
                "Input should be less than or equal to the upper bound",
            );
        }
        SettingsInt(value)
    }

    fn float(&mut self, field: &str, default: f64) -> f64 {
        let value = match self.value(field) {
            None => default,
            Some(Input::Float(value)) => *value,
            Some(Input::Integer(value)) => match num_traits::ToPrimitive::to_f64(value) {
                Some(value) if value.is_finite() => value,
                _ => {
                    self.issue(field, "Input should be a valid number");
                    return default;
                }
            },
            Some(Input::String(value)) => {
                if let Some(value) = float_string(value) {
                    value
                } else {
                    self.issue(field, "Input should be a valid number");
                    return default;
                }
            }
            Some(_) => {
                self.issue(field, "Input should be a valid number");
                return default;
            }
        };
        if !value.is_finite() || value <= 0.0 {
            self.issue(field, "Input should be greater than 0");
        }
        value
    }

    fn boolean(&mut self, field: &str, default: bool) -> bool {
        let value = match self.value(field) {
            None => return default,
            Some(Input::Boolean(value)) => Some(*value),
            Some(Input::String(value)) => match value.to_ascii_lowercase().as_str() {
                "0" | "off" | "f" | "false" | "n" | "no" => Some(false),
                "1" | "on" | "t" | "true" | "y" | "yes" => Some(true),
                _ => None,
            },
            Some(_) => None,
        };
        value.unwrap_or_else(|| {
            self.issue(field, "Input should be a valid boolean");
            default
        })
    }

    fn list(&mut self, field: &str) -> Vec<String> {
        match self.value(field) {
            None => Vec::new(),
            Some(Input::String(value)) => value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect(),
            Some(Input::Array(values)) => {
                let values = values.clone();
                let mut strings = Vec::new();
                for (index, value) in values.into_iter().enumerate() {
                    if let Input::String(value) = value {
                        strings.push(value);
                    } else {
                        self.issues.push(SettingsIssue {
                            location: vec![
                                self.section.to_owned(),
                                field.to_owned(),
                                index.to_string(),
                            ],
                            reason: "Input should be a valid string".to_owned(),
                        });
                    }
                }
                strings
            }
            Some(_) => {
                self.issue(field, "Input should be a valid list");
                Vec::new()
            }
        }
    }

    fn finish(&mut self) {
        let fields = SECTIONS
            .iter()
            .find(|(section, _)| *section == self.section)
            .map_or(&[][..], |(_, fields)| *fields);
        if let Some(table) = self.table {
            for field in table
                .keys()
                .filter(|field| !fields.contains(&field.as_str()))
            {
                self.issue(field, "Extra inputs are not permitted");
            }
        }
    }
}

fn integer(input: &Input) -> Option<BigInt> {
    match input {
        Input::Integer(value) => value.to_i64().map(BigInt::from),
        Input::String(value) => value.trim().parse::<i64>().ok().map(BigInt::from),
        _ => None,
    }
}

fn float_string(value: &str) -> Option<f64> {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

// Keep the field mapping in Python declaration order for aggregated validation.
#[allow(clippy::too_many_lines)]
pub(super) fn build(data: &Table) -> Result<Settings, SettingsError> {
    let mut issues = Vec::new();
    let mut r = Reader::new("database", data, &mut issues);
    let provider = match r.value("provider") {
        None => DatabaseProvider::Url,
        Some(Input::String(value)) if value == "url" => DatabaseProvider::Url,
        Some(Input::String(value)) if value == "managed" => DatabaseProvider::Managed,
        Some(_) => {
            r.issue("provider", "Input should be 'url' or 'managed'");
            DatabaseProvider::Url
        }
    };
    if data.get("database").is_none() {
        r.issues
            .push(SettingsIssue::new("database", "Field required"));
    } else if r.table.is_some() && provider == DatabaseProvider::Url && r.value("url").is_none() {
        r.issue("url", "Field required");
    }
    let database = DatabaseSettings {
        url: r.string("url", ""),
        pool_min_size: r.integer("pool_min_size", 1, None, None),
        pool_max_size: r.integer("pool_max_size", 10, None, None),
        provider,
        data_dir: r.optional("data_dir"),
        postgres_bin_dir: r.optional("postgres_bin_dir"),
        postgres_cache_dir: r.optional("postgres_cache_dir"),
    };
    r.finish();
    let mut r = Reader::new("server", data, &mut issues);
    let server = ServerSettings {
        public_base_url: r.string("public_base_url", "http://localhost:8000"),
    };
    r.finish();
    let start = issues.len();
    let mut r = Reader::new("auth", data, &mut issues);
    let auth = AuthSettings {
        oidc_issuer: r.optional("oidc_issuer"),
        oidc_client_id: r.optional("oidc_client_id"),
        oidc_client_secret: r.secret("oidc_client_secret"),
        oidc_scopes: r.string("oidc_scopes", "openid email profile"),
        allowed_email_domains: r.list("allowed_email_domains"),
        bootstrap_admin_emails: r.list("bootstrap_admin_emails"),
        session_ttl_hours: r.integer("session_ttl_hours", 168, Some(1), Some(i64::MAX / 3600)),
        cookie_secure: r.boolean("cookie_secure", true),
        personal_token_max_days: r.integer("personal_token_max_days", 365, Some(1), None),
    };
    r.finish();
    if issues.len() == start {
        let fields = [
            auth.oidc_issuer.as_ref().is_some_and(|v| !v.is_empty()),
            auth.oidc_client_id.as_ref().is_some_and(|v| !v.is_empty()),
            auth.oidc_client_secret
                .as_ref()
                .is_some_and(|v| !v.is_empty()),
        ];
        if fields.iter().any(|set| *set) && !fields.iter().all(|set| *set) {
            issues.push(SettingsIssue::new(
                "auth",
                "oidc_issuer, oidc_client_id and oidc_client_secret go together",
            ));
        }
    }
    let start = issues.len();
    let mut r = Reader::new("storage", data, &mut issues);
    let backend = match r.value("backend") {
        None => StorageBackend::Local,
        Some(Input::String(value)) if value == "local" => StorageBackend::Local,
        Some(Input::String(value)) if value == "s3" => StorageBackend::S3,
        Some(_) => {
            r.issue("backend", "Input should be 'local' or 's3'");
            StorageBackend::Local
        }
    };
    let local_root = r.string("local_root", ".dev/objects");
    let bucket = r.string("bucket", "local");
    if !bucket_valid(&bucket) {
        r.issue("bucket", "String should match bucket pattern");
    }
    let storage = StorageSettings {
        backend,
        local_root,
        bucket,
        upload_ttl_minutes: r.integer("upload_ttl_minutes", 60, Some(1), None),
        max_object_bytes: r.integer("max_object_bytes", 50 * (1 << 30), Some(1), None),
        max_stream_seconds: r.integer("max_stream_seconds", 6 * 3600, Some(1), None),
        validate_json_max_bytes: r.integer(
            "validate_json_max_bytes",
            64 * (1 << 20),
            Some(1),
            None,
        ),
        max_concurrent_validations: r.integer("max_concurrent_validations", 2, Some(1), None),
        s3_endpoint: r.optional("s3_endpoint"),
        s3_public_endpoint: r.optional("s3_public_endpoint"),
        s3_region: r.string("s3_region", "us-east-1"),
        s3_access_key_id: r.optional("s3_access_key_id"),
        s3_secret_access_key: r.secret("s3_secret_access_key"),
        s3_path_style: r.boolean("s3_path_style", false),
        s3_prefix: r.string("s3_prefix", ""),
        s3_presign_ttl_seconds: r.integer(
            "s3_presign_ttl_seconds",
            900,
            Some(1),
            Some(7 * 24 * 3600),
        ),
        s3_multipart_threshold_bytes: r.integer(
            "s3_multipart_threshold_bytes",
            256 * (1 << 20),
            Some(1),
            Some(5 * (1 << 30)),
        ),
        s3_part_size_bytes: r.integer(
            "s3_part_size_bytes",
            64 * (1 << 20),
            Some(5 * (1 << 20)),
            Some(5 * (1 << 30)),
        ),
    };
    // String grammar validation does not normalize the configured prefix.
    if storage.s3_prefix.chars().count() > 256 {
        r.issue("s3_prefix", "String should have at most 256 characters");
    } else if !prefix_valid(&storage.s3_prefix) {
        r.issue("s3_prefix", "String should match prefix pattern");
    }
    r.finish();
    if issues.len() == start && storage.backend == StorageBackend::S3 {
        if storage
            .s3_access_key_id
            .as_ref()
            .is_none_or(std::string::String::is_empty)
            || storage
                .s3_secret_access_key
                .as_ref()
                .is_none_or(super::types::Secret::is_empty)
        {
            issues.push(SettingsIssue::new(
                "storage",
                "the s3 backend needs s3_access_key_id and s3_secret_access_key",
            ));
        } else if (&storage.max_object_bytes.0 + &storage.s3_part_size_bytes.0 - 1)
            / &storage.s3_part_size_bytes.0
            > BigInt::from(S3_MAX_PARTS)
        {
            issues.push(SettingsIssue::new(
                "storage",
                "max_object_bytes needs more than 10000 parts of s3_part_size_bytes",
            ));
        }
    }
    let mut r = Reader::new("leases", data, &mut issues);
    let leases = LeaseSettings {
        ttl_seconds: r.integer("ttl_seconds", 900, Some(1), None),
        job_ttl_seconds: r.integer("job_ttl_seconds", 900, Some(1), None),
        job_overhead_seconds: r.integer("job_overhead_seconds", 300, Some(0), None),
        stalled_verification_seconds: r.integer(
            "stalled_verification_seconds",
            3600,
            Some(0),
            None,
        ),
    };
    r.finish();
    let mut r = Reader::new("sweeps", data, &mut issues);
    let sweeps = SweepSettings {
        enabled: r.boolean("enabled", true),
        interval_seconds: r.float("interval_seconds", 30.0),
        batch_size: r.integer("batch_size", 100, Some(1), None),
        batches_per_run: r.integer("batches_per_run", 10, Some(1), None),
        timeout_seconds: r.float("timeout_seconds", 120.0),
        connect_timeout_seconds: r.integer("connect_timeout_seconds", 10, Some(1), None),
        lock_timeout_seconds: r.float("lock_timeout_seconds", 5.0),
        statement_timeout_seconds: r.float("statement_timeout_seconds", 60.0),
    };
    r.finish();
    let mut r = Reader::new("mcp", data, &mut issues);
    let mcp = McpSettings {
        max_request_bytes: r.integer("max_request_bytes", 1 << 20, Some(1), None),
        max_result_bytes: r.integer("max_result_bytes", 1 << 20, Some(1), None),
    };
    r.finish();
    let mut r = Reader::new("web", data, &mut issues);
    let web = WebSettings {
        dist_dir: r.optional("dist_dir"),
    };
    r.finish();
    let mut r = Reader::new("testing", data, &mut issues);
    let testing = TestingSettings {
        enabled: r.boolean("enabled", false),
    };
    r.finish();
    for field in data
        .keys()
        .filter(|field| !SECTIONS.iter().any(|(section, _)| field == section))
    {
        issues.push(SettingsIssue {
            location: vec![field.clone()],
            reason: "Extra inputs are not permitted".to_owned(),
        });
    }
    if issues.is_empty() && testing.enabled && !testing_loopback(&server.public_base_url) {
        issues.push(SettingsIssue::new(
            "",
            "testing.enabled requires a loopback server.public_base_url",
        ));
    }
    if !issues.is_empty() {
        return Err(SettingsError::Invalid { issues });
    }
    // Own all values; no environment/file references survive startup.
    Ok(Settings {
        database,
        server,
        auth,
        storage,
        leases,
        sweeps,
        mcp,
        web,
        testing,
    })
}

fn bucket_valid(value: &str) -> bool {
    (1..=63).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}
fn prefix_valid(value: &str) -> bool {
    value.is_empty()
        || value.strip_suffix('/').is_some_and(|value| {
            value.split('/').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
            })
        })
}

fn testing_loopback(value: &str) -> bool {
    // url::Url normalizes abbreviated/octal IPv4, which Python ip_address rejects.
    let normalized: String = value
        .trim_start_matches(|c: char| c <= '\u{20}')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let Some((scheme, rest)) = normalized.split_once("://") else {
        return false;
    };
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return false;
    }
    let bracketed = authority.starts_with('[');
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let Some((host, suffix)) = rest.split_once(']') else {
            return false;
        };
        if suffix.is_empty() {
            (host, None)
        } else if let Some(port) = suffix.strip_prefix(':') {
            (host, Some(port))
        } else {
            return false;
        }
    } else if let Some((host, port)) = authority.split_once(':') {
        (host, Some(port))
    } else {
        (authority, None)
    };
    if port.is_some_and(|port| {
        !port.is_empty()
            && (!port.bytes().all(|b| b.is_ascii_digit())
                || port.trim_start_matches('0').parse::<u16>().is_err()
                    && !port.bytes().all(|b| b == b'0'))
    }) {
        return false;
    }
    if bracketed {
        let address = if let Some((address, scope)) = host.split_once('%') {
            if scope.is_empty() || scope.contains('%') {
                return false;
            }
            address
        } else {
            host
        };
        address
            .parse::<std::net::Ipv6Addr>()
            .is_ok_and(|address| address.is_loopback())
    } else {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    }
}
