//! Replay fresh Python settings recipes through the production Rust loader.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cannery_core::settings::{Settings, SettingsError, SettingsInt, load_settings};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Work(PathBuf);
impl Drop for Work {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("settings parity: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: settings-parity REFERENCE.json")?;
    let reference: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let cases = reference["cases"]
        .as_array()
        .ok_or("reference cases are absent")?;
    if cases.is_empty() {
        return Err("reference cases are empty".into());
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "cannery-settings-parity-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir(&directory)?;
    let work = Work(directory);
    for (index, case) in cases.iter().enumerate() {
        replay(case, &work.0, index)?;
    }
    println!("Compared {} settings recipes: zero mismatches", cases.len());
    Ok(())
}

fn replay(case: &Value, parent: &Path, index: usize) -> Result<()> {
    let directory = parent.join(index.to_string());
    std::fs::create_dir(&directory)?;
    let root = directory.to_str().ok_or("work directory is not UTF-8")?;
    let expand = |value: &str| value.replace("<WORK>", root);
    let environment = case["environment"]
        .as_object()
        .ok_or("reference environment is absent")?
        .iter()
        .map(|(name, value)| {
            Ok((
                name.clone(),
                expand(value.as_str().ok_or("environment value is not a string")?),
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let selected = case["selected_path"]
        .as_str()
        .map(expand)
        .map(PathBuf::from);
    if case["file_exists"].as_bool() == Some(true) {
        let selected = selected.as_ref().ok_or("reference file has no path")?;
        // The maintained corpus's files live in its own isolated work directory.
        if !selected.starts_with(&directory) {
            return Err("reference path escapes work directory".into());
        }
        std::fs::create_dir_all(selected.parent().ok_or("reference file parent absent")?)?;
        std::fs::write(
            selected,
            expand(case["file"].as_str().ok_or("reference TOML is absent")?),
        )?;
    }
    let explicit = if case["explicit_path"].as_bool() == Some(true) {
        selected.as_deref()
    } else {
        None
    };
    let expected = &case["expected"];
    match (
        load_settings(explicit, &environment),
        expected["verdict"].as_str(),
    ) {
        (Ok(settings), Some("valid")) => {
            let mut actual = snapshot(&settings)?;
            normalize_work(&mut actual, root);
            if let Some(path) = difference(&actual, &expected["settings"], "settings") {
                return Err(format!("recipe {index}: mismatch at {path}").into());
            }
            if settings.auth.oidc_enabled()
                != expected["oidc_enabled"]
                    .as_bool()
                    .ok_or("OIDC result absent")?
            {
                return Err(format!("recipe {index}: OIDC enabled mismatch").into());
            }
            if let Some(hash) = expected["s3_secret_sha256"].as_str() {
                let secret = settings
                    .storage
                    .s3_secret_access_key
                    .as_ref()
                    .ok_or("S3 secret absent")?;
                if format!("{:x}", Sha256::digest(secret.expose().as_bytes())) != hash {
                    return Err(format!("recipe {index}: S3 secret mismatch").into());
                }
                if !secret.is_empty() && format!("{:?}", settings.storage).contains(secret.expose())
                {
                    return Err("secret disclosed in debug".into());
                }
            }
        }
        (Err(error), Some("error")) => compare_error(&error, expected, index)?,
        _ => return Err(format!("recipe {index}: verdict mismatch").into()),
    }
    Ok(())
}

fn compare_error(error: &SettingsError, expected: &Value, index: usize) -> Result<()> {
    let message = expected["message"]
        .as_str()
        .ok_or("reference error message absent")?;
    let compatible = match error {
        SettingsError::Read { .. } => message.starts_with("cannot read settings file"),
        SettingsError::Toml { .. } => message.starts_with("invalid TOML"),
        SettingsError::Section { .. } => message.starts_with("settings section"),
        SettingsError::Invalid { issues } => {
            let expected_locations: std::collections::BTreeSet<_> = message
                .lines()
                .skip(1)
                .filter_map(|line| line.split_once(": ").map(|(location, _)| location))
                .collect();
            let actual_locations: std::collections::BTreeSet<_> = issues
                .iter()
                .map(|issue| {
                    if issue.location.is_empty() {
                        "settings".to_owned()
                    } else {
                        issue.location.join(".")
                    }
                })
                .collect();
            message.starts_with("invalid settings:")
                && actual_locations
                    .iter()
                    .map(String::as_str)
                    .collect::<std::collections::BTreeSet<_>>()
                    == expected_locations
        }
    };
    if !compatible {
        return Err(format!("recipe {index}: error category or locations mismatch").into());
    }
    Ok(())
}

fn integer(value: &SettingsInt) -> Result<Value> {
    if let Ok(value) = value.to_i64("reference") {
        return Ok(json!(value));
    }
    if let Ok(value) = value.to_u64("reference") {
        return Ok(json!(value));
    }
    // The current reference uses ordinary JSON integers. Never approximate a larger integer.
    Err("reference integer exceeds lossless JSON transport; extend reference format".into())
}

fn normalize_work(value: &mut Value, root: &str) {
    match value {
        Value::String(text) => *text = text.replace(root, "<WORK>"),
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| normalize_work(value, root)),
        Value::Object(values) => values
            .values_mut()
            .for_each(|value| normalize_work(value, root)),
        _ => {}
    }
}

fn snapshot(settings: &Settings) -> Result<Value> {
    let database = &settings.database;
    let auth = &settings.auth;
    let storage = &settings.storage;
    let leases = &settings.leases;
    let sweeps = &settings.sweeps;
    Ok(json!({
        "database":{"url":database.url,"pool_min_size":integer(&database.pool_min_size)?,"pool_max_size":integer(&database.pool_max_size)?},
        "server":{"public_base_url":settings.server.public_base_url},
        "auth":{"oidc_issuer":auth.oidc_issuer,"oidc_client_id":auth.oidc_client_id,"oidc_client_secret":auth.oidc_client_secret.as_ref().map(cannery_core::settings::Secret::expose),"oidc_scopes":auth.oidc_scopes,"allowed_email_domains":auth.allowed_email_domains,"bootstrap_admin_emails":auth.bootstrap_admin_emails,"session_ttl_hours":integer(&auth.session_ttl_hours)?,"cookie_secure":auth.cookie_secure,"personal_token_max_days":integer(&auth.personal_token_max_days)?},
        "storage":{"backend":storage.backend.as_str(),"local_root":storage.local_root,"bucket":storage.bucket,"upload_ttl_minutes":integer(&storage.upload_ttl_minutes)?,"max_object_bytes":integer(&storage.max_object_bytes)?,"max_stream_seconds":integer(&storage.max_stream_seconds)?,"validate_json_max_bytes":integer(&storage.validate_json_max_bytes)?,"max_concurrent_validations":integer(&storage.max_concurrent_validations)?,"s3_endpoint":storage.s3_endpoint,"s3_public_endpoint":storage.s3_public_endpoint,"s3_region":storage.s3_region,"s3_access_key_id":storage.s3_access_key_id,"s3_secret_access_key":storage.s3_secret_access_key.as_ref().map(|secret|if secret.is_empty(){""}else{"**********"}),"s3_path_style":storage.s3_path_style,"s3_prefix":storage.s3_prefix,"s3_presign_ttl_seconds":integer(&storage.s3_presign_ttl_seconds)?,"s3_multipart_threshold_bytes":integer(&storage.s3_multipart_threshold_bytes)?,"s3_part_size_bytes":integer(&storage.s3_part_size_bytes)?},
        "leases":{"ttl_seconds":integer(&leases.ttl_seconds)?,"job_ttl_seconds":integer(&leases.job_ttl_seconds)?,"job_overhead_seconds":integer(&leases.job_overhead_seconds)?,"stalled_evaluation_seconds":integer(&leases.stalled_evaluation_seconds)?},
        "sweeps":{"enabled":sweeps.enabled,"interval_seconds":sweeps.interval_seconds,"batch_size":integer(&sweeps.batch_size)?,"batches_per_run":integer(&sweeps.batches_per_run)?,"timeout_seconds":sweeps.timeout_seconds,"connect_timeout_seconds":integer(&sweeps.connect_timeout_seconds)?,"lock_timeout_seconds":sweeps.lock_timeout_seconds,"statement_timeout_seconds":sweeps.statement_timeout_seconds},
        "mcp":{"max_request_bytes":integer(&settings.mcp.max_request_bytes)?,"max_result_bytes":integer(&settings.mcp.max_result_bytes)?},
        "web":{"dist_dir":settings.web.dist_dir},"testing":{"enabled":settings.testing.enabled}
    }))
}

fn difference(actual: &Value, expected: &Value, path: &str) -> Option<String> {
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => {
            if a.keys().collect::<Vec<_>>() != e.keys().collect::<Vec<_>>() {
                return Some(path.to_owned());
            }
            a.iter()
                .find_map(|(key, value)| difference(value, &e[key], &format!("{path}.{key}")))
        }
        (Value::Array(a), Value::Array(e)) if a.len() == e.len() => a
            .iter()
            .zip(e)
            .enumerate()
            .find_map(|(index, (a, e))| difference(a, e, &format!("{path}.{index}"))),
        _ if actual == expected => None,
        _ => Some(path.to_owned()),
    }
}
