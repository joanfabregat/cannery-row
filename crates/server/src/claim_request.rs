//! Pure claim body and principal mode selection, without controller authorization.
use crate::validation::{self, BodyInput, ValidationErrors};
use cannery_core::{
    errors::{DomainError, ErrorCode},
    principal::{Principal, ServiceKind},
};
use cannery_tracks::repo::TrackMode;
use num_bigint::BigInt;
use std::collections::BTreeSet;

/// Validated source `ClaimRequest`; values are excluded from diagnostics.
pub struct ClaimRequest {
    pub unit: Option<BigInt>,
    pub track: Option<String>,
    pub mode: Option<TrackMode>,
    pub fields_set: BTreeSet<String>,
}
impl std::fmt::Debug for ClaimRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ClaimRequest([redacted])")
    }
}
impl ClaimRequest {
    /// Validate a decoded required body, retaining the existing transport conventions.
    /// # Errors
    /// Returns ordered source model errors; transport decoding and authentication are separate.
    pub fn parse(input: BodyInput<'_>) -> Result<Self, ValidationErrors> {
        validation::validate_claim_request(input)
    }
}
/// Choose the caller's claim mode, without adding scope/project/role checks.
/// # Errors
/// Returns source `forbidden` messages for cross-mode claims.
pub fn claim_mode(
    principal: &Principal,
    requested: Option<TrackMode>,
) -> Result<TrackMode, DomainError> {
    let experimenter = matches!(principal, Principal::Service(service) if service.kind == ServiceKind::Experimenter);
    let mode = requested.unwrap_or(if experimenter {
        TrackMode::Workflow
    } else {
        TrackMode::Agent
    });
    if mode == TrackMode::Workflow && !experimenter {
        return Err(DomainError::new(
            ErrorCode::Forbidden,
            "only an experimenter service account (a Cannery Row runner) claims units of workflow tracks",
        ));
    }
    if mode == TrackMode::Agent && experimenter {
        return Err(DomainError::new(
            ErrorCode::Forbidden,
            "an experimenter claims only units of workflow tracks",
        ));
    }
    Ok(mode)
}
