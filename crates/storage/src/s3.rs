use crate::{CHUNK, Error, ObjectHead, StoredObject, hex};
use aws_sdk_s3::{
    Client,
    config::{
        BehaviorVersion, Credentials, Region, RequestChecksumCalculation,
        ResponseChecksumValidation, retry::RetryConfig, timeout::TimeoutConfig,
    },
    error::{ProvideErrorMetadata, SdkError},
    presigning::PresigningConfig,
    primitives::ByteStream,
    types::{ChecksumMode, CompletedMultipartUpload, CompletedPart},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{Stream, StreamExt};
use md5::Md5;
use num_traits::ToPrimitive;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime},
};
const STREAM_PART: usize = 8 << 20;

struct MultipartCleanup {
    client: Client,
    bucket: String,
    key: String,
    id: Option<String>,
    sink: Option<std::sync::Arc<dyn crate::CancellationSink>>,
}
impl Drop for MultipartCleanup {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else { return };
        let Some(sink) = &self.sink else { return };
        let client = self.client.clone();
        let bucket = self.bucket.clone();
        let key = self.key.clone();
        sink.submit(Box::pin(async move {
            let _ = client
                .abort_multipart_upload()
                .bucket(bucket)
                .key(key)
                .upload_id(id)
                .send()
                .await;
        }));
    }
}

#[derive(Debug)]
struct SourceQuery;
impl aws_smithy_runtime_api::client::interceptors::Intercept for SourceQuery {
    fn name(&self) -> &'static str {
        "SourceQuery"
    }
    fn modify_before_signing(
        &self,
        context: &mut aws_smithy_runtime_api::client::interceptors::context::BeforeTransmitInterceptorContextMut<'_>,
        _: &aws_smithy_runtime_api::client::runtime_components::RuntimeComponents,
        _: &mut aws_smithy_types::config_bag::ConfigBag,
    ) -> Result<(), aws_smithy_runtime_api::box_error::BoxError> {
        let request = context.request_mut();
        request.headers_mut().remove("x-amz-user-agent");
        if request.method() == "PUT"
            || (request.method() == "POST" && request.uri().contains("uploadId="))
        {
            request.headers_mut().remove("content-type");
        }
        if request.method() == "DELETE" || request.method() == "POST" {
            request.headers_mut().remove("content-length");
        }
        let uri = request.uri();
        if let Some((base, query)) = uri.split_once('?') {
            let mut parameters = query
                .split('&')
                .filter(|parameter| !parameter.starts_with("x-id="))
                .collect::<Vec<_>>();
            if let Some(index) = parameters
                .iter()
                .position(|parameter| parameter.starts_with("uploadId="))
            {
                let upload = parameters.remove(index);
                parameters.insert(0, upload);
            }
            let query = parameters.join("&");
            let base = if query.split('&').any(|p| p.starts_with("prefix="))
                && query.split('&').any(|p| p == "uploads" || p == "uploads=")
            {
                base.strip_suffix('/').unwrap_or(base)
            } else {
                base
            };
            let uri = if query.is_empty() {
                base.to_owned()
            } else {
                format!("{base}?{query}")
            };
            request.set_uri(uri)?;
        }
        Ok(())
    }
    fn modify_before_transmit(
        &self,
        context:&mut aws_smithy_runtime_api::client::interceptors::context::BeforeTransmitInterceptorContextMut<'_>,
        _: &aws_smithy_runtime_api::client::runtime_components::RuntimeComponents,
        _: &mut aws_smithy_types::config_bag::ConfigBag,
    ) -> Result<(), aws_smithy_runtime_api::box_error::BoxError> {
        let request = context.request_mut();
        if request.method() == "DELETE" || request.method() == "POST" {
            let size = request.body().bytes().map_or(0, <[u8]>::len);
            request
                .headers_mut()
                .insert("content-length", size.to_string());
        }
        Ok(())
    }
}

pub struct S3Options {
    pub endpoint: Option<String>,
    pub public_endpoint: Option<String>,
    pub region: String,
    pub credentials: Credentials,
    pub path_style: bool,
    pub bucket: String,
    pub prefix: String,
    pub presign_ttl: u64,
    pub multipart_threshold: u64,
    pub part_size: u64,
    pub upload_ttl: u64,
}
pub struct S3Store {
    credentials: aws_sdk_s3::config::SharedCredentialsProvider,
    client: Client,
    presigner: Client,
    pub bucket: String,
    prefix: String,
    pub presign_ttl: num_bigint::BigInt,
    multipart_threshold: num_bigint::BigInt,
    pub part_size: num_bigint::BigInt,
    min_multipart_age: num_bigint::BigInt,
}
impl std::fmt::Debug for S3Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("S3Store(<redacted>)")
    }
}
pub struct PresignedRequest {
    pub method: &'static str,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub expires_in: num_bigint::BigInt,
}
impl std::fmt::Debug for PresignedRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PresignedRequest(<redacted>)")
    }
}
#[derive(Clone, PartialEq, Eq)]
pub struct UploadedPart {
    pub number: i32,
    pub size_bytes: i64,
    pub etag: String,
}
impl std::fmt::Debug for UploadedPart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UploadedPart(<redacted>)")
    }
}
// Python bytes.fromhex permits ASCII whitespace between complete byte pairs.
/// # Errors
/// Returns `InvalidChecksum` for source bytes.fromhex syntax errors.
pub fn sha256_base64(value: &str) -> Result<String, Error> {
    let mut raw = Vec::new();
    let mut chars = value.chars().peekable();
    while chars.peek().is_some() {
        while chars.peek().is_some_and(char::is_ascii_whitespace) {
            chars.next();
        }
        let Some(high) = chars.next() else { break };
        let low = chars.next().ok_or(Error::InvalidChecksum)?;
        let high = high
            .to_digit(16)
            .filter(|_| high.is_ascii())
            .ok_or(Error::InvalidChecksum)?;
        let low = low
            .to_digit(16)
            .filter(|_| low.is_ascii())
            .ok_or(Error::InvalidChecksum)?;
        raw.push(u8::try_from(high * 16 + low).map_err(|_| Error::InvalidChecksum)?);
    }
    Ok(STANDARD.encode(raw))
}
#[must_use]
pub fn sha256_hex(value: &str) -> Option<String> {
    if value.contains('-') {
        return None;
    }
    let decoder = base64::engine::general_purpose::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::general_purpose::GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true),
    );
    let raw = decoder.decode(value).ok()?;
    if raw.len() != 32 {
        return None;
    }
    Some(hex(raw))
}
fn failure<E: ProvideErrorMetadata>(operation: &'static str, error: &SdkError<E>) -> Error {
    let code = error
        .as_service_error()
        .and_then(ProvideErrorMetadata::code)
        .map(str::to_owned)
        .or_else(|| {
            error
                .raw_response()
                .map(|response| response.status().as_u16().to_string())
        });
    if let Some(code) = code {
        Error::Store { operation, code }
    } else {
        Error::Transport {
            operation,
            kind: match error {
                SdkError::TimeoutError(_) => "TimeoutError",
                SdkError::DispatchFailure(_) => "DispatchFailure",
                SdkError::ConstructionFailure(_) => "ConstructionFailure",
                _ => "ResponseError",
            },
        }
    }
}
impl S3Store {
    #[must_use]
    pub fn new(options: S3Options) -> Self {
        let credentials =
            aws_sdk_s3::config::SharedCredentialsProvider::new(options.credentials.clone());
        Self::with_provider(options, credentials)
    }
    fn with_provider(
        options: S3Options,
        credentials: aws_sdk_s3::config::SharedCredentialsProvider,
    ) -> Self {
        let http = aws_smithy_http_client::Builder::new()
            .tls_provider(aws_smithy_http_client::tls::Provider::Rustls(
                aws_smithy_http_client::tls::rustls_provider::CryptoMode::Ring,
            ))
            .build_https();
        let mut config = aws_sdk_s3::Config::builder()
            .interceptor(SourceQuery)
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(options.region))
            .credentials_provider(credentials.clone())
            .http_client(http)
            .force_path_style(options.path_style)
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
            .response_checksum_validation(ResponseChecksumValidation::WhenRequired)
            .retry_config(RetryConfig::standard().with_max_attempts(4))
            .timeout_config(
                TimeoutConfig::builder()
                    .connect_timeout(Duration::from_secs(10))
                    .read_timeout(Duration::from_secs(120))
                    .build(),
            );
        config.set_endpoint_url(options.endpoint);
        let client = Client::from_conf(config.clone().build());
        let presigner = if let Some(endpoint) = options.public_endpoint {
            Client::from_conf(config.endpoint_url(endpoint).build())
        } else {
            client.clone()
        };
        Self {
            credentials,
            client,
            presigner,
            bucket: options.bucket,
            prefix: options.prefix,
            presign_ttl: options.presign_ttl.into(),
            multipart_threshold: options.multipart_threshold.into(),
            part_size: options.part_size.into(),
            min_multipart_age: num_bigint::BigInt::from(options.upload_ttl) + 600,
        }
    }
    #[must_use]
    pub const fn backend(&self) -> &'static str {
        "s3"
    }
    fn key(&self, key: &str) -> Result<String, Error> {
        if key.is_empty()
            || key.starts_with('/')
            || key.split('/').any(|p| matches!(p, "" | "." | ".."))
        {
            return Err(Error::InvalidKey);
        }
        Ok(format!("{}{key}", self.prefix))
    }
    #[must_use]
    pub fn transfer_for(&self, size: impl Into<num_bigint::BigInt>) -> &'static str {
        if size.into() > self.multipart_threshold {
            "multipart"
        } else {
            "single"
        }
    }
    /// # Errors
    /// Returns source-classified storage, key, stream, or SDK range errors.
    pub async fn head(&self, key: &str) -> Result<Option<ObjectHead>, Error> {
        let result = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(self.key(key)?)
            .checksum_mode(ChecksumMode::Enabled)
            .send()
            .await;
        match result {
            Ok(output) => Ok(Some(ObjectHead {
                size_bytes: u64::try_from(output.content_length().ok_or(Error::IntegerRange)?)
                    .map_err(|_| Error::IntegerRange)?,
                generation: output.e_tag().map(str::to_owned),
                sha256: output.checksum_sha256().and_then(sha256_hex),
            })),
            Err(e) => {
                let error = failure("head_object", &e);
                if error.missing() {
                    Ok(None)
                } else {
                    Err(error)
                }
            }
        }
    }
    /// # Errors
    /// Returns source-classified storage, key, stream, or SDK range errors.
    pub async fn read(&self, key: &str) -> Result<S3Reader, Error> {
        let output = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(self.key(key)?)
            .send()
            .await
            .map_err(|e| failure("get_object", &e))?;
        Ok(S3Reader {
            body: Box::new(output.body.into_async_read()),
        })
    }
    /// # Errors
    /// Returns source-classified storage, key, stream, or SDK range errors.
    pub async fn stat(&self, key: &str) -> Result<Option<StoredObject>, Error> {
        let output = match self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(self.key(key)?)
            .send()
            .await
        {
            Ok(v) => v,
            Err(e) => {
                let e = failure("get_object", &e);
                if e.missing() {
                    return Ok(None);
                }
                return Err(e);
            }
        };
        let generation = output.e_tag().map(str::to_owned);
        let mut reader = S3Reader {
            body: Box::new(output.body.into_async_read()),
        };
        let mut digest = Sha256::new();
        let mut size = 0u64;
        while let Some(chunk) = reader.next_chunk().await? {
            size += chunk.len() as u64;
            digest.update(chunk);
        }
        Ok(Some(StoredObject {
            size_bytes: size,
            sha256: hex(digest.finalize()),
            generation,
        }))
    }
    /// # Errors
    /// Returns source-classified storage, key, stream, or SDK range errors.
    #[allow(clippy::too_many_lines)] // Preserve the source multipart cleanup boundary.
    pub async fn write_new<S>(
        &self,
        key: &str,
        chunks: S,
        max_bytes: impl Into<num_bigint::BigInt>,
    ) -> Result<StoredObject, Error>
    where
        S: Stream<Item = Result<Vec<u8>, Error>> + Unpin,
    {
        self.write_new_inner(key, chunks, max_bytes.into(), None)
            .await
    }
    /// # Errors
    /// Returns the same errors as `write_new`, with caller-owned cancellation cleanup.
    pub async fn write_new_with_cancellation<S>(
        &self,
        key: &str,
        chunks: S,
        max_bytes: impl Into<num_bigint::BigInt>,
        cancellation: std::sync::Arc<dyn crate::CancellationSink>,
    ) -> Result<StoredObject, Error>
    where
        S: Stream<Item = Result<Vec<u8>, Error>> + Unpin,
    {
        self.write_new_inner(key, chunks, max_bytes.into(), Some(cancellation))
            .await
    }
    #[allow(
        clippy::too_many_lines,
        reason = "Preserve source multipart cleanup order"
    )]
    async fn write_new_inner<S>(
        &self,
        key: &str,
        mut chunks: S,
        max_bytes: num_bigint::BigInt,
        sink: Option<std::sync::Arc<dyn crate::CancellationSink>>,
    ) -> Result<StoredObject, Error>
    where
        S: Stream<Item = Result<Vec<u8>, Error>> + Unpin,
    {
        let full = self.key(key)?;
        if self.head(key).await?.is_some() {
            return Err(Error::ObjectExists);
        }
        let mut upload_id = None;
        let mut cleanup = MultipartCleanup {
            client: self.client.clone(),
            bucket: self.bucket.clone(),
            key: full.clone(),
            id: None,
            sink,
        };
        let mut parts = Vec::new();
        let mut buffer = Vec::new();
        let mut size = 0u64;
        let mut digest = Sha256::new();
        let result = async {
            while let Some(chunk) = chunks.next().await {
                let chunk = chunk?;
                size = size
                    .checked_add(chunk.len() as u64)
                    .ok_or(Error::IntegerRange)?;
                if num_bigint::BigInt::from(size) > max_bytes {
                    return Err(Error::ObjectTooLarge);
                }
                digest.update(&chunk);
                buffer.extend_from_slice(&chunk);
                while buffer.len() >= STREAM_PART {
                    if upload_id.is_none() {
                        let output = self
                            .client
                            .create_multipart_upload()
                            .bucket(&self.bucket)
                            .key(&full)
                            .send()
                            .await
                            .map_err(|e| failure("create_multipart_upload", &e))?;
                        upload_id = Some(output.upload_id().ok_or(Error::IntegerRange)?.to_owned());
                        cleanup.id.clone_from(&upload_id);
                    }
                    let remaining = buffer.split_off(STREAM_PART);
                    let data = std::mem::replace(&mut buffer, remaining);
                    self.put_part(
                        &full,
                        upload_id.as_deref().ok_or(Error::IntegerRange)?,
                        data,
                        &mut parts,
                    )
                    .await?;
                }
            }
            let checksum = STANDARD.encode(digest.clone().finalize());
            let generation = if let Some(id) = upload_id.as_deref() {
                if !buffer.is_empty() {
                    self.put_part(&full, id, std::mem::take(&mut buffer), &mut parts)
                        .await?;
                }
                let xml = complete_xml(&parts);
                let output = self
                    .client
                    .complete_multipart_upload()
                    .bucket(&self.bucket)
                    .key(&full)
                    .upload_id(id)
                    .multipart_upload(
                        CompletedMultipartUpload::builder()
                            .set_parts(Some(parts))
                            .build(),
                    )
                    .customize()
                    .mutate_request(move |request| source_complete_body(request, &xml))
                    .send()
                    .await
                    .map_err(|e| failure("complete_multipart_upload", &e))?;
                upload_id = None;
                cleanup.id = None;
                output.e_tag().map(str::to_owned)
            } else {
                let length = i64::try_from(buffer.len()).map_err(|_| Error::IntegerRange)?;
                let output = self
                    .client
                    .put_object()
                    .bucket(&self.bucket)
                    .key(&full)
                    .body(ByteStream::from(buffer))
                    .content_length(length)
                    .checksum_sha256(checksum)
                    .send()
                    .await
                    .map_err(|e| failure("put_object", &e))?;
                output.e_tag().map(str::to_owned)
            };
            Ok(StoredObject {
                size_bytes: size,
                sha256: hex(digest.finalize()),
                generation,
            })
        }
        .await;
        if result.is_err()
            && let Some(id) = upload_id
        {
            let _ = self
                .client
                .abort_multipart_upload()
                .bucket(&self.bucket)
                .key(full)
                .upload_id(id)
                .send()
                .await;
        }
        cleanup.id = None;
        result
    }
    async fn put_part(
        &self,
        key: &str,
        id: &str,
        data: Vec<u8>,
        parts: &mut Vec<CompletedPart>,
    ) -> Result<(), Error> {
        let md5 = STANDARD.encode(Md5::digest(&data));
        let number = i32::try_from(parts.len() + 1).map_err(|_| Error::IntegerRange)?;
        let size = i64::try_from(data.len()).map_err(|_| Error::IntegerRange)?;
        let output = self
            .client
            .upload_part()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(id)
            .part_number(number)
            .content_length(size)
            .content_md5(md5)
            .body(ByteStream::from(data))
            .send()
            .await
            .map_err(|e| failure("upload_part", &e))?;
        parts.push(
            CompletedPart::builder()
                .part_number(number)
                .set_e_tag(output.e_tag().map(str::to_owned))
                .build(),
        );
        Ok(())
    }
    /// # Errors
    /// Returns source-classified storage, key, stream, or SDK range errors.
    pub async fn delete(&self, key: &str, generation: Option<&str>) -> Result<(), Error> {
        let full = self.key(key)?;
        if let Some(expected) = generation
            && self.head(key).await?.and_then(|h| h.generation).as_deref() != Some(expected)
        {
            return Ok(());
        }
        match self
            .client
            .delete_object()
            .bucket(&self.bucket)
            .key(full)
            .set_if_match(generation.map(str::to_owned))
            .send()
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => {
                let e = failure("delete_object", &e);
                if e.missing()
                    || (generation.is_some()
                        && e.code()
                            .is_some_and(|c| matches!(c, "412" | "PreconditionFailed")))
                {
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }
    /// # Errors
    /// Returns source-classified storage, key, stream, or SDK range errors.
    pub async fn create_multipart(&self, key: &str, media_type: &str) -> Result<String, Error> {
        let output = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(self.key(key)?)
            .content_type(media_type)
            .send()
            .await
            .map_err(|e| failure("create_multipart_upload", &e))?;
        output
            .upload_id()
            .map(str::to_owned)
            .ok_or(Error::IntegerRange)
    }
    /// # Errors
    /// Returns source-classified storage, key, stream, or SDK range errors.
    pub async fn abort_multipart(&self, key: &str, upload_id: &str) -> Result<(), Error> {
        match self
            .client
            .abort_multipart_upload()
            .bucket(&self.bucket)
            .key(self.key(key)?)
            .upload_id(upload_id)
            .send()
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => {
                let e = failure("abort_multipart_upload", &e);
                if e.missing() { Ok(()) } else { Err(e) }
            }
        }
    }
    /// # Errors
    /// Returns source-classified storage, key, stream, or SDK range errors.
    pub async fn list_parts(
        &self,
        key: &str,
        upload_id: &str,
    ) -> Result<Option<Vec<UploadedPart>>, Error> {
        let key = self.key(key)?;
        let mut marker = None;
        let mut parts = Vec::new();
        loop {
            let output = match self
                .client
                .list_parts()
                .bucket(&self.bucket)
                .key(&key)
                .upload_id(upload_id)
                .max_parts(1000)
                .set_part_number_marker(marker)
                .send()
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    let e = failure("list_parts", &e);
                    if e.missing() {
                        return Ok(None);
                    }
                    return Err(e);
                }
            };
            for part in output.parts() {
                parts.push(UploadedPart {
                    number: part.part_number().ok_or(Error::IntegerRange)?,
                    size_bytes: part.size().ok_or(Error::IntegerRange)?,
                    etag: part.e_tag().ok_or(Error::IntegerRange)?.to_owned(),
                });
            }
            if output.is_truncated() != Some(true) || output.next_part_number_marker().is_none() {
                parts.sort_by_key(|p| p.number);
                return Ok(Some(parts));
            }
            marker = output.next_part_number_marker().map(str::to_owned);
        }
    }
    /// # Errors
    /// Returns source-classified storage, key, stream, or SDK range errors.
    pub async fn complete_multipart(
        &self,
        key: &str,
        id: &str,
        parts: &[UploadedPart],
    ) -> Result<bool, Error> {
        let parts: Vec<_> = parts
            .iter()
            .map(|p| {
                CompletedPart::builder()
                    .part_number(p.number)
                    .e_tag(&p.etag)
                    .build()
            })
            .collect();
        let xml = complete_xml(&parts);
        match self
            .client
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(self.key(key)?)
            .upload_id(id)
            .multipart_upload(
                CompletedMultipartUpload::builder()
                    .set_parts(Some(parts))
                    .build(),
            )
            .customize()
            .mutate_request(move |request| source_complete_body(request, &xml))
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(e) => {
                let e = failure("complete_multipart_upload", &e);
                if e.missing() {
                    Ok(false)
                } else if e.code().is_some_and(|c| {
                    matches!(c, "InvalidPart" | "InvalidPartOrder" | "EntityTooSmall")
                }) {
                    Err(Error::PartsRejected(
                        e.code().unwrap_or_default().to_owned(),
                    ))
                } else {
                    Err(e)
                }
            }
        }
    }
    /// # Errors
    /// Returns source-classified storage, key, stream, or SDK range errors.
    #[allow(clippy::cast_precision_loss)] // Source compares floating-point POSIX seconds.
    pub async fn sweep_staging(
        &self,
        older_than_seconds: f64,
        now: SystemTime,
    ) -> Result<u64, Error> {
        let minimum = self.min_multipart_age.to_f64().unwrap_or(f64::INFINITY);
        let age = if minimum > older_than_seconds {
            minimum
        } else {
            older_than_seconds
        };
        if !age.is_finite() {
            return Err(Error::IntegerRange);
        }
        let now = match now.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(duration) => {
                i128::try_from(duration.as_micros()).map_err(|_| Error::IntegerRange)?
            }
            Err(error) => {
                -i128::try_from(error.duration().as_micros()).map_err(|_| Error::IntegerRange)?
            }
        };
        // datetime/timedelta compares microseconds, rounding the fractional seconds once.
        let seconds = age.trunc().to_i64().ok_or(Error::IntegerRange)?;
        let fractional = ((age - age.trunc()) * 1e6)
            .round_ties_even()
            .to_i64()
            .ok_or(Error::IntegerRange)?;
        let cutoff = now - i128::from(seconds) * 1_000_000 - i128::from(fractional);
        let python_range = -62_135_596_800_000_000..253_402_300_800_000_000;
        if !python_range.contains(&now) || !python_range.contains(&cutoff) {
            return Err(Error::IntegerRange);
        }
        let mut key_marker = None;
        let mut id_marker = None;
        let mut count = 0;
        loop {
            let output = self
                .client
                .list_multipart_uploads()
                .bucket(&self.bucket)
                .prefix(&self.prefix)
                .set_key_marker(key_marker)
                .set_upload_id_marker(id_marker)
                .send()
                .await
                .map_err(|e| failure("list_multipart_uploads", &e))?;
            for upload in output.uploads() {
                if upload.initiated().is_some_and(|date| {
                    i128::from(date.secs()) * 1_000_000 + i128::from(date.subsec_nanos() / 1000)
                        < cutoff
                }) {
                    match self
                        .client
                        .abort_multipart_upload()
                        .bucket(&self.bucket)
                        .key(upload.key().ok_or(Error::IntegerRange)?)
                        .upload_id(upload.upload_id().ok_or(Error::IntegerRange)?)
                        .send()
                        .await
                    {
                        Ok(_) => count += 1,
                        Err(e) if e.as_service_error().is_some() => {}
                        Err(e) => return Err(failure("abort_multipart_upload", &e)),
                    }
                }
            }
            if output.is_truncated() != Some(true)
                || (output.next_key_marker().is_none_or(str::is_empty)
                    && output.next_upload_id_marker().is_none_or(str::is_empty))
            {
                return Ok(count);
            }
            key_marker = Some(output.next_key_marker().unwrap_or_default().to_owned());
            id_marker = Some(
                output
                    .next_upload_id_marker()
                    .unwrap_or_default()
                    .to_owned(),
            );
        }
    }
    fn signing(
        expires_in: &num_bigint::BigInt,
        now: SystemTime,
    ) -> Result<PresigningConfig, Error> {
        let seconds = expires_in
            .to_u64()
            .filter(|seconds| (1..=604_800).contains(seconds))
            .ok_or(Error::PresignExpiry)?;
        PresigningConfig::builder()
            .expires_in(Duration::from_secs(seconds))
            .start_time(now)
            .build()
            .map_err(|_| Error::PresignExpiry)
    }
    /// # Errors
    /// Returns source-classified storage, key, stream, or SDK range errors.
    pub async fn presign_put(
        &self,
        key: &str,
        size_bytes: impl Into<num_bigint::BigInt>,
        sha256: &str,
        expires_in: impl Into<num_bigint::BigInt>,
        now: SystemTime,
    ) -> Result<PresignedRequest, Error> {
        let checksum = sha256_base64(sha256)?;
        let full = self.key(key)?;
        let size_bytes = size_bytes.into();
        let length = size_bytes
            .to_i64()
            .filter(|length| *length >= 0)
            .ok_or(Error::IntegerRange)?;
        let expires_in = expires_in.into();
        let credentials = self.signing_credentials().await?;
        let signed = self
            .presigner
            .put_object()
            .bucket(&self.bucket)
            .key(full)
            .content_length(length)
            .checksum_sha256(&checksum)
            .customize()
            .config_override(
                aws_sdk_s3::Config::builder().credentials_provider(credentials.clone()),
            )
            .presigned(Self::signing(&expires_in, now)?)
            .await
            .map_err(|e| failure("generate_presigned_url", &e))?;
        let headers = BTreeMap::from([
            ("Content-Length".to_owned(), size_bytes.to_string()),
            ("x-amz-checksum-sha256".to_owned(), checksum),
        ]);
        let url = signed.uri().to_owned();
        Ok(PresignedRequest {
            method: "PUT",
            url,
            headers,
            expires_in,
        })
    }
    /// # Errors
    /// Returns source-classified storage, key, stream, or SDK range errors.
    pub async fn presign_part(
        &self,
        key: &str,
        id: &str,
        number: impl Into<num_bigint::BigInt>,
        size_bytes: impl Into<num_bigint::BigInt>,
        expires_in: impl Into<num_bigint::BigInt>,
        now: SystemTime,
    ) -> Result<PresignedRequest, Error> {
        let full = self.key(key)?;
        let size_bytes = size_bytes.into();
        let number = number.into();
        let length = size_bytes
            .to_i64()
            .filter(|length| *length >= 0)
            .ok_or(Error::IntegerRange)?;
        let part = number
            .to_i32()
            .filter(|part| (1..=10_000).contains(part))
            .ok_or(Error::IntegerRange)?;
        let expires_in = expires_in.into();
        let credentials = self.signing_credentials().await?;
        let signed = self
            .presigner
            .upload_part()
            .bucket(&self.bucket)
            .key(full)
            .upload_id(id)
            .part_number(part)
            .content_length(length)
            .customize()
            .config_override(
                aws_sdk_s3::Config::builder().credentials_provider(credentials.clone()),
            )
            .presigned(Self::signing(&expires_in, now)?)
            .await
            .map_err(|e| failure("generate_presigned_url", &e))?;
        let headers = BTreeMap::from([("Content-Length".to_owned(), size_bytes.to_string())]);
        let url = signed.uri().to_owned();
        Ok(PresignedRequest {
            method: "PUT",
            url,
            headers,
            expires_in,
        })
    }
    /// # Errors
    /// Returns source-classified storage, key, stream, or SDK range errors.
    pub async fn presign_get(
        &self,
        key: &str,
        filename: &str,
        media_type: &str,
        expires_in: impl Into<num_bigint::BigInt>,
        now: SystemTime,
    ) -> Result<String, Error> {
        let full = self.key(key)?;
        let expires_in = expires_in.into();
        let credentials = self.signing_credentials().await?;
        let signed = self
            .presigner
            .get_object()
            .bucket(&self.bucket)
            .key(full)
            .response_content_disposition(format!("attachment; filename=\"{filename}\""))
            .response_content_type(media_type)
            .customize()
            .config_override(
                aws_sdk_s3::Config::builder().credentials_provider(credentials.clone()),
            )
            .presigned(Self::signing(&expires_in, now)?)
            .await
            .map_err(|e| failure("generate_presigned_url", &e))?;
        Ok(signed.uri().to_owned())
    }
    async fn signing_credentials(&self) -> Result<Credentials, Error> {
        use aws_sdk_s3::config::ProvideCredentials;
        self.credentials
            .provide_credentials()
            .await
            .map_err(|_| Error::Credentials)
    }
}
pub struct S3Reader {
    body: Box<dyn tokio::io::AsyncRead + Unpin + Send>,
}
impl S3Reader {
    /// # Errors
    /// Returns source-classified storage, key, stream, or SDK range errors.
    pub async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, Error> {
        use tokio::io::AsyncReadExt;
        let mut bytes = vec![0; CHUNK];
        let mut filled = 0;
        while filled < CHUNK {
            let count = self
                .body
                .read(&mut bytes[filled..])
                .await
                .map_err(|error| Error::Body(error.kind()))?;
            if count == 0 {
                break;
            }
            filled += count;
        }
        bytes.truncate(filled);
        if filled == 0 {
            Ok(None)
        } else {
            Ok(Some(bytes))
        }
    }
}

/// # Errors
/// Returns `IntegerRange` for a zero divisor; otherwise retains Python's signed floor division.
pub fn part_count(
    size_bytes: &num_bigint::BigInt,
    part_size: &num_bigint::BigInt,
) -> Result<num_bigint::BigInt, Error> {
    use num_traits::{One, Zero};
    if part_size.is_zero() {
        return Err(Error::IntegerRange);
    }
    let numerator = -size_bytes;
    let mut quotient = &numerator / part_size;
    if !(&numerator % part_size).is_zero() && (numerator.sign() != part_size.sign()) {
        quotient -= 1;
    }
    Ok((-quotient).max(num_bigint::BigInt::one()))
}
#[must_use]
pub fn part_length(
    size_bytes: &num_bigint::BigInt,
    part_size: &num_bigint::BigInt,
    number: &num_bigint::BigInt,
) -> num_bigint::BigInt {
    part_size.clone().min(size_bytes - (number - 1) * part_size)
}
impl S3Store {
    /// # Errors
    /// Returns `IntegerRange` for settings outside SDK wire widths, and safe credential configuration errors.
    pub async fn from_settings(
        settings: &cannery_core::settings::StorageSettings,
    ) -> Result<Self, Error> {
        let credentials = match (&settings.s3_access_key_id, &settings.s3_secret_access_key) {
            (Some(key), Some(secret)) => aws_sdk_s3::config::SharedCredentialsProvider::new(
                Credentials::new(key, secret.expose(), None, None, "cannery-settings"),
            ),
            (None, None) => {
                let http = aws_smithy_http_client::Builder::new()
                    .tls_provider(aws_smithy_http_client::tls::Provider::Rustls(
                        aws_smithy_http_client::tls::rustls_provider::CryptoMode::Ring,
                    ))
                    .build_https();
                let config = aws_config::defaults(BehaviorVersion::latest())
                    .region(Region::new(settings.s3_region.clone()))
                    .http_client(http)
                    .load()
                    .await;
                config.credentials_provider().ok_or(Error::Worker)?
            }
            _ => {
                return Err(Error::Store {
                    operation: "client",
                    code: "PartialCredentialsError".to_owned(),
                });
            }
        };
        let options = S3Options {
            endpoint: settings.s3_endpoint.clone(),
            public_endpoint: settings.s3_public_endpoint.clone(),
            region: settings.s3_region.clone(),
            credentials: Credentials::new("", "", None, None, "unused"),
            path_style: settings.s3_path_style,
            bucket: settings.bucket.clone(),
            prefix: settings.s3_prefix.clone(),
            presign_ttl: 0,
            multipart_threshold: 0,
            part_size: 0,
            upload_ttl: 0,
        };
        let mut store = Self::with_provider(options, credentials);
        store
            .presign_ttl
            .clone_from(settings.s3_presign_ttl_seconds.as_bigint());
        store
            .multipart_threshold
            .clone_from(settings.s3_multipart_threshold_bytes.as_bigint());
        store
            .part_size
            .clone_from(settings.s3_part_size_bytes.as_bigint());
        store.min_multipart_age = settings.upload_ttl_minutes.as_bigint() * 60 + 600;
        Ok(store)
    }
}

fn complete_xml(parts: &[CompletedPart]) -> String {
    let root = "<CompleteMultipartUpload xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";
    if parts.is_empty() {
        return format!("{root} />");
    }
    let mut xml = format!("{root}>");
    for part in parts {
        use std::fmt::Write;
        xml.push_str("<Part>");
        if let Some(number) = part.part_number() {
            let _ = write!(xml, "<PartNumber>{number}</PartNumber>");
        }
        if let Some(etag) = part.e_tag() {
            xml.push_str("<ETag>");
            xml.push_str(
                &etag
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;"),
            );
            xml.push_str("</ETag>");
        }
        xml.push_str("</Part>");
    }
    xml.push_str("</CompleteMultipartUpload>");
    xml
}
fn source_complete_body(
    request: &mut aws_smithy_runtime_api::client::orchestrator::HttpRequest,
    xml: &str,
) {
    request.headers_mut().remove("content-length");
    *request.body_mut() = aws_smithy_types::body::SdkBody::from(xml.to_owned());
}
