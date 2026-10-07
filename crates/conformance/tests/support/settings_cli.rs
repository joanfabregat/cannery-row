use reqwest::Client;
use serde_json::Value;
use std::{
    error::Error,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
struct Work(PathBuf);
impl Work {
    fn new() -> Result<Self> {
        let root = PathBuf::from(std::env::var("CANNERY_CONFORMANCE_WORK_DIR")?).join(format!(
            "settings-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        fs::create_dir(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        Ok(Self(root))
    }
    fn file(&self, name: &str, contents: &str) -> Result<PathBuf> {
        let path = self.0.join(name);
        fs::write(&path, contents)?;
        Ok(path)
    }
}
impl Drop for Work {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn command() -> Result<Command> {
    let mut command = Command::new(std::env::var("CANNERY_CONFORMANCE_CLI")?);
    for (name, _) in std::env::vars().filter(|(name, _)| {
        name.starts_with("CANNERY_") || name.starts_with("AWS_") || name.starts_with("S3_")
    }) {
        command.env_remove(name);
    }
    command.stdin(Stdio::null());
    Ok(command)
}
fn run(case: &str, args: &[&str], env: &[(&str, &str)]) -> Result<Output> {
    let mut command = command()?;
    command
        .args(args)
        .envs(env.iter().copied())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        if Instant::now() > deadline {
            child.kill()?;
            let _ = child.wait();
            return Err(format!("settings CLI case {case} exceeded 15 seconds").into());
        }
        thread::sleep(Duration::from_millis(20));
    }
}
fn refused(output: &Output, status: i32, fragment: &str) {
    assert_eq!(
        output.status.code(),
        Some(status),
        "CLI refusal exit status"
    );
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains(fragment),
        "CLI error omitted expected path/category {fragment}"
    );
    for secret in [
        "settings-sentinel-secret",
        "settings-sentinel-password",
        "cr_svc_settings_sentinel",
    ] {
        assert!(
            !error.contains(secret),
            "CLI error disclosed synthetic secret"
        );
    }
}
fn toml(value: &str) -> String {
    Value::String(value.to_owned()).to_string()
}
struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
async fn start(file: &Path, env: &[(&str, &str)]) -> Result<Server> {
    // Refuse an existing listener so a stale server cannot satisfy readiness.
    drop(std::net::TcpListener::bind("127.0.0.1:9012")?);
    let child = command()?
        .args([
            "--settings",
            file.to_str().ok_or("settings path encoding")?,
            "serve",
            "--host",
            "127.0.0.1",
            "--port",
            "9012",
        ])
        .envs(env.iter().copied())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut server = Server(child);
    let client = Client::builder()
        .timeout(Duration::from_millis(300))
        .build()?;
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        if server.0.try_wait()?.is_some() {
            return Err("isolated settings server exited during startup".into());
        }
        if let Ok(response) = client.get("http://127.0.0.1:9012/api/health").send().await
            && response.status() == 200
        {
            return Ok(server);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err("isolated settings server did not become healthy".into())
}
