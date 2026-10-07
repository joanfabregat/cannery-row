//! Installation settings and runner configuration exercised through the real CLI.
#![allow(clippy::too_many_lines)]
include!("support/settings_cli.rs");

#[test]
#[ignore = "requires the containerized conformance CLI"]
fn settings_files_bounds_and_secret_safe_errors() -> Result<()> {
    let work = Work::new()?;
    let good = work.file(
        "good.toml",
        "[database]\nurl='postgresql://unused.invalid/db'\n",
    )?;
    let good = good.to_str().ok_or("path encoding")?;
    for (text, fragment) in [
        ("", "database"),
        (
            "[database]\nurl='postgresql://unused.invalid/db'\ntypo=1\n",
            "database.typo",
        ),
        ("[database\n", "invalid TOML"),
        ("database='not-a-table'\n", "database"),
        (
            "[database]\nurl='postgresql://unused.invalid/db'\n[auth]\noidc_issuer='http://127.0.0.1:9011'\n",
            "auth",
        ),
        (
            "[database]\nurl='postgresql://unused.invalid/db'\n[storage]\nbackend='s3'\n",
            "s3_access_key_id and s3_secret_access_key",
        ),
    ] {
        let path = work.file("bad.toml", text)?;
        refused(
            &run(
                fragment,
                &["--settings", path.to_str().ok_or("path encoding")?, "serve"],
                &[],
            )?,
            1,
            fragment,
        );
    }
    let missing = work.0.join("missing.toml");
    refused(
        &run(
            "unreadable file",
            &[
                "--settings",
                missing.to_str().ok_or("path encoding")?,
                "serve",
            ],
            &[],
        )?,
        1,
        "cannot read settings file",
    );
    // The explicit argument wins over CANNERY_SETTINGS, so it reaches field validation.
    refused(
        &run(
            "explicit path precedence",
            &["--settings", good, "serve"],
            &[
                ("CANNERY_SETTINGS", missing.to_str().ok_or("path encoding")?),
                ("CANNERY_AUTH_SESSION_TTL_HOURS", "0"),
            ],
        )?,
        1,
        "auth.session_ttl_hours",
    );
    let s3 = [
        ("CANNERY_DATABASE_URL", "postgresql://unused.invalid/db"),
        ("CANNERY_STORAGE_BACKEND", "s3"),
        ("AWS_ACCESS_KEY_ID", "synthetic-access"),
        ("AWS_SECRET_ACCESS_KEY", "settings-sentinel-secret"),
        ("CANNERY_STORAGE_S3_PART_SIZE_BYTES", "1024"),
        (
            "CANNERY_DATABASE_POOL_MIN_SIZE",
            "postgresql://user:settings-sentinel-password@db",
        ),
    ];
    let output = run("aggregated secret-safe errors", &["serve"], &s3)?;
    refused(&output, 1, "storage.s3_part_size_bytes");
    refused(&output, 1, "database.pool_min_size");
    for (name, value, fragment) in [
        (
            "CANNERY_AUTH_PERSONAL_TOKEN_MAX_DAYS",
            "0",
            "auth.personal_token_max_days",
        ),
        (
            "CANNERY_STORAGE_UPLOAD_TTL_MINUTES",
            "0",
            "storage.upload_ttl_minutes",
        ),
        (
            "CANNERY_STORAGE_MAX_STREAM_SECONDS",
            "0",
            "storage.max_stream_seconds",
        ),
        (
            "CANNERY_STORAGE_VALIDATE_JSON_MAX_BYTES",
            "0",
            "storage.validate_json_max_bytes",
        ),
        (
            "CANNERY_STORAGE_MAX_CONCURRENT_VALIDATIONS",
            "0",
            "storage.max_concurrent_validations",
        ),
        ("CANNERY_STORAGE_BUCKET", "invalid bucket", "storage.bucket"),
        ("CANNERY_STORAGE_S3_PREFIX", "a//", "storage.s3_prefix"),
        (
            "CANNERY_STORAGE_S3_PRESIGN_TTL_SECONDS",
            "604801",
            "storage.s3_presign_ttl_seconds",
        ),
        (
            "CANNERY_STORAGE_S3_MULTIPART_THRESHOLD_BYTES",
            "5368709121",
            "storage.s3_multipart_threshold_bytes",
        ),
        (
            "CANNERY_STORAGE_S3_PART_SIZE_BYTES",
            "5368709121",
            "storage.s3_part_size_bytes",
        ),
        (
            "CANNERY_LEASES_JOB_OVERHEAD_SECONDS",
            "-1",
            "leases.job_overhead_seconds",
        ),
        (
            "CANNERY_LEASES_STALLED_EVALUATION_SECONDS",
            "-1",
            "leases.stalled_evaluation_seconds",
        ),
        (
            "CANNERY_SWEEPS_INTERVAL_SECONDS",
            "0",
            "sweeps.interval_seconds",
        ),
        ("CANNERY_SWEEPS_BATCH_SIZE", "0", "sweeps.batch_size"),
        (
            "CANNERY_SWEEPS_BATCHES_PER_RUN",
            "0",
            "sweeps.batches_per_run",
        ),
        (
            "CANNERY_SWEEPS_TIMEOUT_SECONDS",
            "0",
            "sweeps.timeout_seconds",
        ),
        (
            "CANNERY_SWEEPS_CONNECT_TIMEOUT_SECONDS",
            "0",
            "sweeps.connect_timeout_seconds",
        ),
        (
            "CANNERY_SWEEPS_LOCK_TIMEOUT_SECONDS",
            "0",
            "sweeps.lock_timeout_seconds",
        ),
        (
            "CANNERY_SWEEPS_STATEMENT_TIMEOUT_SECONDS",
            "0",
            "sweeps.statement_timeout_seconds",
        ),
        (
            "CANNERY_MCP_MAX_REQUEST_BYTES",
            "0",
            "mcp.max_request_bytes",
        ),
        ("CANNERY_MCP_MAX_RESULT_BYTES", "0", "mcp.max_result_bytes"),
    ] {
        refused(
            &run(fragment, &["--settings", good, "serve"], &[(name, value)])?,
            1,
            fragment,
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the conformance database and containerized CLI"]
async fn settings_precedence_controls_live_web_and_mcp() -> Result<()> {
    let work = Work::new()?;
    let file_dist = work.0.join("file-web");
    let env_dist = work.0.join("env-web");
    for (directory, label) in [
        (&file_dist, "file-selected"),
        (&env_dist, "environment-selected"),
    ] {
        fs::create_dir(directory)?;
        fs::write(
            directory.join("index.html"),
            format!("<!doctype html><title>{label}</title>"),
        )?;
    }
    let database = std::env::var("CANNERY_DATABASE_URL")?;
    let settings=work.file("server.toml",&format!("[database]\nurl='postgresql://unused.invalid/db'\n[sweeps]\nenabled=false\n[storage]\nlocal_root={}\n[web]\ndist_dir={}\n[mcp]\nmax_request_bytes=32\n",toml(work.0.join("objects").to_str().ok_or("path encoding")?),toml(file_dist.to_str().ok_or("path encoding")?)))?;
    fs::set_permissions(&settings, fs::Permissions::from_mode(0o600))?;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .build()?;
    for (override_value, expected) in [
        ("  ", "file-selected"),
        (
            env_dist.to_str().ok_or("path encoding")?,
            "environment-selected",
        ),
    ] {
        let server = start(
            &settings,
            &[
                ("CANNERY_DATABASE_URL", &database),
                ("CANNERY_WEB_DIST_DIR", override_value),
            ],
        )
        .await?;
        let health: Value = client
            .get("http://127.0.0.1:9012/api/health")
            .send()
            .await?
            .json()
            .await?;
        assert_eq!(health["status"], "ok");
        assert_eq!(health["database"], "ok");
        let response = client.get("http://127.0.0.1:9012/").send().await?;
        assert_eq!(response.status().as_u16(), 200);
        assert!(response.text().await?.contains(expected));
        let response = client
            .post("http://127.0.0.1:9012/mcp")
            .header("Content-Type", "application/json")
            .body(" ".repeat(33))
            .send()
            .await?;
        assert_eq!(response.status().as_u16(), 413);
        let body: Value = response.json().await?;
        assert_eq!(body["error"]["code"], -32600);
        drop(server);
    }
    Ok(())
}

#[test]
#[ignore = "requires the containerized conformance CLI"]
fn runner_configuration_refuses_ambiguous_and_invalid_recipes() -> Result<()> {
    // Native typed configuration exposes a value-free category instead of
    // Python's constructor/path diagnostics. Keep every refusal and redaction
    // check; installation-settings path checks elsewhere remain exact.
    let expected_error = |source: &str| {
        if std::env::var("CANNERY_CONFORMANCE_SERVER_IMPLEMENTATION").as_deref() == Ok("rust") {
            "cannery runner: runner configuration is invalid".to_owned()
        } else {
            source.to_owned()
        }
    };
    let work = Work::new()?;
    for name in ["tester.token", "evaluator.token", "experimenter.token"] {
        let file = work.file(name, "cr_svc_settings_sentinel\n")?;
        fs::set_permissions(file, fs::Permissions::from_mode(0o600))?;
    }
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/fixture");
    fs::copy(
        fixture.join("evaluator.json"),
        work.0.join("evaluator.json"),
    )?;
    let base = "api_url='http://127.0.0.1:9'\nproject='alpha'\n";
    let test = "[[kinds]]\nkind='test'\ntoken_file='tester.token'\n";
    let eval = "[[kinds]]\nkind='eval'\ntoken_file='evaluator.token'\n";
    let experiment = "[[kinds]]\nkind='experiment'\ntoken_file='experimenter.token'\n";
    let recipes = [
        (format!("project='alpha'\n{test}"), "api_url: required"),
        (format!("{base}colour=1\n{test}"), "colour: unknown key"),
        (base.into(), "kinds: list the job kinds to run"),
        (
            format!("{base}kinds=[]\n"),
            "kinds: list the job kinds to run",
        ),
        (format!("{base}kinds=[1]\n"), "kinds[0]: must be a table"),
        (
            format!("{base}{experiment}policy='evaluator.json'\n"),
            "kinds[0].policy: unknown key",
        ),
        (
            format!("{base}{test}[[kinds]]\nkind='experiment'\ntoken_file='tester.token'\n"),
            "kinds[1].token_file: the token of test, a test kind;",
        ),
        (
            format!("{base}[[kinds]]\nkind='deploy'\n"),
            "kinds[0].kind: must be one of eval, experiment, test",
        ),
        (
            format!("{base}[[kinds]]\nkind='test'\n"),
            "kinds[0].token_file: required",
        ),
        (
            format!("{base}{test}policy='evaluator.json'\n"),
            "kinds[0].policy: unknown key",
        ),
        (
            format!("{base}{test}concurrency=0\n"),
            "kinds[0].concurrency: must be an integer",
        ),
        (
            format!("{base}{test}concurrency=true\n"),
            "kinds[0].concurrency: must be an integer",
        ),
        (
            format!("{base}{test}poll_seconds=-1\n"),
            "kinds[0].poll_seconds: must be a positive",
        ),
        (
            format!("{base}{test}name='Not A Name'\n"),
            "kinds[0].name: must be a lowercase name",
        ),
        (
            format!("{base}{test}{test}"),
            "kinds[1].name: another kind is named test",
        ),
        (
            format!(
                "{base}{test}[[kinds]]\nkind='eval'\ntoken_file='tester.token'\npolicy='evaluator.json'\n"
            ),
            "kinds[1].token_file: the token of test, a test kind;",
        ),
        (
            format!("{base}{eval}"),
            "kinds[0].policy: the path of the evaluation policy is required",
        ),
        (
            format!("{base}{eval}policy='missing.json'\n"),
            "kinds[0].policy: cannot read",
        ),
        (
            format!("{base}{eval}policy='runner.toml'\n"),
            "kinds[0].policy:",
        ),
        (
            format!("{base}[launcher]\ntype='pod'\n{test}"),
            "launcher.type: must be local, docker or kubernetes",
        ),
        (
            format!("{base}[launcher]\n{test}"),
            "launcher.type: required",
        ),
        (
            format!("{base}[launcher]\ntype='docker'\ndocker_gpu_mode='amd'\n{test}"),
            "launcher.docker_gpu_mode: must be nvidia or cos",
        ),
        (
            format!("{base}[launcher]\ntype='docker'\ndocker_gpu_devices='0,0'\n{test}"),
            "launcher.docker_gpu_devices: '0,0' lists a GPU twice",
        ),
        (
            format!("{base}[launcher]\ntype='docker'\ndocker_gpu_devices=['0']\n{test}"),
            "launcher.docker_gpu_devices: must list GPU indices",
        ),
        (
            format!("{base}[github]\nallowed_repos='a/b'\n{test}"),
            "github.allowed_repos: must be",
        ),
        (
            format!("{base}[github]\nsecret=1\n{test}"),
            "github.secret: unknown key",
        ),
        ("api_url=[".into(), "is not a TOML document"),
    ];
    for (recipe, expected) in recipes {
        let file = work.file("runner.toml", &recipe)?;
        refused(
            &run(
                expected,
                &[
                    "runner",
                    "--config",
                    file.to_str().ok_or("path encoding")?,
                    "--once",
                ],
                &[],
            )?,
            2,
            &expected_error(expected),
        );
    }
    let file = work.file("runner.toml", &format!("{base}{test}"))?;
    fs::set_permissions(
        work.0.join("tester.token"),
        fs::Permissions::from_mode(0o644),
    )?;
    refused(
        &run(
            "token file permissions",
            &[
                "runner",
                "--config",
                file.to_str().ok_or("path encoding")?,
                "--once",
            ],
            &[],
        )?,
        2,
        &expected_error("chmod 600"),
    );
    refused(
        &run(
            "runner config flag conflict",
            &[
                "runner",
                "--config",
                file.to_str().ok_or("path encoding")?,
                "--project",
                "override",
            ],
            &[],
        )?,
        2,
        &expected_error("--config replaces --project"),
    );
    Ok(())
}
