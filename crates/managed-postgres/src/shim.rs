//! The private exec shim that ties the server's life to its supervisor.
//!
//! Setting a parent-death signal must happen in the child between `fork` and
//! `exec`, which safe Rust cannot do with `std::process`. Instead the
//! supervisor starts its own executable under a private `argv[0]`; that
//! process sets the signal and replaces itself with `postgres`, keeping the
//! PID. The setting survives `exec`, so the postmaster receives `SIGINT` (a
//! fast shutdown) when the supervisor dies.
use std::{
    ffi::OsStr,
    os::unix::process::CommandExt,
    process::{Command, ExitCode},
};

pub const EXEC_SHIM_ARGV0: &str = "cannery-managed-postgres-exec-v1";
pub(crate) const PARENT_ENV: &str = "CANNERY_MANAGED_POSTGRES_PARENT";

/// Call first in `main`, before any runtime or thread exists. Ordinary
/// invocations return `None`.
#[must_use]
pub fn dispatch_exec_shim() -> Option<ExitCode> {
    if std::env::args_os().next().as_deref() != Some(OsStr::new(EXEC_SHIM_ARGV0)) {
        return None;
    }
    Some(exec())
}

fn exec() -> ExitCode {
    let mut arguments = std::env::args_os().skip(1);
    let Some(program) = arguments.next() else {
        return ExitCode::from(70);
    };
    let parent = std::env::var(PARENT_ENV)
        .ok()
        .and_then(|value| value.parse::<i32>().ok());
    #[cfg(target_os = "linux")]
    {
        if rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::INT))
            .is_err()
        {
            return ExitCode::from(70);
        }
        // The supervisor may have died before the signal was armed.
        let current = rustix::process::getppid().map(|pid| pid.as_raw_nonzero().get());
        if parent.is_none() || current != parent {
            return ExitCode::from(70);
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = parent;
    let error = Command::new(&program)
        .args(arguments)
        .env_remove(PARENT_ENV)
        .exec();
    eprintln!("cannot execute {}: {error}", program.to_string_lossy());
    ExitCode::from(127)
}
