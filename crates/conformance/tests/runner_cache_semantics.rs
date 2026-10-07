#![forbid(unsafe_code)]
#[path = "support/runner_code_fetch_security.rs"]
#[allow(
    dead_code,
    reason = "Shared read-only loopback provider covers fetch profiles"
)]
mod fetch;
#[path = "support/lifecycle_support.rs"]
#[allow(
    dead_code,
    reason = "Shared read-only public bootstrap supports other suites"
)]
mod lifecycle;
#[path = "support/runner_security_semantics.rs"]
#[allow(dead_code, reason = "Shared read-only CLI support covers other suites")]
mod runner;
#[path = "support/runner_cache_semantics.rs"]
mod support;
use conformance::Result;
use serde_json::json;
use std::{fs, time::Duration};

#[tokio::test]
#[ignore = "requires CLI, API and controlled loopback provider"]
async fn runner_oversize_cache_entries_stay_held_then_evict() -> Result<()> {
    let (mut rig, mock, _) = support::rig("cache-held", true).await?;
    rig.cli_extra
        .extend(["--cache-max-bytes".into(), "1".into()]);
    let number = rig.submit().await?;
    let child = runner::Running::new(rig.command()?.spawn()?);
    support::wait_started(&rig).await?;
    assert!(support::code(&rig).is_dir());
    assert_eq!(support::setups(&rig)?.len(), 1);
    assert!(support::bytes(&support::code(&rig))? > 1);
    assert!(support::bytes(&support::setups(&rig)?[0])? > 1);
    assert_eq!(rig.job(number).await?["state"], "claimed");
    support::release(&rig)?;
    let output = child.wait(Duration::from_secs(20)).await?;
    runner::successful_process(&output);
    let job = rig.job(number).await?;
    assert_eq!(job["state"], "completed");
    fetch::assert_redacted(&rig, &output, &job);
    assert!(!support::code(&rig).exists());
    assert_eq!(support::setups(&rig)?.len(), 0);
    assert_eq!(support::count(&rig)?, 1);
    assert_eq!(mock.downloads(), 1);
    support::complete(&mut rig, &mock).await?;
    assert_eq!(support::count(&rig)?, 2);
    assert_eq!(mock.downloads(), 2);
    assert!(!support::code(&rig).exists());
    assert_eq!(support::setups(&rig)?.len(), 0);
    rig.finish("cache-held-oversize").await
}

#[tokio::test]
#[ignore = "requires CLI, API and controlled loopback provider"]
async fn runner_real_cache_budget_evicts_least_recently_used() -> Result<()> {
    let (mut rig, mock, mut producer) = support::rig("cache-lru", false).await?;
    support::complete(&mut rig, &mock).await?;
    let a = support::variant_entry(&rig, "A")?;
    let entry_size = support::bytes(&a)?;
    let code_size = support::bytes(&support::code(&rig))?;
    let cap = code_size + 2 * entry_size + 64;
    rig.cli_extra
        .extend(["--cache-max-bytes".into(), cap.to_string()]);
    for variant in ["B", "A", "C"] {
        producer["spec"]["container"]["env"]
            .as_array_mut()
            .ok_or("env missing")?
            .last_mut()
            .ok_or("variant missing")?["value"] = json!(variant);
        rig.bind(producer.clone()).await?;
        support::complete(&mut rig, &mock).await?;
    }
    assert!(a.is_dir());
    assert!(support::variant_entry(&rig, "C")?.is_dir());
    assert!(support::variant_entry(&rig, "B").is_err());
    assert_eq!(support::setups(&rig)?.len(), 2);
    assert_eq!(support::count(&rig)?, 3);
    assert_eq!(mock.downloads(), 1);
    assert!(
        support::bytes(&rig.work.0.join("cache/code"))?
            + support::bytes(&rig.work.0.join("cache/setup"))?
            <= cap
    );
    rig.finish("cache-lru-budget").await
}

#[tokio::test]
#[ignore = "requires CLI, API and controlled loopback provider"]
async fn runner_reopening_indexes_real_entries_and_clears_interrupted_tmp() -> Result<()> {
    let (mut rig, mock, _) = support::rig("cache-reopen", false).await?;
    support::complete(&mut rig, &mock).await?;
    let a = support::variant_entry(&rig, "A")?;
    let stale = rig.work.0.join("cache/tmp/interrupted-setup");
    fs::create_dir(&stale)?;
    fs::write(stale.join("unfinished"), "owned interrupted fixture")?;
    support::complete(&mut rig, &mock).await?;
    assert!(!stale.exists());
    assert_eq!(support::count(&rig)?, 1);
    assert_eq!(mock.downloads(), 1);
    assert!(a.is_dir());
    // Only remove a real published task-owned entry between CLI processes.
    // No internal index or synthetic cache keys are introduced.
    fs::remove_dir_all(&a)?;
    support::complete(&mut rig, &mock).await?;
    assert_eq!(support::count(&rig)?, 2);
    assert_eq!(mock.downloads(), 1);
    assert_eq!(support::variant_entry(&rig, "A")?, a);
    assert!(support::code(&rig).is_dir());
    rig.finish("cache-reopen-recover").await
}

#[tokio::test]
#[ignore = "requires concurrent owned CLI processes and real leases"]
async fn runner_same_cache_root_is_exclusive_and_reusable_after_release() -> Result<()> {
    let (mut rig, mock, _) = support::rig("cache-root-lock", true).await?;
    let first = rig.submit().await?;
    let child = runner::Running::new(rig.command()?.spawn()?);
    support::wait_started(&rig).await?;
    let second = rig.submit().await?;
    let second_output = rig.run().await?;
    runner::successful_process(&second_output);
    let second_job = rig.job(second).await?;
    assert_eq!(second_job["state"], "failed");
    assert_eq!(second_job["error_code"], "runner_error");
    assert!(lifecycle::string(&second_job["error_reason"])?.contains("cache root"));
    fetch::assert_redacted(&rig, &second_output, &second_job);
    assert_eq!(mock.downloads(), 1);
    assert_eq!(support::count(&rig)?, 1);
    assert_eq!(rig.job(first).await?["state"], "claimed");
    support::release(&rig)?;
    let output = child.wait(Duration::from_secs(20)).await?;
    runner::successful_process(&output);
    let first_job = rig.job(first).await?;
    assert_eq!(first_job["state"], "completed");
    fetch::assert_redacted(&rig, &output, &first_job);
    let retry_output = rig.run().await?;
    runner::successful_process(&retry_output);
    let listed = rig
        .world
        .api(lifecycle::Call::get(
            &format!("{}/jobs", lifecycle::ATTEMPT),
            format!("{}/hypotheses/{second}/attempts/1/jobs", rig.world.base()),
            &rig.actors.admin,
        ))
        .await?
        .body;
    let retry = listed["items"]
        .as_array()
        .ok_or("job items missing")?
        .iter()
        .find(|job| job["stage"] == "tester" && job["run_number"] == 2)
        .ok_or("retry job missing")?;
    assert_eq!(retry["state"], "completed");
    assert_eq!(rig.job(second).await?["state"], "failed");
    fetch::assert_redacted(&rig, &retry_output, retry);
    mock.assert_credentials();
    rig.assert_clean()?;
    assert_eq!(mock.downloads(), 1);
    assert_eq!(support::count(&rig)?, 1);
    rig.finish("cache-root-exclusion").await
}
