//! Authored native copy-depth boundaries, independent of interpreter profiles.
#![forbid(unsafe_code)]
#![allow(clippy::expect_used, clippy::unwrap_used)]
use cannery_runner::local_preparation::{
    CopyLimits, LocalPreparation, MetadataCapabilities, PreparationError, copy_tree,
};
use std::{
    fmt::Write,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).expect("entropy");
        let mut name = String::new();
        for byte in nonce {
            write!(&mut name, "{byte:02x}").expect("string write");
        }
        let root = std::env::temp_dir().join(format!("cannery-native-copy-{name}"));
        fs::create_dir(&root).expect("owned root");
        Self(root)
    }
    fn tree(&self, depth: usize) -> PathBuf {
        let source = self.0.join("source");
        fs::create_dir(&source).expect("source root");
        let mut leaf = source;
        for _ in 0..depth {
            leaf.push("a");
            fs::create_dir(&leaf).expect("source directory");
        }
        fs::write(leaf.join("file"), b"native content").expect("leaf file");
        fs::set_permissions(leaf.join("file"), fs::Permissions::from_mode(0o640))
            .expect("file mode");
        symlink("file", leaf.join("link")).expect("leaf link");
        leaf
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn metadata() -> MetadataCapabilities {
    MetadataCapabilities {
        symlink_chmod: false,
        symlink_chflags: false,
    }
}
fn descendant(root: &Path, depth: usize) -> PathBuf {
    let mut path = root.to_owned();
    for _ in 0..depth {
        path.push("a");
    }
    path
}

#[test]
fn native_default_allows_root_through_depth_128_with_files_links_and_metadata() {
    assert_eq!(CopyLimits::default().max_depth, 128);
    for depth in [0, 127, 128] {
        for preserve_links in [false, true] {
            for ignore_bytecode in [false, true] {
                let fixture = Fixture::new();
                fixture.tree(depth);
                let destination = fixture.0.join("copied");
                copy_tree(
                    &fixture.0.join("source"),
                    &destination,
                    preserve_links,
                    ignore_bytecode,
                    CopyLimits::default(),
                    metadata(),
                )
                .expect("bounded copy");
                let copied = descendant(&destination, depth);
                assert_eq!(
                    fs::read(copied.join("file")).expect("copied file"),
                    b"native content"
                );
                assert_eq!(
                    fs::read(copied.join("link")).expect("copied link contents"),
                    b"native content"
                );
                assert_eq!(
                    fs::metadata(copied.join("file"))
                        .expect("file metadata")
                        .permissions()
                        .mode()
                        & 0o7777,
                    0o640
                );
                assert_eq!(
                    fs::symlink_metadata(copied.join("link"))
                        .expect("link metadata")
                        .is_symlink(),
                    preserve_links
                );
            }
        }
    }
}

#[test]
fn over_depth_directory_is_rejected_before_destination_creation_in_every_copy_mode() {
    for preserve_links in [false, true] {
        for ignore_bytecode in [false, true] {
            for limits in [
                CopyLimits::default(),
                CopyLimits {
                    max_depth: usize::MAX,
                },
                CopyLimits { max_depth: 2 },
            ] {
                let accepted = limits.max_depth.min(128);
                let fixture = Fixture::new();
                fixture.tree(accepted + 1);
                let destination = fixture.0.join("copied");
                assert!(matches!(
                    copy_tree(
                        &fixture.0.join("source"),
                        &destination,
                        preserve_links,
                        ignore_bytecode,
                        limits,
                        metadata()
                    ),
                    Err(PreparationError::DepthLimit)
                ));
                assert!(descendant(&destination, accepted).is_dir());
                assert!(!descendant(&destination, accepted + 1).exists());
                assert!(!descendant(&destination, accepted).join("file").exists());
            }
        }
    }
}

#[test]
fn zero_limit_allows_root_members_but_refuses_child_directories() {
    let fixture = Fixture::new();
    fixture.tree(0);
    let destination = fixture.0.join("copied");
    copy_tree(
        &fixture.0.join("source"),
        &destination,
        true,
        false,
        CopyLimits { max_depth: 0 },
        metadata(),
    )
    .expect("root-only copy");
    assert_eq!(
        fs::read(destination.join("file")).expect("file"),
        b"native content"
    );
    assert_eq!(
        fs::read_link(destination.join("link")).expect("link"),
        PathBuf::from("file")
    );
    fs::create_dir(fixture.0.join("source/child")).expect("source child");
    let second = fixture.0.join("second");
    assert!(matches!(
        copy_tree(
            &fixture.0.join("source"),
            &second,
            true,
            false,
            CopyLimits { max_depth: 0 },
            metadata()
        ),
        Err(PreparationError::DepthLimit)
    ));
    assert!(!second.join("child").exists());
}

#[test]
fn ignored_bytecode_subtrees_do_not_consume_copy_depth() {
    let fixture = Fixture::new();
    let source_leaf = fixture.tree(128);
    fs::create_dir(source_leaf.join("__pycache__")).expect("ignored directory");
    fs::write(source_leaf.join("__pycache__/generated.pyc"), b"cache").expect("ignored file");
    let destination = fixture.0.join("copied");
    copy_tree(
        &fixture.0.join("source"),
        &destination,
        false,
        true,
        CopyLimits::default(),
        metadata(),
    )
    .expect("ignore prunes before descending");
    assert!(!descendant(&destination, 128).join("__pycache__").exists());
    let second = fixture.0.join("second");
    assert!(matches!(
        copy_tree(
            &fixture.0.join("source"),
            &second,
            true,
            false,
            CopyLimits::default(),
            metadata()
        ),
        Err(PreparationError::DepthLimit)
    ));
    assert!(!descendant(&second, 128).join("__pycache__").exists());
}

#[tokio::test]
async fn blocking_preparation_settles_before_returning_depth_results() {
    for depth in [128, 129] {
        let fixture = Fixture::new();
        fixture.tree(depth);
        fs::create_dir(fixture.0.join("job")).expect("job root");
        let operation = LocalPreparation {
            step_root: fixture.0.join("source"),
            root: fixture.0.join("job"),
            code_dir: fixture.0.join("copied"),
            mounts: vec![],
            copy: true,
            limits: CopyLimits::default(),
            metadata: metadata(),
        };
        let result = tokio::task::spawn_blocking(move || operation.prepare())
            .await
            .expect("blocking task settled");
        if depth == 128 {
            result.expect("accepted boundary");
            assert_eq!(
                fs::read(descendant(&fixture.0.join("copied"), depth).join("file"))
                    .expect("completed copy"),
                b"native content"
            );
        } else {
            assert!(matches!(result, Err(PreparationError::DepthLimit)));
            assert!(!descendant(&fixture.0.join("copied"), depth).exists());
        }
        assert!(fixture.0.join("job/tmp").is_dir());
    }
}
