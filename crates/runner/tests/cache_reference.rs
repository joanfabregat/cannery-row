//! Every recipe observes the actual frozen production cache, not directory labels.
#![allow(clippy::expect_used, clippy::panic, clippy::too_many_lines)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;

use cannery_runner::{
    cache::{CacheError, CacheEvent, CacheLog, CacheRoot},
    files::TreeError,
    removal::remove_tree,
};
use num_bigint::BigInt;
use rustix::fs::{AtFlags, CWD, Timespec, Timestamps, utimensat};
use serde_json::{Value, json};
use std::fmt::Write;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
#[derive(Default)]
struct Log(Mutex<Vec<CacheEvent>>);
impl CacheLog for Log {
    fn event(&self, event: CacheEvent) -> Result<(), CacheError> {
        self.0.lock().expect("synthetic log mutex").push(event);
        Ok(())
    }
}
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).expect("test entropy");
        let name = bytes.iter().fold(String::new(), |mut name, byte| {
            write!(&mut name, "{byte:02x}").expect("string write");
            name
        });
        let parent = std::env::var_os("CANNERY_CACHE_TEST_TMP").map_or_else(
            || {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .and_then(Path::parent)
                    .expect("workspace layout")
                    .join("target/cache-reference-layouts")
            },
            PathBuf::from,
        );
        fs::create_dir_all(&parent).expect("owned cache fixture parent");
        let path = parent.join(format!("cache-native-{name}"));
        fs::create_dir(&path).expect("owned test root");
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        remove_tree(&self.0);
    }
}
fn text(value: &Value) -> Result<String, CacheError> {
    if let Some(points) = value.get("python_codepoints") {
        // These three source-only recipes contain lone surrogates. Assert the
        // native text boundary rejects them, then retain their failure/state checks.
        let native: Option<String> = points
            .as_array()
            .expect("codepoints")
            .iter()
            .map(|point| {
                char::from_u32(u32::try_from(point.as_u64().expect("codepoint")).expect("u32"))
            })
            .collect();
        assert_eq!(native, None, "source recipe must contain invalid Unicode");
        assert!(serde_json::from_str::<String>("\"\\ud800\"").is_err());
    }
    serde_json::from_value(value.clone()).map_err(|_| CacheError::Repository)
}
fn native(value: &Value) -> PathBuf {
    // Fixture transport includes invalid native paths to exercise cache refusals.
    PathBuf::from(text(value).expect("native text recipe"))
}
fn points(mut bytes: &[u8]) -> Vec<u32> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        match std::str::from_utf8(bytes) {
            Ok(text) => {
                out.extend(text.chars().map(u32::from));
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                out.extend(
                    std::str::from_utf8(&bytes[..valid])
                        .expect("valid prefix")
                        .chars()
                        .map(u32::from),
                );
                let end = valid + error.error_len().unwrap_or(bytes.len() - valid);
                out.extend(
                    bytes[valid..end]
                        .iter()
                        .map(|&byte| 0xdc00 + u32::from(byte)),
                );
                bytes = &bytes[end..];
            }
        }
    }
    out
}
fn transport(data: &[u8]) -> Value {
    match std::str::from_utf8(data) {
        Ok(text) => json!(text),
        // A fixture transport for raw Unix path bytes, never a production string.
        Err(_) => json!({"python_codepoints": points(data)}),
    }
}
fn path(base: &Path, aliases: &BTreeMap<String, PathBuf>, value: &Value) -> PathBuf {
    let path = native(value);
    let bytes = path.as_os_str().as_bytes();
    if bytes.starts_with(b"$") {
        let mut parts = bytes.splitn(2, |&b| b == b'/');
        let alias = std::str::from_utf8(&parts.next().expect("alias")[1..]).expect("ASCII alias");
        let anchor = &aliases[alias];
        if let Some(rest) = parts.next() {
            anchor.join(std::ffi::OsString::from_vec(rest.to_vec()))
        } else {
            anchor.clone()
        }
    } else {
        base.join(path)
    }
}
fn relative(path: &Path) -> PathBuf {
    let cwd = std::env::current_dir().expect("cwd");
    let base: Vec<_> = cwd.components().collect();
    let target: Vec<_> = path.components().collect();
    let common = base.iter().zip(&target).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..base.len() {
        out.push("..");
    }
    for part in &target[common..] {
        out.push(part.as_os_str());
    }
    out
}
fn target_path(
    base: &Path,
    aliases: &BTreeMap<String, PathBuf>,
    value: &Value,
    operation: &Value,
) -> PathBuf {
    if operation["anchor"] == "double" {
        let mut bytes = vec![b'/'];
        bytes.extend_from_slice(base.as_os_str().as_bytes());
        PathBuf::from(std::ffi::OsString::from_vec(bytes)).join(native(value))
    } else {
        path(base, aliases, value)
    }
}
fn label(base: &Path, aliases: &BTreeMap<String, PathBuf>, value: &Path) -> Value {
    if !value.is_absolute() {
        return json!({"relative": transport(value.strip_prefix(relative(base)).expect("relative owned root").as_os_str().as_bytes())});
    }
    if value.as_os_str().as_bytes().starts_with(b"//") {
        let mut label = b"$double/".to_vec();
        label.extend_from_slice(
            value
                .strip_prefix(base)
                .expect("owned double anchor")
                .as_os_str()
                .as_bytes(),
        );
        return transport(&label);
    }
    for (name, anchor) in aliases {
        if let Ok(rest) = value.strip_prefix(anchor) {
            let mut bytes = format!("${name}").into_bytes();
            if !rest.as_os_str().is_empty() {
                bytes.push(b'/');
                bytes.extend_from_slice(rest.as_os_str().as_bytes());
            }
            return transport(&bytes);
        }
    }
    transport(
        value
            .strip_prefix(base)
            .expect("owned logical path")
            .as_os_str()
            .as_bytes(),
    )
}
#[allow(clippy::cast_precision_loss)]
fn mtime(path: &Path) -> f64 {
    let stat = fs::metadata(path).expect("mtime snapshot");
    stat.mtime() as f64 + stat.mtime_nsec() as f64 * 1e-9
}
#[allow(clippy::float_cmp)] // Exact equality is the source stat-storage invariant under test.
fn snapshot(cache: &CacheRoot, base: &Path, aliases: &BTreeMap<String, PathBuf>) -> Value {
    let entries: Vec<_> = cache.entries().expect("entries").into_iter().map(|entry| {
        let used = if entry.used < 1_000_000_000.0 { json!({"fixed":entry.used}) } else if entry.path.is_dir() { json!({"live":true,"matches_stat":entry.used == mtime(&entry.path)}) } else { json!({"live":true,"missing":true}) };
        json!({"path":label(base, aliases, &entry.path), "size":entry.size.to_string(), "used":used})
    }).collect();
    let mut held: Vec<_> = cache
        .holds()
        .expect("holds")
        .into_iter()
        .map(|(path, count)| json!([label(base, aliases, &path), count.to_string()]))
        .collect();
    held.sort_by_key(|item| item[0].to_string());
    let mut pending = vec![base.to_owned()];
    let mut tree = Vec::new();
    while let Some(directory) = pending.pop() {
        let Ok(children) = fs::read_dir(directory) else {
            continue;
        };
        for child in children.filter_map(Result::ok) {
            let path = child.path();
            let stat = fs::symlink_metadata(&path).expect("owned snapshot");
            let mut item = json!({"path":label(base, aliases, &path),"mode":stat.mode() & 0o7777});
            if stat.file_type().is_symlink() {
                item["kind"] = json!("link");
                item["target"] =
                    transport(fs::read_link(&path).expect("link").as_os_str().as_bytes());
            } else if stat.is_dir() {
                item["kind"] = json!("directory");
                pending.push(path);
            } else {
                item["kind"] = json!("file");
                item["size"] = json!(stat.len());
            }
            tree.push(item);
        }
    }
    tree.sort_by_key(|item| item["path"].to_string());
    json!({"entries":entries,"held":held,"total":cache.total_bytes().expect("total").to_string(),"tree":tree})
}
fn failure(error: CacheError) -> Value {
    match error {
        CacheError::InvalidCap
        | CacheError::Repository
        | CacheError::Commit
        | CacheError::Key
        | CacheError::PathValue => {
            json!({"error":"ValueError"})
        }
        CacheError::Busy => json!({"error":"CacheBusy"}),
        CacheError::Tree(TreeError::Unsafe) => json!({"error":"UnsafeTree"}),
        CacheError::Io { errno } | CacheError::Tree(TreeError::Io { errno }) => io_failure(errno),
        other => panic!("unexpected static native error: {other:?}"),
    }
}
fn io_failure(errno: Option<i32>) -> Value {
    let name = match errno {
        Some(2) => "FileNotFoundError",
        Some(13 | 1) => "PermissionError",
        Some(17) => "FileExistsError",
        Some(20) => "NotADirectoryError",
        Some(21) => "IsADirectoryError",
        _ => "OSError",
    };
    json!({"error":name,"errno":errno})
}
// Entries with equal timestamps are listed and evicted in directory listing
// order, which the file system decides: compare those lists as sets.
fn unordered(mut observations: Value) -> Value {
    for observation in observations.as_array_mut().into_iter().flatten() {
        if let Some(entries) = observation
            .pointer_mut("/state/entries")
            .and_then(Value::as_array_mut)
        {
            entries.sort_by_key(|item| item["path"].to_string());
        }
        if let Some(evicted) = observation.get_mut("result").and_then(Value::as_array_mut) {
            evicted.sort_by_key(ToString::to_string);
        }
    }
    observations
}
fn logs(log: &Log) -> Value {
    let events = std::mem::take(&mut *log.0.lock().expect("synthetic log"));
    json!(
        events
            .into_iter()
            .map(|event| match event {
                CacheEvent::Evicted(n) => format!(
                    "cannery runner: cache: evicted {n} entr{}",
                    if n == 1 { "y" } else { "ies" }
                ),
                CacheEvent::OverCap { bytes, cap } =>
                    format!("cannery runner: cache: {bytes} bytes in use exceed the cap of {cap}"),
            })
            .collect::<Vec<_>>()
    )
}
#[test]
fn all_actual_cache_recipes_match_complete_effects() {
    let fixture_text = std::env::var_os("CANNERY_CACHE_REFERENCE").map_or_else(
        || {
            runtime_reference!("/../../crates/runner/tests/fixtures/cache_reference.json")
                .to_owned()
        },
        |path| fs::read_to_string(path).expect("generated source corpus"),
    );
    let fixture: Value = serde_json::from_str(&fixture_text).expect("source corpus");
    assert_eq!(fixture["cases"].as_array().expect("cases").len(), 67);
    assert_eq!(
        fixture["cases"]
            .as_array()
            .expect("cases")
            .iter()
            .map(|case| case["observations"].as_array().expect("observations").len())
            .sum::<usize>(),
        255,
    );
    assert_eq!(fixture["tempfile_tmp_max"], 238_328);
    for case in fixture["cases"].as_array().expect("cases") {
        let root = Root::new();
        let log = Arc::new(Log::default());
        let cap: BigInt = case["cap"].as_str().expect("cap").parse().expect("integer");
        let mut cache_path = root.0.join(case["root"].as_str().unwrap_or("."));
        if case["root_style"].is_string() {
            cache_path = relative(&root.0);
            if case["root_style"] == "relative-dotdot" {
                cache_path = cache_path.join("hop/..");
            }
        }
        let cache = match CacheRoot::new(&cache_path, cap, log.clone()) {
            Ok(cache) => cache,
            Err(error) => {
                assert_eq!(
                    json!([failure(error)]),
                    case["observations"],
                    "{}",
                    case["name"]
                );
                continue;
            }
        };
        let mut aliases = BTreeMap::new();
        let mut actual = Vec::new();
        for operation in case["operations"].as_array().expect("operations") {
            let method = operation["method"].as_str().expect("method");
            let result: Result<Value, CacheError> = (|| {
                let target = || target_path(&root.0, &aliases, &operation["path"], operation);
                match method {
                    "mkdir" => {
                        fs::create_dir_all(target()).map_err(|e| CacheError::Io {
                            errno: e.raw_os_error(),
                        })?;
                    }
                    "write" => {
                        fs::write(
                            target(),
                            vec![
                                b'x';
                                usize::try_from(operation["size"].as_u64().expect("size"))
                                    .expect("size")
                            ],
                        )
                        .map_err(|e| CacheError::Io {
                            errno: e.raw_os_error(),
                        })?;
                    }
                    "mtime" => {
                        let ns = operation["ns"].as_i64().expect("ns");
                        let time = Timespec {
                            tv_sec: ns / 1_000_000_000,
                            tv_nsec: ns % 1_000_000_000,
                        };
                        utimensat(
                            CWD,
                            target(),
                            &Timestamps {
                                last_access: time,
                                last_modification: time,
                            },
                            AtFlags::empty(),
                        )
                        .expect("owned mtime");
                    }
                    "chmod" => {
                        fs::set_permissions(
                            target(),
                            fs::Permissions::from_mode(
                                u32::try_from(operation["mode"].as_u64().expect("mode"))
                                    .expect("mode"),
                            ),
                        )
                        .expect("owned mode");
                    }
                    "symlink" => {
                        symlink(native(&operation["target"]), target()).expect("owned link");
                    }
                    "remove" => remove_tree(&target()),
                    "open" => cache.open()?,
                    "close" => cache.close()?,
                    "second_open" => {
                        let second =
                            CacheRoot::new(&root.0, BigInt::from(20u64 << 30), log.clone())?;
                        second.open()?;
                        second.close()?;
                    }
                    "code_path" => {
                        return cache
                            .code_path(&text(&operation["repo"])?, &text(&operation["commit"])?)
                            .map(|path| label(&root.0, &aliases, &path));
                    }
                    "setup_path" => {
                        return cache
                            .setup_path(&text(&operation["key"])?)
                            .map(|path| label(&root.0, &aliases, &path));
                    }
                    "staging" => {
                        let prefix = native(&operation["prefix"]);
                        let staged = cache.staging(&prefix)?;
                        let suffix = staged
                            .file_name()
                            .expect("name")
                            .as_bytes()
                            .strip_prefix(prefix.as_os_str().as_bytes())
                            .expect("prefix");
                        let valid = suffix.len() == 8
                            && suffix.iter().all(|byte| {
                                byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_'
                            });
                        let mode = fs::metadata(&staged).expect("stage metadata").mode() & 0o7777;
                        let absolute = staged.is_absolute();
                        aliases.insert(
                            operation["alias"].as_str().expect("alias").to_owned(),
                            staged,
                        );
                        let unique = aliases
                            .values()
                            .collect::<std::collections::BTreeSet<_>>()
                            .len()
                            == aliases.len();
                        return Ok(
                            json!({"unique":unique,"mode":mode,"suffix_valid":valid,"absolute":absolute}),
                        );
                    }
                    "lookup" => return cache.lookup(&target()).map(|value| json!(value)),
                    "acquire" => return cache.acquire(&target()).map(|value| json!(value)),
                    "hold" => cache.hold(&target())?,
                    "release" => cache.release(
                        &operation["paths"]
                            .as_array()
                            .expect("release")
                            .iter()
                            .map(|value| target_path(&root.0, &aliases, value, operation))
                            .collect::<Vec<_>>(),
                    )?,
                    "publish" => {
                        return cache
                            .publish(
                                &path(&root.0, &aliases, &operation["staging"]),
                                &target(),
                                operation["hold"].as_bool().expect("hold"),
                            )
                            .map(|value| json!(value));
                    }
                    "discard" => cache.discard(&path(&root.0, &aliases, &operation["staging"])),
                    "evict" => {
                        return cache.evict().map(|paths| {
                            json!(
                                paths
                                    .iter()
                                    .map(|path| label(&root.0, &aliases, path))
                                    .collect::<Vec<_>>()
                            )
                        });
                    }
                    _ => panic!("unknown fixture operation"),
                }
                Ok(Value::Null)
            })();
            let mut outcome = match result {
                Ok(result) => {
                    let mut outcome = json!({"result":result});
                    if ["lookup", "acquire", "publish"].contains(&method) && result == true {
                        let used = mtime(&path(&root.0, &aliases, &operation["path"]));
                        let end = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .expect("time")
                            .as_secs_f64();
                        outcome["touch_recent"] = json!((used - end).abs() <= 1.0);
                    }
                    outcome
                }
                Err(error) => failure(error),
            };
            outcome["log"] = logs(&log);
            outcome["state"] = snapshot(&cache, &root.0, &aliases);
            actual.push(outcome);
        }
        assert_eq!(
            unordered(json!(actual)),
            unordered(case["observations"].clone()),
            "case {}",
            case["name"]
        );
        cache.close().expect("close test cache");
    }
}
#[test]
fn concurrent_holds_and_actual_same_process_lock_contention() {
    let root = Root::new();
    let log = Arc::new(Log::default());
    let cache = Arc::new(CacheRoot::new(&root.0, BigInt::from(100), log.clone()).expect("cache"));
    cache.open().expect("open");
    let second = CacheRoot::new(&root.0, BigInt::from(100), log).expect("second");
    assert_eq!(second.open(), Err(CacheError::Busy));
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let cache = &cache;
            scope.spawn(move || {
                for _ in 0..100 {
                    cache.hold(Path::new("unknown")).expect("hold");
                }
            });
        }
    });
    assert_eq!(
        cache
            .holds()
            .expect("holds")
            .into_iter()
            .find(|(path, _)| path == Path::new("unknown"))
            .expect("held unknown")
            .1,
        BigInt::from(800)
    );
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let cache = &cache;
            scope.spawn(move || {
                for _ in 0..100 {
                    cache.release(&[PathBuf::from("unknown")]).expect("release");
                }
            });
        }
    });
    assert_eq!(cache.holds().expect("holds"), Vec::new());
    cache.close().expect("close");
    second.open().expect("released OS flock");
}
