//! A fixed-width SHA-256 digest with constant-time equality.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};

/// Length of a SHA-256 digest in bytes.
pub const SHA256_LEN: usize = 32;

/// A SHA-256 digest, serialised as a lowercase hex string.
#[derive(Debug, Clone, Copy)]
pub struct Sha256Digest([u8; SHA256_LEN]);

impl Sha256Digest {
    /// Wraps raw digest bytes.
    pub fn from_bytes(bytes: [u8; SHA256_LEN]) -> Self {
        Self(bytes)
    }

    /// Borrows the raw digest bytes.
    pub fn as_bytes(&self) -> &[u8; SHA256_LEN] {
        &self.0
    }

    /// Parses a 64-character lowercase or uppercase hex digest.
    pub fn parse_hex(text: &str) -> Result<Self> {
        let mut out = [0u8; SHA256_LEN];
        hex::decode_to_slice(text, &mut out).map_err(|e| {
            Error::invalid("sha256", format!("{text:?} is not a 64-character hex digest: {e}"))
        })?;
        Ok(Self(out))
    }

    /// Renders the digest as lowercase hex.
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }
}

/// Compares digests in constant time.
///
/// A digest mismatch is an attacker-visible signal. Byte-wise short-circuit
/// comparison leaks how many leading bytes matched, which is the foundation of
/// a hash-forgery timing oracle, so equality is always data-independent.
impl PartialEq for Sha256Digest {
    fn eq(&self, other: &Self) -> bool {
        let mut diff = 0u8;
        for (a, b) in self.0.iter().zip(other.0.iter()) {
            diff |= a ^ b;
        }
        diff == 0
    }
}

impl Eq for Sha256Digest {}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl FromStr for Sha256Digest {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse_hex(s)
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse_hex(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZERO_HEX: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    #[test]
    fn hex_round_trips() {
        let d = Sha256Digest::from_bytes([0xab; SHA256_LEN]);
        assert_eq!(Sha256Digest::parse_hex(&d.to_hex()).unwrap(), d);
    }

    #[test]
    fn rejects_wrong_length_and_non_hex() {
        assert!(Sha256Digest::parse_hex("abcd").is_err());
        assert!(Sha256Digest::parse_hex(&"z".repeat(64)).is_err());
        assert!(Sha256Digest::parse_hex(&format!("{ZERO_HEX}00")).is_err());
    }

    #[test]
    fn differs_when_any_byte_differs() {
        let a = Sha256Digest::from_bytes([0u8; SHA256_LEN]);
        let mut raw = [0u8; SHA256_LEN];
        raw[SHA256_LEN - 1] = 1;
        assert_ne!(a, Sha256Digest::from_bytes(raw));
    }

    #[test]
    fn serialises_as_a_bare_hex_string() {
        let json = serde_json::to_string(&Sha256Digest::from_bytes([0u8; SHA256_LEN])).unwrap();
        assert_eq!(json, format!("\"{ZERO_HEX}\""));
    }
}
