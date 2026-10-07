//! Source direct-upload planning, signing and verification using the official SDK adapter.
use crate::{
    attempt_lease_routes::{Failure, domain, internal},
    requests::RequestContext,
};
use cannery_attempts::model::{Transfer, Upload};
use cannery_core::{
    errors::{DomainError, ErrorCode},
    timestamps::Timestamp,
};
use cannery_storage::{
    ObjectStore, StoredObject,
    s3::{S3Store, part_count, part_length},
};
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, time::SystemTime};
pub(crate) fn store_error(error: cannery_storage::Error, context: &RequestContext) -> Failure {
    let (operation, code) = match error {
        cannery_storage::Error::Store { operation, code } => (operation, code),
        cannery_storage::Error::Transport { operation, kind } => (operation, kind.to_owned()),
        _ => return internal(context, "upload object store"),
    };
    crate::attempt_lease_routes::failure(crate::errors::ApiError::RetryableStore(DomainError::new(
        ErrorCode::StoreUnavailable,
        format!(
            "the object store is unavailable (object store {operation} failed: {code}); retry later"
        ),
    )))
}

pub(crate) struct Plan {
    pub transfer: &'static str,
    pub id: Option<String>,
    pub part_size: Option<BigInt>,
}
#[derive(Serialize)]
pub(crate) struct Signed {
    method: &'static str,
    url: String,
    headers: std::collections::BTreeMap<String, String>,
    expires_at: String,
}
#[derive(Serialize)]
pub(crate) struct Part {
    #[serde(flatten)]
    request: Signed,
    part_number: i64,
}
#[derive(Serialize)]
pub(crate) struct Presign {
    pub request: Option<Signed>,
    pub parts: Vec<Part>,
}
#[derive(Serialize)]
pub(crate) struct Grant {
    pub transfer: &'static str,
    pub request: Option<Signed>,
    pub part_size: Option<i64>,
    pub part_count: Option<i64>,
    pub parts: Vec<Part>,
    pub presign_url: String,
    pub finish_url: String,
}

pub(crate) async fn plan(
    store: &S3Store,
    key: &str,
    size: &BigInt,
    media: &str,
    context: &RequestContext,
) -> Result<Plan, Failure> {
    if store.transfer_for(size.clone()) == "single" {
        return Ok(Plan {
            transfer: "single",
            id: None,
            part_size: None,
        });
    }
    if part_count(size, &store.part_size).map_err(|_| internal(context, "multipart count"))?
        > BigInt::from(10_000)
    {
        return Err(domain(
            ErrorCode::ValidationFailed,
            "an upload has at most 10000 parts",
        ));
    }
    let id = store
        .create_multipart(key, media)
        .await
        .map_err(|e| store_error(e, context))?;
    Ok(Plan {
        transfer: "multipart",
        id: Some(id),
        part_size: Some(store.part_size.clone()),
    })
}
pub(crate) async fn discard(store: Option<&S3Store>, key: &str, plan: &Plan) {
    if let (Some(store), Some(id)) = (store, &plan.id) {
        let _ = store.abort_multipart(key, id).await;
    }
}
pub(crate) fn store<'a>(store: &'a ObjectStore, upload: &Upload) -> Result<&'a S3Store, Failure> {
    if upload.transfer == Transfer::Stream {
        return Err(domain(
            ErrorCode::Conflict,
            "this upload streams through the API: PUT its bytes to upload_url",
        ));
    }
    store.presigning().filter(|_| upload.backend==store.backend() && upload.bucket==store.bucket()).ok_or_else(|| domain(ErrorCode::Conflict,"this upload was granted for another object store: request a new grant once it expires"))
}
pub(crate) fn lifetime(
    store: &S3Store,
    upload: &Upload,
    now: Timestamp,
    context: &RequestContext,
) -> Result<BigInt, Failure> {
    let micros = upload
        .expires_at
        .0
        .signed_duration_since(now.0)
        .num_microseconds()
        .ok_or_else(|| internal(context, "upload duration"))?;
    // Python int(total_seconds()) truncates towards zero, including expired recovery records.
    let result = store
        .presign_ttl
        .clone()
        .min(BigInt::from(micros / 1_000_000 - 30));
    if result <= BigInt::from(0) {
        return Err(domain(
            ErrorCode::UploadExpired,
            "the upload grant expires too soon to sign URLs; request a new grant",
        ));
    }
    Ok(result)
}
pub(crate) async fn presign(
    store: &S3Store,
    upload: &Upload,
    numbers: Option<Vec<BigInt>>,
    lifetime: &BigInt,
    now: Timestamp,
    clock: SystemTime,
    context: &RequestContext,
) -> Result<Presign, Failure> {
    let seconds = lifetime
        .to_i64()
        .ok_or_else(|| internal(context, "presign datetime"))?;
    let expiry = now
        .0
        .checked_add_signed(chrono::Duration::seconds(seconds))
        .ok_or_else(|| internal(context, "presign datetime"))?;
    let wrap = |signed: cannery_storage::s3::PresignedRequest| Signed {
        method: "PUT",
        url: signed.url,
        headers: signed.headers,
        expires_at: Timestamp(expiry).model_isoformat(),
    };
    if upload.transfer == Transfer::Single {
        if numbers.is_some() {
            return Err(domain(
                ErrorCode::ValidationFailed,
                "a single-PUT upload has no parts",
            ));
        }
        let signed = store
            .presign_put(
                &upload.key,
                upload.declared_size,
                &upload.declared_sha256,
                lifetime.clone(),
                clock,
            )
            .await
            .map_err(|e| store_error(e, context))?;
        return Ok(Presign {
            request: Some(wrap(signed)),
            parts: vec![],
        });
    }
    if upload.transfer != Transfer::Multipart {
        return Err(domain(
            ErrorCode::Conflict,
            "this upload streams through the API: PUT its bytes to upload_url",
        ));
    }
    let id = upload
        .multipart_upload_id
        .as_deref()
        .ok_or_else(|| internal(context, "multipart upload id"))?;
    let size = BigInt::from(upload.declared_size);
    let part_size = BigInt::from(
        upload
            .part_size
            .ok_or_else(|| internal(context, "multipart part size"))?,
    );
    let count = part_count(&size, &part_size).map_err(|_| internal(context, "multipart count"))?;
    let numbers = numbers.filter(|n| !n.is_empty()).unwrap_or_else(|| {
        (1..=count.to_i64().unwrap_or(100).min(100))
            .map(BigInt::from)
            .collect()
    });
    if numbers.iter().any(|n| n < &BigInt::from(1) || n > &count) {
        return Err(domain(
            ErrorCode::ValidationFailed,
            format!("part numbers go from 1 to {count}"),
        ));
    }
    let mut parts = vec![];
    for number in numbers.into_iter().collect::<BTreeSet<_>>() {
        let signed = store
            .presign_part(
                &upload.key,
                id,
                number.clone(),
                part_length(&size, &part_size, &number),
                lifetime.clone(),
                clock,
            )
            .await
            .map_err(|e| store_error(e, context))?;
        parts.push(Part {
            request: wrap(signed),
            part_number: number
                .to_i64()
                .ok_or_else(|| internal(context, "part number response"))?,
        });
    }
    Ok(Presign {
        request: None,
        parts,
    })
}
fn parts_to_send(
    upload: &Upload,
    parts: &[cannery_storage::s3::UploadedPart],
    context: &RequestContext,
) -> Result<Vec<i64>, Failure> {
    let size = BigInt::from(upload.declared_size);
    let part_size = BigInt::from(
        upload
            .part_size
            .ok_or_else(|| internal(context, "multipart part size"))?,
    );
    let count = part_count(&size, &part_size)
        .map_err(|_| internal(context, "multipart count"))?
        .to_i64()
        .ok_or_else(|| internal(context, "multipart count range"))?;
    let sizes: std::collections::BTreeMap<_, _> = parts
        .iter()
        .map(|p| (i64::from(p.number), p.size_bytes))
        .collect();
    Ok((1..=count)
        .filter(|n| {
            sizes.get(n).copied().map(BigInt::from)
                != Some(part_length(&size, &part_size, &BigInt::from(*n)))
        })
        .collect())
}
fn conflict(message: String, missing: &[i64]) -> Failure {
    crate::attempt_lease_routes::failure(crate::errors::ApiError::from(
        DomainError::new(ErrorCode::Conflict, message).with_details(
            serde_json::json!({"missing_parts":missing.iter().take(1000).collect::<Vec<_>>()}),
        ),
    ))
}
#[allow(
    clippy::too_many_lines,
    reason = "Source multipart completion and read verification order"
)]
pub(crate) async fn verify(
    store: &S3Store,
    upload: &Upload,
    context: &RequestContext,
) -> Result<StoredObject, Failure> {
    if upload.transfer == Transfer::Multipart {
        let id = upload
            .multipart_upload_id
            .as_deref()
            .ok_or_else(|| internal(context, "multipart id"))?;
        let parts = store
            .list_parts(&upload.key, id)
            .await
            .map_err(|e| store_error(e, context))?;
        if let Some(parts) = parts {
            let missing = parts_to_send(upload, &parts, context)?;
            let count = part_count(
                &BigInt::from(upload.declared_size),
                &BigInt::from(
                    upload
                        .part_size
                        .ok_or_else(|| internal(context, "multipart size"))?,
                ),
            )
            .map_err(|_| internal(context, "multipart count"))?;
            if !missing.is_empty() {
                let shown = missing
                    .iter()
                    .take(20)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(conflict(
                    format!(
                        "{} of {count} parts are missing or have the wrong size ({shown}); upload them and finish again",
                        missing.len()
                    ),
                    &missing,
                ));
            }
            let parts: Vec<_> = parts
                .into_iter()
                .filter(|p| BigInt::from(p.number) <= count)
                .collect();
            if let Err(error) = store.complete_multipart(&upload.key, id, &parts).await {
                if let cannery_storage::Error::PartsRejected(code) = error {
                    let listed = store
                        .list_parts(&upload.key, id)
                        .await
                        .map_err(|e| store_error(e, context))?;
                    let missing = listed
                        .as_ref()
                        .map(|p| parts_to_send(upload, p, context))
                        .transpose()?
                        .unwrap_or_default();
                    let shown = missing
                        .iter()
                        .take(20)
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(conflict(
                        format!(
                            "the object store refused the uploaded parts ({code}): upload {}finish again",
                            if missing.is_empty() {
                                String::new()
                            } else {
                                format!("parts {shown} again and ")
                            }
                        ),
                        &missing,
                    ));
                }
                return Err(store_error(error, context));
            }
        }
    }
    let head = store.head(&upload.key).await.map_err(|error| store_error(error, context))?.ok_or_else(|| {
        if upload.transfer == Transfer::Multipart {
            domain(ErrorCode::UploadExpired, "the multipart upload is gone and nothing was stored; request a new grant once this one expires")
        } else {
            domain(ErrorCode::Conflict, "nothing was uploaded yet: PUT the bytes to the presigned URL first")
        }
    })?;
    if upload.transfer != Transfer::Multipart
        && let Some(sha256) = head.sha256
    {
        return Ok(StoredObject {
            size_bytes: head.size_bytes,
            sha256,
            generation: head.generation,
        });
    }
    let mut reader = store
        .read(&upload.key)
        .await
        .map_err(|e| store_error(e, context))?;
    let mut size = 0u64;
    let mut digest = Sha256::new();
    while let Some(chunk) = reader
        .next_chunk()
        .await
        .map_err(|e| store_error(e, context))?
    {
        size += chunk.len() as u64;
        digest.update(&chunk);
        if BigInt::from(size) > BigInt::from(upload.declared_size) {
            break;
        }
    }
    Ok(StoredObject {
        size_bytes: size,
        sha256: format!("{:x}", digest.finalize()),
        generation: head.generation,
    })
}
