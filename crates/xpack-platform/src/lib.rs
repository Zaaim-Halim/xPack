//! Operating-system integration: locking, launching, and the derived link.
//!
//! Everything here touches the machine rather than the package format, so it is
//! kept deliberately small. Shortcuts, desktop entries and service registration
//! belong with the installer; this crate is depended on by every other
//! component and must not become a grab bag.
//!
//! # Unsafe code: one call
//!
//! The workspace forbids unsafe, and this crate keeps to that with one
//! exception: [`announce_environment_change`], a single Win32 call with no safe
//! equivalent, justified where it is made. Unsafe is denied rather than
//! forbidden here so that call can be allowed; any other use still fails the
//! build.
//!
//! Anything that is only a convenience does without. Creating an NTFS
//! directory junction through `DeviceIoControl` is reported as unsupported
//! instead, because authority over which version runs belongs to the
//! installation state file, so a missing link degrades ergonomics and nothing
//! else. See [`link`]. A command that a new terminal cannot find until the user
//! signs out again is not a convenience missing; it is the feature not working.

pub mod environment;
pub mod link;
pub mod lock;
pub mod process;
pub mod sharing;
pub mod space;
pub mod windows_manifest;

pub use environment::announce_environment_change;
pub use link::{LinkOutcome, update_current_link};
pub use lock::{DownloadLease, InstallLock};
pub use process::{LaunchRequest, launch, request_close, resolve_executable, without_a_console};
pub use space::{available_space, ensure_space};
pub use windows_manifest::WINDOWS_MANIFEST;
