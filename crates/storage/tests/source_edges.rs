#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::panic)] // Oracle assertions.
#[macro_use]
#[path = "../../../tests/support/runtime_reference.rs"]
mod runtime_reference;
use aws_sdk_s3::config::Credentials;
use cannery_storage::{
    Error,
    local::LocalStore,
    s3::{S3Options, S3Store, part_count, part_length, sha256_base64, sha256_hex},
};
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use serde_json::Value;
use std::{
    str::FromStr,
    time::{Duration, SystemTime},
};
fn integer(value: &Value, key: &str) -> BigInt {
    BigInt::from_str(value[key].as_str().unwrap()).unwrap()
}
async fn filesystem_edges(root: &std::path::Path, local: &LocalStore, reference: &Value) {
    let bucket = root.join("bucket");
    std::fs::create_dir(&bucket).unwrap();
    std::fs::write(bucket.join("end"), b"long-chain").unwrap();
    for number in 0..80 {
        let target = if number < 79 {
            format!("link{}", number + 1)
        } else {
            "end".to_owned()
        };
        std::os::unix::fs::symlink(target, bucket.join(format!("link{number}"))).unwrap();
    }
    assert_eq!(
        local.head("link0").await.unwrap().unwrap().size_bytes,
        reference["filesystem"]["long_chain_size"].as_u64().unwrap()
    );
    std::os::unix::fs::symlink("cycle", bucket.join("cycle")).unwrap();
    assert!(local.stat("cycle").await.unwrap().is_none());
    assert!(matches!(local.head("cycle").await, Err(Error::Io(_))));
    assert_eq!(
        reference["filesystem"]["cycle_head_errno"],
        nix::libc::ELOOP
    );
    local.delete("cycle", None).await.unwrap();
    assert!(std::fs::symlink_metadata(bucket.join("cycle")).is_err());
    for text in [r#""\udc80""#, r#""\ud800""#] {
        assert!(serde_json::from_str::<Value>(text).is_err());
    }
}
#[tokio::test]
async fn supported_storage_edges_and_native_signing_limits() {
    let reference: Value = serde_json::from_str(runtime_reference!(
        "/../../crates/storage/tests/fixtures/edges-reference.json"
    ))
    .unwrap();
    let root = std::env::temp_dir().join(format!("cannery-edges-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let local = LocalStore::new(&root, "bucket").unwrap();
    for case in reference["paths"].as_array().unwrap() {
        let result = match local.head(case["key"].as_str().unwrap()).await {
            Ok(None) => "None",
            Err(Error::InvalidKey) => "InvalidKey",
            other => panic!("unexpected path {other:?}"),
        };
        assert_eq!(result, case["result"]);
    }
    filesystem_edges(&root, &local, &reference).await;
    for case in reference["counts"].as_array().unwrap() {
        let result = match part_count(&integer(case, "size"), &integer(case, "part")) {
            Ok(n) => n.to_string(),
            Err(Error::IntegerRange) => "ZeroDivisionError".to_owned(),
            other => panic!("unexpected count {other:?}"),
        };
        assert_eq!(result, case["result"]);
    }
    for case in reference["lengths"].as_array().unwrap() {
        assert_eq!(
            part_length(
                &integer(case, "size"),
                &integer(case, "part"),
                &integer(case, "number")
            )
            .to_string(),
            case["result"]
        );
    }
    for case in reference["encode"].as_array().unwrap() {
        let result = match sha256_base64(case["input"].as_str().unwrap()) {
            Ok(s) => s,
            Err(Error::InvalidChecksum) => "InvalidChecksum".to_owned(),
            other => panic!("unexpected encode {other:?}"),
        };
        assert_eq!(result, case["result"]);
    }
    for case in reference["decode"].as_array().unwrap() {
        assert_eq!(
            serde_json::to_value(sha256_hex(case["input"].as_str().unwrap())).unwrap(),
            case["result"]
        );
    }
    let store = S3Store::new(S3Options {
        endpoint: Some("https://s3.example.org".to_owned()),
        public_endpoint: None,
        region: "garage".to_owned(),
        credentials: Credentials::new("GKtest", "fixture-signing-only", None, None, "fixture"),
        path_style: true,
        bucket: "fixture".to_owned(),
        prefix: String::new(),
        presign_ttl: 900,
        multipart_threshold: 256 << 20,
        part_size: 64 << 20,
        upload_ttl: 3600,
    });
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    supported_presigning(&store, now, &reference).await;
    native_signing_limits(&store, now).await;
    std::fs::remove_dir_all(root).unwrap();
}

async fn supported_presigning(store: &S3Store, now: SystemTime, reference: &Value) {
    for case in reference["expiry"].as_array().unwrap() {
        let expiry = integer(case, "seconds");
        let result = store
            .presign_part("key", "id", 1, 5, expiry.clone(), now)
            .await;
        if !expiry
            .to_u64()
            .is_some_and(|value| (1..=604_800).contains(&value))
        {
            assert!(
                matches!(result, Err(Error::PresignExpiry)),
                "expiry {expiry}"
            );
            continue;
        }
        let actual = result.unwrap();
        assert_eq!(actual.url, case["url"]);
        assert_eq!(
            serde_json::to_value(actual.headers).unwrap(),
            case["headers"]
        );
    }
    for case in reference["scalars"].as_array().unwrap() {
        let number = integer(case, "number");
        let size = integer(case, "size");
        let result = store
            .presign_part("key", "id", number.clone(), size.clone(), 1, now)
            .await;
        if number
            .to_i32()
            .is_none_or(|value| !(1..=10_000).contains(&value))
            || size.to_i64().is_none_or(|value| value < 0)
        {
            assert!(
                matches!(result, Err(Error::IntegerRange)),
                "part {number}, size {size}"
            );
            continue;
        }
        let actual = result.unwrap();
        assert_eq!(actual.url, case["result"]["url"]);
        assert_eq!(
            serde_json::to_value(actual.headers).unwrap(),
            case["result"]["headers"]
        );
    }
}

async fn native_signing_limits(store: &S3Store, now: SystemTime) {
    for expiry in [0, -1, 604_801] {
        assert!(matches!(
            store
                .presign_get("key", "file", "text/plain", expiry, now)
                .await,
            Err(Error::PresignExpiry)
        ));
        assert!(matches!(
            store
                .presign_put("key", 0, &"0".repeat(64), expiry, now)
                .await,
            Err(Error::PresignExpiry)
        ));
    }
    for expiry in [1, 604_800] {
        let request = store
            .presign_part("key", "id", 10_000, i64::MAX, expiry, now)
            .await
            .unwrap();
        let parsed = url::Url::parse(&request.url).unwrap();
        assert!(
            parsed
                .query_pairs()
                .any(|(name, value)| name == "X-Amz-Expires" && value == expiry.to_string())
        );
        assert_eq!(request.headers["Content-Length"], i64::MAX.to_string());
    }
    for (part, size) in [(0, 1), (10_001, 1), (1, -1)] {
        assert!(matches!(
            store.presign_part("key", "id", part, size, 1, now).await,
            Err(Error::IntegerRange)
        ));
    }
    assert!(matches!(
        store
            .presign_put("key", BigInt::from(i64::MAX) + 1, &"0".repeat(64), 1, now)
            .await,
        Err(Error::IntegerRange)
    ));
}
