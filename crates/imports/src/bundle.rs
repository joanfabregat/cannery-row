use crate::{Error, Problem, Result};
use cannery_core::{
    contracts::{ContractKind, ContractValidator},
    json,
};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, open, openat, statat};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::Path,
};

/// Explicit resource policy for parsing a reviewed directory.
#[derive(Clone, Copy, Debug)]
pub struct BundleLimits {
    pub max_files: usize,
    pub max_file_bytes: usize,
    pub max_report_bytes: usize,
    pub max_total_bytes: usize,
    pub max_depth: usize,
    pub max_nodes: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Project,
    Policy,
    Track,
    Unit,
}
impl EntryKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Policy => "policy",
            Self::Track => "track",
            Self::Unit => "unit",
        }
    }
}
#[derive(Clone)]
pub struct Entry {
    pub(crate) kind: EntryKind,
    pub(crate) key: String,
    pub(crate) path: String,
    pub(crate) content: Value,
}
impl Entry {
    #[must_use]
    pub fn kind(&self) -> EntryKind {
        self.kind
    }
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
    #[must_use]
    pub fn content(&self) -> &Value {
        &self.content
    }
    pub(crate) fn problem(&self, pointer: &str, message: &str) -> Problem {
        Problem::new(&self.path, pointer, message)
    }
}
impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Entry([redacted])")
    }
}
/// Schema-checked entries and reports. Construction only occurs through the safe reader.
#[derive(Clone)]
pub struct Bundle {
    pub(crate) sha256: String,
    pub(crate) project: Entry,
    pub(crate) entries: Vec<Entry>,
    pub(crate) reports: BTreeMap<String, String>,
}
impl Bundle {
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    #[must_use]
    pub fn project(&self) -> &Entry {
        &self.project
    }
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        std::iter::once(&self.project).chain(&self.entries)
    }
    #[must_use]
    pub fn reports(&self) -> &BTreeMap<String, String> {
        &self.reports
    }
}
impl std::fmt::Debug for Bundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Bundle([redacted])")
    }
}
/// Sorted-object UTF-8 compact JSON digest. Finite schema-checked values only.
#[must_use]
pub fn canonical_sha256(value: &Value) -> String {
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), sorted(value)))
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
            _ => value.clone(),
        }
    }
    let digest = Sha256::digest(sorted(value).to_string().as_bytes());
    hex(&digest)
}
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|byte| [byte >> 4, byte & 15])
        .map(|digit| char::from(b"0123456789abcdef"[usize::from(digit)]))
        .collect()
}
struct Reader {
    limits: BundleLimits,
    count: usize,
    bytes: usize,
    files: BTreeMap<String, Vec<u8>>,
}
impl Reader {
    #[allow(clippy::too_many_lines)] // Keep fd-relative traversal and accounting in one scope.
    fn directory(&mut self, directory: &File, prefix: &str, depth: usize) -> Result<()> {
        if depth > self.limits.max_depth {
            return Err(Error::problem(
                "bundle",
                "",
                "directory depth limit exceeded",
            ));
        }
        let entries = Dir::read_from(directory)
            .map_err(|_| Error::problem("bundle", "", "cannot read directory"))?;
        let mut names = Vec::new();
        for entry in entries {
            let entry =
                entry.map_err(|_| Error::problem("bundle", "", "cannot read directory entry"))?;
            let name = entry
                .file_name()
                .to_str()
                .map_err(|_| Error::problem("bundle", "", "file names must be UTF-8"))?;
            if name == "." || name == ".." {
                continue;
            }
            self.count = self
                .count
                .checked_add(1)
                .ok_or_else(|| Error::problem("bundle", "", "file count overflow"))?;
            if self.count > self.limits.max_files {
                return Err(Error::problem("bundle", "", "file count limit exceeded"));
            }
            names.push(name.to_owned());
        }
        names.sort();
        for name in names {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let stat = statat(directory, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|_| Error::problem(&path, "", "cannot inspect entry"))?;
            let kind = FileType::from_raw_mode(stat.st_mode);
            if kind == FileType::Symlink {
                return Err(Error::problem(&path, "", "symbolic links are forbidden"));
            }
            if name.starts_with('.') {
                continue;
            }
            if kind == FileType::Directory {
                if !(prefix.is_empty()
                    && ["policies", "tracks", "units", "reports"].contains(&name.as_str())
                    || prefix == "reports"
                    || prefix.starts_with("reports/"))
                {
                    return Err(Error::problem(&path, "", "not part of a bundle"));
                }
                let fd = openat(
                    directory,
                    name.as_str(),
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|_| Error::problem(&path, "", "cannot open directory safely"))?;
                self.directory(&File::from(fd), &path, depth + 1)?;
            } else if kind == FileType::RegularFile {
                let report = prefix == "reports" || prefix.starts_with("reports/");
                let extension = Path::new(&name)
                    .extension()
                    .and_then(|v| v.to_str())
                    .unwrap_or("");
                if report && extension != "md"
                    || !report && !["yaml", "yml", "json"].contains(&extension)
                {
                    return Err(Error::problem(&path, "", "unsupported bundle file"));
                }
                let cap = if report {
                    self.limits.max_report_bytes
                } else {
                    self.limits.max_file_bytes
                };
                let fd = openat(
                    directory,
                    name.as_str(),
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|_| Error::problem(&path, "", "cannot open file safely"))?;
                let file = File::from(fd);
                if !file
                    .metadata()
                    .map_err(|_| Error::problem(&path, "", "cannot inspect file"))?
                    .is_file()
                {
                    return Err(Error::problem(&path, "", "not a regular file"));
                }
                let mut data = Vec::new();
                file.take(u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1))
                    .read_to_end(&mut data)
                    .map_err(|_| Error::problem(&path, "", "cannot read file"))?;
                self.bytes = self
                    .bytes
                    .checked_add(data.len())
                    .ok_or_else(|| Error::problem("bundle", "", "byte count overflow"))?;
                if data.len() > cap || self.bytes > self.limits.max_total_bytes {
                    return Err(Error::problem(&path, "", "bundle byte limit exceeded"));
                }
                self.files.insert(path, data);
            } else {
                return Err(Error::problem(&path, "", "not a regular file or directory"));
            }
        }
        Ok(())
    }
}
fn parse(path: &str, data: &[u8], limits: BundleLimits, total_nodes: &mut usize) -> Result<Value> {
    let text = std::str::from_utf8(data).map_err(|_| Error::problem(path, "", "not UTF-8 text"))?;
    if text.contains('\0') {
        return Err(Error::problem(path, "", "NUL characters are forbidden"));
    }
    let options = cannery_core::yaml::strict_options(
        limits.max_depth,
        limits.max_nodes.saturating_sub(*total_nodes),
    );
    cannery_core::yaml::validate_scalars(text, options.clone())
        .map_err(|_| Error::problem(path, "", "invalid, nonfinite or resource-limited scalar"))?;
    // JSON is a YAML subset: this pass supplies duplicate and parser-budget checks.
    let checked: Value = serde_saphyr::from_str_with_options(text, options)
        .map_err(|_| Error::problem(path, "", "invalid, duplicate or resource-limited document"))?;
    let value = if Path::new(path)
        .extension()
        .is_some_and(|extension| extension == "json")
    {
        serde_json::from_str(text).map_err(|_| Error::problem(path, "", "invalid JSON"))?
    } else {
        checked
    };
    let mut stack = vec![(&value, 0usize)];
    while let Some((value, depth)) = stack.pop() {
        *total_nodes += 1;
        if depth > limits.max_depth || *total_nodes > limits.max_nodes {
            return Err(Error::problem(path, "", "document resource limit exceeded"));
        }
        match value {
            Value::Array(items) => stack.extend(items.iter().map(|v| (v, depth + 1))),
            Value::Object(fields) => stack.extend(fields.values().map(|v| (v, depth + 1))),
            Value::Number(number) if number.to_string().len() > 1024 => {
                return Err(Error::problem(path, "", "number width limit exceeded"));
            }
            _ => {}
        }
    }
    Ok(value)
}
/// Read without following symlinks; validate the published bundle schema and report references.
/// # Errors
/// Reports only file names, pointers and value-free diagnostics.
#[allow(clippy::too_many_lines)] // Assemble, validate, then hash before publishing the immutable bundle.
pub fn read_bundle(
    root: &Path,
    limits: BundleLimits,
    contracts: &ContractValidator,
) -> Result<Bundle> {
    let fd = open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| Error::problem("bundle", "", "cannot open bundle directory safely"))?;
    let mut reader = Reader {
        limits,
        count: 0,
        bytes: 0,
        files: BTreeMap::new(),
    };
    reader.directory(&File::from(fd), "", 0)?;
    let mut project = None;
    let mut entries = Vec::new();
    let mut reports = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut document_nodes = 0;
    for (path, data) in reader.files {
        if path.starts_with("reports/") {
            let body =
                String::from_utf8(data).map_err(|_| Error::problem(&path, "", "not UTF-8 text"))?;
            if body.contains('\0') || body.trim().is_empty() {
                return Err(Error::problem(&path, "", "empty or NUL-containing report"));
            }
            reports.insert(path, body);
            continue;
        }
        let file = Path::new(&path);
        let stem = file
            .file_stem()
            .and_then(|v| v.to_str())
            .ok_or_else(|| Error::problem(&path, "", "invalid file name"))?;
        let directory = file.parent().and_then(|v| v.to_str()).unwrap_or("");
        let (kind, key_field) = match directory {
            "" if stem == "project" => (EntryKind::Project, "slug"),
            "policies" => (EntryKind::Policy, "id"),
            "tracks" => (EntryKind::Track, "slug"),
            "units" => (EntryKind::Unit, "id"),
            _ => return Err(Error::problem(&path, "", "not part of a bundle")),
        };
        if !seen.insert((kind, stem.to_owned())) {
            return Err(Error::problem(&path, "", "duplicate file stem"));
        }
        let content = parse(&path, &data, limits, &mut document_nodes)?;
        if !content.is_object() {
            return Err(Error::problem(&path, "", "must be an object"));
        }
        if kind != EntryKind::Project && content[key_field].as_str() != Some(stem) {
            return Err(Error::problem(
                &path,
                &format!("/{key_field}"),
                "must equal the file name",
            ));
        }
        let entry = Entry {
            kind,
            key: if kind == EntryKind::Project {
                "project".into()
            } else {
                stem.into()
            },
            path,
            content,
        };
        if kind == EntryKind::Project {
            if project.replace(entry).is_some() {
                return Err(Error::problem("project", "", "given more than once"));
            }
        } else {
            entries.push(entry);
        }
    }
    let project = project.ok_or_else(|| Error::problem("project.yaml", "", "missing"))?;
    let mut assembled = json!({"project":project.content,"policies":[],"tracks":[],"units":[]});
    for entry in &entries {
        let key = match entry.kind {
            EntryKind::Policy => "policies",
            EntryKind::Track => "tracks",
            EntryKind::Unit => "units",
            EntryKind::Project => return Err(Error::CorruptData),
        };
        assembled[key]
            .as_array_mut()
            .ok_or(Error::CorruptData)?
            .push(entry.content.clone());
    }
    let document = json::decode(assembled.to_string().as_bytes(), limits.max_depth)
        .map_err(|_| Error::problem("bundle", "", "invalid document"))?;
    let violations = contracts
        .violation_paths(ContractKind::ImportBundle, &document)
        .map_err(|_| Error::problem("bundle", "", "schema validation failed"))?;
    if !violations.is_empty() {
        return Err(Error::Refused(
            violations
                .iter()
                .map(|path| {
                    let pointer = path.as_utf8().unwrap_or_else(|| "/".into());
                    let mut parts = pointer.trim_start_matches('/').splitn(3, '/');
                    let group = parts.next().unwrap_or("");
                    let (file, local) = if group == "project" {
                        (
                            project.path.as_str(),
                            pointer.strip_prefix("/project").unwrap_or(""),
                        )
                    } else if let Some(kind) = match group {
                        "policies" => Some(EntryKind::Policy),
                        "tracks" => Some(EntryKind::Track),
                        "units" => Some(EntryKind::Unit),
                        _ => None,
                    } {
                        let index = parts.next().and_then(|index| index.parse::<usize>().ok());
                        let entry = index.and_then(|index| {
                            entries.iter().filter(|entry| entry.kind == kind).nth(index)
                        });
                        entry.map_or(("bundle", pointer.as_str()), |entry| {
                            (
                                entry.path.as_str(),
                                parts.next().map_or("", |suffix| {
                                    &pointer[pointer.len() - suffix.len() - 1..]
                                }),
                            )
                        })
                    } else {
                        ("bundle", pointer.as_str())
                    };
                    Problem::new(file, local, "does not satisfy the published import schema")
                })
                .collect(),
        ));
    }
    let mut referenced = BTreeSet::new();
    for entry in &mut entries {
        if entry.kind != EntryKind::Unit {
            continue;
        }
        if let Some(attempts) = entry
            .content
            .get_mut("attempts")
            .and_then(Value::as_array_mut)
        {
            for (index, attempt) in attempts.iter_mut().enumerate() {
                if let Some(report) = attempt.get_mut("report") {
                    let path = report["path"]
                        .as_str()
                        .ok_or(Error::CorruptData)?
                        .to_owned();
                    if !referenced.insert(path.clone()) {
                        return Err(Error::problem(
                            &entry.path,
                            &format!("/attempts/{index}/report/path"),
                            "another attempt references this report",
                        ));
                    }
                    let body = reports.get(&path).ok_or_else(|| {
                        Error::problem(
                            &entry.path,
                            &format!("/attempts/{index}/report/path"),
                            "no such report in the bundle",
                        )
                    })?;
                    report["sha256"] = Value::String(hex(&Sha256::digest(body.as_bytes())));
                }
            }
        }
    }
    if reports.keys().any(|path| !referenced.contains(path)) {
        return Err(Error::problem(
            "reports",
            "",
            "an unreferenced report exists",
        ));
    }
    for entry in &entries {
        let key = match entry.kind {
            EntryKind::Policy => "policies",
            EntryKind::Track => "tracks",
            EntryKind::Unit => "units",
            EntryKind::Project => return Err(Error::CorruptData),
        };
        if entry.kind == EntryKind::Unit
            && let Some(items) = assembled[key].as_array_mut()
            && let Some(old) = items
                .iter_mut()
                .find(|item| item["id"] == entry.content["id"])
        {
            *old = entry.content.clone();
        }
    }
    Ok(Bundle {
        sha256: canonical_sha256(&assembled),
        project,
        entries,
        reports,
    })
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
