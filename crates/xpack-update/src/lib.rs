//! Checking for, downloading and applying updates.
//!
//! This is the first crate whose input arrives from a channel an attacker may
//! control, so the rule that governs everything here is worth stating once:
//!
//! > **Nothing the update server says is trusted.** The index chooses what to
//! > download; the publisher's signature decides what may be installed.
//!
//! A downloaded package is routed through the same
//! [`xpack_install::open_and_verify`] every local install uses. There is no
//! second verification path in this crate, because two implementations of one
//! security check will eventually disagree, and the weaker one is the bug.
//!
//! # What this version does not do
//!
//! Differential updates are designed but not implemented here. Full-package
//! updates ship first and are proven; adding delta assembly behind the same
//! verification comes after. Mixing them now would make failures ambiguous,
//! and an ambiguous failure in an update path is the worst kind.
//!
//! Interrupted downloads are discarded and re-fetched rather than resumed.
//! Resuming is safe — the result is fully hash-verified before use — but it
//! needs rules about a partial file whose target version has since changed,
//! and re-downloading is correct while packages are small enough not to make
//! that trade-off pay.

pub mod index;
pub mod reporting;
pub mod transport;
pub mod updater;

pub use index::{PackageRef, UpdateIndex};
pub use reporting::ProgressWriter;
#[cfg(feature = "https")]
pub use transport::HttpsTransport;
pub use transport::{Timeouts, UpdateTransport};
pub use updater::{Available, UpdateOptions, Updater};
