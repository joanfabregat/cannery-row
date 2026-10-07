#![forbid(unsafe_code)]
#[path = "support/lifecycle_support.rs"]
#[allow(
    dead_code,
    reason = "Shared lifecycle bootstrap supports additional scenario suites"
)]
mod lifecycle;
#[path = "support/runner_security_semantics.rs"]
mod support;
use conformance::Result;
use serde_json::{Value, json};
use std::{fs, time::Duration};
use support::{Rig, producer, science, successful_process};

const OBSERVING_PRODUCER: &str = r#"import json
import os
import runpy
import subprocess
import sys
from pathlib import Path

def main() -> None:
    root = Path(os.environ['CR_ROOT'])
    document = json.loads((root / 'job.json').read_text())
    observed = {'role': document['role'], 'step': document['step'], 'datasets': [item['id'] for item in document['inputs']['datasets']], 'input_dirs': sorted(path.name for path in (root / 'inputs').iterdir()), 'env': sorted(os.environ), 'cwd_in_job': Path.cwd().parent == root.parent, 'manifest_matches': document['manifest']['spec']['role'] == 'producer', 'has_lease': any('token' in key or 'lease' in key for key in document)}
    print('semantics producer: ' + json.dumps(observed), flush=True)
    Path('fixture_step.py').write_text(Path('fixture_step.py').read_text() + '\n# local_tampered\n')
    marker = root.parents[2] / 'surviving-child'
    code = 'import time; from pathlib import Path; time.sleep(2.5); Path(' + repr(str(marker)) + ').write_text("survived")'
    subprocess.Popen([sys.executable, '-c', code])
    runpy.run_path('produce_overlap.py', run_name='__main__')

if __name__ == '__main__':
    main()
"#;
const OBSERVING_SCORER: &str = r"import json
import os
import runpy
from pathlib import Path

def main() -> None:
    root = Path(os.environ['CR_ROOT'])
    document = json.loads((root / 'job.json').read_text())
    observed = {'role': document['role'], 'datasets': [item['id'] for item in document['inputs']['datasets']], 'input_dirs': sorted(path.name for path in (root / 'inputs').iterdir()), 'fresh_copy': '# local_tampered' not in Path('fixture_step.py').read_text(), 'cwd_in_job': Path.cwd().parent == root.parent}
    print('semantics scorer: ' + json.dumps(observed), flush=True)
    runpy.run_path('score.py', run_name='__main__')

if __name__ == '__main__':
    main()
";
fn observation(log: &str, prefix: &str) -> Result<Value> {
    Ok(serde_json::from_str(
        log.lines()
            .find_map(|line| line.strip_prefix(prefix))
            .ok_or("fixture observation missing")?,
    )?)
}

#[tokio::test]
#[ignore = "requires the conformance server, OIDC provider and local CLI"]
async fn runner_contract_environment_fresh_copy_and_background_cleanup() -> Result<()> {
    let mut producer = producer()?;
    producer["spec"]["container"]["command"] = json!(["python3", "observe_producer.py"]);
    let mut science = science()?;
    science["scorer"]["spec"]["container"]["command"] = json!(["python3", "observe_scorer.py"]);
    let mut rig = Rig::new("runner-contract", producer, science).await?;
    rig.script("observe_producer.py", OBSERVING_PRODUCER)?;
    rig.script("observe_scorer.py", OBSERVING_SCORER)?;
    let number = rig.submit().await?;
    successful_process(&rig.run().await?);
    let job = rig.job(number).await?;
    assert_eq!(job["state"], "completed");
    let logs = rig.logs(&job).await?;
    let observed = observation(&logs, "semantics producer: ")?;
    assert_eq!(observed["datasets"], json!(["queries"]));
    assert_eq!(observed["input_dirs"], json!(["candidate", "queries"]));
    assert_eq!(observed["role"], "producer");
    assert_eq!(observed["step"], "overlap-producer");
    assert_eq!(observed["manifest_matches"], true);
    assert_eq!(observed["cwd_in_job"], true);
    assert_eq!(observed["has_lease"], false);
    assert_eq!(
        observed["env"],
        json!([
            "CR_ROOT",
            "FIXTURE_SPLITTER",
            "HOME",
            "LANG",
            "PATH",
            "PYTHONDONTWRITEBYTECODE",
            "TMPDIR"
        ])
    );
    let observed = observation(&logs, "semantics scorer: ")?;
    assert_eq!(observed["role"], "scorer");
    assert_eq!(observed["datasets"], json!(["qrels"]));
    assert_eq!(
        observed["input_dirs"],
        json!(["claimed_sheet", "qrels", "run"])
    );
    assert_eq!(observed["fresh_copy"], true);
    assert_eq!(observed["cwd_in_job"], true);
    assert!(
        !fs::read_to_string(rig.work.0.join("steps/fixture_step.py"))?.contains("# local_tampered")
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!rig.work.0.join("surviving-child").exists());
    rig.assert_clean()?;
    rig.finish("runner-contract").await
}

#[tokio::test]
#[ignore = "requires the conformance server, OIDC provider and local CLI"]
async fn runner_step_failures_deadline_logs_and_output_links() -> Result<()> {
    let variants = [
        ("exit", "step_failed"),
        ("missing-command", "step_failed"),
        ("deadline", "deadline_exceeded"),
        ("directory-link", "invalid_output"),
        ("parent-link", "invalid_output"),
        ("file-link", "invalid_output"),
        ("inner-link", "invalid_output"),
        ("large-log", "invalid_output"),
    ];
    for (variant, code) in variants {
        let mut producer = producer()?;
        producer["spec"]["container"]["command"] = if variant == "missing-command" {
            json!(["cannery-conformance-no-such-command"])
        } else {
            json!(["python3", "negative_producer.py"])
        };
        let mut science = science()?;
        if variant == "deadline" {
            producer["spec"]["activeDeadlineSeconds"] = json!(1);
        }
        if variant == "large-log" {
            science["limits"]["max_output_bytes"] = json!(65_536);
        }
        let mut rig = Rig::new(&format!("runner-{variant}"), producer, science).await?;
        fs::create_dir(rig.work.0.join("outside"))?;
        fs::write(rig.work.0.join("outside/sentinel"), "preserved")?;
        let action = match variant {
            "exit" => {
                "print('PRIVATE_PAYLOAD_STDOUT', flush=True); print('PRIVATE_PAYLOAD_STDERR', file=sys.stderr, flush=True); raise SystemExit(7)"
            }
            "deadline" => "print('deadline-started', flush=True); time.sleep(30)",
            "directory-link" => {
                "shutil.rmtree(root / 'outputs/run'); (root / 'outputs/run').symlink_to(outside, target_is_directory=True)"
            }
            "parent-link" => {
                "shutil.rmtree(root / 'outputs'); (root / 'outputs').symlink_to(outside, target_is_directory=True)"
            }
            "file-link" => "(root / 'outputs/run/run.json').symlink_to(outside / 'sentinel')",
            "inner-link" => {
                "(root / 'outputs/run/valid.json').write_text('{\"queries\":{}}'); (root / 'outputs/run/alias.json').symlink_to(root / 'outputs/run/valid.json')"
            }
            "large-log" => "print('x' * 100000, flush=True); raise SystemExit(7)",
            _ => "raise SystemExit(9)",
        };
        rig.script("negative_producer.py",&format!("import os\nimport shutil\nimport sys\nimport time\nfrom pathlib import Path\n\ndef main() -> None:\n    root = Path(os.environ['CR_ROOT'])\n    outside = root.parents[2] / 'outside'\n    {action}\n\nif __name__ == '__main__':\n    main()\n"))?;
        let number = rig.submit().await?;
        successful_process(&rig.run().await?);
        let job = rig.job(number).await?;
        assert_eq!(job["state"], "failed", "{variant}");
        assert_eq!(job["error_code"], code, "{variant}");
        assert_eq!(job["error_step"], "overlap-producer");
        assert_ne!(
            job["error_reason"]
                .as_str()
                .ok_or("failure reason missing")?
                .trim(),
            ""
        );
        assert!(!job["error_reason"].to_string().contains("PRIVATE_PAYLOAD"));
        let logs = rig.logs(&job).await?;
        if variant == "exit" {
            assert!(logs.contains("PRIVATE_PAYLOAD_STDOUT"));
            assert!(logs.contains("PRIVATE_PAYLOAD_STDERR"));
        }
        if variant == "missing-command" {
            assert!(logs.contains("cannery-conformance-no-such-command"));
        }
        if variant == "deadline" {
            assert!(logs.contains("deadline-started"));
        }
        assert_eq!(
            fs::read_to_string(rig.work.0.join("outside/sentinel"))?,
            "preserved"
        );
        rig.assert_clean()?;
        rig.finish(&format!("runner-{variant}")).await?;
    }
    Ok(())
}

const SLOW_PRODUCER: &str = r#"import os
import subprocess
import sys
import time
from pathlib import Path

def main() -> None:
    root = Path(os.environ['CR_ROOT'])
    outside = root.parents[2]
    code = 'import time; from pathlib import Path; time.sleep(15); Path(' + repr(str(outside / 'surviving-child')) + ').write_text("survived")'
    process = subprocess.Popen([sys.executable, '-c', code])
    (outside / 'child.pid').write_text(str(process.pid))
    (outside / 'started').write_text('started')
    time.sleep(90)

if __name__ == '__main__':
    main()
"#;

#[tokio::test]
#[ignore = "requires local CLI with short normal job leases"]
async fn runner_sigterm_and_disabled_worker_heartbeat_cleanup() -> Result<()> {
    for mode in ["sigterm", "disabled-worker"] {
        let mut producer = producer()?;
        producer["spec"]["container"]["command"] = json!(["python3", "slow_producer.py"]);
        let mut rig = Rig::new(&format!("runner-{mode}"), producer, science()?).await?;
        rig.script("slow_producer.py", SLOW_PRODUCER)?;
        let number = rig.submit().await?;
        let child = support::Running::new(rig.command()?.spawn()?);
        let until = std::time::Instant::now() + Duration::from_secs(20);
        while !rig.work.0.join("started").exists() {
            if std::time::Instant::now() > until {
                return Err("step did not start within bounded time".into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let output = if mode == "sigterm" {
            child.terminate()?;
            let output = child.wait(Duration::from_secs(15)).await?;
            assert_eq!(output.status.code(), Some(143));
            output
        } else {
            let initial = rig.job(number).await?;
            assert_eq!(initial["state"], "claimed");
            // A genuine renewal is observed before withdrawing the worker's
            // public service permission. No heartbeat response is injected.
            let until = std::time::Instant::now() + Duration::from_secs(30);
            loop {
                let current = rig.job(number).await?;
                if current["lease_expires_at"] != initial["lease_expires_at"] {
                    break;
                }
                if std::time::Instant::now() > until {
                    return Err("genuine job heartbeat was not observed".into());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            rig.world
                .api(lifecycle::Call::post(
                    "/api/projects/{slug}/service-accounts/{name}/disable",
                    format!(
                        "{}/service-accounts/cannery-runner/disable",
                        rig.world.base()
                    ),
                    &rig.actors.admin,
                    json!({"reason":"Exercise real worker credential withdrawal"}),
                    200,
                ))
                .await?;
            let output = child.wait(Duration::from_secs(30)).await?;
            successful_process(&output);
            assert!(
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                )
                .contains("HTTP 401")
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("abandoned"));
            output
        };
        assert!(!String::from_utf8_lossy(&output.stdout).contains(&rig.actors.tester));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(&rig.actors.tester));
        let job = rig.job(number).await?;
        assert_eq!(job["state"], "claimed");
        assert_eq!(job["error_code"], Value::Null);
        rig.assert_clean()?;
        // Work roots disappear and no delayed writer survives cancellation.
        // A zombie cannot execute or write the delayed marker; the local
        // launcher documents that a reparented child may await its reaper.
        let child_pid: u32 = fs::read_to_string(rig.work.0.join("child.pid"))?.parse()?;
        match fs::read_to_string(format!("/proc/{child_pid}/stat")) {
            Ok(stat) => assert_eq!(
                stat.rsplit_once(')')
                    .and_then(|(_, tail)| tail.split_whitespace().next()),
                Some("Z")
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        assert!(!rig.work.0.join("surviving-child").exists());
        rig.finish(&format!("runner-{mode}")).await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires local CLI and loopback GitHub fixtures"]
async fn runner_setup_keys_ignore_commit_and_include_content_environment_network() -> Result<()> {
    let github = support::GithubFixture::new().await?;
    let mut manifest = support::cached_producer(support::COMMIT_A)?;
    let mut rig = Rig::new(
        "runner-cache-keys",
        manifest.clone(),
        support::cache_science()?,
    )
    .await?;
    github
        .publish(&rig, support::COMMIT_A, "dep==1.0\n")
        .await?;
    github
        .publish(&rig, support::COMMIT_B, "dep==1.0\n")
        .await?;
    github
        .publish(&rig, support::COMMIT_C, "dep==2.0\n")
        .await?;
    github.configure(&mut rig)?;
    for (variant, count, fetches) in [
        ("initial", 1, 1),
        ("same-key-new-commit", 1, 2),
        ("changed-key", 2, 3),
        ("environment", 3, 3),
        ("network", 4, 3),
        ("network-reordered", 4, 3),
        ("run", 5, 3),
        ("image", 6, 3),
    ] {
        match variant {
            "same-key-new-commit" => manifest["spec"]["code"]["commit"] = json!(support::COMMIT_B),
            "changed-key" => manifest["spec"]["code"]["commit"] = json!(support::COMMIT_C),
            "environment" => manifest["spec"]["container"]["env"]
                .as_array_mut()
                .ok_or("env missing")?
                .push(json!({"name":"SETUP_VARIANT","value":"environment-change"})),
            "network" => {
                manifest["spec"]["setup"]["network"] =
                    json!({"egress":["pypi.org:443","files.pythonhosted.org:443"]});
            }
            "network-reordered" => {
                manifest["spec"]["setup"]["network"] =
                    json!({"egress":["files.pythonhosted.org:443","pypi.org:443"]});
            }
            "run" => {
                manifest["spec"]["setup"]["run"] = json!(
                    "mkdir -p \"$CR_ROOT/cache/site\" && cat requirements.txt > \"$CR_ROOT/cache/site/fixture.txt\" && printf rerun > \"$CR_ROOT/cache/site/extra\""
                );
            }
            "image" => {
                manifest["spec"]["container"]["image"] = json!(format!(
                    "fixture.invalid/cannery-row/overlap-producer@sha256:{}",
                    "b2".repeat(32)
                ));
            }
            _ => {}
        }
        if variant != "initial" {
            rig.bind(manifest.clone()).await?;
        }
        let number = rig.submit().await?;
        successful_process(&rig.run().await?);
        let job = rig.job(number).await?;
        assert_eq!(job["state"], "completed", "{variant}");
        assert_eq!(
            support::entries(&rig.work.0.join("cache/setup"))?,
            count,
            "{variant}"
        );
        assert_eq!(github.fetches(), fetches, "{variant}");
        let values = support::cached_values(&rig)?;
        assert_eq!(values[0], "dep==1.0\n");
        if count > 1 {
            assert!(values.contains(&"dep==2.0\n".into()));
        }
        rig.assert_clean()?;
    }
    rig.finish("runner-cache-keys").await
}

#[tokio::test]
#[ignore = "requires local CLI and loopback GitHub fixtures"]
#[allow(
    clippy::too_many_lines,
    reason = "The trust-class control and six failed publication cases share one controlled provider"
)]
async fn runner_setup_trust_classes_and_invalid_setup_never_publish() -> Result<()> {
    let github = support::GithubFixture::new().await?;
    let manifest = support::cached_producer(support::COMMIT_A)?;
    let mut science = support::cache_science()?;
    let mut scorer = science["scorer"].clone();
    for step in std::iter::once(&mut scorer).chain(
        science["validators"]
            .as_array_mut()
            .ok_or("validators missing")?
            .iter_mut(),
    ) {
        step["spec"]["code"] = manifest["spec"]["code"].clone();
        step["spec"]["setup"] = manifest["spec"]["setup"].clone();
        step["spec"]["container"]["image"] = manifest["spec"]["container"]["image"].clone();
        step["spec"]["container"]["env"] = manifest["spec"]["container"]["env"].clone();
    }
    science["scorer"] = scorer;
    let mut rig = Rig::new("runner-cache-trust", manifest, science).await?;
    github
        .publish(&rig, support::COMMIT_A, "dep==1.0\n")
        .await?;
    github.configure(&mut rig)?;
    let number = rig.submit().await?;
    successful_process(&rig.run().await?);
    assert_eq!(rig.job(number).await?["state"], "completed");
    assert_eq!(
        support::entries(&rig.work.0.join("cache/setup"))?,
        2,
        "candidate setup never serves trusted steps; scorer and validator share trust"
    );
    assert_eq!(github.fetches(), 1);
    rig.finish("runner-cache-trust").await?;
    for (variant, code) in [
        ("exit", "setup_failed"),
        ("missing-cache-path", "setup_failed"),
        ("linked-cache", "setup_failed"),
        ("deadline", "deadline_exceeded"),
        ("missing-key", "invalid_code"),
        ("missing-code-path", "invalid_code"),
    ] {
        let mut manifest = support::cached_producer(support::COMMIT_A)?;
        match variant {
            "exit" => {
                manifest["spec"]["setup"]["run"] = json!(
                    "mkdir -p \"$CR_ROOT/cache/site\"; printf half > \"$CR_ROOT/cache/site/fixture.txt\"; exit 3"
                );
            }
            "missing-cache-path" => {
                manifest["spec"]["setup"]["run"] = json!("mkdir -p \"$CR_ROOT/cache/other\"");
            }
            "linked-cache" => {
                manifest["spec"]["setup"]["run"] = json!(
                    "mkdir -p \"$CR_ROOT/cache/site\"; ln -s \"$CR_ROOT/code\" \"$CR_ROOT/cache/site/outside\""
                );
            }
            "deadline" => {
                manifest["spec"]["setup"]["activeDeadlineSeconds"] = json!(1);
                manifest["spec"]["setup"]["run"] =
                    json!("mkdir -p \"$CR_ROOT/cache/site\"; sleep 30");
            }
            "missing-key" => {
                manifest["spec"]["setup"]["cache"]["key_files"] = json!(["package-lock.json"]);
            }
            "missing-code-path" => manifest["spec"]["code"]["path"] = json!("absent"),
            _ => {}
        }
        let mut rig = Rig::new(
            &format!("runner-setup-{variant}"),
            manifest,
            support::cache_science()?,
        )
        .await?;
        github
            .publish(&rig, support::COMMIT_A, "dep==1.0\n")
            .await?;
        github.configure(&mut rig)?;
        let number = rig.submit().await?;
        successful_process(&rig.run().await?);
        let job = rig.job(number).await?;
        assert_eq!(job["state"], "failed", "{variant}");
        assert_eq!(job["error_code"], code, "{variant}");
        assert_eq!(
            support::entries(&rig.work.0.join("cache/setup"))?,
            0,
            "{variant}"
        );
        assert_eq!(
            support::entries(&rig.work.0.join("cache/tmp"))?,
            0,
            "{variant}"
        );
        if !variant.starts_with("missing-") || variant == "missing-cache-path" {
            assert!(
                job["outputs"]
                    .as_array()
                    .ok_or("outputs missing")?
                    .iter()
                    .any(|artifact| artifact["role"] == "setup_log")
            );
        }
        if variant == "exit" {
            successful_process(&rig.run().await?);
            let listed = rig
                .world
                .api(lifecycle::Call::get(
                    &format!("{}/jobs", lifecycle::ATTEMPT),
                    format!("{}/hypotheses/{number}/attempts/1/jobs", rig.world.base()),
                    &rig.actors.admin,
                ))
                .await?
                .body;
            assert_eq!(
                listed["items"]
                    .as_array()
                    .ok_or("job items missing")?
                    .iter()
                    .filter(|job| job["stage"] == "tester" && job["state"] == "failed")
                    .count(),
                2
            );
            assert_eq!(support::entries(&rig.work.0.join("cache/setup"))?, 0);
        }
        rig.assert_clean()?;
        rig.finish(&format!("runner-setup-{variant}")).await?;
    }
    Ok(())
}
