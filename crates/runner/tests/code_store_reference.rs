//! Authored code-store publication, cleanup and ownership regression cases.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines
)]
use cannery_runner::{
    cache::{CacheError, CacheEvent, CacheLog, CacheRoot},
    cancellation::CancellationEvent,
    code_store::{
        CodeCancellation, CodeError, CodeFuture, CodeLimits, CodeSlot, CodeStore, FsExecutor,
        FsOperation, FsReceipt, GitHubSource, OperationKind, SlotConfig,
    },
    removal::remove_tree,
};
use num_bigint::BigInt;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fmt::Write,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
#[derive(Default)]
struct Log {
    fail: bool,
}
impl CacheLog for Log {
    fn event(&self, _: CacheEvent) -> Result<(), CacheError> {
        if self.fail {
            Err(CacheError::Io { errno: Some(5) })
        } else {
            Ok(())
        }
    }
}
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).expect("fixture entropy");
        let name = bytes.iter().fold(String::new(), |mut name, v| {
            write!(&mut name, "{v:02x}").expect("string write");
            name
        });
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/code-store-layouts");
        fs::create_dir_all(&parent).expect("owned fixture parent");
        let root = parent.join(name);
        fs::create_dir(&root).expect("owned fixture root");
        Self(root)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        remove_tree(&self.0);
    }
}
struct Executor {
    events: Arc<Mutex<Vec<String>>>,
    failure: String,
}
impl FsExecutor for Executor {
    fn execute(&self, op: FsOperation, _: CodeCancellation) -> CodeFuture<'_, FsReceipt> {
        Box::pin(async move {
            let name = match op.kind() {
                OperationKind::Acquire => "acquire",
                OperationKind::Staging => "staging",
                OperationKind::Publish => "publish",
                OperationKind::Discard => "discard",
                _ => "",
            };
            if !name.is_empty() {
                self.events.lock().expect("events").push(name.into());
            }
            if (!name.is_empty() && name == self.failure)
                || (name == "discard" && self.failure == "cleanup")
            {
                return Err(CodeError::Cache(CacheError::Io { errno: Some(5) }));
            }
            if name == "publish" && self.failure == "publish-unsafe" {
                return Err(CodeError::InvalidCode);
            }
            op.run()
        })
    }
    fn detach(&self, operations: Vec<FsOperation>) {
        for operation in operations {
            let _ = operation.run();
        }
    }
}
struct Source {
    events: Arc<Mutex<Vec<String>>>,
    failure: String,
    payload: Vec<u8>,
    limits: Mutex<Vec<BigInt>>,
    arguments: Mutex<Vec<Vec<String>>>,
    barrier: Option<tokio::sync::Barrier>,
}
impl GitHubSource for Source {
    fn verify_commit<'a>(
        &'a self,
        repo: &'a str,
        commit: &'a str,
        _: CodeCancellation,
    ) -> CodeFuture<'a, ()> {
        Box::pin(async move {
            self.events.lock().expect("events").push("verify".into());
            self.arguments
                .lock()
                .expect("arguments")
                .push(vec![repo.to_owned(), commit.to_owned()]);
            if let Some(barrier) = &self.barrier {
                barrier.wait().await;
            }
            match self.failure.as_str() {
                "verify" => Err(CodeError::GitHub),
                "callback" => Err(CodeError::Callback),
                _ => Ok(()),
            }
        })
    }
    fn download_tarball<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        path: PathBuf,
        limit: BigInt,
        _: CodeCancellation,
    ) -> CodeFuture<'a, ()> {
        Box::pin(async move {
            self.events.lock().expect("events").push("download".into());
            self.limits.lock().expect("limits").push(limit);
            match self.failure.as_str() {
                "download" => return Err(CodeError::GitHub),
                "io" => return Err(CodeError::Cache(CacheError::Io { errno: Some(5) })),
                _ => {}
            }
            fs::write(path, &self.payload).map_err(|e| {
                CodeError::Cache(CacheError::Io {
                    errno: e.raw_os_error(),
                })
            })?;
            Ok(())
        })
    }
}
fn signal() -> CodeCancellation {
    CodeCancellation::from_event(CancellationEvent::new())
}

#[path = "support/archive.rs"]
mod archive_fixtures;
fn normal_payload() -> Vec<u8> {
    archive_fixtures::archive(
        Some(archive_fixtures::COMMIT),
        &[
            archive_fixtures::Entry::directory("root/"),
            archive_fixtures::Entry::file("root/a", b"data"),
        ],
    )
}
fn case(recipe: Value, payload: &[u8]) -> Value {
    let mut hex = String::new();
    for byte in payload {
        write!(&mut hex, "{byte:02x}").expect("string write");
    }
    let mut value = serde_json::Map::new();
    value.insert("recipe".into(), recipe);
    value.insert("archive_hex".into(), Value::String(hex));
    Value::Object(value)
}
fn bytes(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|value| {
            u8::from_str_radix(std::str::from_utf8(value).expect("hex"), 16).expect("hex byte")
        })
        .collect()
}
fn category(error: CodeError) -> &'static str {
    use cannery_runner::archive::ExtractError;
    match error {
        CodeError::CodeNotAllowed => "code_not_allowed",
        CodeError::RunnerError => "runner_error",
        CodeError::Cache(CacheError::Busy) => "cache_busy",
        CodeError::InvalidCode | CodeError::Archive(ExtractError::Unsafe) => "invalid_code",
        CodeError::Cache(CacheError::Io { .. }) | CodeError::Archive(ExtractError::Io { .. }) => {
            "io"
        }
        CodeError::Cache(CacheError::Repository | CacheError::Commit | CacheError::InvalidCap) => {
            "value"
        }
        _ => "callback",
    }
}
fn outcome(
    result: Result<PathBuf, CodeError>,
    root: &Path,
    returned: &mut Vec<String>,
) -> &'static str {
    match result {
        Ok(path) => {
            returned.push(
                path.strip_prefix(root)
                    .expect("returned cache path")
                    .to_str()
                    .expect("UTF8 returned recipe")
                    .to_owned(),
            );
            "ok"
        }
        Err(error) => category(error),
    }
}
async fn observe(case: &Value) -> Value {
    let root = Root::new();
    let path = root.0.join("cache");
    let recipe = &case["recipe"];
    let failure = recipe["failure"].as_str().unwrap_or("").to_owned();
    let events = Arc::new(Mutex::new(Vec::new()));
    let executor = Arc::new(Executor {
        events: events.clone(),
        failure: failure.clone(),
    });
    let log = Arc::new(Log {
        fail: recipe["log_failure"].as_bool().unwrap_or(false),
    });
    let cache = Arc::new(
        CacheRoot::new(
            &path,
            BigInt::from(recipe["cap"].as_u64().unwrap_or(1 << 30)),
            log,
        )
        .expect("cache"),
    );
    cache.open().expect("open");
    let source = Arc::new(Source {
        events: events.clone(),
        failure: recipe["github_failure"]
            .as_str()
            .unwrap_or(&failure)
            .to_owned(),
        payload: bytes(case["archive_hex"].as_str().expect("archive")),
        limits: Mutex::new(Vec::new()),
        arguments: Mutex::new(Vec::new()),
        barrier: recipe["concurrent"]
            .as_bool()
            .filter(|v| *v)
            .map(|_| tokio::sync::Barrier::new(2)),
    });
    let mut limits = CodeLimits::default();
    for (name, value) in recipe["limits"].as_object().into_iter().flatten() {
        let number = BigInt::from(value.as_u64().expect("limit"));
        match name.as_str() {
            "max_tree_bytes" => limits.max_tree_bytes = number,
            "max_archive_bytes" => limits.max_archive_bytes = number,
            "max_files" => limits.max_files = number,
            _ => panic!("unknown fixture limit"),
        }
    }
    let allow = recipe["allow"].as_array().map(|list| {
        list.iter()
            .map(|v| String::from(v.as_str().expect("allow")))
            .collect()
    });
    let store = CodeStore::new(cache.clone(), source.clone(), executor, limits, allow);
    let repo = String::from(recipe["repo"].as_str().unwrap_or("Owner/Repo"));
    let commit = String::from(
        recipe["commit"]
            .as_str()
            .unwrap_or("0123456789abcdef0123456789abcdef01234567"),
    );
    let mut results = Vec::new();
    let mut returned = Vec::new();
    if recipe["concurrent"].as_bool().unwrap_or(false) {
        let (a, b) = tokio::join!(
            store.tree(&repo, &commit, signal()),
            store.tree(&repo, &commit, signal())
        );
        results.extend([a, b].into_iter().map(|r| outcome(r, &path, &mut returned)));
    } else {
        for _ in 0..recipe["calls"].as_u64().unwrap_or(1) {
            results.push(outcome(
                store.tree(&repo, &commit, signal()).await,
                &path,
                &mut returned,
            ));
        }
    }
    let mut entries = cache
        .entries()
        .expect("entries")
        .into_iter()
        .map(|e| {
            json!([
                e.path
                    .strip_prefix(&path)
                    .expect("relative")
                    .to_str()
                    .expect("UTF8"),
                e.size.to_string().parse::<u64>().expect("size")
            ])
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(Value::to_string);
    let mut holds = cache
        .holds()
        .expect("holds")
        .into_iter()
        .map(|(p, n)| {
            json!([
                p.strip_prefix(&path)
                    .expect("relative")
                    .to_str()
                    .expect("UTF8"),
                n.to_string().parse::<u64>().expect("count")
            ])
        })
        .collect::<Vec<_>>();
    holds.sort_by_key(Value::to_string);
    let events = events.lock().expect("events").clone();
    let mut counts = BTreeMap::new();
    for event in &events {
        *counts.entry(event.clone()).or_insert(0_u64) += 1;
    }
    let mut actual = json!({"results":results,"returned":returned,"events":events,"event_counts":counts,"limits":source.limits.lock().expect("limits").iter().map(|v|v.to_string().parse::<u64>().expect("limit")).collect::<Vec<_>>(),"arguments":*source.arguments.lock().expect("arguments"),"entries":entries,"holds":holds,"tmp_count":fs::read_dir(path.join("tmp")).expect("tmp").count()});
    if recipe["concurrent"].as_bool().unwrap_or(false) {
        actual.as_object_mut().expect("object").remove("events");
    }
    cache.close().expect("close");
    actual
}
#[tokio::test]
async fn cold_warm_and_concurrent_publications_transfer_one_hold_per_caller() {
    for recipe in [json!({}), json!({"calls":2}), json!({"concurrent":true})] {
        let actual = observe(&case(recipe.clone(), &normal_payload())).await;
        let calls = if recipe["calls"] == 2 || recipe["concurrent"] == true {
            2
        } else {
            1
        };
        assert_eq!(actual["results"], json!(vec!["ok"; calls]));
        assert_eq!(actual["holds"][0][1], json!(calls));
        assert_eq!(actual["entries"].as_array().expect("entries").len(), 1);
        assert_eq!(actual["tmp_count"], 0);
        assert_eq!(
            actual["event_counts"]["verify"],
            if recipe["concurrent"] == true { 2 } else { 1 }
        );
    }
}
#[tokio::test]
async fn failure_paths_preserve_cleanup_error_precedence_and_publication_state() {
    for (failure, result, staging) in [
        ("verify", "runner_error", 0),
        ("download", "runner_error", 0),
        ("callback", "callback", 0),
        ("io", "io", 0),
        ("staging", "io", 0),
        ("publish", "io", 0),
        ("publish-unsafe", "invalid_code", 0),
        ("cleanup", "io", 1),
    ] {
        let actual = observe(&case(json!({"failure":failure}), &normal_payload())).await;
        assert_eq!(actual["results"], json!([result]), "{failure}");
        assert_eq!(actual["holds"], json!([]), "{failure}");
        if failure == "cleanup" {
            assert_eq!(actual["entries"].as_array().expect("entries").len(), 1);
        }
        assert_eq!(actual["tmp_count"], staging, "{failure}");
    }
    let actual = observe(&case(
        json!({"failure":"cleanup","github_failure":"verify"}),
        &normal_payload(),
    ))
    .await;
    assert_eq!(actual["results"], json!(["io"]));
    assert_eq!(actual["tmp_count"], 1);
}
#[tokio::test]
async fn allowlists_invalid_references_and_acquisition_limits_are_checked() {
    for (recipe, expected) in [
        (json!({"allow":["owner/repo"]}), "ok"),
        (json!({"allow":["Other/Repo"]}), "code_not_allowed"),
        (json!({"allow":[],"repo":"bad"}), "code_not_allowed"),
        (json!({"repo":"bad"}), "value"),
        (json!({"commit":"bad"}), "value"),
        (json!({"limits":{"max_tree_bytes":3}}), "invalid_code"),
        (json!({"limits":{"max_files":0}}), "invalid_code"),
    ] {
        let actual = observe(&case(recipe, &normal_payload())).await;
        assert_eq!(actual["results"], json!([expected]));
        assert_eq!(actual["tmp_count"], 0);
        if expected != "ok" {
            assert_eq!(actual["holds"], json!([]));
        }
    }
    let actual = observe(&case(
        json!({"limits":{"max_archive_bytes":17}}),
        &normal_payload(),
    ))
    .await;
    assert_eq!(actual["results"], json!(["ok"]));
    assert_eq!(actual["limits"], json!([17]));
}
#[tokio::test]
async fn malformed_or_wrong_commit_archives_are_never_published() {
    let normal = normal_payload();
    let mut bad_crc = normal.clone();
    let index = bad_crc.len() - 8;
    bad_crc[index] ^= 1;
    let wrong = archive_fixtures::archive(
        Some("wrong"),
        &[archive_fixtures::Entry::directory("root/")],
    );
    let escaping_link = archive_fixtures::archive(
        Some(archive_fixtures::COMMIT),
        &[
            archive_fixtures::Entry::directory("root/"),
            archive_fixtures::Entry::link("root/link", "../outside"),
        ],
    );
    for (payload, expected) in [
        (b"garbage".to_vec(), "invalid_code"),
        (normal[..3].to_vec(), "invalid_code"),
        (bad_crc, "invalid_code"),
        (wrong, "runner_error"),
        (escaping_link, "invalid_code"),
    ] {
        let actual = observe(&case(json!({}), &payload)).await;
        assert_eq!(actual["results"], json!([expected]));
        assert_eq!(actual["event_counts"]["publish"], Value::Null);
        assert_eq!(actual["holds"], json!([]));
        assert_eq!(actual["tmp_count"], 0);
    }
}
#[tokio::test]
async fn publication_logging_failure_keeps_the_cached_entry_and_cleans_staging() {
    let actual = observe(&case(
        json!({"cap":1,"log_failure":true}),
        &normal_payload(),
    ))
    .await;
    assert_eq!(actual["results"], json!(["io"]));
    assert_eq!(actual["entries"].as_array().expect("entries").len(), 1);
    assert_eq!(actual["holds"], json!([]));
    assert_eq!(actual["tmp_count"], 0);
}
#[tokio::test]
async fn setup_adapter_preserves_native_archive_failure_categories_and_cleanup() {
    use cannery_runner::setup::{Cancellation, CodeRef, CodeSource, SetupError};
    for (payload, expected) in [
        (normal_payload(), None),
        (b"invalid gzip".to_vec(), Some(SetupError::InvalidCode)),
        (
            archive_fixtures::archive(None, &[archive_fixtures::Entry::directory("root/")]),
            Some(SetupError::RunnerError),
        ),
    ] {
        let root = Root::new();
        let cache = Arc::new(
            CacheRoot::new(
                &root.0.join("cache"),
                1_000_000.into(),
                Arc::new(Log::default()),
            )
            .expect("cache"),
        );
        cache.open().expect("open");
        let events = Arc::new(Mutex::new(Vec::new()));
        let source = Arc::new(Source {
            events: events.clone(),
            failure: String::new(),
            payload,
            limits: Mutex::new(Vec::new()),
            arguments: Mutex::new(Vec::new()),
            barrier: None,
        });
        let store = CodeStore::new(
            cache.clone(),
            source,
            Arc::new(Executor {
                events,
                failure: String::new(),
            }),
            CodeLimits::default(),
            None,
        );
        let code = CodeRef {
            repo: "Owner/Repo".into(),
            commit: archive_fixtures::COMMIT.into(),
            path: None,
        };
        let (sender, receiver) = tokio::sync::watch::channel(false);
        let actual = CodeSource::tree(&store, &code, Cancellation::from_receiver(receiver)).await;
        drop(sender);
        if let Some(expected) = expected {
            assert_eq!(actual.expect_err("rejection"), expected);
        } else {
            let path = actual.expect("published");
            assert_eq!(fs::read(path.join("a")).expect("file"), b"data");
            assert_eq!(cache.holds().expect("holds").len(), 1);
            cache.release(&[path]).expect("release");
        }
        assert_eq!(cache.holds().expect("holds"), Vec::new());
        assert_eq!(
            fs::read_dir(root.0.join("cache/tmp"))
                .expect("staging")
                .count(),
            0
        );
        cache.close().expect("close");
    }
}
async fn observe_slot(kind: &str) -> Value {
    let root = Root::new();
    let path = root.0.join("cache");
    let log = Arc::new(Log::default());
    let other = if kind == "busy" {
        let c = CacheRoot::new(&path, BigInt::from(1 << 30), log.clone()).expect("other");
        c.open().expect("other open");
        Some(c)
    } else {
        None
    };
    if kind == "file" {
        fs::write(&path, b"x").expect("file");
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let executor = Arc::new(Executor {
        events: events.clone(),
        failure: String::new(),
    });
    let source = Arc::new(Source {
        events,
        failure: String::new(),
        payload: Vec::new(),
        limits: Mutex::new(Vec::new()),
        arguments: Mutex::new(Vec::new()),
        barrier: None,
    });
    let slot = CodeSlot::new(
        SlotConfig {
            root: if kind == "missing" { None } else { Some(path) },
            max_bytes: BigInt::from(if kind == "cap" { 0 } else { 1 << 30 }),
            allowed_repos: None,
        },
        source,
        executor,
        log,
    );
    let (a, b) = tokio::join!(slot.get(signal()), slot.get(signal()));
    let actual = match (a, b) {
        (Ok(a), Ok(b)) => {
            let same = Arc::ptr_eq(&a, &b);
            slot.close().await.expect("close");
            let c = slot.get(signal()).await.expect("reopen");
            json!({"result":"ok","same":same,"reopened":!Arc::ptr_eq(&a,&c)})
        }
        (Err(error), _) | (_, Err(error)) => json!({"result":category(error)}),
    };
    slot.close().await.expect("final close");
    if let Some(other) = other {
        other.close().expect("other close");
    }
    actual
}
#[tokio::test]
async fn slots_share_the_store_and_preserve_configuration_and_lock_failures() {
    for (kind, expected) in [
        (
            "ordinary",
            json!({"result":"ok","same":true,"reopened":true}),
        ),
        ("busy", json!({"result":"cache_busy"})),
        ("file", json!({"result":"runner_error"})),
        ("missing", json!({"result":"runner_error"})),
        ("cap", json!({"result":"value"})),
    ] {
        assert_eq!(observe_slot(kind).await, expected, "{kind}");
    }
}
/// The test executor's work is synchronous and settled before its receipt is
/// suspended. This proves receipt ownership, not a production thread policy.
struct PausedExecutor {
    pause: OperationKind,
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}
impl FsExecutor for PausedExecutor {
    fn execute(&self, operation: FsOperation, _: CodeCancellation) -> CodeFuture<'_, FsReceipt> {
        Box::pin(async move {
            let kind = operation.kind();
            let receipt = operation.run()?;
            if kind == self.pause {
                self.entered.notify_one();
                self.resume.notified().await;
            }
            Ok(receipt)
        })
    }
    fn detach(&self, operations: Vec<FsOperation>) {
        for operation in operations {
            let _ = operation.run();
        }
    }
}
#[tokio::test]
async fn abandoned_settled_receipts_release_acquired_and_published_holds() {
    for kind in [
        OperationKind::Acquire,
        OperationKind::Publish,
        OperationKind::Staging,
    ] {
        let root = Root::new();
        let cache = Arc::new(
            CacheRoot::new(
                &root.0.join("cache"),
                BigInt::from(1 << 30),
                Arc::new(Log::default()),
            )
            .expect("cache"),
        );
        cache.open().expect("open");
        let events = Arc::new(Mutex::new(Vec::new()));
        let source = Arc::new(Source {
            events: events.clone(),
            failure: String::new(),
            payload: normal_payload(),
            limits: Mutex::new(Vec::new()),
            arguments: Mutex::new(Vec::new()),
            barrier: None,
        });
        let initial = Arc::new(Executor {
            events,
            failure: String::new(),
        });
        let repo = String::from("Owner/Repo");
        let commit = String::from("0123456789abcdef0123456789abcdef01234567");
        if kind == OperationKind::Acquire {
            let store = CodeStore::new(
                cache.clone(),
                source.clone(),
                initial,
                CodeLimits::default(),
                None,
            );
            let path = store.tree(&repo, &commit, signal()).await.expect("warm");
            cache.release(&[path]).expect("release first caller");
        }
        let executor = Arc::new(PausedExecutor {
            pause: kind,
            entered: tokio::sync::Notify::new(),
            resume: tokio::sync::Notify::new(),
        });
        let store = Arc::new(CodeStore::new(
            cache.clone(),
            source,
            executor.clone(),
            CodeLimits::default(),
            None,
        ));
        let task = tokio::spawn(async move { store.tree(&repo, &commit, signal()).await });
        executor.entered.notified().await;
        task.abort();
        assert!(task.await.expect_err("aborted").is_cancelled());
        assert_eq!(cache.holds().expect("holds"), [] as [(PathBuf, BigInt); 0]);
        assert_eq!(
            fs::read_dir(root.0.join("cache/tmp")).expect("tmp").count(),
            0
        );
        cache.close().expect("close");
    }
}
#[tokio::test]
async fn get_during_close_observes_installed_store_without_opening_lock() {
    let root = Root::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let source = Arc::new(Source {
        events,
        failure: String::new(),
        payload: Vec::new(),
        limits: Mutex::new(Vec::new()),
        arguments: Mutex::new(Vec::new()),
        barrier: None,
    });
    let executor = Arc::new(PausedExecutor {
        pause: OperationKind::Close,
        entered: tokio::sync::Notify::new(),
        resume: tokio::sync::Notify::new(),
    });
    let slot = Arc::new(CodeSlot::new(
        SlotConfig {
            root: Some(root.0.join("cache")),
            max_bytes: BigInt::from(1 << 30),
            allowed_repos: None,
        },
        source,
        executor.clone(),
        Arc::new(Log::default()),
    ));
    let first = slot.get(signal()).await.expect("first");
    let closer = slot.clone();
    let task = tokio::spawn(async move { closer.close().await });
    executor.entered.notified().await;
    let during = slot.get(signal()).await.expect("get during close");
    let during_same = Arc::ptr_eq(&first, &during);
    executor.resume.notify_one();
    task.await.expect("close task").expect("closed");
    let after = slot.get(signal()).await.expect("new store");
    assert_eq!(
        json!({"during_same":during_same,"after_new":!Arc::ptr_eq(&first,&after)}),
        json!({"during_same":true,"after_new":true})
    );
    executor.resume.notify_one();
    slot.close().await.expect("final close");
}
