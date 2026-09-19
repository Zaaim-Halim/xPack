//! Installation state — the single source of truth for "which version runs".
//!
//! # Why a JSON pointer and not a symlink
//!
//! There are two plausible ways to record which version is active: a
//! `currentVersion` field in a small JSON file, or an OS-level
//! junction/symlink swap of a `current/` directory. They cannot both be
//! authoritative, so xPack picks one:
//!
//! * Writing this file with *temp → fsync → rename → fsync-parent* is atomic
//!   on both NTFS and POSIX and needs no elevation.
//! * Replacing a Windows directory junction is not a single atomic operation —
//!   it is delete-then-create, leaving a window in which `current/` does not
//!   exist. A crash inside that window leaves the application unlaunchable.
//!
//! So **this file is the authority**. A `current` symlink/junction may still
//! exist as a convenience for shortcuts and external scripts, but it is a
//! *derived* artefact that the launcher repairs from this file on startup.
//!
//! The file also records an explicit [`UpdatePhase`], which is what makes an
//! interrupted update recoverable: the next process to start can see exactly
//! which step was in flight and finish or undo it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::version::Version;

/// Format version of the state document itself.
pub const STATE_FORMAT_VERSION: u32 = 1;

/// How far an in-flight update had progressed.
///
/// The value is persisted *before* the step it names is attempted, so
/// recovery always errs towards assuming the step may have partially run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE", tag = "phase")]
pub enum UpdatePhase {
    /// No update in flight.
    Idle,
    /// A package is being downloaded to the staging area.
    Downloading {
        /// Version being fetched.
        version: Version,
    },
    /// A downloaded package is being verified.
    Verifying {
        /// Version being verified.
        version: Version,
    },
    /// A verified package is being extracted into `versions/<version>`.
    Installing {
        /// Version being extracted.
        version: Version,
    },
    /// Extraction finished; the version is complete but not yet active.
    Staged {
        /// Version staged and ready to activate.
        version: Version,
    },
    /// The active pointer has moved but the new version has not proved healthy.
    ///
    /// This is the only phase that can trigger an automatic rollback.
    PendingVerification {
        /// Version now active but still on probation.
        version: Version,
        /// Version to return to if the probation fails.
        rollback_to: Version,
    },
    /// A rollback is in progress.
    RollingBack {
        /// Version being abandoned.
        from: Version,
        /// Version being restored.
        to: Version,
    },
}

impl UpdatePhase {
    /// The version this phase concerns, if any.
    pub fn version(&self) -> Option<&Version> {
        match self {
            Self::Idle => None,
            Self::Downloading { version }
            | Self::Verifying { version }
            | Self::Installing { version }
            | Self::Staged { version }
            | Self::PendingVerification { version, .. } => Some(version),
            Self::RollingBack { from, .. } => Some(from),
        }
    }

    /// Returns `true` when no update is in flight.
    pub fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }
}

/// Health of an installed version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VersionStatus {
    /// Extracted and fully verified, never yet run.
    Staged,
    /// Has run successfully at least once; a valid rollback target.
    Good,
    /// Failed its health check. Never activated again automatically.
    Bad,
}

/// Bookkeeping for one installed version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionRecord {
    /// Current health of this version.
    pub status: VersionStatus,
    /// RFC 3339 timestamp of installation, for diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_at: Option<String>,
    /// Why the version was marked [`VersionStatus::Bad`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

impl VersionRecord {
    /// A freshly extracted, not-yet-proven version.
    pub fn staged(installed_at: Option<String>) -> Self {
        Self { status: VersionStatus::Staged, installed_at, failure_reason: None }
    }
}

/// The complete persisted state of one installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallState {
    /// Format version of this document.
    pub state_format_version: u32,
    /// Application this installation belongs to.
    pub application_id: String,
    /// Version the launcher must start. `None` only before the first install.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_version: Option<Version>,
    /// Last version known to be healthy; the automatic rollback target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<Version>,
    /// In-flight update step, used for crash recovery.
    #[serde(default = "idle_phase")]
    pub update: UpdatePhase,
    /// Every version present under `versions/`, keyed by version string.
    #[serde(default)]
    pub versions: BTreeMap<String, VersionRecord>,
}

fn idle_phase() -> UpdatePhase {
    UpdatePhase::Idle
}

impl InstallState {
    /// Creates empty state for a new installation.
    pub fn new(application_id: impl Into<String>) -> Self {
        Self {
            state_format_version: STATE_FORMAT_VERSION,
            application_id: application_id.into(),
            current_version: None,
            previous_version: None,
            update: UpdatePhase::Idle,
            versions: BTreeMap::new(),
        }
    }

    /// Fails closed on a state document written by a newer xPack.
    pub fn ensure_supported(&self) -> Result<()> {
        if self.state_format_version > STATE_FORMAT_VERSION {
            return Err(Error::UnsupportedFormatVersion {
                found: self.state_format_version,
                supported: STATE_FORMAT_VERSION,
            });
        }
        Ok(())
    }

    /// The version the launcher should start.
    pub fn active(&self) -> Result<&Version> {
        self.current_version
            .as_ref()
            .ok_or_else(|| Error::invalid("state", "no version is active; the install is incomplete"))
    }

    /// Looks up a version's record.
    pub fn record(&self, version: &Version) -> Option<&VersionRecord> {
        self.versions.get(&version.to_string())
    }

    /// Returns `true` when the version failed a health check and is quarantined.
    ///
    /// Re-activating a known-bad version is how an update loop gets started, so
    /// both the updater and the installer consult this.
    pub fn is_bad(&self, version: &Version) -> bool {
        self.record(version).is_some_and(|r| r.status == VersionStatus::Bad)
    }

    /// Registers a newly extracted version.
    pub fn stage_version(&mut self, version: &Version, installed_at: Option<String>) {
        self.versions.insert(version.to_string(), VersionRecord::staged(installed_at));
    }

    /// Marks a version healthy, making it a valid rollback target.
    pub fn mark_good(&mut self, version: &Version) {
        if let Some(record) = self.versions.get_mut(&version.to_string()) {
            record.status = VersionStatus::Good;
            record.failure_reason = None;
        }
    }

    /// Quarantines a version so it is never activated automatically again.
    pub fn mark_bad(&mut self, version: &Version, reason: impl Into<String>) {
        let entry = self
            .versions
            .entry(version.to_string())
            .or_insert_with(|| VersionRecord::staged(None));
        entry.status = VersionStatus::Bad;
        entry.failure_reason = Some(reason.into());
    }

    /// Best rollback target: the newest healthy version that is not `exclude`.
    pub fn best_rollback_target(&self, exclude: &Version) -> Option<Version> {
        // `previous_version` is the intended target, but it may itself have
        // been quarantined by an earlier failure, so fall back to a scan.
        if let Some(previous) = &self.previous_version
            && previous != exclude
            && !self.is_bad(previous)
        {
            return Some(previous.clone());
        }

        self.versions
            .iter()
            .filter(|(_, r)| r.status == VersionStatus::Good)
            .filter_map(|(v, _)| Version::parse(v).ok())
            .filter(|v| v != exclude)
            .max()
    }

    /// Rejects a candidate that is not strictly newer than the active version.
    ///
    /// This is the anti-downgrade control: an attacker who can serve traffic
    /// must not be able to force a client back onto an older, validly signed
    /// version with a known vulnerability. It is applied twice — before
    /// download and again immediately before activation — so a state change
    /// during a long download cannot slip past it.
    pub fn ensure_not_downgrade(&self, candidate: &Version, allow_downgrade: bool) -> Result<()> {
        if allow_downgrade {
            return Ok(());
        }
        if let Some(current) = &self.current_version
            && candidate.as_semver().cmp_precedence(current.as_semver())
                != std::cmp::Ordering::Greater
        {
            return Err(Error::DowngradeRejected {
                current: current.to_string(),
                candidate: candidate.to_string(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn state_at(current: &str) -> InstallState {
        let mut s = InstallState::new("com.example.app");
        s.current_version = Some(v(current));
        s.stage_version(&v(current), None);
        s.mark_good(&v(current));
        s
    }

    #[test]
    fn rejects_equal_and_older_candidates() {
        let s = state_at("1.2.0");
        assert!(s.ensure_not_downgrade(&v("1.1.0"), false).is_err());
        assert!(s.ensure_not_downgrade(&v("1.2.0"), false).is_err());
        s.ensure_not_downgrade(&v("1.2.1"), false).unwrap();
    }

    #[test]
    fn rejects_a_prerelease_of_the_active_release() {
        // 1.3.0-rc.1 precedes 1.3.0, so from 1.3.0 it is a downgrade.
        assert!(state_at("1.3.0").ensure_not_downgrade(&v("1.3.0-rc.1"), false).is_err());
    }

    #[test]
    fn ignores_build_metadata_when_deciding_downgrades() {
        // SemVer precedence ignores build metadata, so this is not an upgrade.
        assert!(state_at("1.2.0").ensure_not_downgrade(&v("1.2.0+build.9"), false).is_err());
    }

    #[test]
    fn explicit_override_permits_a_downgrade() {
        state_at("1.2.0").ensure_not_downgrade(&v("1.0.0"), true).unwrap();
    }

    #[test]
    fn first_install_has_nothing_to_downgrade_from() {
        InstallState::new("com.example.app").ensure_not_downgrade(&v("0.1.0"), false).unwrap();
    }

    #[test]
    fn rollback_skips_versions_already_known_bad() {
        let mut s = state_at("1.0.0");
        s.stage_version(&v("1.1.0"), None);
        s.mark_good(&v("1.1.0"));
        s.stage_version(&v("1.2.0"), None);
        s.previous_version = Some(v("1.1.0"));
        s.mark_bad(&v("1.1.0"), "crashed on startup");

        // 1.1.0 is the nominal target but is quarantined, so 1.0.0 wins.
        assert_eq!(s.best_rollback_target(&v("1.2.0")), Some(v("1.0.0")));
    }

    #[test]
    fn rollback_never_returns_the_failing_version() {
        let mut s = state_at("1.0.0");
        s.previous_version = Some(v("1.0.0"));
        assert_eq!(s.best_rollback_target(&v("1.0.0")), None);
    }

    #[test]
    fn staged_versions_are_not_rollback_targets() {
        let mut s = InstallState::new("com.example.app");
        s.stage_version(&v("1.0.0"), None); // staged, never proven healthy
        assert_eq!(s.best_rollback_target(&v("1.1.0")), None);
    }

    #[test]
    fn rejects_state_from_a_newer_xpack() {
        let mut s = InstallState::new("com.example.app");
        s.state_format_version = STATE_FORMAT_VERSION + 1;
        assert!(matches!(s.ensure_supported(), Err(Error::UnsupportedFormatVersion { .. })));
    }

    #[test]
    fn phase_round_trips_through_json() {
        let phase = UpdatePhase::PendingVerification {
            version: v("1.2.0"),
            rollback_to: v("1.1.0"),
        };
        let json = serde_json::to_string(&phase).unwrap();
        assert_eq!(serde_json::from_str::<UpdatePhase>(&json).unwrap(), phase);
    }
}
