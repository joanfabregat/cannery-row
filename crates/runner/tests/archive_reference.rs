//! Authored native archive interoperability, corruption and extraction-safety cases.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/archive.rs"]
mod fixtures;
use cannery_runner::archive::{self, CodeLimits, ExtractError};
use fixtures::{COMMIT, Entry};
use std::{fmt::Write, fs, io::Cursor, os::unix::fs::PermissionsExt, path::PathBuf};
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).unwrap();
        let mut name = String::new();
        for byte in bytes {
            write!(&mut name, "{byte:02x}").unwrap();
        }
        let path = std::env::temp_dir().join(format!("cannery-native-archive-{name}"));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn extract(bytes: &[u8], limits: &CodeLimits) -> (Temp, Result<(), ExtractError>) {
    let target = Temp::new();
    let result = archive::extract_into(Cursor::new(bytes), &target.0, COMMIT, limits);
    (target, result)
}
#[test]
fn github_style_archive_preserves_contents_links_and_readonly_permissions() {
    let mut executable = Entry::file("root/run.sh", b"#!/bin/sh\n");
    executable.mode = 0o7777;
    let bytes = fixtures::archive(
        Some(COMMIT),
        &[
            Entry::directory("root/"),
            Entry::directory("root/lib/"),
            Entry::file("root/lib/clé", b"UTF-8 content"),
            executable,
            Entry::link("root/link", "lib/clé"),
            Entry::link("root/lib/parent", "../run.sh"),
            Entry::link("root/dangling", "missing"),
        ],
    );
    let (target, result) = extract(&bytes, &CodeLimits::default());
    result.unwrap();
    assert_eq!(fs::read(target.0.join("link")).unwrap(), b"UTF-8 content");
    assert_eq!(
        fs::read_link(target.0.join("lib/parent")).unwrap(),
        PathBuf::from("../run.sh")
    );
    assert_eq!(
        fs::metadata(target.0.join("run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o555
    );
    assert_eq!(
        fs::metadata(target.0.join("lib/clé"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o444
    );
}
#[test]
fn verifies_commit_before_extracting_files() {
    for commit in [None, Some("wrong")] {
        let bytes = fixtures::archive(
            commit,
            &[Entry::directory("root/"), Entry::file("root/file", b"body")],
        );
        let (target, result) = extract(&bytes, &CodeLimits::default());
        assert_eq!(result, Err(ExtractError::Commit));
        assert_eq!(fs::read_dir(&target.0).unwrap().count(), 0);
    }
}
#[test]
fn gzip_extra_field_is_supported_but_crc_size_and_truncation_are_rejected() {
    let normal = fixtures::archive(
        Some(COMMIT),
        &[Entry::directory("root/"), Entry::file("root/file", b"body")],
    );
    let mut extra = normal.clone();
    extra[3] |= 4;
    extra.splice(10..10, [3, 0, b'f', b'o', b'o']);
    extract(&extra, &CodeLimits::default()).1.unwrap();
    for distance in [8, 4] {
        let mut corrupted = normal.clone();
        let index = corrupted.len() - distance;
        corrupted[index] ^= 1;
        assert_eq!(
            extract(&corrupted, &CodeLimits::default()).1,
            Err(ExtractError::Unsafe)
        );
    }
    for length in [3, normal.len() / 2, normal.len() - 1] {
        assert_eq!(
            extract(&normal[..length], &CodeLimits::default()).1,
            Err(ExtractError::Unsafe)
        );
    }
    let mut concatenated = normal;
    concatenated.extend(fixtures::gzip(b"hidden second gzip member"));
    assert_eq!(
        extract(&concatenated, &CodeLimits::default()).1,
        Err(ExtractError::Unsafe)
    );
}
#[test]
fn rejects_unsafe_paths_duplicates_types_and_links() {
    for path in [
        "/outside",
        "root/../outside",
        "root/./file",
        "root//file",
        "root/back\\slash",
        "other/file",
    ] {
        let bytes = fixtures::archive(
            Some(COMMIT),
            &[Entry::directory("root/"), Entry::file(path, b"body")],
        );
        assert_eq!(
            extract(&bytes, &CodeLimits::default()).1,
            Err(ExtractError::Unsafe),
            "{path}"
        );
    }
    for link in ["/outside", "../outside", "back\\slash"] {
        let bytes = fixtures::archive(
            Some(COMMIT),
            &[Entry::directory("root/"), Entry::link("root/link", link)],
        );
        assert_eq!(
            extract(&bytes, &CodeLimits::default()).1,
            Err(ExtractError::Unsafe),
            "{link}"
        );
    }
    for kind in [
        tar::EntryType::Link,
        tar::EntryType::Char,
        tar::EntryType::Block,
        tar::EntryType::Fifo,
        tar::EntryType::GNUSparse,
    ] {
        let mut entry = Entry::file("root/file", b"");
        entry.kind = kind;
        let bytes = fixtures::archive(Some(COMMIT), &[Entry::directory("root/"), entry]);
        assert_eq!(
            extract(&bytes, &CodeLimits::default()).1,
            Err(ExtractError::Unsafe),
            "{kind:?}"
        );
    }
    let duplicate = fixtures::archive(
        Some(COMMIT),
        &[
            Entry::directory("root/"),
            Entry::file("root/file", b"first"),
            Entry::file("root/file", b"second"),
        ],
    );
    assert_eq!(
        extract(&duplicate, &CodeLimits::default()).1,
        Err(ExtractError::Unsafe)
    );
    let pivot = fixtures::archive(
        Some(COMMIT),
        &[
            Entry::directory("root/"),
            Entry::directory("root/dir/"),
            Entry::link("root/pivot", "dir"),
            Entry::file("root/pivot/file", b"body"),
        ],
    );
    assert_eq!(
        extract(&pivot, &CodeLimits::default()).1,
        Err(ExtractError::Unsafe)
    );
}
#[test]
fn file_count_size_and_metadata_are_bounded() {
    let bytes = fixtures::archive(
        Some(COMMIT),
        &[Entry::directory("root/"), Entry::file("root/file", b"body")],
    );
    for limits in [
        CodeLimits {
            max_files: 0.into(),
            max_tree_bytes: 100.into(),
        },
        CodeLimits {
            max_files: 100.into(),
            max_tree_bytes: 3.into(),
        },
    ] {
        assert_eq!(extract(&bytes, &limits).1, Err(ExtractError::Unsafe));
    }
    let large = vec![b'x'; (1 << 20) + 1];
    let metadata = Entry {
        path: "pax",
        kind: tar::EntryType::XHeader,
        body: &large,
        link: None,
        mode: 0o644,
    };
    let bytes = fixtures::archive(Some(COMMIT), &[metadata, Entry::directory("root/")]);
    assert_eq!(
        extract(&bytes, &CodeLimits::default()).1,
        Err(ExtractError::Unsafe)
    );
    let mut raw = fixtures::raw(
        Some(COMMIT),
        &[Entry::directory("root/"), Entry::file("root/file", b"body")],
    );
    // Header at block three: declare an enormous body without supplying it.
    let offset = 3 * 512;
    let mut header = tar::Header::new_ustar();
    header
        .as_mut_bytes()
        .copy_from_slice(&raw[offset..offset + 512]);
    header.set_size(u64::MAX / 2);
    header.set_cksum();
    raw[offset..offset + 512].copy_from_slice(header.as_bytes());
    assert_eq!(
        extract(&fixtures::gzip(&raw), &CodeLimits::default()).1,
        Err(ExtractError::Unsafe)
    );
}
#[test]
fn local_pax_utf8_paths_use_native_decoder_semantics() {
    let mut raw = fixtures::raw(Some(COMMIT), &[Entry::directory("root/")]);
    raw.truncate(raw.len() - 1024);
    let mut builder = tar::Builder::new(raw);
    builder
        .append_pax_extensions([("path", b"root/long UTF-8 \xc3\xa9".as_slice())])
        .unwrap();
    let mut header = tar::Header::new_ustar();
    header.set_size(4);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, "root/placeholder", b"body".as_slice())
        .unwrap();
    let bytes = fixtures::gzip(&builder.into_inner().unwrap());
    let (target, result) = extract(&bytes, &CodeLimits::default());
    result.unwrap();
    assert_eq!(fs::read(target.0.join("long UTF-8 é")).unwrap(), b"body");
}

#[test]
fn destination_links_and_preexisting_files_are_never_followed_or_overwritten() {
    let bytes = fixtures::archive(
        Some(COMMIT),
        &[
            Entry::directory("root/"),
            Entry::file("root/file", b"replacement"),
        ],
    );
    let target = Temp::new();
    fs::write(target.0.join("file"), b"original").unwrap();
    assert_eq!(
        archive::extract_into(
            Cursor::new(&bytes),
            &target.0,
            COMMIT,
            &CodeLimits::default()
        ),
        Err(ExtractError::Unsafe)
    );
    assert_eq!(fs::read(target.0.join("file")).unwrap(), b"original");
    let base = Temp::new();
    let outside = Temp::new();
    let link = base.0.join("destination");
    std::os::unix::fs::symlink(&outside.0, &link).unwrap();
    assert_eq!(
        archive::extract_into(Cursor::new(bytes), &link, COMMIT, &CodeLimits::default()),
        Err(ExtractError::Unsafe)
    );
    assert_eq!(fs::read_dir(&outside.0).unwrap().count(), 0);
}

#[test]
fn tar_checksum_and_incomplete_member_bodies_fail_even_with_valid_gzip() {
    let normal = fixtures::raw(
        Some(COMMIT),
        &[Entry::directory("root/"), Entry::file("root/file", b"body")],
    );
    let mut bad_checksum = normal.clone();
    bad_checksum[3 * 512] ^= 1;
    assert_eq!(
        extract(&fixtures::gzip(&bad_checksum), &CodeLimits::default()).1,
        Err(ExtractError::Unsafe)
    );
    let mut short = normal;
    let mut header = tar::Header::new_ustar();
    header
        .as_mut_bytes()
        .copy_from_slice(&short[3 * 512..4 * 512]);
    header.set_size(10_000);
    header.set_cksum();
    short[3 * 512..4 * 512].copy_from_slice(header.as_bytes());
    short.truncate(4 * 512 + 2);
    assert_eq!(
        extract(&fixtures::gzip(&short), &CodeLimits::default()).1,
        Err(ExtractError::Unsafe)
    );
}
