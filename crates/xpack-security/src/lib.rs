//! Cryptographic services for xPack.
//!
//! # Threat model
//!
//! The adversary is assumed to control the network and the update server: they
//! can serve any bytes, replay any past artefact, and rewrite the update index
//! freely. They are assumed **not** to hold the publisher's private key.
//!
//! Under that model the only thing a client may trust is an Ed25519 signature
//! that verifies against a key pinned in *its own* installation. In particular:
//!
//! * A public key shipped inside a package proves nothing — it is exactly as
//!   forgeable as the rest of the package. Trust is established once, at
//!   install time, and pinned in [`TrustStore`]; updates must verify against
//!   that pin.
//! * The signature covers the raw manifest bytes, and the manifest covers a
//!   SHA-256 of every payload file, so signing one small document
//!   transitively authenticates the whole archive.
//! * Transport security (TLS) is defence in depth, never the trust root.
//!
//! # Known limitations
//!
//! What follows is what this crate does **not** protect against. Verification
//! confirms the implementation does what it claims; it does not make these go
//! away. They are appropriate for a first version and must be understood
//! before anyone depends on this in production.
//!
//! ## Key compromise is unrecoverable
//!
//! Trust rests on a single pinned key. There is no threshold signing and no
//! role separation, so a leaked publisher key cannot be rotated out through
//! the update channel — every installation must be re-pinned out of band,
//! which for a deployed application usually means telling users to reinstall.
//!
//! *Fix:* separate signing roles with independent keys, so compromising the
//! online key does not allow replacing arbitrary content, plus threshold
//! signatures requiring k of n keys. This is what The Update Framework (TUF)
//! exists to solve.
//!
//! ## No freeze-attack protection
//!
//! An attacker who controls the server can withhold updates indefinitely.
//! Clients keep running a known-vulnerable version and cannot tell the
//! difference between "no update exists" and "an update is being hidden from
//! me", because nothing they hold has an expiry.
//!
//! *Fix:* signed metadata carrying an expiry timestamp, so a client can detect
//! that its view of the repository has gone stale.
//!
//! ## No revocation
//!
//! [`TrustStore::revoke`] removes a local pin, and that is all. There is no
//! revocation list and no online status protocol, so a key known to be
//! compromised keeps verifying on every installation that has not been
//! individually updated.
//!
//! *Fix:* signed revocation metadata distributed through the same channel as
//! updates, checked before a signature is accepted.
//!
//! ## Trust on first use trusts the first download
//!
//! Pinning the key that arrives with the first package gives the same
//! guarantee as SSH host keys: everything after the first contact is
//! protected, the first contact is not. An attacker positioned during the
//! initial install can pin their own key and will then be trusted forever.
//!
//! *Mitigation, available today:* supply the expected key explicitly at
//! install time. Trust on first use must always be requested deliberately and
//! is never the default, precisely because of this gap.

pub mod hash;
pub mod keys;
pub mod signature;
pub mod trust;

pub use hash::{Hasher, sha256, sha256_reader, verify_digest};
pub use keys::{KeyPair, PUBLIC_KEY_LEN, PublicKey, SECRET_KEY_LEN};
pub use signature::{SIGNATURE_LEN, Signature, sign, verify};
pub use trust::{TrustEntry, TrustStore};
