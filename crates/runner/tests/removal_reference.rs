//! Complete filesystem effects from the frozen, unmodified Python removal helpers.
#![forbid(unsafe_code)]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;

use cannery_runner::{paths, removal};
use rustix::fs::{Access, AtFlags, CWD, Mode, accessat, mkfifoat};
use serde_json::{Value, json};
use std::fmt::Write;
use std::{
    ffi::OsString,
    fs, io,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
};

fn unhex(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0);
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap())
        .collect()
}
fn hex(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(result, "{byte:02x}").unwrap();
    }
    result
}
fn native(value: &Value) -> PathBuf {
    let bytes = unhex(value["hex"].as_str().unwrap());
    let points = value["codepoints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|point| u32::try_from(point.as_u64().unwrap()).unwrap())
        .collect();
    if let Some(text) = cannery_core::text::from_codepoints(points) {
        if text.contains('\0') {
            assert_eq!(paths::from_text(&text), Err(paths::PathError::InvalidNul));
            return PathBuf::from(OsString::from_vec(bytes));
        }
        let encoded = paths::from_text(&text).unwrap();
        assert_eq!(encoded.as_os_str().as_bytes(), bytes);
    } else {
        assert!(std::str::from_utf8(&bytes).is_err());
    }
    PathBuf::from(OsString::from_vec(bytes))
}
fn native_observation(bytes: &[u8]) -> Value {
    let mut tail = bytes;
    let mut points = Vec::new();
    while !tail.is_empty() {
        match std::str::from_utf8(tail) {
            Ok(valid) => {
                points.extend(valid.chars().map(u32::from));
                break;
            }
            Err(error) => {
                points.extend(
                    std::str::from_utf8(&tail[..error.valid_up_to()])
                        .unwrap()
                        .chars()
                        .map(u32::from),
                );
                let count = error
                    .error_len()
                    .unwrap_or(tail.len() - error.valid_up_to());
                points.extend(
                    tail[error.valid_up_to()..error.valid_up_to() + count]
                        .iter()
                        .map(|byte| 0xdc00 + u32::from(*byte)),
                );
                tail = &tail[error.valid_up_to() + count..];
            }
        }
    }
    let value = json!({"hex":hex(bytes),"codepoints":points});
    assert_eq!(native(&value).as_os_str().as_bytes(), bytes);
    value
}
fn mode(value: &Value) -> u32 {
    u32::try_from(value["mode"].as_u64().unwrap()).unwrap()
}
fn length(value: &Value) -> usize {
    usize::try_from(value.as_u64().unwrap()).unwrap()
}

struct Fixture {
    base: PathBuf,
    restore: Vec<PathBuf>,
}
impl Fixture {
    fn new(profile: &Value) -> Self {
        let suffix = format!("{:08x}", std::process::id());
        assert_eq!(suffix.len(), length(&profile["suffix_byte_length"]));
        let base = PathBuf::from(format!("{}{suffix}", profile["prefix"].as_str().unwrap()));
        assert_eq!(
            base.as_os_str().as_bytes().len(),
            length(&profile["root_byte_length"])
        );
        // Exclusive creation refuses any pre-existing object owned by another run.
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            base,
            restore: Vec::new(),
        }
    }
    fn create(&mut self, operation: &Value) {
        let path = self.base.join(native(&operation["path"]));
        match operation["op"].as_str().unwrap() {
            "mkdir" => {
                fs::create_dir(&path).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(mode(operation))).unwrap();
            }
            "write" => {
                fs::write(&path, unhex(operation["bytes_hex"].as_str().unwrap())).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(mode(operation))).unwrap();
            }
            "chmod" => {
                self.restore.push(path.clone());
                fs::set_permissions(path, fs::Permissions::from_mode(mode(operation))).unwrap();
            }
            "symlink" => {
                let target = native(&operation["target"]);
                let target = match operation["target"]["anchor"].as_str().unwrap() {
                    "ROOT" => self.base.join(target),
                    "relative" => target,
                    _ => panic!("unexpected link target anchor"),
                };
                symlink(target, path).unwrap();
            }
            "hardlink" => {
                fs::hard_link(self.base.join(native(&operation["source"])), path).unwrap();
            }
            "fifo" => {
                mkfifoat(CWD, &path, Mode::from_raw_mode(mode(operation))).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(mode(operation))).unwrap();
            }
            _ => panic!("unexpected fixture operation"),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.restore.sort_by_key(|path| path.components().count());
        for path in &self.restore {
            if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.file_type().is_symlink()) {
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
            }
        }
        let result = fs::remove_dir_all(&self.base);
        if !std::thread::panicking() {
            result.unwrap();
        }
    }
}
fn os_failure(error: &io::Error) -> Value {
    let errno = error.raw_os_error().expect("native filesystem errno");
    let exception = match errno {
        1 | 13 => "PermissionError",
        2 => "FileNotFoundError",
        17 => "FileExistsError",
        20 => "NotADirectoryError",
        21 => "IsADirectoryError",
        _ => "OSError",
    };
    json!({"exception":exception,"errno":errno})
}
fn snapshot(base: &Path) -> Value {
    let mut entries = Vec::new();
    visit(base, Path::new(""), &mut entries);
    Value::Array(entries)
}
fn visit(base: &Path, relative: &Path, entries: &mut Vec<Value>) {
    let path = if relative.as_os_str().is_empty() {
        base.to_owned()
    } else {
        base.join(relative)
    };
    let mut entry = json!({"path":native_observation(relative.as_os_str().as_bytes())});
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => {
            entry["stat_error"] = os_failure(&error);
            entries.push(entry);
            return;
        }
    };
    entry["mode"] = json!(metadata.permissions().mode() & 0o7777);
    let kind = metadata.file_type();
    if kind.is_symlink() {
        entry["type"] = json!("symlink");
        let target = fs::read_link(&path).unwrap();
        let target_bytes = target.as_os_str().as_bytes();
        entry["target_byte_length"] = json!(target_bytes.len());
        let mut prefix = base.as_os_str().as_bytes().to_vec();
        prefix.push(b'/');
        let (anchor, suffix) = if let Some(suffix) = target_bytes.strip_prefix(prefix.as_slice()) {
            ("ROOT", suffix)
        } else {
            ("relative", target_bytes)
        };
        let mut target = native_observation(suffix);
        target["anchor"] = json!(anchor);
        entry["target"] = target;
        entries.push(entry);
    } else if kind.is_dir() {
        entry["type"] = json!("directory");
        let directory = match fs::read_dir(&path) {
            Ok(directory) => directory,
            Err(error) => {
                entry["list_error"] = os_failure(&error);
                entries.push(entry);
                return;
            }
        };
        let mut names: Vec<_> = directory.map(|item| item.unwrap().file_name()).collect();
        names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        entries.push(entry);
        for name in names {
            visit(base, &relative.join(name), entries);
        }
    } else if kind.is_file() {
        entry["type"] = json!("file");
        match fs::read(&path) {
            Ok(content) => entry["bytes_hex"] = json!(hex(&content)),
            Err(error) => entry["read_error"] = os_failure(&error),
        }
        entries.push(entry);
    } else {
        use std::os::unix::fs::FileTypeExt;
        assert!(kind.is_fifo(), "unexpected filesystem class");
        entry["type"] = json!("fifo");
        entries.push(entry);
    }
}
fn require_nonroot() {
    assert!(
        !rustix::process::getuid().is_root(),
        "nonroot real uid required"
    );
    assert!(
        !rustix::process::geteuid().is_root(),
        "nonroot effective uid required"
    );
}

#[test]
fn all_source_removal_effects_match_without_masks_or_skips() {
    require_nonroot();
    let corpus: Value =
        serde_json::from_str(runtime_reference!("/tests/fixtures/removal_reference.json")).unwrap();
    let cases = corpus["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 48);
    let mut calls = 0;
    let mut restricted_cases = 0;
    for case in cases {
        let mut fixture = Fixture::new(&corpus["root_profile"]);
        assert_eq!(
            fixture.base.as_os_str().as_bytes().len(),
            length(&case["root_byte_length"])
        );
        for operation in case["operations"].as_array().unwrap() {
            fixture.create(operation);
        }
        if !case["restricted_directory"].is_null() {
            let path = fixture.base.join(native(&case["restricted_directory"]));
            assert_eq!(
                accessat(
                    CWD,
                    path,
                    Access::READ_OK | Access::EXEC_OK,
                    AtFlags::empty()
                ),
                Err(rustix::io::Errno::ACCESS),
                "actual permission refusal required"
            );
            restricted_cases += 1;
        }
        assert_eq!(
            snapshot(&fixture.base),
            case["before"],
            "before {}",
            case["name"]
        );
        let path = fixture.base.join(native(&case["input_root"]));
        let observations = case["observations"].as_array().unwrap();
        assert_eq!(observations.len(), length(&case["call_count"]));
        for (index, expected) in observations.iter().enumerate() {
            let outcome = match case["method"].as_str().unwrap() {
                "remove_tree" => {
                    removal::remove_tree(&path);
                    json!({"returned":null})
                }
                "make_writable" => match removal::make_writable(&path) {
                    Ok(()) => json!({"returned":null}),
                    Err(paths::PathError::InvalidNul) => json!({"exception":"ValueError"}),
                    Err(error) => panic!("unexpected static helper error: {error:?}"),
                },
                _ => panic!("unexpected source method"),
            };
            assert_eq!(
                outcome, expected["outcome"],
                "outcome {} call {index}",
                case["name"]
            );
            assert_eq!(
                snapshot(&fixture.base),
                expected["after"],
                "after {} call {index}",
                case["name"]
            );
            calls += 1;
        }
    }
    assert_eq!(calls, 74);
    assert_eq!(restricted_cases, 12);
}
