#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::panic)] // Test assertions.
use cannery_storage::{Error, local::LocalStore};
use futures_util::stream;
use std::{
    fs,
    os::unix::fs::{MetadataExt, symlink},
    time::{Duration, SystemTime},
};
#[tokio::test]
async fn filesystem_integrity_and_create_only_race() {
    let root = std::env::temp_dir().join(format!("cannery-local-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let store = LocalStore::new(&root, "bucket").unwrap();
    let data = vec![42; 2 * (1 << 20) + 3];
    let (first, second) = tokio::join!(
        store.write_new(
            "race",
            stream::iter(vec![Ok(data.clone())]),
            data.len() as i128
        ),
        store.write_new(
            "race",
            stream::iter(vec![Ok(data.clone())]),
            data.len() as i128
        )
    );
    let record = match (first, second) {
        (Ok(v), Err(Error::ObjectExists)) | (Err(Error::ObjectExists), Ok(v)) => v,
        other => panic!("unexpected race result {other:?}"),
    };
    let metadata = fs::metadata(root.join("bucket/race")).unwrap();
    assert_eq!(
        record.generation,
        Some(format!(
            "{}:{}",
            metadata.ino(),
            i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec())
        ))
    );
    assert_eq!(fs::read(root.join("bucket/race")).unwrap(), data);
    assert!(
        fs::read_dir(root.join("bucket/.staging"))
            .unwrap()
            .next()
            .is_none()
    );
    assert!(matches!(
        store
            .write_new("over", stream::iter(vec![Ok(vec![1; 10])]), 9)
            .await,
        Err(Error::ObjectTooLarge)
    ));
    assert!(store.head("over").await.unwrap().is_none());
    assert!(matches!(
        store
            .write_new(
                "broken",
                stream::iter(vec![Ok(vec![1]), Err(Error::Worker)]),
                10
            )
            .await,
        Err(Error::Worker)
    ));
    assert!(store.head("broken").await.unwrap().is_none());
    fs::create_dir_all(root.join("outside")).unwrap();
    symlink(root.join("outside"), root.join("bucket/link")).unwrap();
    assert!(matches!(
        store.head("link/file").await,
        Err(Error::InvalidKey)
    ));
    symlink(root.join("bucket/race"), root.join("bucket/inside")).unwrap();
    assert_eq!(
        store.head("inside").await.unwrap().unwrap().generation,
        record.generation
    );
    store
        .delete("inside", record.generation.as_deref())
        .await
        .unwrap();
    assert!(store.head("race").await.unwrap().is_none());
    assert!(fs::symlink_metadata(root.join("bucket/inside")).is_ok());
    let old = root.join("bucket/.staging/old");
    let file = fs::File::create(&old).unwrap();
    file.set_times(
        fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
    )
    .unwrap();
    drop(file);
    fs::create_dir(root.join("bucket/.staging/dir")).unwrap();
    symlink(&old, root.join("bucket/.staging/alias")).unwrap();
    let removed = store.sweep_staging(0.0).await.unwrap();
    assert!((1..=2).contains(&removed));
    assert!(root.join("bucket/.staging/dir").is_dir());
    for key in ["", "../outside/file", ".staging/file", "\0"] {
        assert!(matches!(store.head(key).await, Err(Error::InvalidKey)));
    }
    fs::remove_dir_all(root).unwrap();
}
