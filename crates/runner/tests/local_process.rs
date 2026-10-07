#![forbid(unsafe_code)]

use cannery_runner::local_process::{LocalError, LocalProcessBackend, PreparedRequest};
use std::{error::Error, ffi::OsStr, path::PathBuf};

fn text(value: &str) -> String {
    String::from(value)
}

#[test]
fn acknowledgement_and_interpreter_are_explicit() -> Result<(), Box<dyn Error>> {
    assert!(matches!(
        LocalProcessBackend::new("binary".into(), "interpreter".into(), false),
        Err(LocalError::Acknowledgement)
    ));
    let backend = LocalProcessBackend::new("binary".into(), "/owned/interpreter".into(), true)?;
    assert_eq!(
        backend.argv(&[text("python"), text("-c")], &[text("a b")])?,
        vec![text("/owned/interpreter"), text("-c"), text("a b")]
    );
    assert_eq!(
        backend.argv(&[text("python3.13")], &[])?,
        vec![text("python3.13")]
    );
    assert_eq!(backend.argv(&[], &[]), Err(LocalError::EmptyCommand));
    Ok(())
}

#[test]
fn minimal_environment_preserves_dict_overwrite_order() -> Result<(), Box<dyn Error>> {
    let root = PathBuf::from("/owned/root");
    let declared = [
        (text("ZETA"), text("first")),
        (text("HOME"), text("forged")),
        (text("ZETA"), text("last")),
    ];
    let environment = LocalProcessBackend::environment(&root, &declared, Some(OsStr::new("")))?;
    assert_eq!(
        environment,
        vec![
            (text("ZETA"), text("last")),
            (text("HOME"), text("/owned/root")),
            (text("PATH"), text("")),
            (text("LANG"), text("C.UTF-8")),
            (text("TMPDIR"), text("/owned/root/tmp")),
            (text("PYTHONDONTWRITEBYTECODE"), text("1")),
            (text("CR_ROOT"), text("/owned/root"))
        ]
    );
    let fallback = LocalProcessBackend::environment(&root, &[], None)?;
    assert_eq!(fallback[0], (text("PATH"), text("/bin:/usr/bin")));
    Ok(())
}

#[test]
fn request_and_error_diagnostics_do_not_disclose_arguments_or_paths() {
    let request = PreparedRequest {
        command: vec![text("private command")],
        args: vec![text("private argument")],
        env: vec![(text("private key"), text("private value"))],
        root: "/private/root".into(),
        code_dir: "/private/code".into(),
        log_path: "/private/log".into(),
        deadline_seconds: f64::NAN.into(),
    };
    assert_eq!(format!("{request:?}"), "PreparedRequest([redacted])");
    let error = LocalError::Io { errno: Some(13) };
    assert!(error.source().is_none());
    assert_eq!(
        error.to_string(),
        "local process filesystem operation failed"
    );
}

#[test]
fn process_text_boundaries_reject_non_utf8_native_paths() -> Result<(), Box<dyn Error>> {
    use std::os::unix::ffi::OsStrExt;

    let invalid = OsStr::from_bytes(b"/private/\xff");
    let backend = LocalProcessBackend::new("binary".into(), invalid.into(), true)?;
    assert_eq!(
        backend.argv(&[text("python")], &[]),
        Err(LocalError::Encoding)
    );
    assert_eq!(
        LocalProcessBackend::environment(&PathBuf::from(invalid), &[], None),
        Err(LocalError::Encoding)
    );
    assert_eq!(
        LocalProcessBackend::environment(&PathBuf::from("/owned/root"), &[], Some(invalid)),
        Err(LocalError::Encoding)
    );
    assert!(!LocalError::Encoding.to_string().contains("private"));
    Ok(())
}
