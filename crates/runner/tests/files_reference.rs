//! Native filesystem observations against the actual frozen Python functions.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;

use cannery_runner::{
    files::{self, TreeError},
    paths::{self, PathError},
};
use num_bigint::BigInt;
use rustix::fs::{CWD, Mode, mkfifoat};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{PermissionsExt, symlink},
    },
    path::PathBuf,
};

fn bytes(hex: &str) -> Vec<u8> {
    assert_eq!(hex.len() % 2, 0);
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}
fn native(value: &Value) -> PathBuf {
    let bytes = bytes(value["hex"].as_str().unwrap());
    let text = cannery_core::text::from_codepoints(
        value["codepoints"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| u32::try_from(p.as_u64().unwrap()).unwrap())
            .collect(),
    );
    if let Some(text) = text {
        if text.contains('\0') {
            assert_eq!(paths::from_text(&text), Err(PathError::InvalidNul));
            return PathBuf::from(OsString::from_vec(bytes));
        }
        let encoded = paths::from_text(&text).unwrap();
        assert_eq!(encoded.as_os_str().as_bytes(), bytes);
    } else {
        // Unix paths retain their raw bytes; invalid UTF-8 cannot enter a String.
        assert!(std::str::from_utf8(&bytes).is_err());
    }
    PathBuf::from(OsString::from_vec(bytes))
}
fn mode(op: &Value) -> u32 {
    u32::try_from(op["mode"].as_u64().unwrap()).unwrap()
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
        // Refuse an existing root; never overwrite another process's fixture.
        fs::create_dir(&base).unwrap();
        Self {
            base,
            restore: Vec::new(),
        }
    }
    fn create(&mut self, op: &Value) {
        let path = self.base.join(native(&op["path"]));
        match op["op"].as_str().unwrap() {
            "mkdir" => {
                fs::create_dir(&path).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(mode(op))).unwrap();
            }
            "write" => {
                fs::write(&path, bytes(op["bytes_hex"].as_str().unwrap())).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(mode(op))).unwrap();
            }
            "chmod" => {
                self.restore.push(path.clone());
                fs::set_permissions(path, fs::Permissions::from_mode(mode(op))).unwrap();
            }
            "symlink" => {
                let target = native(&op["target"]);
                let target = match op["target"]["anchor"].as_str().unwrap() {
                    "ROOT" => self.base.join(target),
                    "relative" => target,
                    _ => panic!("unrecognized target anchor"),
                };
                symlink(target, path).unwrap();
            }
            "hardlink" => fs::hard_link(self.base.join(native(&op["source"])), path).unwrap(),
            "fifo" => {
                mkfifoat(CWD, &path, Mode::from_raw_mode(mode(op))).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(mode(op))).unwrap();
            }
            _ => panic!("unrecognized fixture operation"),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for path in &self.restore {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::remove_dir_all(&self.base).unwrap();
    }
}
fn observation(result: Result<BigInt, TreeError>) -> Value {
    match result {
        Ok(size) => json!({"size":serde_json::from_str::<Value>(&size.to_string()).unwrap()}),
        Err(TreeError::Unsafe) => json!({"category":"UnsafeTree","exception":"UnsafeTree"}),
        Err(TreeError::Io { errno }) => {
            json!({"category":"OSError","exception":if errno == Some(13) { "PermissionError" } else { "OSError" },"errno":errno})
        }
        Err(TreeError::Path(PathError::InvalidNul)) => {
            json!({"category":"InputValue","exception":"ValueError"})
        }
        Err(error) => panic!("unexpected static failure: {error:?}"),
    }
}
#[test]
fn every_source_inspection_and_size_observation_matches_without_skips() {
    let corpus: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/runner/tests/fixtures/files_reference.json"
    ))
    .unwrap();
    let cases = corpus["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 48);
    for case in cases {
        let mut fixture = Fixture::new(&corpus["root_profile"]);
        for op in case["operations"].as_array().unwrap() {
            fixture.create(op);
        }
        let input = fixture.base.join(native(&case["input_root"]));
        assert_eq!(
            fixture.base.as_os_str().as_bytes().len(),
            length(&case["root_byte_length"])
        );
        assert_eq!(
            input.as_os_str().as_bytes().len(),
            length(&case["input_root_byte_length"])
        );
        let expected = if matches!(
            case["name"].as_str(),
            Some("relative-self-cycle" | "relative-two-cycle" | "cycle-and-regular-file")
        ) {
            json!({"category":"UnsafeTree","exception":"UnsafeTree"})
        } else {
            case["check_tree"].clone()
        };
        assert_eq!(
            observation(files::check_tree(&input)),
            expected,
            "check_tree {}",
            case["name"]
        );
        assert_eq!(
            observation(files::tree_size(&input)),
            case["tree_size"],
            "tree_size {}",
            case["name"]
        );
    }
}

#[test]
fn native_tree_links_reject_cycles_and_escaping_dangling_targets() {
    let root =
        std::env::temp_dir().join(format!("cannery-native-link-policy-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    symlink("missing", root.join("inside")).unwrap();
    assert_eq!(files::check_tree(&root).unwrap(), BigInt::from(0));
    symlink("../outside/missing", root.join("escape")).unwrap();
    assert!(matches!(files::check_tree(&root), Err(TreeError::Unsafe)));
    fs::remove_file(root.join("escape")).unwrap();
    symlink("cycle", root.join("cycle")).unwrap();
    assert!(matches!(files::check_tree(&root), Err(TreeError::Unsafe)));
    assert!(matches!(
        files::check_tree(&root.join("cycle")),
        Err(TreeError::Unsafe)
    ));
    fs::remove_dir_all(root).unwrap();
}
