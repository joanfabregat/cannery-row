#![forbid(unsafe_code)]
use std::{
    os::unix::process::CommandExt,
    process::{Command, Stdio},
};

#[test]
fn private_argv0_without_control_socket_executes_no_command()
-> Result<(), Box<dyn std::error::Error>> {
    let result = Command::new(env!("CARGO_BIN_EXE_cannery"))
        .arg0("cannery-private-local-bootstrap-v1")
        .arg("openapi")
        .stdin(Stdio::null())
        .output()?;
    assert_eq!(result.status.code(), Some(70));
    assert_eq!(result.stdout, [] as [u8; 0]);
    assert_eq!(result.stderr, [] as [u8; 0]);
    Ok(())
}
