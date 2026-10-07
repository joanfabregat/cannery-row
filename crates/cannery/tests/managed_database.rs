//! `provider = "managed"` through the installed CLI: `migrate`, `serve` with
//! a healthy database, and the server stopping when `cannery` is killed.
//!
//! Run with `CANNERY_MANAGED_POSTGRES_BIN_DIR` naming a directory with
//! PostgreSQL 17 `postgres` and `initdb`, as a non-root user:
//! `cargo test -p cannery --test managed_database -- --ignored`.
#![cfg(unix)]

use std::{
    error::Error,
    io::{Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

fn cannery(data_dir: &Path) -> Result<Command> {
    let bin_dir = std::env::var("CANNERY_MANAGED_POSTGRES_BIN_DIR")?;
    let mut command = Command::new(env!("CARGO_BIN_EXE_cannery"));
    command
        .env_clear()
        .env("CANNERY_DATABASE_PROVIDER", "managed")
        .env("CANNERY_DATABASE_DATA_DIR", data_dir)
        .env("CANNERY_DATABASE_POSTGRES_BIN_DIR", bin_dir)
        .env("CANNERY_DATABASE_POOL_MAX_SIZE", "4")
        .env("CANNERY_STORAGE_LOCAL_ROOT", data_dir.join("objects"))
        .env("CANNERY_SWEEPS_ENABLED", "false");
    Ok(command)
}

fn health(port: u16) -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream
        .write_all(b"GET /api/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    Some(response)
}

fn postmaster_pid(data_dir: &Path) -> Option<i32> {
    std::fs::read_to_string(data_dir.join("pgdata/postmaster.pid"))
        .ok()?
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
}

fn alive(pid: i32) -> bool {
    if cfg!(target_os = "linux") {
        // A zombie has no working directory; only a running process does.
        return std::fs::read_link(format!("/proc/{pid}/cwd")).is_ok();
    }
    // Elsewhere the postmaster is never our zombie: its parent was cannery.
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

struct Killed(Child);
impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "requires PostgreSQL binaries in CANNERY_MANAGED_POSTGRES_BIN_DIR"]
fn managed_database_through_the_cli() -> Result {
    let data_dir: PathBuf = std::env::temp_dir().join(format!("cmc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);

    let output = cannery(&data_dir)?.arg("migrate").output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8(output.stdout)?.starts_with("applied "));
    assert!(
        postmaster_pid(&data_dir).is_none(),
        "migrate left the server running"
    );

    let port = 18_000 + u16::try_from(std::process::id() % 1000)?;
    let started = Instant::now();
    let mut server = Killed(
        cannery(&data_dir)?
            .args(["serve", "--host", "127.0.0.1", "--port", &port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    assert!(
        wait_until(Duration::from_secs(60), || {
            health(port).is_some_and(|response| response.starts_with("HTTP/1.1 200"))
        }),
        "serve did not become healthy"
    );
    eprintln!("serve healthy after {:?}", started.elapsed());

    // Killing cannery outright must not leave PostgreSQL running (Linux).
    let pid = postmaster_pid(&data_dir).ok_or("no postmaster.pid")?;
    assert!(alive(pid));
    server.0.kill()?;
    server.0.wait()?;
    if cfg!(target_os = "linux") {
        assert!(
            wait_until(Duration::from_secs(30), || !alive(pid)),
            "the postmaster outlived cannery"
        );
    }

    // The next command starts cleanly on the same directory.
    let output = cannery(&data_dir)?.arg("migrate").output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout)?,
        "applied 0 migration(s)\n"
    );
    // Without a parent-death signal (macOS) the leftover server was found
    // through postmaster.pid and stopped by that start.
    assert!(!alive(pid), "the leftover postmaster was not reclaimed");
    std::fs::remove_dir_all(&data_dir)?;
    Ok(())
}
