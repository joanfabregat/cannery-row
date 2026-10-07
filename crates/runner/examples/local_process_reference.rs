//! Offline native process comparator; not a distributed CLI command.
#![forbid(unsafe_code)]

use cannery_runner::{
    cancellation::CancellationEvent,
    local_process::{LocalError, LocalProcessBackend, PreparedRequest},
};
use serde_json::{Value, json};
use std::{
    fs,
    os::fd::AsRawFd,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::ExitCode,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
fn text(value: &Value) -> Result<String, Box<dyn std::error::Error>> {
    let points = value
        .as_array()
        .ok_or("missing points")?
        .iter()
        .map(|n| Ok(u32::try_from(n.as_u64().ok_or("invalid point")?)?))
        .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
    points
        .into_iter()
        .map(|point| char::from_u32(point).ok_or_else(|| "unsupported Unicode scalar".into()))
        .collect()
}
fn texts(value: &Value) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    value
        .as_array()
        .ok_or("missing text sequence")?
        .iter()
        .map(text)
        .collect()
}
// Fixture code points preserve frozen source inputs without admitting lone
// surrogates into the native String API. Check every field before process launch.
fn native_input_supported(case: &Value) -> Result<bool, Box<dyn std::error::Error>> {
    let mut fields = Vec::new();
    for name in ["command", "args"] {
        fields.extend(case[name].as_array().ok_or("missing text sequence")?);
    }
    for pair in case["env"].as_array().ok_or("missing env")? {
        let pair = pair.as_array().ok_or("invalid env pair")?;
        if pair.len() != 2 {
            return Err("invalid env pair".into());
        }
        fields.extend(pair);
    }
    let mut supported = true;
    for field in fields {
        let mut field_supported = true;
        for point in field.as_array().ok_or("missing points")? {
            let point = u32::try_from(point.as_u64().ok_or("invalid point")?)?;
            if (0xD800..=0xDFFF).contains(&point) {
                field_supported = false;
            } else if char::from_u32(point).is_none() {
                return Err("invalid fixture Unicode point".into());
            }
        }
        if text(field).is_ok() != field_supported {
            return Err("native String boundary did not reject surrogate input".into());
        }
        supported &= field_supported;
    }
    Ok(supported)
}
fn native_start_log(name: &str) -> Option<&'static str> {
    match name {
        "missing-command" => Some(
            "cannot start cannery-local-reference-missing-executable: [Errno 2] No such file or directory: \"cannery-local-reference-missing-executable\"\n",
        ),
        "path-permission-first" => {
            Some("cannot start probe: [Errno 13] Permission denied: \"probe\"\n")
        }
        "path-format-first" => Some("cannot start probe: [Errno 8] Exec format error: \"probe\"\n"),
        "permission-denied" => Some(
            "cannot start ./not-executable: [Errno 13] Permission denied: \"./not-executable\"\n",
        ),
        "no-shebang" => {
            Some("cannot start ./no-shebang: [Errno 8] Exec format error: \"./no-shebang\"\n")
        }
        _ => None,
    }
}
struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn root() -> Result<Root, Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!(
        "cannery-native-local-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::create_dir(&path)?;
    Ok(Root(path))
}
fn prepare(root: &Root, case: &Value) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let dir = root.0.join("code");
    fs::create_dir(&dir)?;
    fs::create_dir(root.0.join("tmp"))?;
    for (name, body, mode) in [
        ("not-executable", "echo forbidden\n", 0o600),
        ("no-shebang", "printf 'source-noshebang\\n'\n", 0o700),
    ] {
        fs::write(dir.join(name), body)?;
        fs::set_permissions(dir.join(name), fs::Permissions::from_mode(mode))?;
    }
    if let Some(layout) = case["path_setup"].as_str() {
        for name in ["search-a", "search-b"] {
            fs::create_dir(dir.join(name))?;
        }
        let first = dir.join("search-a/probe");
        fs::write(&first, "printf 'source-noshebang\\n'\n")?;
        fs::set_permissions(
            &first,
            fs::Permissions::from_mode(if layout == "permission-first" {
                0o600
            } else {
                0o700
            }),
        )?;
        if layout != "permission-first" {
            let second = dir.join("search-b/probe");
            let (body, mode) = if layout == "format-first" {
                ("echo forbidden\n", 0o600)
            } else {
                ("#!/bin/sh\nprintf 'later-success\\n'\n", 0o700)
            };
            fs::write(&second, body)?;
            fs::set_permissions(&second, fs::Permissions::from_mode(mode))?;
        }
    }
    let log = root.0.join("step.log");
    if case["log_directory"] == true {
        fs::create_dir(&log)?;
    }
    Ok((dir, log))
}
fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("--signal-xfsz") {
        if rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::XFSZ)
            .is_err()
        {
            return ExitCode::FAILURE;
        }
        println!("survived");
        return ExitCode::SUCCESS;
    }
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => match runtime.block_on(compare()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("native local reference failed: {error}");
                ExitCode::FAILURE
            }
        },
        Err(_) => ExitCode::FAILURE,
    }
}
async fn compare() -> Result<(), Box<dyn std::error::Error>> {
    let fixture: Value = serde_json::from_str(&fs::read_to_string(
        std::env::args().nth(1).ok_or("missing fixture")?,
    )?)?;
    let binary =
        PathBuf::from(std::env::var_os("CANNERY_LOCAL_PROCESS_BINARY").ok_or("missing binary")?);
    let interpreter =
        PathBuf::from(std::env::var_os("CANNERY_LOCAL_INTERPRETER").ok_or("missing interpreter")?);
    let backend = LocalProcessBackend::new(binary, interpreter.clone(), true)?;
    let cases = fixture["cases"].as_array().ok_or("missing cases")?;
    if cases.len() != 44 {
        return Err("incomplete source process reference".into());
    }
    let selected = std::env::args().nth(2);
    if selected
        .as_deref()
        .is_some_and(|name| !cases.iter().any(|case| case["name"] == name))
    {
        return Err("unknown source process selector".into());
    }
    let mut failures = 0;
    for case in cases {
        if selected.as_deref().is_some_and(|name| case["name"] != name) {
            continue;
        }
        let supported = native_input_supported(case)?;
        if case["native_input_supported"].as_bool() != Some(supported) {
            return Err("native input boundary metadata differs".into());
        }
        if !supported {
            println!(
                "case {}: native String input rejected before process launch",
                case["name"]
            );
            continue;
        }
        if case["path_setup"].is_string() && selected.is_none() {
            let output = std::process::Command::new(std::env::current_exe()?)
                .arg(std::env::args().nth(1).ok_or("missing fixture")?)
                .arg(case["name"].as_str().ok_or("missing selector")?)
                .env("PATH", "search-a:search-b")
                .output()?;
            if !output.status.success() {
                failures += 1;
                eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            }
            continue;
        }
        let actual = compare_case(&backend, case).await?;
        let mut expected = case["expected"].clone();
        if let Some(log) = native_start_log(case["name"].as_str().ok_or("missing name")?) {
            expected["log"] = json!(log.as_bytes());
        }
        if case["name"] == "missing-cwd" {
            let interpreter = interpreter.to_str().ok_or("interpreter must be UTF-8")?;
            let log = format!(
                "cannot start {interpreter}: [Errno 2] No such file or directory: PosixPath(\"cannery-local-reference-missing-cwd\")\n"
            );
            expected["log"] = json!(log.as_bytes());
        }
        if actual != expected {
            failures += 1;
            eprintln!(
                "case {} differs; expected {}, actual {}",
                case["name"], expected, actual
            );
        }
    }
    if failures > 0 {
        return Err(format!("{failures} source process differences").into());
    }
    println!("all supported source local process outcomes and native input boundaries match");
    Ok(())
}
async fn compare_case(
    backend: &LocalProcessBackend,
    case: &Value,
) -> Result<Value, Box<dyn std::error::Error>> {
    let root = root()?;
    let (dir, log) = prepare(&root, case)?;
    let mut env = case["env"]
        .as_array()
        .ok_or("missing env")?
        .iter()
        .map(|pair| Ok((text(&pair[0])?, text(&pair[1])?)))
        .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
    let inherited = fs::File::create(root.0.join("inherited"))?;
    rustix::io::fcntl_setfd(&inherited, rustix::io::FdFlags::empty())?;
    if case["inherited_fd"] == true {
        env.push((
            String::from("INHERITED_FD"),
            String::from(&inherited.as_raw_fd().to_string()),
        ));
    }
    let code_dir = if case["cwd_nul"] == true {
        PathBuf::from("bad\0cwd")
    } else if case["missing_cwd"] == true {
        PathBuf::from("cannery-local-reference-missing-cwd")
    } else {
        dir
    };
    if case["missing_cwd"] == true && code_dir.exists() {
        return Err("missing cwd fixture exists".into());
    }
    let seconds = deadline_value(case)?;
    let cancel = cancellation(case);
    let handle = backend.run_prepared(
        PreparedRequest {
            command: texts(&case["command"])?,
            args: texts(&case["args"])?,
            env,
            root: root.0.clone(),
            code_dir,
            log_path: log.clone(),
            deadline_seconds: seconds.into(),
        },
        cancel,
    );
    let result = if case["drop_after_ready"] == true {
        let settlement = handle.settlement();
        let marker = root.0.join("ready");
        for _ in 0..500 {
            if marker.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if !marker.exists() {
            return Err("owned child never signaled readiness".into());
        }
        drop(handle);
        settlement.wait().await
    } else {
        handle.result().await
    };
    let outcome = result_value(result)?;
    drop(inherited);
    let log = match fs::read(&log) {
        Ok(bytes) => json!(bytes),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::IsADirectory
            ) =>
        {
            Value::Null
        }
        Err(error) => return Err(error.into()),
    };
    tokio::time::sleep(Duration::from_millis(350)).await;
    Ok(json!({"outcome":outcome,"log":log,"background_wrote":root.0.join("survivor").exists()}))
}
fn deadline_value(case: &Value) -> Result<f64, Box<dyn std::error::Error>> {
    Ok(match case["deadline"].as_str() {
        Some("nan") => f64::NAN,
        Some("inf") => f64::INFINITY,
        Some("-inf") => f64::NEG_INFINITY,
        _ => case["deadline"].as_f64().ok_or("invalid deadline")?,
    })
}
fn cancellation(case: &Value) -> CancellationEvent {
    let cancel = CancellationEvent::new();
    if case["cancel_initial"] == true {
        cancel.set();
    }
    if let Some(seconds) = case["cancel_after"].as_f64() {
        let event = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
            event.set();
        });
    }
    cancel
}
fn result_value(
    result: Result<cannery_runner::launcher::Outcome, LocalError>,
) -> Result<Value, Box<dyn std::error::Error>> {
    Ok(match result {
        Ok(value) => {
            json!({"exit_code":value.exit_code.map(|n|n.to_string()),"timed_out":value.timed_out,"cancelled":value.cancelled,"oom_killed":value.oom_killed,"output_error":null})
        }
        Err(LocalError::Encoding) => json!({"error":"UnicodeError"}),
        Err(LocalError::Value | LocalError::EmptyCommand) => json!({"error":"ValueError"}),
        Err(LocalError::Abandoned) => json!({"error":"CancelledError"}),
        Err(LocalError::Io { errno }) => json!({"error":"OSError","errno":errno}),
        Err(error) => return Err(format!("safe process error: {error}").into()),
    })
}
