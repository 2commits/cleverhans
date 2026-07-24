//! §14.2 payload signing: `X-CleverHans-Signature: t=<unix>,v1=<hex>`
//! where the signature is `HMAC-SHA256(key, "<t>." || body bytes)`, hex
//! lowercase. Optional and additive: hosts that ignore the header lose
//! nothing; hosts that verify gain payload integrity past TLS termination,
//! a bounded replay window, and a credential that never travels the wire.

use std::time::Duration;

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// The signature header name (lowercase, as delivered).
pub const SIGNATURE_HEADER: &str = "x-cleverhans-signature";

/// Default verification clock-skew tolerance (spec §14.2: 5 minutes).
pub const DEFAULT_SKEW: Duration = Duration::from_secs(300);

type HmacSha256 = Hmac<Sha256>;

fn mac(key: &[u8], timestamp: u64, body: &[u8]) -> HmacSha256 {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    mac
}

/// Builds the header value for the exact body bytes about to be sent.
#[must_use]
pub fn signature_header(key: &[u8], timestamp: u64, body: &[u8]) -> String {
    let digest = mac(key, timestamp, body).finalize().into_bytes();
    format!("t={timestamp},v1={}", hex::encode(digest))
}

/// Why a signature failed verification.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SignatureError {
    /// The header is not `t=<unix>,v1=<hex>`.
    #[error("malformed signature header")]
    Malformed,
    /// The timestamp is outside the skew window.
    #[error("signature timestamp outside the skew window")]
    Expired,
    /// No configured key produced this signature over this body.
    #[error("signature does not match the body")]
    Mismatch,
}

/// Verifies a header against the exact received body bytes. `keys` may hold
/// more than one key (dual-key grace window during rotation); any match
/// passes. Comparison is constant-time.
///
/// # Errors
///
/// [`SignatureError`] — hosts SHOULD map any variant to `401`.
pub fn verify_signature(
    keys: &[impl AsRef<[u8]>],
    header: &str,
    body: &[u8],
    now: u64,
    skew: Duration,
) -> Result<(), SignatureError> {
    let mut timestamp: Option<u64> = None;
    let mut signatures: Vec<Vec<u8>> = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", value)) => {
                timestamp = Some(value.parse().map_err(|_| SignatureError::Malformed)?);
            }
            Some(("v1", value)) => {
                signatures.push(hex::decode(value).map_err(|_| SignatureError::Malformed)?);
            }
            _ => return Err(SignatureError::Malformed),
        }
    }
    let timestamp = timestamp.ok_or(SignatureError::Malformed)?;
    if signatures.is_empty() {
        return Err(SignatureError::Malformed);
    }
    if now.abs_diff(timestamp) > skew.as_secs() {
        return Err(SignatureError::Expired);
    }
    for key in keys {
        for signature in &signatures {
            if mac(key.as_ref(), timestamp, body)
                .verify_slice(signature)
                .is_ok()
            {
                return Ok(());
            }
        }
    }
    Err(SignatureError::Mismatch)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"test-signing-key";
    const BODY: &[u8] = br#"{"kind":"execute","params":{}}"#;
    const T: u64 = 1_700_000_000;

    #[test]
    fn round_trips() {
        let header = signature_header(KEY, T, BODY);
        assert!(header.starts_with(&format!("t={T},v1=")));
        assert_eq!(
            verify_signature(&[KEY], &header, BODY, T + 60, DEFAULT_SKEW),
            Ok(())
        );
    }

    #[test]
    fn known_vector() {
        // Pinned so independent implementations can check against it
        // (cross-verified with Python's hmac/hashlib).
        assert_eq!(
            signature_header(KEY, T, BODY),
            "t=1700000000,v1=54043b28f3ce9c05dd923645ca289ac7cee7910b87042a03b29677cef8ffdf50"
        );
    }

    #[test]
    fn rejects_tampered_body_wrong_key_and_stale_timestamp() {
        let header = signature_header(KEY, T, BODY);
        assert_eq!(
            verify_signature(
                &[KEY],
                &header,
                br#"{"kind":"execute","params":{"x":1}}"#,
                T,
                DEFAULT_SKEW
            ),
            Err(SignatureError::Mismatch)
        );
        assert_eq!(
            verify_signature(&[b"other-key".as_slice()], &header, BODY, T, DEFAULT_SKEW),
            Err(SignatureError::Mismatch)
        );
        assert_eq!(
            verify_signature(&[KEY], &header, BODY, T + 301, DEFAULT_SKEW),
            Err(SignatureError::Expired)
        );
        assert_eq!(
            verify_signature(&[KEY], "v1=abcd", BODY, T, DEFAULT_SKEW),
            Err(SignatureError::Malformed)
        );
    }

    #[test]
    fn rotation_grace_accepts_either_key() {
        let header = signature_header(KEY, T, BODY);
        assert_eq!(
            verify_signature(
                &[b"new-key".as_slice(), KEY],
                &header,
                BODY,
                T,
                DEFAULT_SKEW
            ),
            Ok(())
        );
    }
}
