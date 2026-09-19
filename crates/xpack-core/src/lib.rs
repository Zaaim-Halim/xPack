//! Core domain types shared by every xPack component.
//!
//! This crate is deliberately free of I/O policy, networking and platform
//! syscalls. It owns the vocabulary — [`Manifest`], [`Version`], [`Platform`],
//! [`InstallState`] — plus the durable file primitives every other crate needs
//! to keep an installation recoverable after a crash.

pub mod atomic;
pub mod digest;
pub mod error;
pub mod manifest;
pub mod paths;
pub mod platform;
pub mod state;
pub mod version;

pub use digest::{Sha256Digest, SHA256_LEN};
pub use error::{Error, Result};
pub use manifest::{
    Application, FormatVersion, LaunchSpec, Manifest, PayloadFile, PayloadSpec, UpdateSpec,
    MANIFEST_ENTRY, SIGNATURE_ENTRY,
};
pub use paths::InstallPaths;
pub use platform::{Arch, Os, Platform};
pub use state::{InstallState, UpdatePhase, VersionRecord, VersionStatus};
pub use version::Version;
