//! Runs a package's hook script.
//!
//! The one program in xPack that carries a script engine (`QuickJS`). The
//! installer, launcher and uninstaller start it as a process when a hook is
//! due, hand it a [`Request`] on standard input, and read how it ended from
//! its exit code; nothing else links it, so nothing else pays for the engine.
//!
//! A hook gets one object, `ctx`, and nothing else: no `require`, no imports,
//! no network. What `ctx` lets it read, write and run is held to what its
//! package declared; see the policy.

mod engine;
mod policy;
mod recorder;
mod request;

pub use engine::{Outcome, check, run};
pub use request::Request;
pub use xpack_core::hooks::Scope;

/// Exit codes: how a hook ended, for the program that started this one.
pub mod exit {
    /// It returned, or its promise resolved.
    pub const SUCCEEDED: u8 = 0;
    /// It threw, its promise rejected, or it could not be run.
    pub const FAILED: u8 = 1;
    /// It was still running at its deadline.
    pub const TIMED_OUT: u8 = 2;
    /// The request was not one this program can act on.
    pub const BAD_REQUEST: u8 = 3;
}
