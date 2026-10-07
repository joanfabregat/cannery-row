//! Authored launcher and runtime credential contracts.
#![forbid(unsafe_code)]
use cannery_runner::{
    credentials::{TokenFileError, read_token_file},
    launcher::{self, ContractError, Manifest, Mount, Network, PosixPath, StepSpec},
};
use num_bigint::BigInt;
use num_rational::BigRational;
use std::{error::Error, fs, os::unix::fs::PermissionsExt, path::PathBuf};
type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

fn manifest() -> Manifest {
    Manifest {
        image: format!(
            "registry.example:5000/team/worker@sha256:{}",
            "a".repeat(64)
        ),
        command: vec!["worker".to_owned()],
        args: vec!["--test".to_owned()],
        env: vec![
            ("A".to_owned(), "first".to_owned()),
            ("B".to_owned(), "other".to_owned()),
            ("A".to_owned(), "last".to_owned()),
        ],
        cpu: Some("250m".to_owned()),
        memory: Some("1.5Ki".to_owned()),
        gpus: Some("2".to_owned()),
        network: Network::None,
    }
}

#[test]
fn ordinary_resource_quantities_are_exact_and_invalid_syntax_is_rejected() -> Result {
    for (text, numerator, denominator) in [
        ("250m", 1, 4),
        ("1.5", 3, 2),
        ("1Ki", 1024, 1),
        ("2Mi", 2_097_152, 1),
        ("0", 0, 1),
        ("2G", 2_000_000_000, 1),
    ] {
        assert_eq!(
            launcher::quantity(text)?,
            BigRational::new(BigInt::from(numerator), BigInt::from(denominator))
        );
    }
    for text in ["", "-1", "+1", ".5", "1.", "1e3", "１", "1GB", " 1", "1  "] {
        assert_eq!(launcher::quantity(text), Err(ContractError::Quantity));
    }
    Ok(())
}

#[test]
fn manifests_keep_commands_resources_environment_and_explicit_network_access() -> Result {
    let mut input = manifest();
    let mut step = StepSpec::from_manifest(&input, "job-1".to_owned(), "test".to_owned())?;
    assert_eq!(step.job_id, "job-1");
    assert_eq!(step.image, input.image);
    assert_eq!(step.command, input.command);
    assert_eq!(step.args, input.args);
    assert_eq!(
        step.env,
        [
            ("A".to_owned(), "last".to_owned()),
            ("B".to_owned(), "other".to_owned())
        ]
    );
    assert_eq!(
        step.resources.cpu,
        Some(BigRational::new(1.into(), 4.into()))
    );
    assert_eq!(step.resources.memory_bytes, Some(1536.into()));
    assert_eq!(step.resources.gpus, 2.into());
    assert!(!step.networked());
    step.open_network = true;
    assert!(step.networked());
    input.network = Network::Egress(vec!["api.example:443".to_owned()]);
    let step = StepSpec::from_manifest(&input, "job-2".to_owned(), "test".to_owned())?;
    assert_eq!(step.egress, ["api.example:443"]);
    assert!(step.networked());
    input.memory = Some("63.1".to_owned());
    assert_eq!(
        StepSpec::from_manifest(&input, "job-3".to_owned(), "test".to_owned())?
            .resources
            .memory_bytes,
        Some(64.into())
    );
    Ok(())
}

#[test]
fn pinned_images_and_mount_workdirs_preserve_application_constraints() -> Result {
    let digest = "a".repeat(64);
    assert_eq!(
        launcher::pinned_image(&format!(
            "registry.example:5000/team/worker:v1@sha256:{digest}"
        ))?,
        (
            "registry.example:5000/team/worker".to_owned(),
            format!("sha256:{digest}")
        )
    );
    for image in [
        "worker:latest".to_owned(),
        format!("worker@sha256:{}", "a".repeat(63)),
        format!("worker@sha256:{}", "G".repeat(64)),
    ] {
        assert_eq!(launcher::pinned_image(&image), Err(ContractError::Image));
    }
    let root = PathBuf::from("/runner/job");
    assert_eq!(
        launcher::host_path(&root, "/cr/code/file")?,
        root.join("code/file")
    );
    assert!(launcher::host_path(&root, "/outside/file").is_err());
    let mut step = StepSpec::from_manifest(&manifest(), "job".to_owned(), "test".to_owned())?;
    let mount = Mount::new(root.join("code"), PosixPath::new("/cr/code"), true)?;
    assert_eq!(mount.source(), root.join("code"));
    assert!(Mount::new(PathBuf::from("relative"), PosixPath::new("/cr/code"), true).is_err());
    assert!(Mount::new(root.clone(), PosixPath::new("/outside"), true).is_err());
    step.mounts.push(mount.clone());
    step.workdir = Some(PosixPath::new("/cr/code/project"));
    step.validate()?;
    step.workdir = Some(PosixPath::new("/outside"));
    assert!(step.validate().is_err());
    step.mounts.push(mount);
    assert_eq!(step.validate(), Err(ContractError::DuplicateMount));
    Ok(())
}

struct TokenDirectory(PathBuf);
impl TokenDirectory {
    fn new() -> Result<Self> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("cannery-credential-{}-{nonce}", std::process::id()));
        fs::create_dir(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        Ok(Self(path))
    }
}
impl Drop for TokenDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn runtime_credentials_require_private_utf8_files_and_redact_secrets() -> Result {
    let directory = TokenDirectory::new()?;
    let path = directory.0.join("token");
    fs::write(&path, " \r\nsynthetic-test-token\r\n\t")?;
    for mode in [0o400, 0o600] {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
        let secret = read_token_file(&path)?;
        assert_eq!(secret.expose(), "synthetic-test-token");
        assert!(!format!("{secret:?}").contains("synthetic-test-token"));
    }
    for mode in [0o640, 0o604, 0o644] {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
        assert!(matches!(
            read_token_file(&path),
            Err(TokenFileError::Permissions)
        ));
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    fs::write(&path, [0xff])?;
    assert!(matches!(
        read_token_file(&path),
        Err(TokenFileError::Encoding)
    ));
    fs::write(&path, " \r\n\t")?;
    assert!(matches!(read_token_file(&path), Err(TokenFileError::Empty)));
    assert!(matches!(
        read_token_file(&directory.0),
        Err(TokenFileError::NotRegular)
    ));
    assert!(read_token_file(&directory.0.join("missing")).is_err());
    Ok(())
}
