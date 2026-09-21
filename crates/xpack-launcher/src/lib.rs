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
//! That is an approximation, and an application can replace it with a real
//! answer. The launcher puts a path in [`HEALTH_FILE_ENV`]; an application
//! that creates that file has *said* it started, rather than merely having
//! avoided dying. A manifest setting `requireStartupReport` makes the report
//! mandatory, so a version that never sends one is rolled back.
//!
//! A file, not a socket. xPack is runtime-independent, and creating a file is
//! a line of code in every language a payload might be written in — where a
//! named pipe on Windows would need Win32 calls this workspace forbids. It is
//! one-way and one-shot, which is exactly what a startup check needs; richer
//! two-way messaging, for progress or for liveness after startup, is a larger
//! piece and is not built.
//!
//! # It starts the updater
//!
//! The launcher spawns the background updater, detached, on every start, and
//! never waits for it. Where the running version asks to be checked while it
//! runs, a thread here keeps doing so for as long as the application is open.
//!
//! Between them that is the whole update trigger: no scheduler to register at
//! install time, no background agent to notarise, no privileges. A user who
//! never opens the application never updates, which for a desktop application
//! is the right trade — and an application left open for days is checked
//! anyway, which is what the thread is for.
//!
//! A version the updater staged is activated *here*, not there, because
//! activation begins a probation and a probation needs something watching the
//! version start. See [`Launcher::launch`].
//!
//! # Which application am I?
//!
//! The launcher is installed inside the application's own directory, so it
//! answers that by looking at where it is: the directory containing the
//! executable is the installation root. Nothing is embedded at build time,
//! which means one prebuilt launcher binary works for every application.

pub mod launcher;
pub mod run;

pub use launcher::{
    APPLICATION_DIR_ENV, HEALTH_FILE_ENV, Launcher, Outcome, RunningApplication, StartupResult,
    announce_a_staged_version, spawn_periodic_update_checks, spawn_updater,
};
pub use run::run;
