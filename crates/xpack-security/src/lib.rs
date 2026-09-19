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

pub mod hash;
pub mod keys;
pub mod signature;
pub mod trust;

pub use hash::{sha256, sha256_reader, verify_digest};
pub use keys::{KeyPair, PublicKey, SECRET_KEY_LEN, PUBLIC_KEY_LEN};
pub use signature::{sign, verify, Signature, SIGNATURE_LEN};
pub use trust::{TrustEntry, TrustStore};
