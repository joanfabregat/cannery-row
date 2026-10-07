//! Small authored source-archive fixtures built with the production dependencies.
use flate2::{Compression, write::GzEncoder};
use std::io::{Cursor, Write};
pub const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
pub struct Entry<'a> {
    pub path: &'a str,
    pub kind: tar::EntryType,
    pub body: &'a [u8],
    pub link: Option<&'a str>,
    pub mode: u32,
}
impl<'a> Entry<'a> {
    pub fn file(path: &'a str, body: &'a [u8]) -> Self {
        Self {
            path,
            kind: tar::EntryType::Regular,
            body,
            link: None,
            mode: 0o644,
        }
    }
    pub fn directory(path: &'a str) -> Self {
        Self {
            path,
            kind: tar::EntryType::Directory,
            body: &[],
            link: None,
            mode: 0o755,
        }
    }
    pub fn link(path: &'a str, target: &'a str) -> Self {
        Self {
            path,
            kind: tar::EntryType::Symlink,
            body: &[],
            link: Some(target),
            mode: 0o777,
        }
    }
}
pub fn gzip(body: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(body).unwrap();
    encoder.finish().unwrap()
}
pub fn raw(commit: Option<&str>, entries: &[Entry<'_>]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    if let Some(commit) = commit {
        let record = format!("comment={commit}\n");
        let mut length = record.len() + 2;
        while length != record.len() + length.to_string().len() + 1 {
            length = record.len() + length.to_string().len() + 1;
        }
        let record = format!("{length} {record}");
        let entry = Entry {
            path: "pax_global_header",
            kind: tar::EntryType::XGlobalHeader,
            body: record.as_bytes(),
            link: None,
            mode: 0o644,
        };
        append(&mut builder, &entry);
    }
    for entry in entries {
        append(&mut builder, entry);
    }
    builder.into_inner().unwrap()
}
fn append(builder: &mut tar::Builder<Vec<u8>>, entry: &Entry<'_>) {
    let mut header = tar::Header::new_ustar();
    // Raw bytes intentionally permit adversarial path fixtures which set_path rejects.
    assert!(entry.path.len() < 100);
    header.as_mut_bytes()[..entry.path.len()].copy_from_slice(entry.path.as_bytes());
    header.set_entry_type(entry.kind);
    header.set_size(entry.body.len() as u64);
    header.set_mode(entry.mode);
    if let Some(link) = entry.link {
        assert!(link.len() < 100);
        header.as_mut_bytes()[157..157 + link.len()].copy_from_slice(link.as_bytes());
    }
    header.set_cksum();
    builder.append(&header, Cursor::new(entry.body)).unwrap();
}
pub fn archive(commit: Option<&str>, entries: &[Entry<'_>]) -> Vec<u8> {
    gzip(&raw(commit, entries))
}
