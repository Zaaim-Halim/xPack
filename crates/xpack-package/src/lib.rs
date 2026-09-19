//! The `.xpkg` container: writing, reading, verifying and extracting.
//!
//! # Layout
//!
//! An `.xpkg` is a ZIP archive with a fixed internal shape:
//!
//! ```text
//! manifest.json      the signed document (see xpack_core::manifest)
//! manifest.sig       detached Ed25519 signature over manifest.json's bytes
//! payload/...        every application file, mirrored in the manifest
//! ```
//!
//! ZIP is an implementation detail. Consumers only ever see `.xpkg`, so the
//! container can be replaced in a later `formatVersion` without changing the
//! public surface.
//!
//! # Verification order
//!
//! [`PackageReader`] enforces one order and offers no way around it:
//!
//! 1. read `manifest.json`'s **raw bytes**,
//! 2. verify the detached signature over those bytes against a pinned key,
//! 3. only then parse the manifest,
//! 4. extract each payload file to a staging directory, streaming its
//!    SHA-256 and comparing against the signed manifest,
//! 5. promote staging into place only once every file has matched.
//!
//! Extracting first and verifying afterwards would mean attacker-controlled
//! bytes had already touched the filesystem; there is no API here that allows
//! it.

pub mod entry_path;
pub mod reader;
pub mod writer;

pub use entry_path::{SafePath, safe_payload_path};
pub use reader::{PackageReader, VerifiedPackage};
pub use writer::{PackageBuilder, PackedPackage};
