//! Making an installed application appear in the user's desktop environment.
//!
//! A Start-Menu shortcut, a freedesktop `.desktop` entry and a macOS
//! application bundle have almost nothing in common as files. What they have
//! in common is the intent, which the signed manifest records in
//! [`DesktopSpec`]: *this application should be openable from the desktop,
//! here is its icon, here is roughly what kind of program it is*. Each
//! platform below renders that intent in its own terms.
//!
//! [`DesktopSpec`]: xpack_core::DesktopSpec
//!
//! # This is the only thing xPack writes outside its own directory
//!
//! Everything else an installation does happens under the installation root.
//! These entries do not: they land in the user's menus, and on Windows in the
//! user's registry. That makes three rules non-negotiable.
//!
//! **Per-user, never system-wide.** The Start-Menu folder is the per-user one,
//! the registry key is under `HKCU`, the `.desktop` file is under
//! `XDG_DATA_HOME` and the bundle is in `~/Applications`. Nothing here needs
//! elevation, a UAC prompt or `sudo`, which is the same choice the install
//! root makes and for the same reasons.
//!
//! **Only when asked.** `desktop.shortcut` defaults to off. Writing into a
//! user's application menu because a package did not say not to would be
//! rude, and unpicking it later is exactly the kind of mess uninstallers are
//! infamous for.
//!
//! **Deterministic paths.** Nothing records what was created. Every path here
//! is derived from the application id and the installation root, so
//! [`remove`] recomputes precisely what [`install`] would have written. The
//! alternative — a list of created files in the state document — has a failure
//! mode this design does not: a state file lost or rolled back leaves entries
//! nobody can find to delete.
//!
//! # Failure here never fails an installation
//!
//! [`Outcome`] is deliberately not a `Result`, for the same reason
//! [`xpack_platform::LinkOutcome`] is not: a caller cannot propagate it with
//! `?` and accidentally fail a perfectly good install because a menu entry
//! could not be written. An application that installed correctly and is
//! missing from the menu is a nuisance. An application that refused to install
//! because of a menu entry is a bug.

use std::path::{Path, PathBuf};

use xpack_core::{DesktopSpec, InstallPaths, Manifest, Version};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

pub mod lnk;

/// What creating or removing a desktop entry did.
///
/// Not a `Result`. See the module documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The entry was created or removed.
    Done(Vec<PathBuf>),
    /// The manifest did not ask for a desktop entry.
    NotRequested,
    /// There was no entry to remove.
    NothingToDo,
    /// This platform has no desktop integration in this build.
    Unsupported(String),
    /// The entry could not be written; the installation is still valid.
    Failed(String),
}

impl Outcome {
    /// Returns `true` when the entry now exists, or is now gone.
    pub fn is_done(&self) -> bool {
        matches!(self, Self::Done(_))
    }

    /// Logs the outcome at a level matching its severity.
    pub fn log(&self, action: &str) {
        match self {
            Self::Done(paths) => {
                tracing::info!(action, count = paths.len(), "desktop integration updated");
            }
            Self::NotRequested => tracing::debug!(action, "no desktop entry was asked for"),
            Self::NothingToDo => tracing::debug!(action, "no desktop entry was present"),
            Self::Unsupported(reason) => {
                tracing::debug!(action, reason, "desktop integration is not supported here");
            }
            Self::Failed(reason) => tracing::warn!(
                action,
                reason,
                "desktop integration failed; the installation is unaffected"
            ),
        }
    }
}

/// Everything a platform needs to describe one application in a menu.
///
/// Resolved from the signed manifest and the installation layout, so that no
/// platform module has to reach back into either. Keeping this flat is what
/// lets the Windows code be reviewed on a Mac: it reads fields, it does not
/// re-derive them.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Reverse-DNS identifier. Used as the file and registry key name.
    pub application_id: String,
    /// Human-facing name, shown in the menu.
    pub name: String,
    /// One-line description, where the platform shows one.
    pub description: Option<String>,
    /// Publisher, where the platform shows one.
    pub publisher: Option<String>,
    /// Version of the application being installed.
    pub version: Version,
    /// The installation root.
    pub root: PathBuf,
    /// The launcher the entry should start.
    pub target: PathBuf,
    /// The icon, copied into the installation root, if there is one.
    pub icon: Option<PathBuf>,
    /// freedesktop categories, used on Linux and ignored elsewhere.
    pub categories: Vec<String>,
    /// Whether the application wants a terminal.
    pub terminal: bool,
    /// The uninstaller, where the platform can offer one.
    pub uninstaller: Option<PathBuf>,
}

impl Entry {
    /// Builds an entry from a manifest and the layout it was installed into.
    ///
    /// Returns `None` when the manifest asked for no desktop entry, so that
    /// "was one requested" is answered once, here, rather than by every
    /// platform module repeating the same check.
    ///
    /// # Which launcher it points at
    ///
    /// A terminal application is pointed at the console launcher and
    /// everything else at [`InstallPaths::shortcut_target`]. On Unix those are
    /// the same file. On Windows they are not, and the distinction is the
    /// whole reason two launcher builds are installed: a terminal program
    /// started by the windowed build would write into a console that does not
    /// exist.
    pub fn from_manifest(manifest: &Manifest, paths: &InstallPaths) -> Option<Self> {
        let desktop = &manifest.desktop;
        if !desktop.shortcut {
            return None;
        }

        let target = if desktop.terminal { paths.launcher_file() } else { paths.shortcut_target() };

        Some(Self {
            application_id: manifest.application.id.clone(),
            name: manifest.application.name.clone(),
            description: manifest.application.description.clone(),
            publisher: manifest.application.publisher.clone(),
            version: manifest.application.version.clone(),
            root: paths.root().to_path_buf(),
            target,
            icon: icon_destination(paths, desktop),
            categories: desktop.categories.clone(),
            terminal: desktop.terminal,
            uninstaller: Some(paths.uninstaller_file()),
        })
    }
}

/// Where the installation keeps its copy of the icon.
///
/// The icon ships inside a version directory, and a version directory is
/// replaced by the next update. A menu entry pointing into one would show a
/// missing image the first time the application updated, so the icon is copied
/// to a stable path in the installation root and the entry points there.
///
/// The extension is preserved because it is the only thing that tells a
/// desktop environment what the file is.
pub fn icon_destination(paths: &InstallPaths, desktop: &DesktopSpec) -> Option<PathBuf> {
    let icon = desktop.icon.as_ref()?;
    let extension = Path::new(icon).extension().and_then(|e| e.to_str()).unwrap_or("png");
    Some(paths.root().join(format!("icon.{extension}")))
}

/// Copies the manifest's icon out of the version directory into the root.
///
/// Best effort: a missing or unreadable icon produces an entry without one,
/// which is a cosmetic problem, not an installation failure.
pub fn place_icon(paths: &InstallPaths, manifest: &Manifest, version: &Version) -> Option<PathBuf> {
    let relative = manifest.desktop.icon.as_ref()?;
    let destination = icon_destination(paths, &manifest.desktop)?;

    let mut source = paths.version_dir(version);
    for component in relative.replace('\\', "/").split('/') {
        source.push(component);
    }

    match std::fs::read(&source) {
        Ok(bytes) => match xpack_core::atomic::write(&destination, &bytes) {
            Ok(()) => Some(destination),
            Err(error) => {
                tracing::warn!(%error, path = %destination.display(), "could not place the icon");
                None
            }
        },
        Err(error) => {
            tracing::warn!(%error, path = %source.display(), "could not read the icon");
            None
        }
    }
}

/// Where this platform's desktop entries live.
///
/// Passed in rather than looked up inside each platform module, for the same
/// reason [`xpack_core::paths::install_root_from`] takes its override as an
/// argument: since Rust 2024 `std::env::set_var` is unsafe and this workspace
/// forbids unsafe code, so a function that resolves the user's home directory
/// internally cannot be tested at all. It would write a real bundle into the
/// developer's own `~/Applications` every time the suite ran.
///
/// Taking the directories as a value makes every path decision testable
/// against a temporary directory, and leaves [`host_roots`] as the single
/// untestable lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    /// The user's data directory.
    ///
    /// `$XDG_DATA_HOME` on Linux, which holds `applications/`; `%APPDATA%` on
    /// Windows, which holds the Start Menu.
    pub data: PathBuf,
    /// The user's home directory, which on macOS holds `Applications/`.
    pub home: PathBuf,
}

/// Resolves the directories this user's desktop entries belong in.
pub fn host_roots() -> Option<Roots> {
    let dirs = directories::BaseDirs::new()?;
    Some(Roots { data: dirs.data_dir().to_path_buf(), home: dirs.home_dir().to_path_buf() })
}

/// Creates the desktop entry this platform uses.
pub fn install(entry: &Entry) -> Outcome {
    match host_roots() {
        Some(roots) => install_into(entry, &roots),
        None => Outcome::Failed("no home directory for the current user".to_string()),
    }
}

/// Creates the entry under an explicit set of directories.
pub fn install_into(entry: &Entry, roots: &Roots) -> Outcome {
    platform_install(entry, roots)
}

/// Removes the entry from under an explicit set of directories.
pub fn remove_from(entry: &Entry, roots: &Roots) -> Outcome {
    platform_remove(entry, roots)
}

/// Removes the desktop entry this platform uses.
///
/// Takes the same [`Entry`] shape as [`install`] because the Windows registry
/// key and the macOS bundle are both named after fields on it. Only the
/// identifying fields are read, so an uninstaller that no longer has the
/// manifest can supply a minimal one.
pub fn remove(entry: &Entry) -> Outcome {
    match host_roots() {
        Some(roots) => remove_from(entry, &roots),
        None => Outcome::NothingToDo,
    }
}

#[cfg(target_os = "linux")]
use linux::{install as platform_install, remove as platform_remove};
#[cfg(target_os = "macos")]
use macos::{install as platform_install, remove as platform_remove};
#[cfg(windows)]
use windows::{install as platform_install, remove as platform_remove};

/// Platforms with no desktop integration say so rather than failing.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn platform_install(entry: &Entry, roots: &Roots) -> Outcome {
    let _ = (entry, roots);
    Outcome::Unsupported(format!("{} has no desktop integration", std::env::consts::OS))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn platform_remove(entry: &Entry, roots: &Roots) -> Outcome {
    let _ = (entry, roots);
    Outcome::Unsupported(format!("{} has no desktop integration", std::env::consts::OS))
}

/// Deletes a set of paths, reporting what actually went.
///
/// Shared by all three platforms: each computes the paths it would have
/// written, and removal is then the same operation everywhere. A path that is
/// already gone is not an error — an uninstall run twice, or a user who
/// deleted the entry by hand, must still report success.
pub(crate) fn remove_paths(paths: Vec<PathBuf>) -> Outcome {
    let mut removed = Vec::new();
    for path in paths {
        let result = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match result {
            Ok(()) => removed.push(path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Outcome::Failed(format!("{}: {e}", path.display())),
        }
    }
    if removed.is_empty() { Outcome::NothingToDo } else { Outcome::Done(removed) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn manifest(shortcut: bool, icon: Option<&str>, terminal: bool) -> Manifest {
        Manifest {
            format_version: xpack_core::FormatVersion::default(),
            application: xpack_core::Application {
                id: "com.example.app".into(),
                name: "Example".into(),
                version: Version::parse("1.0.0").unwrap(),
                description: Some("An example".into()),
                publisher: Some("Example Ltd".into()),
            },
            platform: xpack_core::Platform::host().unwrap(),
            launch: xpack_core::LaunchSpec {
                executable: "bin/app".into(),
                arguments: Vec::new(),
                working_directory: None,
                environment: BTreeMap::new(),
            },
            update: xpack_core::UpdateSpec::default(),
            health: xpack_core::HealthSpec::default(),
            desktop: DesktopSpec {
                shortcut,
                icon: icon.map(str::to_string),
                categories: vec!["Utility".into()],
                terminal,
            },
            payload: xpack_core::PayloadSpec::default(),
            signing_key: None,
            created_at: None,
        }
    }

    fn paths() -> InstallPaths {
        InstallPaths::new(Path::new("/opt/xpack"), "com.example.app").unwrap()
    }

    #[test]
    fn a_manifest_that_asks_for_nothing_produces_no_entry() {
        assert!(Entry::from_manifest(&manifest(false, None, false), &paths()).is_none());
    }

    #[test]
    fn an_entry_carries_the_identity_a_menu_shows() {
        let entry = Entry::from_manifest(&manifest(true, None, false), &paths()).unwrap();
        assert_eq!(entry.name, "Example");
        assert_eq!(entry.publisher.as_deref(), Some("Example Ltd"));
        assert_eq!(entry.application_id, "com.example.app");
    }

    #[test]
    fn a_terminal_application_points_at_the_console_launcher() {
        // On Windows the windowed build has no console to write to, so a
        // terminal program started from it would produce no output at all.
        let entry = Entry::from_manifest(&manifest(true, None, true), &paths()).unwrap();
        assert_eq!(entry.target, paths().launcher_file());
    }

    #[test]
    fn a_windowed_application_points_at_the_shortcut_launcher() {
        let entry = Entry::from_manifest(&manifest(true, None, false), &paths()).unwrap();
        assert_eq!(entry.target, paths().shortcut_target());
    }

    #[test]
    fn the_icon_is_referenced_in_the_root_not_in_a_version_directory() {
        // A version directory is replaced by the next update. An entry
        // pointing into one shows a missing image the first time the
        // application updates.
        let entry =
            Entry::from_manifest(&manifest(true, Some("res/logo.png"), false), &paths()).unwrap();
        let icon = entry.icon.unwrap();
        assert_eq!(icon, paths().root().join("icon.png"));
        assert!(!icon.starts_with(paths().versions_dir()));
    }

    #[test]
    fn the_icon_extension_is_preserved_because_it_is_the_only_type_marker() {
        for (given, expected) in
            [("a/b.ico", "icon.ico"), ("x.icns", "icon.icns"), ("y.svg", "icon.svg")]
        {
            let entry =
                Entry::from_manifest(&manifest(true, Some(given), false), &paths()).unwrap();
            assert_eq!(entry.icon.unwrap(), paths().root().join(expected));
        }
    }

    #[test]
    fn removing_paths_that_are_already_gone_still_succeeds() {
        // An uninstall run twice, or a user who deleted the entry by hand.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(remove_paths(vec![dir.path().join("absent")]), Outcome::NothingToDo);
    }

    #[test]
    fn removing_reports_exactly_what_it_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("entry.desktop");
        std::fs::write(&present, b"x").unwrap();

        let outcome = remove_paths(vec![present.clone(), dir.path().join("absent")]);
        assert_eq!(outcome, Outcome::Done(vec![present.clone()]));
        assert!(!present.exists());
    }

    #[test]
    fn an_outcome_cannot_be_mistaken_for_a_failure_when_nothing_was_asked_for() {
        assert!(!Outcome::NotRequested.is_done());
        assert!(!Outcome::Failed("x".into()).is_done());
        assert!(Outcome::Done(Vec::new()).is_done());
    }
}
