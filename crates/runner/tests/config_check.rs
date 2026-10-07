//! Actual installed preflight: local files are read, but no services or runtime state are used.
#![forbid(unsafe_code)]
use std::{error::Error, fs, io, net::TcpListener, os::unix::fs::PermissionsExt, process::Command};

#[test]
#[ignore = "requires the installed CANNERY_NATIVE_CLI artifact"]
fn installed_runner_check_config_is_offline() -> Result<(), Box<dyn Error>> {
    let binary = std::env::var_os("CANNERY_NATIVE_CLI").ok_or("CLI artifact required")?;
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce)?;
    let directory = std::env::temp_dir().join(format!(
        "cr-check-config-{}",
        uuid::Uuid::from_bytes(nonce).simple()
    ));
    fs::create_dir(&directory)?;
    let result = (|| -> Result<(), Box<dyn Error>> {
        let peer = TcpListener::bind("127.0.0.1:0")?;
        peer.set_nonblocking(true)?;
        let api = format!("http://{}", peer.local_addr()?);
        for (file, token) in [
            ("tester", "synthetic-tester-private"),
            ("evaluator", "synthetic-evaluator-private"),
            ("cluster", "synthetic-cluster-private"),
        ] {
            let path = directory.join(file);
            fs::write(&path, token)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        fs::write(
            directory.join("policy.json"),
            r#"{"schema_version":"0.2","evaluator":{"id":"stock-evaluator","revision":"v1"},"gates":[{"id":"quality","metric":"mrr","split":"dev","statistic":"value","compare":"control","op":">=","min_delta":0}],"baselines":[]}"#,
        )?;
        let text = format!(
            "api_url = {api:?}\nproject = \"fixture\"\ndata_root = \"absent-data\"\nwork_root = \"absent-work\"\ncache_root = \"absent-cache\"\n[launcher]\ntype = \"kubernetes\"\nrunner_id = \"fixture-runner\"\nk8s_namespace = \"isolated-runner\"\nk8s_api_url = {api:?}\nk8s_token_file = \"cluster\"\n[[kinds]]\nkind = \"test\"\ntoken_file = \"tester\"\n[[kinds]]\nkind = \"eval\"\ntoken_file = \"evaluator\"\npolicy = \"policy.json\"\n"
        );
        let config = directory.join("runner.toml");
        fs::write(&config, &text)?;
        let run = |extra: &[&str]| {
            Command::new(&binary)
                .current_dir(&directory)
                .arg("runner")
                .arg("--config")
                .arg(&config)
                .arg("--check-config")
                .args(extra)
                .output()
        };
        let valid = run(&["--k8s-namespace-policy-acknowledged"])?;
        assert!(
            valid.status.success(),
            "preflight rejected valid configuration: {}",
            String::from_utf8_lossy(&valid.stderr)
        );
        for launcher in ["docker", "local"] {
            let local = text.replace(
                "type = \"kubernetes\"",
                &format!("type = {launcher:?}\nstep_root = \"absent-step\""),
            );
            fs::write(&config, local)?;
            assert!(run(&[])?.status.success());
        }
        fs::write(&config, &text)?;
        for extra in [
            &[][..],
            &["--k8s-namespace-policy-acknowledged", "--once"][..],
            &[
                "--k8s-namespace-policy-acknowledged",
                "--default-step-memory",
                "0",
            ][..],
        ] {
            let failed = run(extra)?;
            assert_eq!(failed.status.code(), Some(2));
        }
        fs::set_permissions(directory.join("tester"), fs::Permissions::from_mode(0o644))?;
        let failed = run(&["--k8s-namespace-policy-acknowledged"])?;
        assert_eq!(failed.status.code(), Some(2));
        assert!(!String::from_utf8_lossy(&failed.stderr).contains("synthetic-tester-private"));
        fs::set_permissions(directory.join("tester"), fs::Permissions::from_mode(0o600))?;
        for suffix in [
            "\n[github]\napp_id = \"1\"\n",
            "\n[github]\napi_url = \"http://private-user:private-password@127.0.0.1\"\n",
            "\n[github]\ntoken_file = \"missing-github-token\"\n",
        ] {
            fs::write(&config, format!("{text}{suffix}"))?;
            let failure = run(&["--k8s-namespace-policy-acknowledged"])?;
            assert_eq!(failure.status.code(), Some(2));
            assert!(!String::from_utf8_lossy(&failure.stderr).contains("private-password"));
        }
        fs::write(&config, &text)?;
        fs::write(directory.join("policy.json"), "{}")?;
        assert_eq!(
            run(&["--k8s-namespace-policy-acknowledged"])?.status.code(),
            Some(2)
        );
        assert!(matches!(peer.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
        for path in ["absent-data", "absent-work", "absent-cache", "absent-step"] {
            assert!(!directory.join(path).exists());
        }
        Ok(())
    })();
    fs::remove_dir_all(directory)?;
    result
}
