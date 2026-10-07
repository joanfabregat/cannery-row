//! Installation settings with explicit precedence and checked native scalar types.
use crate::configuration_toml as input;
mod parse;
mod types;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

pub use types::*;

pub const ENV_PREFIX: &str = "CANNERY_";
pub const SETTINGS_PATH_ENV: &str = "CANNERY_SETTINGS";
pub const S3_MAX_PARTS: i64 = 10_000;

/// Environment variables understood by the installation settings loader.
#[must_use]
pub fn environment_names() -> Vec<String> {
    let mut names = vec![SETTINGS_PATH_ENV.to_owned()];
    for (section, fields) in parse::SECTIONS {
        names.extend(
            fields
                .iter()
                .map(|field| format!("{ENV_PREFIX}{section}_{field}").to_uppercase()),
        );
    }
    names.extend(
        [
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_ENDPOINT_URL_S3",
            "AWS_ENDPOINT_URL",
            "AWS_REGION",
            "AWS_DEFAULT_REGION",
            "S3_BUCKET",
            "S3_FORCE_PATH_STYLE",
        ]
        .map(str::to_owned),
    );
    names
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsIssue {
    pub location: Vec<String>,
    pub reason: String,
}

impl SettingsIssue {
    fn new(field: &str, reason: &str) -> Self {
        Self {
            location: if field.is_empty() {
                Vec::new()
            } else {
                field.split('.').map(str::to_owned).collect()
            },
            reason: reason.to_owned(),
        }
    }
}

// Error debug is also value-free: parser errors are never retained.
pub enum SettingsError {
    Read { path: PathBuf, reason: String },
    Toml { path: PathBuf, reason: String },
    Invalid { issues: Vec<SettingsIssue> },
    Section { section: &'static str },
}

impl fmt::Display for SettingsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, reason } => write!(
                formatter,
                "cannot read settings file {}: {reason}",
                path.display()
            ),
            Self::Toml { path, reason } => {
                write!(formatter, "invalid TOML in {}: {reason}", path.display())
            }
            Self::Invalid { issues } => {
                formatter.write_str("invalid settings:")?;
                for issue in issues {
                    let location = if issue.location.is_empty() {
                        "settings".to_owned()
                    } else {
                        issue.location.join(".")
                    };
                    write!(formatter, "\n{location}: {}", issue.reason)?;
                }
                Ok(())
            }
            Self::Section { section } => {
                write!(formatter, "settings section [{section}] must be a table")
            }
        }
    }
}

impl fmt::Debug for SettingsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { .. } => formatter.write_str("SettingsError::Read(..)"),
            Self::Toml { .. } => formatter.write_str("SettingsError::Toml(..)"),
            Self::Invalid { .. } => formatter.write_str("SettingsError::Invalid(..)"),
            Self::Section { .. } => formatter.write_str("SettingsError::Section(..)"),
        }
    }
}
impl std::error::Error for SettingsError {}

/// Explicit path wins over `CANNERY_SETTINGS`. The environment is isolated by the caller.
///
/// # Errors
/// Returns value-free read, TOML, section or aggregated field validation errors.
pub fn load_settings(
    explicit_path: Option<&Path>,
    environment: &BTreeMap<String, String>,
) -> Result<Settings, SettingsError> {
    let selected = explicit_path.map(Path::to_path_buf).or_else(|| {
        environment
            .get(SETTINGS_PATH_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    });
    let mut data = if let Some(path) = selected {
        let bytes = std::fs::read(&path).map_err(|error| SettingsError::Read {
            path: path.clone(),
            reason: error.kind().to_string(),
        })?;
        let source = std::str::from_utf8(&bytes).map_err(|_| SettingsError::Toml {
            path: path.clone(),
            reason: "settings file is not UTF-8".to_owned(),
        })?;
        input::document(source).map_err(|_| SettingsError::Toml {
            path,
            reason: "invalid TOML 1.0 document".to_owned(),
        })?
    } else {
        BTreeMap::new()
    };
    parse::apply_environment(&mut data, environment)?;
    parse::build(&data)
}
