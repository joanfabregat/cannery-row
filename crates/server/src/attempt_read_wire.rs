//! Attempt responses are domain adapters to the serde/OpenAPI DTOs.
use crate::{
    api_contract::{convert, decode, encode},
    api_models::{
        ArtifactOut, AttemptDetail, AttemptOut, ClaimOut, Claimant, LogRef, Page_AttemptOut_UUID_,
        Page_AttemptOut_int_, StorageRef, cannery_row__attempts__routes__FailureOut,
    },
};
use cannery_attempts::model::{Artifact, Attempt, Failure, StoredJson};
use cannery_core::json::{
    Document, Node,
    model::{self, ModelEncodeError},
};
type Result<T> = std::result::Result<T, ModelEncodeError>;
#[derive(Clone, Copy)]
pub struct ResponseContext {
    pub inferred_nesting_budget: usize,
}
fn mapping<T: serde::de::DeserializeOwned>(
    value: &StoredJson,
    p: ResponseContext,
    nullable: bool,
) -> Result<Option<T>> {
    if nullable && value.python_none() {
        return Ok(None);
    }
    let StoredJson::Value(d) = value else {
        return Err(ModelEncodeError::InvalidNode);
    };
    let result = decode(&model::encode_model_mapping(
        d,
        d.root(),
        p.inferred_nesting_budget,
    )?)?;
    Ok(Some(result))
}
pub(crate) fn validate_attempt(a: &Attempt) -> Result<()> {
    for value in [&a.producer, &a.workflow, &a.imported] {
        if !value.python_none()
            && !matches!(value,StoredJson::Value(d) if matches!(d.node(d.root()),Some(Node::Object(_))))
        {
            return Err(ModelEncodeError::InvalidNode);
        }
    }
    Ok(())
}
fn base(a: &Attempt, p: ResponseContext) -> Result<AttemptOut> {
    validate_attempt(a)?;
    Ok(AttemptOut {
        id: a.id.0.to_string(),
        r#ref: format!("#{}.{}", a.hypothesis_number, a.sequence),
        number: i64::from(a.hypothesis_number),
        sequence: i64::from(a.sequence),
        state: a.state.as_str().to_owned(),
        track: convert(&a.track_slug)?,
        hypothesis_revision: i64::from(a.hypothesis_revision),
        science_revision: i64::from(a.science_revision),
        producer: mapping(&a.producer, p, true)?,
        mode: convert(a.mode().as_str())?,
        workflow: mapping(&a.workflow, p, true)?,
        claimed_by: Claimant {
            kind: if a.claimed_by_user.is_some() {
                "user"
            } else {
                "service"
            }
            .to_owned(),
            id: a
                .claimed_by_user
                .map(|v| v.0)
                .or(a.claimed_by_service.map(|v| v.0))
                .ok_or(ModelEncodeError::InvalidNode)?
                .to_string(),
        },
        via_channel: convert(&a.via_channel)?,
        via_client: convert(&a.via_client)?,
        predecessor_id: a.predecessor_id.map(|v| v.to_string()),
        lease_generation: i64::from(a.lease_generation),
        lease_expires_at: a.lease_expires_at.map(crate::timestamps::public_timestamp),
        claimed_at: crate::timestamps::public_timestamp(a.claimed_at),
        started_at: a.started_at.map(crate::timestamps::public_timestamp),
        submitted_at: a.submitted_at.map(crate::timestamps::public_timestamp),
        finished_at: a.finished_at.map(crate::timestamps::public_timestamp),
        origin: convert(a.origin.as_str())?,
        source_ref: convert(&a.source_ref)?,
        imported: mapping(&a.imported, p, true)?,
    })
}
pub(crate) fn attempt(a: &Attempt, p: ResponseContext) -> Result<Vec<u8>> {
    encode(&base(a, p)?)
}
pub(crate) fn claim(
    a: &Attempt,
    token: &str,
    heartbeat: &num_bigint::BigInt,
    workflow: Option<&Document>,
    pins: crate::context_bundle::Pins,
    p: ResponseContext,
) -> Result<Vec<u8>> {
    use num_traits::ToPrimitive;
    encode(&ClaimOut {
        attempt: base(a, p)?,
        lease_token: token.to_owned(),
        lease_generation: i64::from(a.lease_generation),
        lease_expires_at: a
            .lease_expires_at
            .ok_or(ModelEncodeError::InvalidNode)?
            .model_isoformat(),
        heartbeat_seconds: heartbeat.to_i64().ok_or(ModelEncodeError::InvalidNode)?,
        workflow: workflow
            .map(|d| {
                decode(&model::encode_model_mapping(
                    d,
                    d.root(),
                    p.inferred_nesting_budget,
                )?)
            })
            .transpose()?,
        brief: pins.brief,
        plan: pins.plan,
        context: pins.context,
    })
}
pub(crate) fn artifact_model(a: &Artifact) -> Result<ArtifactOut> {
    Ok(ArtifactOut {
        id: a.id.0.to_string(),
        role: convert(&a.role)?,
        storage: StorageRef {
            backend: convert(&a.backend)?,
            bucket: convert(&a.bucket)?,
            key: convert(&a.key)?,
        },
        size_bytes: a.size_bytes,
        sha256: convert(&a.sha256)?,
        media_type: convert(&a.media_type)?,
        verified_at: crate::timestamps::public_timestamp(a.verified_at),
        interface: convert(&a.interface)?,
        content_validated: a.content_validated,
        origin: Some(convert(a.origin.as_str())?),
        source_ref: convert(&a.source_ref)?,
        uri: convert(&a.uri)?,
    })
}
pub(crate) fn artifact(a: &Artifact) -> Result<Vec<u8>> {
    encode(&artifact_model(a)?)
}
fn logs(value: &StoredJson, p: ResponseContext) -> Result<Vec<LogRef>> {
    let StoredJson::Value(d) = value else {
        return Err(ModelEncodeError::InvalidNode);
    };
    match d.node(d.root()) {
        Some(Node::Array(_)) => {
            let logs: Vec<LogRef> = decode(&model::encode_inferred(
                d,
                d.root(),
                p.inferred_nesting_budget,
            )?)?;
            if logs.iter().any(|log| {
                !(1..=1024).contains(&log.key.chars().count())
                    || log.size_bytes < 0
                    || log.sha256.len() != 64
                    || !log
                        .sha256
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            }) {
                return Err(ModelEncodeError::InvalidNode);
            }
            Ok(logs)
        }
        Some(Node::Object(fields)) if fields.is_empty() => Ok(Vec::new()),
        Some(Node::String(text)) if text.codepoints().is_empty() => Ok(Vec::new()),
        _ => Err(ModelEncodeError::InvalidNode),
    }
}
fn failure(f: &Failure, p: ResponseContext) -> Result<cannery_row__attempts__routes__FailureOut> {
    Ok(cannery_row__attempts__routes__FailureOut {
        stage: f.stage.as_str().to_owned(),
        code: convert(&f.code)?,
        reason: convert(&f.reason)?,
        details: mapping(&f.details, p, false)?.ok_or(ModelEncodeError::InvalidNode)?,
        created_at: crate::timestamps::public_timestamp(f.created_at),
        requeued: Some(f.requeued),
        log_refs: Some(logs(&f.log_refs, p)?),
    })
}
pub(crate) fn detail(
    a: &Attempt,
    artifacts: &[Artifact],
    failures: &[Failure],
    sheet: &StoredJson,
    pins: crate::context_bundle::Pins,
    p: ResponseContext,
) -> Result<Vec<u8>> {
    let base = base(a, p)?;
    encode(&AttemptDetail {
        id: base.id,
        r#ref: base.r#ref,
        number: base.number,
        sequence: base.sequence,
        state: base.state,
        track: base.track,
        hypothesis_revision: base.hypothesis_revision,
        science_revision: base.science_revision,
        producer: base.producer,
        mode: base.mode,
        workflow: base.workflow,
        claimed_by: base.claimed_by,
        via_channel: base.via_channel,
        via_client: base.via_client,
        predecessor_id: base.predecessor_id,
        lease_generation: base.lease_generation,
        lease_expires_at: base.lease_expires_at,
        claimed_at: base.claimed_at,
        started_at: base.started_at,
        submitted_at: base.submitted_at,
        finished_at: base.finished_at,
        origin: base.origin,
        source_ref: base.source_ref,
        imported: base.imported,
        artifacts: artifacts
            .iter()
            .map(artifact_model)
            .collect::<Result<_>>()?,
        failures: failures
            .iter()
            .map(|f| failure(f, p))
            .collect::<Result<_>>()?,
        claimed_sheet: mapping(sheet, p, true)?,
        brief: pins.brief,
        plan: pins.plan,
        context: pins.context,
    })
}
pub(crate) fn page(
    rows: &[Attempt],
    limit: usize,
    sequence: bool,
    p: ResponseContext,
) -> Result<Vec<u8>> {
    let items = rows
        .iter()
        .take(limit)
        .map(|a| base(a, p))
        .collect::<Result<Vec<_>>>()?;
    let last = if rows.len() > limit {
        rows.get(limit.saturating_sub(1))
    } else {
        None
    };
    if sequence {
        encode(&Page_AttemptOut_int_ {
            items,
            next_before: last.map(|a| i64::from(a.sequence)),
        })
    } else {
        encode(&Page_AttemptOut_UUID_ {
            items,
            next_before: last.map(|a| a.id.to_string()),
        })
    }
}

#[allow(unused_imports)]
use cannery_core::text::TextExt as _;
