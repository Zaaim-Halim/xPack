//! The per-installation store of pinned signing keys.
//!
//! # Why pinning, and why trust-on-first-use
//!
//! A package cannot vouch for itself: the public key inside it is forgeable by
//! whoever forged the package. So trust has to enter the system exactly once,
//! out of band, and then be *pinned* — every later update must verify against
//! the key already on disk.
//!
//! xPack supports two ways to establish that first pin:
//!
//! * **Explicit** — the operator passes the expected key, obtained from the
//!   publisher's website or their configuration management. This is the only
//!   mode that resists an attacker who controls the very first download, and
//!   it is what production deployments should use.
//! * **Trust on first use** — the key travelling with the first package is
//!   pinned. This trusts the initial install and nothing after it, which is
//!   the same guarantee SSH host keys give. It must be requested explicitly;
//!   it is never the default.
//!
//! Once pinned, adding a key is an operator decision, not something a package
//! can cause. That is what makes a compromised update server unable to swap in
//! its own signing key.

use std::path::Path;

use serde::{Deserialize, Serialize};
use xpack_core::atomic;
use xpack_core::{Error, Result};

use crate::keys::PublicKey;
use crate::signature::{self, Signature};

/// Format version of the trust store document.
const TRUST_FORMAT_VERSION: u32 = 1;

/// One pinned key, with provenance for auditing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustEntry {
    /// Hex-encoded Ed25519 public key.
    pub public_key: String,
    /// How this key came to be trusted, e.g. `"explicit"` or `"first-use"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Free-form operator note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// The set of keys whose signatures this installation accepts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustStore {
    /// Format version of this document.
    pub format_version: u32,
    /// Pinned keys, in the order they were added.
    #[serde(default)]
    pub keys: Vec<TrustEntry>,
}

impl Default for TrustStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TrustStore {
    /// Creates an empty store, which trusts nothing.
    pub fn new() -> Self {
        Self { format_version: TRUST_FORMAT_VERSION, keys: Vec::new() }
    }

    /// Loads a store, or returns an empty one when the file does not exist.
    ///
    /// "Absent" and "empty" both mean *trust nothing*, so they are safely
    /// equivalent — an installation with no trust file cannot be tricked into
    /// accepting an update.
    pub fn load_or_empty(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let store: Self = atomic::read_json(path)?;
        if store.format_version > TRUST_FORMAT_VERSION {
            return Err(Error::UnsupportedFormatVersion {
                found: store.format_version,
                supported: TRUST_FORMAT_VERSION,
            });
        }
        // Reject malformed keys at load time rather than at verification time,
        // so a corrupted store fails loudly instead of silently trusting less.
        store.public_keys()?;
        Ok(store)
    }

    /// Writes the store atomically.
    pub fn save(&self, path: &Path) -> Result<()> {
        atomic::write_json(path, self)
    }

    /// Parses every pinned key.
    pub fn public_keys(&self) -> Result<Vec<PublicKey>> {
        self.keys.iter().map(|e| PublicKey::parse_hex(&e.public_key)).collect()
    }

    /// Returns `true` when nothing is pinned.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Returns `true` when `key` is already pinned.
    pub fn contains(&self, key: &PublicKey) -> bool {
        self.public_keys().is_ok_and(|keys| keys.iter().any(|k| k == key))
    }

    /// Pins a key. Re-adding an existing key is a no-op, not an error.
    pub fn trust(&mut self, key: &PublicKey, origin: impl Into<String>) {
        if self.contains(key) {
            return;
        }
        self.keys.push(TrustEntry {
            public_key: key.to_hex(),
            origin: Some(origin.into()),
            comment: None,
        });
    }

    /// Unpins a key, returning `true` when one was removed.
    pub fn revoke(&mut self, key: &PublicKey) -> bool {
        let target = key.to_hex();
        let before = self.keys.len();
        self.keys.retain(|e| !e.public_key.eq_ignore_ascii_case(&target));
        self.keys.len() != before
    }

    /// Verifies `message` against the pinned keys.
    ///
    /// This is the single choke point every install and update must pass
    /// through. It fails when the store is empty, so a missing trust file can
    /// never be mistaken for "trust everything".
    pub fn verify(&self, message: &[u8], signature: &Signature) -> Result<PublicKey> {
        let keys = self.public_keys()?;
        signature::verify_any(&keys, message, signature).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::KeyPair;
    use crate::signature::sign;

    #[test]
    fn a_new_store_trusts_nothing() {
        let key = KeyPair::generate().unwrap();
        let store = TrustStore::new();
        assert!(store.is_empty());
        assert!(store.verify(b"m", &sign(&key, b"m")).is_err());
    }

    #[test]
    fn a_missing_file_behaves_like_an_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = TrustStore::load_or_empty(&dir.path().join("absent.json")).unwrap();
        assert!(store.is_empty());
    }

    #[test]
    fn a_pinned_key_verifies_its_own_signatures() {
        let key = KeyPair::generate().unwrap();
        let mut store = TrustStore::new();
        store.trust(&key.public(), "explicit");
        assert_eq!(store.verify(b"m", &sign(&key, b"m")).unwrap(), key.public());
    }

    #[test]
    fn an_unpinned_key_is_rejected_even_with_a_valid_signature() {
        // The core property: a validly signed package from the wrong publisher
        // must not install.
        let attacker = KeyPair::generate().unwrap();
        let mut store = TrustStore::new();
        store.trust(&KeyPair::generate().unwrap().public(), "explicit");
        assert!(store.verify(b"m", &sign(&attacker, b"m")).is_err());
    }

    #[test]
    fn trusting_is_idempotent() {
        let key = KeyPair::generate().unwrap();
        let mut store = TrustStore::new();
        store.trust(&key.public(), "explicit");
        store.trust(&key.public(), "first-use");
        assert_eq!(store.keys.len(), 1);
    }

    #[test]
    fn revoking_stops_later_verification() {
        let key = KeyPair::generate().unwrap();
        let mut store = TrustStore::new();
        store.trust(&key.public(), "explicit");
        assert!(store.revoke(&key.public()));
        assert!(!store.revoke(&key.public()));
        assert!(store.verify(b"m", &sign(&key, b"m")).is_err());
    }

    #[test]
    fn survives_a_save_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let key = KeyPair::generate().unwrap();

        let mut store = TrustStore::new();
        store.trust(&key.public(), "explicit");
        store.save(&path).unwrap();

        let loaded = TrustStore::load_or_empty(&path).unwrap();
        assert!(loaded.contains(&key.public()));
        loaded.verify(b"m", &sign(&key, b"m")).unwrap();
    }

    #[test]
    fn a_corrupt_store_fails_loudly_rather_than_trusting_less() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        std::fs::write(&path, br#"{"formatVersion":1,"keys":[{"publicKey":"not-a-key"}]}"#).unwrap();
        assert!(TrustStore::load_or_empty(&path).is_err());
    }

    #[test]
    fn rejects_a_store_written_by_a_newer_xpack() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        std::fs::write(&path, br#"{"formatVersion":99,"keys":[]}"#).unwrap();
        assert!(matches!(
            TrustStore::load_or_empty(&path),
            Err(Error::UnsupportedFormatVersion { .. })
        ));
    }
}
