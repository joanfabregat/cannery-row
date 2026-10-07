//! Code/setup preparation, with required code-source and execution boundaries.
use crate::{
    cache::{CacheError, CacheRoot},
    files::TreeError,
    launcher::{self, Mount, Outcome, PosixPath, StepSpec},
    paths::{self, PathError},
};
use cannery_core::{
    json::{Document, DocumentBuilder, Node, NodeId},
    text::{self, RenderError},
};
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    future::Future,
    io::{Read, Write},
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

pub const KEY_VERSION: u32 = 2;
pub const DEFAULT_SETUP_DEADLINE_SECONDS: u32 = 600;
/// Static classifications only: paths, commands and environment values are never diagnostic fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SetupError {
    #[error("code path or key file was refused")]
    InvalidCode,
    #[error("code repository is not allowed")]
    CodeNotAllowed,
    #[error("code acquisition failed")]
    RunnerError,
    #[error("setup manifest is missing a field")]
    Missing,
    #[error("setup manifest has an incompatible type")]
    Type,
    #[error("setup manifest does not support mapping access")]
    Attribute,
    #[error("setup integer conversion failed")]
    Value,
    #[error("code archive data has a missing index")]
    Index,
    #[error("setup integer conversion overflowed")]
    Overflow,
    #[error("setup text encoding failed")]
    Encoding,
    #[error("setup text rendering exceeded the Python recursion limit")]
    Recursion,
    #[error("setup filesystem operation failed")]
    Io { errno: Option<i32> },
    #[error("setup image or mount contract failed")]
    Contract(#[from] launcher::ContractError),
    #[error("setup cache operation failed")]
    Cache(#[from] CacheError),
    #[error("setup callback failed")]
    Callback,
    #[error("setup preparation was cancelled")]
    Cancelled,
    #[error("setup task could not settle")]
    Task,
}
fn io(error: &std::io::Error) -> SetupError {
    SetupError::Io {
        errno: error.raw_os_error(),
    }
}
fn render(error: RenderError) -> SetupError {
    match error {
        RenderError::Encoding => SetupError::Encoding,
        RenderError::IntegerLimit => SetupError::Value,
        RenderError::Recursion => SetupError::Recursion,
        RenderError::InvalidNode => SetupError::Type,
    }
}
fn path_error(error: PathError) -> SetupError {
    match error {
        PathError::Encoding => SetupError::Encoding,
        PathError::InvalidNul => SetupError::Value,
        PathError::NotDirectory => SetupError::Io { errno: Some(20) },
        PathError::Io { errno } => SetupError::Io { errno },
    }
}
fn nul(path: &Path) -> Result<(), SetupError> {
    use std::os::unix::ffi::OsStrExt;
    if path.as_os_str().as_bytes().contains(&0) {
        Err(SetupError::Value)
    } else {
        Ok(())
    }
}

#[derive(Clone)]
pub struct CodeRef {
    pub repo: String,
    pub commit: String,
    pub path: Option<String>,
}
#[derive(Clone)]
pub struct SetupSpec {
    pub run: String,
    pub key_files: Vec<String>,
    pub paths: Vec<String>,
    pub egress: Vec<String>,
    pub open_network: bool,
    pub deadline_seconds: BigInt,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustClass {
    Candidate,
    Trusted,
}
impl TrustClass {
    fn name(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Trusted => "trusted",
        }
    }
}
impl std::fmt::Debug for CodeRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CodeRef([redacted])")
    }
}
impl std::fmt::Debug for SetupSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SetupSpec([redacted])")
    }
}
fn field(d: &Document, id: NodeId, key: &str) -> Result<NodeId, SetupError> {
    if !matches!(d.node(id), Some(Node::Object(_))) {
        return Err(SetupError::Type);
    }
    d.field(id, key).ok_or(SetupError::Missing)
}
fn spec(d: &Document) -> Result<NodeId, SetupError> {
    let id = field(d, d.root(), "spec")?;
    if !matches!(d.node(id), Some(Node::Object(_))) {
        return Err(SetupError::Attribute);
    }
    Ok(id)
}
fn text(d: &Document, id: NodeId, budget: usize) -> Result<String, SetupError> {
    text::str_value(d, id, budget).map_err(render)
}
fn strings(d: &Document, id: NodeId, budget: usize) -> Result<Vec<String>, SetupError> {
    match d.node(id) {
        Some(Node::Array(items)) => items.iter().map(|&id| text(d, id, budget)).collect(),
        Some(Node::String(value)) => value
            .codepoints()
            .iter()
            .map(|&cp| cannery_core::text::from_codepoints(vec![cp]).ok_or(SetupError::Encoding))
            .collect(),
        Some(Node::Object(items)) => Ok(items.iter().map(|(k, _)| k.clone()).collect()),
        _ => Err(SetupError::Type),
    }
}
/// Project source `str(...)` coercions; the caller supplies its calibrated rendering budget.
/// # Errors
/// Missing fields, incompatible access, and unhandled rendering failures remain distinct.
pub fn code_ref(d: &Document, budget: usize) -> Result<Option<CodeRef>, SetupError> {
    let s = spec(d)?;
    let Some(code) = d.field(s, "code") else {
        return Ok(None);
    };
    if matches!(d.node(code), Some(Node::Null)) {
        return Ok(None);
    }
    if !matches!(d.node(code), Some(Node::Object(_))) {
        return Err(SetupError::Attribute);
    }
    let path = d
        .field(code, "path")
        .filter(|&id| !matches!(d.node(id), Some(Node::Null)));
    let repo = text(d, field(d, code, "repo")?, budget)?;
    let commit = text(d, field(d, code, "commit")?, budget)?;
    let path = path.map(|id| text(d, id, budget)).transpose()?;
    Ok(Some(CodeRef { repo, commit, path }))
}
fn integer(d: &Document, id: NodeId) -> Result<BigInt, SetupError> {
    match d.node(id) {
        Some(Node::Integer(value)) => value.to_i64().map(BigInt::from).ok_or(SetupError::Overflow),
        _ => Err(SetupError::Type),
    }
}
/// A bounded JSON deadline precedes cache/run projection; absent setup returns None.
/// # Errors
/// Preserves missing/type/value/overflow/rendering categories.
pub fn setup_spec(d: &Document, budget: usize) -> Result<Option<SetupSpec>, SetupError> {
    let s = spec(d)?;
    let Some(setup) = d.field(s, "setup") else {
        return Ok(None);
    };
    if matches!(d.node(setup), Some(Node::Null)) {
        return Ok(None);
    }
    if !matches!(d.node(setup), Some(Node::Object(_))) {
        return Err(SetupError::Attribute);
    }
    let deadline_seconds = d
        .field(setup, "activeDeadlineSeconds")
        .map(|id| integer(d, id))
        .transpose()?
        .unwrap_or_else(|| BigInt::from(DEFAULT_SETUP_DEADLINE_SECONDS));
    let cache = field(d, setup, "cache")?;
    let run = text(d, field(d, setup, "run")?, budget)?;
    let key_files = strings(d, field(d, cache, "key_files")?, budget)?;
    let paths = d
        .field(cache, "paths")
        .map(|id| strings(d, id, budget))
        .transpose()?
        .unwrap_or_default();
    let network = d.field(setup, "network");
    let open_network = network.is_none_or(|id| matches!(d.node(id), Some(Node::Null)));
    let egress = if let Some(id) = network.filter(|&id| matches!(d.node(id), Some(Node::Object(_))))
    {
        strings(d, field(d, id, "egress")?, budget)?
    } else {
        Vec::new()
    };
    Ok(Some(SetupSpec {
        run,
        key_files,
        paths,
        egress,
        open_network,
        deadline_seconds,
    }))
}
/// # Errors
/// Missing spec or failed source str rendering.
pub fn manifest_trust(d: &Document, budget: usize) -> Result<TrustClass, SetupError> {
    let s = spec(d)?;
    let role = d
        .field(s, "role")
        .map(|id| text(d, id, budget))
        .transpose()?
        .unwrap_or_else(String::new);
    Ok(
        if role.equals_utf8("producer") || role.equals_utf8("experiment") {
            TrustClass::Candidate
        } else {
            TrustClass::Trusted
        },
    )
}

/// Never follows a component link; None returns the original tree without stat.
/// # Errors
/// Only ENOENT becomes `InvalidCode`; ENOTDIR, permissions, NUL and encoding stay distinct.
pub fn resolve_path(tree: &Path, relative: Option<&String>) -> Result<PathBuf, SetupError> {
    let mut current = tree.to_owned();
    let Some(relative) = relative else {
        return Ok(current);
    };
    for part in relative.codepoints().split(|&cp| cp == 47) {
        if part.is_empty() || part == [46] || part == [46, 46] {
            return Err(SetupError::InvalidCode);
        }
        let value =
            cannery_core::text::from_codepoints(part.to_vec()).ok_or(SetupError::Encoding)?;
        current.push(paths::from_text(&value).map_err(path_error)?);
        nul(&current)?;
        let metadata = fs::symlink_metadata(&current).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SetupError::InvalidCode
            } else {
                io(&e)
            }
        })?;
        if metadata.file_type().is_symlink() {
            return Err(SetupError::InvalidCode);
        }
    }
    Ok(current)
}
/// # Errors
/// Resolution is ordered; NOFOLLOW/fstat protects the final key-file descriptor.
pub fn key_file_digests(
    code: &Path,
    files: &[String],
) -> Result<Vec<(String, String)>, SetupError> {
    let mut output = Vec::new();
    for relative in files {
        let path = resolve_path(code, Some(relative))?;
        let mut file = open_key_file(&path)?;
        let metadata = file.metadata().map_err(|e| io(&e))?;
        // CPython fdopen rejects a directory before the source's fstat regular check.
        if metadata.is_dir() {
            return Err(SetupError::Io { errno: Some(21) });
        }
        if !metadata.is_file() {
            return Err(SetupError::InvalidCode);
        }
        let mut hash = Sha256::new();
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            let count = file.read(&mut buffer).map_err(|e| io(&e))?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        output.push((
            relative.clone(),
            String::from(&format!("{:x}", hash.finalize())),
        ));
    }
    Ok(output)
}
fn open_key_file(path: &Path) -> Result<File, SetupError> {
    rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map(File::from)
    .map_err(|e| SetupError::Io {
        errno: Some(e.raw_os_error()),
    })
}
/// Copy only declared files; no stronger race-confinement guarantee than source shutil.
/// # Errors
/// Effects of earlier files remain when a later copy fails.
pub fn stage_key_files(code: &Path, files: &[String], target: &Path) -> Result<(), SetupError> {
    for relative in files {
        let source = resolve_path(code, Some(relative))?;
        let destination = target.join(paths::from_text(relative).map_err(path_error)?);
        nul(&destination)?;
        fs::create_dir_all(destination.parent().unwrap_or(Path::new("."))).map_err(|e| io(&e))?;
        let source_stat = fs::metadata(&source).map_err(|e| io(&e))?;
        if let Ok(dest) = fs::metadata(&destination)
            && source_stat.dev() == dest.dev()
            && source_stat.ino() == dest.ino()
        {
            return Err(SetupError::Io { errno: None });
        }
        if source_stat.file_type().is_fifo()
            || fs::metadata(&destination).is_ok_and(|m| m.file_type().is_fifo())
        {
            return Err(SetupError::Io { errno: None });
        }
        let metadata = fs::symlink_metadata(&source).map_err(|e| io(&e))?;
        if metadata.file_type().is_symlink() {
            symlink(fs::read_link(&source).map_err(|e| io(&e))?, &destination)
                .map_err(|e| io(&e))?;
        } else {
            let mut input = File::open(&source).map_err(|e| io(&e))?;
            if input.metadata().map_err(|e| io(&e))?.is_dir() {
                return Err(SetupError::Io { errno: Some(21) });
            }
            let mut output = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&destination)
                .map_err(|e| io(&e))?;
            std::io::copy(&mut input, &mut output).map_err(|e| io(&e))?;
            output.flush().map_err(|e| io(&e))?;
        }
        let executable = fs::symlink_metadata(&source).map_err(|e| io(&e))?.mode() & 0o111 != 0;
        fs::set_permissions(
            &destination,
            fs::Permissions::from_mode(if executable { 0o555 } else { 0o444 }),
        )
        .map_err(|e| io(&e))?;
    }
    Ok(())
}
/// # Errors
/// Catch only code refusals; unrelated OS/value failures escape.
pub fn missing_paths(cache: &Path, paths: &[String]) -> Result<Vec<String>, SetupError> {
    let mut missing = Vec::new();
    for path in paths {
        match resolve_path(cache, Some(path)) {
            Ok(_) => {}
            Err(SetupError::InvalidCode) => missing.push(path.clone()),
            Err(error) => return Err(error),
        }
    }
    Ok(missing)
}

fn push(builder: &mut DocumentBuilder, node: Node) -> Result<NodeId, SetupError> {
    builder.push(node).map_err(|_| SetupError::Type)
}
fn string(builder: &mut DocumentBuilder, value: String) -> Result<NodeId, SetupError> {
    push(builder, Node::String(value))
}
fn array_strings(builder: &mut DocumentBuilder, values: &[String]) -> Result<NodeId, SetupError> {
    let items = values
        .iter()
        .map(|v| string(builder, v.clone()))
        .collect::<Result<_, _>>()?;
    push(builder, Node::Array(items))
}
/// Exact typed v2 document; the commit is deliberately absent.
/// # Errors
/// Image parsing precedes canonical text encoding.
pub fn cache_key_document(
    image: &str,
    run: &str,
    code: &CodeRef,
    env: &[(String, String)],
    setup: &SetupSpec,
    trust: TrustClass,
    files: &[(String, String)],
) -> Result<Document, SetupError> {
    let (_, digest) = launcher::pinned_image(image)?;
    let mut b = DocumentBuilder::new();
    let mut fields = Vec::new();
    // Source Mapping inputs are unique. Collapse duplicate native pairs last-wins.
    let env = env
        .iter()
        .cloned()
        .collect::<std::collections::HashMap<_, _>>();
    let mut env = env.into_iter().collect::<Vec<_>>();
    env.sort_by_key(|a| a.0.codepoints());
    let mut pairs = Vec::new();
    for (name, value) in env {
        let name = string(&mut b, name)?;
        let value = string(&mut b, value)?;
        pairs.push(push(&mut b, Node::Array(vec![name, value]))?);
    }
    fields.push((String::from("env"), push(&mut b, Node::Array(pairs))?));
    fields.push((String::from("image"), string(&mut b, digest)?));
    let mut pairs = Vec::new();
    for (path, hash) in files {
        let path = string(&mut b, path.clone())?;
        let hash = string(&mut b, hash.clone())?;
        pairs.push(push(&mut b, Node::Array(vec![path, hash]))?);
    }
    fields.push((String::from("key_files"), push(&mut b, Node::Array(pairs))?));
    let network = if setup.open_network {
        string(&mut b, String::from("open"))?
    } else {
        let mut egress = setup.egress.clone();
        egress.sort_by_key(cannery_core::text::TextExt::codepoints);
        array_strings(&mut b, &egress)?
    };
    fields.push((String::from("network"), network));
    let path = match &code.path {
        None => push(&mut b, Node::Null)?,
        Some(v) => string(&mut b, v.clone())?,
    };
    fields.push((String::from("path"), path));
    fields.push((String::from("repo"), string(&mut b, code.repo.lowercase())?));
    fields.push((String::from("run"), string(&mut b, run.to_owned())?));
    fields.push((
        String::from("trust"),
        string(&mut b, String::from(trust.name()))?,
    ));
    fields.push((
        String::from("version"),
        push(&mut b, Node::Integer(BigInt::from(KEY_VERSION)))?,
    ));
    let root = push(&mut b, Node::Object(fields))?;
    b.finish(root).map_err(|_| SetupError::Type)
}
fn quoted(text: &String, output: &mut String) -> Result<(), SetupError> {
    output.push('"');
    for cp in text.codepoints() {
        match cp {
            34 => output.push_str("\\\""),
            92 => output.push_str("\\\\"),
            8 => output.push_str("\\b"),
            9 => output.push_str("\\t"),
            10 => output.push_str("\\n"),
            12 => output.push_str("\\f"),
            13 => output.push_str("\\r"),
            0..=31 => {
                use std::fmt::Write;
                write!(output, "\\u{cp:04x}").map_err(|_| SetupError::Encoding)?;
            }
            _ => output.push(char::from_u32(cp).ok_or(SetupError::Encoding)?),
        }
    }
    output.push('"');
    Ok(())
}
/// Compact sorted-key UTF-8 bytes for the fixed-depth typed cache-key document.
/// # Errors
/// Lone surrogate strings fail encoding; no replacement or ASCII escape workaround.
pub fn canonical_key_bytes(document: &Document) -> Result<Vec<u8>, SetupError> {
    enum Part<'a> {
        Node(NodeId),
        Text(&'a str),
        String(&'a String),
    }
    let mut stack = vec![Part::Node(document.root())];
    let mut output = String::new();
    while let Some(part) = stack.pop() {
        match part {
            Part::Text(v) => output.push_str(v),
            Part::String(v) => quoted(v, &mut output)?,
            Part::Node(id) => match document.node(id).ok_or(SetupError::Type)? {
                Node::Null => output.push_str("null"),
                Node::String(v) => quoted(v, &mut output)?,
                Node::Integer(v) => output.push_str(&v.to_string()),
                Node::Array(items) => {
                    output.push('[');
                    stack.push(Part::Text("]"));
                    for (i, &id) in items.iter().enumerate().rev() {
                        stack.push(Part::Node(id));
                        if i > 0 {
                            stack.push(Part::Text(","));
                        }
                    }
                }
                Node::Object(items) => {
                    output.push('{');
                    let mut items = items.iter().collect::<Vec<_>>();
                    items.sort_by_key(|a| a.0.codepoints());
                    stack.push(Part::Text("}"));
                    for (i, (key, id)) in items.into_iter().enumerate().rev() {
                        stack.push(Part::Node(*id));
                        stack.push(Part::Text(":"));
                        stack.push(Part::String(key));
                        if i > 0 {
                            stack.push(Part::Text(","));
                        }
                    }
                }
                _ => return Err(SetupError::Type),
            },
        }
    }
    Ok(output.into_bytes())
}
/// # Errors
/// See `cache_key_document` and `canonical_key_bytes`.
pub fn cache_key(
    image: &str,
    run: &str,
    code: &CodeRef,
    env: &[(String, String)],
    setup: &SetupSpec,
    trust: TrustClass,
    files: &[(String, String)],
) -> Result<String, SetupError> {
    let d = cache_key_document(image, run, code, env, setup, trust, files)?;
    Ok(String::from(&format!(
        "{:x}",
        Sha256::digest(canonical_key_bytes(&d)?)
    )))
}
fn mount(source: &Path, target: &str, read_only: bool) -> Result<Mount, SetupError> {
    Ok(Mount::new(
        source.to_owned(),
        PosixPath::new(&String::from(target)),
        read_only,
    )?)
}
/// # Errors
/// Source Mount construction requires absolute paths.
pub fn setup_step(
    step: &StepSpec,
    setup: &SetupSpec,
    code: &Path,
    cache: &Path,
) -> Result<StepSpec, SetupError> {
    let mut result = step.clone();
    let mut label = step.label.codepoints().clone();
    label.extend(".setup".chars().map(u32::from));
    result.label = cannery_core::text::from_codepoints(label).ok_or(SetupError::Encoding)?;
    result.command = vec![
        String::from("/bin/sh"),
        String::from("-c"),
        setup.run.clone(),
    ];
    result.args.clear();
    result.egress.clone_from(&setup.egress);
    result.open_network = setup.open_network;
    result.mounts = vec![
        mount(code, "/cr/code", true)?,
        mount(cache, "/cr/cache", false)?,
    ];
    result.workdir = Some(PosixPath::new(&String::from("/cr/code")));
    Ok(result)
}
/// # Errors
/// Source Mount construction requires absolute paths.
pub fn code_step(
    step: &StepSpec,
    code: &Path,
    cache: Option<&Path>,
) -> Result<StepSpec, SetupError> {
    let mut result = step.clone();
    result.mounts = vec![mount(code, "/cr/code", true)?];
    if let Some(cache) = cache {
        result.mounts.push(mount(cache, "/cr/cache", true)?);
    }
    result.workdir = Some(PosixPath::new(&String::from("/cr/code")));
    Ok(result)
}

#[derive(Clone, Debug)]
pub struct SetupRun {
    pub key: String,
    pub outcome: Outcome,
    pub published: bool,
    pub missing: Vec<String>,
    pub unsafe_tree: Option<TreeError>,
}
#[derive(Clone, Debug)]
pub struct Provisioned {
    pub step: StepSpec,
    pub held: Vec<PathBuf>,
    pub setup: Option<SetupRun>,
}
impl Provisioned {
    #[must_use]
    pub fn ready(&self) -> bool {
        self.setup.as_ref().is_none_or(|s| s.published)
    }
}
pub type SetupFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, SetupError>> + Send + 'a>>;
/// Callback implementations must settle their own work before returning cancellation.
pub trait CodeSource: Send + Sync {
    fn tree<'a>(&'a self, code: &'a CodeRef, cancel: Cancellation) -> SetupFuture<'a, PathBuf>;
}
pub trait RunSetup: Send + Sync {
    fn run(
        &self,
        step: StepSpec,
        directory: PathBuf,
        seconds: BigInt,
        cancel: Cancellation,
    ) -> SetupFuture<'_, Outcome>;
}
/// Fixed owned operations; no arbitrary filesystem script or implicit executor.
pub enum FsOperation {
    Resolve {
        tree: PathBuf,
        relative: Option<String>,
    },
    IsDir(PathBuf),
    Digests {
        code: PathBuf,
        files: Vec<String>,
    },
    Stage {
        code: PathBuf,
        files: Vec<String>,
        target: PathBuf,
    },
    Mkdir(PathBuf),
    Staging {
        cache: Arc<CacheRoot>,
        prefix: PathBuf,
    },
    Acquire {
        cache: Arc<CacheRoot>,
        path: PathBuf,
    },
    Publish {
        cache: Arc<CacheRoot>,
        staging: PathBuf,
        path: PathBuf,
    },
    Missing {
        cache: PathBuf,
        paths: Vec<String>,
    },
    Discard {
        cache: Arc<CacheRoot>,
        path: PathBuf,
    },
    Release {
        cache: Arc<CacheRoot>,
        paths: Vec<PathBuf>,
    },
}
pub enum FsValue {
    Path(PathBuf),
    Bool(bool),
    Digests(Vec<(String, String)>),
    Missing(Vec<String>),
    Unit,
}
impl FsValue {
    fn path(self) -> Result<PathBuf, SetupError> {
        if let Self::Path(v) = self {
            Ok(v)
        } else {
            Err(SetupError::Type)
        }
    }
    fn boolean(self) -> Result<bool, SetupError> {
        if let Self::Bool(v) = self {
            Ok(v)
        } else {
            Err(SetupError::Type)
        }
    }
    fn digests(self) -> Result<Vec<(String, String)>, SetupError> {
        if let Self::Digests(v) = self {
            Ok(v)
        } else {
            Err(SetupError::Type)
        }
    }
    fn missing(self) -> Result<Vec<String>, SetupError> {
        if let Self::Missing(v) = self {
            Ok(v)
        } else {
            Err(SetupError::Type)
        }
    }
    fn unit(self) -> Result<(), SetupError> {
        if let Self::Unit = self {
            Ok(())
        } else {
            Err(SetupError::Type)
        }
    }
}
impl FsOperation {
    fn execute(self) -> Result<FsValue, SetupError> {
        match self {
            Self::Resolve { tree, relative } => {
                resolve_path(&tree, relative.as_ref()).map(FsValue::Path)
            }
            Self::IsDir(path) => is_dir(&path).map(FsValue::Bool),
            Self::Digests { code, files } => key_file_digests(&code, &files).map(FsValue::Digests),
            Self::Stage {
                code,
                files,
                target,
            } => stage_key_files(&code, &files, &target).map(|()| FsValue::Unit),
            Self::Mkdir(path) => {
                nul(&path)?;
                fs::create_dir(path).map_err(|e| io(&e))?;
                Ok(FsValue::Unit)
            }
            Self::Staging { cache, prefix } => cache
                .staging(&prefix)
                .map(FsValue::Path)
                .map_err(Into::into),
            Self::Acquire { cache, path } => {
                cache.acquire(&path).map(FsValue::Bool).map_err(Into::into)
            }
            Self::Publish {
                cache,
                staging,
                path,
            } => cache
                .publish(&staging, &path, true)
                .map(FsValue::Bool)
                .map_err(Into::into),
            Self::Missing { cache, paths } => missing_paths(&cache, &paths).map(FsValue::Missing),
            Self::Discard { cache, path } => {
                cache.discard(&path);
                Ok(FsValue::Unit)
            }
            Self::Release { cache, paths } => cache
                .release(&paths)
                .map(|()| FsValue::Unit)
                .map_err(Into::into),
        }
    }
}
pub trait FsExecutor: Send + Sync {
    /// Implementations must settle the operation and return its completed value even
    /// when cancellation arrives: that value can carry staging/hold ownership.
    /// Preserve the operation's first failure; callers register ownership before
    /// acknowledging cancellation.
    fn execute(&self, operation: FsOperation, cancel: Cancellation) -> SetupFuture<'_, FsValue>;
    /// Emergency abandoned-future cleanup. It must not block an async worker.
    fn detach(&self, operations: Vec<FsOperation>);
}
/// Explicit owned blocking-pool adapter, never installed as an implicit default.
/// Its cancellation result waits for the operation; `CPython` `to_thread` cancellation
/// can instead return while the thread continues. Worker integration must settle this gap.
pub struct SpawnBlocking;
impl FsExecutor for SpawnBlocking {
    fn execute(&self, operation: FsOperation, _cancel: Cancellation) -> SetupFuture<'_, FsValue> {
        Box::pin(async move {
            let result = tokio::task::spawn_blocking(move || operation.execute())
                .await
                .map_err(|_| SetupError::Task)??;
            Ok(result)
        })
    }
    fn detach(&self, operations: Vec<FsOperation>) {
        let run = move || {
            for operation in operations {
                if operation.execute().is_err() {
                    break;
                }
            }
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn_blocking(run);
        } else {
            let _ = std::thread::Builder::new().spawn(run);
        }
    }
}
/// Per-operation signal, independent of lease/backend signal policy.
#[derive(Clone)]
pub struct Cancellation(tokio::sync::watch::Receiver<bool>);
impl Cancellation {
    /// Wrap the caller's signal without selecting an execution policy.
    /// A closed channel whose last value is false remains pending.
    #[must_use]
    pub fn from_receiver(receiver: tokio::sync::watch::Receiver<bool>) -> Self {
        Self(receiver)
    }
    fn never() -> Self {
        let (_, receiver) = tokio::sync::watch::channel(false);
        Self(receiver)
    }
    #[must_use]
    pub fn requested(&self) -> bool {
        *self.0.borrow()
    }
    pub async fn cancelled(&mut self) {
        while !self.requested() {
            if self.0.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
    fn check(&self) -> Result<(), SetupError> {
        if self.requested() {
            Err(SetupError::Cancelled)
        } else {
            Ok(())
        }
    }
}
struct Held<'a> {
    cache: &'a Arc<CacheRoot>,
    executor: &'a Arc<dyn FsExecutor>,
    paths: Vec<PathBuf>,
    armed: bool,
}
impl Drop for Held<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.executor.detach(vec![FsOperation::Release {
                cache: self.cache.clone(),
                paths: self.paths.clone(),
            }]);
        }
    }
}
impl Held<'_> {
    fn register(&mut self, path: PathBuf, cancel: &Cancellation) -> Result<(), SetupError> {
        self.paths.push(path);
        cancel.check()
    }
    async fn failure(&mut self, error: SetupError) -> SetupError {
        self.armed = false;
        match self
            .executor
            .execute(
                FsOperation::Release {
                    cache: self.cache.clone(),
                    paths: self.paths.clone(),
                },
                Cancellation::never(),
            )
            .await
            .and_then(FsValue::unit)
        {
            Ok(()) => error,
            Err(cleanup) => cleanup,
        }
    }
    fn transfer(&mut self) -> Vec<PathBuf> {
        self.armed = false;
        std::mem::take(&mut self.paths)
    }
}
struct ScratchGuard<'a> {
    cache: &'a Arc<CacheRoot>,
    executor: &'a Arc<dyn FsExecutor>,
    keyed: PathBuf,
    staging: PathBuf,
    published: bool,
    armed: bool,
}
impl ScratchGuard<'_> {
    async fn clean(&mut self) -> Result<(), SetupError> {
        let result = async {
            self.executor
                .execute(
                    FsOperation::Discard {
                        cache: self.cache.clone(),
                        path: self.keyed.clone(),
                    },
                    Cancellation::never(),
                )
                .await?
                .unit()?;
            if !self.published {
                self.executor
                    .execute(
                        FsOperation::Discard {
                            cache: self.cache.clone(),
                            path: self.staging.clone(),
                        },
                        Cancellation::never(),
                    )
                    .await?
                    .unit()?;
            }
            Ok(())
        }
        .await;
        self.armed = false;
        result
    }
}
impl Drop for ScratchGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let mut operations = vec![FsOperation::Discard {
                cache: self.cache.clone(),
                path: self.keyed.clone(),
            }];
            if !self.published {
                operations.push(FsOperation::Discard {
                    cache: self.cache.clone(),
                    path: self.staging.clone(),
                });
            }
            self.executor.detach(operations);
        }
    }
}
fn succeeded(outcome: &Outcome) -> bool {
    outcome
        .exit_code
        .as_ref()
        .is_some_and(|n| n == &BigInt::from(0))
        && !outcome.timed_out
        && !outcome.cancelled
        && !outcome.oom_killed
        && outcome.output_error.is_none()
}
async fn keyed_staging(
    context: &ProvisionContext<'_>,
    staging: &Path,
) -> Result<PathBuf, SetupError> {
    match context
        .execute(FsOperation::Staging {
            cache: Arc::clone(context.cache),
            prefix: PathBuf::from("setup-code-"),
        })
        .await
    {
        Ok(value) => value.path(),
        Err(SetupError::Cancelled) => {
            context
                .executor
                .execute(
                    FsOperation::Discard {
                        cache: Arc::clone(context.cache),
                        path: staging.to_owned(),
                    },
                    Cancellation::never(),
                )
                .await?
                .unit()?;
            Err(SetupError::Cancelled)
        }
        Err(error) => Err(error),
    }
}
struct PublishedHold {
    cache: Arc<CacheRoot>,
    executor: Arc<dyn FsExecutor>,
    path: PathBuf,
    armed: bool,
}
impl PublishedHold {
    fn transfer(mut self, held: &mut Held<'_>, cancel: &Cancellation) -> Result<(), SetupError> {
        held.paths.push(self.path.clone());
        self.armed = false;
        cancel.check()
    }
}
impl Drop for PublishedHold {
    fn drop(&mut self) {
        if self.armed {
            self.executor.detach(vec![FsOperation::Release {
                cache: self.cache.clone(),
                paths: vec![self.path.clone()],
            }]);
        }
    }
}
struct RunMiss {
    setup: SetupRun,
    publication: Option<PublishedHold>,
}
async fn finish_miss(
    cleanup: &mut ScratchGuard<'_>,
    mut publication: Option<PublishedHold>,
    result: Result<SetupRun, SetupError>,
) -> Result<RunMiss, SetupError> {
    if let Err(error) = cleanup.clean().await {
        // Ordinary Python finally failure occurs before held.append(entry):
        // preserve that completed partial failure, but guard abandoned awaits.
        if let Some(hold) = &mut publication {
            hold.armed = false;
        }
        return Err(error);
    }
    result.map(|setup| RunMiss { setup, publication })
}
async fn run_miss(
    context: &ProvisionContext<'_>,
    step: &StepSpec,
    setup: &SetupSpec,
    key: String,
    code: &Path,
    entry: &Path,
    directory: &Path,
) -> Result<RunMiss, SetupError> {
    let ProvisionContext {
        cache,
        run,
        cancel,
        executor,
        ..
    } = context;
    let staging = context
        .execute(FsOperation::Staging {
            cache: Arc::clone(cache),
            prefix: PathBuf::from("setup-"),
        })
        .await?
        .path()?;
    let keyed = keyed_staging(context, &staging).await?;
    let mut cleanup = ScratchGuard {
        cache,
        keyed,
        staging,
        published: false,
        executor,
        armed: true,
    };
    let mut publication = None;
    let result = async {
        context
            .execute(FsOperation::Stage {
                code: code.to_owned(),
                files: setup.key_files.clone(),
                target: cleanup.keyed.clone(),
            })
            .await?
            .unit()?;
        context
            .execute(FsOperation::Mkdir(directory.to_owned()))
            .await?
            .unit()?;
        let outcome = run
            .run(
                setup_step(step, setup, &cleanup.keyed, &cleanup.staging)?,
                directory.to_owned(),
                setup.deadline_seconds.clone(),
                cancel.clone(),
            )
            .await?;
        cancel.check()?;
        let mut result = SetupRun {
            key,
            outcome,
            published: false,
            missing: Vec::new(),
            unsafe_tree: None,
        };
        if succeeded(&result.outcome) {
            result.missing = context
                .execute(FsOperation::Missing {
                    cache: cleanup.staging.clone(),
                    paths: setup.paths.clone(),
                })
                .await?
                .missing()?;
            if result.missing.is_empty() {
                match context
                    .execute(FsOperation::Publish {
                        cache: Arc::clone(cache),
                        staging: cleanup.staging.clone(),
                        path: entry.to_owned(),
                    })
                    .await
                {
                    Ok(value) => {
                        value.boolean()?;
                        publication = Some(PublishedHold {
                            cache: Arc::clone(cache),
                            executor: Arc::clone(executor),
                            path: entry.to_owned(),
                            armed: true,
                        });
                        cleanup.published = true;
                        result.published = true;
                    }
                    Err(SetupError::Cache(CacheError::Tree(TreeError::Unsafe))) => {
                        result.unsafe_tree = Some(TreeError::Unsafe);
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(result)
    }
    .await;
    finish_miss(&mut cleanup, publication, result).await
}
/// Required boundaries and the caller's explicit rendering budget.
pub struct ProvisionContext<'a> {
    pub cache: &'a Arc<CacheRoot>,
    pub source: &'a dyn CodeSource,
    pub run: &'a dyn RunSetup,
    pub rendering_budget: usize,
    pub cancel: Cancellation,
    pub executor: &'a Arc<dyn FsExecutor>,
}
impl ProvisionContext<'_> {
    async fn execute(&self, operation: FsOperation) -> Result<FsValue, SetupError> {
        self.cancel.check()?;
        self.executor.execute(operation, self.cancel.clone()).await
    }
}
fn is_dir(path: &Path) -> Result<bool, SetupError> {
    nul(path)?;
    match fs::metadata(path) {
        Ok(m) => Ok(m.is_dir()),
        Err(e) if matches!(e.raw_os_error(), Some(2 | 9 | 20 | 40)) => Ok(false),
        Err(e) => Err(io(&e)),
    }
}
/// Await this future to completion; use `ProvisionTask` for externally cancellable supervision.
/// # Errors
/// Releases source-registered holds on exceptional paths; cleanup failures supersede
/// earlier failures. A completed post-publication cleanup failure retains the
/// source's unregistered publication hold; abandoned awaits have an armed guard.
pub async fn provision(
    manifest: &Document,
    step: StepSpec,
    directory: &Path,
    context: ProvisionContext<'_>,
) -> Result<Provisioned, SetupError> {
    let ProvisionContext {
        cache,
        source,
        rendering_budget: budget,
        ref cancel,
        ..
    } = context;
    let Some(code_ref) = code_ref(manifest, budget)? else {
        return Ok(Provisioned {
            step,
            held: Vec::new(),
            setup: None,
        });
    };
    let tree = source.tree(&code_ref, cancel.clone()).await?;
    let mut held = Held {
        cache,
        executor: context.executor,
        paths: vec![tree.clone()],
        armed: true,
    };
    let result = async {
        cancel.check()?;
        let code = context
            .execute(FsOperation::Resolve {
                tree: tree.clone(),
                relative: code_ref.path.clone(),
            })
            .await?
            .path()?;
        if !context
            .execute(FsOperation::IsDir(code.clone()))
            .await?
            .boolean()?
        {
            return Err(SetupError::InvalidCode);
        }
        let Some(setup) = setup_spec(manifest, budget)? else {
            return Ok(Provisioned {
                step: code_step(&step, &code, None)?,
                held: Vec::new(),
                setup: None,
            });
        };
        let files = context
            .execute(FsOperation::Digests {
                code: code.clone(),
                files: setup.key_files.clone(),
            })
            .await?
            .digests()?;
        let key = cache_key(
            &step.image,
            &setup.run,
            &code_ref,
            &step.env,
            &setup,
            manifest_trust(manifest, budget)?,
            &files,
        )?;
        let entry = cache.setup_path(&key)?;
        if context
            .execute(FsOperation::Acquire {
                cache: Arc::clone(cache),
                path: entry.clone(),
            })
            .await?
            .boolean()?
        {
            held.register(entry.clone(), cancel)?;
            return Ok(Provisioned {
                step: code_step(&step, &code, Some(&entry))?,
                held: Vec::new(),
                setup: None,
            });
        }
        let run = run_miss(&context, &step, &setup, key, &code, &entry, directory).await?;
        let prepared = if let Some(publication) = run.publication {
            publication.transfer(&mut held, cancel)?;
            code_step(&step, &code, Some(&entry))?
        } else {
            step
        };
        Ok(Provisioned {
            step: prepared,
            held: Vec::new(),
            setup: Some(run.setup),
        })
    }
    .await;
    match result {
        Ok(mut result) => {
            result.held = held.transfer();
            Ok(result)
        }
        Err(error) => Err(held.failure(error).await),
    }
}
/// Owned task prevents a frontend's dropped waiter from dropping async callback cleanup.
/// Dropping requests cancellation; the task still settles. Frontends must await settle
/// before deleting job directories or closing the cache.
struct TaskResult {
    result: Option<Result<Provisioned, SetupError>>,
    cache: Arc<CacheRoot>,
    executor: Arc<dyn FsExecutor>,
}
impl Drop for TaskResult {
    fn drop(&mut self) {
        if let Some(Ok(result)) = &self.result {
            self.executor.detach(vec![FsOperation::Release {
                cache: self.cache.clone(),
                paths: result.held.clone(),
            }]);
        }
    }
}
pub struct ProvisionTask {
    cancel: tokio::sync::watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<TaskResult>>,
}
/// Monotonic owned signal; callers still settle the provisioning task.
#[derive(Clone)]
pub struct ProvisionCancellation(tokio::sync::watch::Sender<bool>);
impl ProvisionCancellation {
    pub fn cancel(&self) {
        let _ = self.0.send(true);
    }
}
/// All runtime boundaries are mandatory and owned for task settlement.
pub struct ProvisionRequest {
    pub manifest: Arc<Document>,
    pub step: StepSpec,
    pub cache: Arc<CacheRoot>,
    pub source: Arc<dyn CodeSource>,
    pub directory: PathBuf,
    pub run: Arc<dyn RunSetup>,
    pub rendering_budget: usize,
    pub executor: Arc<dyn FsExecutor>,
}
impl ProvisionTask {
    #[must_use]
    pub fn cancellation_handle(&self) -> ProvisionCancellation {
        ProvisionCancellation(self.cancel.clone())
    }
    #[must_use]
    pub fn start(request: ProvisionRequest) -> Self {
        let ProvisionRequest {
            manifest,
            step,
            cache,
            source,
            directory,
            run,
            rendering_budget: budget,
            executor,
        } = request;
        let (sender, receiver) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move {
            let result = provision(
                &manifest,
                step,
                &directory,
                ProvisionContext {
                    cache: &cache,
                    source: source.as_ref(),
                    run: run.as_ref(),
                    rendering_budget: budget,
                    cancel: Cancellation(receiver),
                    executor: &executor,
                },
            )
            .await;
            TaskResult {
                result: Some(result),
                cache,
                executor,
            }
        });
        Self {
            cancel: sender,
            task: Some(task),
        }
    }
    pub fn cancel(&self) {
        let _ = self.cancel.send(true);
    }
    /// # Errors
    /// Returns the settled callback/provisioning failure or sanitized join failure.
    pub async fn settle(mut self) -> Result<Provisioned, SetupError> {
        let task = self.task.take().ok_or(SetupError::Task)?;
        let mut result = task.await.map_err(|_| SetupError::Task)?;
        result.result.take().ok_or(SetupError::Task)?
    }
}
impl Drop for ProvisionTask {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::cache::{CacheEvent, CacheLog};
    use std::os::unix::ffi::OsStrExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Log;
    impl CacheLog for Log {
        fn event(&self, _event: CacheEvent) -> Result<(), CacheError> {
            Ok(())
        }
    }
    struct Seams {
        code: PathBuf,
        cache: Arc<CacheRoot>,
    }
    impl CodeSource for Seams {
        fn tree<'a>(
            &'a self,
            _code: &'a CodeRef,
            _cancel: Cancellation,
        ) -> SetupFuture<'a, PathBuf> {
            Box::pin(async move {
                self.cache.hold(&self.code)?;
                Ok(self.code.clone())
            })
        }
    }
    impl RunSetup for Seams {
        fn run(
            &self,
            step: StepSpec,
            _directory: PathBuf,
            _seconds: BigInt,
            _cancel: Cancellation,
        ) -> SetupFuture<'_, Outcome> {
            Box::pin(async move {
                fs::write(step.mounts[1].source().join("installed"), b"ready")
                    .map_err(|e| io(&e))?;
                Ok(Outcome::new(Some(0.into())))
            })
        }
    }
    struct CleanupBarrier {
        entered: tokio::sync::Notify,
        resume: tokio::sync::Notify,
        finished: Arc<tokio::sync::Notify>,
        detached: Arc<AtomicUsize>,
    }
    impl FsExecutor for CleanupBarrier {
        fn execute(
            &self,
            operation: FsOperation,
            cancel: Cancellation,
        ) -> SetupFuture<'_, FsValue> {
            Box::pin(async move {
                if matches!(&operation, FsOperation::Discard { path, .. } if path.file_name().is_some_and(|n|n.as_bytes().starts_with(b"setup-code-")))
                {
                    assert!(!cancel.requested());
                    self.entered.notify_one();
                    self.resume.notified().await;
                }
                SpawnBlocking.execute(operation, cancel).await
            })
        }
        fn detach(&self, operations: Vec<FsOperation>) {
            let finished = self.finished.clone();
            let detached = self.detached.clone();
            tokio::spawn(async move {
                for operation in operations {
                    SpawnBlocking
                        .execute(operation, Cancellation::never())
                        .await
                        .unwrap();
                }
                detached.fetch_add(1, Ordering::Release);
                finished.notify_one();
            });
        }
    }
    #[tokio::test]
    async fn published_hold_survives_cleanup_await_and_abandoned_future_releases_it() {
        for abandoned in [false, true] {
            let root = std::env::temp_dir().join(format!(
                "cannery-setup-published-{}-{abandoned}",
                std::process::id()
            ));
            fs::create_dir(&root).unwrap();
            fs::create_dir(root.join("code")).unwrap();
            let cache = Arc::new(
                CacheRoot::new(&root.join("cache"), (20_u64 << 30).into(), Arc::new(Log)).unwrap(),
            );
            cache.open().unwrap();
            let seams = Arc::new(Seams {
                code: root.join("code"),
                cache: cache.clone(),
            });
            let barrier = Arc::new(CleanupBarrier {
                entered: tokio::sync::Notify::new(),
                resume: tokio::sync::Notify::new(),
                finished: Arc::new(tokio::sync::Notify::new()),
                detached: Arc::new(AtomicUsize::new(0)),
            });
            let manifest = Arc::new(cannery_core::json::decode(br#"{"spec":{"role":"producer","code":{"repo":"Owner/Repo","commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"setup":{"run":"install","cache":{"key_files":[],"paths":["installed"]}}}}"#, 64).unwrap());
            let step = StepSpec {
                job_id: String::from("job"),
                label: String::from("producer"),
                image: String::from(&format!("registry/step@sha256:{}", "b".repeat(64))),
                command: vec![String::from("run")],
                args: Vec::new(),
                env: Vec::new(),
                resources: launcher::Resources::default(),
                egress: Vec::new(),
                mounts: Vec::new(),
                workdir: None,
                open_network: false,
            };
            if abandoned {
                let executor: Arc<dyn FsExecutor> = barrier.clone();
                let owned_cache = cache.clone();
                let directory = root.join("job");
                let raw = tokio::spawn(async move {
                    provision(
                        &manifest,
                        step,
                        &directory,
                        ProvisionContext {
                            cache: &owned_cache,
                            source: seams.as_ref(),
                            run: seams.as_ref(),
                            rendering_budget: 128,
                            cancel: Cancellation::never(),
                            executor: &executor,
                        },
                    )
                    .await
                });
                barrier.entered.notified().await;
                assert_eq!(cache.holds().unwrap().len(), 2);
                raw.abort();
                assert!(raw.await.unwrap_err().is_cancelled());
                while barrier.detached.load(Ordering::Acquire) != 3 {
                    barrier.finished.notified().await;
                }
            } else {
                let task = ProvisionTask::start(ProvisionRequest {
                    manifest,
                    step,
                    cache: cache.clone(),
                    source: seams.clone(),
                    directory: root.join("job"),
                    run: seams,
                    rendering_budget: 128,
                    executor: barrier.clone(),
                });
                barrier.entered.notified().await;
                assert_eq!(cache.holds().unwrap().len(), 2);
                task.cancel();
                barrier.resume.notify_one();
                assert!(matches!(task.settle().await, Err(SetupError::Cancelled)));
            }
            assert_eq!(cache.holds().unwrap().len(), 0);
            assert_eq!(cache.entries().unwrap().len(), 1);
            assert_eq!(fs::read_dir(root.join("cache/tmp")).unwrap().count(), 0);
            cache.close().unwrap();
            crate::removal::remove_tree(&root);
        }
    }
    #[test]
    fn closed_false_cancellation_remains_pending() {
        let mut cancel = Cancellation::never();
        assert!(!cancel.requested());
        let mut future = Box::pin(cancel.cancelled());
        assert!(
            future
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                .is_pending()
        );
    }
    #[test]
    fn key_file_descriptor_is_close_on_exec() {
        let path = std::env::temp_dir().join(format!("cannery-setup-fd-{}", std::process::id()));
        let original = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let file = open_key_file(&path).unwrap();
        fs::remove_file(&path).unwrap();
        let flags = rustix::io::fcntl_getfd(&file).unwrap();
        assert!(flags.contains(rustix::io::FdFlags::CLOEXEC));
        drop(original);
    }
}
