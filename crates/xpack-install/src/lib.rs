//! Installing, activating, rolling back and removing applications.
//!
//! This crate turns a verified package into a working installation and manages
//! the lifecycle afterwards. Everything it does runs under an [`InstallLock`],
//! so two operations can never interleave.
//!
//! # Health checks are primitives, not policy
//!
//! Activation puts a version on probation. Deciding *when* probation has
//! passed — an exit code, an elapsed timer, a message from the application —
//! is the launcher's business, not the installer's. This crate therefore
//! exposes [`Installer::begin_attempt`], [`Installer::commit_health`] and
//! [`Installer::record_failure`] and takes no view on when they are called.
//!
//! # The launcher is part of the installation
//!
//! An installation with no launcher has no entry point. [`Installer`] places
//! one in the root when the caller supplies a binary to place, because the
//! launcher identifies its application by its own location. Where that binary
//! comes from is the caller's decision; see [`InstallOptions::launcher`].
//!
//! # Uninstall and user data
//!
//! [`uninstall`] removes the installation root and nothing else. xPack does
//! not know where an application keeps user data — that path is chosen by the
//! application, not by the packaging system — so it cannot remove it and does
//! not pretend to. An application wanting its data removed must do so itself
//! from an uninstall hook. This is a deliberate decision rather than an
//! omission: deleting a directory xPack merely guessed at would be far worse
//! than leaving it behind.
//!
//! It reports what it actually deleted. A root it could not empty is named in
//! [`Removal::remaining`] rather than described as removed.

pub mod installer;
pub mod recovery;
pub mod trust;

pub use installer::{InstallOptions, Installed, Installer, LauncherOutcome, Removal, uninstall};
pub use recovery::RecoveryReport;
pub use trust::{TrustDecision, open_and_verify};

pub use xpack_platform::InstallLock;
