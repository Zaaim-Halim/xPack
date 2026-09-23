//! The installation wizard an xPack installer shows a person.
//!
//! The console installer is right for a script and wrong for someone who
//! double-clicked a download: on Windows a console flashes up, on macOS nothing
//! appears at all. This crate is the window that person sees instead.
//!
//! # It installs nothing
//!
//! The wizard collects a few choices and hands them to an [`Engine`], which the
//! installer implements over the same install call its console path makes,
//! with the same defaults for anything it does not ask. There is no second way into an installation here, because the
//! weaker of two paths to the same state is always where the bug ends up.
//!
//! # Everything but the window is ordinary code
//!
//! Which page comes next, when a button is enabled, whether closing is allowed
//! and every word on every page are decided by [`model`], which links no
//! toolkit and is tested on any machine. A platform front-end only draws what
//! the model says and reports what the user did. The part that can only be
//! judged by looking at it on one operating system is kept as small as it can
//! be.

pub mod engine;
pub mod installation;
pub mod model;
// Only where a front-end uses it: on other platforms the window reports that
// it cannot be shown, and nothing would drive a session.
#[cfg(any(test, all(feature = "window", any(target_os = "macos", windows))))]
mod session;
#[cfg(feature = "window")]
pub mod window;

#[cfg(all(feature = "window", target_os = "macos"))]
mod macos;

#[cfg(all(feature = "window", windows))]
mod windows;

pub use engine::{Choices, Engine, Failure, FailureKind, Inspection, Installed, RootProblem};
pub use installation::Installation;
pub use model::{
    Buttons, CloseRequest, CommandOffer, Conclusion, Counts, Facts, FinishView, Flavour,
    InstallKind, Key, Page, PageSet, Progress, Severity, Status, Step, TextOverride, Texts, UiPlan,
    Visibility, Wizard, WizardSpec,
};
