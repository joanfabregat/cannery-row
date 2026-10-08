//! Actual production Python filesystem oracle; only owned roots are mutated.
#![forbid(unsafe_code)]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;

use cannery_runner::{
    launcher::{Mount, PosixPath},
    local_preparation::{
        CopyFailure, CopyLimits, LocalPreparation, MetadataCapabilities, PreparationError,
    },
};
use rustix::fs::{AtFlags, CWD, Mode, Timespec, Timestamps, XattrFlags, mkfifoat, utimensat};
use serde_json::{Value, json};
use std::{
    ffi::{CString, OsString},
    fs,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
};
fn decode(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}
fn native_metadata() -> MetadataCapabilities {
    MetadataCapabilities {
        symlink_chmod: false,
        symlink_chflags: false,
    }
}
fn reference() -> Value {
    let text = std::env::var_os("CANNERY_LOCAL_PREPARATION_REFERENCE").map_or_else(
        || {
            runtime_reference!(
                "/../../crates/runner/tests/fixtures/local_preparation_reference.json"
            )
            .to_owned()
        },
        |path| fs::read_to_string(path).unwrap(),
    );
    serde_json::from_str(&text).unwrap()
}
fn encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|byte| {
            [
                char::from(HEX[usize::from(byte >> 4)]),
                char::from(HEX[usize::from(byte & 15)]),
            ]
        })
        .collect()
}
fn native(value: &Value) -> PathBuf {
    PathBuf::from(OsString::from_vec(decode(value.as_str().unwrap())))
}
const TIME: i64 = 4_102_444_800;
const NANOS: i64 = 123_456_789;
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let mut nonce = [0; 4];
        getrandom::fill(&mut nonce).unwrap();
        let root = PathBuf::from(format!("/tmp/cannery-local-prepare-{}", encode(&nonce)));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn build(&self, operations: &Value) {
        for action in operations.as_array().unwrap() {
            let path = self.0.join(native(&action["path_hex"]));
            match action["kind"].as_str().unwrap() {
                "dir" => {
                    fs::create_dir(&path).unwrap();
                    fs::set_permissions(
                        path,
                        fs::Permissions::from_mode(
                            u32::try_from(action["mode"].as_u64().unwrap_or(0o755)).unwrap(),
                        ),
                    )
                    .unwrap();
                }
                "file" => {
                    fs::write(&path, decode(action["content_hex"].as_str().unwrap())).unwrap();
                    fs::set_permissions(
                        path,
                        fs::Permissions::from_mode(
                            u32::try_from(action["mode"].as_u64().unwrap_or(0o644)).unwrap(),
                        ),
                    )
                    .unwrap();
                }
                "link" => {
                    let target = action["target"].as_str().unwrap();
                    symlink(
                        if action["absolute"].as_bool().unwrap_or(false) {
                            self.0.join(target)
                        } else {
                            PathBuf::from(target)
                        },
                        path,
                    )
                    .unwrap();
                }
                "hardlink" => {
                    fs::hard_link(self.0.join(action["target"].as_str().unwrap()), path).unwrap();
                }
                "fifo" => mkfifoat(CWD, path, Mode::from_raw_mode(0o600)).unwrap(),
                "chmod" => fs::set_permissions(
                    path,
                    fs::Permissions::from_mode(
                        u32::try_from(action["mode"].as_u64().unwrap()).unwrap(),
                    ),
                )
                .unwrap(),
                "xattr" => {
                    let name = CString::new(action["name"].as_str().unwrap()).unwrap();
                    rustix::fs::setxattr(
                        path,
                        &name,
                        &decode(action["value_hex"].as_str().unwrap()),
                        XattrFlags::empty(),
                    )
                    .unwrap();
                }
                other => panic!("unknown operation {other}"),
            }
        }
        let locks: Vec<_> = operations
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| matches!(a["kind"].as_str(), Some("dir" | "chmod")) && !a["mode"].is_null())
            .collect();
        for action in &locks {
            fs::set_permissions(
                self.0.join(native(&action["path_hex"])),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        stamp(&self.0);
        for action in locks.into_iter().rev() {
            fs::set_permissions(
                self.0.join(native(&action["path_hex"])),
                fs::Permissions::from_mode(
                    u32::try_from(action["mode"].as_u64().unwrap()).unwrap(),
                ),
            )
            .unwrap();
        }
    }
}
fn stamp(path: &Path) {
    let meta = fs::symlink_metadata(path).unwrap();
    if meta.is_dir() {
        for child in fs::read_dir(path).unwrap() {
            stamp(&child.unwrap().path());
        }
    }
    let times = Timestamps {
        last_access: Timespec {
            tv_sec: TIME,
            tv_nsec: NANOS,
        },
        last_modification: Timespec {
            tv_sec: TIME,
            tv_nsec: NANOS,
        },
    };
    utimensat(CWD, path, &times, AtFlags::SYMLINK_NOFOLLOW).unwrap();
}
fn unlock(path: &Path) {
    if fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        for child in fs::read_dir(path).unwrap() {
            unlock(&child.unwrap().path());
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        unlock(&self.0);
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn children(path: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(path)?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort_by(|a, b| {
        a.file_name()
            .unwrap()
            .as_bytes()
            .cmp(b.file_name().unwrap().as_bytes())
    });
    Ok(paths)
}
fn growing(
    mut action: impl FnMut(&mut [u8]) -> rustix::io::Result<usize>,
) -> rustix::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        match action(&mut bytes) {
            Ok(size) => {
                if bytes.is_empty() && size != 0 {
                    bytes.resize(size, 0);
                } else {
                    bytes.truncate(size);
                    return Ok(bytes);
                }
            }
            Err(rustix::io::Errno::RANGE) => bytes.resize(bytes.len().max(1) * 2, 0),
            Err(error) => return Err(error),
        }
    }
}
fn snapshot(root: &Path, metadata: &Value) -> Value {
    fn visit(root: &Path, path: &Path, metadata: &Value, result: &mut Vec<Value>) {
        let info = fs::symlink_metadata(path).unwrap();
        let relative = path.strip_prefix(root).unwrap();
        let mut row =
            json!({"path_hex":encode(relative.as_os_str().as_bytes()),"mode":info.mode()&0o7777});
        if info.file_type().is_symlink() {
            row["kind"] = json!("link");
            let target = fs::read_link(path).unwrap();
            let relative = target.strip_prefix(root);
            row["target"] = json!({"absolute":relative.is_ok(),"hex":encode(relative.unwrap_or(&target).as_os_str().as_bytes())});
            row["size"] = json!(info.len());
        } else if info.is_dir() {
            row["kind"] = json!("dir");
        } else if info.mode() & 0o170_000 == 0o010_000 {
            row["kind"] = json!("fifo");
        } else {
            row["kind"] = json!("file");
            row["size"] = json!(info.len());
            match fs::read(path) {
                Ok(bytes) => row["content_hex"] = json!(encode(&bytes)),
                Err(error) => row["read_errno"] = json!(error.raw_os_error()),
            }
        }
        if metadata
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p.as_str().unwrap().as_bytes() == relative.as_os_str().as_bytes())
        {
            row["mtime_ns"] =
                json!(i128::from(info.mtime()) * 1_000_000_000 + i128::from(info.mtime_nsec()));
        }
        match growing(|buffer| rustix::fs::llistxattr(path, buffer)) {
            Ok(names) => {
                let mut attrs = serde_json::Map::new();
                for name in names.split(|&b| b == 0).filter(|n| !n.is_empty()) {
                    let key = String::from_utf8(name.to_vec()).unwrap();
                    let name = CString::new(name).unwrap();
                    let bytes =
                        growing(|buffer| rustix::fs::lgetxattr(path, &name, buffer)).unwrap();
                    attrs.insert(key, json!(encode(&bytes)));
                }
                row["xattrs"] = Value::Object(attrs);
            }
            Err(error) => row["xattr_errno"] = json!(error.raw_os_error()),
        }
        let index = result.len();
        result.push(row);
        if info.is_dir() {
            match children(path) {
                Ok(children) => {
                    for child in children {
                        visit(root, &child, metadata, result);
                    }
                }
                Err(error) => result[index]["list_errno"] = json!(error.raw_os_error()),
            }
        }
    }
    let mut result = Vec::new();
    for path in children(root).unwrap() {
        visit(root, &path, metadata, &mut result);
    }
    json!(result)
}
fn outcome(result: Result<(), PreparationError>, root: &Path) -> Value {
    match result {
        Ok(()) => json!({"ok":true}),
        Err(PreparationError::Value) => json!({"error":"Value"}),
        Err(PreparationError::Io { errno }) => json!({"error":"Io","errno":errno}),
        Err(PreparationError::DepthLimit) => json!({"error":"DepthLimit"}),
        Err(PreparationError::Capability) => panic!("unmeasured native metadata capability"),
        Err(PreparationError::Copy(issues)) => {
            json!({"error":"Copy","issues":issues.into_iter().map(|issue|{let (failure,errno)=match issue.failure{CopyFailure::Os{errno}=>("Os",errno),CopyFailure::NamedPipe=>("NamedPipe",None),other=>panic!("unmeasured failure {other:?}")};json!({"source_hex":encode(issue.source.strip_prefix(root).unwrap().as_os_str().as_bytes()),"destination_hex":encode(issue.destination.strip_prefix(root).unwrap().as_os_str().as_bytes()),"failure":failure,"errno":errno})}).collect::<Vec<_>>()})
        }
    }
}
/// Copy issues follow directory listing order, which the file system decides.
fn unordered_issues(mut outcome: Value) -> Value {
    if let Some(issues) = outcome.get_mut("issues").and_then(Value::as_array_mut) {
        issues.sort_by(|a, b| a["source_hex"].as_str().cmp(&b["source_hex"].as_str()));
    }
    outcome
}
#[test]
fn runtime_reference_preparation_effects() {
    assert_ne!(rustix::process::geteuid().as_raw(), 0);
    let reference = reference();
    assert_eq!(reference["runtime"], "3.13.11");
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 41);
    for case in cases {
        let fixture = Fixture::new();
        fixture.build(&case["operations"]);
        assert_eq!(
            fixture.0.as_os_str().as_bytes().len(),
            usize::try_from(case["observed"]["root_byte_length"].as_u64().unwrap()).unwrap()
        );
        assert_eq!(
            snapshot(&fixture.0, &json!([])),
            case["observed"]["before"],
            "before {}",
            case["name"]
        );
        let file_atime = if case["name"] == "normal" {
            Some(fs::metadata(fixture.0.join("source/a")).unwrap())
        } else {
            None
        };
        let link_atime = if case["name"] == "directory-cache-boundary" {
            Some(fs::metadata(fixture.0.join("target")).unwrap())
        } else {
            None
        };
        let mounts = case["mounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                Mount::new(
                    fixture.0.join(m["source"].as_str().unwrap()),
                    PosixPath::new(&String::from(m["target"].as_str().unwrap())),
                    m["read_only"].as_bool().unwrap(),
                )
                .unwrap()
            })
            .collect();
        let preparation = LocalPreparation {
            step_root: fixture.0.join("source"),
            root: fixture.0.join("job"),
            code_dir: fixture.0.join("code"),
            mounts,
            copy: case["copy"].as_bool().unwrap(),
            metadata: native_metadata(),
            limits: CopyLimits::default(),
        };
        let mut observed = outcome(preparation.prepare(), &fixture.0);
        if let Some(before) = file_atime {
            let copied = fs::metadata(fixture.0.join("code/a")).unwrap();
            observed["copied_file_atime_matches_source_before"] = json!(
                (copied.atime(), copied.atime_nsec()) == (before.atime(), before.atime_nsec())
            );
            let copied = fs::metadata(fixture.0.join("code")).unwrap();
            let source = fs::metadata(fixture.0.join("source")).unwrap();
            observed["copied_directory_atime_matches_source_after"] = json!(
                (copied.atime(), copied.atime_nsec()) == (source.atime(), source.atime_nsec())
            );
        }
        if let Some(before) = link_atime {
            let copied = fs::metadata(fixture.0.join("code/link")).unwrap();
            observed["followed_directory_atime_matches_source_before"] = json!(
                (copied.atime(), copied.atime_nsec()) == (before.atime(), before.atime_nsec())
            );
            let copied = fs::metadata(fixture.0.join("code/plain")).unwrap();
            let source = fs::metadata(fixture.0.join("source/plain")).unwrap();
            observed["ordinary_directory_atime_matches_source_after"] = json!(
                (copied.atime(), copied.atime_nsec()) == (source.atime(), source.atime_nsec())
            );
        }
        assert_eq!(
            unordered_issues(observed),
            unordered_issues(case["observed"]["outcome"].clone()),
            "outcome {}",
            case["name"]
        );
        assert_eq!(
            snapshot(&fixture.0, &case["metadata_paths"]),
            case["observed"]["after"],
            "after {}",
            case["name"]
        );
    }
}

struct InlineTestExecutor;
impl cannery_runner::local_preparation::PreparationExecutor for InlineTestExecutor {
    fn execute(
        &self,
        preparation: LocalPreparation,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), PreparationError>> + Send + '_>,
    > {
        Box::pin(async move { preparation.prepare() })
    }
}
fn step() -> cannery_runner::launcher::StepSpec {
    cannery_runner::launcher::StepSpec {
        job_id: String::from("synthetic"),
        label: String::from("producer"),
        image: String::from("unused"),
        command: vec![
            String::from("python3"),
            String::from("-c"),
            String::from("import time;time.sleep(30)"),
        ],
        args: vec![],
        env: vec![],
        resources: cannery_runner::launcher::Resources::default(),
        egress: vec![],
        mounts: vec![],
        workdir: None,
        open_network: false,
    }
}
#[tokio::test]
async fn launcher_defers_filesystem_and_preserves_workdir() {
    use cannery_runner::{local_preparation::LocalLauncher, local_process::LocalProcessBackend};
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("source")).unwrap();
    fs::write(fixture.0.join("source/a"), b"abc").unwrap();
    fs::create_dir(fixture.0.join("job")).unwrap();
    let backend = LocalProcessBackend::new(
        PathBuf::from("/synthetic"),
        PathBuf::from("/synthetic"),
        true,
    )
    .unwrap();
    let launcher = LocalLauncher::new(backend, fixture.0.join("source"));
    launcher.start().await;
    let local = launcher.prepare(step(), fixture.0.join("job"));
    assert!(!fixture.0.join("job/tmp").exists());
    let ready = local
        .prepare_with(
            &InlineTestExecutor,
            CopyLimits::default(),
            native_metadata(),
        )
        .await
        .unwrap();
    assert_eq!(ready.code_dir(), fixture.0.join("job.code"));
    assert_eq!(fs::read(fixture.0.join("job.code/a")).unwrap(), b"abc");
    local.cleanup().await;
    assert!(fixture.0.join("job.code/a").exists());
    launcher.release_job(&String::from("synthetic")).await;
    launcher.aclose().await;
    fs::create_dir(fixture.0.join("second")).unwrap();
    fs::create_dir(fixture.0.join("cache")).unwrap();
    let mut spec = step();
    spec.mounts = vec![
        Mount::new(
            fixture.0.join("source"),
            PosixPath::new(&String::from("/cr/code")),
            true,
        )
        .unwrap(),
        Mount::new(
            fixture.0.join("cache"),
            PosixPath::new(&String::from("/cr/cache")),
            false,
        )
        .unwrap(),
    ];
    spec.workdir = Some(PosixPath::new(&String::from("/cr/code/../cache")));
    let ready = launcher
        .prepare(spec, fixture.0.join("second"))
        .prepare_with(
            &InlineTestExecutor,
            CopyLimits::default(),
            native_metadata(),
        )
        .await
        .unwrap();
    assert_eq!(
        ready.code_dir().as_os_str(),
        fixture.0.join("second/code/../cache").as_os_str()
    );
    assert!(!fixture.0.join("second.code").exists());
    assert_eq!(fs::read(fixture.0.join("second/code/a")).unwrap(), b"abc");
    assert!(fixture.0.join("second/cache").is_symlink());
}
