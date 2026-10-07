#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)] // Fixture assertions.
use cannery_storage::{create_store, presigning};
use std::collections::BTreeMap;
#[tokio::test]
async fn factory_owns_backends_and_rejects_out_of_range_native_settings() {
    let root = std::env::temp_dir().join(format!("cannery-factory-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let mut environment = BTreeMap::from([
        ("CANNERY_DATABASE_URL".to_owned(), "unused".to_owned()),
        ("CANNERY_STORAGE_BACKEND".to_owned(), "local".to_owned()),
        (
            "CANNERY_STORAGE_LOCAL_ROOT".to_owned(),
            root.to_str().unwrap().to_owned(),
        ),
        ("CANNERY_STORAGE_BUCKET".to_owned(), "fixture".to_owned()),
    ]);
    let settings = cannery_core::settings::load_settings(None, &environment).unwrap();
    let store = create_store(&settings.storage).await.unwrap();
    assert_eq!(store.backend(), "local");
    assert_eq!(store.bucket(), "fixture");
    assert!(presigning(&store).is_none());
    let record = store
        .write_new(
            "key",
            futures_util::stream::iter(vec![Ok(b"hello".to_vec())]),
            num_bigint::BigInt::from(i64::MAX),
        )
        .await
        .unwrap();
    assert_eq!(store.head("key").await.unwrap().unwrap().size_bytes, 5);
    assert_eq!(store.stat("key").await.unwrap().unwrap(), record);
    let mut reader = store.read("key").await.unwrap();
    assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"hello");
    assert!(reader.next_chunk().await.unwrap().is_none());
    store
        .delete("key", record.generation.as_deref())
        .await
        .unwrap();
    assert_eq!(
        store
            .sweep_staging(0.0, std::time::SystemTime::now())
            .await
            .unwrap(),
        0
    );
    environment.insert("CANNERY_STORAGE_BACKEND".to_owned(), "s3".to_owned());
    environment.insert(
        "CANNERY_STORAGE_S3_ENDPOINT".to_owned(),
        "https://s3.example.org".to_owned(),
    );
    environment.insert(
        "CANNERY_STORAGE_S3_ACCESS_KEY_ID".to_owned(),
        "GKtest".to_owned(),
    );
    environment.insert(
        "CANNERY_STORAGE_S3_SECRET_ACCESS_KEY".to_owned(),
        "fixture-signing-only".to_owned(),
    );
    environment.insert(
        "CANNERY_STORAGE_UPLOAD_TTL_MINUTES".to_owned(),
        "1".to_owned() + &"0".repeat(100),
    );
    assert!(cannery_core::settings::load_settings(None, &environment).is_err());
    environment.insert(
        "CANNERY_STORAGE_UPLOAD_TTL_MINUTES".to_owned(),
        "15".to_owned(),
    );
    let settings = cannery_core::settings::load_settings(None, &environment).unwrap();
    let store = create_store(&settings.storage).await.unwrap();
    assert_eq!(store.backend(), "s3");
    let signing = presigning(&store).unwrap();
    assert_eq!(signing.transfer_for(256 << 20), "single");
    assert_eq!(
        signing.transfer_for(num_bigint::BigInt::from((256_u64 << 20) + 1)),
        "multipart"
    );
    std::fs::remove_dir_all(root).unwrap();
}
