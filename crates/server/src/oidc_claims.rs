//! Post-signature OIDC checks with bounded numeric JWT dates.
use std::fmt;

use cannery_core::json::{Document, Node};
use num_bigint::BigInt;
use num_traits::{FromPrimitive, ToPrimitive};

/// Normalized identity. Debug deliberately omits claim values.
#[derive(Clone, Eq, PartialEq)]
pub struct Identity {
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub name: Option<String>,
}
impl fmt::Debug for Identity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Identity { claims: [redacted] }")
    }
}

/// Python's binary64 POSIX clock value, checked before invoking claim checks.
#[derive(Clone, Copy, Debug)]
pub struct ClockTime(f64);
impl ClockTime {
    #[must_use]
    pub fn new(seconds: f64) -> Option<Self> {
        (seconds.is_finite() && (seconds + 60.0).is_finite() && (seconds - 60.0).is_finite())
            .then_some(Self(seconds))
    }
}

pub struct ClaimContext<'a> {
    /// Configured client issuer, already normalized by the caller's rstrip('/').
    pub stored_issuer: &'a String,
    /// Raw discovery issuer, used for JWT claim comparison.
    pub token_issuer: &'a String,
    pub audience: &'a String,
    pub nonce: &'a String,
    pub now: ClockTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequiredClaim {
    Exp,
    Iat,
    Sub,
    Iss,
    Aud,
}
impl RequiredClaim {
    fn name(self) -> &'static str {
        match self {
            Self::Exp => "exp",
            Self::Iat => "iat",
            Self::Sub => "sub",
            Self::Iss => "iss",
            Self::Aud => "aud",
        }
    }
}

/// Every rejection here is caught by the source OIDC client and maps to LoginFailed(400).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaimRejection {
    PayloadShape,
    Missing(RequiredClaim),
    InvalidIat,
    InvalidNbf,
    InvalidExp,
    ImmatureIat,
    ImmatureNbf,
    Expired,
    IssuerType,
    IssuerMismatch,
    AudienceFormat,
    AudienceMismatch,
    SubjectType,
    JtiType,
    NonceMismatch,
}
impl ClaimRejection {
    #[must_use]
    pub fn python_exception(self) -> &'static str {
        match self {
            Self::Missing(_) => "MissingRequiredClaimError",
            Self::InvalidIat => "InvalidIssuedAtError",
            Self::PayloadShape | Self::InvalidNbf | Self::InvalidExp => "DecodeError",
            Self::ImmatureIat | Self::ImmatureNbf => "ImmatureSignatureError",
            Self::Expired => "ExpiredSignatureError",
            Self::IssuerType | Self::IssuerMismatch => "InvalidIssuerError",
            Self::AudienceFormat | Self::AudienceMismatch => "InvalidAudienceError",
            Self::SubjectType => "InvalidSubjectError",
            Self::JtiType => "InvalidJTIError",
            Self::NonceMismatch => "OIDCError",
        }
    }
    #[must_use]
    pub fn oidc_message(self) -> String {
        if self == Self::NonceMismatch {
            return "id_token nonce mismatch".to_owned();
        }
        let message = match self {
            Self::PayloadShape => "Invalid payload string: must be a json object".to_owned(),
            Self::Missing(claim) => format!("Token is missing the \"{}\" claim", claim.name()),
            Self::InvalidIat => "Issued At claim (iat) must be an integer.".to_owned(),
            Self::InvalidNbf => "Not Before claim (nbf) must be an integer.".to_owned(),
            Self::InvalidExp => "Expiration Time claim (exp) must be an integer.".to_owned(),
            Self::ImmatureIat => "The token is not yet valid (iat)".to_owned(),
            Self::ImmatureNbf => "The token is not yet valid (nbf)".to_owned(),
            Self::Expired => "Signature has expired".to_owned(),
            Self::IssuerType => "Payload Issuer (iss) must be a string".to_owned(),
            Self::IssuerMismatch => "Invalid issuer".to_owned(),
            Self::AudienceFormat => "Invalid claim format in token".to_owned(),
            Self::AudienceMismatch => "Audience doesn't match".to_owned(),
            Self::SubjectType => "Subject must be a string".to_owned(),
            Self::JtiType => "JWT ID must be a string".to_owned(),
            Self::NonceMismatch => "id_token nonce mismatch".to_owned(),
        };
        format!("invalid id_token: {message}")
    }
}
impl fmt::Display for ClaimRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.oidc_message())
    }
}
impl std::error::Error for ClaimRejection {}

/// Validate only after signature/key/algorithm verification has succeeded.
///
/// # Errors
/// Returns source-equivalent claim rejections; malformed JWT/JSON, key errors and
/// unhandled crypto/JWKS exceptions remain the caller's responsibility.
pub fn validate_claims(
    document: &Document,
    context: &ClaimContext<'_>,
) -> Result<Identity, ClaimRejection> {
    if !matches!(document.node(document.root()), Some(Node::Object(_))) {
        return Err(ClaimRejection::PayloadShape);
    }
    let field = |name: &str| {
        document
            .field(document.root(), name)
            .and_then(|id| document.node(id))
    };
    for claim in [
        RequiredClaim::Exp,
        RequiredClaim::Iat,
        RequiredClaim::Sub,
        RequiredClaim::Iss,
        RequiredClaim::Aud,
    ] {
        if matches!(field(claim.name()), None | Some(Node::Null)) {
            return Err(ClaimRejection::Missing(claim));
        }
    }
    // Comparing arbitrary Python integers to a float is exact, without first
    // rounding the integer to binary64. Floor yields the same integer boundary.
    let future =
        BigInt::from_f64((context.now.0 + 60.0).floor()).ok_or(ClaimRejection::InvalidIat)?;
    let expired =
        BigInt::from_f64((context.now.0 - 60.0).floor()).ok_or(ClaimRejection::InvalidExp)?;
    if numeric_date(field("iat")).ok_or(ClaimRejection::InvalidIat)? > future {
        return Err(ClaimRejection::ImmatureIat);
    }
    if let Some(value) = field("nbf")
        && numeric_date(Some(value)).ok_or(ClaimRejection::InvalidNbf)? > future
    {
        return Err(ClaimRejection::ImmatureNbf);
    }
    if numeric_date(field("exp")).ok_or(ClaimRejection::InvalidExp)? <= expired {
        return Err(ClaimRejection::Expired);
    }
    let Some(Node::String(issuer)) = field("iss") else {
        return Err(ClaimRejection::IssuerType);
    };
    if issuer != context.token_issuer {
        return Err(ClaimRejection::IssuerMismatch);
    }
    let audience = field("aud").ok_or(ClaimRejection::Missing(RequiredClaim::Aud))?;
    if !truthy(audience) {
        return Err(ClaimRejection::Missing(RequiredClaim::Aud));
    }
    let audience_matches = match audience {
        Node::String(value) => value == context.audience,
        Node::Array(items) => {
            let mut found = false;
            for &id in items {
                let Some(Node::String(value)) = document.node(id) else {
                    return Err(ClaimRejection::AudienceFormat);
                };
                found |= value == context.audience;
            }
            found
        }
        _ => return Err(ClaimRejection::AudienceFormat),
    };
    if !audience_matches {
        return Err(ClaimRejection::AudienceMismatch);
    }
    let Some(Node::String(subject)) = field("sub") else {
        return Err(ClaimRejection::SubjectType);
    };
    if field("jti").is_some_and(|value| !matches!(value, Node::String(_))) {
        return Err(ClaimRejection::JtiType);
    }
    if !matches!(field("nonce"), Some(Node::String(value)) if value == context.nonce) {
        return Err(ClaimRejection::NonceMismatch);
    }
    let text = |name| match field(name) {
        Some(Node::String(value)) => Some(value.clone()),
        _ => None,
    };
    Ok(Identity {
        issuer: context.stored_issuer.clone(),
        subject: subject.clone(),
        email: text("email"),
        email_verified: matches!(field("email_verified"), Some(Node::Bool(true))),
        name: text("name"),
    })
}

fn truthy(value: &Node) -> bool {
    match value {
        Node::Null => false,
        Node::Bool(value) => *value,
        Node::Integer(value) => value != &BigInt::from(0),
        Node::Float(value) => *value != 0.0,
        Node::String(value) => !value.is_empty(),
        Node::Array(value) => !value.is_empty(),
        Node::Object(value) => !value.is_empty(),
    }
}
fn numeric_date(value: Option<&Node>) -> Option<BigInt> {
    match value? {
        Node::Integer(value) => value.to_i64().map(BigInt::from),
        Node::Float(value) if value.is_finite() => {
            BigInt::from_f64(value.trunc()).filter(|number| number.to_i64().is_some())
        }
        _ => None,
    }
}
