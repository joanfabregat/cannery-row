//! `pg_dump` and `pg_restore` as child processes, for `cannery db`.
//!
//! The password never appears on a command line: it reaches the tool as
//! `PGPASSWORD` in the child's environment only, which starts empty so no
//! `PG*` variable of the caller changes the target.
use std::{
    ffi::OsString,
    fs::{self, File},
    io,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::process::Command;

/// Lines of a failing tool's standard error kept in the error message.
const STDERR_LINES: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("no {tool} in {dir}")]
    Missing { tool: &'static str, dir: PathBuf },
    #[error("{action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    #[error("{0} is not a file path")]
    Path(PathBuf),
    #[error("{tool} failed ({status}): {stderr}")]
    Failed {
        tool: &'static str,
        status: std::process::ExitStatus,
        stderr: String,
    },
}

fn io_error<'a>(action: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> ToolError + 'a {
    move |source| ToolError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

/// Where a client tool connects: libpq connection arguments plus the
/// password, which is passed through the environment.
#[derive(Clone)]
pub struct ClientTarget {
    pub args: Vec<OsString>,
    pub password: Option<String>,
}

impl std::fmt::Debug for ClientTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientTarget")
            .field("args", &self.args)
            .field("password", &self.password.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

/// `dir/name` if it is a file.
///
/// # Errors
/// [`ToolError::Missing`] names the directory.
pub fn tool(dir: &Path, name: &'static str) -> Result<PathBuf, ToolError> {
    let path = dir.join(name);
    if path.is_file() {
        Ok(path)
    } else {
        Err(ToolError::Missing {
            tool: name,
            dir: dir.to_path_buf(),
        })
    }
}

/// The first directory of a `PATH`-style list that holds a file `name`.
#[must_use]
pub fn find_in_path(name: &str, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path?)
        .filter(|dir| dir.is_absolute())
        .find(|dir| dir.join(name).is_file())
}

fn client_command(program: &Path, target: &ClientTarget) -> Command {
    let mut command = Command::new(program);
    command.env_clear().env("LC_ALL", "C");
    // libpq reads ~/.pgpass and ~/.postgresql/ certificates through HOME.
    if let Some(home) = std::env::var_os("HOME") {
        command.env("HOME", home);
    }
    if let Some(password) = &target.password {
        command.env("PGPASSWORD", password);
    }
    command
        .arg("--no-password")
        .args(&target.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command
}

async fn run(tool: &'static str, mut command: Command) -> Result<(), ToolError> {
    let output = command.output().await.map_err(|source| ToolError::Io {
        action: "cannot run",
        path: PathBuf::from(tool),
        source,
    })?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lines: Vec<&str> = stderr.lines().filter(|line| !line.is_empty()).collect();
    Err(ToolError::Failed {
        tool,
        status: output.status,
        stderr: lines[lines.len().saturating_sub(STDERR_LINES)..].join(" | "),
    })
}

/// The temporary sibling a dump is written to before it is renamed.
fn staging_path(output: &Path) -> Result<PathBuf, ToolError> {
    let name = output
        .file_name()
        .ok_or_else(|| ToolError::Path(output.to_path_buf()))?;
    let mut staging = OsString::from(".");
    staging.push(name);
    staging.push(format!(".{}.partial", std::process::id()));
    Ok(output.with_file_name(staging))
}

/// Writes a custom-format, uncompressed dump of `target` to `output`
/// atomically: `pg_dump` writes a mode-0600 sibling that is synced and then
/// renamed over `output`, so a failed dump never leaves a truncated file.
///
/// The bundled PostgreSQL is built without zlib, so the archive is written
/// with `--compress=0` whatever binaries are used; compress the file
/// separately if needed.
///
/// # Errors
/// I/O failures and `pg_dump`'s own errors (the tail of its stderr).
pub async fn dump(pg_dump: &Path, target: &ClientTarget, output: &Path) -> Result<(), ToolError> {
    let staging = staging_path(output)?;
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&staging)
        .map_err(io_error("cannot create", &staging))?;
    let mut command = client_command(pg_dump, target);
    command
        .args(["--format=custom", "--compress=0"])
        .arg("--file")
        .arg(&staging);
    let result = async {
        run("pg_dump", command).await?;
        file.sync_all().map_err(io_error("cannot sync", &staging))?;
        fs::rename(&staging, output).map_err(io_error("cannot write", output))?;
        let parent = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        // Persist the rename; a directory that cannot be opened is not fatal.
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    result
}

/// Checks that `input` is an archive this `pg_restore` can read, before
/// anything is changed.
///
/// # Errors
/// `pg_restore --list`'s own errors.
pub async fn check_archive(pg_restore: &Path, input: &Path) -> Result<(), ToolError> {
    if !input.is_file() {
        return Err(ToolError::Io {
            action: "cannot read",
            path: input.to_path_buf(),
            source: io::Error::from(io::ErrorKind::NotFound),
        });
    }
    let mut command = Command::new(pg_restore);
    command
        .env_clear()
        .env("LC_ALL", "C")
        .arg("--list")
        .arg(input)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    run("pg_restore", command).await
}

/// Restores `input` into the (empty) database `target` names, in one
/// transaction, without the archive's owners and privileges.
///
/// # Errors
/// `pg_restore`'s own errors; the transaction is rolled back.
pub async fn restore(
    pg_restore: &Path,
    target: &ClientTarget,
    input: &Path,
) -> Result<(), ToolError> {
    let mut command = client_command(pg_restore, target);
    command
        .args([
            "--exit-on-error",
            "--single-transaction",
            "--no-owner",
            "--no-privileges",
        ])
        .arg(input);
    run("pg_restore", command).await
}

#[cfg(test)]
mod tests {
    use super::{ClientTarget, find_in_path, staging_path, tool};
    use std::path::{Path, PathBuf};

    #[test]
    fn staging_is_a_hidden_sibling() -> Result<(), super::ToolError> {
        let staging = staging_path(Path::new("/backups/cannery.dump"))?;
        assert_eq!(staging.parent(), Some(Path::new("/backups")));
        let name = staging
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        assert!(
            name.is_some_and(
                |name| name.starts_with(".cannery.dump.") && name.ends_with(".partial")
            )
        );
        assert!(staging_path(Path::new("/")).is_err());
        assert!(staging_path(Path::new("backups/..")).is_err());
        Ok(())
    }

    #[test]
    fn tools_are_found_in_a_directory_or_path() -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("cmp-tools-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a"))?;
        std::fs::create_dir_all(root.join("b"))?;
        std::fs::write(root.join("b/pg_dump"), "")?;
        assert!(tool(&root.join("a"), "pg_dump").is_err());
        assert_eq!(tool(&root.join("b"), "pg_dump")?, root.join("b/pg_dump"));
        let path =
            std::env::join_paths([PathBuf::from("relative"), root.join("a"), root.join("b")])?;
        assert_eq!(find_in_path("pg_dump", Some(&path)), Some(root.join("b")));
        assert_eq!(find_in_path("pg_restore", Some(&path)), None);
        assert_eq!(find_in_path("pg_dump", None), None);
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn the_password_is_redacted() {
        let target = ClientTarget {
            args: vec!["--host=/run".into()],
            password: Some("hunter2".to_owned()),
        };
        assert!(!format!("{target:?}").contains("hunter2"));
    }
}
