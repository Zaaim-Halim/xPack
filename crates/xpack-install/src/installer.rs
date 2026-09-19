//! Installing, activating, rolling back and removing versions.

use xpack_core::atomic;
use xpack_core::state::{UpdatePhase, VersionStatus};
use xpack_core::{Error, InstallState, Platform, Result, Version};
use xpack_package::VerifiedPackage;
use xpack_platform::InstallLock;

use crate::recovery::{self, RecoveryReport};

/// Choices a caller makes when installing.
#[derive(Debug, Clone, Default)]
pub struct InstallOptions {
    /// Permit installing a version that is not newer than the active one.
    pub allow_downgrade: bool,
    /// Make the installed version active immediately.
    pub activate: bool,
}

/// The result of a successful install.
#[derive(Debug, Clone)]
pub struct Installed {
    /// Version now present under `versions/`.
    pub version: Version,
    /// Whether it was also made active.
    pub activated: bool,
    /// What recovery cleaned up beforehand.
    pub recovery: RecoveryReport,
}

/// Drives an installation. Every method runs under the lock it borrows.
#[derive(Debug)]
pub struct Installer<'lock> {
    lock: &'lock InstallLock,
}

impl<'lock> Installer<'lock> {
    /// Wraps a held lock.
    pub fn new(lock: &'lock InstallLock) -> Self {
        Self { lock }
    }

    /// The installation being operated on.
    pub fn lock(&self) -> &InstallLock {
        self.lock
    }

    /// Resolves whatever a previous interrupted run left behind.
    pub fn recover(&self) -> Result<RecoveryReport> {
        recovery::recover(self.lock)
    }

    /// Installs a verified package.
    pub fn install(
        &self,
        package: &mut VerifiedPackage,
        options: &InstallOptions,
    ) -> Result<Installed> {
        let report = self.recover()?;
        let paths = self.lock.paths();
        let manifest = package.manifest().clone();
        let version = manifest.application.version.clone();

        // The installation and the package each carry an application id, and
        // nothing else forces them to agree. Installing one application's
        // package into another's directory would corrupt both.
        self.ensure_same_application(&manifest.application.id)?;
        package.ensure_installable_on(Platform::host()?)?;

        let mut state = self.load_state()?;

        // Checked before the downgrade rule so reinstalling the active version
        // reports what is actually wrong, rather than "downgrades are
        // rejected", which describes a different problem entirely.
        let is_repair = if state.record(&version).is_some() {
            if self.is_usable(&version) {
                return Err(Error::invalid(
                    "install",
                    format!("version {version} is already installed"),
                ));
            }
            // State records it but the files are gone. Dropping the record
            // lets the install proceed as a repair.
            tracing::warn!(%version, "reinstalling a version whose files are missing");
            state.versions.remove(&version.to_string());
            self.lock.save_state(&state)?;
            true
        } else {
            false
        };

        // A repair restores files that state already expects, so it is not a
        // version change and the downgrade rule does not apply — refusing
        // would leave a user unable to fix their own installation, and
        // repairing the *active* version always compares equal to itself.
        //
        // Nothing is weakened by this: activation checks the rule again, so a
        // repaired older version still cannot become active without passing
        // it or an explicit override.
        if !is_repair {
            state.ensure_not_downgrade(&version, options.allow_downgrade)?;
        }

        // Extraction happens in staging and is promoted only once every hash
        // has matched, so a crash can never leave a half-written tree under
        // `versions/` that looks complete.
        state.update = UpdatePhase::Installing { version: version.clone() };
        self.lock.save_state(&state)?;

        let staging = paths.staging_dir(&version);
        package.extract_to(&staging)?;
        write_version_metadata(&staging, package)?;

        self.promote(&staging, &version, &state)?;

        state.stage_version(&version, None);
        state.update = UpdatePhase::Staged { version: version.clone() };
        self.lock.save_state(&state)?;

        tracing::info!(%version, "version installed");

        let activated = if options.activate {
            // Re-activating the version that is already current is a no-op for
            // the downgrade rule; a repair must be allowed to restore it.
            let already_current = self.load_state()?.current_version.as_ref() == Some(&version);
            self.activate(&version, options.allow_downgrade || (is_repair && already_current))?;
            true
        } else {
            false
        };

        Ok(Installed { version, activated, recovery: report })
    }

    /// Moves a fully verified staging tree into place.
    ///
    /// `rename` onto a non-empty directory fails, so the destination must not
    /// exist. A directory state already records is a real installed version
    /// and is never removed here; one state has never heard of is crash
    /// debris and is cleared. Staging and `versions/` share a filesystem, so
    /// the rename is atomic.
    fn promote(
        &self,
        staging: &std::path::Path,
        version: &Version,
        state: &InstallState,
    ) -> Result<()> {
        let destination = self.lock.paths().version_dir(version);
        if destination.exists() {
            if state.record(version).is_some() {
                return Err(Error::invalid(
                    "install",
                    format!("{} already exists for an installed version", destination.display()),
                ));
            }
            atomic::remove_dir_all_if_exists(&destination)?;
        }
        atomic::create_dir_all(atomic::parent_dir(&destination)?)?;
        std::fs::rename(staging, &destination).map_err(|e| Error::io(&destination, e))?;
        atomic::sync_dir(atomic::parent_dir(&destination)?)
    }

    /// Makes an installed version active and puts it on probation.
    ///
    /// The active version and the probation record are written together. Two
    /// writes would leave a crash window in which `current` names an unproven
    /// version with no recorded rollback target.
    pub fn activate(&self, version: &Version, allow_downgrade: bool) -> Result<()> {
        let mut state = self.load_state()?;

        if state.record(version).is_none() {
            return Err(Error::VersionNotInstalled(version.to_string()));
        }
        // State and the filesystem can disagree: a user may have deleted a
        // version directory by hand, or a previous removal may have partly
        // succeeded. Activating a version that is not on disk would leave
        // `current` naming something that cannot be launched.
        if !self.is_usable(version) {
            return Err(Error::VersionNotInstalled(format!(
                "{version} is recorded but its files are missing"
            )));
        }
        if state.is_bad(version) {
            return Err(Error::invalid(
                "activate",
                format!("version {version} previously failed its health check"),
            ));
        }
        // Checked again here, not only before download: state can change
        // during a long operation.
        state.ensure_not_downgrade(version, allow_downgrade)?;

        let previous = state.current_version.clone();
        state.current_version = Some(version.clone());
        if let Some(previous) = &previous {
            state.previous_version = Some(previous.clone());
        }
        state.update = match &previous {
            Some(previous) => UpdatePhase::PendingVerification {
                version: version.clone(),
                rollback_to: previous.clone(),
                attempts: 0,
            },
            // A first install has nothing to roll back to, so there is no
            // probation to run: it is simply active.
            None => UpdatePhase::Idle,
        };
        if previous.is_none() {
            state.mark_good(version);
        }
        self.lock.save_state(&state)?;

        xpack_platform::update_current_link(self.lock.paths(), version).log();
        tracing::info!(%version, "version activated");
        Ok(())
    }

    // --- health primitives ------------------------------------------------
    //
    // The launcher decides *when* probation passes. These only record it.

    /// Records that a probationary launch is about to be attempted.
    ///
    /// Persisted *before* the launch, because a counter incremented afterwards
    /// never records the crash that prevented the increment.
    pub fn begin_attempt(&self) -> Result<UpdatePhase> {
        let mut state = self.load_state()?;
        state.update.record_attempt();
        let phase = state.update.clone();
        self.lock.save_state(&state)?;
        Ok(phase)
    }

    /// Marks the active version healthy and ends probation.
    pub fn commit_health(&self) -> Result<()> {
        let mut state = self.load_state()?;
        let active = state.active()?.clone();
        state.mark_good(&active);
        state.update = UpdatePhase::Idle;
        self.lock.save_state(&state)?;
        tracing::info!(version = %active, "version committed as healthy");
        Ok(())
    }

    /// Records that the active version failed, and rolls back if possible.
    ///
    /// Returning `Ok(None)` is a **terminal state**: the active version is
    /// quarantined and there is nothing healthier to fall back to. The
    /// installation is left naming that version deliberately — erasing
    /// `current` would leave nothing to report and nothing to repair — so a
    /// caller seeing `None` must surface it rather than retrying. Use
    /// [`Installer::is_terminal_failure`] to detect it.
    pub fn record_failure(&self, reason: impl Into<String>) -> Result<Option<Version>> {
        let reason = reason.into();
        let mut state = self.load_state()?;
        let active = state.active()?.clone();
        state.mark_bad(&active, reason.clone());
        self.lock.save_state(&state)?;
        tracing::warn!(version = %active, reason, "version failed its health check");
        self.rollback()
    }

    /// Returns to the newest healthy version that is actually present.
    ///
    /// Rollback is the recovery mechanism, so it must never recover *into* a
    /// broken state. A recorded version whose files have gone is skipped and
    /// quarantined, and the search continues, rather than switching to
    /// something that cannot be launched.
    pub fn rollback(&self) -> Result<Option<Version>> {
        let mut state = self.load_state()?;
        let active = state.active()?.clone();

        let target = loop {
            let Some(candidate) = state.best_rollback_target(&active) else {
                tracing::error!(version = %active, "no healthy version to roll back to");
                return Ok(None);
            };
            if self.is_usable(&candidate) {
                break candidate;
            }
            tracing::warn!(version = %candidate, "rollback target is missing its files; skipping");
            state.mark_bad(&candidate, "version directory is missing");
            self.lock.save_state(&state)?;
        };

        state.update = UpdatePhase::RollingBack { from: active.clone(), to: target.clone() };
        self.lock.save_state(&state)?;

        state.mark_bad(&active, "an update was rolled back");
        state.current_version = Some(target.clone());
        state.update = UpdatePhase::Idle;
        self.lock.save_state(&state)?;

        xpack_platform::update_current_link(self.lock.paths(), &target).log();
        tracing::warn!(from = %active, to = %target, "rolled back");
        Ok(Some(target))
    }

    /// Removes versions that are no longer needed.
    ///
    /// Never removes the active version, the recorded previous version, or the
    /// version rollback would currently choose. On Windows a running
    /// executable holds its files open, so removal can fail; that is logged
    /// and retried by a later operation rather than failing the caller.
    pub fn prune(&self) -> Result<Vec<Version>> {
        let mut state = self.load_state()?;
        let paths = self.lock.paths();

        let active = state.current_version.clone();
        let mut keep: Vec<Version> = Vec::new();
        if let Some(active) = &active {
            keep.push(active.clone());
            if let Some(target) = state.best_rollback_target(active) {
                keep.push(target);
            }
        }
        if let Some(previous) = &state.previous_version {
            keep.push(previous.clone());
        }

        let candidates: Vec<Version> = state
            .versions
            .keys()
            .filter_map(|v| Version::parse(v).ok())
            .filter(|v| !keep.contains(v))
            .collect();

        let mut removed = Vec::new();
        for version in candidates {
            match atomic::remove_dir_all_if_exists(&paths.version_dir(&version)) {
                Ok(()) => {
                    state.versions.remove(&version.to_string());
                    removed.push(version);
                }
                Err(e) => {
                    tracing::warn!(%version, error = %e, "could not remove version; will retry later");
                }
            }
        }

        if !removed.is_empty() {
            self.lock.save_state(&state)?;
        }
        Ok(removed)
    }

    /// Removes the entire installation.
    ///
    /// Only the installation root. User data lives wherever the application
    /// chose to put it, which xPack does not know and will not guess at.
    pub fn uninstall(&self) -> Result<()> {
        let paths = self.lock.paths();
        for entry in [paths.versions_dir(), paths.staging_root(), paths.downloads_dir()] {
            atomic::remove_dir_all_if_exists(&entry)?;
        }
        atomic::remove_file_if_exists(&paths.current_link())?;

        let mut state = self.load_state()?;
        state.current_version = None;
        state.previous_version = None;
        state.versions.clear();
        state.update = UpdatePhase::Idle;
        self.lock.save_state(&state)?;

        tracing::info!(root = %paths.root().display(), "installation removed");
        Ok(())
    }

    /// Returns `true` when the active version is quarantined with no way back.
    ///
    /// Nothing automatic can resolve this; it needs a new version or a repair.
    pub fn is_terminal_failure(&self) -> Result<bool> {
        let state = self.load_state()?;
        let Some(active) = &state.current_version else {
            return Ok(false);
        };
        Ok(state.is_bad(active) && state.best_rollback_target(active).is_none())
    }

    /// Lists installed versions, newest first, with their status.
    pub fn installed(&self) -> Result<Vec<(Version, VersionStatus)>> {
        let state = self.load_state()?;
        let mut out: Vec<(Version, VersionStatus)> = state
            .versions
            .iter()
            .filter_map(|(v, r)| Version::parse(v).ok().map(|v| (v, r.status)))
            .collect();
        out.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(out)
    }

    /// Returns `true` when a version is recorded *and* present on disk.
    ///
    /// State and the filesystem are separate sources of truth, and they can
    /// diverge: a user deletes a directory, a removal half-succeeds, a disk
    /// fails. Every operation that makes a version active consults both.
    pub fn is_usable(&self, version: &Version) -> bool {
        let paths = self.lock.paths();
        paths.version_dir(version).is_dir() && paths.version_manifest_file(version).is_file()
    }

    fn load_state(&self) -> Result<InstallState> {
        let id = self
            .lock
            .paths()
            .application_id()
            .ok_or_else(|| Error::invalid("installation", "root has no application id"))?;
        self.lock.load_or_new_state(id)
    }

    fn ensure_same_application(&self, package_id: &str) -> Result<()> {
        let Some(expected) = self.lock.paths().application_id() else {
            return Ok(());
        };
        if package_id == expected {
            return Ok(());
        }
        Err(Error::invalid(
            "install",
            format!("package is for {package_id:?} but this installation is {expected:?}"),
        ))
    }
}

/// Records the manifest and signature a version was installed from.
///
/// Written into staging *before* promotion, so a promoted version always has
/// its metadata. Writing afterwards would leave a window in which a complete
/// version exists that can never serve as a differential update base, with
/// nothing marking it incomplete.
///
/// The manifest bytes are written verbatim. Re-serialising a parsed manifest
/// would produce different bytes and the signature beside them would no longer
/// verify.
fn write_version_metadata(staging: &std::path::Path, package: &VerifiedPackage) -> Result<()> {
    let dir = staging.join(xpack_core::manifest::RESERVED_METADATA_DIR);
    atomic::create_dir_all(&dir)?;
    atomic::write(&dir.join(xpack_core::manifest::MANIFEST_ENTRY), package.manifest_bytes())?;
    atomic::write(
        &dir.join(xpack_core::manifest::SIGNATURE_ENTRY),
        format!("{}\n", package.signature().to_hex()).as_bytes(),
    )?;
    Ok(())
}
