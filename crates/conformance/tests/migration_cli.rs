//! Idempotence through the installed migration command, including concurrent callers.
use conformance::Harness;
use reqwest::Method;
use std::{
    error::Error,
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

fn migrate(cli: &str) -> Result<()> {
    let mut child = Command::new(cli)
        .arg("migrate")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if child.try_wait()?.is_some() {
            let output = child.wait_with_output()?;
            if !output.status.success() {
                return Err("migration command refused the already migrated database".into());
            }
            if String::from_utf8(output.stdout)?.trim() != "applied 0 migration(s)" {
                return Err("migration command unexpectedly reapplied migrations".into());
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Err("migration command exceeded 20 seconds".into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[tokio::test]
#[ignore = "requires the migrated conformance database and installed command"]
async fn migration_command_repeated_and_concurrent_is_idempotent() -> Result<()> {
    let cli = std::env::var("CANNERY_CONFORMANCE_CLI")?;
    migrate(&cli)?;
    migrate(&cli)?;
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let cli = cli.clone();
            thread::spawn(move || migrate(&cli))
        })
        .collect();
    for worker in workers {
        worker.join().map_err(|_| "migration worker panicked")??;
    }
    let mut harness = Harness::new(&std::env::var("CANNERY_CONFORMANCE_URL")?)?;
    let response = harness.request(Method::GET, "/api/health")?.send().await?;
    let checked = harness
        .check_response(Method::GET, "/api/health", response, 200)
        .await?;
    assert_eq!(checked.body["status"], "ok");
    if let Ok(directory) = std::env::var("CANNERY_CONFORMANCE_COVERAGE_DIR") {
        fs::write(
            PathBuf::from(directory).join("migration-cli.json"),
            serde_json::to_vec_pretty(harness.coverage())?,
        )?;
    }
    Ok(())
}
