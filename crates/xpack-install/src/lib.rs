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
//! # Desktop integration
//!
//! A package may ask to appear in the user's application menu. That is the
//! only thing this crate writes outside the installation root, it is always
//! per-user, and it happens only when the signed manifest asks for it. See
//! [`integration`], where the three platforms' very different mechanisms —
//! a Start-Menu shortcut, a `.desktop` entry, an application bundle — are
//! rendered from one description.
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

pub mod inspect;
pub mod installer;
pub mod integration;
pub mod recovery;
pub mod trust;

pub use inspect::{Existing, inspect};
pub use installer::{
    DeltaSource, InstallOptions, InstallSource, Installed, Installer, LauncherOutcome, Removal,
    uninstall, uninstall_with_roots,
};
pub use integration::{Entry as DesktopEntry, Outcome as DesktopOutcome};
pub use recovery::RecoveryReport;
pub use trust::{TrustDecision, open_and_verify, open_and_verify_delta};

pub use xpack_platform::InstallLock;
