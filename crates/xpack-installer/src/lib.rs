//! The self-contained installer: one file a user runs to get an application.
//!
//! Every other xPack component assumes an installation already exists. This is
//! the one that creates it, on a machine where nothing of xPack is present and
//! the developer CLI never will be.
//!
//! # What it does, and what it deliberately does not
//!
//! It unpacks its own payload, verifies the application package against the
//! key the publisher pinned into it, and hands the result to
//! [`xpack_install`] — the same installer the CLI and the background updater
//! use. It does not extract archives itself, does not decide trust for itself
//! and does not write installation state for itself.
//!
//! That is the whole design rule. An installer with its own copy of the
//! install path is an installer that misses the next fix to it, and the
//! failure would appear on exactly the machines nobody can reproduce: the ones
//! where the application was installed once and never touched again.
//!
//! # No window here
//!
//! This crate draws nothing. The installation wizard lives in
//! `xpack-installer-ui`, which implements nothing of installing and reaches
//! it only through [`xpack_installer_ui::Engine`], implemented here over
//! [`VerifiedPayload`]. The executables that show it are built by
//! `xpack-installer-stub`, the one crate that turns the window on: the
//! developer CLI depends on this crate to build installers, and must not
//! compile a windowing toolkit to do so.

pub mod bundle;
mod engine;

pub use xpack_installer_ui::UiPlan;

use std::path::{Path, PathBuf};

use xpack_core::{Error, InstallPaths, Manifest, Platform, ProgressReporter, Result};
use xpack_install::{Existing, InstallOptions, Installer, TrustDecision, open_and_verify};
use xpack_package::PackageReader;
use xpack_platform::InstallLock;
use xpack_security::PublicKey;

use bundle::{BINARY_PREFIX, InstallPlan, LICENCE_ENTRY, PACKAGE_ENTRY, PLAN_ENTRY};

/// Exit codes. Anything a script might branch on gets its own.
///
/// The same table `xpack` and `xpack-updater` use, so a monitoring system
/// watching all three never has to ask which one a code came from.
pub mod exit {
    /// The application was installed.
    pub const INSTALLED: u8 = 0;
    /// The installation could not be completed.
    pub const FAILED: u8 = 1;
    /// A signature or checksum did not verify. Never worth retrying.
    pub const INTEGRITY: u8 = 3;
    /// Another xPack operation holds the lock. Worth retrying.
    pub const BUSY: u8 = 4;
    /// The person installing chose not to, before anything was changed.
    pub const CANCELLED: u8 = 5;
}

/// Maps a failure to the exit code a script should branch on.
///
/// Uses the same predicate the CLI does rather than matching variants here, so
/// the two cannot drift into disagreeing about what counts as a security
/// failure — the one code a script must never retry.
pub fn exit_code_for(error: &Error) -> u8 {
    if error.is_integrity_failure() {
        return exit::INTEGRITY;
    }
    match error {
        Error::Locked(_) => exit::BUSY,
        _ => exit::FAILED,
    }
}

/// Where a root came from, which decides whether it may be changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootSource {
    /// Named on the command line or in `XPACK_INSTALL_ROOT`.
    Given,
    /// Where this user already has the application, found through its
    /// desktop entry.
    Found,
    /// The per-user default.
    Default,
}

/// The root an installation goes into, and why that one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRoot {
    /// The directory holding the user's xPack applications.
    pub root: PathBuf,
    /// Where it came from.
    pub source: RootSource,
}

/// Picks the root: the one given, else where the application already is,
/// else the default.
///
/// An installation found elsewhere wins over the default because a second
/// copy beside it would fight it over the same menu entry and uninstall
/// entry, and removing either would take the other's entry with it.
fn choose_root(
    explicit: Option<&Path>,
    recorded: impl FnOnce() -> Option<PathBuf>,
    default: impl FnOnce() -> Result<PathBuf>,
) -> Result<ResolvedRoot> {
    if let Some(root) = explicit {
        // Made absolute against the working directory, as the shell meant it.
        // Kept relative, it would be written into the menu and uninstall
        // entries, which then break the moment anything starts from elsewhere.
        let root = std::path::absolute(root).map_err(|e| Error::io(root, e))?;
        return Ok(ResolvedRoot { root, source: RootSource::Given });
    }
    if let Some(root) = recorded() {
        return Ok(ResolvedRoot { root, source: RootSource::Found });
    }
    Ok(ResolvedRoot { root: default()?, source: RootSource::Default })
}

/// What the person installing chose.
///
/// Every field that is not the root defaults to what happens when nobody is
/// asked, so an installer run without a window and one whose wizard was
/// clicked straight through do exactly the same thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The directory holding the user's xPack applications. The
    /// application's own directory is made inside it.
    pub root: PathBuf,
    /// Whether to add the desktop entry the package asks for. `None` leaves
    /// it to the package. See [`InstallOptions::desktop_entry`].
    pub desktop_entry: Option<bool>,
}

impl Request {
    /// Install into `root`, choosing nothing else.
    pub fn new(root: PathBuf) -> Self {
        Self { root, desktop_entry: None }
    }
}

/// What an installation run produced.
#[derive(Debug)]
pub struct Outcome {
    /// Where the application was installed.
    pub root: PathBuf,
    /// Version now installed.
    pub version: xpack_core::Version,
    /// Whether it was made active.
    pub activated: bool,
    /// The launcher a user should run.
    pub launcher: PathBuf,
    /// What happened to the desktop entry, if the package asked for one.
    pub desktop: xpack_install::DesktopOutcome,
}

/// The payload, unpacked into a temporary directory.
///
/// Held together with the directory so the files outlive the archive and are
/// cleaned up when the installer exits, however it exits.
///
/// Nothing can be installed from this: see [`Payload::verify`].
pub struct Payload {
    _directory: tempfile::TempDir,
    plan: InstallPlan,
    package: PathBuf,
    binaries: Vec<PathBuf>,
    licence: Option<String>,
}

impl Payload {
    /// The install plan the publisher wrote at build time.
    ///
    /// Unsigned. It shapes how the wizard looks — which pages, the licence,
    /// a few reworded lines — but no fact about the application and nothing
    /// about where it is installed comes from it: those come from the verified
    /// manifest and the root resolved by [`VerifiedPayload::resolve_root`].
    pub fn plan(&self) -> &InstallPlan {
        &self.plan
    }

    /// Unpacks a payload archive into a fresh temporary directory.
    ///
    /// Entry names are validated before use: this archive is the one part of
    /// an installer an attacker could rewrite without touching a signature, so
    /// a crafted name must not be able to write outside the temporary
    /// directory. The same rule the package reader applies, applied here.
    pub fn unpack(bytes: &[u8]) -> Result<Self> {
        let directory =
            tempfile::tempdir().map_err(|e| Error::io(Path::new("a temporary directory"), e))?;
        let root = directory.path().to_path_buf();

        let cursor = std::io::Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cursor)
            .map_err(|e| Error::invalid("installer payload", e.to_string()))?;

        let mut plan = None;
        let mut package = None;
        let mut binaries = Vec::new();
        let mut licence = None;

        for index in 0..archive.len() {
            let mut entry = archive
                .by_index(index)
                .map_err(|e| Error::invalid("installer payload", e.to_string()))?;
            if entry.is_dir() {
                continue;
            }

            let name = entry.name().to_string();
            let destination = safe_join(&root, &name)?;

            if let Some(parent) = destination.parent() {
                xpack_core::atomic::create_dir_all(parent)?;
            }
            let mut out =
                std::fs::File::create(&destination).map_err(|e| Error::io(&destination, e))?;
            std::io::copy(&mut entry, &mut out).map_err(|e| Error::io(&destination, e))?;
            drop(out);

            if name == PLAN_ENTRY {
                let document: InstallPlan = xpack_core::atomic::read_json(&destination)?;
                document.ensure_supported()?;
                plan = Some(document);
            } else if name == PACKAGE_ENTRY {
                package = Some(destination);
            } else if name.starts_with(BINARY_PREFIX) {
                make_executable(&destination)?;
                binaries.push(destination);
            } else if name == LICENCE_ENTRY {
                licence = Some(read_licence(&destination)?);
            }
        }

        let plan = plan.ok_or_else(|| {
            Error::invalid("installer payload", format!("contains no {PLAN_ENTRY}"))
        })?;
        let package = package.ok_or_else(|| {
            Error::invalid("installer payload", format!("contains no {PACKAGE_ENTRY}"))
        })?;

        binaries.sort();
        Ok(Self { _directory: directory, plan, package, binaries, licence })
    }

    /// Checks the package before anything is shown or installed.
    ///
    /// Its signature against the key the publisher pinned into this
    /// installer, and its platform against this machine. Only what this
    /// returns can be inspected or installed, so neither the console nor a
    /// window can reach an installation without passing through here first,
    /// and every fact either of them shows comes from a manifest that has.
    ///
    /// The plan is held to the package too. It names the application so it
    /// can be described before anything is read, but it is not signed; a plan
    /// naming a different application than the signed package is a rewritten
    /// installer, and nothing is installed from it.
    pub fn verify(self) -> Result<VerifiedPayload> {
        let key = PublicKey::parse_hex(&self.plan.signing_key)?;
        let mut verified =
            PackageReader::open(&self.package)?.verify_with_keys(std::slice::from_ref(&key))?;
        verified.ensure_installable_on(Platform::host()?)?;
        let manifest = verified.manifest().clone();

        if manifest.application.id != self.plan.application_id {
            return Err(Error::Integrity(format!(
                "the installer names {:?} but its signed package is {:?}",
                self.plan.application_id, manifest.application.id
            )));
        }
        let icon = read_icon(&mut verified)?;
        Ok(VerifiedPayload { payload: self, key, manifest, icon })
    }

    /// Finds a runtime binary in the payload by its stem.
    ///
    /// Matched on the file stem so one lookup works whether or not the name
    /// carries `.exe`.
    fn binary(&self, stem: &str) -> Option<PathBuf> {
        self.binaries
            .iter()
            .find(|path| path.file_stem().is_some_and(|found| found == stem))
            .cloned()
    }
}

/// The largest icon read out of a package for the installer to show.
///
/// A macOS `.icns` with every size up to 1024 pixels runs to a few megabytes;
/// anything bigger is not an icon.
pub const MAX_ICON_BYTES: u64 = 8 * 1024 * 1024;

/// The application's icon, as its package declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    /// Its path in the package, whose extension says what format it is.
    pub name: String,
    /// Its bytes, checked against the signed manifest.
    pub bytes: Vec<u8>,
}

/// Reads the icon the manifest names, from the package that was just verified.
///
/// The one source of the application's icon: the same file the installed
/// menu entry uses, covered by the same signature. An icon that fails its
/// digest means a tampered package and refuses the install; one that is
/// merely too large to show is left out, and the window falls back to a
/// default.
fn read_icon(package: &mut xpack_package::VerifiedPackage) -> Result<Option<Icon>> {
    let Some(name) = package.manifest().desktop.icon.clone() else {
        return Ok(None);
    };
    match package.read_payload_file(&name, MAX_ICON_BYTES) {
        Ok(bytes) => Ok(Some(Icon { name, bytes })),
        Err(error) if error.is_integrity_failure() => Err(error),
        Err(error) => {
            tracing::warn!(%error, "the application's icon cannot be shown");
            Ok(None)
        }
    }
}

/// A payload whose package has been verified.
pub struct VerifiedPayload {
    payload: Payload,
    key: PublicKey,
    manifest: Manifest,
    icon: Option<Icon>,
}

impl VerifiedPayload {
    /// The verified manifest: the only source of what may be shown about the
    /// application.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The install plan the publisher wrote at build time. Unsigned.
    pub fn plan(&self) -> &InstallPlan {
        &self.payload.plan
    }

    /// The wizard's settings: the publisher's, or the recommended ones.
    pub fn ui(&self) -> xpack_installer_ui::UiPlan {
        self.payload.plan.ui.clone().unwrap_or_default()
    }

    /// The licence the wizard shows, when the installer carries one.
    pub fn licence(&self) -> Option<&str> {
        self.payload.licence.as_deref()
    }

    /// The application's icon, read from the verified package.
    pub fn icon(&self) -> Option<&Icon> {
        self.icon.as_ref()
    }

    /// The application's own directory under `root`.
    pub fn paths(&self, root: &Path) -> Result<InstallPaths> {
        InstallPaths::new(root, &self.manifest.application.id)
    }

    /// The root to install into: the one given, else where this user already
    /// has the application, else the default. Every way of installing uses
    /// this, so none of them makes a second copy the others would not.
    pub fn resolve_root(&self, explicit: Option<&Path>) -> Result<ResolvedRoot> {
        choose_root(explicit, || self.recorded_root(), xpack_core::paths::default_install_root)
    }

    /// Where this user already has the application, according to its desktop
    /// entry, when that entry leads to a real installation of it.
    pub fn recorded_root(&self) -> Option<PathBuf> {
        let application = &self.manifest.application;
        xpack_install::integration::recorded_installation_for_user(
            &application.id,
            &application.name,
        )
    }

    /// What installing into `root` would find. Changes nothing.
    pub fn inspect(&self, root: &Path) -> Result<Existing> {
        Ok(xpack_install::inspect(&self.paths(root)?, &self.manifest.application.version))
    }

    /// Installs the application as `request` asks, reporting progress.
    pub fn install_into(
        &self,
        request: &Request,
        progress: &dyn ProgressReporter,
    ) -> Result<Outcome> {
        let paths = self.paths(&request.root)?;
        let lock = InstallLock::acquire(&paths)?;

        // Verified again, under the lock, by the path every install takes.
        // The early check decided what could be shown; this one is what the
        // installation is built from, and it pins the publisher's key so
        // every later update is held to it too.
        let mut verified = open_and_verify(
            &self.payload.package,
            &lock,
            &TrustDecision::Explicit(self.key.clone()),
        )?;

        let payload = &self.payload;
        let options = InstallOptions {
            activate: payload.plan.activate,
            allow_downgrade: false,
            launcher: payload.binary("xpack-launcher"),
            gui_launcher: payload.binary("xpack-launcherw"),
            updater: payload.binary("xpack-updater"),
            uninstaller: payload.binary("xpack-uninstaller"),
            notifier: payload.binary("xpack-notify"),
            desktop_roots: None,
            desktop_entry: request.desktop_entry,
            command: None,
            command_roots: None,
        };

        let installed =
            Installer::new(&lock).install_with_progress(&mut verified, &options, progress)?;

        // Read after the install, because that is when an installation is
        // given the names its executables carry. Reporting the path this
        // binary was built expecting would print one the user cannot run.
        let names = lock
            .load_or_new_state(&self.manifest.application.id)
            .map_or(xpack_core::BinaryNames::Xpack, |state| state.binary_names());

        Ok(Outcome {
            root: paths.root().to_path_buf(),
            version: installed.version,
            activated: installed.activated,
            launcher: launcher_to_report(&paths, &names),
            desktop: installed.desktop,
        })
    }
}

/// Reads the licence out of the unpacked payload, held to the same rules it
/// was built under: the payload is not signed, so nothing about it is assumed.
fn read_licence(path: &Path) -> Result<String> {
    let size = std::fs::metadata(path).map_err(|e| Error::io(path, e))?.len();
    if usize::try_from(size).map_or(true, |size| size > bundle::MAX_LICENCE_BYTES) {
        return Err(Error::invalid("licence", format!("is {size} bytes, over the limit")));
    }
    let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
    Ok(bundle::check_licence(&bytes)?.to_string())
}

/// The launcher to tell the user about, which has to be one that is there.
///
/// Windows prefers the windowed build: it is what a shortcut points at, and
/// starting an application from the console build leaves an empty console
/// window behind it. But an installer is only able to place the binaries its
/// payload carries, and a payload without a windowed build is a payload whose
/// installation has a console launcher and nothing else.
///
/// Naming the preferred path regardless would print a path that does not
/// exist -- the one thing this line must never do, since a user's next action
/// is to run what it printed.
fn launcher_to_report(paths: &InstallPaths, names: &xpack_core::BinaryNames) -> PathBuf {
    let preferred = paths.shortcut_target_named(names);
    if preferred.is_file() { preferred } else { paths.launcher_file_named(names) }
}

/// Joins an archive entry name onto a directory, refusing anything that escapes.
///
/// The payload archive carries no signature of its own — the signature that
/// matters is on the package *inside* it — so a tampered installer could name
/// an entry `../../../.bashrc`. Unpacking happens before any verification can,
/// which is precisely why the name has to be checked here.
fn safe_join(root: &Path, name: &str) -> Result<PathBuf> {
    let normalised = name.replace('\\', "/");
    if normalised.is_empty() || normalised.starts_with('/') {
        return Err(Error::invalid(
            "installer payload",
            format!("{name:?} is not a relative path"),
        ));
    }

    let mut joined = root.to_path_buf();
    for component in normalised.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(Error::invalid(
                "installer payload",
                format!("{name:?} contains a traversing path component"),
            ));
        }
        // A drive-qualified component resolves outside the target on Windows.
        if component.len() >= 2 && component.as_bytes()[1] == b':' {
            return Err(Error::invalid(
                "installer payload",
                format!("{name:?} is drive-qualified"),
            ));
        }
        joined.push(component);
    }
    Ok(joined)
}

/// Marks an unpacked runtime binary executable.
///
/// The archive's own mode bits are not consulted: this file is about to be
/// copied into an installation by the installer, which sets the final
/// permissions itself. This only has to make it runnable here.
#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| Error::io(path, e))
}

// Mirrors the Unix version's signature, which genuinely can fail.
#[allow(clippy::unnecessary_wraps)]
#[cfg(not(unix))]
fn make_executable(path: &Path) -> Result<()> {
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_given_root_wins_then_an_existing_installation_then_the_default() {
        let found = || Some(PathBuf::from("/found"));
        let nothing = || None;
        let default = || Ok(PathBuf::from("/default"));
        // Absolute on every platform: `/given` has no drive on Windows, so it
        // would rightly be resolved against the current one.
        let given_root = std::env::current_dir().unwrap().join("given");

        let given = choose_root(Some(&given_root), found, default).unwrap();
        assert_eq!(given, ResolvedRoot { root: given_root.clone(), source: RootSource::Given });

        let existing = choose_root(None, found, default).unwrap();
        assert_eq!(existing, ResolvedRoot { root: "/found".into(), source: RootSource::Found });

        let fresh = choose_root(None, nothing, default).unwrap();
        assert_eq!(fresh, ResolvedRoot { root: "/default".into(), source: RootSource::Default });
    }

    #[test]
    fn a_relative_given_root_is_made_absolute_against_the_working_directory() {
        let resolved =
            choose_root(Some(Path::new("apps")), || None, || Ok(PathBuf::from("/default")))
                .unwrap();
        assert!(resolved.root.is_absolute(), "{}", resolved.root.display());
        assert_eq!(resolved.root, std::env::current_dir().unwrap().join("apps"));
    }

    #[test]
    fn a_given_root_is_used_without_looking_anywhere_else() {
        // Looking reads the user's real entries; a given root must not.
        let resolved = choose_root(
            Some(Path::new("/given")),
            || panic!("looked for an existing installation"),
            || panic!("resolved the default"),
        );
        assert_eq!(resolved.unwrap().source, RootSource::Given);
    }

    #[test]
    fn an_ordinary_entry_name_joins_under_the_root() {
        let root = Path::new("/tmp/unpack");
        assert_eq!(safe_join(root, "bin/xpack-launcher").unwrap(), root.join("bin/xpack-launcher"));
        assert_eq!(safe_join(root, PLAN_ENTRY).unwrap(), root.join(PLAN_ENTRY));
    }

    #[test]
    fn a_traversing_entry_name_is_refused() {
        // The payload archive carries no signature of its own, and unpacking
        // happens before anything can be verified.
        let root = Path::new("/tmp/unpack");
        for name in ["../escape", "bin/../../escape", "a/./b", "..", "bin//x"] {
            assert!(safe_join(root, name).is_err(), "{name:?} was accepted");
        }
    }

    #[test]
    fn an_absolute_entry_name_is_refused() {
        let root = Path::new("/tmp/unpack");
        assert!(safe_join(root, "/etc/passwd").is_err());
        assert!(safe_join(root, "").is_err());
    }

    #[test]
    fn a_backslash_separated_name_is_refused_when_it_traverses() {
        // Windows separators are normalised first, so `..\..\x` cannot slip
        // past a check that only looked for forward slashes.
        let root = Path::new("/tmp/unpack");
        assert!(safe_join(root, r"..\..\escape").is_err());
        assert_eq!(safe_join(root, r"bin\app").unwrap(), root.join("bin/app"));
    }

    #[test]
    fn a_drive_qualified_component_is_refused() {
        assert!(safe_join(Path::new("/tmp/unpack"), "C:evil").is_err());
    }

    /// An installation directory with the named launchers present.
    fn installation(launchers: &[&str]) -> (tempfile::TempDir, InstallPaths) {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let paths = InstallPaths::from_application_dir(dir.path());
        let names = xpack_core::BinaryNames::Xpack;

        for launcher in launchers {
            let path = match *launcher {
                "console" => paths.launcher_file_named(&names),
                "windowed" => paths.gui_launcher_file_named(&names),
                other => panic!("unknown launcher {other}"),
            };
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("the directory");
            std::fs::write(&path, b"a launcher").expect("the launcher");
        }
        (dir, paths)
    }

    #[test]
    fn an_installation_without_a_windowed_launcher_is_told_about_the_one_it_has() {
        // A payload carrying no windowed build produces an installation with
        // only a console launcher. Naming the windowed one anyway would print
        // a path that is not there, and the user's next action is to run what
        // was printed.
        let (_dir, paths) = installation(&["console"]);
        let names = xpack_core::BinaryNames::Xpack;

        assert_eq!(launcher_to_report(&paths, &names), paths.launcher_file_named(&names));
    }

    #[test]
    fn a_windowed_launcher_is_preferred_wherever_the_platform_prefers_it() {
        // Which build that is differs by platform, so the expectation is asked
        // of the same function the shortcut writer asks.
        let (_dir, paths) = installation(&["console", "windowed"]);
        let names = xpack_core::BinaryNames::Xpack;

        assert_eq!(launcher_to_report(&paths, &names), paths.shortcut_target_named(&names));
    }

    #[test]
    fn an_installation_with_no_launcher_at_all_names_the_ordinary_one() {
        // Nothing exists to report. The console launcher is the honest answer:
        // it is where one would be if the payload had carried it.
        let (_dir, paths) = installation(&[]);
        let names = xpack_core::BinaryNames::Xpack;

        assert_eq!(launcher_to_report(&paths, &names), paths.launcher_file_named(&names));
    }
}
