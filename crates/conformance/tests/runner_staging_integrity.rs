#![forbid(unsafe_code)]
#[path = "support/lifecycle_support.rs"]
#[allow(dead_code, reason = "Read-only bootstrap serves other suites")]
mod lifecycle;
#[path = "support/runner_security_semantics.rs"]
#[allow(dead_code, reason = "Read-only real CLI helper serves other suites")]
mod runner;
#[path = "support/runner_staging_integrity.rs"]
mod support;
use conformance::Result;
use lifecycle::{ATTEMPT, Call, sha, string};
use reqwest::Method;
use std::{fs, os::unix::fs::symlink, time::Duration};

#[tokio::test]
#[ignore = "requires actual CLI, local uploads and real 60s job leases"]
async fn runner_verified_output_changed_after_upload_is_not_scored() -> Result<()> {
    exercise("bytes").await
}
#[tokio::test]
#[ignore = "requires actual CLI, local uploads and real 60s job leases"]
async fn runner_verified_output_swapped_for_link_after_upload_is_not_scored() -> Result<()> {
    exercise("symlink").await
}
#[allow(
    clippy::too_many_lines,
    reason = "One genuine held upload establishes provenance and the failed/retry observations"
)]
async fn exercise(mode: &str) -> Result<()> {
    let mut rig = runner::Rig::new(
        &format!("staging-{mode}"),
        runner::producer()?,
        runner::science()?,
    )
    .await?;
    support::scorer_marker(&rig)?;
    let number = rig.submit().await?;
    let relay = support::Relay::new(&rig.world.base_url).await?;
    let mut command = rig.command_for_api(&relay.base)?;
    let child = runner::Running::new(command.spawn()?);
    let artifact = relay.wait().await?;
    assert_eq!(artifact["role"], "run");
    let output = support::output(&rig)?;
    let file = output.join("run.json");
    let original = fs::read(&file)?;
    assert_eq!(artifact["sha256"], sha(&original));
    assert_eq!(artifact["size_bytes"], original.len());
    assert!(!rig.work.0.join("scorer-started").exists());
    let secret = rig.work.0.join("owned-secret");
    fs::create_dir(&secret)?;
    let secret_bytes = b"owned-private-staging-sentinel";
    fs::write(secret.join("sentinel.txt"), secret_bytes)?;
    fs::write(secret.join("run.json"), br#"{"queries":{}}"#)?;
    if mode == "bytes" {
        let mut modified = original.clone();
        let position = modified
            .iter()
            .position(|byte| *byte == b'q')
            .ok_or("fixture run missing expected JSON key")?;
        modified[position] = b'Q';
        assert_eq!(modified.len(), original.len());
        assert_ne!(sha(&modified), sha(&original));
        fs::write(&file, modified)?;
    } else {
        fs::rename(&output, output.with_file_name("uploaded"))?;
        symlink(&secret, &output)?;
    }
    relay.release();
    let result = child.wait(Duration::from_secs(30)).await?;
    runner::successful_process(&result);
    relay.assert_clean();
    assert!(!rig.work.0.join("scorer-started").exists());
    assert_eq!(fs::read(secret.join("sentinel.txt"))?, secret_bytes);
    let job = rig.job(number).await?;
    assert_eq!(job["state"], "failed");
    assert_eq!(job["error_code"], "input_verification_failed");
    assert_eq!(job["error_step"], "fixture-scorer");
    #[allow(
        clippy::assert_is_empty,
        reason = "Failure diagnostics must not print credential-bearing strings"
    )]
    {
        assert!(!string(&job["error_reason"])?.is_empty());
    }
    let roles = job["outputs"].as_array().ok_or("outputs missing")?;
    assert_eq!(roles.iter().filter(|item| item["role"] == "run").count(), 1);
    assert_eq!(
        roles
            .iter()
            .filter(|item| item["role"] == "step_log")
            .count(),
        1
    );
    assert_eq!(
        roles
            .iter()
            .filter(|item| item["role"] == "per_query_results" || item["role"] == "evidence")
            .count(),
        0
    );
    let verified = roles
        .iter()
        .find(|item| item["role"] == "run")
        .ok_or("verified run missing")?;
    assert_eq!(verified["sha256"], artifact["sha256"]);
    let request = rig
        .world
        .h
        .request(
            Method::GET,
            &format!(
                "{}/artifacts/{}",
                rig.world.base(),
                string(&verified["id"])?
            ),
        )?
        .bearer_auth(&rig.actors.admin);
    let downloaded = rig
        .world
        .h
        .check_response(
            Method::GET,
            "/api/projects/{slug}/artifacts/{artifact_id}",
            request.send().await?,
            200,
        )
        .await?;
    assert!(
        downloaded.raw_body == original,
        "verified uploaded bytes changed with local staging mutation"
    );
    let path = format!("{}/hypotheses/{number}/attempts/1", rig.world.base());
    let attempt = rig
        .world
        .api(Call::get(ATTEMPT, path.clone(), &rig.actors.admin))
        .await?
        .body;
    assert_eq!(attempt["state"], "testing");
    let jobs = rig
        .world
        .api(Call::get(
            &format!("{ATTEMPT}/jobs"),
            format!("{path}/jobs"),
            &rig.actors.admin,
        ))
        .await?
        .body;
    let jobs = jobs["items"].as_array().ok_or("jobs missing")?;
    assert_eq!(jobs.len(), 2);
    let pending = jobs
        .iter()
        .find(|item| item["run_number"] == 2)
        .ok_or("automatic retry missing")?;
    assert_eq!(pending["state"], "pending");
    assert_eq!(pending["origin"], "auto_retry");
    assert_eq!(pending["stage"], "tester");
    assert!(pending["error_code"].is_null());
    let logs = rig.logs(&job).await?;
    let diagnostics = format!(
        "{}{}{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
        logs,
        job
    );
    for private in [
        &rig.actors.admin,
        &rig.actors.tester,
        &rig.actors.agent,
        "owned-private-staging-sentinel",
    ] {
        assert!(
            !diagnostics.contains(private),
            "private material leaked through staging failure"
        );
    }
    rig.assert_clean()?;
    rig.finish(&format!("runner-staging-{mode}")).await
}
