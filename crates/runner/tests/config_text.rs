//! Authored UTF-8 configuration loading and sanitized failures.
use cannery_runner::{
    config::{
        self, ErrorKind, EvaluationPolicy, NativePathResolver, PolicyLoadError, PolicyLoader,
    },
    launcher::PosixPath,
};
use serde_json::json;
use std::{
    cell::Cell,
    error::Error,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

struct Policies(Cell<usize>);
impl PolicyLoader for Policies {
    fn load(&self, _: &PosixPath) -> Result<EvaluationPolicy, PolicyLoadError> {
        self.0.set(self.0.get() + 1);
        Err(PolicyLoadError::Configuration)
    }
}
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn utf8_files_load_and_invalid_text_is_refused_before_policy_loading() -> Result<(), Box<dyn Error>>
{
    let directory = Directory(std::env::temp_dir().join(format!(
        "cannery-config-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    )));
    fs::create_dir(&directory.0)?;
    let token = directory.0.join("tester.token");
    fs::write(&token, b"public-synthetic-tester")?;
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600))?;
    let path = directory.0.join("config.json");
    let policies = Policies(Cell::new(0));
    let valid = serde_json::to_vec(
        &json!({"api_url":"http://localhost:9010", "project":"fixture", "launcher":{"type":"local"}, "kinds":[{"kind":"test","name":"fixture","token_file":"tester.token"}]}),
    )?;
    fs::write(&path, &valid)?;
    let loaded = config::load_config_file(&path, &policies, &NativePathResolver, 128)?;
    assert_eq!(loaded.project, "fixture");
    assert_eq!(loaded.kinds.len(), 1);
    for (bytes, kind) in [
        (b"\xff".as_slice(), ErrorKind::Utf8),
        (b"\xed\xa0\x80".as_slice(), ErrorKind::Utf8),
        (br#"{"project":"\ud800"}"#.as_slice(), ErrorKind::Json),
        (b"{private-sentinel".as_slice(), ErrorKind::Json),
        (b"NaN".as_slice(), ErrorKind::Json),
        (b"Infinity".as_slice(), ErrorKind::Json),
    ] {
        fs::write(&path, bytes)?;
        let error = config::load_config_file(&path, &policies, &NativePathResolver, 128)
            .err()
            .ok_or("invalid configuration accepted")?;
        assert_eq!(error.kind, kind);
        assert!(!error.to_string().contains("private-sentinel"));
    }
    assert_eq!(policies.0.get(), 0);
    Ok(())
}
