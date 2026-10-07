//! Concurrent installed import commands must serialize one immutable history.
#[path = "support/import_support.rs"]
#[allow(
    dead_code,
    reason = "Shared fixture bootstrap includes other import scenarios"
)]
mod support;

use conformance::Result;
use std::{
    fs,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use support::{PROJECT, World};

struct Worker {
    child: Child,
    output: PathBuf,
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test]
#[ignore = "requires the URL-driven API, OIDC and installed import command"]
async fn concurrent_imports_commit_one_history_and_replay_without_writes() -> Result<()> {
    let mut world = World::new().await?;
    let slug = world.slug.clone();
    let bundle = world.bundle("concurrent-history", &slug)?;
    let cli = std::env::var("CANNERY_CONFORMANCE_CLI")?;
    let (before, _) = world.audit(0).await?;
    let mut workers = Vec::new();
    for index in 0..4 {
        let output = world.directory.join(format!("concurrent-{index}.stdout"));
        let child = Command::new(&cli)
            .args(["import", "--bundle"])
            .arg(&bundle)
            .args(["--project", &slug, "--science"])
            .arg(&world.science)
            .stdin(Stdio::null())
            .stdout(fs::File::create(&output)?)
            .stderr(Stdio::null())
            .spawn()?;
        workers.push(Worker { child, output });
    }
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let mut finished = true;
        for worker in &mut workers {
            finished &= worker.child.try_wait()?.is_some();
        }
        if finished {
            break;
        }
        if Instant::now() >= deadline {
            return Err("concurrent import commands exceeded 90 seconds".into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let mut imported = 0;
    let mut replayed = 0;
    for mut worker in workers {
        if !worker.child.wait()?.success() {
            return Err("concurrent import command failed".into());
        }
        let output = fs::read_to_string(&worker.output)?;
        if output.contains("already imported, nothing to do (11 unchanged entries)") {
            replayed += 1;
        } else {
            assert!(output.contains(
                "1 policies, 2 tracks, 7 hypotheses, 6 attempts, 5 decisions, 1 reports"
            ));
            imported += 1;
        }
    }
    assert_eq!((imported, replayed), (1, 3));
    let project = world.get(PROJECT, &world.base()).await?;
    let (_, audit) = world.audit(before).await?;
    let events: Vec<_> = audit
        .iter()
        .filter(|event| {
            event["project_id"] == project["id"]
                && event["action"]
                    .as_str()
                    .is_some_and(|action| action.starts_with("import."))
        })
        .collect();
    assert_eq!(events.len(), 15);
    for (action, count) in [
        ("import.project_created", 1),
        ("import.completed", 1),
        ("import.track", 2),
        ("import.hypothesis", 7),
    ] {
        assert_eq!(
            events
                .iter()
                .filter(|event| event["action"] == action)
                .count(),
            count
        );
    }
    let snapshot = world.snapshot().await?;
    let (cursor, _) = world.audit(0).await?;
    let replay = world.run(&bundle, &slug, Some(&world.science), &[], 0)?;
    assert!(
        replay
            .stdout
            .contains("already imported, nothing to do (11 unchanged entries)")
    );
    world.unchanged(&project["id"], cursor, &snapshot).await?;
    world.finish()
}
