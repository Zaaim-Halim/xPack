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
pub mod store;
pub mod version;

pub use digest::{SHA256_LEN, Sha256Digest};
pub use error::{Error, Result};
pub use manifest::{
    Application, FormatVersion, HealthSpec, LaunchSpec, MANIFEST_ENTRY, Manifest, PayloadFile,
    PayloadSpec, SIGNATURE_ENTRY, UpdateSpec,
};
pub use paths::InstallPaths;
pub use platform::{Arch, Os, Platform};
pub use state::{InstallState, UpdatePhase, VersionRecord, VersionStatus};
pub use store::Loaded;
pub use version::Version;
