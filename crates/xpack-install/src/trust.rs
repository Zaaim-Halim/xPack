//! Establishing which signing key an installation accepts.

use std::path::Path;

use xpack_core::{Error, Result};
use xpack_package::{PackageReader, VerifiedPackage};
use xpack_platform::InstallLock;
use xpack_security::keys::PublicKey;
use xpack_security::trust::TrustStore;

/// How the first signing key enters an installation.
#[derive(Debug, Clone)]
pub enum TrustDecision {
    /// Verify against the keys already pinned. The only mode for updates.
    UsePinned,
    /// Pin this key, supplied out of band by the operator.
    ///
    /// The only mode that resists an attacker who controls the very first
    /// download, and what production deployments should use.
    Explicit(PublicKey),
    /// Pin whichever key signed this package.
    ///
    /// Trusts the first install and nothing after it, which is the guarantee
    /// SSH host keys give. Must be requested deliberately; it is never a
    /// default, because it cannot detect an attacker present at first contact.
    OnFirstUse,
}

/// Opens a package and verifies it according to `decision`.
///
/// Pins the key *before* verifying, so the same trust store governs this
/// install and every later update.
pub fn open_and_verify(
    package: &Path,
    lock: &InstallLock,
    decision: &TrustDecision,
) -> Result<VerifiedPackage> {
    let trust_file = lock.paths().trust_file();
    let mut store = TrustStore::load_or_empty(&trust_file)?;

    match decision {
        TrustDecision::UsePinned => {
            if store.is_empty() {
                return Err(Error::Integrity(
                    "this installation trusts no signing keys; supply one explicitly or \
                     request trust-on-first-use"
                        .to_string(),
                ));
            }
        }
        TrustDecision::Explicit(key) => {
            if !store.is_empty() && !store.contains(key) {
                // Silently adding a second publisher's key would turn one
                // compromised operator action into permanent trust.
                tracing::warn!(
                    key = %key.fingerprint(),
                    "adding a signing key to an installation that already trusts others"
                );
            }
            store.trust(key, "explicit");
            store.save(&trust_file)?;
        }
        TrustDecision::OnFirstUse => {
            if store.is_empty() {
                let mut reader = PackageReader::open(package)?;
                // Unverified by definition: this key is the thing being
                // decided on, and pinning it is what first-use means.
                let key = reader.declared_signing_key_unverified()?;
                tracing::warn!(
                    key = %key.fingerprint(),
                    "pinning a signing key on first use; this trusts the first download"
                );
                store.trust(&key, "first-use");
                store.save(&trust_file)?;
            } else {
                // Trust already exists, so first use has passed. Falling back
                // to the pinned keys is the safe reading of the request.
                tracing::debug!("installation already has pinned keys; ignoring first-use request");
            }
        }
    }

    PackageReader::open(package)?.verify(&store)
}
