//! A shortcut on the user's desktop, beside the menu entry.
//!
//! What it is differs per platform, as the menu entry does: a `.lnk` on
//! Windows, a `.desktop` file on Linux, and on macOS a link to the bundle the
//! menu entry made, which Finder opens as the application itself. It exists
//! only beside a menu entry, so on macOS there is always a bundle to link to.
//!
//! # The one path that is recorded, not derived
//!
//! Every other entry's path is worked out again from the application, so
//! removing it needs no record. A desktop cannot be treated that way: where it
//! is belongs to the user and changes. Windows moves it into a synced folder,
//! a Linux desktop follows the language the session was set up in. A shortcut
//! looked for on the desktop as it is at uninstall time would be missed on the
//! desktop it was written to. So the file written is recorded, and only that
//! file is ever refreshed or removed.
//!
//! # Chosen once
//!
//! Made on a first installation that asks for it, and never by an update: an
//! installation made before this existed has an uninstaller that knows nothing
//! of desktop shortcuts, and a shortcut it cannot remove is worse than none.
//! An update refreshes a recorded shortcut that is still there, and forgets
//! one the user deleted rather than putting it back.

use std::path::{Path, PathBuf};

use xpack_core::InstallPaths;

use super::{Entry, Outcome, Roots};

/// What the record holds: the shortcut that was written.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    format_version: u32,
    path: PathBuf,
}

/// Puts a shortcut on the desktop and records it.
pub(crate) fn create(entry: &Entry, roots: &Roots, paths: &InstallPaths) -> Outcome {
    let Some(desktop) = &roots.desktop else {
        return Outcome::Unsupported("this user has no desktop folder".to_string());
    };
    let path = desktop.join(file_name(entry));
    // Something the user put there, under the same name, is theirs: replaced
    // and recorded, an uninstall would delete it. Left as it is, and nothing
    // is recorded.
    if std::fs::symlink_metadata(&path).is_ok() {
        return Outcome::Failed(format!(
            "{} is already on the desktop; left as it is",
            path.display()
        ));
    }
    if let Err(e) = std::fs::create_dir_all(desktop) {
        return Outcome::Failed(format!("{}: {e}", desktop.display()));
    }
    // Recorded before it is written: a shortcut that exists unrecorded is one
    // no uninstall could find, while a record of a shortcut that failed to
    // appear only names a file that is not there.
    let record = Record { format_version: 1, path: path.clone() };
    if let Err(e) = xpack_core::atomic::write_json(&paths.desktop_shortcut_file(), &record) {
        return Outcome::Failed(e.to_string());
    }
    match write_at(entry, roots, &path) {
        Ok(()) => Outcome::Done(vec![path]),
        Err(reason) => Outcome::Failed(reason),
    }
}

/// Brings a recorded shortcut up to date, when it is still on the desktop.
///
/// A shortcut the user deleted stays deleted: its record goes, so neither this
/// nor a later update brings it back.
pub(crate) fn refresh(entry: &Entry, roots: &Roots, paths: &InstallPaths) -> Outcome {
    let Some(record) = read(paths) else {
        return Outcome::NothingToDo;
    };
    if std::fs::symlink_metadata(&record.path).is_err() {
        tracing::info!(path = %record.path.display(), "the desktop shortcut was deleted; it stays deleted");
        return match xpack_core::atomic::remove_file_if_exists(&paths.desktop_shortcut_file()) {
            Ok(()) => Outcome::NothingToDo,
            Err(e) => Outcome::Failed(e.to_string()),
        };
    }
    match write_at(entry, roots, &record.path) {
        Ok(()) => Outcome::Done(vec![record.path]),
        Err(reason) => Outcome::Failed(reason),
    }
}

/// Removes the recorded shortcut, and only that.
///
/// The record stays for whoever removes the state directory. A directory where
/// the shortcut was is not a shortcut, and is left alone.
pub(crate) fn remove(paths: &InstallPaths) -> Outcome {
    let Some(record) = read(paths) else {
        return Outcome::NothingToDo;
    };
    match std::fs::symlink_metadata(&record.path) {
        Ok(metadata) if metadata.is_dir() => {
            tracing::warn!(path = %record.path.display(), "a directory is where the desktop shortcut was; left alone");
            Outcome::NothingToDo
        }
        Ok(_) => match std::fs::remove_file(&record.path) {
            Ok(()) => Outcome::Done(vec![record.path]),
            Err(e) => Outcome::Failed(format!("{}: {e}", record.path.display())),
        },
        Err(_) => Outcome::NothingToDo,
    }
}

/// The recorded shortcut, if there is a readable record.
fn read(paths: &InstallPaths) -> Option<Record> {
    let file = paths.desktop_shortcut_file();
    if !file.exists() {
        return None;
    }
    match xpack_core::atomic::read_json::<Record>(&file) {
        Ok(record) => Some(record),
        Err(error) => {
            // Nothing is guessed: an unreadable record names no file, and
            // deleting whatever a guess pointed at could be the user's.
            tracing::warn!(%error, file = %file.display(), "unreadable desktop shortcut record; ignored");
            None
        }
    }
}

/// The shortcut's file name on this platform.
#[cfg(windows)]
fn file_name(entry: &Entry) -> String {
    format!("{}.lnk", super::windows::shortcut_file_name(&entry.name))
}

/// The shortcut's file name on this platform.
#[cfg(target_os = "macos")]
fn file_name(entry: &Entry) -> String {
    format!("{}.app", super::macos::bundle_directory_name(&entry.name))
}

/// The shortcut's file name on this platform.
///
/// The application id, as the menu entry's is, for the same reason: it is the
/// one name no other application shares.
#[cfg(not(any(windows, target_os = "macos")))]
fn file_name(entry: &Entry) -> String {
    format!("{}.desktop", entry.application_id)
}

/// Writes the shortcut at `path`: the same as the Start-Menu one.
#[cfg(windows)]
fn write_at(entry: &Entry, _roots: &Roots, path: &Path) -> Result<(), String> {
    let shortcut = super::lnk::Shortcut::new(&entry.target)
        .with_icon(entry.icon.as_deref())
        .with_description(entry.description.as_deref());
    xpack_core::atomic::write(path, &shortcut.to_bytes()).map_err(|e| e.to_string())
}

/// Links `path` to the bundle in `~/Applications`, which Finder then opens as
/// the application.
///
/// A link and not a second bundle: one bundle is what Launch Services indexes,
/// and two would show the application twice.
#[cfg(target_os = "macos")]
fn write_at(entry: &Entry, roots: &Roots, path: &Path) -> Result<(), String> {
    let bundle = super::macos::bundle_path(entry, roots);
    if !bundle.is_dir() {
        return Err(format!("{} is not there to link to", bundle.display()));
    }
    // Replaced, never followed: removing the link itself, not what it names.
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            return Err(format!("{} is a folder", path.display()));
        }
        Ok(_) => std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?,
        Err(_) => {}
    }
    std::os::unix::fs::symlink(&bundle, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Writes the menu entry's `.desktop` file to the desktop, runnable.
///
/// Desktops only start a launcher file that is executable, and GNOME also
/// wants it marked trusted, which only `gio` can do. Asked for, and not
/// relied on: without it the shortcut is there, and GNOME asks once before
/// starting it.
#[cfg(not(any(windows, target_os = "macos")))]
fn write_at(entry: &Entry, _roots: &Roots, path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    xpack_core::atomic::write(path, super::linux::render(entry).as_bytes())
        .map_err(|e| e.to_string())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let trusted = std::process::Command::new("gio")
        .arg("set")
        .arg(path)
        .args(["metadata::trusted", "true"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if !matches!(trusted, Ok(status) if status.success()) {
        tracing::debug!(path = %path.display(), "could not mark the desktop shortcut trusted");
    }
    Ok(())
}

/// The user's desktop folder, as Windows records it.
///
/// Read from the registry because the desktop is often not under the profile:
/// synchronising it with a cloud drive moves it, and a shortcut written to
/// `%USERPROFILE%\Desktop` would then land in a folder the user never sees.
/// The value is stored unexpanded (`%USERPROFILE%\…`).
#[cfg(windows)]
#[allow(
    clippy::unnecessary_wraps,
    reason = "one signature on every platform, and Linux can have no desktop"
)]
pub(crate) fn desktop_dir(home: &Path) -> Option<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let recorded = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders")
        .and_then(|key| key.get_value::<String, _>("Desktop"))
        .ok()
        .and_then(|value| expand_environment(&value, |name| std::env::var(name).ok()));
    Some(recorded.map_or_else(|| home.join("Desktop"), PathBuf::from))
}

/// The desktop folder: `~/Desktop`, which macOS never moves.
#[cfg(target_os = "macos")]
#[allow(
    clippy::unnecessary_wraps,
    reason = "one signature on every platform, and Linux can have no desktop"
)]
pub(crate) fn desktop_dir(home: &Path) -> Option<PathBuf> {
    Some(home.join("Desktop"))
}

/// The desktop folder, as the freedesktop user directories name it.
///
/// Its name follows the language the session was set up in (`~/Bureau`,
/// `~/Escritorio`), so it is read from `user-dirs.dirs` rather than assumed.
/// A desktop set to the home directory itself means there is none, and a
/// session with no such folder has none either: nothing is created there.
#[cfg(not(any(windows, target_os = "macos")))]
pub(crate) fn desktop_dir(home: &Path) -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let named = std::fs::read_to_string(config.join("user-dirs.dirs"))
        .ok()
        .and_then(|text| desktop_from_user_dirs(&text, home));
    let desktop = named.unwrap_or_else(|| home.join("Desktop"));
    (desktop != home && desktop.is_dir()).then_some(desktop)
}

/// `XDG_DESKTOP_DIR` from the text of a `user-dirs.dirs` file.
///
/// The format is shell assignments, of which only `"$HOME/…"` and absolute
/// paths are valid.
#[cfg(any(test, not(any(windows, target_os = "macos"))))]
fn desktop_from_user_dirs(text: &str, home: &Path) -> Option<PathBuf> {
    let line = text.lines().map(str::trim).find(|line| line.starts_with("XDG_DESKTOP_DIR="))?;
    let value = line.trim_start_matches("XDG_DESKTOP_DIR=").trim_matches('"');
    if value == "$HOME" || value == "$HOME/" {
        return Some(home.to_path_buf());
    }
    if let Some(rest) = value.strip_prefix("$HOME/") {
        return Some(home.join(rest));
    }
    value.starts_with('/').then(|| PathBuf::from(value))
}

/// Expands `%NAME%` references the way Windows does for a stored path.
///
/// A reference to a variable that is not set leaves the whole path unknown,
/// rather than a path with a hole in it.
#[cfg(any(test, windows))]
fn expand_environment(value: &str, lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    let mut expanded = String::new();
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        expanded.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let end = after.find('%')?;
        expanded.push_str(&lookup(&after[..end])?);
        rest = &after[end + 1..];
    }
    expanded.push_str(rest);
    Some(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The desktop found here is the one Windows itself reports, so a desktop
    /// moved into a synced folder is where the shortcut goes.
    #[cfg(windows)]
    #[test]
    fn the_windows_desktop_is_the_one_windows_reports() {
        let home = directories::BaseDirs::new().unwrap().home_dir().to_path_buf();
        let output = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Environment]::GetFolderPath('Desktop')",
            ])
            .output()
            .unwrap();
        let reported = String::from_utf8_lossy(&output.stdout).trim().to_string();
        assert!(!reported.is_empty(), "Windows reported no desktop");
        assert_eq!(desktop_dir(&home), Some(PathBuf::from(reported)));
    }

    #[test]
    fn a_windows_desktop_path_is_expanded_from_the_environment() {
        let lookup = |name: &str| (name == "USERPROFILE").then(|| r"C:\Users\ada".to_string());
        assert_eq!(
            expand_environment(r"%USERPROFILE%\OneDrive\Desktop", lookup).as_deref(),
            Some(r"C:\Users\ada\OneDrive\Desktop")
        );
        assert_eq!(expand_environment(r"D:\Desk", lookup).as_deref(), Some(r"D:\Desk"));
        assert_eq!(expand_environment(r"%NOPE%\Desktop", lookup), None, "never a path with a hole");
        assert_eq!(expand_environment(r"%USERPROFILE", lookup), None, "an unclosed reference");
    }

    #[test]
    fn the_linux_desktop_follows_the_user_directories() {
        let home = Path::new("/home/ada");
        let text = "# written by xdg-user-dirs-update\nXDG_DOWNLOAD_DIR=\"$HOME/Téléchargements\"\n\
                    XDG_DESKTOP_DIR=\"$HOME/Bureau\"\n";
        assert_eq!(desktop_from_user_dirs(text, home), Some(home.join("Bureau")));
        assert_eq!(
            desktop_from_user_dirs("XDG_DESKTOP_DIR=\"/data/desk\"", home),
            Some(PathBuf::from("/data/desk"))
        );
        assert_eq!(
            desktop_from_user_dirs("XDG_DESKTOP_DIR=\"$HOME/\"", home),
            Some(home.to_path_buf())
        );
        assert_eq!(desktop_from_user_dirs("XDG_DESKTOP_DIR=\"relative\"", home), None);
        assert_eq!(desktop_from_user_dirs("XDG_MUSIC_DIR=\"$HOME/Music\"", home), None);
    }
}
