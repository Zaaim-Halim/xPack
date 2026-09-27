//! Where a machine-wide installation and its entries go, and whether a place
//! is safe to put one.
//!
//! Everything here is read by an installer running with administrator rights,
//! on behalf of a user who does not have them. So nothing is taken from the
//! environment or a home directory, which that user controls: the locations
//! are fixed per platform, and on Windows they come from the machine's own
//! registry hive, which only an administrator can change.
//!
//! | | Installation | Menu entry | Command | Desktop |
//! | --- | --- | --- | --- | --- |
//! | Windows | `Program Files\<Name>` | all users' Start Menu | machine `PATH` | the public desktop |
//! | macOS | `/Library/Application Support/<Name>` | `/Applications` | `/usr/local/bin` | none |
//! | Linux | `/opt/<name>` | `/usr/share/applications` | `/usr/local/bin` | none |
//!
//! The installation is named after the application, as other programs there
//! are, not after its id: a person looking in `Program Files` should find
//! "Expense Tracker". Its id is in its state.

use std::path::{Path, PathBuf};

use xpack_core::{Error, InstallScope, Result};

use super::Roots;

/// The directory machine-wide installations go in, each in a directory of
/// its own below it.
pub(super) fn install_root() -> Result<PathBuf> {
    platform::install_root()
}

/// The directory a machine-wide installation of the application named
/// `name`, with id `application_id`, is in or goes in.
///
/// Where its menu entry says, when it has one that leads to an installation
/// of this application: a later version may have been renamed, and must
/// still find the directory the first one was put in. Otherwise a directory
/// named after the application.
pub fn application_dir(application_id: &str, name: &str) -> Result<PathBuf> {
    xpack_core::manifest::validate_application_id(application_id)?;
    if let Some(recorded) =
        roots().and_then(|roots| super::recorded_application_dir(application_id, name, &roots))
        && installation_of(&recorded, application_id)
    {
        return Ok(recorded);
    }
    Ok(install_root()?.join(directory_name(name, application_id)))
}

/// Whether `dir` holds an installation of `application_id`.
fn installation_of(dir: &Path, application_id: &str) -> bool {
    xpack_core::store::load::<xpack_core::InstallState>(
        &xpack_core::InstallPaths::from_application_dir(dir).state_file(),
    )
    .is_ok_and(|state| state.value.application_id == application_id)
}

/// The directory name for the application named `name`: that name, made safe
/// for a file name, and lowercase with hyphens on Linux, as `/opt` is. The id
/// when the name has nothing to be recognised by, rather than a stand-in that
/// two applications could share.
fn directory_name(name: &str, application_id: &str) -> String {
    if !name.chars().any(char::is_alphanumeric) {
        return application_id.to_string();
    }
    let safe = xpack_core::safe_file_name(name);
    let named = if cfg!(any(windows, target_os = "macos")) {
        safe
    } else {
        safe.to_lowercase()
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_'))
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("-")
    };
    if named.is_empty() || named.chars().all(|c| c == '.') {
        application_id.to_string()
    } else {
        named
    }
}

/// Every user's desktop entries.
pub(super) fn roots() -> Option<Roots> {
    platform::roots()
}

/// Refuses an installation directory another user could tamper with.
///
/// A machine-wide installation runs for every user, so whoever can change its
/// files can run code as every user who opens it. That is safe only when every
/// directory from the root of the disk down to the installation is writable by
/// an administrator alone. Called before an elevated installer writes anything.
///
/// On macOS and Linux: each existing directory on the way, after resolving
/// links, must belong to root and be writable by nobody else. On Windows,
/// where that question needs the directory's access list: the installation
/// must be inside `Program Files`, which only administrators can write to.
pub fn ensure_safe_root(root: &Path) -> Result<()> {
    platform::ensure_safe_root(root)
}

/// What an elevated installer does before it writes anything for every user:
/// returns the installation to install `application_id`, named `name`, into.
///
/// Refuses unless this process has administrator rights, and unless the
/// directory is one only an administrator can change. Makes every file it
/// creates from here on unwritable by anyone else. Takes nothing from the
/// environment: the directory comes from the system's own locations and the
/// application's menu entry, never from `XPACK_INSTALL_ROOT` or a home
/// directory, which the person who asked for the rights may have set.
pub fn prepare(application_id: &str, name: &str) -> Result<xpack_core::InstallPaths> {
    if !xpack_platform::is_elevated() {
        return Err(Error::invalid(
            "installation",
            "installing for every user needs administrator rights",
        ));
    }
    xpack_platform::restrict_new_files();
    let dir = application_dir(application_id, name)?;
    ensure_safe_root(&dir)?;
    ensure_ours_or_new(&dir, application_id)?;
    xpack_core::InstallPaths::named(dir, application_id)
}

/// Refuses a directory that belongs to some other program.
///
/// The directory is named after the application, as other programs' are, so
/// it can be one of theirs: `/opt/google-chrome`, `Program Files\Git`.
/// Installing into it would write into that program as an administrator, and
/// uninstalling would delete parts of it. Only a directory that is not there
/// yet, is empty, or holds this application's own installation is used.
fn ensure_ours_or_new(dir: &Path, application_id: &str) -> Result<()> {
    let empty =
        |dir: &Path| std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_none());
    if !dir.exists() || empty(dir) || installation_of(dir, application_id) {
        return Ok(());
    }
    Err(Error::invalid(
        "installation",
        format!(
            "{} already exists and is not an installation of {application_id}; it may be \
             another program's, so nothing was written to it",
            dir.display()
        ),
    ))
}

/// Copies `package` into the installation, and returns the copy.
///
/// The package an elevated installer is given sits wherever the user put it,
/// which the user can change at any moment: between the signature check and
/// the unpacking, say. The copy is in a directory only an administrator can
/// write, so what is checked is what is unpacked. Verify and install from the
/// copy, never the original, and remove it afterwards.
pub fn bring_in(package: &Path, paths: &xpack_core::InstallPaths) -> Result<PathBuf> {
    let dir = paths.downloads_dir();
    xpack_core::atomic::create_dir_all(&dir)?;
    let copy = dir.join(format!("incoming-{}.xpkg", std::process::id()));
    std::fs::copy(package, &copy).map_err(|e| Error::io(package, e))?;
    Ok(copy)
}

/// The desktop entries a machine-wide installation's scope stands for.
fn machine(data: PathBuf, home: PathBuf, desktop: Option<PathBuf>) -> Roots {
    Roots { data, home, desktop, scope: InstallScope::Machine }
}

/// Refuses `root` unless an administrator alone can write every directory
/// above it.
#[cfg(unix)]
fn ensure_owned_by_root(root: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    // The deepest part that exists, with every link resolved: what is actually
    // on disk is what gets checked, not what the path spells.
    let mut existing = root;
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or_else(|| Error::invalid("installation", "no part of the path exists"))?;
    }
    let resolved = std::fs::canonicalize(existing).map_err(|e| Error::io(existing, e))?;

    for dir in resolved.ancestors() {
        let metadata = std::fs::metadata(dir).map_err(|e| Error::io(dir, e))?;
        let writable_by_others = metadata.mode() & 0o022 != 0;
        if metadata.uid() != 0 || writable_by_others {
            return Err(Error::invalid(
                "installation",
                format!(
                    "{} is not safe for an installation every user runs: {} {}",
                    root.display(),
                    dir.display(),
                    if metadata.uid() == 0 {
                        "can be written to by users other than root"
                    } else {
                        "does not belong to root"
                    }
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{PathBuf, Result, Roots, machine};

    #[allow(
        clippy::unnecessary_wraps,
        reason = "one signature on every platform, and Windows reads it from the registry"
    )]
    pub(super) fn install_root() -> Result<PathBuf> {
        Ok(PathBuf::from("/Library/Application Support"))
    }

    /// The bundle goes in `<home>/Applications`, so a home of `/` puts it in
    /// `/Applications`. Nothing goes on a desktop: there is no desktop every
    /// user shares.
    #[allow(
        clippy::unnecessary_wraps,
        reason = "one signature on every platform, and Windows reads it from the registry"
    )]
    pub(super) fn roots() -> Option<Roots> {
        Some(machine(PathBuf::from("/Library/Application Support"), PathBuf::from("/"), None))
    }

    pub(super) fn ensure_safe_root(root: &std::path::Path) -> Result<()> {
        super::ensure_owned_by_root(root)
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use super::{PathBuf, Result, Roots, machine};

    #[allow(
        clippy::unnecessary_wraps,
        reason = "one signature on every platform, and Windows reads it from the registry"
    )]
    pub(super) fn install_root() -> Result<PathBuf> {
        Ok(PathBuf::from("/opt"))
    }

    /// The menu entry goes in `<data>/applications`. Nothing goes on a
    /// desktop: there is no desktop every user shares.
    #[allow(
        clippy::unnecessary_wraps,
        reason = "one signature on every platform, and Windows reads it from the registry"
    )]
    pub(super) fn roots() -> Option<Roots> {
        Some(machine(PathBuf::from("/usr/share"), PathBuf::from("/"), None))
    }

    pub(super) fn ensure_safe_root(root: &std::path::Path) -> Result<()> {
        super::ensure_owned_by_root(root)
    }
}

#[cfg(windows)]
mod platform {
    use std::path::Path;

    use winreg::RegKey;
    use winreg::enums::HKEY_LOCAL_MACHINE;

    use super::{Error, PathBuf, Result, Roots, machine};

    /// Where Windows says programs go, from the machine's own registry.
    fn program_files() -> Result<PathBuf> {
        RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(r"SOFTWARE\Microsoft\Windows\CurrentVersion")
            .and_then(|key| key.get_value::<String, _>("ProgramFilesDir"))
            .map(PathBuf::from)
            .map_err(|e| Error::Unsupported(format!("finding Program Files: {e}")))
    }

    /// A folder every user shares, from the machine's own registry. `Shell
    /// Folders` holds the expanded paths, so no variable the user set is read.
    fn shared_folder(name: &str) -> Option<PathBuf> {
        let value = RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\Shell Folders")
            .and_then(|key| key.get_value::<String, _>(name))
            .ok()?;
        Some(PathBuf::from(value))
    }

    pub(super) fn install_root() -> Result<PathBuf> {
        program_files()
    }

    /// The Start Menu entry goes in `<data>\Microsoft\Windows\Start Menu`, so
    /// the data folder is the one `Common Programs` sits four levels under.
    pub(super) fn roots() -> Option<Roots> {
        let programs = shared_folder("Common Programs")?;
        let data = programs.ancestors().nth(4)?.to_path_buf();
        Some(machine(data, PathBuf::from(r"C:\"), shared_folder("Common Desktop")))
    }

    pub(super) fn ensure_safe_root(root: &Path) -> Result<()> {
        let program_files = program_files()?;
        // A `..` would climb back out of Program Files after the prefix
        // matched.
        let climbs = root.components().any(|c| matches!(c, std::path::Component::ParentDir));
        let inside = !climbs
            && root
                .components()
                .zip(program_files.components())
                .all(|(a, b)| a.as_os_str().eq_ignore_ascii_case(b.as_os_str()))
            && root.components().count() > program_files.components().count();
        if inside {
            Ok(())
        } else {
            Err(Error::invalid(
                "installation",
                format!(
                    "{} is not safe for an installation every user runs: it is not inside {}",
                    root.display(),
                    program_files.display()
                ),
            ))
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_directory_a_user_owns_is_refused() {
        // The attack: an installation every user runs, in a directory this
        // user can change. Temporary directories belong to the user.
        let dir = tempfile::tempdir().unwrap();
        let err = ensure_safe_root(&dir.path().join("com.example.app")).unwrap_err();
        assert!(err.to_string().contains("does not belong to root"), "{err}");
    }

    #[test]
    fn a_directory_everyone_can_write_is_refused_even_when_root_owns_it() {
        // `/tmp` belongs to root and anyone can create things in it.
        let err = ensure_safe_root(Path::new("/tmp/xpack-test/com.example.app")).unwrap_err();
        assert!(err.to_string().contains("other than root"), "{err}");
    }

    #[test]
    fn a_link_to_a_users_directory_is_judged_by_where_it_leads() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        assert!(ensure_safe_root(&link.join("com.example.app")).is_err());
    }

    #[test]
    fn a_directory_only_root_can_write_is_accepted() {
        // Nothing is created: the part that exists is what is checked.
        ensure_safe_root(Path::new("/usr/share/xpack-test-never-created/com.example.app")).unwrap();
    }

    #[test]
    fn a_machine_wide_installation_is_named_after_the_application() {
        let dir = application_dir("com.example.tracker", "Expense Tracker").unwrap();
        let expected =
            if cfg!(target_os = "macos") { "Expense Tracker" } else { "expense-tracker" };
        assert_eq!(dir, install_root().unwrap().join(expected));
    }

    #[test]
    fn a_name_with_nothing_usable_in_it_falls_back_to_the_id() {
        assert_eq!(directory_name("///", "com.example.app"), "com.example.app");
        assert_eq!(directory_name("..", "com.example.app"), "com.example.app");
    }

    #[test]
    fn an_install_for_every_user_without_administrator_rights_is_refused() {
        if xpack_platform::is_elevated() {
            return;
        }
        let err = prepare("com.example.app", "Example").unwrap_err();
        assert!(err.to_string().contains("administrator rights"), "{err}");
    }

    #[test]
    fn another_programs_directory_of_the_same_name_is_not_taken_over() {
        // The attack, or the accident: `/opt/<name>` already holds a program
        // that is not this one.
        let dir = tempfile::tempdir().unwrap();
        let theirs = dir.path().join("example");
        std::fs::create_dir_all(theirs.join("bin")).unwrap();
        std::fs::write(theirs.join("bin").join("example"), "someone else's").unwrap();

        let err = ensure_ours_or_new(&theirs, "com.example.app").unwrap_err();

        assert!(err.to_string().contains("another program"), "{err}");
    }

    #[test]
    fn a_new_an_empty_or_its_own_directory_is_used() {
        let dir = tempfile::tempdir().unwrap();
        ensure_ours_or_new(&dir.path().join("missing"), "com.example.app").unwrap();
        ensure_ours_or_new(dir.path(), "com.example.app").unwrap();

        let ours =
            xpack_core::InstallPaths::named(dir.path().join("mine"), "com.example.app").unwrap();
        std::fs::create_dir_all(ours.state_dir()).unwrap();
        xpack_core::InstallState::new("com.example.app").save(&ours.state_file()).unwrap();
        ensure_ours_or_new(ours.root(), "com.example.app").unwrap();
        assert!(ensure_ours_or_new(ours.root(), "com.example.other").is_err(), "another app's");
    }

    #[test]
    fn the_machine_locations_are_system_directories() {
        let roots = roots().unwrap();
        assert_eq!(roots.scope, InstallScope::Machine);
        assert!(roots.desktop.is_none(), "no shared desktop on this platform");
        let root = install_root().unwrap();
        assert!(root.is_absolute() && !root.starts_with("/home") && !root.starts_with("/Users"));
    }
}
