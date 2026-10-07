//! Bounded streaming validation of registered job output interfaces.
use crate::science::InterfaceSpec;
use cannery_core::{
    contracts::instance::ProjectValidator,
    json::{self, Document, Node},
};
use num_bigint::BigInt;
use std::sync::Arc;

#[derive(Clone)]
pub struct Interface {
    pub reference: String,
    pub encoding: String,
    pub schema: Option<Arc<Document>>,
    pub max_bytes: Option<BigInt>,
    pub allow_empty: bool,
    pub magic: Option<String>,
    pub validate: bool,
}
impl std::fmt::Debug for Interface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Interface([redacted])")
    }
}
#[derive(Debug, thiserror::Error)]
#[error("registered output interface cannot be checked")]
pub struct Error;
impl Interface {
    /// Own the registration, preserving its already resolved encoding and magic defaults.
    /// # Errors
    /// Rejects unusable text or schema references without including output values.
    pub fn from_spec(document: &Document, spec: &InterfaceSpec) -> Result<Self, Error> {
        let schema = spec
            .schema
            .map(|id| {
                let mut b = json::DocumentBuilder::new();
                let root = b.import(document, id).map_err(|_| Error)?;
                b.finish(root).map(Arc::new).map_err(|_| Error)
            })
            .transpose()?;
        Ok(Self {
            reference: spec.reference.as_utf8().ok_or(Error)?,
            encoding: spec.encoding.as_utf8().ok_or(Error)?,
            schema,
            max_bytes: spec.max_bytes.clone(),
            allow_empty: spec.allow_empty,
            magic: spec
                .magic
                .as_ref()
                .map(|v| v.as_utf8().ok_or(Error))
                .transpose()?,
            validate: spec.validate,
        })
    }
    #[must_use]
    pub fn parses_content(&self) -> bool {
        self.validate && self.encoding != "binary"
    }
}
#[derive(Clone, Copy)]
pub struct ValidationContext {
    pub json_decode_budget: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub message: String,
    pub pointer: Option<String>,
    pub line: Option<usize>,
}
impl Issue {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            pointer: None,
            line: None,
        }
    }
}
/// One file. JSON documents and individual JSONL lines retain at most `json_max_bytes` bytes.
pub struct ContentChecker {
    interface: Interface,
    validator: Option<ProjectValidator>,
    context: ValidationContext,
    cap: usize,
    size: u64,
    head: Vec<u8>,
    magic_checked: bool,
    buffer: Vec<u8>,
    line: usize,
    issues: Vec<Issue>,
    stopped: bool,
}
impl ContentChecker {
    /// All memory and schema execution bounds are explicit caller inputs.
    /// # Errors
    /// Refuses zero parsing cap and unknown encodings before receiving bytes.
    pub fn new(
        interface: Interface,
        json_max_bytes: usize,
        context: ValidationContext,
    ) -> Result<Self, Error> {
        if json_max_bytes == 0
            || !matches!(interface.encoding.as_str(), "json" | "jsonl" | "binary")
        {
            return Err(Error);
        }
        let validator = interface
            .schema
            .as_ref()
            .map(|schema| ProjectValidator::new(schema))
            .transpose()
            .map_err(|_| Error)?;
        let magic_checked = interface.magic.is_none();
        Ok(Self {
            interface,
            validator,
            context,
            cap: json_max_bytes,
            size: 0,
            head: Vec::new(),
            magic_checked,
            buffer: Vec::new(),
            line: 0,
            issues: Vec::new(),
            stopped: false,
        })
    }
    #[must_use]
    pub fn done(&self) -> bool {
        self.stopped || self.issues.len() >= 5
    }
    #[must_use]
    pub fn buffered_bytes(&self) -> usize {
        self.head.len() + self.buffer.len()
    }
    fn add(&mut self, issue: Issue, fatal: bool) {
        if self.issues.len() < 5 {
            self.issues.push(issue);
        }
        self.stopped |= fatal;
    }
    /// Feed successive file bytes. Stops retaining data after a fatal refusal.
    /// # Errors
    /// Reports arithmetic or unusable registered-schema errors with no file values.
    pub fn feed(&mut self, chunk: &[u8]) -> Result<(), Error> {
        if self.done() || chunk.is_empty() {
            return Ok(());
        }
        self.size = self
            .size
            .checked_add(u64::try_from(chunk.len()).map_err(|_| Error)?)
            .ok_or(Error)?;
        if self
            .interface
            .max_bytes
            .as_ref()
            .is_some_and(|limit| BigInt::from(self.size) > *limit)
        {
            self.add(Issue::new("is larger than the interface's max_bytes"), true);
            return Ok(());
        }
        if !self.magic_checked {
            let needed = 4096usize.saturating_sub(self.head.len());
            self.head
                .extend_from_slice(&chunk[..needed.min(chunk.len())]);
            self.check_magic(false)?;
            if self.done() {
                return Ok(());
            }
        }
        if !self.interface.parses_content() {
            return Ok(());
        }
        if self.interface.encoding == "json" {
            if chunk.len() > self.cap.saturating_sub(self.buffer.len()) {
                self.buffer.clear();
                self.add(Issue::new("is too large to validate as one JSON document; declare encoding jsonl, or validate: false"),true);
            } else {
                self.buffer.extend_from_slice(chunk);
            }
        } else {
            for part in chunk.split_inclusive(|b| *b == b'\n') {
                if self.done() {
                    break;
                }
                let newline = part.last() == Some(&b'\n');
                let part = if newline {
                    &part[..part.len() - 1]
                } else {
                    part
                };
                if part.len() > self.cap.saturating_sub(self.buffer.len()) {
                    self.buffer.clear();
                    let mut issue = Issue::new("has a line longer than the JSON validation cap");
                    issue.line = Some(self.line + 1);
                    self.add(issue, true);
                    break;
                }
                self.buffer.extend_from_slice(part);
                if newline {
                    let line = std::mem::take(&mut self.buffer);
                    self.check_line(&line)?;
                }
            }
        }
        Ok(())
    }
    fn check_magic(&mut self, final_chunk: bool) -> Result<(), Error> {
        let Some(magic) = self.interface.magic.as_deref() else {
            return Ok(());
        };
        let matched = if magic == "json" {
            if self.head.len() < 4 && !final_chunk {
                return Ok(());
            }
            if self.head.starts_with(&[0xff, 0xfe])
                || self.head.starts_with(&[0xfe, 0xff])
                || self.head[..self.head.len().min(4)].contains(&0)
            {
                self.add(
                    Issue::new("is encoded as UTF-16 or UTF-32; JSON must be UTF-8"),
                    true,
                );
                return Ok(());
            }
            let head = self
                .head
                .strip_prefix(&[0xef, 0xbb, 0xbf])
                .unwrap_or(&self.head);
            let first = head.iter().find(|b| !b" \t\r\n".contains(b));
            if first.is_none() && !final_chunk && self.head.len() < 4096 {
                return Ok(());
            }
            first.is_some_and(|b| b"{[\"-0123456789tfn".contains(b))
        } else {
            let prefixes = match magic {
                "gzip" => vec![vec![0x1f, 0x8b]],
                "parquet" => vec![b"PAR1".to_vec()],
                "zip" => vec![
                    b"PK\x03\x04".to_vec(),
                    b"PK\x05\x06".to_vec(),
                    b"PK\x07\x08".to_vec(),
                ],
                hex => vec![
                    (0..hex.len())
                        .step_by(2)
                        .map(|i| {
                            hex.get(i..i + 2)
                                .and_then(|v| u8::from_str_radix(v, 16).ok())
                        })
                        .collect::<Option<Vec<_>>>()
                        .ok_or(Error)?,
                ],
            };
            let needed = prefixes.iter().map(Vec::len).max().unwrap_or(0);
            if self.head.len() < needed && !final_chunk {
                return Ok(());
            }
            prefixes.iter().any(|prefix| self.head.starts_with(prefix))
        };
        self.magic_checked = true;
        self.head.clear();
        if !matched {
            self.add(
                Issue::new("does not start with the interface's magic bytes"),
                true,
            );
        }
        Ok(())
    }
    fn check_line(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.line += 1;
        let bytes = if self.line == 1 {
            bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes)
        } else {
            bytes
        };
        if bytes.iter().all(|b| b" \r\t".contains(b)) {
            return Ok(());
        }
        self.parse(bytes, Some(self.line))
    }
    fn parse(&mut self, bytes: &[u8], line: Option<usize>) -> Result<(), Error> {
        let document = std::str::from_utf8(bytes).map_err(|_| ()).and_then(|text| {
            json::decode_str(text, self.context.json_decode_budget).map_err(|_| ())
        });
        let mut issues = match document {
            Err(()) => vec![Issue::new("is not valid UTF-8 JSON")],
            Ok(document) => {
                if document
                    .nodes()
                    .iter()
                    .any(|n| matches!(n,Node::Float(v) if !v.is_finite()))
                {
                    vec![Issue::new("has a number that is not finite")]
                } else if let Some(validator) = &self.validator {
                    match validator.violations(&document, &BigInt::from(5 - self.issues.len())) {
                        Ok(errors) => errors
                            .into_iter()
                            .map(|v| Issue {
                                message: "violates the registered output schema".into(),
                                pointer: Some(v.path),
                                line: None,
                            })
                            .collect(),
                        Err(_) => return Err(Error),
                    }
                } else {
                    vec![]
                }
            }
        };
        for issue in &mut issues {
            issue.line = line;
        }
        for issue in issues {
            self.add(issue, false);
        }
        Ok(())
    }
    /// Finish the final partial line/document, releasing retained parsing bytes.
    /// # Errors
    /// Reports unusable registered schemas rather than blaming uploaded content.
    pub fn finish(mut self) -> Result<Vec<Issue>, Error> {
        if self.size == 0 {
            return Ok(if self.interface.allow_empty {
                vec![]
            } else {
                vec![Issue::new(
                    "is empty, and the interface does not allow empty files",
                )]
            });
        }
        if !self.done() && !self.magic_checked {
            self.check_magic(true)?;
        }
        if !self.done() && self.interface.parses_content() {
            let bytes = std::mem::take(&mut self.buffer);
            if self.interface.encoding == "json" {
                self.parse(
                    bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes),
                    None,
                )?;
            } else if !bytes.is_empty() {
                self.check_line(&bytes)?;
            }
        }
        Ok(self.issues)
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
