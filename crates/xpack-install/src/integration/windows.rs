//! Start-Menu shortcuts and the Add/Remove Programs entry.
//!
//! Two separate things, both per-user:
//!
//! * A `.lnk` in the user's own Start-Menu `Programs` folder, written by
//!   [`super::lnk`] rather than through COM.
//! * A key under `HKEY_CURRENT_USER\…\Uninstall`, which is what puts the
//!   application in *Settings → Apps → Installed apps* with a working
//!   **Uninstall** button.
//!
//! # Why `HKCU` and not `HKLM`
//!
//! The machine-wide key is the one most installers write, and it needs
//! administrator rights. A per-user installation that asked for elevation just
//! to list itself would give away the property that makes the whole design
//! work — no UAC prompt anywhere in the install or update path. Windows reads
//! the per-user key for the user who owns it, which is exactly the right
//! scope: this application was installed for them and for nobody else.
//!
//! # Removal is by recomputation
//!
//! Nothing records what was written. The shortcut path is derived from the
//! display name and the key name from the application id, so removal rebuilds
//! both. A stale key left behind is the classic uninstaller failure — an entry
//! in the list whose **Uninstall** button runs a program that no longer exists
//! — and deriving the path is what makes it impossible here.

use std::path::PathBuf;

use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_WRITE};

use super::{Entry, Outcome, Roots, lnk, remove_paths};

/// The parent of every per-user uninstall entry.
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";

/// Writes the shortcut and the uninstall entry.
pub(super) fn install(entry: &Entry, roots: &Roots) -> Outcome {
    let path = shortcut_path(entry, roots);

    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return Outcome::Failed(format!("{}: {e}", parent.display()));
    }

    let shortcut = lnk::Shortcut::new(&entry.target)
        .with_icon(entry.icon.as_deref())
        .with_description(entry.description.as_deref());

    if let Err(e) = xpack_core::atomic::write(&path, &shortcut.to_bytes()) {
        return Outcome::Failed(e.to_string());
    }

    // Best effort, and deliberately after the shortcut: a missing entry in the
    // installed-apps list is a smaller loss than no way to start the program,
    // so the shortcut is never held up by the registry.
    if let Err(error) = write_uninstall_entry(entry) {
        tracing::warn!(%error, "could not register the application for removal");
    }

    Outcome::Done(vec![path])
}

/// Removes the shortcut and the uninstall entry.
pub(super) fn remove(entry: &Entry, roots: &Roots) -> Outcome {
    if let Err(error) = delete_uninstall_entry(entry) {
        tracing::warn!(%error, "could not remove the uninstall registration");
    }

    remove_paths(vec![shortcut_path(entry, roots)])
}

/// `%APPDATA%\Microsoft\Windows\Start Menu\Programs\<Name>.lnk`.
///
/// Named after the display name, not the application id: this one is shown to
/// the user, and a reverse-DNS string in a Start Menu reads as an error.
fn shortcut_path(entry: &Entry, roots: &Roots) -> PathBuf {
    roots
        .data
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join(format!("{}.lnk", shortcut_file_name(&entry.name)))
}

/// Makes a display name safe to use as a file name.
///
/// The same rule the rest of xPack names files by, rather than a second
/// version of it living here. This module had its own, and the two had drifted
/// apart: a name made entirely of separators became `---.lnk` in a user's
/// Start Menu instead of falling back to a readable name, and neither the
/// reserved device names nor the length limit were applied to a shortcut at
/// all.
///
/// Two sanitisers for one question is how that happens. There is now one, and
/// it is the one with tests that run on every platform.
pub(super) fn shortcut_file_name(name: &str) -> String {
    xpack_core::safe_file_name(name)
}

/// Writes the per-user Add/Remove Programs entry.
fn write_uninstall_entry(entry: &Entry) -> std::io::Result<()> {
    let root = RegKey::predef(HKEY_CURRENT_USER);
    let path = format!(r"{UNINSTALL_KEY}\{}", entry.application_id);
    let (key, _) = root.create_subkey_with_flags(&path, KEY_WRITE)?;

    key.set_value("DisplayName", &entry.name)?;
    key.set_value("DisplayVersion", &entry.version.to_string())?;
    key.set_value("InstallLocation", &entry.root.to_string_lossy().into_owned())?;

    if let Some(publisher) = &entry.publisher {
        key.set_value("Publisher", publisher)?;
    }
    if let Some(icon) = &entry.icon {
        key.set_value("DisplayIcon", &icon.to_string_lossy().into_owned())?;
    }
    if let Some(uninstaller) = &entry.uninstaller {
        // Quoted because the path contains the application id under a user
        // profile directory, and Windows splits an unquoted command on spaces.
        key.set_value("UninstallString", &format!("\"{}\" --yes", uninstaller.to_string_lossy()))?;
    }

    // xPack has no repair or modify path, and an enabled button that does
    // nothing is worse than an absent one.
    key.set_value("NoModify", &1u32)?;
    key.set_value("NoRepair", &1u32)?;

    // Reported in kilobytes, which is the unit this value is defined in.
    if let Some(size) = installation_size(&entry.root) {
        key.set_value("EstimatedSize", &size)?;
    }

    Ok(())
}

/// Deletes the per-user Add/Remove Programs entry.
///
/// A key that is already gone is not an error: an uninstall run twice, or one
/// following a partial earlier attempt, must still report success.
fn delete_uninstall_entry(entry: &Entry) -> std::io::Result<()> {
    let root = RegKey::predef(HKEY_CURRENT_USER);
    let path = format!(r"{UNINSTALL_KEY}\{}", entry.application_id);
    match root.delete_subkey_all(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Total size of the installation, in kilobytes.
///
/// Advisory: it is what the installed-apps list displays, and nothing depends
/// on it, so a directory that cannot be walked yields no value rather than an
/// error.
fn installation_size(root: &std::path::Path) -> Option<u32> {
    fn walk(dir: &std::path::Path, total: &mut u64) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                walk(&entry.path(), total);
            } else {
                *total += metadata.len();
            }
        }
    }

    let mut total = 0u64;
    walk(root, &mut total);
    u32::try_from(total / 1024).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_separator_in_a_display_name_cannot_escape_the_start_menu_folder() {
        assert_eq!(shortcut_file_name(r"Acme\..\Startup\evil"), "Acme-..-Startup-evil");
        assert_eq!(shortcut_file_name("a/b"), "a-b");
    }

    #[test]
    fn characters_windows_cannot_represent_are_replaced() {
        assert_eq!(shortcut_file_name(r#"a<b>c:d"e|f?g*h"#), "a-b-c-d-e-f-g-h");
    }

    #[test]
    fn a_trailing_dot_or_space_is_stripped_because_windows_strips_it_anyway() {
        // Otherwise the file written and the file looked for disagree.
        assert_eq!(shortcut_file_name("App ."), "App");
        assert_eq!(shortcut_file_name("App   "), "App");
    }

    #[test]
    fn a_name_that_sanitises_to_nothing_still_produces_a_shortcut() {
        assert_eq!(shortcut_file_name("///"), "Application");
    }

    #[test]
    fn an_ordinary_name_is_left_alone() {
        assert_eq!(shortcut_file_name("Example App"), "Example App");
    }
}
