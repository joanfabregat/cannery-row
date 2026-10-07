//! Manual provider gate; no cloud access in ordinary CI.
use aws_sdk_s3::config::Credentials;
use cannery_storage::s3::{PresignedRequest, S3Options, S3Store, UploadedPart, sha256_base64};
use reqwest::{Client, Response, Url};
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    time::{Duration, SystemTime},
};

type Result<T> = std::result::Result<T, &'static str>;
const PART: usize = 5 * 1024 * 1024;
const OP_TIMEOUT: Duration = Duration::from_secs(40);

fn validate(endpoint: &str, bucket: &str, consent: &str) -> Result<Url> {
    let account = endpoint
        .strip_prefix("https://")
        .and_then(|s| s.strip_suffix(".r2.cloudflarestorage.com"))
        .ok_or("endpoint must be exact HTTPS R2 account origin")?;
    if account.len() != 32 || !account.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("endpoint must be exact HTTPS R2 account origin");
    }
    if !(23..=63).contains(&bucket.len())
        || !bucket.starts_with("cannery-rust-acceptance")
        || !bucket
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        || bucket.ends_with('-')
    {
        return Err("dedicated acceptance bucket required");
    }
    if consent != "I_ACCEPT_DEDICATED_R2_TEST_WRITES" {
        return Err("explicit R2 acceptance opt-in required");
    }
    Url::parse(endpoint).map_err(|_| "invalid endpoint")
}

async fn bounded<T>(
    work: impl Future<Output = std::result::Result<T, cannery_storage::Error>>,
) -> Result<T> {
    tokio::time::timeout(OP_TIMEOUT, work)
        .await
        .map_err(|_| "storage operation deadline exceeded")?
        .map_err(|_| "storage operation failed (provider details redacted)")
}
fn require(condition: bool, error: &'static str) -> Result<()> {
    if condition { Ok(()) } else { Err(error) }
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

struct Browser {
    endpoint: Url,
    client: Client,
    bucket: String,
    prefix: String,
}
impl Browser {
    fn url(&self, raw: &str, key: &str) -> Result<Url> {
        let url = Url::parse(raw).map_err(|_| "invalid presigned URL")?;
        require(
            url.origin() == self.endpoint.origin()
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none()
                && url.path() == format!("/{}/{}{}", self.bucket, self.prefix, key),
            "presigned URL escaped exact test object",
        )?;
        Ok(url)
    }
    async fn put(&self, key: &str, plan: &PresignedRequest, bytes: Vec<u8>) -> Result<Response> {
        require(
            plan.method == "PUT" && bytes.len() <= PART,
            "invalid bounded PUT plan",
        )?;
        let mut request = self.client.put(self.url(&plan.url, key)?).body(bytes);
        for (name, value) in &plan.headers {
            require(
                matches!(
                    name.to_ascii_lowercase().as_str(),
                    "content-length" | "x-amz-checksum-sha256"
                ),
                "unexpected presigned header",
            )?;
            request = request.header(name, value);
        }
        request
            .send()
            .await
            .map_err(|_| "PUT transport failed (URL redacted)")
    }
    async fn download(&self, store: &S3Store, key: &str, expected: &[u8]) -> Result<()> {
        let raw = bounded(store.presign_get(
            key,
            "acceptance.bin",
            "application/octet-stream",
            120_u64,
            SystemTime::now(),
        ))
        .await?;
        let mut response = self
            .client
            .get(self.url(&raw, key)?)
            .send()
            .await
            .map_err(|_| "GET transport failed (URL redacted)")?;
        require(response.status().as_u16() == 200, "GET status unexpected")?;
        require(
            response
                .headers()
                .get("content-type")
                .is_some_and(|v| v == "application/octet-stream")
                && response
                    .headers()
                    .get("content-disposition")
                    .is_some_and(|v| v == "attachment; filename=\"acceptance.bin\""),
            "GET metadata differed",
        )?;
        require(
            response.content_length() == Some(expected.len() as u64),
            "GET length differed",
        )?;
        let mut offset = 0;
        while let Some(chunk) = response.chunk().await.map_err(|_| "GET body failed")? {
            let end = offset + chunk.len();
            require(
                end <= expected.len() && expected.get(offset..end) == Some(chunk.as_ref()),
                "GET bytes differed or exceeded bound",
            )?;
            offset = end;
        }
        require(offset == expected.len(), "GET body truncated")
    }
}

// Ledger is fixed: only these unique keys and the returned ID may be cleaned.
#[derive(Default)]
struct Ledger {
    upload: Option<String>,
    mutation_uncertain: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CleanupAction<'a> {
    Abort(&'a str),
    Delete(&'static str),
}
impl Ledger {
    async fn cleanup(&self, store: &S3Store) -> Result<()> {
        self.cleanup_with(|action| async move {
            match action {
                CleanupAction::Abort(id) => bounded(store.abort_multipart("multipart", id)).await,
                CleanupAction::Delete(key) => bounded(store.delete(key, None)).await,
            }
        })
        .await
    }
    async fn cleanup_with<'a, F, Fut>(&'a self, mut execute: F) -> Result<()>
    where
        F: FnMut(CleanupAction<'a>) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        // Do not short-circuit: every cleanup action runs even if another fails.
        let mut clean = !self.mutation_uncertain;
        if let Some(id) = &self.upload {
            clean &= execute(CleanupAction::Abort(id)).await.is_ok();
        }
        for key in ["single", "multipart"] {
            clean &= execute(CleanupAction::Delete(key)).await.is_ok();
        }
        require(
            clean,
            "cleanup incomplete or mutation acknowledgement uncertain",
        )
    }
}
async fn exercise(store: &S3Store, browser: &Browser, ledger: &mut Ledger) -> Result<()> {
    single(store, browser, ledger).await?;
    multipart(store, browser, ledger).await
}
async fn single(store: &S3Store, browser: &Browser, ledger: &mut Ledger) -> Result<()> {
    let bytes = b"R2 SHA256 acceptance: correct bytes\n".to_vec();
    let digest = sha(&bytes);
    let plan = bounded(store.presign_put(
        "single",
        bytes.len() as u64,
        &digest,
        120_u64,
        SystemTime::now(),
    ))
    .await?;
    require(
        plan.headers.get("x-amz-checksum-sha256")
            == Some(&sha256_base64(&digest).map_err(|_| "checksum encoding failed")?),
        "PUT checksum header missing",
    )?;
    let mut wrong = bytes.clone();
    wrong[0] ^= 1;
    ledger.mutation_uncertain = true;
    let rejected = browser.put("single", &plan, wrong).await?;
    ledger.mutation_uncertain = false;
    // Authentication failures alone do not prove checksum rejection: same URL must succeed next.
    require(
        rejected.status().as_u16() == 400,
        "wrong same-length bytes were not rejected with 400",
    )?;
    drop(rejected); // Provider error bodies are never read or printed.
    ledger.mutation_uncertain = true;
    let accepted = browser.put("single", &plan, bytes.clone()).await?;
    ledger.mutation_uncertain = false;
    require(
        accepted.status().as_u16() == 200,
        "correct checksum PUT failed",
    )?;
    drop(accepted);
    let head = bounded(store.head("single"))
        .await?
        .ok_or("single HEAD missing")?;
    require(
        head.size_bytes == bytes.len() as u64 && head.sha256.as_deref() == Some(&digest),
        "single HEAD checksum/length differed",
    )?;
    browser.download(store, "single", &bytes).await?;
    println!(
        "R2 single: wrong_same_length=400 correct=200 sha256_head=exact get_bytes=exact get_metadata=exact"
    );
    Ok(())
}
async fn multipart(store: &S3Store, browser: &Browser, ledger: &mut Ledger) -> Result<()> {
    ledger.mutation_uncertain = true;
    ledger.upload = Some(
        bounded(Box::pin(
            store.create_multipart("multipart", "application/octet-stream"),
        ))
        .await?,
    );
    ledger.mutation_uncertain = false;
    let id = ledger.upload.as_deref().ok_or("upload ID missing")?;
    let first = vec![0x61; PART];
    let second = b"last R2 acceptance part\n".to_vec();
    let mut parts = Vec::new();
    for (number, bytes) in [(1, &first), (2, &second)] {
        let plan = bounded(store.presign_part(
            "multipart",
            id,
            number,
            bytes.len() as u64,
            120_u64,
            SystemTime::now(),
        ))
        .await?;
        ledger.mutation_uncertain = true;
        let response = browser.put("multipart", &plan, bytes.clone()).await?;
        ledger.mutation_uncertain = false;
        require(response.status().as_u16() == 200, "multipart PUT failed")?;
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .filter(|v| v.len() <= 128)
            .ok_or("multipart ETag missing or oversized")?
            .to_owned();
        parts.push(UploadedPart {
            number,
            size_bytes: i64::try_from(bytes.len()).map_err(|_| "part length out of range")?,
            etag,
        });
    }
    ledger.mutation_uncertain = true;
    let completed = bounded(store.complete_multipart("multipart", id, &parts)).await?;
    ledger.mutation_uncertain = false;
    require(completed, "multipart completion missing")?;
    ledger.upload = None;
    let mut expected = first;
    expected.extend_from_slice(&second);
    let head = bounded(store.head("multipart"))
        .await?
        .ok_or("multipart HEAD missing")?;
    require(
        head.size_bytes == expected.len() as u64,
        "multipart HEAD length differed",
    )?;
    browser.download(store, "multipart", &expected).await?;
    println!(
        "R2 multipart: parts=2 complete=true head_length=exact get_bytes=exact get_metadata=exact"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "manual live R2 provider gate; requires dedicated bucket, RAM env file and opt-in"]
async fn live_r2_checksum_and_multipart() -> Result<()> {
    let env = |name| std::env::var(name).map_err(|_| "required acceptance environment missing");
    let endpoint = env("CANNERY_R2_ENDPOINT")?;
    let bucket = env("CANNERY_R2_BUCKET")?;
    let origin = validate(&endpoint, &bucket, &env("CANNERY_R2_ACCEPT")?)?;
    // No logger is installed; reject ambient transport/debug configuration.
    for name in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "RUST_LOG",
        "AWS_ENDPOINT_URL",
        "AWS_ENDPOINT_URL_S3",
    ] {
        require(
            std::env::var_os(name).is_none(),
            "ambient proxy/debug/endpoint override forbidden",
        )?;
    }
    let access = env("CANNERY_R2_ACCESS_KEY_ID")?;
    let secret = env("CANNERY_R2_SECRET_ACCESS_KEY")?;
    require(
        !access.is_empty() && !secret.is_empty(),
        "empty acceptance credentials",
    )?;
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(|_| "prefix randomness failed")?;
    let run = uuid::Uuid::from_bytes(random);
    let prefix = format!("acceptance/{run}/");
    println!("R2 acceptance run={run}; exact keys=single,multipart; no bucket enumeration");
    let store = S3Store::new(S3Options {
        endpoint: Some(endpoint),
        public_endpoint: None,
        region: "auto".into(),
        credentials: Credentials::new(access, secret, None, None, "manual-r2-acceptance"),
        path_style: true,
        bucket: bucket.clone(),
        prefix: prefix.clone(),
        presign_ttl: 120,
        multipart_threshold: PART as u64,
        part_size: PART as u64,
        upload_ttl: 120,
    });
    let browser = Browser {
        endpoint: origin,
        bucket,
        prefix,
        client: Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(OP_TIMEOUT)
            .build()
            .map_err(|_| "HTTP client initialization failed")?,
    };
    let mut ledger = Ledger::default();
    let result = tokio::time::timeout(
        Duration::from_secs(240),
        Box::pin(exercise(&store, &browser, &mut ledger)),
    )
    .await
    .unwrap_or(Err("acceptance workflow deadline exceeded"));
    let cleanup = ledger.cleanup(&store).await;
    println!(
        "R2 acceptance result={} cleanup={}",
        if result.is_ok() { "PASS" } else { "FAIL" },
        if cleanup.is_ok() {
            "PASS"
        } else {
            "INCOMPLETE"
        }
    );
    cleanup?;
    result
}

#[test]
fn rejects_non_dedicated_destinations_and_missing_consent() {
    let good = "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com";
    let consent = "I_ACCEPT_DEDICATED_R2_TEST_WRITES";
    assert!(validate(good, "cannery-rust-acceptance-manual", consent).is_ok());
    for endpoint in [
        "http://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com",
        "https://r2.cloudflarestorage.com",
        "https://example.com",
        "https://user@0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com",
        "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com:443",
        "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com/",
        "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com?secret=1",
    ] {
        assert!(validate(endpoint, "cannery-rust-acceptance-manual", consent).is_err());
    }
    for bucket in [
        "production",
        "cannery-rust-acceptance/other",
        "cannery-rust-acceptance-",
        "cannery-rust-acceptance.UPPER",
    ] {
        assert!(validate(good, bucket, consent).is_err());
    }
    assert!(validate(good, "cannery-rust-acceptance-manual", "").is_err());
}

#[tokio::test]
async fn cleanup_runs_all_exact_actions_after_abort_failure_and_marks_uncertainty() {
    let ledger = Ledger {
        upload: Some("only-created-upload".into()),
        mutation_uncertain: false,
    };
    let mut actions = Vec::new();
    let result = ledger
        .cleanup_with(|action| {
            actions.push(action);
            std::future::ready(if matches!(action, CleanupAction::Abort(_)) {
                Err("simulated abort failure")
            } else {
                Ok(())
            })
        })
        .await;
    assert!(result.is_err());
    assert_eq!(
        actions,
        [
            CleanupAction::Abort("only-created-upload"),
            CleanupAction::Delete("single"),
            CleanupAction::Delete("multipart")
        ]
    );
    let uncertain = Ledger {
        upload: None,
        mutation_uncertain: true,
    };
    let mut actions = Vec::new();
    assert!(
        uncertain
            .cleanup_with(|action| {
                actions.push(action);
                std::future::ready(Ok(()))
            })
            .await
            .is_err()
    );
    assert_eq!(
        actions,
        [
            CleanupAction::Delete("single"),
            CleanupAction::Delete("multipart")
        ]
    );
}

#[tokio::test]
async fn deadline_during_mutation_cleans_exact_keys_but_cannot_claim_cleanup() {
    let mut ledger = Ledger::default();
    let interrupted = tokio::time::timeout(Duration::from_millis(1), async {
        ledger.mutation_uncertain = true;
        std::future::pending::<()>().await;
        ledger.mutation_uncertain = false;
    })
    .await;
    assert!(interrupted.is_err());
    let mut actions = Vec::new();
    assert!(
        ledger
            .cleanup_with(|action| {
                actions.push(action);
                std::future::ready(Ok(()))
            })
            .await
            .is_err()
    );
    assert_eq!(
        actions,
        [
            CleanupAction::Delete("single"),
            CleanupAction::Delete("multipart")
        ]
    );
}
