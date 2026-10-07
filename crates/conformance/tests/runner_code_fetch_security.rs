#![forbid(unsafe_code)]
#[path = "support/lifecycle_support.rs"]
#[allow(
    dead_code,
    reason = "Read-only reusable bootstrap covers other scenarios"
)]
mod lifecycle;
#[path = "support/runner_security_semantics.rs"]
#[allow(dead_code, reason = "Read-only runner helper covers other scenarios")]
mod runner;
#[path = "support/runner_code_fetch_security.rs"]
mod support;
use conformance::Result;
use runner::{COMMIT_A, Rig, cache_science, cached_producer, successful_process};
use std::{
    fs,
    time::{Duration, Instant},
};
use support::Mock;

#[tokio::test]
#[ignore = "requires CLI, conformance server and loopback GitHub"]
async fn runner_unsafe_archives_and_revision_mismatch_never_publish() -> Result<()> {
    for variant in [
        "dotdot",
        "absolute",
        "other-top",
        "symlink-out",
        "symlink-absolute",
        "symlink-parent",
        "hardlink",
        "char-device",
        "block-device",
        "fifo",
        "duplicate",
        "orphan",
        "link-parent",
        "link-chain",
        "wrong-commit",
        "no-commit",
        "garbage",
    ] {
        let mut rig = Rig::new(
            &format!("fetch-{variant}"),
            cached_producer(COMMIT_A)?,
            cache_science()?,
        )
        .await?;
        let (archive, public) = support::archive(&rig, variant, false).await?;
        let mock = Mock::new("plain", archive, public).await?;
        mock.configure(&mut rig, false)?;
        let number = rig.submit().await?;
        let output = rig.run().await?;
        successful_process(&output);
        let job = rig.job(number).await?;
        assert_eq!(job["state"], "failed", "{variant}");
        assert_eq!(
            job["error_code"],
            if matches!(variant, "wrong-commit" | "no-commit") {
                "runner_error"
            } else {
                "invalid_code"
            },
            "{variant}"
        );
        assert_eq!(job["error_step"], "overlap-producer");
        #[allow(
            clippy::assert_is_empty,
            reason = "Failure assertions must not print credential-bearing diagnostics"
        )]
        {
            assert!(!lifecycle::string(&job["error_reason"])?.is_empty());
        }
        assert_eq!(mock.downloads(), 1);
        mock.assert_credentials();
        support::assert_redacted(&rig, &output, &job);
        support::assert_no_publication(&rig)?;
        if variant != "garbage" {
            assert_eq!(
                fs::read_to_string(rig.work.0.join("outside-sentinel"))?,
                "untouched"
            );
        }
        assert!(!rig.work.0.join("escape.txt").exists());
        rig.finish(&format!("code-archive-{variant}")).await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires CLI, conformance server and loopback GitHub"]
async fn runner_repository_fork_redirect_and_allowlist_checks() -> Result<()> {
    for mode in [
        "missing",
        "invalid-revision",
        "fork",
        "missing-redirect",
        "download-missing",
        "alternate-branch",
        "direct",
        "runner-allowlist",
    ] {
        let mut rig = Rig::new(
            &format!("fetch-{mode}"),
            cached_producer(COMMIT_A)?,
            cache_science()?,
        )
        .await?;
        let (archive, public) = support::archive(&rig, "valid", false).await?;
        let mock = Mock::new(mode, archive, public).await?;
        mock.configure(&mut rig, false)?;
        if mode == "runner-allowlist" {
            let index = rig
                .cli_extra
                .iter()
                .position(|value| value == "--github-allowed-repos")
                .ok_or("allowlist missing")?;
            rig.cli_extra[index + 1] = "owner/other".into();
        }
        let number = rig.submit().await?;
        let output = rig.run().await?;
        successful_process(&output);
        let job = rig.job(number).await?;
        let success = matches!(mode, "alternate-branch" | "direct");
        assert_eq!(
            job["state"],
            if success { "completed" } else { "failed" },
            "{mode}"
        );
        if success {
            assert_eq!(mock.downloads(), 1);
            assert!(
                rig.work
                    .0
                    .join("cache/code")
                    .join(runner::REPOSITORY)
                    .join(COMMIT_A)
                    .is_dir()
            );
            rig.assert_clean()?;
        } else {
            assert_eq!(
                job["error_code"],
                if mode == "runner-allowlist" {
                    "code_not_allowed"
                } else {
                    "runner_error"
                },
                "{mode}"
            );
            support::assert_no_publication(&rig)?;
            if matches!(
                mode,
                "fork" | "missing" | "invalid-revision" | "runner-allowlist"
            ) {
                assert_eq!(mock.downloads(), 0);
            }
            if mode == "runner-allowlist" {
                assert_eq!(mock.requests(), 0);
            }
        }
        mock.assert_credentials();
        support::assert_redacted(&rig, &output, &job);
        rig.finish(&format!("code-fetch-{mode}")).await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires CLI, conformance server and loopback GitHub"]
async fn runner_rate_limit_wait_is_real_bounded_and_retried_once() -> Result<()> {
    for (mode, expected_requests, success) in [
        ("rate-once", 2, true),
        ("rate-reset", 2, true),
        ("plain-forbidden", 1, false),
        ("rate-repeat", 2, false),
        ("rate-long", 1, false),
    ] {
        let mut rig = Rig::new(
            &format!("fetch-{mode}"),
            cached_producer(COMMIT_A)?,
            cache_science()?,
        )
        .await?;
        let (archive, public) = support::archive(&rig, "valid", false).await?;
        let mock = Mock::new(mode, archive, public).await?;
        mock.configure(&mut rig, false)?;
        let number = rig.submit().await?;
        let started = Instant::now();
        let output = rig.run().await?;
        let elapsed = started.elapsed();
        successful_process(&output);
        let job = rig.job(number).await?;
        assert_eq!(mock.metadata(), expected_requests);
        assert_eq!(job["state"], if success { "completed" } else { "failed" });
        if success {
            assert!(elapsed >= Duration::from_secs(1));
            assert_eq!(mock.downloads(), 1);
            rig.assert_clean()?;
        } else {
            assert_eq!(job["error_code"], "runner_error");
            assert!(
                elapsed < Duration::from_secs(20),
                "out-of-budget retry was not refused promptly"
            );
            support::assert_no_publication(&rig)?;
        }
        mock.assert_credentials();
        support::assert_redacted(&rig, &output, &job);
        rig.finish(&format!("code-{mode}")).await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires CLI, conformance server and synthetic GitHub App"]
async fn runner_app_refusal_invalid_expiry_and_one_remint() -> Result<()> {
    for (mode, mints, metadata, success) in [
        ("app-refused", 1, 0, false),
        ("app-bad-expiry", 1, 0, false),
        ("app-no-expiry", 1, 0, false),
        ("app-remint", 2, 2, true),
        ("app-always-401", 2, 2, false),
    ] {
        let mut rig = Rig::new(
            &format!("fetch-{mode}"),
            cached_producer(COMMIT_A)?,
            cache_science()?,
        )
        .await?;
        let (archive, public) = support::archive(&rig, "valid", true).await?;
        let mock = Mock::new(mode, archive, public).await?;
        mock.configure(&mut rig, true)?;
        let number = rig.submit().await?;
        let output = rig.run().await?;
        successful_process(&output);
        let job = rig.job(number).await?;
        assert_eq!(
            job["state"],
            if success { "completed" } else { "failed" },
            "{mode}"
        );
        assert_eq!(mock.mints(), mints, "{mode}");
        assert_eq!(mock.metadata(), metadata, "{mode}");
        if success {
            assert_eq!(mock.downloads(), 1);
            rig.assert_clean()?;
        } else {
            assert_eq!(job["error_code"], "runner_error");
            support::assert_no_publication(&rig)?;
        }
        mock.assert_credentials();
        support::assert_redacted(&rig, &output, &job);
        rig.finish(&format!("code-{mode}")).await?;
    }
    Ok(())
}
