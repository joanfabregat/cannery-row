//! Actual production Python observations replayed through required native interfaces.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use cannery_core::json::{Document, DocumentBuilder, Node, NodeId};
use cannery_runner::{
    cache::{CacheError, CacheEvent, CacheLog, CacheRoot},
    launcher::{Outcome, Resources, StepSpec},
    removal::remove_tree,
    setup::{
        self, Cancellation, CodeRef, CodeSource, FsExecutor, FsOperation, FsValue, ProvisionTask,
        RunSetup, SetupError, SetupFuture, SetupSpec, TrustClass,
    },
};
use num_bigint::BigInt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    future::Future,
    os::unix::{
        ffi::OsStringExt,
        fs::{PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
static NEXT: AtomicU64 = AtomicU64::new(0);
fn txt(v: &Value) -> String {
    if let Some(s) = v.as_str() {
        String::from(s)
    } else {
        cannery_core::text::from_codepoints(
            v["codepoints"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| u32::try_from(v.as_u64().unwrap()).unwrap())
                .collect(),
        )
        .unwrap()
    }
}
fn t(v: &str) -> String {
    String::from(v)
}
fn text(v: &String) -> Value {
    v.as_utf8()
        .map_or_else(|| json!({"codepoints":v.codepoints()}), Value::String)
}
fn number(v: &BigInt) -> Value {
    serde_json::from_str(&v.to_string()).unwrap()
}
fn strings(v: &[String]) -> Value {
    json!(v.iter().map(text).collect::<Vec<_>>())
}
fn document(v: &Value) -> Document {
    fn node(b: &mut DocumentBuilder, v: &Value) -> NodeId {
        let n = match v {
            Value::Null => Node::Null,
            Value::Bool(v) => Node::Bool(*v),
            Value::String(_) => Node::String(txt(v)),
            Value::Number(v) => {
                let s = v.to_string();
                if s.contains(['.', 'e', 'E']) {
                    Node::Float(s.parse().unwrap())
                } else {
                    Node::Integer(s.parse().unwrap())
                }
            }
            Value::Array(v) => Node::Array(v.iter().map(|v| node(b, v)).collect()),
            Value::Object(v) if v.len() == 1 && v.contains_key("codepoints") => {
                Node::String(txt(&Value::Object(v.clone())))
            }
            Value::Object(v) if v.len() == 1 && v.contains_key("float") => {
                Node::Float(match v["float"].as_str().unwrap() {
                    "nan" => f64::NAN,
                    "inf" => f64::INFINITY,
                    "-inf" => f64::NEG_INFINITY,
                    _ => unreachable!(),
                })
            }
            Value::Object(v) if v.len() == 1 && v.contains_key("integer_hex") => {
                let hex = v["integer_hex"].as_str().unwrap();
                Node::Integer(
                    BigInt::parse_bytes(hex.strip_prefix("0x").unwrap().as_bytes(), 16).unwrap(),
                )
            }
            Value::Object(v) => Node::Object(v.iter().map(|(k, v)| (t(k), node(b, v))).collect()),
        };
        b.push(n).unwrap()
    }
    let mut b = DocumentBuilder::new();
    let root = node(&mut b, v);
    b.finish(root).unwrap()
}
fn error(e: SetupError) -> Value {
    match e {
        SetupError::Io { errno } => json!({"class":"Io","errno":errno}),
        SetupError::Contract(cannery_runner::launcher::ContractError::Image) => {
            json!({"class":"LaunchError"})
        }
        SetupError::Contract(_) => json!({"class":"Value"}),
        _ => json!({"class":format!("{e:?}")}),
    }
}
fn result<T>(v: Result<T, SetupError>, snapshot: impl FnOnce(T) -> Value) -> Value {
    v.map_or_else(|e| json!({"error":error(e)}), snapshot)
}
fn code(v: &CodeRef) -> Value {
    json!({"repo":text(&v.repo),"commit":text(&v.commit),"path":v.path.as_ref().map(text)})
}
fn setup(v: &SetupSpec) -> Value {
    json!({"run":text(&v.run),"key_files":strings(&v.key_files),"paths":strings(&v.paths),"egress":strings(&v.egress),"open_network":v.open_network,"deadline_seconds":number(&v.deadline_seconds)})
}
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root = PathBuf::from(format!(
            "/tmp/cannery-setup-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        remove_tree(&self.0);
    }
}
fn layout(root: &Path) {
    fs::create_dir_all(root.join("code/steps/dir")).unwrap();
    for (p, bytes) in [
        ("requirements.txt", b"dep==1\n".as_slice()),
        ("execute", b"#!/bin/sh\n"),
        ("dir/nested", b"nested"),
        ("slash\\name", b"backslash"),
    ] {
        fs::write(root.join("code/steps").join(p), bytes).unwrap();
    }
    fs::set_permissions(
        root.join("code/steps/execute"),
        fs::Permissions::from_mode(0o751),
    )
    .unwrap();
    symlink("requirements.txt", root.join("code/steps/link")).unwrap();
    symlink("dir", root.join("code/steps/dirlink")).unwrap();
    fs::write(
        root.join("code/steps")
            .join(std::ffi::OsString::from_vec(vec![255])),
        b"native byte",
    )
    .unwrap();
}
fn path_text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}
// Display opaque filesystem bytes without constructing invalid Unicode strings.
// The actual files and metadata are still inspected on disk.
fn native_snapshot(value: &Value) -> Value {
    match value {
        Value::Object(values) if values.len() == 1 && values.contains_key("codepoints") => {
            Value::String(
                values["codepoints"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|point| {
                        char::from_u32(u32::try_from(point.as_u64().unwrap()).unwrap())
                            .unwrap_or(char::REPLACEMENT_CHARACTER)
                    })
                    .collect(),
            )
        }
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(k, v)| (k.clone(), native_snapshot(v)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(native_snapshot).collect()),
        _ => value.clone(),
    }
}
fn rejects_private_text(value: &Value) -> bool {
    match value {
        Value::Object(values) if values.len() == 1 && values.contains_key("float") => {
            let raw = match values["float"].as_str().unwrap() {
                "nan" => "NaN",
                "inf" => "Infinity",
                "-inf" => "-Infinity",
                _ => unreachable!(),
            };
            assert!(cannery_core::json::decode_str(raw, 64).is_err());
            true
        }
        Value::Object(values) if values.len() == 1 && values.contains_key("codepoints") => {
            let points: Vec<u32> = values["codepoints"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| u32::try_from(v.as_u64().unwrap()).unwrap())
                .collect();
            if points.iter().any(|&p| char::from_u32(p).is_none()) {
                assert!(cannery_core::text::from_codepoints(points).is_none());
                true
            } else {
                false
            }
        }
        Value::Object(values) => values
            .values()
            .map(rejects_private_text)
            .fold(false, |a, b| a | b),
        Value::Array(values) => values
            .iter()
            .map(rejects_private_text)
            .fold(false, |a, b| a | b),
        _ => false,
    }
}
fn native_text(value: &Value) -> Value {
    if value.is_string() {
        value.clone()
    } else if value.get("codepoints").is_some() {
        Value::String(txt(value))
    } else {
        Value::String(value.to_string())
    }
}
fn native_strings(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(native_text).collect()),
        Value::String(value) => Value::Array(
            value
                .chars()
                .map(|c| Value::String(c.to_string()))
                .collect(),
        ),
        Value::Object(items) if items.len() == 1 && items.contains_key("codepoints") => {
            Value::Array(
                txt(value)
                    .chars()
                    .map(|c| Value::String(c.to_string()))
                    .collect(),
            )
        }
        Value::Object(items) => Value::Array(items.keys().cloned().map(Value::String).collect()),
        _ => unreachable!(),
    }
}
fn tree_snapshot(root: &Path) -> Value {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<Value>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let m = fs::symlink_metadata(&p).unwrap();
            let mut item = json!({"path":text(&path_text(p.strip_prefix(root).unwrap())),"mode":m.permissions().mode()&0o7777});
            if m.file_type().is_symlink() {
                item["type"] = json!("link");
                item["target"] = text(&path_text(&fs::read_link(&p).unwrap()));
            } else if m.is_dir() {
                item["type"] = json!("directory");
                walk(root, &p, out);
            } else {
                item["type"] = json!("file");
                item["size"] = json!(m.len());
                item["sha256"] = json!(format!("{:x}", Sha256::digest(fs::read(&p).unwrap())));
            }
            out.push(item);
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort_by(|a, b| {
        txt(&a["path"])
            .codepoints()
            .cmp(&txt(&b["path"]).codepoints())
    });
    json!(out)
}
fn fixture() -> Value {
    let reference = std::env::var_os("CANNERY_SETUP_REFERENCE").map_or_else(
        || {
            runtime_reference!("/../../crates/runner/tests/fixtures/setup_reference.json")
                .to_owned()
        },
        |path| fs::read_to_string(path).unwrap(),
    );
    let value: Value = serde_json::from_str(&reference).unwrap();
    assert_eq!(value["format"], 1);
    assert_eq!(value["python"], "3.13.11");
    assert_eq!(value["unicode"], "15.1.0");
    assert_eq!(
        value["section_counts"],
        json!({"projection":758,"keys":15,"filesystem":42,"provision":21,"concurrency":2})
    );
    for (section, count) in [
        ("projection", 758),
        ("keys", 15),
        ("filesystem", 42),
        ("provision", 21),
        ("concurrency", 2),
    ] {
        assert_eq!(value[section].as_array().unwrap().len(), count);
    }
    assert_eq!(
        value["source_only_ownership_probes"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        value["key_descriptor_flags"],
        json!([{"inheritable":false,"cloexec":true}])
    );
    value
}
fn native_projection_facts(case: &Value, d: &Document) -> Value {
    let mut expected = case["facts"].clone();
    let spec = &case["manifest"]["spec"];
    if !expected["code"].is_null() && expected["code"].get("error").is_none() {
        for key in ["repo", "commit", "path"] {
            if let Some(value) = spec["code"]
                .get(key)
                .filter(|v| key != "path" || !v.is_null())
            {
                expected["code"][key] = native_text(value);
            }
        }
    }
    if spec["setup"].get("activeDeadlineSeconds").is_some() {
        let class = match d
            .field(d.field(d.root(), "spec").unwrap(), "setup")
            .and_then(|s| d.field(s, "activeDeadlineSeconds"))
            .and_then(|id| d.node(id))
        {
            Some(Node::Integer(value)) if value.to_string().parse::<i64>().is_ok() => None,
            Some(Node::Integer(_)) => Some("Overflow"),
            _ => Some("Type"),
        };
        if let Some(class) = class {
            expected["setup"] = json!({"error":{"class":class}});
        }
    }
    if !expected["setup"].is_null() && expected["setup"].get("error").is_none() {
        expected["setup"]["run"] = native_text(&spec["setup"]["run"]);
        for (key, raw) in [
            ("key_files", &spec["setup"]["cache"]["key_files"]),
            ("paths", &spec["setup"]["cache"]["paths"]),
            ("egress", &spec["setup"]["network"]["egress"]),
        ] {
            if !raw.is_null() {
                expected["setup"][key] = native_strings(raw);
            }
        }
    }
    expected
}

#[test]
fn source_projections_and_canonical_keys() {
    let f = fixture();
    for case in f["projection"].as_array().unwrap() {
        if rejects_private_text(&case["manifest"]) {
            continue;
        }
        let d = document(&case["manifest"]);
        let actual = json!({"code":result(setup::code_ref(&d,128),|v|v.as_ref().map_or(Value::Null,code)),"setup":result(setup::setup_spec(&d,128),|v|v.as_ref().map_or(Value::Null,setup)),"trust":result(setup::manifest_trust(&d,128),|v|json!(match v{TrustClass::Candidate=>"candidate",TrustClass::Trusted=>"trusted"}))});
        let expected = native_projection_facts(case, &d);
        assert_eq!(actual, expected, "{}", case["name"]);
    }
    for case in f["keys"].as_array().unwrap() {
        let v = &case["input"];
        if rejects_private_text(v) {
            continue;
        }
        let c = CodeRef {
            repo: txt(&v["repo"]),
            commit: txt(&v["commit"]),
            path: (!v["path"].is_null()).then(|| txt(&v["path"])),
        };
        let s = SetupSpec {
            run: txt(&v["run"]),
            key_files: vec![],
            paths: vec![],
            egress: v["network"]["egress"]
                .as_array()
                .map_or_else(Vec::new, |v| v.iter().map(txt).collect()),
            open_network: v["network"].is_null(),
            deadline_seconds: 600.into(),
        };
        let env = v["env"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (t(k), txt(v)))
            .collect::<Vec<_>>();
        let files = v["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| (txt(&v[0]), txt(&v[1])))
            .collect::<Vec<_>>();
        let trust = if v["trust"] == "candidate" {
            TrustClass::Candidate
        } else {
            TrustClass::Trusted
        };
        let facts = result(
            setup::cache_key_document(&txt(&v["image"]), &s.run, &c, &env, &s, trust, &files)
                .and_then(|d| {
                    Ok((
                        setup::canonical_key_bytes(&d)?,
                        setup::cache_key(&txt(&v["image"]), &s.run, &c, &env, &s, trust, &files)?,
                    ))
                }),
            |(bytes, key)| json!({"canonical":String::from_utf8(bytes).unwrap(),"key":text(&key)}),
        );
        assert_eq!(facts, case["facts"], "{}", case["name"]);
    }
}
#[test]
fn source_filesystem_outcomes_and_partial_effects() {
    for case in fixture()["filesystem"].as_array().unwrap() {
        if rejects_private_text(&case["files"]) | rejects_private_text(&case["path"]) {
            continue;
        }
        let root = Root::new();
        layout(&root.0);
        let target = root.0.join("target");
        fs::create_dir(&target).unwrap();
        let code = root.0.join("code/steps");
        let files = case["files"]
            .as_array()
            .map_or_else(Vec::new, |v| v.iter().map(txt).collect::<Vec<_>>());
        let actual = match case["method"].as_str().unwrap() {
            "resolve" => result(
                setup::resolve_path(
                    &code,
                    (!case["path"].is_null())
                        .then(|| txt(&case["path"]))
                        .as_ref(),
                ),
                |p| json!({"path":text(&path_text(p.strip_prefix(&root.0).unwrap()))}),
            ),
            "hash" => result(
                setup::key_file_digests(&code, &files),
                |v| json!({"digests":v.iter().map(|(p,h)|json!([text(p),text(h)])).collect::<Vec<_>>()}),
            ),
            "stage" => result(
                setup::stage_key_files(&code, &files, &target),
                |()| json!({"ok":true}),
            ),
            "missing" => result(
                setup::missing_paths(&code, &files),
                |v| json!({"missing":strings(&v)}),
            ),
            _ => unreachable!(),
        };
        assert_eq!(actual, case["facts"], "{}", case["name"]);
        assert_eq!(
            tree_snapshot(&target),
            native_snapshot(&case["target"]),
            "{} partial target",
            case["name"]
        );
    }
}
struct Log;
impl CacheLog for Log {
    fn event(&self, _event: CacheEvent) -> Result<(), CacheError> {
        Ok(())
    }
}
fn step() -> StepSpec {
    StepSpec {
        job_id: t("job"),
        label: t("producer"),
        image: t(&format!("registry/step:tag@sha256:{}", "b".repeat(64))),
        command: vec![t("run")],
        args: vec![t("arg")],
        env: vec![(t("Z"), t("last")), (t("A"), t("first"))],
        resources: Resources {
            memory_bytes: Some(123.into()),
            ..Resources::default()
        },
        egress: vec![],
        mounts: vec![],
        workdir: None,
        open_network: false,
    }
}
fn alias(root: &Path, p: &Path, paths: &[(PathBuf, String)]) -> String {
    for (prefix, name) in paths.iter().rev() {
        if p == prefix {
            return name.clone();
        }
        if let Ok(tail) = p.strip_prefix(prefix) {
            return format!("{name}/{}", tail.to_str().unwrap());
        }
    }
    format!("$root/{}", p.strip_prefix(root).unwrap().to_str().unwrap())
}
fn step_value(root: &Path, s: &StepSpec, paths: &[(PathBuf, String)]) -> Value {
    json!({"job_id":text(&s.job_id),"label":text(&s.label),"image":text(&s.image),"command":strings(&s.command),"args":strings(&s.args),"env":s.env.iter().map(|(k,v)|(k.as_utf8().unwrap(),text(v))).collect::<serde_json::Map<_,_>>(),"resources":{"cpu":s.resources.cpu.as_ref().map(ToString::to_string),"memory_bytes":s.resources.memory_bytes.as_ref().map(number),"gpus":number(&s.resources.gpus)},"egress":strings(&s.egress),"open_network":s.open_network,"mounts":s.mounts.iter().map(|m|json!({"source":alias(root,m.source(),paths),"target":text(&m.target().text()),"read_only":m.read_only()})).collect::<Vec<_>>(),"workdir":s.workdir.as_ref().map(|p|text(&p.text()))})
}
struct Seams {
    root: PathBuf,
    cache: Arc<CacheRoot>,
    mode: String,
    trace: Mutex<Vec<Value>>,
    paths: Mutex<Vec<(PathBuf, String)>>,
    started: tokio::sync::Notify,
}
impl CodeSource for Seams {
    fn tree<'a>(&'a self, code: &'a CodeRef, _cancel: Cancellation) -> SetupFuture<'a, PathBuf> {
        Box::pin(async move {
            self.trace.lock().unwrap().push(
                json!({"operation":"tree","repo":text(&code.repo),"commit":text(&code.commit)}),
            );
            if self.mode == "source-error" {
                return Err(SetupError::Callback);
            }
            let path = self.root.join("code");
            self.cache.hold(&path)?;
            Ok(path)
        })
    }
}
impl RunSetup for Seams {
    fn run(
        &self,
        s: StepSpec,
        directory: PathBuf,
        seconds: BigInt,
        mut cancel: Cancellation,
    ) -> SetupFuture<'_, Outcome> {
        Box::pin(async move {
            let mut paths = self.paths.lock().unwrap().clone();
            paths.push((s.mounts[0].source().to_owned(), "$keyed".into()));
            paths.push((s.mounts[1].source().to_owned(), "$staging".into()));
            self.trace.lock().unwrap().push(json!({"operation":"run","step":step_value(&self.root,&s,&paths),"seconds":number(&seconds),"directory_empty":fs::read_dir(directory).unwrap().next().is_none(),"keyed":tree_snapshot(s.mounts[0].source())}));
            if matches!(
                self.mode.as_str(),
                "callback-error" | "release-error" | "cleanup-error"
            ) {
                return Err(SetupError::Callback);
            }
            if self.mode == "cancel" {
                self.started.notify_one();
                cancel.cancelled().await;
                self.trace
                    .lock()
                    .unwrap()
                    .push(json!({"operation":"callback-settled"}));
                return Err(SetupError::Cancelled);
            }
            let out = s.mounts[1].source();
            if self.mode != "missing" {
                fs::create_dir(out.join("site")).unwrap();
                fs::write(out.join("site/dep"), b"installed").unwrap();
            }
            if self.mode == "unsafe" {
                symlink(self.root.join("code"), out.join("escape")).unwrap();
            }
            let mut o = Outcome::new(Some(i32::from(self.mode == "failure").into()));
            o.timed_out = self.mode == "timeout";
            o.cancelled = self.mode == "outcome-cancel";
            o.oom_killed = self.mode == "oom";
            o.output_error = (self.mode == "output-error").then(|| t("output refused"));
            Ok(o)
        })
    }
}
fn manifest() -> Value {
    json!({"spec":{"role":"producer","code":{"repo":"Owner/Repo","commit":"a".repeat(40),"path":"steps"},"setup":{"run":"install dependencies","cache":{"key_files":["requirements.txt"],"paths":["site"]}}}})
}
#[tokio::test]
async fn source_provisioning_transforms_holds_and_cancellation() {
    for case in fixture()["provision"].as_array().unwrap() {
        let mode = case["name"].as_str().unwrap();
        let root = Root::new();
        layout(&root.0);
        let cache = Arc::new(
            CacheRoot::new(
                &root.0.join("cache"),
                BigInt::from(20_u64 << 30),
                Arc::new(Log),
            )
            .unwrap(),
        );
        cache.open().unwrap();
        let seams = Arc::new(Seams {
            root: root.0.clone(),
            cache: cache.clone(),
            mode: mode.into(),
            trace: Mutex::new(vec![]),
            paths: Mutex::new(vec![]),
            started: tokio::sync::Notify::new(),
        });
        let mut m = manifest();
        match mode {
            "no-code" => m["spec"]["code"] = Value::Null,
            "no-setup" => m["spec"]["setup"] = Value::Null,
            "bad-code" => m["spec"]["code"]["path"] = json!("absent"),
            "bad-key" => m["spec"]["setup"]["cache"]["key_files"] = json!(["absent"]),
            "setup-dir-exists" => fs::create_dir(root.0.join("job.setup")).unwrap(),
            _ => {}
        }
        let m = Arc::new(document(&m));
        let mut results = Vec::new();
        for i in 0..if mode == "replay" { 2 } else { 1 } {
            let task = ProvisionTask::start(setup::ProvisionRequest {
                manifest: m.clone(),
                step: step(),
                cache: cache.clone(),
                source: seams.clone(),
                directory: root.0.join(if i == 0 { "job.setup" } else { "job2.setup" }),
                run: seams.clone(),
                rendering_budget: 128,
                executor: Arc::new(FaultExecutor(mode.to_owned())),
            });
            if mode == "cancel" {
                seams.started.notified().await;
                task.cancel();
            }
            results.push(result(task.settle().await,|p|{let paths=p.held.iter().filter(|p|*p!=&root.0.join("code")).map(|p|(p.clone(),"$entry".into())).collect::<Vec<_>>();json!({"step":step_value(&root.0,&p.step,&paths),"held":p.held.iter().map(|p|if *p==root.0.join("code"){"$code"}else{"$entry"}).collect::<Vec<_>>(),"ready":p.ready(),"setup":p.setup.as_ref().map(|s|json!({"key":text(&s.key),"outcome":{"exit_code":s.outcome.exit_code.as_ref().map(number),"timed_out":s.outcome.timed_out,"cancelled":s.outcome.cancelled,"oom_killed":s.outcome.oom_killed,"output_error":s.outcome.output_error.as_ref().map(text)},"published":s.published,"missing":strings(&s.missing),"unsafe":s.unsafe_tree.is_some()}))})}));
        }
        let mut holds=cache.holds().unwrap().into_iter().map(|(p,n)|json!({"path":if p==root.0.join("code"){"$code"}else{"$entry"},"count":number(&n)})).collect::<Vec<_>>();
        holds.sort_by_key(|v| v["path"].as_str().unwrap().to_owned());
        let facts = json!({"results":results,"trace":*seams.trace.lock().unwrap(),"holds":holds,"entries":cache.entries().unwrap().iter().map(|e|json!({"key":e.path.file_name().unwrap().to_str().unwrap(),"size":number(&e.size)})).collect::<Vec<_>>(),"temporary_count":fs::read_dir(root.0.join("cache/tmp")).unwrap().count()});
        assert_eq!(facts, case["facts"], "provision {mode}");
        cache.close().unwrap();
    }
}
struct ConcurrentSeams {
    root: PathBuf,
    cache: Arc<CacheRoot>,
    barrier: Option<tokio::sync::Barrier>,
    calls: AtomicU64,
}
impl CodeSource for ConcurrentSeams {
    fn tree<'a>(&'a self, _code: &'a CodeRef, _cancel: Cancellation) -> SetupFuture<'a, PathBuf> {
        Box::pin(async move {
            let code = self.root.join("code");
            self.cache.hold(&code)?;
            Ok(code)
        })
    }
}
impl RunSetup for ConcurrentSeams {
    fn run(
        &self,
        s: StepSpec,
        _directory: PathBuf,
        _seconds: BigInt,
        _cancel: Cancellation,
    ) -> SetupFuture<'_, Outcome> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if let Some(barrier) = &self.barrier {
                barrier.wait().await;
            }
            let out = s.mounts[1].source();
            fs::create_dir(out.join("site")).unwrap();
            fs::write(out.join("site/dep"), b"installed").unwrap();
            Ok(Outcome::new(Some(0.into())))
        })
    }
}
#[tokio::test]
async fn actual_concurrent_cold_and_warm_cache_reuse() {
    for case in fixture()["concurrency"].as_array().unwrap() {
        let warm = case["warm"].as_bool().unwrap();
        let root = Root::new();
        layout(&root.0);
        let cache = Arc::new(
            CacheRoot::new(
                &root.0.join("cache"),
                BigInt::from(20_u64 << 30),
                Arc::new(Log),
            )
            .unwrap(),
        );
        cache.open().unwrap();
        let seams = Arc::new(ConcurrentSeams {
            root: root.0.clone(),
            cache: cache.clone(),
            barrier: (!warm).then(|| tokio::sync::Barrier::new(2)),
            calls: AtomicU64::new(0),
        });
        let manifest = Arc::new(document(&manifest()));
        if warm {
            let initial = start(
                manifest.clone(),
                step(),
                cache.clone(),
                seams.clone(),
                root.0.join("initial"),
                seams.clone(),
                128,
            )
            .settle()
            .await
            .unwrap();
            cache.release(&initial.held).unwrap();
            seams.calls.store(0, Ordering::Relaxed);
        }
        let tasks = (0..2)
            .map(|i| {
                let mut step = step();
                step.label = t(&i.to_string());
                start(
                    manifest.clone(),
                    step,
                    cache.clone(),
                    seams.clone(),
                    root.0.join(i.to_string()),
                    seams.clone(),
                    128,
                )
            })
            .collect::<Vec<_>>();
        let mut results = Vec::new();
        for task in tasks {
            results.push(task.settle().await.unwrap());
        }
        let mut counts = cache
            .holds()
            .unwrap()
            .into_iter()
            .map(|(_, n)| number(&n))
            .collect::<Vec<_>>();
        counts.sort_by_key(|v| v.as_i64().unwrap());
        let actual = json!({"callback_count":seams.calls.load(Ordering::Relaxed),"ready":results.iter().map(setup::Provisioned::ready).collect::<Vec<_>>(),"published":results.iter().map(|r|r.setup.as_ref().map(|s|s.published)).collect::<Vec<_>>(),"entry_count":cache.entries().unwrap().len(),"hold_counts":counts,"temporary_count":fs::read_dir(root.0.join("cache/tmp")).unwrap().count()});
        assert_eq!(actual, case["facts"]);
        cache.close().unwrap();
    }
}
fn start(
    manifest: Arc<Document>,
    step: StepSpec,
    cache: Arc<CacheRoot>,
    source: Arc<dyn CodeSource>,
    directory: PathBuf,
    run: Arc<dyn RunSetup>,
    budget: usize,
) -> ProvisionTask {
    ProvisionTask::start(setup::ProvisionRequest {
        manifest,
        step,
        cache,
        source,
        directory,
        run,
        rendering_budget: budget,
        executor: Arc::new(setup::SpawnBlocking),
    })
}
struct FaultExecutor(String);
impl FsExecutor for FaultExecutor {
    fn execute(&self, operation: FsOperation, cancel: Cancellation) -> SetupFuture<'_, FsValue> {
        if (self.0 == "release-error" && matches!(&operation, FsOperation::Release { .. }))
            || (matches!(self.0.as_str(), "cleanup-error" | "published-cleanup-error")
                && matches!(&operation,FsOperation::Discard{path,..} if path.file_name().unwrap().to_str().unwrap().starts_with("setup-code-")))
            || (self.0 == "second-staging-error"
                && matches!(&operation,FsOperation::Staging{prefix,..}if prefix==Path::new("setup-code-")))
        {
            return Box::pin(async { Err(SetupError::Io { errno: Some(5) }) });
        }
        if matches!(
            &operation,
            FsOperation::Release { .. } | FsOperation::Discard { .. }
        ) {
            assert!(!cancel.requested());
            let mut signal = cancel.clone();
            let mut future = Box::pin(signal.cancelled());
            assert!(
                future
                    .as_mut()
                    .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                    .is_pending()
            );
        }
        Box::pin(async move { setup::SpawnBlocking.execute(operation, cancel).await })
    }
    fn detach(&self, operations: Vec<FsOperation>) {
        setup::SpawnBlocking.detach(operations);
    }
}
struct DigestWitness {
    entered: tokio::sync::Notify,
}
struct OwnershipWitness {
    operation: &'static str,
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}
impl FsExecutor for OwnershipWitness {
    fn execute(&self, operation: FsOperation, cancel: Cancellation) -> SetupFuture<'_, FsValue> {
        Box::pin(async move {
            let pause = match &operation {
                FsOperation::Staging { prefix, .. } => {
                    self.operation == "staging" && prefix == Path::new("setup-")
                }
                FsOperation::Acquire { .. } => self.operation == "acquire",
                FsOperation::Publish { .. } => self.operation == "publish",
                _ => false,
            };
            let value = setup::SpawnBlocking.execute(operation, cancel).await?;
            if pause {
                self.entered.notify_one();
                self.resume.notified().await;
            }
            Ok(value)
        })
    }
    fn detach(&self, operations: Vec<FsOperation>) {
        setup::SpawnBlocking.detach(operations);
    }
}
#[tokio::test]
async fn completed_resource_operations_register_ownership_before_cancellation() {
    // Actual Python early cancellation leaves these effects behind; this is
    // decision evidence, deliberately distinct from the settlement adapter.
    for observation in fixture()["source_only_ownership_probes"]
        .as_array()
        .unwrap()
    {
        let staging = observation["operation"] == "staging";
        assert_eq!(
            observation["facts"],
            json!({
                "classification": "Cancelled", "operation_still_running": true,
                "holds_after_cancel": usize::from(!staging),
                "temporary_count": usize::from(staging),
                "holds_after_settlement": usize::from(!staging),
                "temporary_count_after_settlement": usize::from(staging),
            })
        );
    }
    for operation in ["staging", "acquire", "publish"] {
        let root = Root::new();
        layout(&root.0);
        let cache = Arc::new(
            CacheRoot::new(&root.0.join("cache"), (20_u64 << 30).into(), Arc::new(Log)).unwrap(),
        );
        cache.open().unwrap();
        let seams = Arc::new(ConcurrentSeams {
            root: root.0.clone(),
            cache: cache.clone(),
            barrier: None,
            calls: AtomicU64::new(0),
        });
        let manifest = Arc::new(document(&manifest()));
        if operation == "acquire" {
            let initial = start(
                manifest.clone(),
                step(),
                cache.clone(),
                seams.clone(),
                root.0.join("initial"),
                seams.clone(),
                128,
            )
            .settle()
            .await
            .unwrap();
            cache.release(&initial.held).unwrap();
        }
        let witness = Arc::new(OwnershipWitness {
            operation,
            entered: tokio::sync::Notify::new(),
            resume: tokio::sync::Notify::new(),
        });
        let task = ProvisionTask::start(setup::ProvisionRequest {
            manifest,
            step: step(),
            cache: cache.clone(),
            source: seams.clone(),
            directory: root.0.join("job"),
            run: seams,
            rendering_budget: 128,
            executor: witness.clone(),
        });
        witness.entered.notified().await;
        let holds = cache.holds().unwrap();
        assert_eq!(holds.len(), if operation == "staging" { 1 } else { 2 });
        assert_eq!(
            fs::read_dir(root.0.join("cache/tmp")).unwrap().count(),
            usize::from(operation != "acquire")
        );
        task.cancel();
        witness.resume.notify_one();
        assert!(matches!(task.settle().await, Err(SetupError::Cancelled)));
        assert!(cache.holds().unwrap().is_empty(), "{operation}");
        assert_eq!(fs::read_dir(root.0.join("cache/tmp")).unwrap().count(), 0);
        cache.close().unwrap();
    }
}
impl FsExecutor for DigestWitness {
    fn execute(&self, operation: FsOperation, cancel: Cancellation) -> SetupFuture<'_, FsValue> {
        Box::pin(async move {
            if matches!(&operation, FsOperation::Digests { .. }) {
                self.entered.notify_one();
            }
            setup::SpawnBlocking.execute(operation, cancel).await
        })
    }
    fn detach(&self, operations: Vec<FsOperation>) {
        setup::SpawnBlocking.detach(operations);
    }
}
#[tokio::test]
async fn fifo_witness_records_pending_cancellation_difference() {
    let source = &fixture()["source_only_continuation_probe"];
    assert_eq!(
        *source,
        json!({"classification":"Cancelled","operation_still_running":true,"holds_after_cancel":0,"temporary_count":0,"operation_eventually_settled":true})
    );
    let root = Root::new();
    layout(&root.0);
    let fifo = root.0.join("code/steps/block");
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_bits_truncate(0o600),
        0,
    )
    .unwrap();
    let cache = Arc::new(
        CacheRoot::new(
            &root.0.join("cache"),
            BigInt::from(20_u64 << 30),
            Arc::new(Log),
        )
        .unwrap(),
    );
    cache.open().unwrap();
    let seams = Arc::new(Seams {
        root: root.0.clone(),
        cache: cache.clone(),
        mode: "success".into(),
        trace: Mutex::new(vec![]),
        paths: Mutex::new(vec![]),
        started: tokio::sync::Notify::new(),
    });
    let executor = Arc::new(DigestWitness {
        entered: tokio::sync::Notify::new(),
    });
    let mut manifest = manifest();
    manifest["spec"]["setup"]["cache"]["key_files"] = json!(["block"]);
    let task = ProvisionTask::start(setup::ProvisionRequest {
        manifest: Arc::new(document(&manifest)),
        step: step(),
        cache: cache.clone(),
        source: seams.clone(),
        directory: root.0.join("job"),
        run: seams,
        rendering_budget: 128,
        executor: executor.clone(),
    });
    executor.entered.notified().await;
    task.cancel();
    let settled = tokio::spawn(task.settle());
    tokio::task::yield_now().await;
    assert!(
        !settled.is_finished(),
        "explicit settlement adapter must retain the blocked operation"
    );
    assert_eq!(cache.holds().unwrap()[0].1, BigInt::from(1));
    let writer = tokio::task::spawn_blocking(move || {
        rustix::fs::open(&fifo, rustix::fs::OFlags::WRONLY, rustix::fs::Mode::empty()).unwrap()
    });
    // The genuine FIFO reader's regular-file refusal wins after the writer connects.
    assert_eq!(settled.await.unwrap().unwrap_err(), SetupError::InvalidCode);
    drop(writer.await.unwrap());
    assert_eq!(cache.holds().unwrap().len(), 0);
    cache.close().unwrap();
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;

#[test]
fn setup_deadline_requires_a_signed_i64_json_integer() {
    for value in [i64::MIN, -1, 0, 1, i64::MAX] {
        let document = cannery_core::json::decode_str(&format!(r#"{{"spec":{{"setup":{{"activeDeadlineSeconds":{value},"run":"echo ready","cache":{{"key_files":[]}}}}}}}}"#), 64).unwrap();
        assert_eq!(
            setup::setup_spec(&document, 64)
                .unwrap()
                .unwrap()
                .deadline_seconds,
            BigInt::from(value)
        );
    }
    for raw in [
        "true",
        "false",
        "1.5",
        r#""1""#,
        r#""١٢""#,
        r#""1_000""#,
        "null",
        "[]",
        "{}",
        "9223372036854775808",
        "-9223372036854775809",
    ] {
        let document = cannery_core::json::decode_str(&format!(r#"{{"spec":{{"setup":{{"activeDeadlineSeconds":{raw},"run":"echo ready","cache":{{"key_files":[]}}}}}}}}"#), 64).unwrap();
        let expected = if raw == "9223372036854775808" || raw == "-9223372036854775809" {
            SetupError::Overflow
        } else {
            SetupError::Type
        };
        assert_eq!(
            setup::setup_spec(&document, 64).unwrap_err(),
            expected,
            "{raw}"
        );
    }
}

#[test]
fn setup_default_deadline_and_integer_error_order_are_preserved() {
    let document = cannery_core::json::decode_str(
        r#"{"spec":{"setup":{"run":"echo 🦦","cache":{"key_files":[]}}}}"#,
        64,
    )
    .unwrap();
    let spec = setup::setup_spec(&document, 64).unwrap().unwrap();
    assert_eq!(spec.deadline_seconds, 600.into());
    assert_eq!(spec.run, "echo 🦦");
    let malformed = cannery_core::json::decode_str(
        r#"{"spec":{"setup":{"activeDeadlineSeconds":"private-marker"}}}"#,
        64,
    )
    .unwrap();
    let error = setup::setup_spec(&malformed, 64).unwrap_err();
    assert_eq!(error, SetupError::Type);
    assert!(!format!("{error:?} {error}").contains("private-marker"));
}
