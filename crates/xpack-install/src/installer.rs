//! Installing, activating, rolling back and removing versions.

use std::path::{Path, PathBuf};

use xpack_core::atomic;
use xpack_core::progress::{NoProgress, ProgressEvent, ProgressReporter};
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
    /// Uninstaller to place in the installation root, if any.
    ///
    /// Same rule again: placed only when absent.
    pub uninstaller: Option<PathBuf>,
    /// Background updater to place in the installation root, if any.
    ///
    /// Optional for the same reason as the launcher, and placed by the same
    /// rule: only when absent. An installation without one is complete and
    /// correct, it simply never checks for updates on its own.
    pub updater: Option<PathBuf>,
    /// Launcher binary to place in the installation root, if any.
    ///
    /// The caller supplies the path rather than this crate finding one. A
    /// library has no business deciding where an executable comes from, and
    /// the answer differs by deployment: a bootstrap installer ships one
    /// alongside itself, a developer running the CLI has one in the same build
    /// directory.
    ///
    /// Without a launcher the installation is complete and correct but has no
    /// entry point, which is only useful when something else provides one.
    pub launcher: Option<PathBuf>,
    /// Where desktop entries are written, when the manifest asks for one.
    ///
    /// `None` uses the directories belonging to the user running this, which
    /// is what every real installation wants. A test supplies a temporary
    /// directory instead — without this, a test whose manifest asked for a
    /// shortcut would write a real entry into the developer's own application
    /// menu and leave it there.
    pub desktop_roots: Option<crate::integration::Roots>,
    /// Windowed launcher to place beside the console one, if any.
    ///
    /// Only meaningful on Windows, where a console build opened from a
    /// shortcut shows an empty console window behind the application. The
    /// caller decides whether to supply one; this crate does not consult the
    /// host platform, so a cross-platform bootstrap installer stays in charge
    /// of what it ships.
    pub gui_launcher: Option<PathBuf>,
}

/// What installing a launcher did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherOutcome {
    /// The binary was written into the installation root.
    Installed,
    /// A launcher was already there and was left untouched.
    AlreadyPresent,
}

/// The result of a successful install.
#[derive(Debug, Clone)]
pub struct Installed {
    /// Version now present under `versions/`.
    pub version: Version,
    /// Whether it was also made active.
    pub activated: bool,
    /// What happened to the launcher, if one was supplied.
    pub launcher: Option<LauncherOutcome>,
    /// What happened to the windowed launcher, if one was supplied.
    pub gui_launcher: Option<LauncherOutcome>,
    /// What happened to the background updater, if one was supplied.
    pub updater: Option<LauncherOutcome>,
    /// What happened to the uninstaller, if one was supplied.
    pub uninstaller: Option<LauncherOutcome>,
    /// What happened to the desktop entry the manifest asked for.
    pub desktop: crate::integration::Outcome,
    /// What recovery cleaned up beforehand.
    pub recovery: RecoveryReport,
}

/// Where an installed version's files come from.
///
/// A full package extracts; a delta assembles from the version already on
/// disk. Everything *else* about installing — recovery, the downgrade rule,
/// staging, promotion, the phase writes, activation, the desktop entry — is
/// identical, and a second copy of it for deltas is how the two would come to
/// disagree about something that matters.
///
/// Both sources verify every byte against the same signed manifest before it
/// reaches staging, which is why the rest of the install path does not need to
/// know which one it was handed.
pub trait InstallSource {
    /// The signed manifest describing the version.
    fn manifest(&self) -> &xpack_core::Manifest;

    /// The exact bytes the signature was verified over.
    fn manifest_bytes(&self) -> &[u8];

    /// The signature over those bytes.
    fn signature(&self) -> &xpack_security::Signature;

    /// Writes the version's files into `staging`, verifying every one.
    fn materialise(&mut self, staging: &Path, progress: &dyn ProgressReporter) -> Result<()>;

    /// Refuses a version built for a different platform.
    ///
    /// Defaulted, because the answer is a property of the signed manifest and
    /// not of how the files arrive.
    fn ensure_installable_on(&self, host: Platform) -> Result<()> {
        let package = self.manifest().platform;
        if package.accepts(host) {
            return Ok(());
        }
        Err(Error::PlatformMismatch { package: package.to_string(), host: host.to_string() })
    }
}

impl InstallSource for VerifiedPackage {
    fn manifest(&self) -> &xpack_core::Manifest {
        Self::manifest(self)
    }

    fn manifest_bytes(&self) -> &[u8] {
        Self::manifest_bytes(self)
    }

    fn signature(&self) -> &xpack_security::Signature {
        Self::signature(self)
    }

    fn materialise(&mut self, staging: &Path, progress: &dyn ProgressReporter) -> Result<()> {
        self.extract_to_with_progress(staging, progress)
    }
}

/// A delta, together with the installed version it rebuilds from.
///
/// Holding the base directory here rather than passing it separately means a
/// delta cannot reach the install path without one.
pub struct DeltaSource<'a> {
    delta: &'a mut xpack_package::VerifiedDelta,
    base_dir: PathBuf,
}

impl<'a> DeltaSource<'a> {
    /// Pairs a verified delta with the directory of the version it applies to.
    ///
    /// The installed version is supplied by the caller from **state**, and the
    /// delta's own claim is checked against it — never the reverse, or
    /// whoever served the index would choose which directory is read.
    pub fn new(
        delta: &'a mut xpack_package::VerifiedDelta,
        installed: &Version,
        base_dir: impl Into<PathBuf>,
    ) -> Result<Self> {
        delta.ensure_applies_to(installed)?;
        Ok(Self { delta, base_dir: base_dir.into() })
    }
}

impl InstallSource for DeltaSource<'_> {
    fn manifest(&self) -> &xpack_core::Manifest {
        self.delta.manifest()
    }

    fn manifest_bytes(&self) -> &[u8] {
        self.delta.manifest_bytes()
    }

    fn signature(&self) -> &xpack_security::Signature {
        self.delta.signature()
    }

    fn materialise(&mut self, staging: &Path, progress: &dyn ProgressReporter) -> Result<()> {
        self.delta.assemble_to_with_progress(staging, &self.base_dir, progress)
    }
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
        self.install_with_progress(package, options, &NoProgress)
    }

    /// Installs from any verified source: a full package or an assembled delta.
    pub fn install_from(
        &self,
        source: &mut dyn InstallSource,
        options: &InstallOptions,
    ) -> Result<Installed> {
        self.install_from_with_progress(source, options, &NoProgress)
    }

    /// Installs, reporting extraction and activation as they happen.
    ///
    /// [`Self::install`] is this with a reporter that discards everything, so
    /// both paths run identical code and every existing test exercises the
    /// same sequence it always did.
    pub fn install_with_progress(
        &self,
        package: &mut VerifiedPackage,
        options: &InstallOptions,
        progress: &dyn ProgressReporter,
    ) -> Result<Installed> {
        self.install_from_with_progress(package, options, progress)
    }

    /// Installs from any verified source, reporting progress.
    pub fn install_from_with_progress(
        &self,
        package: &mut dyn InstallSource,
        options: &InstallOptions,
        progress: &dyn ProgressReporter,
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

        // Checked here rather than at launch. Windows cannot start a program
        // from a path longer than `MAX_PATH`, however well the package
        // extracted, so a version that lands too deep installs cleanly and
        // then cannot be opened. Refusing now reports it while the user is
        // still watching the install they asked for.
        xpack_core::ensure_launch_paths_fit(
            &paths.version_dir(&version),
            &manifest.launch,
            xpack_core::host_launch_path_limit(),
        )?;

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
        package.materialise(&staging, progress)?;
        write_version_metadata(&staging, package)?;

        self.promote(&staging, &version, &state)?;

        state.stage_version(&version, None);

        // Read from the manifest that was just verified, never from the update
        // index: a server that could declare releases mandatory could stop an
        // application starting whenever it liked. Raised only, so an older
        // release cannot lower a requirement a newer one set.
        if manifest.update.mandatory
            && state.required_version.as_ref().is_none_or(|current| current < &version)
        {
            tracing::info!(%version, "this release is mandatory; older versions will not start");
            state.required_version = Some(version.clone());
        }

        state.update = UpdatePhase::Staged { version: version.clone() };
        self.lock.save_state(&state)?;

        tracing::info!(%version, "version installed");

        // Before activation, so that a version becoming current always has an
        // entry point by the time anything could try to start it.
        let launcher = match &options.launcher {
            Some(source) => Some(self.install_launcher(source)?),
            None => None,
        };
        let gui_launcher = match &options.gui_launcher {
            Some(source) => Some(self.install_gui_launcher(source)?),
            None => None,
        };
        let updater = match &options.updater {
            Some(source) => Some(self.install_updater(source)?),
            None => None,
        };
        let uninstaller = match &options.uninstaller {
            Some(source) => Some(self.install_uninstaller(source)?),
            None => None,
        };

        // After the binaries, because the entry points at one of them, and a
        // shortcut to a launcher that is not there yet would be broken for as
        // long as the window between the two writes lasted.
        //
        // Never fails the install: see the integration module for why.
        let desktop =
            self.update_desktop_entry(&manifest, &version, options.desktop_roots.as_ref());

        let activated = if options.activate {
            progress.report(&ProgressEvent::Activating { version: version.clone() });
            // Re-activating the version that is already current is a no-op for
            // the downgrade rule; a repair must be allowed to restore it.
            let already_current = self.load_state()?.current_version.as_ref() == Some(&version);
            self.activate(&version, options.allow_downgrade || (is_repair && already_current))?;
            true
        } else {
            false
        };

        Ok(Installed {
            version,
            activated,
            launcher,
            gui_launcher,
            updater,
            uninstaller,
            desktop,
            recovery: report,
        })
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
            return Err(Error::invalid(
                "activate",
                format!("{version} is recorded but its files are missing"),
            ));
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

    /// Places the launcher binary in the installation root.
    ///
    /// # Only when absent
    ///
    /// An existing launcher is left alone. It is not versioned with the
    /// application — it belongs to xPack, reads state on every run and is
    /// replaced only when xPack itself changes — so an application update has
    /// no reason to rewrite it. Overwriting anyway would fail on Windows for
    /// the worst possible reason: the launcher stays resident to watch its
    /// application start, so the file is locked exactly when an update is most
    /// likely to be running.
    ///
    /// Replacing a launcher is therefore a deliberate xPack upgrade and needs
    /// its own path, not a silent side effect of installing an application.
    pub fn install_launcher(&self, source: &Path) -> Result<LauncherOutcome> {
        Self::install_binary(source, &self.lock.paths().launcher_file(), "launcher")
    }

    /// Creates or refreshes the desktop entry the manifest asked for.
    ///
    /// Run on every install, not only the first. The entry records the
    /// version, and on Windows the installed size, so an update that left the
    /// old entry in place would leave the installed-apps list describing a
    /// version that is no longer there.
    ///
    /// Returns an outcome rather than a `Result` so that a caller cannot fail
    /// an otherwise perfect installation with `?` because a menu entry could
    /// not be written.
    fn update_desktop_entry(
        &self,
        manifest: &xpack_core::Manifest,
        version: &Version,
        roots: Option<&crate::integration::Roots>,
    ) -> crate::integration::Outcome {
        let paths = self.lock.paths();
        let Some(mut entry) = crate::integration::Entry::from_manifest(manifest, paths) else {
            return crate::integration::Outcome::NotRequested;
        };

        // The launcher the entry names has to be on disk. A shortcut to a
        // missing executable is worse than no shortcut: it looks correct, and
        // it fails with "no such file" naming a path the user can see.
        //
        // This is reachable without contrivance. `--no-launcher` installs
        // neither build, and the background updater supplies neither, so an
        // installation that never had the windowed build would otherwise have
        // its entry rewritten to point at it by an update nobody watched.
        //
        // The same rule `update_current_link` applies to `current`.
        match resolve_entry_target(&entry, paths) {
            Some(target) => entry.target = target,
            None => {
                return crate::integration::Outcome::Failed(format!(
                    "{} is not installed, so a desktop entry would point at nothing",
                    entry.target.display()
                ));
            }
        }

        // The icon is copied out of the version directory first, because the
        // entry points at the copy: a version directory is replaced by the
        // next update and an entry pointing into one goes stale.
        entry.icon = crate::integration::place_icon(paths, manifest, version);

        let outcome = match roots {
            Some(roots) => crate::integration::install_into(&entry, roots),
            None => crate::integration::install(&entry),
        };
        outcome.log("install");
        outcome
    }

    /// Places the windowed launcher in the installation root.
    ///
    /// Same rule as the console build, and it is the build a desktop shortcut
    /// points at, so replacing it would break every shortcut that resolved
    /// while the file was gone.
    pub fn install_gui_launcher(&self, source: &Path) -> Result<LauncherOutcome> {
        Self::install_binary(source, &self.lock.paths().gui_launcher_file(), "windowed launcher")
    }

    /// Places the background updater in the installation root.
    ///
    /// Same rule as the launcher, and for a sharper version of the same
    /// reason: the updater may well be running right now — the launcher starts
    /// it on every application start — so replacing it is exactly the write
    /// Windows refuses.
    pub fn install_updater(&self, source: &Path) -> Result<LauncherOutcome> {
        Self::install_binary(source, &self.lock.paths().updater_file(), "updater")
    }

    /// Places the uninstaller in the installation root.
    pub fn install_uninstaller(&self, source: &Path) -> Result<LauncherOutcome> {
        Self::install_binary(source, &self.lock.paths().uninstaller_file(), "uninstaller")
    }

    /// Copies an executable into the installation, if nothing is there yet.
    ///
    /// Takes no `self`: the destination is already resolved by the caller, and
    /// the installation lock is held by whoever called into the installer.
    fn install_binary(source: &Path, destination: &Path, what: &str) -> Result<LauncherOutcome> {
        if destination.exists() {
            tracing::debug!(path = %destination.display(), what, "already present");
            return Ok(LauncherOutcome::AlreadyPresent);
        }

        let bytes = std::fs::read(source).map_err(|e| Error::io(source, e))?;
        if bytes.is_empty() {
            return Err(Error::invalid(
                what,
                format!("{} is empty and cannot be an executable", source.display()),
            ));
        }

        atomic::create_dir_all(atomic::parent_dir(destination)?)?;
        atomic::write(destination, &bytes)?;

        // A binary that exists but cannot be executed is worse than one that is
        // missing: the installation looks complete and fails at the moment a
        // user tries to open their application. Undo rather than ship that.
        if let Err(e) = set_executable(destination) {
            let _ = std::fs::remove_file(destination);
            return Err(e);
        }

        tracing::info!(path = %destination.display(), what, "installed");
        Ok(LauncherOutcome::Installed)
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
fn write_version_metadata(staging: &std::path::Path, package: &dyn InstallSource) -> Result<()> {
    let dir = staging.join(xpack_core::manifest::RESERVED_METADATA_DIR);
    atomic::create_dir_all(&dir)?;
    atomic::write(&dir.join(xpack_core::manifest::MANIFEST_ENTRY), package.manifest_bytes())?;
    atomic::write(
        &dir.join(xpack_core::manifest::SIGNATURE_ENTRY),
        format!("{}\n", package.signature().to_hex()).as_bytes(),
    )?;
    Ok(())
}

/// Marks a file executable by its owner, and readable and executable by all.
///
/// Windows has no equivalent bit — executability there comes from the file
/// extension, which [`InstallPaths::launcher_file`] already supplies.
///
/// [`InstallPaths::launcher_file`]: xpack_core::InstallPaths::launcher_file
#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| Error::io(path, e))
}

// Mirrors the Unix version's signature, which genuinely can fail.
#[allow(clippy::unnecessary_wraps)]
#[cfg(not(unix))]
fn set_executable(path: &Path) -> Result<()> {
    let _ = path;
    Ok(())
}

/// What a removal managed to delete, and what it did not.
#[derive(Debug, Clone)]
pub struct Removal {
    /// The installation root that was targeted.
    pub root: PathBuf,
    /// Whether the root directory itself is now gone.
    pub root_removed: bool,
    /// Entries still present in the root, when it could not be removed.
    pub remaining: Vec<PathBuf>,
    /// What happened to the desktop entry, if there was one.
    pub desktop: crate::integration::Outcome,
}

impl Removal {
    /// Returns `true` when nothing at all was left behind.
    pub fn is_complete(&self) -> bool {
        self.root_removed
    }
}

/// Removes an installation completely.
///
/// # Why this consumes the lock
///
/// The lock file lives at `state/update.lock`, inside the tree being deleted,
/// and the lock is held by an open handle to it. Unix would tolerate deleting
/// it anyway — the inode survives until the handle closes — but Windows
/// refuses to delete a file that is open, so an uninstall written against Unix
/// behaviour would leave `state/` behind on every Windows machine.
///
/// Taking [`InstallLock`] by value is what makes the ordering enforceable: the
/// lock is dropped partway through, and afterwards it is gone from the
/// caller's hands too, so nothing can use a guard that no longer guards
/// anything.
///
/// # Two phases
///
/// Everything that matters is deleted under the lock, including the pinned
/// signing keys in `config/`. State is cleared and saved first, so an
/// interruption leaves a coherent empty installation rather than a full record
/// pointing at versions that are already gone.
///
/// Only then is the lock released and `state/` removed, followed by the root.
///
/// # The root is removed non-recursively
///
/// Between releasing the lock and removing the root, another process can
/// acquire the lock and begin installing. A recursive delete would destroy its
/// work. [`std::fs::remove_dir`] fails harmlessly on a directory that is no
/// longer empty, which turns that race into an accurate report instead of data
/// loss.
///
/// It also declines to delete anything a user put in the root themselves, for
/// the same reason and with the same outcome: the path is reported in
/// [`Removal::remaining`] rather than removed.
///
/// # What is never touched
///
/// Application data. It lives wherever the application chose to put it, which
/// xPack does not know and will not guess at.
pub fn uninstall(lock: InstallLock) -> Result<Removal> {
    uninstall_with_roots(lock, None)
}

/// Removes an installation, writing desktop changes under explicit roots.
///
/// See [`InstallOptions::desktop_roots`] for why this exists.
pub fn uninstall_with_roots(
    lock: InstallLock,
    desktop_roots: Option<&crate::integration::Roots>,
) -> Result<Removal> {
    let paths = lock.paths().clone();
    let root = paths.root().to_path_buf();

    // Resolved *before* state is cleared and the versions are deleted. The
    // Start-Menu shortcut and the macOS bundle are named after the display
    // name, which lives in the installed manifest — read it afterwards and
    // there is nothing left to read it from, and the entry is orphaned in the
    // user's menu with no way to find it again.
    let desktop_entry = desktop_entry_for_removal(&lock);

    // Cleared and persisted before anything is deleted, so an interruption
    // cannot leave state describing versions that no longer exist.
    let id = paths
        .application_id()
        .ok_or_else(|| Error::invalid("installation", "root has no application id"))?;
    let mut state = lock.load_or_new_state(id)?;
    state.current_version = None;
    state.previous_version = None;
    state.versions.clear();
    state.update = UpdatePhase::Idle;
    lock.save_state(&state)?;

    for directory in [paths.versions_dir(), paths.staging_root(), paths.downloads_dir()] {
        atomic::remove_dir_all_if_exists(&directory)?;
    }
    // `current` is a symlink on Unix and is never created on Windows, so
    // removing it as a file is right on both: on Unix this unlinks the link
    // and not the directory it names.
    atomic::remove_file_if_exists(&paths.current_link())?;
    atomic::remove_file_if_exists(&paths.launcher_file())?;
    atomic::remove_file_if_exists(&paths.gui_launcher_file())?;
    atomic::remove_file_if_exists(&paths.updater_file())?;
    atomic::remove_file_if_exists(&paths.uninstaller_file())?;

    // The icon copied into the root for the desktop entry, at the exact path
    // the manifest implies rather than anything matching `icon.*`. Globbing
    // would delete a user's own `icon.jpg` from the root, which this function
    // promises never to do — such a file is reported in `remaining` instead.
    if let Some(icon) = desktop_entry.as_ref().and_then(|entry| entry.icon.clone()) {
        atomic::remove_file_if_exists(&icon)?;
    }

    // The pinned signing keys. Leaving these behind is the consequential part
    // of an incomplete uninstall: a later reinstall would silently inherit a
    // trust decision the user believes they revoked.
    atomic::remove_dir_all_if_exists(&paths.config_dir())?;

    // Before the lock is released, so it cannot race an install that starts
    // the moment the lock is free and recreates the entry we are removing.
    let desktop = match &desktop_entry {
        Some(entry) => {
            let outcome = match desktop_roots {
                Some(roots) => crate::integration::remove_from(entry, roots),
                None => crate::integration::remove(entry),
            };
            outcome.log("uninstall");
            outcome
        }
        None => crate::integration::Outcome::NothingToDo,
    };

    // Releases the lock and closes the handle to the file inside `state/`.
    // Everything after this point runs unlocked, which is why it is ordered
    // last and why the root removal cannot recurse.
    drop(lock);

    atomic::remove_dir_all_if_exists(&paths.state_dir())?;

    let root_removed = match std::fs::remove_dir(&root) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    };

    let remaining = if root_removed { Vec::new() } else { entries_in(&root) };
    if !remaining.is_empty() {
        tracing::warn!(
            root = %root.display(),
            count = remaining.len(),
            "installation root was not empty and was left in place"
        );
    }
    tracing::info!(root = %root.display(), root_removed, "installation removed");
    Ok(Removal { root, root_removed, remaining, desktop })
}

/// Picks a launcher that actually exists for a desktop entry to point at.
///
/// Prefers the one the manifest implies, and falls back to the other build
/// rather than giving up: on Windows a console window behind the application
/// is a nuisance, while a shortcut to a missing file is broken. Returns `None`
/// only when neither build is installed, which is what `--no-launcher`
/// produces and is the case that must not create an entry at all.
fn resolve_entry_target(
    entry: &crate::integration::Entry,
    paths: &xpack_core::InstallPaths,
) -> Option<PathBuf> {
    [entry.target.clone(), paths.launcher_file(), paths.gui_launcher_file()]
        .into_iter()
        .find(|candidate| candidate.is_file())
}

/// Rebuilds the desktop entry an installation would have created.
///
/// Read from the active version's own manifest, because that is where the
/// display name the entry is named after lives.
///
/// # Why a failure here is silent
///
/// An installation with no active version, or whose manifest has already been
/// removed by a partial earlier uninstall, has nothing to rebuild from. There
/// is then no way to know what the entry was called, and guessing would risk
/// deleting a different application's shortcut. Reporting it as nothing to do
/// and leaving the entry is the safe failure.
fn desktop_entry_for_removal(lock: &InstallLock) -> Option<crate::integration::Entry> {
    let paths = lock.paths();
    let id = paths.application_id()?;
    let state = lock.load_or_new_state(id).ok()?;
    let version = state.current_version.clone().or_else(|| state.previous_version.clone())?;

    let bytes = std::fs::read(paths.version_manifest_file(&version)).ok()?;
    let manifest = xpack_core::Manifest::from_slice(&bytes).ok()?;
    crate::integration::Entry::from_manifest(&manifest, paths)
}

/// Lists a directory's entries, treating an unreadable directory as empty.
///
/// Only ever used to explain why a removal stopped, so a failure here must not
/// turn a mostly successful uninstall into an error.
fn entries_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read.filter_map(|entry| entry.ok().map(|entry| entry.path())).collect()
}
