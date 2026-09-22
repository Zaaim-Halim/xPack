//! Looking at an installation without touching it.
//!
//! Something choosing where to install — a person browsing folders, a script
//! checking before it acts — needs to know what an install there would find,
//! before it is allowed to change anything. [`inspect`] answers that.
//!
//! # It creates nothing
//!
//! Taking the install lock creates the state directory and the lock file, so a
//! folder somebody merely looked at would gain both. [`inspect`] reads state
//! without the lock, and asks whether the lock is held only when the lock file
//! already exists. State is always replaced by an atomic rename, so reading it
//! unlocked sees one whole document or the other, never half of each.
//!
//! # It answers the way installing would
//!
//! Every verdict comes from the predicates the installer itself applies: the
//! state record, [`InstallState::ensure_not_downgrade`] and
//! [`InstallPaths::has_version_files`]. A second, hand-written version of the
//! rules would drift, and the first sign would be a window offering an install
//! the installer then refuses.

use xpack_core::store::backup_path;
use xpack_core::{Error, InstallPaths, InstallState, Version};
use xpack_platform::InstallLock;

/// What installing `candidate` would find.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Existing {
    /// No installation: nothing has been installed here yet.
    Nothing,
    /// An older version is active. Installing upgrades it.
    Older(Version),
    /// Versions are installed but none is active. Installing goes ahead.
    Inactive,
    /// This version is already installed, with its files present.
    /// Installing it again is refused.
    Installed,
    /// This version is recorded but its files are missing. Installing
    /// repairs it.
    Damaged,
    /// A newer version is active. Installing would be a downgrade, which is
    /// refused.
    Newer(Version),
    /// Another xPack operation holds the installation.
    Busy,
    /// The installation could not be read, in the words of the error.
    Unreadable(String),
}

impl Existing {
    /// Whether installing would go ahead.
    pub fn allows_install(&self) -> bool {
        matches!(self, Self::Nothing | Self::Older(_) | Self::Inactive | Self::Damaged)
    }

    /// Whether this would be the first installation here.
    ///
    /// The one moment a choice the installation's updater does not know about
    /// can be made safely, because the updater placed is this build's own.
    pub fn is_first_install(&self) -> bool {
        matches!(self, Self::Nothing)
    }
}

/// What installing `candidate` into this installation would find.
///
/// Reads, and never writes or creates. See the module documentation.
pub fn inspect(paths: &InstallPaths, candidate: &Version) -> Existing {
    if !paths.state_dir().is_dir() {
        return Existing::Nothing;
    }

    // Probed first because installing would fail on it first. Only when the
    // file is there: opening one that is not would create it.
    if paths.lock_file().is_file() {
        match InstallLock::acquire(paths) {
            Ok(lock) => drop(lock),
            Err(Error::Locked(_)) => return Existing::Busy,
            Err(error) => return Existing::Unreadable(error.to_string()),
        }
    }

    let file = paths.state_file();
    if !file.exists() && !backup_path(&file).exists() {
        return Existing::Nothing;
    }
    let state = match InstallState::load(&file) {
        Ok(loaded) => loaded.value,
        Err(error) => return Existing::Unreadable(error.to_string()),
    };
    if let Some(expected) = paths.application_id()
        && state.application_id != expected
    {
        return Existing::Unreadable(format!(
            "installation state belongs to {:?}, not {expected:?}",
            state.application_id
        ));
    }

    classify(&state, paths, candidate)
}

/// The verdict for a readable state document, in the order installing checks.
fn classify(state: &InstallState, paths: &InstallPaths, candidate: &Version) -> Existing {
    if state.record(candidate).is_some() {
        return if paths.has_version_files(candidate) {
            Existing::Installed
        } else {
            Existing::Damaged
        };
    }
    if state.ensure_not_downgrade(candidate, false).is_err() {
        let current = state.current_version.clone().expect("a downgrade needs an active version");
        return Existing::Newer(current);
    }
    match &state.current_version {
        Some(current) => Existing::Older(current.clone()),
        None if !state.versions.is_empty() => Existing::Inactive,
        None => Existing::Nothing,
    }
}
