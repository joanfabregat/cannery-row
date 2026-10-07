#![forbid(unsafe_code)]
#[path = "support/runner_code_fetch_security.rs"]
#[allow(
    dead_code,
    reason = "Read-only RSA archive generator serves other suites"
)]
mod fetch;
#[path = "support/lifecycle_support.rs"]
#[allow(dead_code, reason = "Read-only bootstrap serves other suites")]
mod lifecycle;
#[path = "support/runner_security_semantics.rs"]
#[allow(dead_code, reason = "Read-only CLI helper serves other suites")]
mod runner;
#[path = "support/runner_renewal_semantics.rs"]
mod support;
use conformance::Result;
use lifecycle::Call;
use runner::{Rig, Running};
use serde_json::json;
use std::{
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    time::{Duration, Instant},
};

#[tokio::test]
#[ignore = "requires actual continuous CLI and loopback App provider"]
async fn runner_same_worker_naturally_refreshes_github_app_token() -> Result<()> {
    let mut rig = Rig::new(
        "natural-app-refresh",
        runner::cached_producer(runner::COMMIT_A)?,
        runner::cache_science()?,
    )
    .await?;
    let (archive, public) = fetch::archive(&rig, "valid", true).await?;
    let provider = support::Provider::new(archive, public).await?;
    let first = rig.submit().await?;
    let child = Running::new(
        support::command(
            &rig,
            "test",
            "tester.token",
            Some(provider.github(&rig)),
            false,
        )?
        .spawn()?,
    );
    let first_job = support::completed(&mut rig, first).await?;
    assert_eq!(provider.mints(), 1);
    assert_eq!(provider.downloads(), 1);
    assert!(
        !rig.work
            .0
            .join("cache/code")
            .join(runner::REPOSITORY)
            .join(runner::COMMIT_A)
            .exists()
    );
    // The 30-second margin outlasts the first job's 25-second bound even
    // on a busy CI runner. Cross it using real time in this same process.
    tokio::time::sleep(Duration::from_secs(31)).await;
    assert_eq!(provider.mints(), 1);
    let second = rig.submit().await?;
    let second_job = support::completed(&mut rig, second).await?;
    assert_eq!(provider.mints(), 2);
    assert_eq!(provider.downloads(), 2);
    provider.check();
    child.terminate()?;
    let output = child.wait(Duration::from_secs(10)).await?;
    assert_eq!(output.status.code(), Some(143));
    fetch::assert_redacted(&rig, &output, &first_job);
    fetch::assert_redacted(&rig, &output, &second_job);
    assert!(
        !format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .contains(support::TOKEN)
    );
    rig.assert_clean()?;
    rig.finish("natural-app-refresh").await
}

#[tokio::test]
#[ignore = "requires actual experiment CLI and real five-second attempt leases"]
#[allow(
    clippy::too_many_lines,
    reason = "One real workflow observes renewal withdrawal cleanup and sweep"
)]
async fn experiment_worker_credential_withdrawal_abandons_and_sweeps_once() -> Result<()> {
    let mut rig = Rig::new(
        "experiment-lease-withdrawal",
        runner::producer()?,
        runner::science()?,
    )
    .await?;
    let experimenter = rig.world.experimenter(&rig.actors.admin).await?;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(rig.work.0.join("experimenter.token"))?
        .write_all(experimenter.as_bytes())?;
    let root = serde_json::to_string(&rig.work.0.to_str().ok_or("owned root not UTF8")?)?;
    rig.script("slow_experiment.py",&format!("import subprocess\nimport sys\nimport time\nfrom pathlib import Path\n\ndef main() -> None:\n    root = Path({root})\n    code = 'import time; from pathlib import Path; time.sleep(40); Path(' + repr(str(root / 'surviving-experiment-child')) + ').write_text(\"survived\")'\n    child = subprocess.Popen([sys.executable, '-c', code])\n    (root / 'experiment-child.pid').write_text(str(child.pid))\n    (root / 'experiment-started').write_text('started')\n    time.sleep(90)\n\nif __name__ == '__main__':\n    main()\n"))?;
    let mut step = runner::fixture("examples/fixture/experiments/fixture-experiment.json")?;
    step["spec"]["container"]["command"] = json!(["python3", "slow_experiment.py"]);
    rig.world
        .api(Call::post(
            "/api/projects/{slug}/experiment-steps",
            format!("{}/experiment-steps", rig.world.base()),
            &rig.actors.admin,
            step,
            201,
        ))
        .await?;
    let mut track = runner::fixture("examples/fixture/workflow-track.json")?;
    track["producer"]["revision"] = json!(rig.producer_revision);
    rig.world
        .api(Call::post(
            "/api/projects/{slug}/tracks",
            format!("{}/tracks", rig.world.base()),
            &rig.actors.admin,
            track,
            201,
        ))
        .await?;
    let draft = rig
        .world
        .api(Call::post(
            "/api/projects/{slug}/hypotheses",
            format!("{}/hypotheses", rig.world.base()),
            &rig.actors.agent,
            runner::fixture("examples/fixture/workflow-hypothesis.json")?,
            201,
        ))
        .await?
        .body;
    let number = draft["number"]
        .as_i64()
        .ok_or("hypothesis number missing")?;
    rig.world.api(Call::post("/api/projects/{slug}/hypotheses/{number}/draft-review",format!("{}/hypotheses/{number}/draft-review",rig.world.base()),&rig.actors.admin,json!({"draft_revision":1,"action":"approve","reason":"Observe real workflow lease loss"}),200)).await?;
    let child = Running::new(
        support::command(&rig, "experiment", "experimenter.token", None, true)?.spawn()?,
    );
    support::started(&rig).await?;
    let initial = support::attempt(&mut rig, number).await?;
    assert!(initial["state"] == "claimed" || initial["state"] == "running");
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let current = support::attempt(&mut rig, number).await?;
        if current["lease_expires_at"] != initial["lease_expires_at"] {
            assert_eq!(current["state"], "running");
            break;
        }
        if Instant::now() > until {
            return Err(
                "real experiment heartbeat not observed; use five-second lease profile".into(),
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    rig.world
        .api(Call::post(
            "/api/projects/{slug}/service-accounts/{name}/disable",
            format!(
                "{}/service-accounts/workflow-runner/disable",
                rig.world.base()
            ),
            &rig.actors.admin,
            json!({"reason":"Withdraw real experiment worker credential"}),
            200,
        ))
        .await?;
    let output = child.wait(Duration::from_secs(15)).await?;
    runner::successful_process(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("abandoned"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("HTTP 401"));
    let before = support::attempt(&mut rig, number).await?;
    assert_eq!(before["state"], "running");
    assert_eq!(
        before["failures"]
            .as_array()
            .ok_or("failure history missing")?
            .len(),
        0
    );
    assert!(!rig.work.0.join("surviving-experiment-child").exists());
    let pid: u32 = fs::read_to_string(rig.work.0.join("experiment-child.pid"))?.parse()?;
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => assert_eq!(
            stat.rsplit_once(')')
                .and_then(|(_, tail)| tail.split_whitespace().next()),
            Some("Z")
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    // The worker abandons before its next retry would cross expiry. Wait for
    // genuine PostgreSQL expiry, observing bounded real sweeps without changing time.
    let until = Instant::now() + Duration::from_secs(6);
    let report = loop {
        let report = rig.world.h.sweep(&rig.actors.admin).await?;
        if report["attempts_expired"] == 1 {
            break report;
        }
        assert_eq!(report["attempts_expired"], 0);
        assert_eq!(report["attempts_requeued"], 0);
        if Instant::now() > until {
            return Err("real experiment lease did not expire within bound".into());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(report["attempts_expired"], 1);
    assert_eq!(report["attempts_requeued"], 1);
    let again = rig.world.h.sweep(&rig.actors.admin).await?;
    assert_eq!(again["attempts_expired"], 0);
    assert_eq!(again["attempts_requeued"], 0);
    let after = support::attempt(&mut rig, number).await?;
    assert_eq!(after["state"], "failed");
    let failures = after["failures"]
        .as_array()
        .ok_or("failure history missing")?;
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0]["code"], "lease_expired");
    assert_eq!(failures[0]["requeued"], true);
    let hypothesis = rig
        .world
        .api(Call::get(
            "/api/projects/{slug}/hypotheses/{number}",
            format!("{}/hypotheses/{number}", rig.world.base()),
            &rig.actors.admin,
        ))
        .await?
        .body;
    assert_eq!(hypothesis["state"], "queued");
    assert!(
        hypothesis["reviews"]
            .as_array()
            .ok_or("review history missing")?
            .iter()
            .all(|review| review["kind"] != "failure")
    );
    for secret in [&experimenter, &rig.actors.admin] {
        assert!(
            !format!(
                "{}{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
                after
            )
            .contains(secret),
            "experiment diagnostics leaked credential"
        );
    }
    rig.assert_clean()?;
    rig.finish("experiment-lease-withdrawal").await
}
