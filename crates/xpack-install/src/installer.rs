//! Installing, activating, rolling back and removing versions.

use crate::integration::command::CommandRoots;
use std::path::{Path, PathBuf};

use xpack_core::atomic;
use xpack_core::hooks::{HookPoint, HookRecord, Moment, Operation};
use xpack_core::progress::{NoProgress, ProgressEvent, ProgressReporter};
use xpack_core::state::{UpdatePhase, VersionStatus};
use xpack_core::{Error, InstallState, Platform, Result, Version};
use xpack_package::VerifiedPackage;
use xpack_platform::InstallLock;

use crate::recovery::{self, RecoveryReport};
use crate::runtime::Program;
use xpack_core::RuntimeChange;

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
    /// Update notifier to place in the installation root, if any.
    ///
    /// Supplied like every other binary, and placed only when the package
    /// being installed asks for a prompt (it declares `update.notify` and
    /// checks while it runs, so that a prompt has a moment to appear from) or
    /// has hooks, whose start the dialog explains while they run; and it
    /// targets a platform with a dialog implemented. An installation that
    /// updates silently carries no dialog code at all, which is the point —
    /// the only graphical binary xPack has should exist only where somebody
    /// asked for a graphical thing to happen.
    pub notifier: Option<PathBuf>,
    /// Windowed launcher to place beside the console one, if any.
    ///
    /// Only meaningful on Windows, where a console build opened from a
    /// shortcut shows an empty console window behind the application. The
    /// caller decides whether to supply one; this crate does not consult the
    /// host platform, so a cross-platform bootstrap installer stays in charge
    /// of what it ships.
    pub gui_launcher: Option<PathBuf>,
    /// Whether the user wants the desktop entry the package asks for.
    ///
    /// `None` leaves it to the manifest, or to a choice recorded earlier,
    /// which is what every update does. `Some(true)` is the same thing said
    /// out loud: it can never add an entry the package did not ask for.
    ///
    /// `Some(false)` declines the entry and records that, so every later
    /// update honours it. It is accepted only on a **first** installation.
    /// An existing installation keeps the updater it was first installed
    /// with, which may predate the record and would put the entry back at
    /// its next update; a choice that is silently undone is worse than one
    /// that is refused.
    pub desktop_entry: Option<bool>,
    /// Whether to put a shortcut on the user's desktop, beside the menu entry.
    ///
    /// Honoured on a **first** installation that writes a menu entry, and
    /// ignored otherwise: an installation made earlier may have an
    /// uninstaller that does not know desktop shortcuts, and one it cannot
    /// remove is worse than none. Every later install refreshes a shortcut
    /// made this way while it is still on the desktop.
    pub desktop_shortcut: bool,
    /// Whether the user wants the command the package asks for.
    ///
    /// The same rules as [`desktop_entry`](Self::desktop_entry): `None`
    /// leaves it to the package and any earlier choice, and `Some(false)`
    /// declines it, on a first installation only.
    pub command: Option<bool>,
    /// Where the command goes, when the package names one.
    ///
    /// `None` uses the current user's `~/.local/bin` and `PATH`, which every
    /// real installation wants. Tests supply their own, for the reason given
    /// on [`desktop_roots`](Self::desktop_roots).
    pub command_roots: Option<crate::integration::command::CommandRoots>,
    /// Who the installation is for: this user, or every user of the machine.
    ///
    /// Settled by the first install and recorded; a later one with the other
    /// scope is refused, because the two live in different places and are
    /// looked after differently. A machine-wide installation is made by a
    /// process with administrator rights, which the caller is responsible for
    /// being, having checked the directory with
    /// [`crate::integration::machine::ensure_safe_root`].
    pub scope: xpack_core::InstallScope,
    /// The key the package was sealed with, when it was: kept in the
    /// installation so its background updates, sealed the same way, can be
    /// opened. Not kept in an installation for everyone, which has none.
    pub seal_key: Option<std::sync::Arc<xpack_security::seal::SealKey>>,
    /// The program that runs hooks, `xpack-hook`, to place in the
    /// installation and to run this install's hooks with.
    ///
    /// Placed in every installation, whether or not the package has hooks,
    /// so that one which has none now can receive them in an update. Its
    /// hooks run with this one, falling back to the installation's own: the
    /// first of them run before any program has been placed.
    pub hook_engine: Option<PathBuf>,
    /// Set by someone else to cancel the install while a hook runs: the hook
    /// is stopped with everything it started, has failed, and the install is
    /// undone as for any failed hook.
    pub cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

/// What installing a launcher did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherOutcome {
    /// The binary was written into the installation root.
    Installed,
    /// A launcher was already there and was left untouched.
    AlreadyPresent,
    /// One was there, from an older xPack release, and was replaced with
    /// this release's, together with the rest of xPack's programs.
    Replaced,
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
    /// What happened to the update notifier.
    ///
    /// `None` when none was supplied, and also when one was supplied and the
    /// package did not ask for a prompt.
    pub notifier: Option<LauncherOutcome>,
    /// What happened to the program that runs hooks, if one was supplied.
    pub hook_engine: Option<LauncherOutcome>,
    /// What happened to the desktop entry the manifest asked for.
    pub desktop: crate::integration::Outcome,
    /// What happened to the shortcut on the user's desktop.
    pub desktop_shortcut: crate::integration::Outcome,
    /// What happened to the command the manifest asked for.
    pub command: crate::integration::Outcome,
    /// What recovery cleaned up beforehand.
    pub recovery: RecoveryReport,
}

/// The verified manifest an installed version came with.
fn read_manifest_of(
    paths: &xpack_core::InstallPaths,
    version: &Version,
) -> Result<xpack_core::Manifest> {
    let file = paths.version_manifest_file(version);
    let bytes = std::fs::read(&file).map_err(|e| Error::io(&file, e))?;
    xpack_core::Manifest::from_slice(&bytes)
}

/// The hook point of `operation` at `moment`.
const fn point(operation: Operation, moment: Moment) -> HookPoint {
    HookPoint { operation, moment }
}

/// What a package's hooks run with, in an operation of this crate: which
/// program, for whom, where their progress and output go, and what cancels.
#[derive(Clone, Copy)]
pub struct HookContext<'a> {
    /// The `xpack-hook` program. None runs no hook: a point with hooks then
    /// fails, as an installation that cannot run them must not pretend to.
    pub engine: Option<&'a Path>,
    /// Who the installation is for.
    pub scope: xpack_core::InstallScope,
    /// Told when a point's hooks start, and given each line they write.
    pub progress: &'a dyn ProgressReporter,
    /// Set by someone else to stop the running hook; it has then failed.
    pub cancel: Option<&'a std::sync::atomic::AtomicBool>,
}

impl<'a> HookContext<'a> {
    /// What an install's hooks run with: `engine`, and `options`' scope and
    /// cancel.
    fn of(
        engine: Option<&'a Path>,
        options: &'a InstallOptions,
        progress: &'a dyn ProgressReporter,
    ) -> Self {
        Self { engine, scope: options.scope, progress, cancel: options.cancel.as_deref() }
    }

    /// Runs `manifest`'s hooks at `point`, its scripts found in `scripts`.
    #[allow(clippy::too_many_arguments)]
    fn run(
        &self,
        paths: &xpack_core::InstallPaths,
        manifest: &xpack_core::Manifest,
        scripts: &Path,
        point: HookPoint,
        from_version: Option<&Version>,
        to_version: Option<&Version>,
        cause: Option<&str>,
    ) -> Result<()> {
        if manifest.hooks.at(point).next().is_none() {
            return Ok(());
        }
        let engine = self.engine.ok_or_else(|| {
            Error::invalid(
                "hooks",
                format!(
                    "{} {} has {point} hooks, and there is no program to run them with",
                    manifest.application.name, manifest.application.version
                ),
            )
        })?;
        self.progress.report(&ProgressEvent::RunningHooks { point });
        let progress = self.progress;
        let sink = move |point: HookPoint, line: &str| {
            progress.report(&ProgressEvent::HookOutput { point, line: line.to_string() });
        };
        crate::hooks::HookRun {
            engine,
            scope: self.scope,
            manifest,
            scripts,
            from_version,
            to_version,
            cause,
            on_line: Some(&sink),
            cancel: self.cancel,
        }
        .run(paths, point)
    }

    /// As [`Self::run`], for the points whose failure changes nothing: it is
    /// logged, and the operation carries on.
    #[allow(clippy::too_many_arguments)]
    fn run_and_carry_on(
        &self,
        paths: &xpack_core::InstallPaths,
        manifest: &xpack_core::Manifest,
        scripts: &Path,
        point: HookPoint,
        from_version: Option<&Version>,
        to_version: Option<&Version>,
        cause: Option<&str>,
    ) {
        if let Err(error) =
            self.run(paths, manifest, scripts, point, from_version, to_version, cause)
        {
            tracing::warn!(%point, %error, "a hook failed; carrying on, as for every {point} hook");
        }
    }
}

/// A version just committed after a good start, whose `update.confirmed`
/// hooks are still to run; see [`Installer::commit_for_confirmation`].
#[derive(Debug, Clone)]
pub struct Confirmation {
    manifest: xpack_core::Manifest,
    version: Version,
    previous: Option<Version>,
}

impl Confirmation {
    /// Runs the hooks, without the installation lock: the hook record is
    /// guarded by the hook lock alone. Their failure is logged; the version
    /// stays, as it already works.
    pub fn run(&self, paths: &xpack_core::InstallPaths, hooks: &HookContext<'_>) {
        hooks.run_and_carry_on(
            paths,
            &self.manifest,
            &paths.version_dir(&self.version),
            point(Operation::Update, Moment::Confirmed),
            self.previous.as_ref(),
            Some(&self.version),
            None,
        );
    }
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

/// Room kept free beyond what a version's files take: the installation's own
/// state and log writes, and the rest of the user's machine, need some.
pub const SPACE_MARGIN: u64 = 32 * 1024 * 1024;

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
    ///
    /// # Hooks
    ///
    /// A package's install hooks run on a first installation, its update
    /// hooks when it is applied over another version; see
    /// [`InstallOptions::hook_engine`]. A failed hook is
    /// [`Error::HookFailed`](xpack_core::Error::HookFailed). On a first
    /// installation everything the install wrote has then been undone but the
    /// lock and its directory: release the lock, then call
    /// [`finish_removal`], which leaves nothing. It is safe to call after
    /// any failed install: it removes nothing of an installation that has
    /// versions.
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
        self.ensure_same_platform(&manifest)?;

        Self::ensure_version_fits(paths, &manifest)?;

        let mut state = self.load_state()?;
        // Asked before a version is recorded, which is what makes the next
        // install not the first.
        let first_installation = state.versions.is_empty();

        if !first_installation {
            self.refuse_unfinished_first_install(&state, &manifest, options)?;
        }

        // Before anything is written, so a refusal leaves no trace.
        Self::ensure_same_scope(&state, first_installation, options.scope)?;
        state.scope = options.scope;
        // Before anything is written: which of xPack's programs this install
        // replaces, whether anything stops it, and whether a launcher it
        // leaves in place can read the package.
        let (release, replacing, _nothing_runs) =
            self.prepare_programs(options, &manifest, &state)?;
        Self::apply_choices(&state, paths, options)?;

        // The command the version being replaced put in place, so one this
        // package renames or drops is taken away rather than left behind.
        let previous_commands = previous_commands(paths, &state);

        // Named after the application from here on, but only for an
        // installation that has no executables yet.
        //
        // The alternative — deriving the names whenever they are needed —
        // renames files that already exist the first time a publisher renames
        // their application, which orphans the launcher a shortcut points at
        // and, on Windows, cannot be done at all while that launcher is the
        // resident process watching the application start.
        //
        // Presence on disk is the test rather than the state document,
        // because the files are the thing that cannot be renamed. An
        // installation made before executables carried application names
        // keeps xPack's names for the rest of its life.
        if state.binary_base_name.is_none() && !Self::has_xpack_named_binaries(paths) {
            state.pin_binary_names(&manifest.application.name);
        }

        let is_repair = self.check_repair(&mut state, &version, options.allow_downgrade)?;

        // Whether this install runs the package's hooks: the install moments
        // on a first installation, the update moments when it applies a
        // version over another. Never on a repair, whose moments are past,
        // nor when a version is only staged: whatever activates it runs them.
        let has_hooks = !manifest.hooks.is_empty();
        let install_hooks = first_installation && has_hooks;
        let update_hooks = !first_installation && !is_repair && options.activate && has_hooks;
        // Before anything is written, like every other refusal.
        self.ensure_hooks_can_run(
            options,
            &manifest,
            &state,
            &replacing,
            install_hooks || update_hooks,
        )?;

        // After every refusal and before the new version is written: a
        // refused install changes nothing, and the format check above trusts
        // the new launcher to read the package, so the version must never be
        // installed if that launcher did not arrive. Newer programs left by
        // an install that fails later read every older format.
        self.replace_programs(&replacing, &release, &mut state)?;

        // Extraction happens in staging and is promoted only once every hash
        // has matched, so a crash can never leave a half-written tree under
        // `versions/` that looks complete.
        state.update = UpdatePhase::Installing { version: version.clone() };
        self.lock.save_state(&state)?;

        let staging = paths.staging_dir(&version);
        package.materialise(&staging, progress)?;
        write_version_metadata(&staging, package)?;

        let engine = self.hook_engine(options);
        let hooks = HookContext::of(engine.as_deref(), options, progress);
        let dir = paths.version_dir(&version);

        // Parsed whenever the version has hooks, staged only too: the
        // background updater refuses a version whose script cannot run when
        // it downloads it, rather than at the start that would apply it.
        if has_hooks
            && !is_repair
            && let Err(error) = self.ready_to_promote(&hooks, &manifest, &staging, install_hooks)
        {
            let _ = atomic::remove_dir_all_if_exists(&staging);
            if first_installation {
                self.undo_first_install(None, &[], options);
            }
            return Err(error);
        }

        self.promote(&staging, &version, &state)?;

        self.record_staged(&mut state, &manifest)?;
        tracing::info!(%version, "version installed");

        if install_hooks {
            let at = point(Operation::Install, Moment::AfterFiles);
            self.run_or_undo(&hooks, &manifest, &dir, at, None, false, options)?;
        }

        // Before activation, so that a version becoming current always has an
        // entry point by the time anything could try to start it.
        let placed = self.place_binaries(options, &manifest, &replacing, &release)?;
        if let Some(key) = &options.seal_key
            && options.scope == xpack_core::InstallScope::User
        {
            keep_seal_key(paths, key)?;
        }

        let (desktop, written, desktop_shortcut, command) =
            self.write_entries(&manifest, options, first_installation, &previous_commands);

        let activated =
            self.activate_installed(&hooks, &manifest, options, is_repair, update_hooks)?;

        // After activation, so it finds the installation complete; before the
        // tree is restricted, so what it writes is restricted too.
        if install_hooks {
            let wrote = matches!(command, crate::integration::Outcome::Done(_));
            let (at, entry) = (point(Operation::Install, Moment::After), written.as_ref());
            self.run_or_undo(&hooks, &manifest, &dir, at, entry, wrote, options)?;
        }

        if options.scope == xpack_core::InstallScope::Machine {
            restrict_tree(paths.root())?;
        }

        Ok(Installed {
            version,
            activated,
            launcher: placed.launcher,
            gui_launcher: placed.gui_launcher,
            updater: placed.updater,
            uninstaller: placed.uninstaller,
            notifier: placed.notifier,
            hook_engine: placed.hook_engine,
            desktop,
            desktop_shortcut,
            command,
            recovery: report,
        })
    }

    /// Whether this install repairs a version whose files are missing:
    /// recorded in state, but not on disk. A version installed and whole is
    /// refused; the downgrade rule applies to anything that is not a repair.
    fn check_repair(
        &self,
        state: &mut InstallState,
        version: &Version,
        allow_downgrade: bool,
    ) -> Result<bool> {
        // Checked before the downgrade rule so reinstalling the active version
        // reports what is actually wrong, rather than "downgrades are
        // rejected", which describes a different problem entirely.
        let is_repair = if state.record(version).is_some() {
            if self.is_usable(version) {
                return Err(Error::invalid(
                    "install",
                    format!("version {version} is already installed"),
                ));
            }
            // State records it but the files are gone. Dropping the record
            // lets the install proceed as a repair.
            tracing::warn!(%version, "reinstalling a version whose files are missing");
            state.versions.remove(&version.to_string());
            self.lock.save_state(state)?;
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
            state.ensure_not_downgrade(version, allow_downgrade)?;
        }

        Ok(is_repair)
    }

    /// Records the version just promoted as staged, and raises the required
    /// version if its release is mandatory.
    fn record_staged(
        &self,
        state: &mut InstallState,
        manifest: &xpack_core::Manifest,
    ) -> Result<()> {
        let version = &manifest.application.version;
        state.stage_version(version, None);

        // Read from the manifest that was just verified, never from the update
        // index: a server that could declare releases mandatory could stop an
        // application starting whenever it liked. Raised only, so an older
        // release cannot lower a requirement a newer one set.
        if manifest.update.mandatory
            && state.required_version.as_ref().is_none_or(|current| current < version)
        {
            tracing::info!(%version, "this release is mandatory; older versions will not start");
            state.required_version = Some(version.clone());
        }

        state.update = UpdatePhase::Staged { version: version.clone() };
        self.lock.save_state(state)?;

        Ok(())
    }

    /// Writes what the package asks for outside the installation: its menu
    /// entry, the desktop shortcut and its commands. Never fails the install:
    /// see the integration module for why.
    fn write_entries(
        &self,
        manifest: &xpack_core::Manifest,
        options: &InstallOptions,
        first_installation: bool,
        previous_commands: &[crate::integration::command::Command],
    ) -> (
        crate::integration::Outcome,
        Option<crate::integration::Entry>,
        crate::integration::Outcome,
        crate::integration::Outcome,
    ) {
        let version = &manifest.application.version;
        // After the binaries, because the entry points at one of them, and a
        // shortcut to a launcher that is not there yet would be broken for as
        // long as the window between the two writes lasted.
        //
        // Never fails the install: see the integration module for why.
        let (desktop, written) =
            self.update_desktop_entry(manifest, version, options.desktop_roots.as_ref());
        let desktop_shortcut = self.update_desktop_shortcut(
            written.as_ref(),
            first_installation && options.desktop_shortcut,
            options.desktop_roots.as_ref(),
        );
        // After the binaries too: the command runs the launcher.
        let command =
            self.update_commands(manifest, previous_commands, options.command_roots.as_ref());

        (desktop, written, desktop_shortcut, command)
    }

    /// Makes the installed version active, as `options` ask: with its update
    /// hooks when it is applied over another, and committed at once in an
    /// installation for everyone. Returns whether it was made active.
    fn activate_installed(
        &self,
        hooks: &HookContext<'_>,
        manifest: &xpack_core::Manifest,
        options: &InstallOptions,
        is_repair: bool,
        update_hooks: bool,
    ) -> Result<bool> {
        let version = &manifest.application.version;
        if update_hooks {
            self.apply_with_hooks(hooks, manifest, version, options.allow_downgrade)?;
            return Ok(true);
        }
        if !options.activate {
            return Ok(false);
        }
        hooks.progress.report(&ProgressEvent::Activating { version: version.clone() });
        // Re-activating the version that is already current is a no-op for
        // the downgrade rule; a repair must be allowed to restore it.
        let already_current = self.load_state()?.current_version.as_ref() == Some(version);
        self.activate(version, options.allow_downgrade || (is_repair && already_current))?;
        // Nothing can watch a machine-wide version start on its behalf: the
        // launcher runs as a user, who cannot write here. So it is committed
        // now, and the version before it is kept for an administrator to
        // roll back to.
        if options.scope == xpack_core::InstallScope::Machine {
            self.commit_health()?;
        }
        Ok(true)
    }

    /// Refuses, and undoes, a first installation cut short while its hooks
    /// ran (a power cut, a killed installer): its version's install moments
    /// never all succeeded, and none of them will run again. Undone as a
    /// failed install is, so the next run is a new installation.
    fn refuse_unfinished_first_install(
        &self,
        state: &InstallState,
        manifest: &xpack_core::Manifest,
        options: &InstallOptions,
    ) -> Result<()> {
        if !self.unfinished_first_install(state)? {
            return Ok(());
        }
        let name = &manifest.application.name;
        // Only an installer undoes it. A run that only stages a version (the
        // background updater) may be beside the application running: it
        // refuses, and changes nothing.
        if !options.activate {
            return Err(Error::invalid(
                "installation",
                format!("the installation of {name} did not finish; run its installer again"),
            ));
        }
        let entry = desktop_entry_for_removal(self.lock);
        let commands = commands_for_removal(self.lock);
        self.undo_first_install(entry.as_ref(), &commands, options);
        Err(Error::invalid(
            "installation",
            format!(
                "an earlier installation of {name} did not finish, and has been removed; run the \
                 installer again"
            ),
        ))
    }

    /// Runs a first installation's hooks at `at`, and undoes the
    /// installation if they fail: with the desktop `entry` and the commands
    /// it wrote, when it has written them.
    #[allow(clippy::too_many_arguments)]
    fn run_or_undo(
        &self,
        hooks: &HookContext<'_>,
        manifest: &xpack_core::Manifest,
        scripts: &Path,
        at: HookPoint,
        entry: Option<&crate::integration::Entry>,
        commands_written: bool,
        options: &InstallOptions,
    ) -> Result<()> {
        let version = &manifest.application.version;
        let Err(error) =
            hooks.run(self.lock.paths(), manifest, scripts, at, None, Some(version), None)
        else {
            return Ok(());
        };
        let commands = if commands_written {
            crate::integration::command::Command::all_from_manifest(
                manifest,
                self.lock.paths(),
                &self.binary_names(),
            )
        } else {
            Vec::new()
        };
        self.undo_first_install(entry, &commands, options);
        Err(error)
    }

    /// Parses every script of `manifest`, in `staging`, before any of them
    /// runs, so one that cannot run is refused with nothing done; then, on
    /// a first installation, runs `install.before` from staging, before the
    /// version is in `versions/`: what it sees of the installation is what
    /// was there.
    fn ready_to_promote(
        &self,
        hooks: &HookContext<'_>,
        manifest: &xpack_core::Manifest,
        staging: &Path,
        install_hooks: bool,
    ) -> Result<()> {
        let engine =
            hooks.engine.ok_or_else(|| Error::invalid("hooks", "no program to run them with"))?;
        crate::hooks::check_scripts(engine, manifest, staging)?;
        if install_hooks {
            let version = &manifest.application.version;
            hooks.run(
                self.lock.paths(),
                manifest,
                staging,
                point(Operation::Install, Moment::Before),
                None,
                Some(version),
                None,
            )?;
        }
        Ok(())
    }

    /// The `xpack-hook` this install runs hooks with: the one supplied, or the
    /// installation's own.
    fn hook_engine(&self, options: &InstallOptions) -> Option<PathBuf> {
        options.hook_engine.clone().filter(|engine| engine.is_file()).or_else(|| {
            Some(self.lock.paths().hook_engine_file()).filter(|engine| engine.is_file())
        })
    }

    /// Refuses, before anything is written, a package whose hooks could not
    /// run: now, for want of a program to run them with, or later, because
    /// the `xpack-hook` the installation is left with serves too old a hook
    /// interface. The launcher and the uninstaller run that one.
    fn ensure_hooks_can_run(
        &self,
        options: &InstallOptions,
        manifest: &xpack_core::Manifest,
        state: &InstallState,
        replacing: &[Replacement],
        run_now: bool,
    ) -> Result<()> {
        if manifest.hooks.is_empty() {
            return Ok(());
        }
        let installed = self.lock.paths().hook_engine_file().is_file();
        let supplied = options.hook_engine.as_ref().is_some_and(|engine| engine.is_file());
        let replaced = replacing.iter().any(|r| r.slot == Slot::HookEngine);
        let left_with = if supplied && (!installed || replaced) {
            xpack_core::hooks::HOOK_INTERFACE
        } else if installed {
            state.hook_interface.unwrap_or(0)
        } else {
            0
        };
        let name = &manifest.application.name;
        let version = &manifest.application.version;
        if run_now && !supplied && !installed {
            return Err(Error::invalid(
                "hooks",
                format!(
                    "{name} {version} has hooks, and this installation has no program to run \
                     them. Run the newest installer of {name}."
                ),
            ));
        }
        if left_with < xpack_core::hooks::HOOK_INTERFACE {
            return Err(Error::invalid(
                "hooks",
                format!(
                    "{name} {version} has hooks this installation's xPack cannot run. Run the \
                     newest installer of {name}, which brings one that can."
                ),
            ));
        }
        Ok(())
    }

    /// Applies `version`, already installed, over the active one, with its
    /// update hooks: `update.before`, activation, `update.after`, and for an
    /// installation for everyone `update.confirmed` at once, since nothing
    /// can watch its start on an administrator's behalf.
    ///
    /// A failed `update.before` leaves the active version as it was and
    /// marks this one bad; a failed `update.after` rolls it back. Either
    /// runs its rollback hooks, and the failure is returned.
    ///
    /// What the launcher does with a version the background updater staged,
    /// and an installer with one it applies over another.
    pub fn apply_with_hooks(
        &self,
        hooks: &HookContext<'_>,
        manifest: &xpack_core::Manifest,
        version: &Version,
        allow_downgrade: bool,
    ) -> Result<()> {
        let paths = self.lock.paths();
        let scripts = paths.version_dir(version);
        let previous = self.load_state()?.current_version;
        // A cancel stops the update hook; it must not stop the rollback hooks
        // that undo it, which run once and would then never run at all.
        let undo = HookContext { cancel: None, ..*hooks };

        if let Err(error) = hooks.run(
            self.lock.paths(),
            manifest,
            &scripts,
            point(Operation::Update, Moment::Before),
            previous.as_ref(),
            Some(version),
            None,
        ) {
            self.abandon_update(&undo, manifest, version, previous.as_ref(), &error)?;
            return Err(error);
        }

        hooks.progress.report(&ProgressEvent::Activating { version: version.clone() });
        self.activate(version, allow_downgrade)?;

        if let Err(error) = hooks.run(
            self.lock.paths(),
            manifest,
            &scripts,
            point(Operation::Update, Moment::After),
            previous.as_ref(),
            Some(version),
            None,
        ) {
            self.roll_back_with_hooks(
                &undo,
                "hookFailed",
                &format!("its update.after hook failed: {error}"),
            )?;
            return Err(error);
        }

        if hooks.scope == xpack_core::InstallScope::Machine {
            self.commit_health()?;
            hooks.run_and_carry_on(
                self.lock.paths(),
                manifest,
                &scripts,
                point(Operation::Update, Moment::Confirmed),
                previous.as_ref(),
                Some(version),
                None,
            );
        }
        Ok(())
    }

    /// Finishes applying the active version, on probation, if its
    /// `update.after` hooks never finished: the machine stopped between its
    /// activation and their end. Run at each start on probation; once they
    /// have succeeded it does nothing.
    ///
    /// A run of them cut short is a failure, as every hook cut short is: the
    /// version is rolled back with its rollback hooks, and the failure is
    /// returned. One that never started runs now.
    pub fn finish_update_hooks(&self, hooks: &HookContext<'_>) -> Result<()> {
        let state = self.load_state()?;
        let UpdatePhase::PendingVerification { version, rollback_to, .. } = &state.update else {
            return Ok(());
        };
        let paths = self.lock.paths();
        let Some(manifest) = std::fs::read(paths.version_manifest_file(version))
            .ok()
            .and_then(|bytes| xpack_core::Manifest::from_slice(&bytes).ok())
        else {
            return Ok(());
        };
        let scripts = paths.version_dir(version);
        let at = point(Operation::Update, Moment::After);
        let Err(error) = hooks.run(
            self.lock.paths(),
            &manifest,
            &scripts,
            at,
            Some(rollback_to),
            Some(version),
            None,
        ) else {
            return Ok(());
        };
        let undo = HookContext { cancel: None, ..*hooks };
        self.roll_back_with_hooks(
            &undo,
            "hookFailed",
            &format!("its update.after hook failed: {error}"),
        )?;
        Err(error)
    }

    /// Commits the active version, on probation, as having started well,
    /// and returns what runs its `update.confirmed` hooks: once, after the
    /// start that succeeds, never after a failed one.
    ///
    /// Run them with [`Confirmation::run`] once the installation lock is let
    /// go: they run beside the application, for as long as they take, and a
    /// second start of it must not wait on them.
    pub fn commit_for_confirmation(&self) -> Result<Option<Confirmation>> {
        let before = self.load_state()?;
        let (version, previous) = match &before.update {
            UpdatePhase::PendingVerification { version, rollback_to, .. } => {
                (version.clone(), Some(rollback_to.clone()))
            }
            _ => (before.active()?.clone(), before.previous_version.clone()),
        };
        self.commit_health()?;
        let paths = self.lock.paths();
        let manifest = std::fs::read(paths.version_manifest_file(&version))
            .ok()
            .and_then(|bytes| xpack_core::Manifest::from_slice(&bytes).ok());
        Ok(manifest.map(|manifest| Confirmation { manifest, version, previous }))
    }

    /// Gives up `version` after its `update.before` hook failed, before it
    /// was ever active: it is marked bad, and its rollback hooks undo what
    /// its update hooks did.
    fn abandon_update(
        &self,
        hooks: &HookContext<'_>,
        manifest: &xpack_core::Manifest,
        version: &Version,
        active: Option<&Version>,
        error: &Error,
    ) -> Result<()> {
        let scripts = self.lock.paths().version_dir(version);
        let cause = Some("hookFailed");
        hooks.run_and_carry_on(
            self.lock.paths(),
            manifest,
            &scripts,
            point(Operation::Rollback, Moment::Before),
            Some(version),
            active,
            cause,
        );
        let mut state = self.load_state()?;
        state.mark_bad(version, format!("its update.before hook failed: {error}"));
        if state.update == (UpdatePhase::Staged { version: version.clone() }) {
            state.update = UpdatePhase::Idle;
        }
        self.lock.save_state(&state)?;
        hooks.run_and_carry_on(
            self.lock.paths(),
            manifest,
            &scripts,
            point(Operation::Rollback, Moment::After),
            Some(version),
            active,
            cause,
        );
        Ok(())
    }

    /// Undoes the active version because someone asked (`xpack rollback`):
    /// back to the newest earlier version that has proved it starts, with
    /// the undone version's rollback hooks around the switch, cause
    /// `requested`. Not marked bad: it can be activated again, and then runs
    /// none of its hooks a second time. Returns the version now active, or
    /// None when there is none to go back to, and nothing changed.
    pub fn roll_back_on_request(&self, hooks: &HookContext<'_>) -> Result<Option<Version>> {
        let state = self.load_state()?;
        let undone = state.active()?.clone();
        let Some(target) = state.best_rollback_target(&undone) else {
            return Ok(None);
        };
        if !self.is_usable(&target) {
            return Err(Error::invalid(
                "rollback",
                format!("{target} is recorded but its files are missing"),
            ));
        }
        self.leave_with_hooks(hooks, &undone, &target, "requested", || {
            let mut state = self.load_state()?;
            state.previous_version = Some(undone.clone());
            state.current_version = Some(target.clone());
            state.update = UpdatePhase::Idle;
            self.lock.save_state(&state)?;
            xpack_platform::update_current_link(self.lock.paths(), &target).log();
            tracing::info!(from = %undone, to = %target, "rolled back on request");
            Ok(())
        })?;
        Ok(Some(target))
    }

    /// Makes `version` active as `xpack activate` asks, with the hooks that
    /// move means: a newer version is applied with its update hooks, as an
    /// update is; an older one undoes the active version by request, with
    /// its rollback hooks, cause `requested`. The same version, or a first
    /// activation, runs none.
    pub fn activate_with_hooks(
        &self,
        hooks: &HookContext<'_>,
        version: &Version,
        allow_downgrade: bool,
    ) -> Result<()> {
        let Some(active) = self.load_state()?.current_version else {
            return self.activate(version, allow_downgrade);
        };
        if version > &active {
            let manifest = read_manifest_of(self.lock.paths(), version)?;
            return self.apply_with_hooks(hooks, &manifest, version, allow_downgrade);
        }
        if version < &active {
            return self.leave_with_hooks(hooks, &active, version, "requested", || {
                self.activate(version, allow_downgrade)
            });
        }
        self.activate(version, allow_downgrade)
    }

    /// Leaves `undone` for `target` by `switch`, with `undone`'s rollback
    /// hooks around it with `cause`, if its update hooks had started. A
    /// failed rollback hook is logged, and the move carries on.
    fn leave_with_hooks(
        &self,
        hooks: &HookContext<'_>,
        undone: &Version,
        target: &Version,
        cause: &str,
        switch: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let paths = self.lock.paths();
        let manifest = read_manifest_of(paths, undone).ok();
        let started = HookRecord::read(&paths.hook_record_file())
            .is_ok_and(|record| record.any_started(undone, Operation::Update));
        let scripts = paths.version_dir(undone);
        let run = |moment| {
            if let (Some(manifest), true) = (&manifest, started) {
                hooks.run_and_carry_on(
                    paths,
                    manifest,
                    &scripts,
                    point(Operation::Rollback, moment),
                    Some(undone),
                    Some(target),
                    Some(cause),
                );
            }
        };
        run(Moment::Before);
        switch()?;
        run(Moment::After);
        Ok(())
    }

    /// Rolls the active version back, marked bad for `reason`, running its
    /// rollback hooks around it with `cause`, if its update hooks had
    /// started: a version with none has nothing of its own to undo.
    ///
    /// What the launcher does when a version fails to start or its update
    /// hook fails, and what an installer does when the version it applied
    /// fails its `update.after` hook. A failed rollback hook is logged, and
    /// the rollback carries on. Returns the version now active, as
    /// [`Self::record_failure`] does.
    pub fn roll_back_with_hooks(
        &self,
        hooks: &HookContext<'_>,
        cause: &str,
        reason: &str,
    ) -> Result<Option<Version>> {
        let paths = self.lock.paths();
        let state = self.load_state()?;
        let undone = state.active()?.clone();
        let target = state.best_rollback_target(&undone);
        let manifest = std::fs::read(paths.version_manifest_file(&undone))
            .ok()
            .and_then(|bytes| xpack_core::Manifest::from_slice(&bytes).ok());
        let started = HookRecord::read(&paths.hook_record_file())
            .is_ok_and(|record| record.any_started(&undone, Operation::Update));
        let scripts = paths.version_dir(&undone);
        if let (Some(manifest), true) = (&manifest, started) {
            hooks.run_and_carry_on(
                self.lock.paths(),
                manifest,
                &scripts,
                point(Operation::Rollback, Moment::Before),
                Some(&undone),
                target.as_ref(),
                Some(cause),
            );
        }
        let restored = self.record_failure(reason)?;
        if let (Some(manifest), true) = (&manifest, started) {
            hooks.run_and_carry_on(
                self.lock.paths(),
                manifest,
                &scripts,
                point(Operation::Rollback, Moment::After),
                Some(&undone),
                restored.as_ref(),
                Some(cause),
            );
        }
        if let Some(to) = &restored {
            hooks.progress.report(&ProgressEvent::RolledBack { from: undone, to: to.clone() });
        }
        Ok(restored)
    }

    /// Whether this installation is a first installation cut short while its
    /// install hooks ran: one version, with install hooks, not every one of
    /// whose points succeeded.
    fn unfinished_first_install(&self, state: &InstallState) -> Result<bool> {
        if state.versions.len() != 1 {
            return Ok(false);
        }
        let Some(version) = state.versions.keys().next().and_then(|v| Version::parse(v).ok())
        else {
            return Ok(false);
        };
        let paths = self.lock.paths();
        let Some(manifest) = std::fs::read(paths.version_manifest_file(&version))
            .ok()
            .and_then(|bytes| xpack_core::Manifest::from_slice(&bytes).ok())
        else {
            return Ok(false);
        };
        if manifest.hooks.install.is_empty() {
            return Ok(false);
        }
        let record = HookRecord::read(&paths.hook_record_file())?;
        // Only a version that was first-installed here: a version applied
        // by an update declares the same install hooks and never ran them.
        // A first installation records `install.before` before the version
        // is recorded at all, so one cut short always has a start of its own.
        if !record.any_started(&version, Operation::Install) {
            return Ok(false);
        }
        let unfinished = Operation::Install.moments().iter().any(|moment| {
            let point = HookPoint { operation: Operation::Install, moment: *moment };
            let scripts: Vec<&str> = manifest.hooks.at(point).map(|h| h.script.as_str()).collect();
            !scripts.is_empty() && !record.all_succeeded(&version, point, scripts)
        });
        Ok(unfinished)
    }

    /// Undoes a first installation whose hook failed: everything it wrote,
    /// as an uninstall removes it, and what its hooks may have left in the
    /// installation, its data, run directories and log included. Not the
    /// lock in `state/`, nor the root: the caller does that with
    /// [`finish_removal`] once the lock is released.
    ///
    /// Never runs a hook: this reverses an install, it is not an uninstall.
    /// Never fails: what cannot be removed is logged, and the failure that
    /// caused the undo is what the caller reports.
    fn undo_first_install(
        &self,
        entry: Option<&crate::integration::Entry>,
        commands: &[crate::integration::command::Command],
        options: &InstallOptions,
    ) {
        let paths = self.lock.paths();
        if let Err(error) = remove_contents(
            self.lock,
            entry,
            commands,
            options.desktop_roots.as_ref(),
            options.command_roots.as_ref(),
        ) {
            tracing::error!(%error, "could not undo all of a failed installation");
        }
        for dir in [paths.data_dir(), paths.hook_runs_dir(), paths.logs_dir(), paths.staging_root()]
        {
            if let Err(error) = atomic::remove_dir_all_if_exists(&dir) {
                tracing::error!(%error, dir = %dir.display(), "could not remove");
            }
        }
        // Everything in `state/` but the lock, held until the caller lets go.
        let lock_file = paths.lock_file();
        for path in entries_in(&paths.state_dir()) {
            if path == lock_file {
                continue;
            }
            let removed = if path.is_dir() {
                atomic::remove_dir_all_if_exists(&path)
            } else {
                atomic::remove_file_if_exists(&path)
            };
            if let Err(error) = removed {
                tracing::error!(%error, path = %path.display(), "could not remove");
            }
        }
        tracing::warn!(root = %paths.root().display(), "a failed installation was undone");
    }

    /// Records what the user declined of what the package asks for.
    fn apply_choices(
        state: &InstallState,
        paths: &xpack_core::InstallPaths,
        options: &InstallOptions,
    ) -> Result<()> {
        Self::apply_choice(
            state,
            &paths.desktop_preference_file(),
            options.desktop_entry,
            "desktop entry",
        )?;
        Self::apply_choice(
            state,
            &crate::integration::command::preference_file(paths),
            options.command,
            "command",
        )
    }

    /// Records a declined desktop entry or command, or refuses to.
    ///
    /// See [`InstallOptions::desktop_entry`] for why declining is accepted
    /// only on a first installation.
    fn apply_choice(
        state: &InstallState,
        file: &Path,
        choice: Option<bool>,
        what: &str,
    ) -> Result<()> {
        if choice != Some(false) {
            // A first installation that did not decline clears any record an
            // earlier, failed first attempt left: that person's answer is not
            // this one's.
            if state.versions.is_empty() {
                xpack_core::atomic::remove_file_if_exists(file)?;
            }
            return Ok(());
        }
        if !state.versions.is_empty() {
            return Err(Error::invalid(
                what,
                "can only be declined on a first installation; this installation's updater \
                 may not know the choice and would add it back",
            ));
        }
        crate::integration::record_declined_at(file)
    }

    /// Refuses a version that would install but could not work, while
    /// nothing has been written.
    fn ensure_version_fits(
        paths: &xpack_core::InstallPaths,
        manifest: &xpack_core::Manifest,
    ) -> Result<()> {
        // Checked here rather than at launch. Windows cannot start a program
        // from a path longer than `MAX_PATH`, however well the package
        // extracted, so a version that lands too deep installs cleanly and
        // then cannot be opened. Refusing now reports it while the user is
        // still watching the install they asked for.
        xpack_core::ensure_launch_paths_fit(
            &paths.version_dir(&manifest.application.version),
            &manifest.launch,
            xpack_core::host_launch_path_limit(),
        )?;

        // Against the size the signed manifest declares, which extraction
        // enforces as a hard limit. A disk that runs out part-way is
        // recovered from, but the user finds out late and every other
        // program meanwhile meets a full disk.
        xpack_platform::ensure_space(
            paths.root(),
            manifest.payload.total_size.saturating_add(SPACE_MARGIN),
            "this version",
        )
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

    /// Places the xPack programs the caller supplied.
    ///
    /// A machine-wide installation has no background updates, so no updater
    /// and no update notice go in it.
    fn place_binaries(
        &self,
        options: &InstallOptions,
        manifest: &xpack_core::Manifest,
        replacing: &[Replacement],
        release: &Version,
    ) -> Result<Placed> {
        let shared = options.scope == xpack_core::InstallScope::Machine;
        // Whether any of xPack's programs was here before this install: if one
        // was and is not replaced, it still comes from its own release, and
        // placing others beside it must not record this one.
        let had_programs = self.has_any_program();
        // Each is placed where it is missing and left where it is not; the
        // ones this install replaced already are reported as such.
        let place = |source: &Option<PathBuf>, how: fn(&Self, &Path) -> Result<LauncherOutcome>| {
            source.as_deref().map(|source| how(self, source)).transpose()
        };
        let mut placed = Placed {
            launcher: place(&options.launcher, Self::install_launcher)?,
            gui_launcher: place(&options.gui_launcher, Self::install_gui_launcher)?,
            updater: if shared { None } else { place(&options.updater, Self::install_updater)? },
            uninstaller: place(&options.uninstaller, Self::install_uninstaller)?,
            notifier: if shared || !Self::notifier_is_wanted(manifest) {
                None
            } else {
                place(&options.notifier, Self::install_notifier)?
            },
            // In every installation, for everyone too: a hook of an
            // installation for everyone runs in the installer, as the
            // administrator, with the same program.
            hook_engine: place(&options.hook_engine, Self::install_hook_engine)?,
        };

        if !replacing.is_empty() {
            for replaced in replacing {
                let outcome = Some(LauncherOutcome::Replaced);
                match replaced.slot {
                    Slot::Launcher => placed.launcher = outcome,
                    Slot::GuiLauncher => placed.gui_launcher = outcome,
                    Slot::Updater => placed.updater = outcome,
                    Slot::Uninstaller => placed.uninstaller = outcome,
                    Slot::Notifier => placed.notifier = outcome,
                    Slot::HookEngine => placed.hook_engine = outcome,
                    Slot::CommandCopy => {}
                }
            }
        } else if placed.any_installed() && !had_programs {
            // A first installation: every program here is this release's.
            self.record_runtime(release, false)?;
        }
        Ok(placed)
    }

    /// Decides, before anything is written, which of xPack's programs this
    /// install replaces with its newer ones, and refuses the install when it
    /// cannot: something of a per-user installation is running, or a launcher
    /// it leaves in place cannot read the package.
    ///
    /// The presence lock comes back held alone when programs are replaced in
    /// an installation for one user: no launcher may start until they are,
    /// so the caller keeps it for the rest of the install.
    fn prepare_programs(
        &self,
        options: &InstallOptions,
        manifest: &xpack_core::Manifest,
        state: &InstallState,
    ) -> Result<(Version, Vec<Replacement>, Option<xpack_platform::PresenceLock>)> {
        let paths = self.lock.paths();
        let release = xpack_core::xpack_release()?;
        let replacing = self.programs_to_replace(options, manifest, state, &release);
        let alone = if !replacing.is_empty() && options.scope == xpack_core::InstallScope::User {
            Some(ensure_nothing_runs(paths, &replacing, &manifest.application.name)?)
        } else {
            None
        };
        // A launcher this install replaces does not have to read the package;
        // one it leaves in place does.
        if !replaces_every_launcher(state, paths, &replacing) {
            Self::ensure_launcher_reads(state, paths, manifest)?;
        }
        Ok((release, replacing, alone))
    }

    /// Whether any of xPack's programs is in the installation, under the names
    /// it uses.
    fn has_any_program(&self) -> bool {
        let paths = self.lock.paths();
        let names = self.binary_names();
        [
            paths.launcher_file_named(&names),
            paths.gui_launcher_file_named(&names),
            paths.updater_file_named(&names),
            paths.uninstaller_file_named(&names),
            paths.notifier_file_named(&names),
            paths.hook_engine_file(),
        ]
        .iter()
        .any(|path| path.exists())
    }

    /// Replaces `replacing` with this release's programs, all or none, and
    /// records the release: on disk, and in `state`, the install's own copy,
    /// which it saves again later and would otherwise put the old record
    /// back with. See [`crate::runtime`].
    fn replace_programs(
        &self,
        replacing: &[Replacement],
        release: &Version,
        state: &mut InstallState,
    ) -> Result<()> {
        if replacing.is_empty() {
            return Ok(());
        }
        let from = self.load_state()?.runtime_version;
        let programs: Vec<_> = replacing.iter().map(|r| r.program.clone()).collect();
        let launcher = replacing.iter().any(|r| r.slot == Slot::Launcher);
        let engine = replacing.iter().any(|r| r.slot == Slot::HookEngine);
        crate::runtime::replace(self.lock.paths(), &programs, from, release.clone(), &mut || {
            self.record_runtime(release, launcher)?;
            if engine { self.record_hook_interface() } else { Ok(()) }
        })?;
        state.runtime_version = Some(release.clone());
        if engine {
            state.hook_interface = Some(xpack_core::hooks::HOOK_INTERFACE);
        }
        if launcher {
            state.launcher_format_version =
                Some(xpack_core::manifest::MAX_SUPPORTED_FORMAT_VERSION);
        }
        tracing::info!(%release, "replaced xPack's programs with this release's");
        Ok(())
    }

    /// Records that the installation's programs are this release's, and,
    /// when the launcher was among them, the formats it reads.
    fn record_runtime(&self, release: &Version, launcher: bool) -> Result<()> {
        let mut state = self.load_state()?;
        state.runtime_version = Some(release.clone());
        if launcher {
            state.launcher_format_version =
                Some(xpack_core::manifest::MAX_SUPPORTED_FORMAT_VERSION);
        }
        self.lock.save_state(&state)
    }

    /// The programs already in the installation that this install replaces:
    /// those it supplies a newer release of, where one is already in place.
    ///
    /// None unless the installation's programs come from an older release
    /// (or say nothing of theirs): a newer installation keeps its own, and
    /// the same release has nothing to change. A program not in place is
    /// not replaced but placed, as on a first installation.
    fn programs_to_replace(
        &self,
        options: &InstallOptions,
        manifest: &xpack_core::Manifest,
        state: &InstallState,
        release: &Version,
    ) -> Vec<Replacement> {
        if state.runtime_change(release) != RuntimeChange::Upgrade {
            return Vec::new();
        }
        let paths = self.lock.paths();
        let names = state.binary_names();
        let shared = options.scope == xpack_core::InstallScope::Machine;
        let notifier_wanted = !shared && Self::notifier_is_wanted(manifest);
        let candidates = [
            (Slot::Launcher, &options.launcher, paths.launcher_file_named(&names), true),
            (Slot::GuiLauncher, &options.gui_launcher, paths.gui_launcher_file_named(&names), true),
            (Slot::Updater, &options.updater, paths.updater_file_named(&names), !shared),
            (Slot::Uninstaller, &options.uninstaller, paths.uninstaller_file_named(&names), true),
            (Slot::Notifier, &options.notifier, paths.notifier_file_named(&names), notifier_wanted),
            (Slot::HookEngine, &options.hook_engine, paths.hook_engine_file(), true),
        ];
        let mut replacing: Vec<Replacement> = Vec::new();
        for (slot, source, destination, wanted) in candidates {
            // Where there is one build, the windowed launcher's name is the
            // console one's; the file is replaced once, as the launcher.
            let listed = replacing.iter().any(|r| r.program.destination == destination);
            if let Some(source) = source
                && wanted
                && destination.exists()
                && !listed
            {
                replacing.push(Replacement {
                    slot,
                    program: Program { destination, source: source.clone() },
                });
            }
        }
        // On Windows each command is a copy of the console launcher in the
        // command directory, and runs as one: it is replaced with it.
        if cfg!(windows)
            && let Some(launcher) = &options.launcher
        {
            for command in previous_commands(paths, state) {
                let copy = command.command_dir.join(format!("{}.exe", command.name));
                if copy.exists() {
                    replacing.push(Replacement {
                        slot: Slot::CommandCopy,
                        program: Program { destination: copy, source: launcher.clone() },
                    });
                }
            }
        }
        replacing
    }

    /// Refuses installing for one scope into an installation made for the
    /// other.
    fn ensure_same_scope(
        state: &xpack_core::InstallState,
        first_installation: bool,
        scope: xpack_core::InstallScope,
    ) -> Result<()> {
        if first_installation || state.scope == scope {
            return Ok(());
        }
        Err(Error::invalid(
            "installation",
            format!(
                "this installation is for {}, and this install is for {}; uninstall it first",
                describe_scope(state.scope),
                describe_scope(scope)
            ),
        ))
    }

    /// Who this installation is for, as recorded.
    fn scope(&self) -> xpack_core::InstallScope {
        self.load_state().map_or(xpack_core::InstallScope::User, |state| state.scope)
    }

    /// Refuses a package the launcher already in the installation cannot read,
    /// when this install leaves that launcher in place.
    ///
    /// An update never replaces a launcher, and an installer replaces it only
    /// with a newer release's. A version in a format a launcher left in place
    /// does not know would be installed and made active, and then never
    /// start: the launcher refuses the manifest, and a shortcut
    /// start has nowhere to say so. Refused here instead, before anything is
    /// written, with what to do about it.
    ///
    /// An installation with no launcher yet gets one from this install, which
    /// reads everything this build does.
    fn ensure_launcher_reads(
        state: &xpack_core::InstallState,
        paths: &xpack_core::InstallPaths,
        manifest: &xpack_core::Manifest,
    ) -> Result<()> {
        let names = state.binary_names();
        let placed = paths.launcher_file_named(&names).exists()
            || paths.gui_launcher_file_named(&names).exists();
        let reads = state.launcher_reads();
        let needs = manifest.format_version.0;
        if !placed || needs <= reads {
            return Ok(());
        }
        Err(Error::invalid(
            "installation",
            format!(
                "{} {} is package format {needs}, and the launcher already installed reads \
                 format {reads} at most. This install does not replace it, so this version \
                 would install and then not start. Run the newest installer of {}, which \
                 replaces it.",
                manifest.application.name, manifest.application.version, manifest.application.name
            ),
        ))
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
    ///
    /// A launcher placed now records the newest package format it reads, for
    /// the format check to go by at every later install. One
    /// already there is as old as it was, and its record is left alone.
    pub fn install_launcher(&self, source: &Path) -> Result<LauncherOutcome> {
        let outcome = Self::install_binary(
            source,
            &self.lock.paths().launcher_file_named(&self.binary_names()),
            "launcher",
        )?;
        if outcome == LauncherOutcome::Installed {
            let mut state = self.load_state()?;
            state.launcher_format_version =
                Some(xpack_core::manifest::MAX_SUPPORTED_FORMAT_VERSION);
            self.lock.save_state(&state)?;
        }
        Ok(outcome)
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
    ) -> (crate::integration::Outcome, Option<crate::integration::Entry>) {
        let paths = self.lock.paths();
        let names = self.binary_names();
        let Some(mut entry) = crate::integration::Entry::from_manifest(manifest, paths, &names)
        else {
            return (crate::integration::Outcome::NotRequested, None);
        };

        // The package asked; the user said no, at this install or an earlier
        // one. Reported as not requested, because from the user's side it was
        // not.
        if crate::integration::is_declined(paths) {
            tracing::info!("the user declined a desktop entry; none is written");
            return (crate::integration::Outcome::NotRequested, None);
        }

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
        match resolve_entry_target(&entry, paths, &names) {
            Some(target) => entry.target = target,
            None => {
                return (
                    crate::integration::Outcome::Failed(format!(
                        "{} is not installed, so a desktop entry would point at nothing",
                        entry.target.display()
                    )),
                    None,
                );
            }
        }

        // The icon is copied out of the version directory first, because the
        // entry points at the copy: a version directory is replaced by the
        // next update and an entry pointing into one goes stale.
        entry.icon = crate::integration::place_icon(paths, manifest, version);

        let outcome = match roots.cloned().or_else(|| crate::integration::roots_for(self.scope())) {
            Some(roots) => crate::integration::install_into(&entry, &roots),
            None => crate::integration::Outcome::Failed(
                "could not find where the menu entry goes".to_string(),
            ),
        };
        outcome.log("install");
        let written = outcome.is_done().then_some(entry);
        (outcome, written)
    }

    /// Makes, refreshes or leaves the shortcut on the user's desktop.
    ///
    /// Only beside a menu entry this install wrote, which is what it shows
    /// and starts. `create` is the person installing's choice, and only ever
    /// true on a first installation; otherwise a recorded shortcut is
    /// refreshed, and nothing new is made. Never fails the install, for the
    /// reason the menu entry does not.
    fn update_desktop_shortcut(
        &self,
        entry: Option<&crate::integration::Entry>,
        create: bool,
        roots: Option<&crate::integration::Roots>,
    ) -> crate::integration::Outcome {
        use crate::integration::{Outcome, desktop_shortcut};

        let Some(entry) = entry else {
            return Outcome::NotRequested;
        };
        let Some(roots) = roots.cloned().or_else(|| crate::integration::roots_for(self.scope()))
        else {
            return Outcome::Failed("could not find where the desktop is".to_string());
        };
        let paths = self.lock.paths();
        let outcome = if create {
            desktop_shortcut::create(entry, &roots, paths)
        } else {
            desktop_shortcut::refresh(entry, &roots, paths)
        };
        outcome.log("desktop shortcut");
        outcome
    }

    /// Puts the commands the manifest asks for in place, and takes away any
    /// an earlier version put there that this one renamed or dropped.
    ///
    /// Never fails the install, for the reason the desktop entry does not.
    fn update_commands(
        &self,
        manifest: &xpack_core::Manifest,
        previous: &[crate::integration::command::Command],
        roots: Option<&crate::integration::command::CommandRoots>,
    ) -> crate::integration::Outcome {
        use crate::integration::{Outcome, command};

        let Some(roots) = roots.cloned().or_else(|| command::CommandRoots::for_scope(self.scope()))
        else {
            return Outcome::Failed("could not find where commands go".to_string());
        };
        let paths = self.lock.paths();
        let wanted = command::Command::all_from_manifest(manifest, paths, &self.binary_names());

        let dropped: Vec<command::Command> = previous
            .iter()
            .filter(|old| !wanted.iter().any(|new| new.name == old.name))
            .cloned()
            .collect();
        if !dropped.is_empty() {
            command::remove_all(&dropped, &roots).log("remove the previous commands");
        }

        if wanted.is_empty() {
            return Outcome::NotRequested;
        }
        if crate::integration::is_declined_at(&command::preference_file(paths)) {
            tracing::info!("the user declined the commands; none is put in place");
            return Outcome::NotRequested;
        }
        // The scripts and the Windows copies all run the console launcher; a
        // command that names a missing one would fail every time it is typed.
        if !wanted[0].launcher.is_file() {
            return Outcome::Failed(format!(
                "{} is not installed, so the commands would run nothing",
                wanted[0].launcher.display()
            ));
        }
        // A command every user runs goes only where nobody but an
        // administrator can change it. `/usr/local/bin` often belongs to
        // whoever installed Homebrew, who could then replace what every other
        // user runs.
        // On Windows the command is a copy inside the installation itself.
        if cfg!(unix)
            && roots.scope == xpack_core::InstallScope::Machine
            && let Err(error) = crate::integration::machine::ensure_safe_root(&roots.bin)
        {
            return Outcome::Failed(format!("the command was not added: {error}"));
        }
        let outcome = command::install_all(&wanted, &roots);
        outcome.log("command");
        outcome
    }

    /// Places the windowed launcher in the installation root.
    ///
    /// Same rule as the console build, and it is the build a desktop shortcut
    /// points at, so replacing it would break every shortcut that resolved
    /// while the file was gone.
    pub fn install_gui_launcher(&self, source: &Path) -> Result<LauncherOutcome> {
        Self::install_binary(
            source,
            &self.lock.paths().gui_launcher_file_named(&self.binary_names()),
            "windowed launcher",
        )
    }

    /// Places the background updater in the installation root.
    ///
    /// Same rule as the launcher, and for a sharper version of the same
    /// reason: the updater may well be running right now — the launcher starts
    /// it on every application start — so replacing it is exactly the write
    /// Windows refuses.
    pub fn install_updater(&self, source: &Path) -> Result<LauncherOutcome> {
        Self::install_binary(
            source,
            &self.lock.paths().updater_file_named(&self.binary_names()),
            "updater",
        )
    }

    /// Whether this package wants a dialog placed for it.
    ///
    /// Three things have to be true at once, and each rules out a case where
    /// the binary would sit on disk doing nothing:
    ///
    /// * the publisher asked for prompting, because a prompt interrupts a user
    ///   and nobody else may decide to do that on their behalf;
    /// * the package checks while it runs, because a prompt only ever appears
    ///   from one of those checks — a check at startup stages silently and
    ///   says nothing, by design;
    /// * the package targets a platform with a dialog implemented, which is
    ///   Windows and macOS. Read from the manifest rather than from the host,
    ///   so a package built for one platform and inspected on another gets the
    ///   same answer everywhere.
    ///
    /// # This is the one update setting a later release cannot change
    ///
    /// Everything else about updating travels in each release's manifest and
    /// takes effect the moment that release is installed. A dialog cannot: it
    /// is a binary, and the background updater runs from inside the
    /// installation with no copy of one to place. So whether an installation
    /// is *able* to say anything is settled here, when it is first installed,
    /// and a publisher who turns prompting on later needs a new installer for
    /// it to mean anything.
    fn notifier_is_wanted(manifest: &xpack_core::Manifest) -> bool {
        // A prompt to show, or hooks whose start would otherwise wait with
        // nothing on screen: the dialog says why nothing has opened yet.
        let prompts = manifest.update.notify && manifest.update.check_while_running;
        (prompts || !manifest.hooks.is_empty())
            && matches!(manifest.platform.os, xpack_core::Os::Windows | xpack_core::Os::Macos)
    }

    /// Places the update notifier in the installation root.
    pub fn install_notifier(&self, source: &Path) -> Result<LauncherOutcome> {
        Self::install_binary(
            source,
            &self.lock.paths().notifier_file_named(&self.binary_names()),
            "notifier",
        )
    }

    /// Places the program that runs hooks in the installation root, and
    /// records the hook interface it serves.
    pub fn install_hook_engine(&self, source: &Path) -> Result<LauncherOutcome> {
        let outcome =
            Self::install_binary(source, &self.lock.paths().hook_engine_file(), "hook engine")?;
        if outcome == LauncherOutcome::Installed {
            self.record_hook_interface()?;
        }
        Ok(outcome)
    }

    /// Records that the installation's `xpack-hook` is this release's.
    fn record_hook_interface(&self) -> Result<()> {
        let mut state = self.load_state()?;
        state.hook_interface = Some(xpack_core::hooks::HOOK_INTERFACE);
        self.lock.save_state(&state)
    }

    /// Places the uninstaller in the installation root.
    pub fn install_uninstaller(&self, source: &Path) -> Result<LauncherOutcome> {
        Self::install_binary(
            source,
            &self.lock.paths().uninstaller_file_named(&self.binary_names()),
            "uninstaller",
        )
    }

    /// The names this installation's executables carry.
    ///
    /// Read from state on each use rather than cached, because a caller may
    /// place binaries either side of the install that pins them.
    fn binary_names(&self) -> xpack_core::BinaryNames {
        self.load_state().map_or(xpack_core::BinaryNames::Xpack, |state| state.binary_names())
    }

    /// Whether executables carrying xPack's own names are already on disk.
    ///
    /// True for every installation made before executables were named after
    /// the application they serve.
    fn has_xpack_named_binaries(paths: &xpack_core::InstallPaths) -> bool {
        paths.launcher_file().exists()
            || paths.gui_launcher_file().exists()
            || paths.updater_file().exists()
            || paths.uninstaller_file().exists()
    }

    /// Copies an executable into the installation, if nothing is there yet.
    ///
    /// Takes no `self`: the destination is already resolved by the caller, and
    /// the installation lock is held by whoever called into the installer.
    ///
    /// One that is there is left alone here. Replacing xPack's programs with a
    /// newer release's is done for all of them at once, by the install that
    /// supplies them, so that an installation never runs a mixture; see
    /// [`crate::runtime`].
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

    /// Returns `true` when a version's files are present on disk.
    ///
    /// See [`InstallPaths::has_version_files`], which this defers to so that
    /// [`crate::inspect()`] applies exactly the same test.
    ///
    /// [`InstallPaths::has_version_files`]: xpack_core::InstallPaths::has_version_files
    pub fn is_usable(&self, version: &Version) -> bool {
        self.lock.paths().has_version_files(version)
    }

    fn load_state(&self) -> Result<InstallState> {
        let id = self
            .lock
            .paths()
            .application_id()
            .ok_or_else(|| Error::invalid("installation", "root has no application id"))?;
        self.lock.load_or_new_state(id)
    }

    /// Refuses a package for another platform than the installation's.
    ///
    /// An installation holds one platform's programs: its launcher, updater
    /// and uninstaller are built for it. A version for another platform would
    /// be installed beside them and then never updated, because the updater
    /// refuses every package for a platform other than its own. It happens:
    /// the Intel installer run on an Apple Silicon Mac runs under Rosetta,
    /// believes the machine is Intel, and finds its package fits.
    fn ensure_same_platform(&self, manifest: &xpack_core::Manifest) -> Result<()> {
        let Ok(state) = self.load_state() else {
            return Ok(());
        };
        let Some(active) = state.current_version.as_ref() else {
            return Ok(());
        };
        let recorded = self.lock.paths().version_manifest_file(active);
        let Ok(installed) = std::fs::read(&recorded)
            .map_err(drop)
            .and_then(|bytes| xpack_core::Manifest::from_slice(&bytes).map_err(drop))
        else {
            return Ok(());
        };
        if installed.platform == manifest.platform {
            return Ok(());
        }
        let rosetta = if xpack_platform::translated_by_rosetta() {
            format!(
                " This installer is the {} one, running under Rosetta on a Mac the {} one is for; \
                 use that one.",
                manifest.platform, installed.platform
            )
        } else {
            String::new()
        };
        Err(Error::invalid(
            "install",
            format!(
                "this installation is for {}, and this package is for {}: installing it would \
                 leave the installation unable to update. Uninstall it first to change \
                 platform.{rosetta}",
                installed.platform, manifest.platform
            ),
        ))
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

/// Keeps the key sealed updates are opened with, readable by the owner only.
fn keep_seal_key(
    paths: &xpack_core::InstallPaths,
    key: &xpack_security::seal::SealKey,
) -> Result<()> {
    use std::io::Write;

    let file = paths.seal_key_file();
    atomic::create_dir_all(&paths.config_dir())?;
    // Created readable by the owner only, never for a moment by anyone else,
    // then put in place whole.
    let incoming = file.with_extension("key.new");
    let _ = std::fs::remove_file(&incoming);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options
        .open(&incoming)
        .and_then(|mut out| out.write_all(key.to_hex().as_bytes()).and_then(|()| out.sync_all()))
        .map_err(|e| Error::io(&incoming, e));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&incoming);
        return Err(e);
    }
    std::fs::rename(&incoming, &file).map_err(|e| Error::io(&file, e))
}

/// The key an installation keeps for its sealed updates, if it has one.
pub fn kept_seal_key(
    paths: &xpack_core::InstallPaths,
) -> Result<Option<xpack_security::seal::SealKey>> {
    let file = paths.seal_key_file();
    match std::fs::read_to_string(&file) {
        Ok(text) => {
            let text = zeroize::Zeroizing::new(text);
            xpack_security::seal::SealKey::from_hex(&text).map(Some)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(&file, e)),
    }
}

/// Opens `package` into `into` when it is sealed, with `key`, and returns the
/// file to verify: the opened one, or `package` itself when it is not sealed.
///
/// A sealed package with no key, or one sealed for another application, is
/// refused: the signature cannot be checked through the seal.
pub fn open_if_sealed(
    package: &Path,
    into: &Path,
    application_id: &str,
    key: Option<&xpack_security::seal::SealKey>,
) -> Result<PathBuf> {
    if !xpack_security::seal::is_sealed(package)? {
        return Ok(package.to_path_buf());
    }
    let sealed_for = xpack_security::seal::application_of(package)?;
    if sealed_for != application_id {
        return Err(Error::Integrity(format!(
            "the sealed package is for {sealed_for}, not {application_id}"
        )));
    }
    let key = key.ok_or_else(|| {
        Error::invalid(
            "package",
            "it is sealed with a password, and this installation has no key to open it; \
             install it again from the application's installer",
        )
    })?;
    xpack_security::seal::open(package, into, key)?;
    Ok(into.to_path_buf())
}

/// Takes write permission away from everyone but the owner, on everything
/// in an installation for every user.
///
/// The umask an elevated installer sets is not enough: on Linux, a directory
/// with a default access list gives what is created inside it that list's
/// permissions whatever the umask says, and a machine can come that way. So a
/// machine-wide install ends by making sure, file by file. Taking the group
/// bits away also caps what any access-list entry can grant. Links are left
/// alone: their own mode means nothing, and their targets are either in here
/// or not the installation's to change.
#[cfg_attr(
    not(unix),
    allow(
        clippy::unnecessary_wraps,
        reason = "one signature on every platform; Windows takes the directory's access list"
    )
)]
fn restrict_tree(root: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut pending = vec![root.to_path_buf()];
        while let Some(path) = pending.pop() {
            let meta = std::fs::symlink_metadata(&path).map_err(|e| Error::io(&path, e))?;
            if meta.file_type().is_symlink() {
                continue;
            }
            let mode = meta.permissions().mode();
            if mode & 0o022 != 0 {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode & !0o022))
                    .map_err(|e| Error::io(&path, e))?;
            }
            if meta.is_dir() {
                for entry in std::fs::read_dir(&path).map_err(|e| Error::io(&path, e))? {
                    pending.push(entry.map_err(|e| Error::io(&path, e))?.path());
                }
            }
        }
    }
    #[cfg(not(unix))]
    let _ = root;
    Ok(())
}

/// What placing xPack's own programs did, one entry per program.
struct Placed {
    launcher: Option<LauncherOutcome>,
    gui_launcher: Option<LauncherOutcome>,
    updater: Option<LauncherOutcome>,
    uninstaller: Option<LauncherOutcome>,
    notifier: Option<LauncherOutcome>,
    hook_engine: Option<LauncherOutcome>,
}

impl Placed {
    /// Whether any program was written where there was none.
    fn any_installed(&self) -> bool {
        [
            self.launcher,
            self.gui_launcher,
            self.updater,
            self.uninstaller,
            self.notifier,
            self.hook_engine,
        ]
        .contains(&Some(LauncherOutcome::Installed))
    }
}

/// Which of xPack's programs a [`Replacement`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Launcher,
    GuiLauncher,
    Updater,
    Uninstaller,
    Notifier,
    HookEngine,
    /// A command's copy of the console launcher, on Windows.
    CommandCopy,
}

/// A program already in the installation that this install replaces.
#[derive(Debug, Clone)]
struct Replacement {
    slot: Slot,
    program: Program,
}

/// Makes sure nothing of a per-user installation runs before its programs
/// are replaced, and keeps it that way until they are.
///
/// Asked of three things, because no one of them sees every case: the
/// presence lock, which every launcher of this release or later holds while
/// it runs; the instance lock, which the first copy of a running application
/// holds, with launchers older than the presence lock among them; and on
/// Windows each program's file, which says whether it runs whoever started
/// it. The presence lock is returned held alone, so no launcher starts until
/// the caller drops it.
fn ensure_nothing_runs(
    paths: &xpack_core::InstallPaths,
    replacing: &[Replacement],
    application: &str,
) -> Result<xpack_platform::PresenceLock> {
    let running = || Error::ApplicationRunning(application.to_string());
    let Some(alone) = xpack_platform::PresenceLock::exclusive(paths)? else {
        return Err(running());
    };
    if xpack_platform::InstanceLock::acquire(paths)?.is_none() {
        return Err(running());
    }
    for replacement in replacing {
        if xpack_platform::program_in_use(&replacement.program.destination)? {
            return Err(running());
        }
    }
    Ok(alone)
}

/// Whether every launcher in the installation is among the programs being
/// replaced, so none of the old ones has to read the incoming package. Both
/// count: shortcuts on Windows start the windowed one.
fn replaces_every_launcher(
    state: &InstallState,
    paths: &xpack_core::InstallPaths,
    replacing: &[Replacement],
) -> bool {
    let names = state.binary_names();
    let replaced = |path: &Path| replacing.iter().any(|r| r.program.destination == path);
    // Where there is one build, the windowed launcher's name is the console
    // one's: the same file, replaced once.
    let console = paths.launcher_file_named(&names);
    let windowed = paths.gui_launcher_file_named(&names);
    let covered = |path: &Path| !path.exists() || replaced(path);
    !replacing.is_empty() && covered(&console) && covered(&windowed)
}

fn describe_scope(scope: xpack_core::InstallScope) -> &'static str {
    match scope {
        xpack_core::InstallScope::User => "one user",
        xpack_core::InstallScope::Machine => "every user",
    }
}

/// Removes copies of `name` an earlier replacement set aside, where nothing
/// runs them any more. One still running stays until a later install.
/// Marks a file executable by its owner, and readable and executable by all.
///
/// Windows has no equivalent bit — executability there comes from the file
/// extension, which [`InstallPaths::launcher_file`] already supplies.
///
/// [`InstallPaths::launcher_file`]: xpack_core::InstallPaths::launcher_file
#[cfg(unix)]
pub(crate) fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| Error::io(path, e))
}

// Mirrors the Unix version's signature, which genuinely can fail.
#[allow(clippy::unnecessary_wraps)]
#[cfg(not(unix))]
pub(crate) fn set_executable(path: &Path) -> Result<()> {
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
    /// What happened to the shortcut on the desktop, if there was one.
    pub desktop_shortcut: crate::integration::Outcome,
    /// What happened to the command, if there was one.
    pub command: crate::integration::Outcome,
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
    uninstall_into(lock, desktop_roots, None)
}

/// Removes an installation, writing desktop and command changes under
/// explicit roots.
///
/// See [`InstallOptions::desktop_roots`] and [`InstallOptions::command_roots`]
/// for why this exists.
pub fn uninstall_into(
    lock: InstallLock,
    desktop_roots: Option<&crate::integration::Roots>,
    command_roots: Option<&crate::integration::command::CommandRoots>,
) -> Result<Removal> {
    uninstall_reporting(lock, desktop_roots, command_roots, &NoProgress)
}

/// Removes an installation as [`uninstall_into`] does, telling `progress`
/// when its uninstall hooks run and passing on what they write.
///
/// # Hooks
///
/// The active version's `uninstall.before` hooks run first, with the
/// installation whole; its `uninstall.after` hooks run once its files are
/// gone, from a copy of their scripts and of `xpack-hook` taken beforehand,
/// beside the installation. A failed uninstall hook is logged and the
/// uninstall carries on: a half-removed application is worse than either
/// outcome. Nothing cancels them.
///
/// The installation's data directory goes too, after `uninstall.after`, and
/// so the root: uninstalling leaves nothing of the installation.
pub fn uninstall_reporting(
    lock: InstallLock,
    desktop_roots: Option<&crate::integration::Roots>,
    command_roots: Option<&crate::integration::command::CommandRoots>,
    progress: &dyn ProgressReporter,
) -> Result<Removal> {
    let paths = lock.paths().clone();
    let root = paths.root().to_path_buf();

    // Resolved *before* state is cleared and the versions are deleted. The
    // Start-Menu shortcut and the macOS bundle are named after the display
    // name, which lives in the installed manifest — read it afterwards and
    // there is nothing left to read it from, and the entry is orphaned in the
    // user's menu with no way to find it again.
    let desktop_entry = desktop_entry_for_removal(&lock);
    let commands = commands_for_removal(&lock);
    let hooks = UninstallHooks::prepare(&lock);
    let context = hooks.as_ref().map(|hooks| hooks.context(progress));
    if let (Some(hooks), Some(context)) = (&hooks, &context) {
        hooks.run_before(&paths, context);
    }

    let (desktop, desktop_shortcut, command) =
        remove_contents(&lock, desktop_entry.as_ref(), &commands, desktop_roots, command_roots)?;

    if let (Some(hooks), Some(context)) = (&hooks, &context) {
        hooks.run_after(&paths, context);
    }
    // What the application and its hooks kept, and what a hook run cut short
    // left: the installation's, and gone with it.
    atomic::remove_dir_all_if_exists(&paths.data_dir())?;
    atomic::remove_dir_all_if_exists(&paths.hook_runs_dir())?;

    // Releases the lock and closes the handle to the file inside `state/`.
    // Everything after this point runs unlocked, which is why it is ordered
    // last and why the root removal cannot recurse.
    drop(lock);
    drop(hooks);
    let (root_removed, remaining) = finish_removal(&paths)?;
    tracing::info!(root = %root.display(), root_removed, "installation removed");
    Ok(Removal { root, root_removed, remaining, desktop, desktop_shortcut, command })
}

/// The active version's uninstall hooks, and what `uninstall.after` runs
/// from once the installation's files are gone.
struct UninstallHooks {
    manifest: xpack_core::Manifest,
    version_dir: PathBuf,
    scope: xpack_core::InstallScope,
    engine: Option<PathBuf>,
    /// A copy of `xpack-hook` and of the `uninstall.after` scripts, beside
    /// the installation rather than in the system's temporary directory: an
    /// administrator's process can inherit a user's, who could change a
    /// script there between its copy and its run. Removed when this is
    /// dropped.
    kept: Option<tempfile::TempDir>,
}

impl UninstallHooks {
    /// Reads the active version's hooks, and keeps what `uninstall.after`
    /// will need. None when it has no uninstall hooks, or cannot be read.
    fn prepare(lock: &InstallLock) -> Option<Self> {
        let paths = lock.paths();
        let state = lock.load_state().ok()?.value;
        let version = state.current_version.clone().or_else(|| state.previous_version.clone())?;
        let bytes = std::fs::read(paths.version_manifest_file(&version)).ok()?;
        let manifest = xpack_core::Manifest::from_slice(&bytes).ok()?;
        if manifest.hooks.uninstall.is_empty() {
            return None;
        }
        let engine = Some(paths.hook_engine_file()).filter(|engine| engine.is_file());
        let version_dir = paths.version_dir(&version);
        let after = point(Operation::Uninstall, Moment::After);
        let kept = match (&engine, manifest.hooks.at(after).next().is_some()) {
            (Some(engine), true) => keep_for_after(paths, engine, &manifest, &version_dir),
            _ => None,
        };
        Some(Self { manifest, version_dir, scope: state.scope, engine, kept })
    }

    fn context<'a>(&'a self, progress: &'a dyn ProgressReporter) -> HookContext<'a> {
        HookContext { engine: self.engine.as_deref(), scope: self.scope, progress, cancel: None }
    }

    fn run_before(&self, paths: &xpack_core::InstallPaths, context: &HookContext<'_>) {
        let version = &self.manifest.application.version;
        context.run_and_carry_on(
            paths,
            &self.manifest,
            &self.version_dir,
            point(Operation::Uninstall, Moment::Before),
            Some(version),
            None,
            None,
        );
    }

    fn run_after(&self, paths: &xpack_core::InstallPaths, context: &HookContext<'_>) {
        let after = point(Operation::Uninstall, Moment::After);
        if self.manifest.hooks.at(after).next().is_none() {
            return;
        }
        let Some(kept) = &self.kept else {
            tracing::warn!("the uninstall.after hooks could not be kept aside, and do not run");
            return;
        };
        let engine = kept.path().join(engine_name());
        let context = HookContext { engine: Some(&engine), ..*context };
        let version = &self.manifest.application.version;
        context.run_and_carry_on(
            paths,
            &self.manifest,
            &kept.path().join("scripts"),
            after,
            Some(version),
            None,
            None,
        );
    }
}

/// The file name `xpack-hook` has on this platform.
fn engine_name() -> String {
    format!("xpack-hook{}", std::env::consts::EXE_SUFFIX)
}

/// Copies `xpack-hook` and the `uninstall.after` scripts beside the
/// installation, where they outlive its removal. None, logged, if that fails.
fn keep_for_after(
    paths: &xpack_core::InstallPaths,
    engine: &Path,
    manifest: &xpack_core::Manifest,
    version_dir: &Path,
) -> Option<tempfile::TempDir> {
    let beside = paths.root().parent()?;
    let kept = tempfile::Builder::new()
        .prefix(".xpack-uninstall-")
        .tempdir_in(beside)
        .map_err(|error| tracing::warn!(%error, "could not keep the uninstall.after hooks aside"))
        .ok()?;
    let copy = || -> std::io::Result<()> {
        let to = kept.path().join(engine_name());
        std::fs::copy(engine, &to)?;
        let after = point(Operation::Uninstall, Moment::After);
        for hook in manifest.hooks.at(after) {
            let relative: PathBuf = hook.script.split('/').collect();
            let target = kept.path().join("scripts").join(&relative);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(version_dir.join(&relative), target)?;
        }
        Ok(())
    };
    match copy() {
        Ok(()) => Some(kept),
        Err(error) => {
            tracing::warn!(%error, "could not keep the uninstall.after hooks aside");
            None
        }
    }
}

/// Removes what is xPack's in an installation, under its lock: versions,
/// programs, configuration, the desktop entry, shortcut and commands. What
/// is in `state/`, where the lock lives, is left for [`finish_removal`].
fn remove_contents(
    lock: &InstallLock,
    desktop_entry: Option<&crate::integration::Entry>,
    commands: &[crate::integration::command::Command],
    desktop_roots: Option<&crate::integration::Roots>,
    command_roots: Option<&crate::integration::command::CommandRoots>,
) -> Result<(crate::integration::Outcome, crate::integration::Outcome, crate::integration::Outcome)>
{
    let paths = lock.paths();
    // Cleared and persisted before anything is deleted, so an interruption
    // cannot leave state describing versions that no longer exist.
    let id = paths
        .application_id()
        .ok_or_else(|| Error::invalid("installation", "root has no application id"))?;
    let mut state = lock.load_or_new_state(id)?;
    // Read before state is cleared, because clearing it is what would make
    // the named executables unfindable.
    let binary_names = state.binary_names();
    // And where its entries are: a machine-wide installation's are every
    // user's, never the ones of whoever is removing it.
    let scope = state.scope;
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
    // Both sets. An uninstall that removed only the names the current state
    // document mentions would leave files behind on an installation whose
    // state was lost or rewritten, and leaving an executable in a directory a
    // user asked to be rid of is the one thing an uninstaller must not do.
    for names in [&xpack_core::BinaryNames::Xpack, &binary_names] {
        atomic::remove_file_if_exists(&paths.launcher_file_named(names))?;
        atomic::remove_file_if_exists(&paths.gui_launcher_file_named(names))?;
        atomic::remove_file_if_exists(&paths.updater_file_named(names))?;
        atomic::remove_file_if_exists(&paths.notifier_file_named(names))?;
        atomic::remove_file_if_exists(&paths.uninstaller_file_named(names))?;
    }
    atomic::remove_file_if_exists(&paths.hook_engine_file())?;
    // What a hook run that was cut short left behind.
    atomic::remove_dir_all_if_exists(&paths.hook_runs_dir())?;

    // The icon copied into the root for the desktop entry, at the exact path
    // the manifest implies rather than anything matching `icon.*`. Globbing
    // would delete a user's own `icon.jpg` from the root, which this function
    // promises never to do — such a file is reported in `remaining` instead.
    if let Some(icon) = desktop_entry.and_then(|entry| entry.icon.clone()) {
        atomic::remove_file_if_exists(&icon)?;
    }

    // The pinned signing keys. Leaving these behind is the consequential part
    // of an incomplete uninstall: a later reinstall would silently inherit a
    // trust decision the user believes they revoked.
    atomic::remove_dir_all_if_exists(&paths.config_dir())?;

    // Before the lock is released, so it cannot race an install that starts
    // the moment the lock is free and recreates the entry we are removing.
    let desktop = match desktop_entry {
        Some(entry) => {
            let outcome =
                match desktop_roots.cloned().or_else(|| crate::integration::roots_for(scope)) {
                    Some(roots) => crate::integration::remove_from(entry, &roots),
                    None => crate::integration::Outcome::Failed(
                        "could not find where the menu entry is".to_string(),
                    ),
                };
            outcome.log("uninstall");
            outcome
        }
        None => crate::integration::Outcome::NothingToDo,
    };
    // By its record, which is in the state directory removed below.
    let desktop_shortcut = crate::integration::desktop_shortcut::remove(paths);
    desktop_shortcut.log("uninstall");
    let command = match (
        commands.is_empty(),
        command_roots.cloned().or_else(|| CommandRoots::for_scope(scope)),
    ) {
        (true, _) => crate::integration::Outcome::NothingToDo,
        (false, Some(roots)) => {
            let outcome = crate::integration::command::remove_all(commands, &roots);
            outcome.log("uninstall");
            outcome
        }
        (false, None) => {
            crate::integration::Outcome::Failed("could not find where commands are".to_string())
        }
    };

    Ok((desktop, desktop_shortcut, command))
}

/// Finishes removing an installation once its lock is released: its
/// `state/` directory, then its root, if nothing else is left in it.
///
/// What [`uninstall_into`] ends with, and what a caller does after an
/// install of a first installation fails (see [`Installer::install`]): the
/// lock lives in `state/`, and Windows deletes no file that is open.
/// Returns whether the root went, and what kept it if not: a user's own
/// files are never removed.
pub fn finish_removal(paths: &xpack_core::InstallPaths) -> Result<(bool, Vec<PathBuf>)> {
    let root = paths.root();
    // Only an installation that has nothing left: called by mistake on one
    // that still has a version, on disk or in its state, it removes nothing.
    let has_versions = paths.versions_dir().exists()
        || InstallState::load(&paths.state_file()).is_ok_and(|s| !s.value.versions.is_empty());
    if has_versions {
        return Ok((false, entries_in(root)));
    }
    atomic::remove_dir_all_if_exists(&paths.state_dir())?;
    let root_removed = match std::fs::remove_dir(root) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    };
    let remaining = if root_removed { Vec::new() } else { entries_in(root) };
    if !remaining.is_empty() {
        tracing::warn!(
            root = %root.display(),
            count = remaining.len(),
            "installation root was not empty and was left in place"
        );
    }
    Ok((root_removed, remaining))
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
    names: &xpack_core::BinaryNames,
) -> Option<PathBuf> {
    [entry.target.clone(), paths.launcher_file_named(names), paths.gui_launcher_file_named(names)]
        .into_iter()
        .find(|candidate| candidate.is_file())
}

/// The command the active version put in place, if it asked for one.
fn previous_commands(
    paths: &xpack_core::InstallPaths,
    state: &InstallState,
) -> Vec<crate::integration::command::Command> {
    let Some(version) = state.current_version.as_ref() else {
        return Vec::new();
    };
    let Some(manifest) = std::fs::read(paths.version_manifest_file(version))
        .ok()
        .and_then(|bytes| xpack_core::Manifest::from_slice(&bytes).ok())
    else {
        return Vec::new();
    };
    crate::integration::command::Command::all_from_manifest(&manifest, paths, &state.binary_names())
}

/// Rebuilds the commands an installation would have put in place.
fn commands_for_removal(lock: &InstallLock) -> Vec<crate::integration::command::Command> {
    let paths = lock.paths();
    let Some(id) = paths.application_id() else {
        return Vec::new();
    };
    let Ok(state) = lock.load_or_new_state(id) else {
        return Vec::new();
    };
    let Some(version) = state.current_version.clone().or_else(|| state.previous_version.clone())
    else {
        return Vec::new();
    };
    let Some(manifest) = std::fs::read(paths.version_manifest_file(&version))
        .ok()
        .and_then(|bytes| xpack_core::Manifest::from_slice(&bytes).ok())
    else {
        return Vec::new();
    };
    crate::integration::command::Command::all_from_manifest(&manifest, paths, &state.binary_names())
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
    crate::integration::Entry::from_manifest(&manifest, paths, &state.binary_names())
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use xpack_core::manifest::{Application, FormatVersion, LaunchSpec, PayloadSpec, UpdateSpec};
    use xpack_core::{Arch, Os, Platform, Version};

    use super::*;

    /// A manifest for `platform`, carrying `update` and nothing else of note.
    fn manifest(os: Os, update: UpdateSpec) -> xpack_core::Manifest {
        xpack_core::Manifest {
            format_version: FormatVersion::CURRENT,
            application: Application {
                id: "com.example.app".into(),
                name: "App".into(),
                version: Version::parse("1.0.0").unwrap(),
                description: None,
                publisher: None,
            },
            platform: Platform::new(os, Arch::X64),
            launch: LaunchSpec {
                executable: "bin/app".into(),
                arguments: vec![],
                working_directory: None,
                keep_working_directory: false,
                environment: BTreeMap::new(),
            },
            update,
            health: xpack_core::HealthSpec::default(),
            desktop: xpack_core::DesktopSpec::default(),
            signing_key: None,
            payload: PayloadSpec::default(),
            created_at: None,
            command: None,
            commands: Vec::new(),
            instance: xpack_core::InstanceSpec::default(),
            hooks: xpack_core::hooks::Hooks::default(),
        }
    }

    /// The configuration that wants a dialog: prompting on, and the checking
    /// while running that a prompt can appear from.
    fn wants_prompting() -> UpdateSpec {
        UpdateSpec { notify: true, check_while_running: true, ..UpdateSpec::default() }
    }

    #[test]
    fn a_package_that_asked_for_a_prompt_gets_one_where_a_dialog_exists() {
        for os in [Os::Windows, Os::Macos] {
            assert!(Installer::notifier_is_wanted(&manifest(os, wants_prompting())), "{os:?}");
        }
    }

    #[test]
    fn a_package_that_did_not_ask_gets_no_dialog_on_disk() {
        // The default, and the overwhelmingly common case.
        let silent = UpdateSpec { notify: false, ..wants_prompting() };
        assert!(!Installer::notifier_is_wanted(&manifest(Os::Windows, silent)));
        assert!(!Installer::notifier_is_wanted(&manifest(Os::Windows, UpdateSpec::default())));
    }

    #[test]
    fn asking_for_a_prompt_without_checking_while_running_places_nothing() {
        // A prompt only ever appears from a check made while the application
        // runs. A startup check stages in silence by design, so the binary
        // would sit there and never run.
        let startup_only = UpdateSpec { check_while_running: false, ..wants_prompting() };
        assert!(!Installer::notifier_is_wanted(&manifest(Os::Windows, startup_only)));
    }

    #[test]
    fn an_interval_alone_does_not_ask_for_a_dialog() {
        // The interval says how often to ask a server, which has nothing to do
        // with whether anybody is told the answer.
        let quiet = UpdateSpec {
            check_while_running: true,
            check_interval_minutes: Some(30),
            ..UpdateSpec::default()
        };
        assert!(!Installer::notifier_is_wanted(&manifest(Os::Windows, quiet)));
    }

    #[test]
    fn a_platform_with_no_dialog_carries_no_dialog_binary() {
        assert!(!Installer::notifier_is_wanted(&manifest(Os::Linux, wants_prompting())));
    }
}
