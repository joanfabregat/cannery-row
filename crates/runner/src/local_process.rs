//! Prepared local processes. This is deliberately not an isolation boundary.
use crate::{cancellation::CancellationEvent, launcher::Outcome};
use cannery_core::text;
use nix::{
    spawn::{PosixSpawnAttr, PosixSpawnFileActions, PosixSpawnFlags, posix_spawn},
    sys::{
        signal::SigSet,
        wait::{WaitPidFlag, WaitStatus, waitpid},
    },
};
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use rustix::process::{Pid, Signal};
use std::{
    ffi::{CString, OsStr, OsString},
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::{OsStrExt, OsStringExt},
            net::UnixStream,
        },
    },
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Notify,
};

const BOOTSTRAP_ARGV0: &str = "cannery-private-local-bootstrap-v1";
const MAGIC: &[u8; 16] = b"CANNERY_LOCAL_V1";

/// Sanitized source exception classes and infrastructure failures; no native error chain.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LocalError {
    #[error("local execution requires explicit acknowledgement")]
    Acknowledgement,
    #[error("local command is empty")]
    EmptyCommand,
    #[error("local process value cannot be encoded")]
    Encoding,
    #[error("local process value is invalid")]
    Value,
    #[error("local process deadline cannot be converted to a float")]
    Overflow,
    #[error("local process filesystem operation failed")]
    Io { errno: Option<i32> },
    #[error("local process bootstrap failed")]
    Bootstrap,
    #[error("local process supervisor was abandoned")]
    Abandoned,
}
fn io_error(error: io::Error) -> LocalError {
    let result = LocalError::Io {
        errno: error.raw_os_error(),
    };
    drop(error);
    result
}

/// Already prepared paths. No copying, mounts or directory creation occurs here.
pub struct PreparedRequest {
    pub command: Vec<String>,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub root: PathBuf,
    pub code_dir: PathBuf,
    pub log_path: PathBuf,
    pub deadline_seconds: Deadline,
}
/// Source accepts integers as well as floats. Integer conversion is deferred
/// until the subprocess has started, at the asyncio.wait timeout boundary.
#[derive(Clone)]
pub enum Deadline {
    Float(f64),
    Integer(BigInt),
}
impl From<f64> for Deadline {
    fn from(value: f64) -> Self {
        Self::Float(value)
    }
}
impl Deadline {
    fn seconds(&self) -> Result<f64, LocalError> {
        match self {
            Self::Float(value) => Ok(*value),
            Self::Integer(value) => value
                .to_f64()
                .filter(|value| value.is_finite())
                .ok_or(LocalError::Overflow),
        }
    }
}
impl std::fmt::Debug for PreparedRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PreparedRequest([redacted])")
    }
}
#[derive(Clone)]
pub struct LocalProcessBackend {
    binary: PathBuf,
    interpreter: PathBuf,
}
impl LocalProcessBackend {
    /// # Errors
    /// Refuses execution without the source's explicit unisolated acknowledgement.
    pub fn new(
        binary: PathBuf,
        interpreter: PathBuf,
        unisolated: bool,
    ) -> Result<Self, LocalError> {
        if !unisolated {
            return Err(LocalError::Acknowledgement);
        }
        Ok(Self {
            binary,
            interpreter,
        })
    }
    /// # Errors
    /// Empty command has the source's pre-log failure precedence.
    pub fn argv(&self, command: &[String], args: &[String]) -> Result<Vec<String>, LocalError> {
        let (head, tail) = command.split_first().ok_or(LocalError::EmptyCommand)?;
        let first = if head.equals_utf8("python") || head.equals_utf8("python3") {
            filesystem_text(self.interpreter.as_os_str())?
        } else {
            head.clone()
        };
        Ok(std::iter::once(first)
            .chain(tail.iter().cloned())
            .chain(args.iter().cloned())
            .collect())
    }
    /// # Errors
    /// Rejects native paths that cannot be represented in the UTF-8 environment.
    pub fn environment(
        root: &Path,
        declared: &[(String, String)],
        path: Option<&OsStr>,
    ) -> Result<Vec<(String, String)>, LocalError> {
        let mut result = Vec::new();
        for (key, value) in declared {
            put_env(&mut result, key.clone(), value.clone());
        }
        let fixed = [
            (
                "PATH",
                filesystem_text(path.unwrap_or(OsStr::new("/bin:/usr/bin")))?,
            ),
            ("LANG", String::from("C.UTF-8")),
            ("HOME", filesystem_text(root.as_os_str())?),
            ("TMPDIR", filesystem_text(root.join("tmp").as_os_str())?),
            ("PYTHONDONTWRITEBYTECODE", String::from("1")),
            ("CR_ROOT", filesystem_text(root.as_os_str())?),
        ];
        for (key, value) in fixed {
            put_env(&mut result, String::from(key), value);
        }
        Ok(result)
    }
    /// Start an authoritative supervisor in the current Tokio runtime. Dropping
    /// the result handle requests abandonment; retain its settlement barrier and
    /// await it before shutting down the runtime or removing prepared paths.
    #[must_use]
    pub fn run_prepared(&self, request: PreparedRequest, cancel: CancellationEvent) -> RunHandle {
        let shared = Arc::new(Shared::default());
        let abandon = CancellationEvent::new();
        let handle = RunHandle {
            shared: shared.clone(),
            abandon: abandon.clone(),
        };
        let backend = self.clone();
        tokio::spawn(async move {
            let completion = Completion(shared);
            let result = backend.supervise(request, cancel, abandon).await;
            completion.publish(result);
        });
        handle
    }
    async fn supervise(
        &self,
        request: PreparedRequest,
        cancel: CancellationEvent,
        abandon: CancellationEvent,
    ) -> Result<Outcome, LocalError> {
        let argv = self.argv(&request.command, &request.args)?;
        let parent_path = std::env::var_os("PATH");
        let environment = Self::environment(&request.root, &request.env, parent_path.as_deref())?;
        // Source opens/truncates the log before subprocess encoding/validation.
        let mut log = File::create(&request.log_path).map_err(io_error)?;
        let packet = packet(&argv, &environment, &request.code_dir)?;
        let (parent, child) = UnixStream::pair().map_err(io_error)?;
        parent.set_nonblocking(true).map_err(io_error)?;
        let mut control = tokio::net::UnixStream::from_std(parent).map_err(io_error)?;
        let raw = spawn_bootstrap(&self.binary, &child, &log)?.as_raw();
        drop(child);
        if raw <= 1 {
            return Err(LocalError::Bootstrap);
        }
        let pid = Pid::from_raw(raw).ok_or(LocalError::Bootstrap)?;
        let mut owned = Process {
            pid,
            exit_code: None,
            settled: false,
        };
        let startup = tokio::select! {
            biased;
            result=startup(&mut control,&packet)=>result,
            ()=abandon.wait()=>Err(LocalError::Abandoned),
        };
        let result = match startup {
            Ok(Some((stage, errno))) => {
                let filename = if stage == b'C' {
                    request.code_dir.to_string_lossy().into_owned()
                } else {
                    argv[0].clone()
                };
                write_start_error(&mut log, &argv[0], errno, &filename, stage == b'C')
                    .map(|()| Outcome::new(Some(127.into())))
            }
            Ok(None) => wait_process(&mut owned, request.deadline_seconds, &cancel, &abandon).await,
            Err(error) => Err(error),
        };
        // Every path retains ownership until the leader and group have settled.
        owned.finish().await?;
        result
    }
}
fn put_env(values: &mut Vec<(String, String)>, key: String, value: String) {
    if let Some((_, current)) = values.iter_mut().find(|(name, _)| name == &key) {
        *current = value;
    } else {
        values.push((key, value));
    }
}
fn filesystem_text(value: &OsStr) -> Result<String, LocalError> {
    value
        .to_str()
        .map(str::to_owned)
        .ok_or(LocalError::Encoding)
}
fn encoded(value: &str) -> Result<OsString, LocalError> {
    if value.as_bytes().contains(&0) {
        return Err(LocalError::Value);
    }
    Ok(OsString::from(value))
}
fn checked(value: &OsStr) -> Result<(), LocalError> {
    if value.as_bytes().contains(&0) {
        Err(LocalError::Value)
    } else {
        Ok(())
    }
}
fn field(packet: &mut Vec<u8>, bytes: &[u8]) -> Result<(), LocalError> {
    packet.extend_from_slice(
        &u64::try_from(bytes.len())
            .map_err(|_| LocalError::Value)?
            .to_be_bytes(),
    );
    packet.extend_from_slice(bytes);
    Ok(())
}
fn packet(
    argv: &[String],
    environment: &[(String, String)],
    cwd: &Path,
) -> Result<Vec<u8>, LocalError> {
    let mut env = Vec::new();
    for (key, value) in environment {
        let key = encoded(key)?;
        if key.as_bytes().contains(&b'=') {
            return Err(LocalError::Value);
        }
        let value = encoded(value)?;
        env.push((key, value));
    }
    let values = argv
        .iter()
        .map(|text| {
            let value = encoded(text)?;
            checked(&value)?;
            Ok(value)
        })
        .collect::<Result<Vec<_>, LocalError>>()?;
    for (key, value) in &env {
        checked(key)?;
        checked(value)?;
    }
    checked(cwd.as_os_str())?;
    let mut out = MAGIC.to_vec();
    field(&mut out, cwd.as_os_str().as_bytes())?;
    out.extend_from_slice(
        &u64::try_from(values.len())
            .map_err(|_| LocalError::Value)?
            .to_be_bytes(),
    );
    for arg in values {
        field(&mut out, arg.as_bytes())?;
    }
    out.extend_from_slice(
        &u64::try_from(env.len())
            .map_err(|_| LocalError::Value)?
            .to_be_bytes(),
    );
    for (key, value) in env {
        field(&mut out, key.as_bytes())?;
        field(&mut out, value.as_bytes())?;
    }
    Ok(out)
}

#[derive(Default)]
struct Shared {
    result: Mutex<Option<Result<Outcome, LocalError>>>,
    notify: Notify,
}
struct Completion(Arc<Shared>);
impl Completion {
    fn publish(self, result: Result<Outcome, LocalError>) {
        *self
            .0
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
        self.0.notify.notify_waiters();
    }
}
impl Drop for Completion {
    fn drop(&mut self) {
        let mut result = self
            .0
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if result.is_none() {
            *result = Some(Err(LocalError::Bootstrap));
            self.0.notify.notify_waiters();
        }
    }
}
pub struct RunHandle {
    shared: Arc<Shared>,
    abandon: CancellationEvent,
}
#[derive(Clone)]
pub struct Settlement(Arc<Shared>);
impl RunHandle {
    #[must_use]
    pub fn settlement(&self) -> Settlement {
        Settlement(self.shared.clone())
    }
    /// # Errors
    /// Reports source exception categories or safe bootstrap/abandonment failures.
    pub async fn result(&self) -> Result<Outcome, LocalError> {
        self.settlement().wait().await
    }
}
impl Drop for RunHandle {
    fn drop(&mut self) {
        self.abandon.set();
    }
}
impl Settlement {
    /// Await ownership release even after the result handle has been dropped.
    /// # Errors
    /// Returns the supervisor's settled result or sanitized failure.
    pub async fn wait(&self) -> Result<Outcome, LocalError> {
        loop {
            let notified = self.0.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(result) = self
                .0
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
            {
                return result;
            }
            notified.await;
        }
    }
}
struct Process {
    pid: Pid,
    exit_code: Option<i32>,
    settled: bool,
}
fn kill_group(pid: Pid) -> Result<bool, LocalError> {
    match rustix::process::kill_process_group(pid, Signal::KILL) {
        Ok(()) => Ok(true),
        Err(rustix::io::Errno::SRCH) => Ok(false),
        Err(e) => Err(LocalError::Io {
            errno: Some(e.raw_os_error()),
        }),
    }
}
async fn group_gone(pid: Pid) -> Result<(), LocalError> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        match rustix::process::test_kill_process_group(pid) {
            Ok(()) => tokio::time::sleep(Duration::from_millis(20)).await,
            Err(rustix::io::Errno::SRCH) => return Ok(()),
            Err(e) => {
                return Err(LocalError::Io {
                    errno: Some(e.raw_os_error()),
                });
            }
        }
    }
    Ok(())
}
impl Process {
    fn try_wait(&mut self) -> Result<Option<i32>, LocalError> {
        if self.exit_code.is_some() {
            return Ok(self.exit_code);
        }
        let pid = nix::unistd::Pid::from_raw(self.pid.as_raw_nonzero().get());
        match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(_, code)) => self.exit_code = Some(code),
            Ok(WaitStatus::Signaled(_, signal, _)) => self.exit_code = Some(-(signal as i32)),
            Ok(_) | Err(nix::errno::Errno::EINTR) => {}
            Err(error) => return Err(io_error(error.into())),
        }
        Ok(self.exit_code)
    }
    async fn wait(&mut self) -> Result<i32, LocalError> {
        loop {
            if let Some(code) = self.try_wait()? {
                return Ok(code);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    async fn finish(&mut self) -> Result<(), LocalError> {
        let exists = kill_group(self.pid)?;
        if self.try_wait()?.is_none()
            && let Err(error) = rustix::process::kill_process(self.pid, Signal::KILL)
            && error != rustix::io::Errno::SRCH
        {
            return Err(io_error(error.into()));
        }
        let pid = self.pid;
        let (group, leader) = tokio::join!(
            async {
                if exists {
                    group_gone(pid).await
                } else {
                    Ok(())
                }
            },
            self.wait()
        );
        leader?;
        group?;
        self.settled = true;
        Ok(())
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if !self.settled {
            let _ = kill_group(self.pid);
            if self.exit_code.is_none() {
                let _ = rustix::process::kill_process(self.pid, Signal::KILL);
            }
        }
    }
}
async fn deadline(seconds: f64) {
    if seconds.is_nan() || seconds <= 0.0 {
        tokio::task::yield_now().await;
        return;
    }
    let start = Instant::now();
    loop {
        let remaining = seconds - start.elapsed().as_secs_f64();
        if remaining <= 0.0 {
            return;
        }
        tokio::time::sleep(Duration::from_secs_f64(remaining.min(86400.0))).await;
    }
}
async fn wait_process(
    process: &mut Process,
    timeout: Deadline,
    cancel: &CancellationEvent,
    abandon: &CancellationEvent,
) -> Result<Outcome, LocalError> {
    // Source schedules timeout after process creation even if completion or
    // cancellation is already ready. Failure still reaches outer settlement.
    let seconds = timeout.seconds()?;
    let status = tokio::select! {
        biased;
        status=process.wait()=>Some(status?),
        ()=cancel.wait()=>None,
        ()=abandon.wait()=>None,
        ()=deadline(seconds)=>None,
    };
    if let Some(status) = status {
        return Ok(Outcome::new(Some(status.into())));
    }
    process.finish().await?;
    if abandon.is_set() {
        return Err(LocalError::Abandoned);
    }
    let mut outcome = Outcome::new(None);
    outcome.cancelled = cancel.is_set();
    outcome.timed_out = !outcome.cancelled;
    Ok(outcome)
}
async fn startup(
    control: &mut tokio::net::UnixStream,
    packet: &[u8],
) -> Result<Option<(u8, i32)>, LocalError> {
    let mut hello = [0; 16];
    control
        .read_exact(&mut hello)
        .await
        .map_err(|_| LocalError::Bootstrap)?;
    if &hello != MAGIC {
        return Err(LocalError::Bootstrap);
    }
    control
        .write_all(packet)
        .await
        .map_err(|_| LocalError::Bootstrap)?;
    let ready = control.read_u8().await.map_err(|_| LocalError::Bootstrap)?;
    if ready == b'R' {
        control
            .write_all(b"G")
            .await
            .map_err(|_| LocalError::Bootstrap)?;
    } else {
        return read_failure(control, ready).await.map(Some);
    }
    let mut result = [0; 1];
    match control.read(&mut result).await {
        Ok(0) => Ok(None),
        Ok(1) => read_failure(control, result[0]).await.map(Some),
        _ => Err(LocalError::Bootstrap),
    }
}
async fn read_failure(
    control: &mut tokio::net::UnixStream,
    stage: u8,
) -> Result<(u8, i32), LocalError> {
    if !matches!(stage, b'C' | b'S' | b'E') {
        return Err(LocalError::Bootstrap);
    }
    Ok((
        stage,
        control
            .read_i32()
            .await
            .map_err(|_| LocalError::Bootstrap)?,
    ))
}
fn write_start_error(
    log: &mut File,
    head: &String,
    errno: i32,
    filename: &str,
    is_path: bool,
) -> Result<(), LocalError> {
    let head = head.as_utf8().ok_or(LocalError::Encoding)?;
    let name = text::repr_string(filename)
        .map_err(|_| LocalError::Encoding)?
        .as_utf8()
        .ok_or(LocalError::Encoding)?;
    let error = io::Error::from_raw_os_error(errno).to_string();
    let suffix = format!(" (os error {errno})");
    let message = error.strip_suffix(&suffix).unwrap_or(&error);
    let name = if is_path {
        format!("PosixPath({name})")
    } else {
        name
    };
    writeln!(
        log,
        "cannot start {head}: [Errno {errno}] {message}: {name}"
    )
    .map_err(io_error)
}

/// Call before CLI parsing or runtime creation. Ordinary invocations are not
/// affected. The private entry requires a bidirectional Unix socket handshake.
#[must_use]
pub fn dispatch_bootstrap() -> Option<ExitCode> {
    if std::env::args_os().next().as_deref() != Some(OsStr::new(BOOTSTRAP_ARGV0)) {
        return None;
    }
    Some(ExitCode::from(if bootstrap().is_ok() { 127 } else { 70 }))
}
fn read_count(stream: &mut UnixStream) -> io::Result<usize> {
    let mut bytes = [0; 8];
    stream.read_exact(&mut bytes)?;
    usize::try_from(u64::from_be_bytes(bytes))
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
}
fn read_field(stream: &mut UnixStream) -> io::Result<OsString> {
    let count = read_count(stream)?;
    let mut bytes = vec![0; count];
    stream.read_exact(&mut bytes)?;
    Ok(OsString::from_vec(bytes))
}
fn failure(stream: &mut UnixStream, stage: u8, error: &io::Error) -> io::Result<()> {
    stream.write_all(&[stage])?;
    stream.write_all(&error.raw_os_error().unwrap_or(5).to_be_bytes())
}
fn bootstrap() -> io::Result<()> {
    let fd = rustix::io::dup(rustix::stdio::stdin())?;
    rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC)?;
    let mut stream = UnixStream::from(fd);
    stream.peer_addr()?;
    // Rust startup ignores PIPE. A caught fatal handler preserves its default
    // terminating action here and execve resets the caught disposition to DFL.
    // This private process has no runtime or concurrent signal registrations.
    signal_hook::flag::register_conditional_default(
        signal_hook::consts::SIGPIPE,
        Arc::new(std::sync::atomic::AtomicBool::new(true)),
    )?;
    stream.write_all(MAGIC)?;
    let mut magic = [0; 16];
    stream.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let cwd = read_field(&mut stream)?;
    let count = read_count(&mut stream)?;
    let mut argv = Vec::new();
    for _ in 0..count {
        argv.push(read_field(&mut stream)?);
    }
    let count = read_count(&mut stream)?;
    let mut env = Vec::new();
    for _ in 0..count {
        env.push((read_field(&mut stream)?, read_field(&mut stream)?));
    }
    let head = argv
        .first()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
    if let Err(error) = std::env::set_current_dir(&cwd) {
        return failure(&mut stream, b'C', &error);
    }
    if let Err(error) = rustix::process::setsid() {
        return failure(&mut stream, b'S', &error.into());
    }
    stream.write_all(b"R")?;
    let mut proceed = [0];
    stream.read_exact(&mut proceed)?;
    if proceed != *b"G" {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    rustix::stdio::dup2_stdin(File::open("/dev/null")?)?;
    close_inherited(stream.as_raw_fd())?;
    let error = exec_exact(head, &argv, &env);
    failure(&mut stream, b'E', &error)
}

fn close_inherited(control: i32) -> io::Result<()> {
    // No runtime or other thread exists in this private bootstrap. Collect and
    // drop the directory iterator before closing any inherited descriptor.
    #[cfg(target_os = "linux")]
    let directory = "/proc/self/fd";
    #[cfg(not(target_os = "linux"))]
    let directory = "/dev/fd";
    let descriptors = std::fs::read_dir(directory)?
        .map(|entry| {
            let entry = entry?;
            Ok(entry
                .file_name()
                .to_str()
                .and_then(|value| value.parse::<i32>().ok()))
        })
        .collect::<io::Result<Vec<_>>>()?;
    for descriptor in descriptors.into_iter().flatten() {
        if descriptor > 2 && descriptor != control {
            match nix::unistd::close(descriptor) {
                Ok(()) | Err(nix::errno::Errno::EBADF) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

fn spawn_bootstrap(
    binary: &Path,
    child: &UnixStream,
    log: &File,
) -> Result<nix::unistd::Pid, LocalError> {
    let mut files = PosixSpawnFileActions::init().map_err(|e| io_error(e.into()))?;
    for (from, to) in [
        (child.as_raw_fd(), 0),
        (log.as_raw_fd(), 1),
        (log.as_raw_fd(), 2),
    ] {
        files.add_dup2(from, to).map_err(|e| io_error(e.into()))?;
    }
    let mut signals = SigSet::empty();
    signals.add(nix::sys::signal::Signal::SIGPIPE);
    signals.add(nix::sys::signal::Signal::SIGXFSZ);
    let mut attributes = PosixSpawnAttr::init().map_err(|e| io_error(e.into()))?;
    attributes
        .set_sigdefault(&signals)
        .map_err(|e| io_error(e.into()))?;
    attributes
        .set_flags(PosixSpawnFlags::POSIX_SPAWN_SETSIGDEF)
        .map_err(|e| io_error(e.into()))?;
    let marker = CString::new(BOOTSTRAP_ARGV0).map_err(|_| LocalError::Value)?;
    let environment: [CString; 0] = [];
    posix_spawn(binary, &files, &attributes, &[marker], &environment)
        .map_err(|e| io_error(e.into()))
}

fn exec_exact(head: &OsStr, argv: &[OsString], env: &[(OsString, OsString)]) -> io::Error {
    let converted = argv
        .iter()
        .map(|v| CString::new(v.as_bytes()))
        .collect::<Result<Vec<_>, _>>();
    let Ok(arguments) = converted else {
        return io::Error::from(io::ErrorKind::InvalidInput);
    };
    let converted = env
        .iter()
        .map(|(key, value)| {
            let mut bytes = key.as_bytes().to_vec();
            bytes.push(b'=');
            bytes.extend_from_slice(value.as_bytes());
            CString::new(bytes)
        })
        .collect::<Result<Vec<_>, _>>();
    let Ok(environment) = converted else {
        return io::Error::from(io::ErrorKind::InvalidInput);
    };
    let candidates = if head.as_bytes().contains(&b'/') {
        vec![PathBuf::from(head)]
    } else {
        let path = env
            .iter()
            .find(|(key, _)| key == "PATH")
            .map_or(OsStr::new("/bin:/usr/bin"), |(_, v)| v.as_os_str());
        path.as_bytes()
            .split(|&b| b == b':')
            .map(|dir| PathBuf::from(OsStr::from_bytes(dir)).join(head))
            .collect()
    };
    let mut first = None;
    let mut last = nix::errno::Errno::ENOENT;
    for candidate in candidates {
        let Ok(candidate) = CString::new(candidate.as_os_str().as_bytes()) else {
            return io::Error::from(io::ErrorKind::InvalidInput);
        };
        let error = match nix::unistd::execve(&candidate, &arguments, &environment) {
            Err(error) => error,
            Ok(never) => match never {},
        };
        if first.is_none()
            && !matches!(
                error,
                nix::errno::Errno::ENOENT | nix::errno::Errno::ENOTDIR
            )
        {
            first = Some(error);
        }
        last = error;
    }
    first.unwrap_or(last).into()
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
