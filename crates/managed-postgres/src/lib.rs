//! A private PostgreSQL server supervised as a child process.
//!
//! Layout of the data directory, which one `cannery` owns at a time:
//!
//! - `lock`: held with an exclusive lock for the life of the server;
//! - `password`: the generated superuser password, mode 0600;
//! - `run/`: the Unix socket directory, mode 0700 (no TCP listener);
//! - `pgdata/`: the PostgreSQL cluster, created by `initdb` on first start;
//! - `postgresql.log`: the server's own log (appended).
//!
//! The server runs as `postgres -D pgdata` with the settings below passed on
//! its command line, so `postgresql.conf` stays as `initdb` wrote it.
#![forbid(unsafe_code)]

pub mod bundle;
mod shim;
pub mod tools;

pub use shim::{EXEC_SHIM_ARGV0, dispatch_exec_shim};

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use rustix::process::{Pid, Signal};
use sqlx::{Connection, PgConnection, postgres::PgConnectOptions};
use std::{
    fmt,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::Duration,
};
use tokio::process::{Child, Command};

/// The PostgreSQL major version this release line runs.
pub const POSTGRES_MAJOR: u32 = 17;
pub const DATABASE: &str = "cannery";
pub const SUPERUSER: &str = "cannery";
const PORT: u16 = 5432;
const SHARED_BUFFERS: &str = "32MB";
/// Connections beyond the pool: migrations, sweeps, health checks and the
/// three reserved superuser slots.
const EXTRA_CONNECTIONS: u32 = 10;
const FAST_SHUTDOWN_WAIT: Duration = Duration::from_secs(30);
const IMMEDIATE_SHUTDOWN_WAIT: Duration = Duration::from_secs(10);
/// `sun_path` holds 108 bytes on Linux and 104 on macOS, including the NUL.
const MAX_SOCKET_PATH: usize = 103;

#[derive(Debug, thiserror::Error)]
pub enum ManagedError {
    #[error(
        "PostgreSQL refuses to run as root: run cannery as an unprivileged user for a managed database"
    )]
    Root,
    #[error("database data directory {0} is in use by another cannery process")]
    Locked(PathBuf),
    #[error("{action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    #[error(
        "PostgreSQL socket path {0} is too long for a Unix socket; choose a shorter database.data_dir"
    )]
    SocketPath(PathBuf),
    #[error("no PostgreSQL binaries in {0} (expected postgres and initdb)")]
    Binaries(PathBuf),
    #[error("cannot determine the PostgreSQL version of {0}")]
    Version(PathBuf),
    #[error(
        "the data directory was initialized by PostgreSQL {data} but the binaries are PostgreSQL {binaries}; upgrade it explicitly with `cannery db upgrade --from-bin-dir <PostgreSQL {data} bin directory>`"
    )]
    MajorMismatch { data: String, binaries: String },
    #[error("{0} exists but the generated password file is missing")]
    Password(PathBuf),
    #[error("initdb failed ({status}); see {log}")]
    Initdb { status: ExitStatus, log: PathBuf },
    #[error(
        "initdb needs /bin/sh to run postgres; initialize the data directory once where a shell exists (for example a debian-slim image), then any glibc image can run it"
    )]
    NoShell,
    #[error("a previous PostgreSQL server (pid {0}) for this data directory did not stop")]
    Stale(i32),
    #[error("PostgreSQL exited during startup ({status}): {log_tail}")]
    Exited {
        status: ExitStatus,
        log_tail: String,
    },
    #[error("PostgreSQL did not accept connections within {0:?}; see its log")]
    Timeout(Duration),
    #[error("PostgreSQL setup query failed")]
    Setup,
}

fn io_error<'a>(
    action: &'static str,
    path: &'a Path,
) -> impl FnOnce(io::Error) -> ManagedError + 'a {
    move |source| ManagedError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

/// How to run the server.
#[derive(Clone, Debug)]
pub struct Config {
    /// The data directory (`database.data_dir`).
    pub data_dir: PathBuf,
    /// The directory holding `postgres` and `initdb`.
    pub bin_dir: PathBuf,
    /// The application's pool size; `max_connections` is a little above it.
    pub pool_max_size: u32,
    /// An executable that dispatches [`dispatch_exec_shim`] (normally the
    /// running `cannery` binary). On Linux the shim makes the kernel stop the
    /// server with a fast shutdown if this process dies. Without it, the
    /// server is started directly and a crash can leave it running until
    /// the next start reclaims it.
    pub exec_shim: Option<PathBuf>,
    pub startup_timeout: Duration,
}

impl Config {
    #[must_use]
    pub fn new(data_dir: PathBuf, bin_dir: PathBuf, pool_max_size: u32) -> Self {
        Self {
            data_dir,
            bin_dir,
            pool_max_size,
            exec_shim: None,
            startup_timeout: Duration::from_secs(60),
        }
    }
}

/// A running server. Dropping it without [`ManagedPostgres::shutdown`]
/// requests a fast shutdown and does not wait.
pub struct ManagedPostgres {
    child: Child,
    pid: Option<Pid>,
    socket_dir: PathBuf,
    password: String,
    log: PathBuf,
    // Released when the handle goes away, after the server is signalled.
    _lock: File,
}

impl fmt::Debug for ManagedPostgres {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedPostgres")
            .field("pid", &self.pid)
            .field("socket_dir", &self.socket_dir)
            .finish_non_exhaustive()
    }
}

struct Layout {
    root: PathBuf,
    pgdata: PathBuf,
    socket_dir: PathBuf,
    password: PathBuf,
    log: PathBuf,
}

impl Layout {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            pgdata: root.join("pgdata"),
            socket_dir: root.join("run"),
            password: root.join("password"),
            log: root.join("postgresql.log"),
        }
    }
}

impl ManagedPostgres {
    /// Locks the data directory, initializes it on first use, reclaims a
    /// server left behind by a previous process, starts PostgreSQL and waits
    /// until it accepts connections to the `cannery` database.
    ///
    /// Call it from a thread that lives as long as the server (the runtime's
    /// main or worker threads): Linux delivers the parent-death signal when
    /// the spawning thread exits.
    ///
    /// # Errors
    /// See [`ManagedError`]; every error names what to fix.
    pub async fn start(config: &Config) -> Result<Self, ManagedError> {
        if rustix::process::geteuid().is_root() {
            return Err(ManagedError::Root);
        }
        let postgres = config.bin_dir.join("postgres");
        let initdb = config.bin_dir.join("initdb");
        if !postgres.is_file() || !initdb.is_file() {
            return Err(ManagedError::Binaries(config.bin_dir.clone()));
        }
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&config.data_dir)
            .map_err(io_error("cannot create", &config.data_dir))?;
        let root = fs::canonicalize(&config.data_dir)
            .map_err(io_error("cannot resolve", &config.data_dir))?;
        let layout = Layout::new(&root);
        let lock = lock(&layout.root.join("lock"))?;
        let socket = layout.socket_dir.join(format!(".s.PGSQL.{PORT}"));
        if socket.as_os_str().len() > MAX_SOCKET_PATH {
            return Err(ManagedError::SocketPath(socket));
        }
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&layout.socket_dir)
            .or_else(|error| {
                if error.kind() == io::ErrorKind::AlreadyExists {
                    Ok(())
                } else {
                    Err(error)
                }
            })
            .and_then(|()| {
                fs::set_permissions(&layout.socket_dir, fs::Permissions::from_mode(0o700))
            })
            .map_err(io_error("cannot create", &layout.socket_dir))?;

        let binaries = binary_major(&postgres).await?;
        if layout.pgdata.join("PG_VERSION").is_file() {
            let data = fs::read_to_string(layout.pgdata.join("PG_VERSION"))
                .map_err(io_error("cannot read", &layout.pgdata))?;
            if data.trim() != binaries {
                return Err(ManagedError::MajorMismatch {
                    data: data.trim().to_owned(),
                    binaries,
                });
            }
            if !layout.password.is_file() {
                return Err(ManagedError::Password(layout.pgdata.clone()));
            }
            reclaim_stale_server(&layout).await?;
        } else {
            initialize(&layout, &initdb).await?;
        }
        let password = fs::read_to_string(&layout.password)
            .map_err(io_error("cannot read", &layout.password))?
            .trim()
            .to_owned();

        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&layout.log)
            .map_err(io_error("cannot open", &layout.log))?;
        let log_start = log
            .metadata()
            .map_err(io_error("cannot read", &layout.log))?
            .len();
        let mut command = server_command(config, &postgres, &layout);
        command
            .stdin(Stdio::null())
            .stdout(
                log.try_clone()
                    .map_err(io_error("cannot open", &layout.log))?,
            )
            .stderr(log);
        let child = command
            .spawn()
            .map_err(io_error("cannot start", &postgres))?;
        let pid = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(Pid::from_raw);
        tracing::info!(
            pid = pid.map(Pid::as_raw_nonzero),
            data_dir = %layout.root.display(),
            "managed PostgreSQL starting"
        );
        let mut server = Self {
            child,
            pid,
            socket_dir: layout.socket_dir,
            password,
            log: layout.log,
            _lock: lock,
        };
        server.wait_ready(config.startup_timeout, log_start).await?;
        tracing::info!("managed PostgreSQL ready");
        Ok(server)
    }

    /// A `postgresql://` URL for the `cannery` database over the private
    /// socket, in the form `database.url` takes. It contains the password.
    #[must_use]
    pub fn url(&self) -> String {
        self.database_url(DATABASE)
    }

    /// [`ManagedPostgres::url`] for another database of this server.
    #[must_use]
    pub fn database_url(&self, database: &str) -> String {
        let host = self.socket_dir.to_string_lossy();
        format!(
            "postgresql:///{}?host={}&port={PORT}&user={SUPERUSER}&password={}",
            utf8_percent_encode(database, NON_ALPHANUMERIC),
            utf8_percent_encode(&host, NON_ALPHANUMERIC),
            utf8_percent_encode(&self.password, NON_ALPHANUMERIC),
        )
    }

    /// Where `pg_dump` and `pg_restore` reach `database` over the socket.
    #[must_use]
    pub fn client_target(&self, database: &str) -> tools::ClientTarget {
        let mut host = std::ffi::OsString::from("--host=");
        host.push(&self.socket_dir);
        tools::ClientTarget {
            args: vec![
                host,
                format!("--port={PORT}").into(),
                format!("--username={SUPERUSER}").into(),
                format!("--dbname={database}").into(),
            ],
            password: Some(self.password.clone()),
        }
    }

    /// Connection options for `database`, independent of `PG*` variables.
    #[must_use]
    pub fn connect_options(&self, database: &str) -> PgConnectOptions {
        PgConnectOptions::new_without_pgpass()
            .socket(&self.socket_dir)
            .port(PORT)
            .username(SUPERUSER)
            .password(&self.password)
            .database(database)
    }

    /// The server's log file.
    #[must_use]
    pub fn log_path(&self) -> &Path {
        &self.log
    }

    /// Resolves when the server exits; use it to stop serving when the
    /// database is gone.
    ///
    /// # Errors
    /// Returns the wait failure.
    pub async fn exited(&mut self) -> io::Result<ExitStatus> {
        self.child.wait().await
    }

    /// Fast shutdown, then immediate shutdown if it takes too long.
    ///
    /// # Errors
    /// Returns a wait failure or [`ManagedError::Stale`] if the server
    /// outlives both requests.
    pub async fn shutdown(mut self) -> Result<(), ManagedError> {
        if self.child.try_wait().ok().flatten().is_some() {
            return Ok(());
        }
        for (signal, wait) in [
            (Signal::INT, FAST_SHUTDOWN_WAIT),
            (Signal::QUIT, IMMEDIATE_SHUTDOWN_WAIT),
        ] {
            if let Some(pid) = self.pid {
                let _ = rustix::process::kill_process(pid, signal);
            }
            if let Ok(status) = tokio::time::timeout(wait, self.child.wait()).await {
                status.map_err(io_error("cannot wait for", &self.socket_dir))?;
                tracing::info!("managed PostgreSQL stopped");
                return Ok(());
            }
        }
        Err(ManagedError::Stale(
            self.pid.map_or(0, |pid| pid.as_raw_nonzero().get()),
        ))
    }

    async fn wait_ready(&mut self, timeout: Duration, log_start: u64) -> Result<(), ManagedError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(ManagedError::Exited {
                    status,
                    log_tail: log_tail(&self.log, log_start),
                });
            }
            let attempt = tokio::time::timeout(
                Duration::from_secs(2),
                PgConnection::connect_with(&self.connect_options("postgres")),
            )
            .await;
            if let Ok(Ok(mut connection)) = attempt {
                let result = ensure_database(&mut connection).await;
                let _ = connection.close().await;
                return result;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(ManagedError::Timeout(timeout));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

impl Drop for ManagedPostgres {
    fn drop(&mut self) {
        if let (Some(pid), Ok(None)) = (self.pid, self.child.try_wait()) {
            let _ = rustix::process::kill_process(pid, Signal::INT);
        }
    }
}

fn lock(path: &Path) -> Result<File, ManagedError> {
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(io_error("cannot open", path))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => Err(ManagedError::Locked(
            path.parent().unwrap_or(path).to_path_buf(),
        )),
        Err(fs::TryLockError::Error(error)) => Err(io_error("cannot lock", path)(error)),
    }
}

fn server_command(config: &Config, postgres: &Path, layout: &Layout) -> Command {
    let mut command = if let Some(shim) = &config.exec_shim {
        let mut command = Command::new(shim);
        command.arg0(EXEC_SHIM_ARGV0).arg(postgres);
        command
    } else {
        Command::new(postgres)
    };
    let max_connections = config.pool_max_size.saturating_add(EXTRA_CONNECTIONS);
    command.arg("-D").arg(&layout.pgdata);
    // A list setting: quoted so spaces and commas in the path survive.
    let socket_dir = layout.socket_dir.to_string_lossy().replace('"', "\"\"");
    for setting in [
        "listen_addresses=".to_owned(),
        format!("port={PORT}"),
        format!("unix_socket_directories=\"{socket_dir}\""),
        "unix_socket_permissions=0700".to_owned(),
        format!("max_connections={max_connections}"),
        format!("shared_buffers={SHARED_BUFFERS}"),
        "dynamic_shared_memory_type=mmap".to_owned(),
        "log_destination=stderr".to_owned(),
        "logging_collector=off".to_owned(),
    ] {
        command.arg("-c").arg(setting);
    }
    // The server reads nothing from the environment that cannery needs to
    // pass on; a clean environment keeps PG* variables and locales out.
    command.env_clear().env("LC_ALL", "C");
    if config.exec_shim.is_some() {
        command.env(shim::PARENT_ENV, std::process::id().to_string());
    }
    // Its own process group: a terminal's Ctrl-C reaches only cannery, which
    // stops serving first and then shuts the server down.
    command.process_group(0);
    command
}

/// The PostgreSQL major version of the `postgres` binary in `bin_dir`.
///
/// # Errors
/// [`ManagedError::Binaries`] or [`ManagedError::Version`].
pub async fn binaries_major(bin_dir: &Path) -> Result<String, ManagedError> {
    let postgres = bin_dir.join("postgres");
    if !postgres.is_file() {
        return Err(ManagedError::Binaries(bin_dir.to_path_buf()));
    }
    binary_major(&postgres).await
}

/// The PostgreSQL major version that initialized the cluster under
/// `data_dir` (`pgdata/PG_VERSION`), or `None` before the first start.
///
/// # Errors
/// An unreadable `PG_VERSION`.
pub fn data_major(data_dir: &Path) -> Result<Option<String>, ManagedError> {
    let path = Layout::new(data_dir).pgdata.join("PG_VERSION");
    match fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text.trim().to_owned())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error("cannot read", &path)(error)),
    }
}

async fn binary_major(postgres: &Path) -> Result<String, ManagedError> {
    let output = Command::new(postgres)
        .arg("--version")
        .env_clear()
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(io_error("cannot run", postgres))?;
    // "postgres (PostgreSQL) 17.11"
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .last()
        .and_then(|version| version.split('.').next())
        .filter(|major| !major.is_empty() && major.bytes().all(|byte| byte.is_ascii_digit()))
        .map(str::to_owned)
        .ok_or_else(|| ManagedError::Version(postgres.to_path_buf()))
}

/// `initdb` into a sibling directory and rename, so an interrupted first
/// start never leaves a half-initialized `pgdata`.
async fn initialize(layout: &Layout, initdb: &Path) -> Result<(), ManagedError> {
    // initdb starts `postgres` through popen(3). A distroless image has no
    // shell; the server itself does not need one.
    if !Path::new("/bin/sh").exists() {
        return Err(ManagedError::NoShell);
    }
    let staging = layout.root.join("pgdata.initdb");
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(io_error("cannot remove", &staging))?;
    }
    write_password(&layout.password)?;
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&layout.log)
        .map_err(io_error("cannot open", &layout.log))?;
    tracing::info!(data_dir = %layout.root.display(), "initializing managed PostgreSQL");
    let status = Command::new(initdb)
        .arg("-D")
        .arg(&staging)
        .arg(format!("--username={SUPERUSER}"))
        .arg(format!("--pwfile={}", layout.password.display()))
        .args([
            "--auth=scram-sha-256",
            "--encoding=UTF8",
            "--locale-provider=builtin",
            "--builtin-locale=C.UTF-8",
            "--locale=C",
            // Collation stays codepoint order, but character classes are
            // UTF-8: with ctype C, full-text search and pg_trgm would not
            // lowercase or index non-ASCII letters ("École" ≠ "école").
            "--lc-ctype=C.UTF-8",
            "--no-instructions",
        ])
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(
            log.try_clone()
                .map_err(io_error("cannot open", &layout.log))?,
        )
        .stderr(log)
        .status()
        .await
        .map_err(io_error("cannot run", initdb))?;
    if !status.success() {
        let _ = fs::remove_dir_all(&staging);
        return Err(ManagedError::Initdb {
            status,
            log: layout.log.clone(),
        });
    }
    fs::rename(&staging, &layout.pgdata).map_err(io_error("cannot rename", &staging))
}

fn write_password(path: &Path) -> Result<(), ManagedError> {
    let mut bytes = [0_u8; 24];
    getrandom::fill(&mut bytes).map_err(|_| ManagedError::Io {
        action: "cannot generate a password for",
        path: path.to_path_buf(),
        source: io::Error::other("no entropy"),
    })?;
    let password = bundle::hex(&bytes);
    let staging = path.with_extension("tmp");
    let _ = fs::remove_file(&staging);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&staging)
        .map_err(io_error("cannot write", &staging))?;
    file.write_all(password.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(io_error("cannot write", &staging))?;
    fs::rename(&staging, path).map_err(io_error("cannot write", path))
}

/// The first line of `postmaster.pid`, if the file exists and parses.
fn recorded_pid(pgdata: &Path) -> Option<Pid> {
    fs::read_to_string(pgdata.join("postmaster.pid"))
        .ok()?
        .lines()
        .next()?
        .trim()
        .parse::<i32>()
        .ok()
        .and_then(Pid::from_raw)
}

/// Whether `pid` is a PostgreSQL process serving this data directory. Every
/// server process runs with the data directory as its working directory:
/// Linux shows it in `/proc/<pid>/cwd`, macOS through `lsof`, and a zombie or
/// an exited process has none. Only where neither works is a live socket in
/// our private directory the evidence instead.
async fn serves_this_cluster(pid: Pid, layout: &Layout) -> bool {
    if cfg!(target_os = "linux") {
        return fs::read_link(format!("/proc/{}/cwd", pid.as_raw_nonzero()))
            .is_ok_and(|cwd| cwd == layout.pgdata);
    }
    match lsof_cwd(pid).await {
        Ok(cwd) => cwd.is_some_and(|cwd| cwd == layout.pgdata),
        Err(_) => std::os::unix::net::UnixStream::connect(
            layout.socket_dir.join(format!(".s.PGSQL.{PORT}")),
        )
        .is_ok(),
    }
}

/// The working directory of `pid` as `/usr/sbin/lsof` reports it: macOS has
/// no `/proc`, and asking the kernel directly (`proc_pidinfo`) needs FFI.
/// `Ok(None)` when the process has none (exited, a zombie, or not ours to
/// inspect); an error when `lsof` cannot run.
async fn lsof_cwd(pid: Pid) -> io::Result<Option<PathBuf>> {
    let output = Command::new("/usr/sbin/lsof")
        .args(["-w", "-a", "-d", "cwd", "-Fn", "-p"])
        .arg(pid.as_raw_nonzero().to_string())
        .env_clear()
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await?;
    // Field output is "p<pid>", "fcwd", "n<path>", or nothing on no match.
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix('n'))
        .filter(|path| path.starts_with('/'))
        .map(PathBuf::from))
}

/// A server left by a previous `cannery` (its parent-death signal missed, or
/// on macOS, which has none) is stopped. A `postmaster.pid` naming a process
/// that is no longer PostgreSQL (the PID was reused) is removed, because
/// PostgreSQL itself would refuse to start.
async fn reclaim_stale_server(layout: &Layout) -> Result<(), ManagedError> {
    let Some(pid) = recorded_pid(&layout.pgdata) else {
        return Ok(());
    };
    if rustix::process::test_kill_process(pid).is_err() {
        // Gone (or not ours to signal): PostgreSQL clears its own stale file
        // when the PID is gone; remove it for the EPERM case.
        if rustix::process::test_kill_process(pid) != Err(rustix::io::Errno::SRCH) {
            remove_pid_file(layout)?;
        }
        return Ok(());
    }
    if !serves_this_cluster(pid, layout).await {
        tracing::warn!(
            pid = pid.as_raw_nonzero(),
            "removing postmaster.pid that names an unrelated process"
        );
        return remove_pid_file(layout);
    }
    tracing::warn!(
        pid = pid.as_raw_nonzero(),
        "stopping a PostgreSQL server left by a previous cannery process"
    );
    for (signal, wait) in [
        (Signal::INT, FAST_SHUTDOWN_WAIT),
        (Signal::QUIT, IMMEDIATE_SHUTDOWN_WAIT),
    ] {
        let _ = rustix::process::kill_process(pid, signal);
        let deadline = tokio::time::Instant::now() + wait;
        while tokio::time::Instant::now() < deadline {
            if rustix::process::test_kill_process(pid).is_err()
                || !serves_this_cluster(pid, layout).await
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    Err(ManagedError::Stale(pid.as_raw_nonzero().get()))
}

fn remove_pid_file(layout: &Layout) -> Result<(), ManagedError> {
    let path = layout.pgdata.join("postmaster.pid");
    match fs::remove_file(&path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            Err(io_error("cannot remove", &path)(error))
        }
        _ => Ok(()),
    }
}

async fn ensure_database(connection: &mut PgConnection) -> Result<(), ManagedError> {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(DATABASE)
            .fetch_one(&mut *connection)
            .await
            .map_err(|_| ManagedError::Setup)?;
    if !exists {
        sqlx::query(&format!("CREATE DATABASE {DATABASE}"))
            .execute(&mut *connection)
            .await
            .map_err(|_| ManagedError::Setup)?;
    }
    Ok(())
}

/// The last lines this start wrote to the server log, for error messages.
fn log_tail(path: &Path, start: u64) -> String {
    let mut text = String::new();
    if let Ok(mut file) = File::open(path) {
        let _ = file.seek(SeekFrom::Start(start));
        let _ = file.take(64 << 10).read_to_string(&mut text);
    }
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(5)..].join(" | ")
}

/// Default data directory: `$XDG_DATA_HOME/cannery/postgres` (else
/// `~/.local/share/...`) on Linux, `~/Library/Application Support/Cannery/postgres`
/// on macOS.
#[must_use]
pub fn default_data_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return home().map(|home| home.join("Library/Application Support/Cannery/postgres"));
    }
    xdg("XDG_DATA_HOME", ".local/share").map(|base| base.join("cannery/postgres"))
}

/// Default cache for unpacked bundles: `$XDG_CACHE_HOME/cannery/postgresql`
/// (else `~/.cache/...`; `~/Library/Caches/Cannery/postgresql` on macOS).
#[must_use]
pub fn default_cache_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return home().map(|home| home.join("Library/Caches/Cannery/postgresql"));
    }
    xdg("XDG_CACHE_HOME", ".cache").map(|base| base.join("cannery/postgresql"))
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
}

fn xdg(variable: &str, fallback: &str) -> Option<PathBuf> {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home().map(|home| home.join(fallback)))
}

#[cfg(test)]
mod tests {
    use super::{Layout, ManagedError, lock, log_tail, recorded_pid};
    use std::path::PathBuf;

    fn scratch(name: &str) -> Result<PathBuf, std::io::Error> {
        let path = std::env::temp_dir().join(format!(
            "cannery-managed-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)?;
        Ok(path)
    }

    #[test]
    fn a_second_lock_on_the_data_directory_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        let root = scratch("lock")?;
        let first = lock(&root.join("lock"))?;
        assert!(matches!(
            lock(&root.join("lock")),
            Err(ManagedError::Locked(path)) if path == root
        ));
        drop(first);
        assert!(lock(&root.join("lock")).is_ok());
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn postmaster_pid_and_log_tail_are_read_defensively() -> Result<(), Box<dyn std::error::Error>>
    {
        let root = scratch("pid")?;
        let layout = Layout::new(&root);
        std::fs::create_dir_all(&layout.pgdata)?;
        assert!(recorded_pid(&layout.pgdata).is_none());
        std::fs::write(layout.pgdata.join("postmaster.pid"), "not a pid\n")?;
        assert!(recorded_pid(&layout.pgdata).is_none());
        std::fs::write(layout.pgdata.join("postmaster.pid"), "4242\n/data\n")?;
        assert_eq!(
            recorded_pid(&layout.pgdata).map(|pid| pid.as_raw_nonzero().get()),
            Some(4242)
        );
        std::fs::write(&layout.log, "old\na\nb\nc\nd\ne\nf\n")?;
        assert_eq!(log_tail(&layout.log, 4), "b | c | d | e | f");
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }
}
