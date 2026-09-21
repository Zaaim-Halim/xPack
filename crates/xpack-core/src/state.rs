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
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::store::{self, Loaded};
use crate::version::Version;

/// Format version of the state document itself.
pub const STATE_FORMAT_VERSION: u32 = 1;

/// How many times a version on probation may be launched before rollback.
///
/// Two, so that a single interruption — a power cut, a user closing the
/// splash screen, an OS-initiated reboot — does not condemn a version that
/// would have started perfectly well on the next attempt, while a version that
/// genuinely fails to start is abandoned quickly.
pub const MAX_ACTIVATION_ATTEMPTS: u32 = 2;

/// How far an in-flight update had progressed.
///
/// The value is persisted *before* the step it names is attempted, so
/// recovery always errs towards assuming the step may have partially run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE", rename_all_fields = "camelCase", tag = "phase")]
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
        /// How many times activation of `version` has been attempted.
        ///
        /// Without this counter the probation cannot terminate. Consider a
        /// power cut between activating 1.2.0 and resolving its health check:
        /// the next launch finds `PendingVerification` and has no way to tell
        /// "this version crashes" from "the machine lost power". Retrying
        /// unconditionally loops forever on a genuinely broken version;
        /// rolling back unconditionally condemns a good version because of one
        /// unrelated power cut. Counting attempts allows a bounded number of
        /// retries and then a rollback, so the probation always ends.
        #[serde(default)]
        attempts: u32,
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

    /// Returns `true` when a version is active but has not yet proved healthy.
    ///
    /// Distinct from "not idle" on purpose. Several phases are non-idle
    /// without any version being on trial — a download in flight, a version
    /// staged and waiting to be activated — and treating those as a probation
    /// would make the launcher judge a version that is not being tested,
    /// spending its bounded attempts and, on a non-zero exit, quarantining a
    /// version that was working perfectly well.
    pub fn is_probation(&self) -> bool {
        matches!(self, Self::PendingVerification { .. })
    }

    /// Returns `true` when the probation has used up its retry budget.
    ///
    /// At this point the launcher must roll back instead of trying again.
    pub fn attempts_exhausted(&self) -> bool {
        matches!(self, Self::PendingVerification { attempts, .. } if *attempts >= MAX_ACTIVATION_ATTEMPTS)
    }

    /// A one-line description suitable for showing a user.
    ///
    /// The derived `Debug` form exposes field names and nesting, which is
    /// right for a log and wrong for someone asking what their installation is
    /// doing.
    pub fn describe(&self) -> String {
        match self {
            Self::Idle => "idle".to_string(),
            Self::Downloading { version } => format!("downloading {version}"),
            Self::Verifying { version } => format!("verifying {version}"),
            Self::Installing { version } => format!("installing {version}"),
            Self::Staged { version } => format!("{version} is staged and ready to activate"),
            Self::PendingVerification { version, rollback_to, attempts } => format!(
                "{version} is on probation ({attempts} of {MAX_ACTIVATION_ATTEMPTS} attempts \
                 used); will roll back to {rollback_to} if it fails"
            ),
            Self::RollingBack { from, to } => format!("rolling back from {from} to {to}"),
        }
    }

    /// Records one more activation attempt of a version on probation.
    ///
    /// Callers must persist the state *before* launching, not after. A
    /// counter incremented after a successful launch never counts the crash
    /// that prevented the increment from happening.
    pub fn record_attempt(&mut self) {
        if let Self::PendingVerification { attempts, .. } = self {
            *attempts = attempts.saturating_add(1);
        }
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
    /// The oldest version allowed to run, if a release demanded one.
    ///
    /// Written when a version whose **signed** manifest marks it mandatory is
    /// installed. The launcher refuses to start anything older, so a security
    /// release cannot be left unapplied by a user who simply never updates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_version: Option<Version>,
    /// When an update server was last asked, in seconds since the Unix epoch.
    ///
    /// The background updater runs every time the application starts, and a
    /// user may open their application many times a day. Without a record of
    /// the last check, every one of those becomes a request, which is a small
    /// denial-of-service aimed at the publisher's own server.
    ///
    /// Absent means "never asked", which is why it is an `Option` rather than
    /// a zero: zero is a real instant in 1970, and treating it as "never"
    /// would be a guess dressed as a value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_update_check: Option<u64>,
    /// Base name this installation's executables were given.
    ///
    /// Absent means they carry xPack's own names, which is what every
    /// installation made before this field existed has on disk.
    ///
    /// # Why it is pinned rather than derived on demand
    ///
    /// The obvious implementation recomputes the names from the manifest
    /// whenever they are needed. That breaks the first time a publisher
    /// renames their application: the launcher already written is found under
    /// its old name, the new name matches nothing, and the installation grows
    /// a second launcher while every shortcut still points at the first.
    ///
    /// Recorded at the first install and never rewritten, a rename changes
    /// what a user sees in their menus — which is a display concern the
    /// desktop entry already handles — and leaves the files alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_base_name: Option<String>,
    /// Version the user has already been told is waiting, if any.
    ///
    /// A staged version sits on disk until the application is next started,
    /// which may be days. Without a record of having announced it, every
    /// periodic check that found nothing new would still find a staged version
    /// and announce it again — a dialog every few hours about an update the
    /// user has already seen, which is how a person learns to dismiss whatever
    /// the updater says without reading it.
    ///
    /// Holding the version rather than a flag means a newer staged version is
    /// announced even though the previous one was: the value no longer matches
    /// and the comparison says so, with nothing to reset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announced_update: Option<Version>,

    /// Stable random identifier deciding this installation's staged-rollout
    /// cohort.
    ///
    /// Generated once, on the first update check that needs it, and never
    /// changed afterwards — a value that moved would let an installation drop
    /// out of a rollout it had already been offered.
    ///
    /// It is never transmitted. The client fetches a static index and decides
    /// in private, so this identifies nothing to anyone: it exists only so the
    /// decision is the same on every check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout_id: Option<String>,
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
            required_version: None,
            last_update_check: None,
            binary_base_name: None,
            announced_update: None,
            rollout_id: None,
        }
    }

    /// The names this installation's executables carry.
    ///
    /// An installation that predates named executables keeps xPack's names,
    /// because those are the files it actually has.
    pub fn binary_names(&self) -> crate::naming::BinaryNames {
        match &self.binary_base_name {
            Some(base) => crate::naming::BinaryNames::Application { base: base.clone() },
            None => crate::naming::BinaryNames::Xpack,
        }
    }

    /// Records the names for an installation that does not have them yet.
    ///
    /// Does nothing when a name is already recorded, which is what makes a
    /// later rename of the application harmless. Returns whether anything
    /// changed, so a caller writes state only when it did.
    pub fn pin_binary_names(&mut self, display_name: &str) -> bool {
        if self.binary_base_name.is_some() {
            return false;
        }
        self.binary_base_name = Some(crate::naming::safe_file_name(display_name));
        true
    }

    /// The staged-rollout identifier, creating one if this is the first need.
    ///
    /// Returns the identifier and whether it was just generated, so the caller
    /// can persist state only when something actually changed. Generating it
    /// lazily rather than at install time means an installation that never
    /// checks for updates never acquires one.
    pub fn rollout_id_or_create(
        &mut self,
        generate: impl FnOnce() -> Result<String>,
    ) -> Result<(String, bool)> {
        if let Some(existing) = &self.rollout_id {
            return Ok((existing.clone(), false));
        }
        let created = generate()?;
        self.rollout_id = Some(created.clone());
        Ok((created, true))
    }

    /// Returns `true` when an update check is due.
    ///
    /// A clock that has moved backwards — a correction, a dual-boot, a virtual
    /// machine resuming from a snapshot — makes the recorded instant look like
    /// the future. That is treated as due rather than as a reason to wait out
    /// a wait that may never end.
    pub fn update_check_is_due(&self, now: u64, interval_seconds: u64) -> bool {
        match self.last_update_check {
            None => true,
            Some(last) if last > now => true,
            Some(last) => now.saturating_sub(last) >= interval_seconds,
        }
    }

    /// Whether `version` still needs announcing to the user.
    ///
    /// False once it has been announced, and true again for anything newer,
    /// because the comparison is against the version itself rather than a flag
    /// somebody has to remember to clear.
    pub fn update_needs_announcing(&self, version: &Version) -> bool {
        self.announced_update.as_ref() != Some(version)
    }

    /// Reads state from disk, recovering from the backup if needed.
    ///
    /// The returned [`Loaded`] reports whether recovery happened. Callers must
    /// surface that: running on recovered state means the primary file was
    /// lost, which usually means the disk is failing.
    pub fn load(path: &Path) -> Result<Loaded<Self>> {
        let loaded: Loaded<Self> = store::load(path)?;
        loaded.value.ensure_supported()?;
        Ok(loaded)
    }

    /// Writes state atomically, rotating the previous good copy to a backup.
    pub fn save(&self, path: &Path) -> Result<()> {
        store::save(path, self)
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
        self.current_version.as_ref().ok_or_else(|| {
            Error::invalid("state", "no version is active; the install is incomplete")
        })
    }

    /// Looks up a version's record.
    pub fn record(&self, version: &Version) -> Option<&VersionRecord> {
        self.versions.get(&version.to_string())
    }

    /// Returns `true` when the version has proved healthy at least once.
    ///
    /// Only such a version is a valid rollback target: falling back to
    /// something that has never run is not a recovery, it is a second gamble.
    pub fn is_good(&self, version: &Version) -> bool {
        self.record(version).is_some_and(|r| r.status == VersionStatus::Good)
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
        let entry =
            self.versions.entry(version.to_string()).or_insert_with(|| VersionRecord::staged(None));
        entry.status = VersionStatus::Bad;
        entry.failure_reason = Some(reason.into());
    }

    /// Best rollback target: the newest healthy version that is not `exclude`.
    pub fn best_rollback_target(&self, exclude: &Version) -> Option<Version> {
        // One rule on both paths: a rollback target must have proved healthy.
        // `previous_version` is the intended target, but it may itself have
        // been quarantined by an earlier failure — or never have run at all —
        // so it is held to exactly the same standard as the fallback scan.
        if let Some(previous) = &self.previous_version
            && previous != exclude
            && self.is_good(previous)
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
    mod binary_naming {
        use super::super::*;

        #[test]
        fn an_installation_that_predates_named_executables_keeps_the_old_names() {
            // The files on that machine are called xpack-launcher and so on.
            // Deciding otherwise would look for a launcher that is not there.
            let json = br#"{"stateFormatVersion":1,"applicationId":"com.example.app"}"#;
            let state: InstallState = serde_json::from_slice(json).unwrap();

            assert_eq!(state.binary_base_name, None);
            assert_eq!(state.binary_names(), crate::naming::BinaryNames::Xpack);
            assert_eq!(state.binary_names().launcher(), "xpack-launcher");
        }

        #[test]
        fn the_name_is_recorded_on_the_first_install() {
            let mut state = InstallState::new("com.example.app");

            assert!(state.pin_binary_names("My App"));
            assert_eq!(state.binary_base_name.as_deref(), Some("My App"));
            assert_eq!(state.binary_names().updater(), "My App Updater");
        }

        #[test]
        fn renaming_the_application_does_not_rename_the_files_already_written() {
            // The whole reason this is pinned. A second pin would leave the
            // launcher on disk unreachable and grow another beside it.
            let mut state = InstallState::new("com.example.app");
            state.pin_binary_names("My App");

            assert!(!state.pin_binary_names("Something Else Entirely"));
            assert_eq!(state.binary_base_name.as_deref(), Some("My App"));
        }

        #[test]
        fn a_hostile_display_name_is_sanitised_before_it_reaches_disk() {
            let mut state = InstallState::new("com.example.app");
            state.pin_binary_names("../../evil");

            let base = state.binary_base_name.clone().unwrap();
            assert!(!base.contains('/'), "{base}");
            assert!(!base.starts_with('.'), "{base}");
        }

        #[test]
        fn the_name_survives_being_written_and_read_back() {
            let mut state = InstallState::new("com.example.app");
            state.pin_binary_names("My App");

            let bytes = serde_json::to_vec(&state).unwrap();
            let read: InstallState = serde_json::from_slice(&bytes).unwrap();

            assert_eq!(read.binary_names(), state.binary_names());
        }

        #[test]
        fn a_state_without_a_name_does_not_write_the_field() {
            // An installation that never needed one should produce the same
            // document it always did, so an older build can still read it.
            let state = InstallState::new("com.example.app");
            let json = serde_json::to_string(&state).unwrap();

            assert!(!json.contains("binaryBaseName"), "{json}");
        }
    }

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
    fn a_staged_previous_version_is_not_a_rollback_target_either() {
        // The fast path and the fallback scan must apply the same rule. A
        // version that has never run is not a recovery target, however it
        // came to be recorded as `previous_version`.
        let mut s = InstallState::new("com.example.app");
        s.stage_version(&v("1.1.0"), None);
        s.previous_version = Some(v("1.1.0"));
        assert_eq!(s.best_rollback_target(&v("1.2.0")), None);
    }

    #[test]
    fn probation_terminates_after_a_bounded_number_of_attempts() {
        let mut phase = UpdatePhase::PendingVerification {
            version: v("1.2.0"),
            rollback_to: v("1.1.0"),
            attempts: 0,
        };
        assert!(!phase.attempts_exhausted(), "a fresh probation must be retryable");

        for _ in 0..MAX_ACTIVATION_ATTEMPTS {
            phase.record_attempt();
        }
        assert!(phase.attempts_exhausted(), "probation must end rather than loop forever");
    }

    #[test]
    fn recording_an_attempt_is_a_no_op_outside_probation() {
        let mut phase = UpdatePhase::Idle;
        phase.record_attempt();
        assert_eq!(phase, UpdatePhase::Idle);
        assert!(!phase.attempts_exhausted());
    }

    #[test]
    fn attempt_counts_survive_a_restart() {
        // The counter is only useful if it is durable: it exists precisely to
        // survive the crash that interrupted the health check.
        let phase = UpdatePhase::PendingVerification {
            version: v("1.2.0"),
            rollback_to: v("1.1.0"),
            attempts: 1,
        };
        let json = serde_json::to_string(&phase).unwrap();
        assert_eq!(serde_json::from_str::<UpdatePhase>(&json).unwrap(), phase);
    }

    #[test]
    fn state_written_before_the_counter_existed_still_loads() {
        // Forward compatibility within formatVersion 1: an older document has
        // no `attempts` field and must default to zero, not fail to parse.
        let json = r#"{"phase":"PENDING_VERIFICATION","version":"1.2.0","rollbackTo":"1.1.0"}"#;
        let phase: UpdatePhase = serde_json::from_str(json).unwrap();
        assert!(matches!(phase, UpdatePhase::PendingVerification { attempts: 0, .. }));
    }

    #[test]
    fn rejects_state_from_a_newer_xpack() {
        let mut s = InstallState::new("com.example.app");
        s.state_format_version = STATE_FORMAT_VERSION + 1;
        assert!(matches!(s.ensure_supported(), Err(Error::UnsupportedFormatVersion { .. })));
    }

    #[test]
    fn survives_a_save_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut original = state_at("1.2.0");
        original.previous_version = Some(v("1.1.0"));
        original.save(&path).unwrap();

        let loaded = InstallState::load(&path).unwrap();
        assert_eq!(loaded.value, original);
        assert!(!loaded.recovered_from_backup);
    }

    #[test]
    fn a_corrupt_state_file_falls_back_to_the_last_good_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        state_at("1.1.0").save(&path).unwrap();
        state_at("1.2.0").save(&path).unwrap();

        std::fs::write(&path, b"garbage from a failing disk").unwrap();

        let loaded = InstallState::load(&path).unwrap();
        assert_eq!(loaded.value.current_version, Some(v("1.1.0")));
        assert!(loaded.recovered_from_backup, "recovery must be reported to the caller");
    }

    #[test]
    fn a_state_file_from_a_newer_xpack_is_rejected_after_loading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut future = state_at("1.2.0");
        future.state_format_version = STATE_FORMAT_VERSION + 1;
        future.save(&path).unwrap();

        assert!(matches!(InstallState::load(&path), Err(Error::UnsupportedFormatVersion { .. })));
    }

    #[test]
    fn health_predicates_distinguish_the_three_statuses() {
        let mut s = InstallState::new("com.example.app");
        let staged = v("1.0.0");
        s.stage_version(&staged, None);
        assert!(!s.is_good(&staged), "staged has not proved healthy");
        assert!(!s.is_bad(&staged), "staged has not failed either");

        s.mark_good(&staged);
        assert!(s.is_good(&staged));
        assert!(!s.is_bad(&staged));

        s.mark_bad(&staged, "crashed on startup");
        assert!(s.is_bad(&staged));
        assert!(!s.is_good(&staged));
        assert_eq!(
            s.record(&staged).unwrap().failure_reason.as_deref(),
            Some("crashed on startup")
        );

        // An unknown version is neither good nor bad.
        assert!(!s.is_good(&v("9.9.9")));
        assert!(!s.is_bad(&v("9.9.9")));
    }

    #[test]
    fn marking_a_version_good_clears_an_earlier_failure_reason() {
        let mut s = InstallState::new("com.example.app");
        let ver = v("1.0.0");
        s.stage_version(&ver, None);
        s.mark_bad(&ver, "transient disk error");
        s.mark_good(&ver);
        assert!(s.record(&ver).unwrap().failure_reason.is_none());
    }

    #[test]
    fn an_incomplete_install_has_no_active_version() {
        let s = InstallState::new("com.example.app");
        assert!(s.active().is_err());
        assert!(s.update.is_idle());
        assert_eq!(s.update.version(), None);
    }

    #[test]
    fn each_phase_reports_the_version_it_concerns() {
        assert_eq!(UpdatePhase::Downloading { version: v("1.2.0") }.version(), Some(&v("1.2.0")));
        assert_eq!(UpdatePhase::Staged { version: v("1.2.0") }.version(), Some(&v("1.2.0")));
        // A rollback is identified by the version being abandoned.
        let rolling = UpdatePhase::RollingBack { from: v("1.2.0"), to: v("1.1.0") };
        assert_eq!(rolling.version(), Some(&v("1.2.0")));
        assert!(!rolling.is_idle());
    }

    #[test]
    fn phases_describe_themselves_readably() {
        let phase = UpdatePhase::PendingVerification {
            version: v("1.1.0"),
            rollback_to: v("1.0.0"),
            attempts: 1,
        };
        let text = phase.describe();
        assert!(text.contains("1.1.0"), "{text}");
        assert!(text.contains("1.0.0"), "{text}");
        assert!(!text.contains('{'), "must not leak struct syntax: {text}");
        assert_eq!(UpdatePhase::Idle.describe(), "idle");
    }

    #[test]
    fn phase_round_trips_through_json() {
        let phase = UpdatePhase::PendingVerification {
            version: v("1.2.0"),
            rollback_to: v("1.1.0"),
            attempts: 0,
        };
        let json = serde_json::to_string(&phase).unwrap();
        assert_eq!(serde_json::from_str::<UpdatePhase>(&json).unwrap(), phase);
    }
}

#[cfg(test)]
mod update_check_tests {
    use super::*;

    fn state() -> InstallState {
        InstallState::new("com.example.app")
    }

    const HOUR: u64 = 3600;

    #[test]
    fn a_version_is_announced_until_it_has_been() {
        let mut state = state();
        let version = Version::parse("1.1.0").unwrap();
        assert!(state.update_needs_announcing(&version), "a new version was not announced");
        state.announced_update = Some(version.clone());
        assert!(!state.update_needs_announcing(&version), "the same version was announced twice");
    }

    #[test]
    fn a_newer_version_is_announced_even_after_an_older_one_was() {
        // Holding the version rather than a flag is what makes this work with
        // nothing to reset.
        let mut state = state();
        state.announced_update = Some(Version::parse("1.1.0").unwrap());
        assert!(state.update_needs_announcing(&Version::parse("1.2.0").unwrap()));
    }

    #[test]
    fn the_announced_version_survives_a_round_trip() {
        let mut s = state();
        s.announced_update = Some(Version::parse("2.0.0").unwrap());
        let bytes = serde_json::to_vec(&s).unwrap();
        let back: InstallState = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.announced_update, Some(Version::parse("2.0.0").unwrap()));
    }

    #[test]
    fn a_first_check_is_always_due() {
        assert!(state().update_check_is_due(1_000_000, 4 * HOUR));
    }

    #[test]
    fn a_check_inside_the_interval_is_not_due() {
        let mut s = state();
        s.last_update_check = Some(1_000_000);
        assert!(!s.update_check_is_due(1_000_000 + HOUR, 4 * HOUR));
    }

    #[test]
    fn a_check_at_exactly_the_interval_is_due() {
        let mut s = state();
        s.last_update_check = Some(1_000_000);
        assert!(s.update_check_is_due(1_000_000 + 4 * HOUR, 4 * HOUR));
    }

    #[test]
    fn a_clock_that_moved_backwards_does_not_block_checks_forever() {
        // A correction, a dual boot or a resumed snapshot can leave the
        // recorded instant in the future. Waiting it out could mean never
        // checking again.
        let mut s = state();
        s.last_update_check = Some(2_000_000);
        assert!(s.update_check_is_due(1_000_000, 4 * HOUR));
    }

    #[test]
    fn the_last_check_survives_a_round_trip() {
        let mut s = state();
        s.last_update_check = Some(1_234_567);
        let json = serde_json::to_vec(&s).unwrap();
        let back: InstallState = serde_json::from_slice(&json).unwrap();
        assert_eq!(back.last_update_check, Some(1_234_567));
    }

    #[test]
    fn state_written_before_this_field_existed_still_loads() {
        // Older installations have no such field, and an update must not
        // refuse to read the state it is meant to update.
        let json = br#"{"stateFormatVersion":1,"applicationId":"com.example.app"}"#;
        let s: InstallState = serde_json::from_slice(json).unwrap();
        assert_eq!(s.last_update_check, None);
        assert!(s.update_check_is_due(1_000_000, 4 * HOUR));
    }
}
