//! Detached Ed25519 signatures over raw manifest bytes.
//!
//! # The rule that matters
//!
//! Verification runs over the **exact bytes read from the archive**, before
//! those bytes are parsed. The tempting alternative — parse the manifest,
//! re-serialise it, verify the result — is broken: key order, whitespace,
//! number formatting and Unicode escaping all differ between serialisers and
//! between versions of the same serialiser. Any such drift turns a valid
//! package into an invalid one (or, worse, invites a lenient comparison that
//! an attacker can exploit).

use ed25519_dalek::Signer;
use xpack_core::{Error, Result};

use crate::keys::{KeyPair, PublicKey};

/// Length of an Ed25519 signature, in bytes.
pub const SIGNATURE_LEN: usize = 64;

/// A detached Ed25519 signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signature([u8; SIGNATURE_LEN]);

impl Signature {
    /// Wraps 64 raw signature bytes.
    pub fn from_bytes(bytes: [u8; SIGNATURE_LEN]) -> Self {
        Self(bytes)
    }

    /// Parses a 128-character hex signature, tolerating surrounding whitespace.
    pub fn parse_hex(text: &str) -> Result<Self> {
        let mut raw = [0u8; SIGNATURE_LEN];
        hex::decode_to_slice(text.trim(), &mut raw).map_err(|e| {
            Error::Integrity(format!("signature is not 128 hex characters: {e}"))
        })?;
        Ok(Self(raw))
    }

    /// The raw signature bytes.
    pub fn to_bytes(self) -> [u8; SIGNATURE_LEN] {
        self.0
    }

    /// Lowercase hex encoding, the form stored in the archive.
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }
}

/// Signs `message` with the publisher's key.
pub fn sign(key: &KeyPair, message: &[u8]) -> Signature {
    Signature(key.inner().sign(message).to_bytes())
}

/// Verifies a detached signature over `message`.
///
/// Uses `verify_strict`, which additionally rejects small-order public keys and
/// non-canonically encoded scalars. Plain `verify` accepts signatures that are
/// malleable — two distinct byte strings valid for one message — which breaks
/// any scheme that treats a signature as an identifier.
pub fn verify(key: &PublicKey, message: &[u8], signature: &Signature) -> Result<()> {
    let parsed = ed25519_dalek::Signature::from_bytes(&signature.0);
    key.inner().verify_strict(message, &parsed).map_err(|_| {
        Error::Integrity(format!(
            "signature does not verify against trusted key {}",
            key.fingerprint()
        ))
    })
}

/// Verifies against any key in `trusted`, returning the one that matched.
///
/// Accepting a set of keys is what makes key rotation possible: the publisher
/// adds the new key to clients in release N, then starts signing with it in
/// release N+1, with no flag day.
pub fn verify_any<'k>(
    trusted: &'k [PublicKey],
    message: &[u8],
    signature: &Signature,
) -> Result<&'k PublicKey> {
    if trusted.is_empty() {
        return Err(Error::Integrity(
            "no trusted signing keys are configured for this installation".to_string(),
        ));
    }
    for key in trusted {
        if verify(key, message, signature).is_ok() {
            return Ok(key);
        }
    }
    Err(Error::Integrity(format!(
        "signature does not verify against any of the {} trusted key(s)",
        trusted.len()
    )))
}

impl std::fmt::Display for Signature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> KeyPair {
        KeyPair::generate().unwrap()
    }

    #[test]
    fn a_genuine_signature_verifies() {
        let key = pair();
        let msg = b"{\"formatVersion\":1}";
        verify(&key.public(), msg, &sign(&key, msg)).unwrap();
    }

    #[test]
    fn rejects_a_single_flipped_message_bit() {
        let key = pair();
        let msg = b"{\"version\":\"1.2.0\"}".to_vec();
        let sig = sign(&key, &msg);

        let mut tampered = msg.clone();
        tampered[2] ^= 0x01;
        let err = verify(&key.public(), &tampered, &sig).unwrap_err();
        assert!(err.is_integrity_failure());
    }

    #[test]
    fn rejects_a_single_flipped_signature_bit() {
        let key = pair();
        let msg = b"payload";
        let mut raw = sign(&key, msg).to_bytes();
        raw[0] ^= 0x01;
        assert!(verify(&key.public(), msg, &Signature::from_bytes(raw)).is_err());
    }

    #[test]
    fn rejects_a_signature_from_a_different_key() {
        let msg = b"payload";
        assert!(verify(&pair().public(), msg, &sign(&pair(), msg)).is_err());
    }

    #[test]
    fn rejects_a_truncated_message_with_a_valid_prefix_signature() {
        let key = pair();
        let sig = sign(&key, b"full message");
        assert!(verify(&key.public(), b"full", &sig).is_err());
    }

    #[test]
    fn hex_round_trips() {
        let key = pair();
        let sig = sign(&key, b"x");
        assert_eq!(Signature::parse_hex(&sig.to_hex()).unwrap(), sig);
        assert_eq!(Signature::parse_hex(&format!("  {}\n", sig.to_hex())).unwrap(), sig);
    }

    #[test]
    fn rejects_malformed_signature_encodings() {
        assert!(Signature::parse_hex("").is_err());
        assert!(Signature::parse_hex("ab").is_err());
    }

    #[test]
    fn key_rotation_accepts_either_configured_key() {
        let (old, new) = (pair(), pair());
        let trusted = vec![old.public(), new.public()];
        let msg = b"release N+1";

        let matched = verify_any(&trusted, msg, &sign(&new, msg)).unwrap();
        assert_eq!(*matched, new.public());
        verify_any(&trusted, msg, &sign(&old, msg)).unwrap();
        assert!(verify_any(&trusted, msg, &sign(&pair(), msg)).is_err());
    }

    #[test]
    fn an_empty_trust_store_trusts_nothing() {
        let key = pair();
        let err = verify_any(&[], b"x", &sign(&key, b"x")).unwrap_err();
        assert!(err.to_string().contains("no trusted signing keys"));
    }
}
