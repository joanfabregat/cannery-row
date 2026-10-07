//! API and object-store transports use separate clients and explicit headers.
use super::RuntimeError;
use crate::cancellation::CancellationEvent;
use cannery_core::principal::Secret;
use futures_util::stream;
use reqwest::{
    Client, Method, Response, Url,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{future::Future, path::Path, time::Duration};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

#[derive(Clone)]
pub struct ApiClient {
    api: Client,
    bucket: Client,
    root: Url,
}
impl ApiClient {
    /// # Errors
    /// Reject malformed API bases and unsupported protocols before using a token.
    pub fn new(root: &str) -> Result<Self, RuntimeError> {
        let root = Self::check_root(root)?;
        let build = || {
            Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(60))
                .read_timeout(Duration::from_secs(60))
                .build()
                .map_err(|_| RuntimeError::Transport)
        };
        Ok(Self {
            api: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(60))
                .read_timeout(Duration::from_secs(60))
                .timeout(Duration::from_secs(60))
                .build()
                .map_err(|_| RuntimeError::Transport)?,
            bucket: build()?,
            root,
        })
    }
    pub(crate) fn check_root(root: &str) -> Result<Url, RuntimeError> {
        let mut root = Url::parse(root).map_err(|_| RuntimeError::Configuration)?;
        if !matches!(root.scheme(), "http" | "https")
            || root.host_str().is_none()
            || !root.username().is_empty()
            || root.password().is_some()
            || root.query().is_some()
            || root.fragment().is_some()
        {
            return Err(RuntimeError::Configuration);
        }
        if !root.path().ends_with('/') {
            root.set_path(&format!("{}/", root.path()));
        }
        Ok(root)
    }
    fn api_url(&self, path: &str) -> Result<Url, RuntimeError> {
        let url = self.root.join(path).map_err(|_| RuntimeError::Contract)?;
        // A server-provided upload path must never exfiltrate API credentials.
        if url.origin() != self.root.origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(RuntimeError::Contract);
        }
        Ok(url)
    }
    /// # Errors
    /// Header/transport errors are sanitized. No cookies or redirects are enabled.
    pub async fn request(
        &self,
        method: Method,
        path: &str,
        token: &Secret,
        lease: Option<&LeaseHeaders>,
        body: Option<&Value>,
    ) -> Result<Response, RuntimeError> {
        let mut request = self
            .api
            .request(method.clone(), self.api_url(path)?)
            .bearer_auth(token.expose());
        if let Some(lease) = lease {
            request = request.headers(lease.headers()?);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        for attempt in 0..4 {
            let response = request
                .try_clone()
                .ok_or(RuntimeError::Contract)?
                .send()
                .await?;
            if response.status().as_u16() != 503 || method != Method::POST || attempt == 3 {
                return Ok(response);
            }
            tokio::time::sleep(Duration::from_millis(500 << attempt)).await;
        }
        Err(RuntimeError::Transport)
    }
    /// API upload uses ONLY the capability headers returned by the server.
    /// # Errors
    /// Rejects foreign upload origins, unreadable files and transfer failures.
    pub async fn upload_api(
        &self,
        url: &str,
        headers: &Value,
        path: &Path,
        size: u64,
    ) -> Result<Response, RuntimeError> {
        Ok(self
            .bucket
            .put(self.api_url(url)?)
            .headers(header_map(headers)?)
            .header(reqwest::header::CONTENT_LENGTH, size)
            .body(file_body(path, 0, size).await?)
            .send()
            .await?)
    }
    /// # Errors
    /// Send an upload capability without adding the runner's bearer credential.
    pub async fn capability_post(
        &self,
        url: &str,
        headers: &Value,
        body: Option<&Value>,
    ) -> Result<Response, RuntimeError> {
        let mut request = self
            .api
            .post(self.api_url(url)?)
            .headers(header_map(headers)?);
        if let Some(body) = body {
            request = request.json(body);
        }
        for attempt in 0..4 {
            let retry = request.try_clone().ok_or(RuntimeError::Contract)?;
            let response = retry.send().await?;
            if response.status().as_u16() != 503 || attempt == 3 {
                return Ok(response);
            }
            tokio::time::sleep(Duration::from_millis(500 * (1 << attempt))).await;
        }
        Err(RuntimeError::Transport)
    }
    /// # Errors
    /// Presigned transfers use a credential-free client and explicit signed headers.
    pub async fn signed_put(
        &self,
        request: &Value,
        path: &Path,
        offset: u64,
        length: u64,
    ) -> Result<Response, RuntimeError> {
        let url = request["url"].as_str().ok_or(RuntimeError::Contract)?;
        validate_bucket_url(url)?;
        let headers = header_map(&request["headers"])?;
        Ok(self
            .bucket
            .put(url)
            .headers(headers)
            .header(reqwest::header::CONTENT_LENGTH, length)
            .body(file_body(path, offset, length).await?)
            .send()
            .await?)
    }
    /// # Errors
    /// Writes each response chunk to disk. API auth is never sent to a redirect target.
    #[allow(clippy::too_many_arguments)] // One leased transfer and its cancellation owner.
    pub async fn download(
        &self,
        path: &str,
        key: &str,
        token: &Secret,
        lease: &LeaseHeaders,
        destination: &Path,
        expected_size: u64,
        cancel: &CancellationEvent,
    ) -> Result<(), RuntimeError> {
        let mut url = self.api_url(path)?;
        url.query_pairs_mut().append_pair("key", key);
        let mut response = cancelled(cancel, RuntimeError::LostLease, async {
            Ok(self
                .bucket
                .get(url)
                .bearer_auth(token.expose())
                .headers(lease.headers()?)
                .send()
                .await?)
        })
        .await?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or(RuntimeError::Contract)?;
            validate_bucket_url(location)?;
            response = cancelled(cancel, RuntimeError::LostLease, async {
                Ok(self.bucket.get(location).send().await?)
            })
            .await?;
        }
        checked_status(&response)?;
        let mut file = tokio::fs::File::create(destination).await?;
        let mut written = 0_u64;
        loop {
            let next = cancelled(cancel, RuntimeError::LostLease, async {
                Ok(response.chunk().await?)
            })
            .await;
            let next = match next {
                Ok(next) => next,
                Err(error) => {
                    // Settle buffered disk work before callers remove the owned tree.
                    let _ = file.flush().await;
                    return Err(error);
                }
            };
            let Some(chunk) = next else {
                break;
            };
            let next_size = u64::try_from(chunk.len())
                .ok()
                .and_then(|size| written.checked_add(size));
            let Some(next_size) = next_size.filter(|size| *size <= expected_size) else {
                // Preserve the integrity error while settling previous writes.
                let _ = file.flush().await;
                return Err(RuntimeError::Integrity);
            };
            file.write_all(&chunk).await?;
            written = next_size;
        }
        file.flush().await?;
        Ok(())
    }
}

/// Cancel only HTTP waits, never owned step/filesystem settlement. Dropping a
/// claim or completion response can leave a committed mutation on the server:
/// leases recover by expiry; completion callers retain their stable body/key.
pub(crate) async fn cancelled<T, E>(
    signal: &CancellationEvent,
    error: E,
    operation: impl Future<Output = Result<T, E>>,
) -> Result<T, E> {
    tokio::select! {
        biased;
        () = signal.wait() => Err(error),
        result = operation => result,
    }
}
/// Secrets deliberately do not implement Debug or Serialize.
pub struct LeaseHeaders {
    pub token: Secret,
    pub generation: String,
    pub idempotency: Option<String>,
}
impl LeaseHeaders {
    fn headers(&self) -> Result<HeaderMap, RuntimeError> {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("x-lease-token", self.token.expose()),
            ("x-lease-generation", &self.generation),
        ] {
            headers.insert(
                HeaderName::from_static(name),
                HeaderValue::from_str(value).map_err(|_| RuntimeError::Contract)?,
            );
        }
        if let Some(value) = &self.idempotency {
            headers.insert(
                "idempotency-key",
                HeaderValue::from_str(value).map_err(|_| RuntimeError::Contract)?,
            );
        }
        Ok(headers)
    }
}
fn validate_bucket_url(text: &str) -> Result<(), RuntimeError> {
    let url = Url::parse(text).map_err(|_| RuntimeError::Contract)?;
    if matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
    {
        Ok(())
    } else {
        Err(RuntimeError::Contract)
    }
}
fn header_map(value: &Value) -> Result<HeaderMap, RuntimeError> {
    let mut result = HeaderMap::new();
    for (key, value) in value.as_object().ok_or(RuntimeError::Contract)? {
        let name = HeaderName::from_bytes(key.as_bytes()).map_err(|_| RuntimeError::Contract)?;
        // Object-store requests must never inherit API cookies or bearer credentials.
        if name == reqwest::header::AUTHORIZATION || name == reqwest::header::COOKIE {
            return Err(RuntimeError::Contract);
        }
        result.insert(
            name,
            HeaderValue::from_str(value.as_str().ok_or(RuntimeError::Contract)?)
                .map_err(|_| RuntimeError::Contract)?,
        );
    }
    Ok(result)
}
async fn file_body(path: &Path, offset: u64, length: u64) -> Result<reqwest::Body, RuntimeError> {
    let mut file = tokio::fs::File::open(path).await?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let input = stream::try_unfold((file, length), |(mut file, left)| async move {
        if left == 0 {
            return Ok::<_, std::io::Error>(None);
        }
        let capacity =
            usize::try_from(left.min(1024 * 1024)).map_err(|_| std::io::ErrorKind::InvalidData)?;
        let mut bytes = vec![0; capacity];
        let read = file.read(&mut bytes).await?;
        if read == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        bytes.truncate(read);
        Ok(Some((
            bytes,
            (
                file,
                left - u64::try_from(read).map_err(|_| std::io::ErrorKind::InvalidData)?,
            ),
        )))
    });
    Ok(reqwest::Body::wrap_stream(input))
}
/// # Errors
/// Hash files incrementally; metadata alone never determines the digest.
pub async fn digest(path: &Path) -> Result<(u64, String), RuntimeError> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut bytes = vec![0; 1024 * 1024];
    loop {
        let read = file.read(&mut bytes).await?;
        if read == 0 {
            break;
        }
        hash.update(&bytes[..read]);
        size = size
            .checked_add(u64::try_from(read).map_err(|_| RuntimeError::Contract)?)
            .ok_or(RuntimeError::Contract)?;
    }
    Ok((size, format!("{:x}", hash.finalize())))
}
/// # Errors
/// Every non-success status is explicit; callers classify durable lease failures.
pub fn checked_status(response: &Response) -> Result<(), RuntimeError> {
    if response.status().is_success() {
        Ok(())
    } else {
        Err(RuntimeError::Http(response.status().as_u16()))
    }
}
/// # Errors
/// Parses the response only after checking HTTP status.
pub async fn json_response(response: Response) -> Result<Value, RuntimeError> {
    let status = response.status().as_u16();
    let body = read_json(response).await?;
    if status == 409 && body["error"]["code"] == "stale_lease" {
        Err(RuntimeError::LostLease)
    } else if (200..300).contains(&status) {
        Ok(body)
    } else {
        Err(RuntimeError::Http(status))
    }
}
/// Read an API document with explicit native byte and nesting bounds.
/// # Errors
/// Refuses oversized, malformed or too deeply nested JSON without diagnostics.
pub async fn read_json(mut response: Response) -> Result<Value, RuntimeError> {
    const MAX_BYTES: usize = 16 * 1024 * 1024;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BYTES as u64)
    {
        return Err(RuntimeError::Contract);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > MAX_BYTES - bytes.len() {
            return Err(RuntimeError::Contract);
        }
        bytes.extend_from_slice(&chunk);
    }
    // The API uses a typed finite JSON model. Verify the native depth policy
    // before serde's bounded recursive value construction.
    cannery_core::json::decode(&bytes, 128).map_err(|_| RuntimeError::Contract)?;
    serde_json::from_slice(&bytes).map_err(|_| RuntimeError::Contract)
}
/// Build the protocol completion manifest, never including lease/API credentials.
#[must_use]
pub fn completion(job: &Value, evidence: &Value, objects: &[Value]) -> Value {
    json!({"schema_version":"0.2", "job_id":job["job_id"], "evidence":evidence,
        "manifest":{"schema_version":"0.2", "attempt_id":job["attempt_id"], "objects":objects}})
}
