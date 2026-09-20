//! Ed25519 key material and its on-disk representation.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use ed25519_dalek::{SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use xpack_core::atomic;
use xpack_core::{Error, Result};

/// Length of an Ed25519 secret key seed, in bytes.
pub const SECRET_KEY_LEN: usize = 32;
/// Length of an Ed25519 public key, in bytes.
pub const PUBLIC_KEY_LEN: usize = 32;

/// Format version of the key files written by this build.
const KEY_FILE_VERSION: u32 = 1;
/// Algorithm tag recorded in key files, so a future migration is unambiguous.
const ALGORITHM: &str = "ed25519";

/// A public verification key.
#[derive(Clone)]
pub struct PublicKey(VerifyingKey);

impl PublicKey {
    /// Parses a key from its 32 raw bytes.
    ///
    /// Rejects the low-order points that would make signature verification
    /// trivially forgeable.
    pub fn from_bytes(bytes: &[u8; PUBLIC_KEY_LEN]) -> Result<Self> {
        let key = VerifyingKey::from_bytes(bytes)
            .map_err(|e| Error::Integrity(format!("invalid ed25519 public key: {e}")))?;
        if key.is_weak() {
            return Err(Error::Integrity(
                "ed25519 public key is a low-order point and cannot be trusted".to_string(),
            ));
        }
        Ok(Self(key))
    }

    /// Parses a 64-character hex-encoded key.
    pub fn parse_hex(text: &str) -> Result<Self> {
        let mut raw = [0u8; PUBLIC_KEY_LEN];
        hex::decode_to_slice(text.trim(), &mut raw).map_err(|e| {
            Error::invalid("public key", format!("{text:?} is not 64 hex characters: {e}"))
        })?;
        Self::from_bytes(&raw)
    }

    /// The raw key bytes.
    pub fn to_bytes(&self) -> [u8; PUBLIC_KEY_LEN] {
        self.0.to_bytes()
    }

    /// Lowercase hex encoding, the form used in files and on the CLI.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0.to_bytes())
    }

    /// A short, human-comparable fingerprint for logs and prompts.
    ///
    /// Only ever an aid to a human eyeballing two values; every programmatic
    /// comparison uses the full key.
    pub fn fingerprint(&self) -> String {
        let hex = self.to_hex();
        let mut out = String::with_capacity(24);
        for (i, chunk) in hex.as_bytes().chunks(4).take(4).enumerate() {
            if i > 0 {
                out.push('-');
            }
            out.push_str(std::str::from_utf8(chunk).unwrap_or("????"));
        }
        out
    }

    pub(crate) fn inner(&self) -> &VerifyingKey {
        &self.0
    }

    /// Loads a public key from a JSON key file or a bare hex file.
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
        if let Ok(file) = serde_json::from_slice::<PublicKeyFile>(&bytes) {
            file.ensure_supported()?;
            return Self::parse_hex(&file.public_key);
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| Error::invalid("public key file", "is neither JSON nor hex text"))?;
        Self::parse_hex(&text)
    }

    /// Writes the key as a JSON key file.
    pub fn save(&self, path: &Path) -> Result<()> {
        atomic::write_json(
            path,
            &PublicKeyFile {
                format_version: KEY_FILE_VERSION,
                algorithm: ALGORITHM.to_string(),
                public_key: self.to_hex(),
            },
        )
    }
}

/// Compares keys in constant time.
impl PartialEq for PublicKey {
    fn eq(&self, other: &Self) -> bool {
        let (a, b) = (self.0.to_bytes(), other.0.to_bytes());
        let mut diff = 0u8;
        for (x, y) in a.iter().zip(b.iter()) {
            diff |= x ^ y;
        }
        diff == 0
    }
}

impl Eq for PublicKey {}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PublicKey").field(&self.fingerprint()).finish()
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl FromStr for PublicKey {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse_hex(s)
    }
}

/// A signing key pair. Only ever held by the publisher, never by a client.
pub struct KeyPair {
    signing: SigningKey,
}

impl KeyPair {
    /// Generates a fresh key pair from the operating system CSPRNG.
    ///
    /// An Ed25519 secret key *is* 32 uniformly random bytes, so seeding
    /// directly from `getrandom` is equivalent to any RNG-based constructor
    /// while removing a dependency whose API churns between releases.
    pub fn generate() -> Result<Self> {
        let mut seed = [0u8; SECRET_KEY_LEN];
        getrandom::fill(&mut seed).map_err(|e| {
            Error::Integrity(format!("operating system CSPRNG is unavailable: {e}"))
        })?;
        let signing = SigningKey::from_bytes(&seed);
        // Best-effort scrub; the seed is copied into `signing`, which zeroizes
        // itself on drop.
        seed.fill(0);
        Ok(Self { signing })
    }

    /// Rebuilds a key pair from its 32-byte seed.
    pub fn from_secret_bytes(seed: &[u8; SECRET_KEY_LEN]) -> Self {
        Self { signing: SigningKey::from_bytes(seed) }
    }

    /// The matching public key.
    pub fn public(&self) -> PublicKey {
        PublicKey(self.signing.verifying_key())
    }

    pub(crate) fn inner(&self) -> &SigningKey {
        &self.signing
    }

    /// Loads a key pair from a JSON key file.
    ///
    /// The stored public key is recomputed from the secret and compared, which
    /// catches a truncated or hand-edited key file before it produces
    /// signatures nobody can verify.
    pub fn load(path: &Path) -> Result<Self> {
        let file: SecretKeyFile = atomic::read_json(path)?;
        file.ensure_supported()?;

        let mut seed = [0u8; SECRET_KEY_LEN];
        hex::decode_to_slice(file.private_key.trim(), &mut seed)
            .map_err(|e| Error::invalid("private key", format!("is not 64 hex characters: {e}")))?;
        let pair = Self::from_secret_bytes(&seed);
        seed.fill(0);

        if let Some(declared) = &file.public_key {
            let declared = PublicKey::parse_hex(declared)?;
            if declared != pair.public() {
                return Err(Error::Integrity(format!(
                    "key file {} is corrupt: its public key does not match its private key",
                    path.display()
                )));
            }
        }
        Ok(pair)
    }

    /// Writes the key pair, restricting it to the owner where possible.
    ///
    /// On Unix the file is created with mode `0600`. Windows has no equivalent
    /// one-call restriction through `std`, so the file inherits the directory
    /// ACL; the CLI warns about this when generating keys.
    pub fn save(&self, path: &Path) -> Result<()> {
        let file = SecretKeyFile {
            format_version: KEY_FILE_VERSION,
            algorithm: ALGORITHM.to_string(),
            private_key: hex::encode(self.signing.to_bytes()),
            public_key: Some(self.public().to_hex()),
        };
        atomic::write_json(path, &file)?;
        restrict_to_owner(path)
    }
}

impl fmt::Debug for KeyPair {
    /// Never prints secret material, even in a panic message.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyPair")
            .field("public", &self.public().fingerprint())
            .finish_non_exhaustive()
    }
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| Error::io(path, e))
}

// The signature has to match the Unix version, which genuinely can fail, so
// this one returns a `Result` it never uses. Clippy is right that the wrapper
// is unnecessary *here*; removing it would split the two into different
// signatures and push the platform branch out to every caller.
#[allow(clippy::unnecessary_wraps)]
#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) -> Result<()> {
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublicKeyFile {
    format_version: u32,
    algorithm: String,
    public_key: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SecretKeyFile {
    format_version: u32,
    algorithm: String,
    private_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    public_key: Option<String>,
}

macro_rules! impl_ensure_supported {
    ($t:ty) => {
        impl $t {
            fn ensure_supported(&self) -> Result<()> {
                if self.format_version > KEY_FILE_VERSION {
                    return Err(Error::UnsupportedFormatVersion {
                        found: self.format_version,
                        supported: KEY_FILE_VERSION,
                    });
                }
                if self.algorithm != ALGORITHM {
                    return Err(Error::invalid(
                        "key file",
                        format!(
                            "algorithm {:?} is not supported, expected {ALGORITHM:?}",
                            self.algorithm
                        ),
                    ));
                }
                Ok(())
            }
        }
    };
}

impl_ensure_supported!(PublicKeyFile);
impl_ensure_supported!(SecretKeyFile);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_pairs_are_distinct() {
        let a = KeyPair::generate().unwrap();
        let b = KeyPair::generate().unwrap();
        assert_ne!(a.public(), b.public(), "CSPRNG returned the same key twice");
    }

    #[test]
    fn key_pair_survives_a_save_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signing.json");
        let original = KeyPair::generate().unwrap();
        original.save(&path).unwrap();
        assert_eq!(KeyPair::load(&path).unwrap().public(), original.public());
    }

    #[cfg(unix)]
    #[test]
    fn private_key_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signing.json");
        KeyPair::generate().unwrap().save(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "private key is readable by others");
    }

    #[test]
    fn detects_a_key_file_whose_halves_disagree() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signing.json");
        KeyPair::generate().unwrap().save(&path).unwrap();

        let mut file: SecretKeyFile = atomic::read_json(&path).unwrap();
        file.public_key = Some(KeyPair::generate().unwrap().public().to_hex());
        atomic::write_json(&path, &file).unwrap();

        assert!(KeyPair::load(&path).unwrap_err().to_string().contains("corrupt"));
    }

    #[test]
    fn public_key_reads_both_json_and_bare_hex() {
        let dir = tempfile::tempdir().unwrap();
        let pair = KeyPair::generate().unwrap();

        let json_path = dir.path().join("key.json");
        pair.public().save(&json_path).unwrap();
        assert_eq!(PublicKey::load(&json_path).unwrap(), pair.public());

        let hex_path = dir.path().join("key.hex");
        std::fs::write(&hex_path, format!("{}\n", pair.public().to_hex())).unwrap();
        assert_eq!(PublicKey::load(&hex_path).unwrap(), pair.public());
    }

    #[test]
    fn rejects_the_all_zero_low_order_public_key() {
        // The identity point accepts a signature from any "signer".
        assert!(PublicKey::from_bytes(&[0u8; PUBLIC_KEY_LEN]).is_err());
    }

    #[test]
    fn rejects_malformed_hex_keys() {
        assert!(PublicKey::parse_hex("").is_err());
        assert!(PublicKey::parse_hex("abcd").is_err());
        assert!(PublicKey::parse_hex(&"z".repeat(64)).is_err());
    }

    #[test]
    fn debug_output_never_contains_secret_material() {
        let pair = KeyPair::generate().unwrap();
        let secret_hex = hex::encode(pair.inner().to_bytes());
        assert!(!format!("{pair:?}").contains(&secret_hex));
    }

    #[test]
    fn fingerprints_are_short_and_stable() {
        let pair = KeyPair::generate().unwrap();
        let fp = pair.public().fingerprint();
        assert_eq!(fp.len(), 19, "expected 4 groups of 4 hex chars joined by dashes");
        assert_eq!(fp, pair.public().fingerprint());
    }
}
