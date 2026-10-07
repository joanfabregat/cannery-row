#![forbid(unsafe_code)]
use aws_sdk_s3::config::Credentials;
use cannery_storage::{
    Error, StoredObject,
    local::LocalStore,
    s3::{S3Options, S3Store, UploadedPart},
};
use futures_util::stream;
use serde_json::{Value, json};
use std::time::{Duration, SystemTime};
fn chunks(data: &[u8]) -> impl futures_util::Stream<Item = Result<Vec<u8>, Error>> + Unpin {
    stream::iter(
        data.chunks(1 << 20)
            .map(|x| Ok(x.to_vec()))
            .collect::<Vec<_>>(),
    )
}
fn stored(value: &StoredObject) -> Value {
    json!({"size":value.size_bytes,"sha256":value.sha256,"generation":value.generation})
}
fn options(endpoint: &str, credentials: Credentials) -> S3Options {
    S3Options {
        endpoint: Some(endpoint.to_owned()),
        public_endpoint: None,
        region: "garage".to_owned(),
        credentials,
        path_style: true,
        bucket: "cannery-conformance".to_owned(),
        prefix: "proof/".to_owned(),
        presign_ttl: 900,
        multipart_threshold: 256 << 20,
        part_size: 64 << 20,
        upload_ttl: 3600,
    }
}
#[allow(clippy::too_many_lines)] // One bounded ordered source/native scenario.
async fn run() -> Result<Value, Box<dyn std::error::Error>> {
    let endpoint = std::env::var("CANNERY_TEST_S3_ENDPOINT")?;
    if !matches!(
        endpoint.as_str(),
        "http://garage:3900" | "http://wire:3900" | "http://127.0.0.1:3904"
    ) {
        return Err("fixture endpoint required".into());
    }
    let key = std::env::var("CANNERY_TEST_S3_ACCESS_KEY_ID")?;
    let secret = std::env::var("CANNERY_TEST_S3_SECRET_ACCESS_KEY")?;
    let store = S3Store::new(options(
        &endpoint,
        Credentials::new(key, secret, None, None, "isolated-fixture"),
    ));
    store
        .sweep_staging(0.0, SystemTime::now() + Duration::from_hours(48))
        .await?;
    let mut observations = Vec::new();
    for size in [0usize, 5, (8 << 20) - 1, 8 << 20, (9 << 20) + 3] {
        let data = vec![b'x'; size];
        let key = format!("data-{size}");
        store.delete(&key, None).await?;
        let first = store.write_new(&key, chunks(&data), size as i128).await?;
        let head = store.head(&key).await?.ok_or("missing")?;
        let stat = store.stat(&key).await?.ok_or("missing")?;
        let mut reader = store.read(&key).await?;
        let mut read = Vec::new();
        let mut lengths = Vec::new();
        while let Some(chunk) = reader.next_chunk().await? {
            lengths.push(chunk.len());
            read.extend_from_slice(&chunk);
        }
        let duplicate = matches!(
            store.write_new(&key, chunks(b"different"), 99).await,
            Err(Error::ObjectExists)
        );
        store.delete(&key, Some("wrong")).await?;
        let spared = store.head(&key).await?.is_some();
        store.delete(&key, first.generation.as_deref()).await?;
        observations.push(json!({"write":stored(&first),"stat":stored(&stat),"head":{"size":head.size_bytes,"generation":head.generation,"sha256":head.sha256},"read_exact":read==data,"chunks":lengths,"duplicate":duplicate,"spared":spared,"deleted":store.head(&key).await?.is_none()}));
    }
    let over = matches!(
        store
            .write_new("over", chunks(&vec![b'x'; 9 << 20]), (9 << 20) - 1)
            .await,
        Err(Error::ObjectTooLarge)
    );
    let missing_head = store.head("missing").await?.is_none();
    let missing_stat = store.stat("missing").await?.is_none();
    let read_absent = store
        .read("missing")
        .await
        .err()
        .and_then(|e| e.code().map(str::to_owned));
    let upload = store
        .create_multipart("manual", "application/octet-stream")
        .await?;
    let empty = store
        .list_parts("manual", &upload)
        .await?
        .is_some_and(|p| p.is_empty());
    let rejected = store
        .complete_multipart(
            "manual",
            &upload,
            &[UploadedPart {
                number: 1,
                size_bytes: 5,
                etag: "wrong".to_owned(),
            }],
        )
        .await
        .err()
        .map(|error| match error {
            Error::PartsRejected(code) => code,
            other => other.code().unwrap_or_default().to_owned(),
        });
    store.abort_multipart("manual", &upload).await?;
    store.abort_multipart("manual", &upload).await?;
    let absent = store.list_parts("manual", &upload).await?.is_none();
    let absent_complete = !store.complete_multipart("manual", &upload, &[]).await?;
    store
        .create_multipart("stale", "application/octet-stream")
        .await?;
    let fresh = store.sweep_staging(0.0, SystemTime::now()).await?;
    let swept = store
        .sweep_staging(0.0, SystemTime::now() + Duration::from_hours(48))
        .await?;
    let signer = S3Store::new(options(
        "https://s3.example.org",
        Credentials::new("GKtest", "fixture-signing-only", None, None, "fixture"),
    ));
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let put = signer
        .presign_put(
            "a/雪 space.json",
            5,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
            120,
            now,
        )
        .await?;
    let part = signer
        .presign_part("a/雪 space.json", "fixed-id", 2, 5, 120, now)
        .await?;
    let get = signer
        .presign_get(
            "a/雪 space.json",
            "quote\"雪.json",
            "application/json",
            120,
            now,
        )
        .await?;
    let signed_requests = json!({"put":{"url":put.url,"headers":put.headers},"part":{"url":part.url,"headers":part.headers},"get":get});
    let local_root = std::env::var("CANNERY_STORAGE_LOCAL_ROOT")?;
    let local = LocalStore::new(&local_root, "native")?;
    let local_data = vec![b'y'; (1 << 20) + 17];
    let local_record = local
        .write_new("nested/data", chunks(&local_data), local_data.len() as i128)
        .await?;
    let local_head = local.head("nested/data").await?.ok_or("missing")?;
    let local_stat = local.stat("nested/data").await?.ok_or("missing")?;
    let mut reader = local.read("nested/data").await?;
    let mut download_bytes = Vec::new();
    let mut local_chunks = Vec::new();
    while let Some(chunk) = reader.next_chunk().await? {
        local_chunks.push(chunk.len());
        download_bytes.extend_from_slice(&chunk);
    }
    local.delete("nested/data", Some("wrong")).await?;
    let local_spared = local.head("nested/data").await?.is_some();
    local
        .delete("nested/data", local_record.generation.as_deref())
        .await?;
    let local_empty = local.sweep_staging(0.0).await?;
    let mut invalid = Vec::new();
    for key in ["", "../escape", ".hidden/file", "/escape"] {
        invalid.push(matches!(local.head(key).await, Err(Error::InvalidKey)));
    }
    Ok(
        json!({"objects":observations,"over":over,"missing":[missing_head,missing_stat,read_absent],"multipart":{"empty":empty,"rejected":rejected,"absent":absent,"absent_complete":absent_complete,"fresh":fresh,"swept":swept},"signed":signed_requests,"local":{"size":local_record.size_bytes,"sha256":local_record.sha256,"stat_same":local_stat==local_record,"head_same":local_head.generation==local_record.generation&&local_head.size_bytes==local_record.size_bytes&&local_head.sha256.is_none(),"read_exact":download_bytes==local_data,"chunks":local_chunks,"spared":local_spared,"deleted":local.head("nested/data").await?.is_none(),"sweep":local_empty,"invalid":invalid}}),
    )
}
#[tokio::main]
async fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    let result = if arguments.len() > 1 {
        transfer_fixture(&arguments).await
    } else {
        run().await
    };
    match result {
        Ok(value) => println!("{value}"),
        Err(error) => {
            eprintln!("storage reference failed: {error}");
            std::process::exit(1);
        }
    }
}

async fn transfer_fixture(arguments: &[String]) -> Result<Value, Box<dyn std::error::Error>> {
    let endpoint = std::env::var("CANNERY_TEST_S3_ENDPOINT")?;
    if !matches!(
        endpoint.as_str(),
        "http://wire:3900" | "http://127.0.0.1:3904"
    ) {
        return Err("owned fixture required".into());
    }
    let credentials = Credentials::new(
        std::env::var("CANNERY_TEST_S3_ACCESS_KEY_ID")?,
        std::env::var("CANNERY_TEST_S3_SECRET_ACCESS_KEY")?,
        None,
        None,
        "fixture",
    );
    let store = S3Store::new(options(&endpoint, credentials));
    if arguments[1] == "--finish-transfer" {
        let id = arguments.get(2).ok_or("missing id")?;
        let etag = arguments.get(3).ok_or("missing etag")?;
        let completed = store
            .complete_multipart(
                "presigned-multipart",
                id,
                &[UploadedPart {
                    number: 1,
                    size_bytes: 5,
                    etag: etag.clone(),
                }],
            )
            .await?;
        let stat = store
            .stat("presigned-multipart")
            .await?
            .ok_or("missing multipart")?;
        store.delete("presigned-single", None).await?;
        store.delete("presigned-multipart", None).await?;
        return Ok(json!({"completed":completed,"record":stored(&stat)}));
    }
    store.delete("presigned-single", None).await?;
    let id = store
        .create_multipart("presigned-multipart", "application/octet-stream")
        .await?;
    let now = SystemTime::now();
    let put = store
        .presign_put(
            "presigned-single",
            5,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
            120,
            now,
        )
        .await?;
    let part = store
        .presign_part("presigned-multipart", &id, 1, 5, 120, now)
        .await?;
    let get = store
        .presign_get(
            "presigned-single",
            "fixed.json",
            "application/json",
            120,
            now,
        )
        .await?;
    Ok(
        json!({"put":{"url":put.url,"headers":put.headers},"part":{"url":part.url,"headers":part.headers},"get":get,"id":id}),
    )
}
