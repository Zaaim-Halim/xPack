//! Starting an installed application, and resolving its probation.
//!
//! The launcher is the only xPack component that runs every single time a user
//! opens their application, so it is kept small and it fails loudly rather
//! than cleverly.
//!
//! # How a version proves it works
//!
//! Activating a version puts it on probation. Something has to decide whether
//! probation passed, and the engine deliberately takes no view — this is where
//! the decision is made.
//!
//! **A version that starts and is still running after its startup window has
//! passed.** That is the only signal available without the application
//! cooperating, and it is a real one: the overwhelming majority of broken
//! updates fail immediately — a missing runtime, an unreadable file, an
//! incompatible library — rather than minutes later.
//!
//! The alternative, waiting for the process to exit and judging its status, is
//! what `xpack run` does. That is right for a short-lived command someone
//! typed and wrong here: a desktop application would only be confirmed healthy
//! when the user eventually quit, and a crash three days later would trigger a
//! rollback long after the update stopped being the explanation.
//!
//! So: exit non-zero inside the window is a failure, exit zero inside the
//! window is success, and still running when the window closes is success.
//!
//! This is an approximation and is documented as one. An application that
//! starts, paints a window and is broken in every other respect passes. Real
//! health confirmation needs the application to report it, which is what the
//! IPC channel in the design is for and is not built yet.
//!
//! # Which application am I?
//!
//! The launcher is installed inside the application's own directory, so it
//! answers that by looking at where it is: the directory containing the
//! executable is the installation root. Nothing is embedded at build time,
//! which means one prebuilt launcher binary works for every application.

pub mod launcher;

pub use launcher::{Launcher, Outcome, StartupResult};
