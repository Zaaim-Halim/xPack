//! On-disk layout of an installation.
//!
//! The default root is **per-user**, not system-wide. That is a deliberate
//! security and reliability choice: a per-user install needs no UAC elevation
//! on Windows, no `sudo` on Unix, and no `SeCreateSymbolicLinkPrivilege` — so
//! the update path never has to prompt, and an unprivileged attacker gains
//! nothing by racing a privileged installer.
//!
//! ```text
//! <root>/<application-id>/
//! ├── config/trust.json     pinned signing keys for this installation
//! ├── versions/<version>/   extracted payloads, one directory per version
//! ├── state/state.json      authoritative active version + update phase
//! ├── state/update.lock     cross-process installation lock
//! ├── state/logs/           component logs
//! ├── downloads/            partial downloads, never trusted
//! └── staging/              half-extracted versions, deleted on recovery
//! ```

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::manifest::validate_application_id;
use crate::version::Version;

/// Resolved paths for a single installed application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPaths {
    root: PathBuf,
}

impl InstallPaths {
    /// Builds the layout for `application_id` under an explicit root.
    ///
    /// The id is re-validated here even though the manifest already checked
    /// it, because this constructor is also reachable from CLI arguments.
    pub fn new(install_root: &Path, application_id: &str) -> Result<Self> {
        validate_application_id(application_id)?;
        Ok(Self { root: install_root.join(application_id) })
    }

    /// Builds the layout under the platform's default per-user data directory.
    pub fn user_default(application_id: &str) -> Result<Self> {
        Self::new(&default_install_root()?, application_id)
    }

    /// Wraps an already-resolved application directory.
    pub fn from_application_dir(dir: impl Into<PathBuf>) -> Self {
        Self { root: dir.into() }
    }

    /// The application's root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The application id this layout belongs to.
    ///
    /// Taken from the final path component, which is how the layout is built.
    pub fn application_id(&self) -> Option<&str> {
        self.root.file_name().and_then(|n| n.to_str())
    }

    /// Directory holding every installed version.
    pub fn versions_dir(&self) -> PathBuf {
        self.root.join("versions")
    }

    /// Directory holding one specific version's payload.
    pub fn version_dir(&self, version: &Version) -> PathBuf {
        self.versions_dir().join(version.to_directory_name())
    }

    /// Directory inside a version holding xPack's own metadata.
    ///
    /// Payload files can never live here: the reserved name is rejected by
    /// manifest validation and by the archive entry validator.
    pub fn version_metadata_dir(&self, version: &Version) -> PathBuf {
        self.version_dir(version).join(xpack_core_metadata_dir())
    }

    /// The manifest a version was installed from.
    ///
    /// Kept byte-identical to the package's own, so the signature beside it
    /// still verifies and the version can serve as a differential update base.
    pub fn version_manifest_file(&self, version: &Version) -> PathBuf {
        self.version_metadata_dir(version).join(crate::manifest::MANIFEST_ENTRY)
    }

    /// The signature over [`Self::version_manifest_file`].
    pub fn version_signature_file(&self, version: &Version) -> PathBuf {
        self.version_metadata_dir(version).join(crate::manifest::SIGNATURE_ENTRY)
    }

    /// Directory holding mutable installation state.
    pub fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }

    /// The authoritative state document.
    pub fn state_file(&self) -> PathBuf {
        self.state_dir().join("state.json")
    }

    /// The user's recorded choice about a desktop entry, when they made one.
    ///
    /// A file of its own rather than a field in the state document. The
    /// updater in an installation is never replaced, and an older one saving
    /// state would drop a field it had never heard of; a file it never opens
    /// it cannot lose.
    pub fn desktop_preference_file(&self) -> PathBuf {
        self.state_dir().join("desktop.json")
    }

    /// Whether a version's files are present: its directory and the manifest
    /// it was installed from.
    ///
    /// State and the filesystem are separate sources of truth, and they can
    /// diverge: a user deletes a directory, a removal half-succeeds, a disk
    /// fails. Anything deciding whether a recorded version is really there
    /// asks this, so installing and looking can never disagree about it.
    pub fn has_version_files(&self, version: &Version) -> bool {
        self.version_dir(version).is_dir() && self.version_manifest_file(version).is_file()
    }

    /// The cross-process installation lock file.
    pub fn lock_file(&self) -> PathBuf {
        self.state_dir().join("update.lock")
    }

    /// The lock held for the duration of a download.
    ///
    /// Separate from [`Self::lock_file`] on purpose. The installation lock is
    /// released across a download so the application stays usable, which
    /// leaves nothing saying whether the downloader is still alive. This lock
    /// says so, and the operating system answers rather than a timestamp: a
    /// process that dies has its locks released immediately, with no clock to
    /// trust and no process id to mistake for a reused one.
    pub fn download_lock_file(&self) -> PathBuf {
        self.state_dir().join("download.lock")
    }

    /// Directory for component logs.
    pub fn logs_dir(&self) -> PathBuf {
        self.state_dir().join("logs")
    }

    /// Directory for installation configuration.
    pub fn config_dir(&self) -> PathBuf {
        self.root.join("config")
    }

    /// The pinned signing-key store.
    pub fn trust_file(&self) -> PathBuf {
        self.config_dir().join("trust.json")
    }

    /// Whoever owns this installation's answer about automatic updates.
    ///
    /// Beside the trust store rather than in installation state, because it is
    /// a decision somebody made rather than a record of what xPack did. State
    /// is xPack's to write; this file is the user's, and it survives every
    /// update because nothing in an update touches it.
    pub fn update_policy_file(&self) -> PathBuf {
        self.config_dir().join("updates.json")
    }

    /// Directory for in-progress downloads. Contents are never trusted.
    pub fn downloads_dir(&self) -> PathBuf {
        self.root.join("downloads")
    }

    /// Staging path for a version being extracted.
    ///
    /// Extraction happens here and the directory is renamed into place only
    /// once every hash has been verified, so a crash can never leave a
    /// half-written tree under `versions/` that looks complete.
    pub fn staging_dir(&self, version: &Version) -> PathBuf {
        self.root.join("staging").join(version.to_directory_name())
    }

    /// Parent of all staging directories.
    pub fn staging_root(&self) -> PathBuf {
        self.root.join("staging")
    }

    /// The derived `current` symlink/junction.
    ///
    /// Convenience only — shortcuts and external scripts can point at a stable
    /// path. [`crate::state::InstallState`] remains the authority; see that
    /// module for why.
    pub fn current_link(&self) -> PathBuf {
        self.root.join("current")
    }

    /// Locates an installation from the running executable's own position.
    ///
    /// Every binary xPack installs — the launcher, the updater, the
    /// uninstaller — lives in the installation root, so the directory
    /// containing the executable *is* the root. Deriving it rather than
    /// embedding it at build time means one prebuilt binary serves every
    /// application.
    ///
    /// The one exception is the command directory: see
    /// [`application_dir_of`](Self::application_dir_of).
    ///
    /// `XPACK_APPLICATION_DIR` overrides this, which is what the tests use and
    /// what an unusual deployment can fall back on.
    pub fn discover() -> Result<Self> {
        if let Some(dir) = std::env::var_os(APPLICATION_DIR_ENV) {
            return Ok(Self::from_application_dir(PathBuf::from(dir)));
        }
        let executable = std::env::current_exe().map_err(|e| {
            Error::invalid("installation", format!("cannot locate this binary: {e}"))
        })?;
        Ok(Self::from_application_dir(Self::application_dir_of(&executable)?))
    }

    /// The installation an executable at `executable` belongs to.
    ///
    /// Its own directory, except for a launcher in the installation's
    /// [command directory](Self::command_dir), which belongs to the directory
    /// above. That copy exists so a terminal can start the application by name
    /// on Windows, where the command is an executable in a directory on the
    /// `PATH` rather than a script.
    ///
    /// The exception applies only when the directory above really is an
    /// installation, holding its `state` directory. A launcher that happens
    /// to sit in some unrelated `bin` keeps the ordinary rule, and fails the
    /// ordinary way if that is not an installation either.
    pub fn application_dir_of(executable: &Path) -> Result<PathBuf> {
        let dir = crate::atomic::parent_dir(executable)?;
        if dir.file_name().is_some_and(|name| name == COMMAND_DIR)
            && let Some(parent) = dir.parent()
            && Self::from_application_dir(parent).state_dir().is_dir()
        {
            return Ok(parent.to_path_buf());
        }
        Ok(dir.to_path_buf())
    }

    /// Where the command that starts this application from a terminal lives
    /// on Windows: a directory of its own, added to the user's `PATH`.
    ///
    /// Not the installation root, because that also holds the uninstaller
    /// and the updater, which have no business being on the `PATH`.
    pub fn command_dir(&self) -> PathBuf {
        self.root.join(COMMAND_DIR)
    }

    /// The launcher binary that starts this application.
    ///
    /// It sits in the installation root because that is how the launcher
    /// identifies which application it belongs to: it resolves its own
    /// executable path and treats the containing directory as the root.
    /// Nothing is compiled into it, so one prebuilt binary serves every
    /// installation.
    ///
    /// The name is fixed rather than derived from the application's display
    /// name. A display name is free-form Unicode and turning one into a
    /// filename invites normalisation collisions, and nothing reads this name
    /// anyway — the launcher only cares where it is, not what it is called.
    pub fn launcher_file(&self) -> PathBuf {
        self.launcher_file_named(&crate::naming::BinaryNames::Xpack)
    }

    /// The console launcher, under the names this installation actually uses.
    pub fn launcher_file_named(&self, names: &crate::naming::BinaryNames) -> PathBuf {
        self.executable(&names.launcher())
    }

    /// The windowed launcher, for platforms that distinguish one.
    ///
    /// Windows fixes at link time whether an executable is a console program
    /// or a windowed one, and the choice cannot be made when the program runs.
    /// A console build opened from a shortcut has a console allocated for it,
    /// and an empty black window sits behind the user's application for as
    /// long as it runs.
    ///
    /// So two builds of the same program are installed there, differing in
    /// that one attribute, and shortcuts point at this one. Unix has no such
    /// distinction, and this file is not installed on it.
    pub fn gui_launcher_file(&self) -> PathBuf {
        self.gui_launcher_file_named(&crate::naming::BinaryNames::Xpack)
    }

    /// The windowed launcher, under the names this installation actually uses.
    pub fn gui_launcher_file_named(&self, names: &crate::naming::BinaryNames) -> PathBuf {
        self.executable(&names.windowed_launcher())
    }

    /// The launcher a desktop shortcut should point at.
    ///
    /// The windowed build where one exists, the ordinary launcher elsewhere.
    ///
    /// Written with `cfg!` rather than `#[cfg]` deliberately: both arms are
    /// compiled on every platform, so the Windows arm cannot rot unnoticed on
    /// a machine that never builds for Windows.
    pub fn shortcut_target(&self) -> PathBuf {
        self.shortcut_target_named(&crate::naming::BinaryNames::Xpack)
    }

    /// The shortcut target, under the names this installation actually uses.
    pub fn shortcut_target_named(&self, names: &crate::naming::BinaryNames) -> PathBuf {
        if HAS_WINDOWED_LAUNCHER {
            self.gui_launcher_file_named(names)
        } else {
            self.launcher_file_named(names)
        }
    }

    /// An executable in the installation root, named for this platform.
    fn executable(&self, stem: &str) -> PathBuf {
        self.root.join(format!("{stem}{}", std::env::consts::EXE_SUFFIX))
    }

    /// The background updater binary for this installation.
    ///
    /// Beside the launcher, and found the same way: it resolves its own
    /// location to learn which application it serves.
    pub fn updater_file(&self) -> PathBuf {
        self.updater_file_named(&crate::naming::BinaryNames::Xpack)
    }

    /// The updater, under the names this installation actually uses.
    pub fn updater_file_named(&self, names: &crate::naming::BinaryNames) -> PathBuf {
        self.executable(&names.updater())
    }

    /// Where a version signals that it started successfully.
    ///
    /// Per version, so a report from an older one can never be mistaken for a
    /// report from the version currently being judged.
    pub fn health_file(&self, version: &Version) -> PathBuf {
        self.state_dir().join(format!("started-{}.ok", version.to_directory_name()))
    }

    /// The uninstaller binary for this installation.
    ///
    /// Beside the launcher and the updater. It removes the directory it lives
    /// in, which is why it relocates itself before doing so.
    pub fn uninstaller_file(&self) -> PathBuf {
        self.uninstaller_file_named(&crate::naming::BinaryNames::Xpack)
    }

    /// The uninstaller, under the names this installation actually uses.
    pub fn uninstaller_file_named(&self, names: &crate::naming::BinaryNames) -> PathBuf {
        self.executable(&names.uninstaller())
    }

    /// The update notifier, under xPack's own names.
    pub fn notifier_file(&self) -> PathBuf {
        self.notifier_file_named(&crate::naming::BinaryNames::Xpack)
    }

    /// The update notifier, under the names this installation actually uses.
    ///
    /// Most installations have nothing at this path: a prompt is placed only
    /// where a publisher asked for one.
    pub fn notifier_file_named(&self, names: &crate::naming::BinaryNames) -> PathBuf {
        self.executable(&names.notifier())
    }

    /// Returns `true` when this looks like an initialised installation.
    pub fn is_installed(&self) -> bool {
        self.state_file().is_file()
    }
}

fn xpack_core_metadata_dir() -> &'static str {
    crate::manifest::RESERVED_METADATA_DIR
}

/// Whether this platform installs a separate windowed launcher.
pub const HAS_WINDOWED_LAUNCHER: bool = cfg!(windows);

/// The longest path Windows accepts where a long path cannot be used.
///
/// Rust's standard library rewrites long absolute paths into the `\\?\`
/// verbatim form before handing them to the file APIs, so reading, writing and
/// renaming a payload file work well past this limit. Two things escape that
/// rewrite, and both are things the launcher does:
///
/// * `CreateProcessW` does not accept a verbatim path for the executable.
/// * `SetCurrentDirectory` does not accept one either, and `std` strips the
///   prefix back off before setting a child's working directory for exactly
///   that reason.
///
/// So a package can extract perfectly and still be impossible to start. The
/// value is `MAX_PATH` less one for the terminating null the API counts.
pub const WINDOWS_MAX_PATH: usize = 259;

/// The path-length ceiling this platform imposes on launching, if any.
pub fn host_launch_path_limit() -> Option<usize> {
    if cfg!(windows) { Some(WINDOWS_MAX_PATH) } else { None }
}

/// Refuses a version whose launch paths would be too long to start.
///
/// Checked when a version is installed rather than when it is launched. Both
/// would be correct, and failing at install time is far kinder: the user
/// learns while they are watching, with the package still in front of them,
/// instead of discovering months later that an update they never saw arrive
/// cannot be opened.
///
/// # Why the limit is a parameter
///
/// The same reason [`install_root_from`] takes its override as one. A function
/// that read `cfg!(windows)` internally could only ever be tested on Windows,
/// so the branch that matters would ship unexercised on the machine this is
/// developed on. Passing the limit in makes the rule pure and testable
/// everywhere, and leaves [`host_launch_path_limit`] as the single untestable
/// lookup.
///
/// # What is not checked
///
/// Payload files the application opens for itself. xPack extracts them
/// through `std`, which handles long paths, and whether an application can
/// then read its own data files is a property of that application. Checking
/// them here would reject packages that work.
pub fn ensure_launch_paths_fit(
    version_dir: &Path,
    launch: &crate::manifest::LaunchSpec,
    limit: Option<usize>,
) -> Result<()> {
    let Some(limit) = limit else {
        return Ok(());
    };

    // A bare executable name is resolved on `PATH` by the operating system and
    // never becomes a path under the installation, so its length is not ours.
    if launch.is_bundled() {
        let executable = join_relative(version_dir, &launch.executable);
        check_one(&executable, limit, "launch.executable")?;
    }

    let working_directory = match &launch.working_directory {
        Some(relative) => join_relative(version_dir, relative),
        None => version_dir.to_path_buf(),
    };
    check_one(&working_directory, limit, "launch.workingDirectory")
}

/// Joins a manifest-relative path, which always uses `/`, onto a directory.
fn join_relative(base: &Path, relative: &str) -> PathBuf {
    let mut joined = base.to_path_buf();
    for component in relative.replace('\\', "/").split('/') {
        joined.push(component);
    }
    joined
}

fn check_one(path: &Path, limit: usize, what: &str) -> Result<()> {
    let length = path.as_os_str().len();
    if length <= limit {
        return Ok(());
    }
    Err(Error::invalid(
        what,
        format!(
            "resolves to a {length}-character path, and this platform cannot start a program \
             from one longer than {limit}: {}",
            path.display()
        ),
    ))
}

/// The default per-user root that holds every xPack application directory.
///
/// The name of an installation's [command directory](InstallPaths::command_dir).
pub const COMMAND_DIR: &str = "bin";

/// Honours `XPACK_INSTALL_ROOT`, which the test suite and system integrators
/// use to relocate installations without touching the user's real data.
pub fn default_install_root() -> Result<PathBuf> {
    install_root_from(std::env::var_os(INSTALL_ROOT_ENV).as_deref())
}

/// Name of the environment variable that relocates installations.
pub const INSTALL_ROOT_ENV: &str = "XPACK_INSTALL_ROOT";

/// Overrides the installation an installed binary decides it belongs to.
///
/// Used by [`InstallPaths::discover`]. The test suite relies on it, and so
/// does any deployment that cannot put the binaries in the root.
pub const APPLICATION_DIR_ENV: &str = "XPACK_APPLICATION_DIR";

/// Names the file an application creates to say it started successfully.
///
/// Set by the launcher in every application's environment. A file rather than
/// a socket or a pipe because xPack is runtime-independent: creating one is a
/// line of code in any language, needs no xPack library, and works identically
/// on every platform — where a named pipe on Windows would need Win32 calls
/// this workspace does not permit.
pub const HEALTH_FILE_ENV: &str = "XPACK_HEALTH_FILE";

/// Resolves the install root from an explicit override, or the user default.
///
/// The environment read is kept out of this function on purpose. Since Rust
/// 2024 `std::env::set_var` is `unsafe`, and this workspace forbids unsafe
/// code, so a function that reads the environment directly cannot be tested at
/// all — its override and rejection branches would ship unexercised. Taking the
/// value as an argument makes the logic pure and fully testable, and leaves the
/// untestable part a single unconditional lookup.
pub fn install_root_from(overridden: Option<&OsStr>) -> Result<PathBuf> {
    if let Some(value) = overridden {
        if value.is_empty() {
            return Err(Error::invalid(INSTALL_ROOT_ENV, "must not be empty"));
        }
        let path = PathBuf::from(value);
        if path.is_relative() {
            return Err(Error::invalid(
                INSTALL_ROOT_ENV,
                format!("{} must be an absolute path", path.display()),
            ));
        }
        return Ok(path);
    }

    let dirs = directories::BaseDirs::new().ok_or_else(|| {
        Error::Unsupported("locating a home directory for the current user".to_string())
    })?;
    Ok(dirs.data_local_dir().join("xpack"))
}

#[cfg(test)]
mod tests {
    mod named_binaries {
        use super::super::*;
        use crate::naming::BinaryNames;

        fn paths() -> InstallPaths {
            InstallPaths::new(std::path::Path::new("/r"), "com.example.app").unwrap()
        }

        #[test]
        fn the_unnamed_accessors_still_resolve_exactly_what_they_always_did() {
            // Existing installations depend on these bytes. This is the guard
            // that routing them through BinaryNames changed nothing.
            let p = paths();
            let suffix = std::env::consts::EXE_SUFFIX;

            assert_eq!(p.launcher_file(), p.root().join(format!("xpack-launcher{suffix}")));
            assert_eq!(p.gui_launcher_file(), p.root().join(format!("xpack-launcherw{suffix}")));
            assert_eq!(p.updater_file(), p.root().join(format!("xpack-updater{suffix}")));
            assert_eq!(p.uninstaller_file(), p.root().join(format!("xpack-uninstaller{suffix}")));
        }

        #[test]
        fn a_named_installation_puts_the_application_name_on_every_executable() {
            let p = paths();
            let names = BinaryNames::from_display_name("My App");
            let suffix = std::env::consts::EXE_SUFFIX;

            assert_eq!(
                p.updater_file_named(&names),
                p.root().join(format!("My App Updater{suffix}"))
            );
            assert_eq!(
                p.uninstaller_file_named(&names),
                p.root().join(format!("Uninstall My App{suffix}"))
            );
            assert_eq!(p.gui_launcher_file_named(&names), p.root().join(format!("My App{suffix}")));
        }

        #[test]
        fn every_named_executable_lands_inside_the_installation() {
            // A display name is publisher-controlled, so this is the property
            // that matters most: none of these may escape the root.
            let p = paths();
            for hostile in ["../../etc/cron.d/x", "/etc/passwd", "..", "C:\\Windows\\a"] {
                let names = BinaryNames::from_display_name(hostile);
                for file in [
                    p.launcher_file_named(&names),
                    p.gui_launcher_file_named(&names),
                    p.updater_file_named(&names),
                    p.uninstaller_file_named(&names),
                ] {
                    assert_eq!(
                        file.parent(),
                        Some(p.root()),
                        "{hostile:?} escaped the installation as {}",
                        file.display()
                    );
                }
            }
        }

        #[test]
        fn the_shortcut_points_at_a_launcher_that_is_named_the_same_way() {
            let p = paths();
            let names = BinaryNames::from_display_name("My App");
            let target = p.shortcut_target_named(&names);

            let expected = if HAS_WINDOWED_LAUNCHER {
                p.gui_launcher_file_named(&names)
            } else {
                p.launcher_file_named(&names)
            };
            assert_eq!(target, expected);
        }
    }

    use super::*;

    #[test]
    fn a_launcher_belongs_to_the_directory_it_is_in() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("com.example.app");
        std::fs::create_dir_all(root.join("state")).unwrap();
        let found = InstallPaths::application_dir_of(&root.join("MyApp")).unwrap();
        assert_eq!(found, root);
    }

    #[test]
    fn the_command_copy_of_a_launcher_belongs_to_the_installation_above_it() {
        // The Windows command: `<root>/bin/mytool.exe`, on the PATH.
        let dir = tempfile::tempdir().unwrap();
        let paths = InstallPaths::from_application_dir(dir.path().join("com.example.app"));
        std::fs::create_dir_all(paths.state_dir()).unwrap();
        std::fs::create_dir_all(paths.command_dir()).unwrap();

        let copy = paths.command_dir().join("mytool.exe");
        assert_eq!(InstallPaths::application_dir_of(&copy).unwrap(), paths.root());
    }

    #[test]
    fn a_launcher_in_an_unrelated_bin_keeps_the_ordinary_rule() {
        // No installation above it: `bin` is just where it was put, and it is
        // not promoted to something it is not.
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("tools/bin");
        std::fs::create_dir_all(&bin).unwrap();
        assert_eq!(InstallPaths::application_dir_of(&bin.join("mytool")).unwrap(), bin);
    }

    fn paths() -> InstallPaths {
        InstallPaths::new(Path::new("/opt/xpack"), "com.example.app").unwrap()
    }

    #[test]
    fn nests_every_path_under_the_application_directory() {
        let p = paths();
        assert!(p.root().ends_with("com.example.app"));
        for path in [p.state_file(), p.trust_file(), p.downloads_dir(), p.lock_file()] {
            assert!(path.starts_with(p.root()), "{} escaped the root", path.display());
        }
    }

    #[test]
    fn refuses_to_build_paths_from_a_traversing_application_id() {
        assert!(InstallPaths::new(Path::new("/opt/xpack"), "../../etc").is_err());
        assert!(InstallPaths::new(Path::new("/opt/xpack"), "/absolute").is_err());
    }

    #[test]
    fn separates_staging_from_installed_versions() {
        let p = paths();
        let v = Version::parse("1.2.0").unwrap();
        assert_ne!(p.staging_dir(&v), p.version_dir(&v));
        assert!(!p.staging_dir(&v).starts_with(p.versions_dir()));
    }

    /// A path this platform calls absolute.
    ///
    /// `/opt/xpack-test` is absolute on Unix and *relative* on Windows, where
    /// a path becomes absolute only once it names a drive. A test that hard
    /// codes the Unix form is not testing the override, it is testing which
    /// operating system it is running on.
    fn an_absolute_path() -> &'static str {
        if cfg!(windows) { r"C:\xpack-test" } else { "/opt/xpack-test" }
    }

    #[test]
    fn an_override_relocates_the_install_root() {
        let expected = an_absolute_path();
        let root = install_root_from(Some(OsStr::new(expected))).unwrap();
        assert_eq!(root, Path::new(expected));
    }

    #[test]
    fn a_path_without_a_drive_is_not_absolute_on_windows() {
        // The reason the test above cannot name one path for both platforms,
        // stated as a test so it is checked rather than remembered.
        assert_eq!(Path::new("/opt/xpack-test").is_absolute(), !cfg!(windows));
        assert!(Path::new(an_absolute_path()).is_absolute());
    }

    #[test]
    fn an_empty_override_is_rejected_rather_than_silently_ignored() {
        // An empty value would otherwise resolve to the process working
        // directory, scattering installations wherever xpack happened to run.
        let err = install_root_from(Some(OsStr::new(""))).unwrap_err();
        assert!(err.to_string().contains("must not be empty"), "got {err}");
    }

    #[test]
    fn a_relative_override_is_rejected() {
        let err = install_root_from(Some(OsStr::new("relative/dir"))).unwrap_err();
        assert!(err.to_string().contains("absolute"), "got {err}");
    }

    #[test]
    fn without_an_override_the_user_data_directory_is_used() {
        let root = install_root_from(None).unwrap();
        assert!(root.is_absolute());
        assert!(root.ends_with("xpack"), "got {}", root.display());
    }

    #[test]
    fn an_installation_is_recognised_only_once_state_exists() {
        let dir = tempfile::tempdir().unwrap();
        let paths = InstallPaths::new(dir.path(), "com.example.app").unwrap();
        assert!(!paths.is_installed());

        std::fs::create_dir_all(paths.state_dir()).unwrap();
        std::fs::write(paths.state_file(), b"{}").unwrap();
        assert!(paths.is_installed());
    }

    #[test]
    fn an_existing_application_directory_can_be_wrapped_directly() {
        let paths = InstallPaths::from_application_dir("/opt/xpack/com.example.app");
        assert_eq!(paths.root(), Path::new("/opt/xpack/com.example.app"));
        assert!(paths.config_dir().starts_with(paths.root()));
        assert!(paths.logs_dir().starts_with(paths.state_dir()));
        assert!(paths.staging_root().starts_with(paths.root()));
        assert!(paths.current_link().starts_with(paths.root()));
    }

    fn launch_spec(
        executable: &str,
        working_directory: Option<&str>,
    ) -> crate::manifest::LaunchSpec {
        crate::manifest::LaunchSpec {
            executable: executable.to_string(),
            arguments: Vec::new(),
            working_directory: working_directory.map(str::to_string),
            keep_working_directory: false,
            environment: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn a_launch_path_within_the_limit_is_accepted() {
        let dir = Path::new("/opt/xpack/com.example.app/versions/1.0.0");
        let spec = launch_spec("bin/app", None);
        assert!(ensure_launch_paths_fit(dir, &spec, Some(WINDOWS_MAX_PATH)).is_ok());
    }

    #[test]
    fn a_bundled_executable_past_the_limit_is_refused() {
        // The package extracts perfectly and cannot be started: `CreateProcessW`
        // takes no verbatim path, so this has to fail while someone is watching.
        let dir = Path::new("/opt/xpack/com.example.app/versions/1.0.0");
        let spec = launch_spec(&format!("{}/app", "deep".repeat(80)), None);

        let err = ensure_launch_paths_fit(dir, &spec, Some(WINDOWS_MAX_PATH)).unwrap_err();
        assert!(err.to_string().contains("launch.executable"), "got {err}");
        assert!(err.to_string().contains("cannot start a program"), "got {err}");
    }

    #[test]
    fn a_working_directory_past_the_limit_is_refused() {
        let dir = Path::new("/opt/xpack/com.example.app/versions/1.0.0");
        let spec = launch_spec("app", Some(&"nested/".repeat(50)));

        let err = ensure_launch_paths_fit(dir, &spec, Some(WINDOWS_MAX_PATH)).unwrap_err();
        assert!(err.to_string().contains("workingDirectory"), "got {err}");
    }

    #[test]
    fn a_system_executable_is_not_measured_against_the_limit() {
        // A bare name is resolved on PATH by the operating system and never
        // becomes a path under the installation. The two cases below share a
        // version directory that only just fits, so the *only* difference
        // between passing and failing is whether the executable is bundled.
        let dir = PathBuf::from("/opt").join("a".repeat(WINDOWS_MAX_PATH - 5));
        assert_eq!(dir.as_os_str().len(), WINDOWS_MAX_PATH);

        let system = launch_spec("java", None);
        assert!(ensure_launch_paths_fit(&dir, &system, Some(WINDOWS_MAX_PATH)).is_ok());

        let bundled = launch_spec("bin/app", None);
        let err = ensure_launch_paths_fit(&dir, &bundled, Some(WINDOWS_MAX_PATH)).unwrap_err();
        assert!(err.to_string().contains("launch.executable"), "got {err}");
    }

    #[test]
    fn without_a_limit_nothing_is_refused() {
        // Unix has no such ceiling, and imposing one there would reject
        // packages that work perfectly.
        let dir = Path::new("/opt/xpack/com.example.app/versions/1.0.0");
        let spec = launch_spec(&format!("{}/app", "deep".repeat(200)), None);
        assert!(ensure_launch_paths_fit(dir, &spec, None).is_ok());
    }

    #[test]
    fn the_shortcut_points_at_a_launcher_this_platform_installs() {
        let p = paths();
        let target = p.shortcut_target();
        assert!(target.starts_with(p.root()));
        if HAS_WINDOWED_LAUNCHER {
            assert_eq!(target, p.gui_launcher_file());
        } else {
            assert_eq!(target, p.launcher_file());
        }
    }

    #[test]
    fn the_two_launcher_builds_are_separate_files() {
        let p = paths();
        assert_ne!(p.launcher_file(), p.gui_launcher_file());
    }

    #[test]
    fn version_directories_are_named_after_the_version() {
        let p = paths();
        assert!(p.version_dir(&Version::parse("1.2.0-rc.1").unwrap()).ends_with("1.2.0-rc.1"));
    }
}
