// SPDX-License-Identifier: AGPL-3.0-only
//! Markdown documents with YAML front matter, the format of every phase output.
//!
//! A document opens with a line `---`, then the front matter (one strict YAML
//! mapping, see [`crate::yaml`]), then a closing line `---` or `...`; the rest
//! is the Markdown body, kept byte for byte. Lines end with `\n` or `\r\n`. A
//! leading byte order mark is ignored. The body may be empty; the front matter
//! may be empty (an empty mapping), but it must be there.
use crate::yaml::{self, YamlError};
use serde_json::{Map, Value};

/// Bounds on one document, checked before and while parsing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    /// The whole document, front matter and body, in UTF-8 bytes.
    pub max_bytes: usize,
    pub max_depth: usize,
    pub max_nodes: usize,
}

impl Default for Limits {
    /// 1 MiB, nesting depth 64, 100 000 front matter nodes.
    fn default() -> Self {
        Self {
            max_bytes: 1 << 20,
            max_depth: 64,
            max_nodes: 100_000,
        }
    }
}

/// A parsed document: its front matter as a JSON object and its body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Document {
    pub front_matter: Map<String, Value>,
    pub body: String,
}

/// Why a document was refused. Messages never quote the document.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum FrontMatterError {
    #[error("document exceeds its size limit")]
    TooLarge,
    #[error("NUL characters are forbidden")]
    Nul,
    #[error("document must open with a front matter line `---`")]
    MissingFrontMatter,
    #[error("front matter is not closed by a line `---` or `...`")]
    Unterminated,
    #[error("front matter is not strict YAML: {0}")]
    Yaml(YamlError),
    #[error("front matter must be a mapping")]
    NotMapping,
}

/// Split a document into its front matter and its body.
/// # Errors
/// Refuses a document over its limits, without an opening or closing line,
/// whose front matter is not strict YAML, or is not a mapping.
pub fn parse(text: &str, limits: Limits) -> Result<Document, FrontMatterError> {
    if text.len() > limits.max_bytes {
        return Err(FrontMatterError::TooLarge);
    }
    if text.contains('\0') {
        return Err(FrontMatterError::Nul);
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = Lines { rest: text };
    if lines.next() != Some("---") {
        return Err(FrontMatterError::MissingFrontMatter);
    }
    let start = text.len() - lines.rest.len();
    loop {
        let before = text.len() - lines.rest.len();
        let Some(line) = lines.next() else {
            return Err(FrontMatterError::Unterminated);
        };
        if line == "---" || line == "..." {
            let yaml = &text[start..before];
            let front_matter = match yaml::parse(yaml, limits.max_depth, limits.max_nodes)
                .map_err(FrontMatterError::Yaml)?
            {
                Value::Null => Map::new(),
                Value::Object(fields) => fields,
                _ => return Err(FrontMatterError::NotMapping),
            };
            return Ok(Document {
                front_matter,
                body: lines.rest.to_owned(),
            });
        }
    }
}

/// Lines with their terminators removed; the last line may have none.
struct Lines<'a> {
    rest: &'a str,
}

impl<'a> Iterator for Lines<'a> {
    type Item = &'a str;
    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        let (line, rest) = match self.rest.find('\n') {
            Some(end) => (&self.rest[..end], &self.rest[end + 1..]),
            None => (self.rest, ""),
        };
        self.rest = rest;
        Some(line.strip_suffix('\r').unwrap_or(line))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parsed(text: &str) -> Result<Document, FrontMatterError> {
        parse(text, Limits::default())
    }

    #[test]
    fn splits_front_matter_from_the_body() -> Result<(), FrontMatterError> {
        let document = parsed(
            "---\nverdict: pass\nreason: all gates passed\n---\n# Notes\n\n--- not a delimiter\n",
        )?;
        assert_eq!(
            Value::Object(document.front_matter),
            json!({"verdict": "pass", "reason": "all gates passed"})
        );
        assert_eq!(document.body, "# Notes\n\n--- not a delimiter\n");
        Ok(())
    }

    #[test]
    fn accepts_crlf_bom_dots_and_empty_parts() -> Result<(), FrontMatterError> {
        let document = parsed("\u{feff}---\r\na: 1\r\n...\r\nbody\r\n")?;
        assert_eq!(Value::Object(document.front_matter), json!({"a": 1}));
        assert_eq!(document.body, "body\r\n");
        let empty = parsed("---\n---\n")?;
        assert!(empty.front_matter.is_empty());
        assert_eq!(empty.body, "");
        assert_eq!(parsed("---\n---")?.body, "");
        Ok(())
    }

    #[test]
    fn refuses_documents_without_proper_front_matter() {
        assert_eq!(
            parsed("# Title\n"),
            Err(FrontMatterError::MissingFrontMatter)
        );
        assert_eq!(parsed(""), Err(FrontMatterError::MissingFrontMatter));
        assert_eq!(
            parsed(" ---\na: 1\n---\n"),
            Err(FrontMatterError::MissingFrontMatter)
        );
        assert_eq!(parsed("---\na: 1\n"), Err(FrontMatterError::Unterminated));
        assert_eq!(parsed("---\n- a\n---\n"), Err(FrontMatterError::NotMapping));
        assert_eq!(parsed("---\n7\n---\n"), Err(FrontMatterError::NotMapping));
        assert_eq!(
            parsed("---\na: 1\na: 2\n---\n"),
            Err(FrontMatterError::Yaml(YamlError::Invalid))
        );
        assert_eq!(parsed("---\na: x\n---\n\0"), Err(FrontMatterError::Nul));
        assert_eq!(
            parse(
                "---\na: 1\n---\nbody",
                Limits {
                    max_bytes: 8,
                    ..Limits::default()
                }
            ),
            Err(FrontMatterError::TooLarge)
        );
    }
}
