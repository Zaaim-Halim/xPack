//! What the wizard asks of the installer that hosts it.
//!
//! The wizard knows nothing about packages or locks. It asks three questions
//! through [`Engine`] and renders the answers, so the only way it can change a
//! machine is the way the console installer does.

use std::path::{Path, PathBuf};

use xpack_core::{Error, ProgressReporter, Version};
use xpack_install::Existing;

/// The installer, as the wizard sees it.
///
/// `Send + Sync` because installing runs on a worker thread while the window
/// stays responsive on the main one.
pub trait Engine: Send + Sync + 'static {
    /// What installing into `root` would find.
    ///
    /// Must not change anything on disk: it is called while the person is
    /// still choosing, and a folder they merely looked at must not gain files.
    fn inspect(&self, root: &Path) -> Inspection;

    /// Installs, reporting progress as it goes.
    ///
    /// Called at most once, on a worker thread. It cannot be interrupted,
    /// which is why the wizard refuses to close while it runs.
    fn install(
        &self,
        choices: &Choices,
        progress: &dyn ProgressReporter,
    ) -> Result<Installed, Error>;

    /// Starts the application installed under `root`, without waiting for it.
    fn launch(&self, root: &Path) -> Result<(), Error>;
}

/// The answer to [`Engine::inspect`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspection {
    /// The application's own directory under the root: what the person is
    /// shown as the place it will live.
    pub target: PathBuf,
    /// What is there, or why the root itself cannot be used.
    pub verdict: Result<Existing, RootProblem>,
}

/// Why a chosen root cannot be installed into, before looking inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootProblem {
    /// Not a full path.
    NotAbsolute,
    /// The person cannot write there.
    NotWritable,
    /// So deep that the application could not be started from it.
    TooDeep,
}

/// What the person chose, handed to [`Engine::install`].
///
/// Anything they were not asked is `None`, which means exactly what the
/// installer does when nobody is asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choices {
    /// The directory holding the person's xPack applications.
    pub root: PathBuf,
    /// `Some(false)` when they declined the desktop entry. Never `Some(true)`:
    /// agreeing to what the package asks for is the same as not being asked.
    pub desktop_entry: Option<bool>,
}

/// What a successful installation produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// The application's directory.
    pub directory: PathBuf,
    /// The version now installed.
    pub version: Version,
    /// Whether a desktop entry exists now. Reported, not predicted: an entry
    /// that could not be written does not fail an install.
    pub shortcut_added: bool,
}

/// Why the engine did not do what it was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// Which kind, which decides what the wizard offers next.
    pub kind: FailureKind,
    /// The engine's own words, already meant for a person.
    pub message: String,
}

impl Failure {
    /// Classifies an engine error.
    ///
    /// With the predicate the installer's exit codes use, so a window and a
    /// console can never disagree about what counts as a security failure.
    pub fn of(error: &Error) -> Self {
        let kind = if error.is_integrity_failure() {
            FailureKind::Integrity
        } else if matches!(error, Error::Locked(_)) {
            FailureKind::Busy
        } else {
            FailureKind::Other
        };
        Self { kind, message: error.to_string() }
    }
}

/// The distinctions the wizard acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// A signature or checksum did not verify. Never offered a retry.
    Integrity,
    /// Another xPack operation holds the installation. Worth retrying.
    Busy,
    /// Anything else.
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_are_classified_the_way_exit_codes_are() {
        assert_eq!(Failure::of(&Error::Integrity("bad".into())).kind, FailureKind::Integrity);
        assert_eq!(Failure::of(&Error::Locked("held".into())).kind, FailureKind::Busy);
        assert_eq!(Failure::of(&Error::invalid("x", "y")).kind, FailureKind::Other);
    }
}
