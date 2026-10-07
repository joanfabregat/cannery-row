//! Opaque credentials compatible with Python's `secrets.token_urlsafe(32)`.
use crate::error::{IdentityError, Result};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use cannery_core::principal::Secret;
use sha2::{Digest, Sha256};
use std::fmt;
use subtle::ConstantTimeEq;

pub const PERSONAL_PREFIX: &str = "cr_pat_";
pub const SERVICE_PREFIX: &str = "cr_svc_";
pub const SESSION_PREFIX: &str = "cr_ses_";
pub const JOB_PREFIX: &str = "cr_job_";
pub const LEASE_PREFIX: &str = "cr_lease_";
pub const UPLOAD_PREFIX: &str = "cr_upl_";

/// The digest has the same confidentiality as the credential and is redacted.
pub struct NewSecret {
    plaintext: Secret,
    digest: [u8; 32],
}
impl fmt::Debug for NewSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NewSecret([redacted])")
    }
}
impl NewSecret {
    #[must_use]
    pub fn plaintext(&self) -> &Secret {
        &self.plaintext
    }
    #[must_use]
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
    #[must_use]
    pub fn display_prefix(&self) -> &str {
        // The ASCII prefixes used by production are shorter than twelve bytes;
        // entropy always adds 43 ASCII bytes. Unknown Unicode prefixes remain
        // accepted like Python's helper; return twelve Unicode characters.
        let end = self
            .plaintext
            .expose()
            .char_indices()
            .nth(12)
            .map_or(self.plaintext.expose().len(), |(index, _)| index);
        &self.plaintext.expose()[..end]
    }
}
/// Mint 32 bytes of OS entropy without retaining the native random error.
///
/// # Errors
/// Returns a sanitized error if the OS entropy source fails.
pub fn new_secret(prefix: &str) -> Result<NewSecret> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| IdentityError::Random)?;
    Ok(from_bytes(prefix, &bytes))
}
fn from_bytes(prefix: &str, bytes: &[u8; 32]) -> NewSecret {
    let plaintext = format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes));
    let digest = digest(&plaintext);
    NewSecret {
        plaintext: Secret::new(plaintext),
        digest,
    }
}
#[must_use]
pub fn digest(plaintext: &str) -> [u8; 32] {
    Sha256::digest(plaintext.as_bytes()).into()
}

/// Compare a stored whole-token digest without exposing credential bytes.
#[must_use]
pub fn matches_digest(held: &[u8], plaintext: &str) -> bool {
    bool::from(held.ct_eq(&digest(plaintext)))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn six_prefixes_use_exact_encoding_and_whole_plaintext_digest() {
        for prefix in [
            PERSONAL_PREFIX,
            SERVICE_PREFIX,
            SESSION_PREFIX,
            JOB_PREFIX,
            LEASE_PREFIX,
            UPLOAD_PREFIX,
        ] {
            let secret = from_bytes(prefix, &[0; 32]);
            assert_eq!(
                secret.plaintext().expose(),
                format!("{prefix}{}", "A".repeat(43))
            );
            assert_eq!(secret.digest(), &digest(secret.plaintext().expose()));
            assert_ne!(secret.digest(), &digest(&"A".repeat(43)));
            assert_eq!(secret.display_prefix(), &secret.plaintext().expose()[..12]);
            assert_eq!(format!("{secret:?}"), "NewSecret([redacted])");
        }
    }
    #[test]
    fn actual_os_mint_is_urlsafe_unpadded_and_redacted() -> Result<()> {
        for prefix in [
            PERSONAL_PREFIX,
            SERVICE_PREFIX,
            SESSION_PREFIX,
            JOB_PREFIX,
            LEASE_PREFIX,
            UPLOAD_PREFIX,
        ] {
            let first = new_secret(prefix)?;
            let second = new_secret(prefix)?;
            let encoded = first.plaintext().expose().strip_prefix(prefix);
            assert!(encoded.is_some_and(|encoded| {
                encoded.len() == 43
                    && encoded
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            }));
            // Boolean assertions prevent a failure from printing either secret.
            let distinct = first.plaintext().expose() != second.plaintext().expose();
            assert!(distinct);
            assert!(!format!("{first:?}").contains(first.plaintext().expose()));
            let hash_matches = first.digest() == &digest(first.plaintext().expose());
            assert!(hash_matches);
        }
        Ok(())
    }
    #[test]
    fn digest_matches_known_utf8_sha256() {
        assert_eq!(
            digest("abc"),
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad
            ]
        );
    }
}
