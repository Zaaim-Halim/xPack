//! Operating-system integration: locking, launching, and the derived link.
//!
//! Everything here touches the machine rather than the package format, so it is
//! kept deliberately small. Shortcuts, desktop entries and service registration
//! belong with the installer; this crate is depended on by every other
//! component and must not become a grab bag.
//!
//! # No unsafe code
//!
//! The workspace forbids unsafe, and this crate keeps that promise. The one
//! feature that would require it — creating an NTFS directory junction through
//! `DeviceIoControl` — is reported as unsupported instead, because the
//! junction is only ever a convenience. Authority over which version runs
//! belongs to the installation state file, so a missing link degrades
//! ergonomics and nothing else. See [`link`].

pub mod link;
pub mod lock;
pub mod process;
pub mod sharing;

pub use link::{LinkOutcome, update_current_link};
pub use lock::{DownloadLease, InstallLock};
pub use process::{LaunchRequest, launch, resolve_executable};
