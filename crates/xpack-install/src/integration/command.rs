//! Putting an application's command where a terminal looks.
//!
//! A package that names a [`CommandSpec`] asks for this: typing `mytool` in a
//! terminal starts the application, as its launcher would.
//!
//! # Per platform
//!
//! **macOS and Linux:** a script at `~/.local/bin/<name>` that runs the
//! launcher. A script rather than a symbolic link, because a process started
//! through a link on macOS is told the link's path as its own, and the
//! launcher finds its installation from that path. The macOS application
//! bundle meets the same problem and solves it the same way. The script's
//! second line names the application it belongs to; see [`Availability`].
//!
//! **Windows:** a copy of the console launcher at `<root>\bin\<name>.exe`,
//! and `<root>\bin` added to the user's own `PATH`. The copy finds its
//! installation through the rule in
//! [`InstallPaths::application_dir_of`](xpack_core::InstallPaths::application_dir_of).
//! The `PATH` is edited in the registry, keeping the value's type and every
//! other entry exactly as it was, and running programs are then told the
//! environment changed, or no new terminal would see it before the user next
//! signed in.
//!
//! # The rules
//!
//! The same as the desktop entry's, for the same reasons: per user and never
//! elevated; only when the package asks and the user has not declined; paths
//! derived, never recorded, so removal recomputes them; and never a reason for
//! an installation to fail. One more: **never take what is not ours.** A
//! `mytool` that some other program put there is left alone, on install and on
//! removal alike.
//!
//! [`CommandSpec`]: xpack_core::CommandSpec

use std::path::{Path, PathBuf};

use xpack_core::{BinaryNames, InstallPaths, Manifest};

use super::Outcome;

/// The registry key, under `HKEY_CURRENT_USER`, that holds the user's `PATH`.
pub const USER_ENVIRONMENT_KEY: &str = "Environment";

/// Where a command is put, for the current user.
///
/// A value rather than looked up inside, for the reason [`super::Roots`] is:
/// tests point it at a temporary directory and a scratch registry key, so a
/// test run never writes into the developer's `~/.local/bin` or edits a build
/// machine's real `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRoots {
    /// Where the script goes on macOS and Linux: `~/.local/bin`.
    pub bin: PathBuf,
    /// The registry key under `HKEY_CURRENT_USER` whose `Path` value is
    /// edited on Windows: [`USER_ENVIRONMENT_KEY`] for real.
    pub environment_key: String,
}

impl CommandRoots {
    /// The current user's.
    pub fn host() -> Option<Self> {
        let dirs = directories::BaseDirs::new()?;
        Some(Self {
            bin: dirs.home_dir().join(".local").join("bin"),
            environment_key: USER_ENVIRONMENT_KEY.to_string(),
        })
    }
}

/// One application's command, resolved against its installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// The application it starts; the ownership mark names it.
    pub application_id: String,
    /// What the user types.
    pub name: String,
    /// The console launcher it runs, as an absolute path.
    pub launcher: PathBuf,
    /// The installation's command directory, as an absolute path. Used on
    /// Windows only.
    pub command_dir: PathBuf,
}

impl Command {
    /// The command a manifest asks for, in the installation at `paths`.
    ///
    /// `None` when it asks for none. Paths are made absolute here, because a
    /// command runs from wherever the user is, and a relative installation
    /// root would name nothing from there.
    pub fn from_manifest(
        manifest: &Manifest,
        paths: &InstallPaths,
        names: &BinaryNames,
    ) -> Option<Self> {
        let spec = manifest.command.as_ref()?;
        Some(Self {
            application_id: manifest.application.id.clone(),
            name: spec.name.clone(),
            launcher: absolute(&paths.launcher_file_named(names)),
            command_dir: absolute(&paths.command_dir()),
        })
    }
}

impl Command {
    /// Every command a manifest asks for: the main one first, then the extra
    /// ones. All run the same launcher, which starts the program each names.
    pub fn all_from_manifest(
        manifest: &Manifest,
        paths: &InstallPaths,
        names: &BinaryNames,
    ) -> Vec<Self> {
        let launcher = absolute(&paths.launcher_file_named(names));
        let command_dir = absolute(&paths.command_dir());
        manifest
            .command
            .iter()
            .map(|command| command.name.clone())
            .chain(manifest.commands.iter().map(|extra| extra.name.clone()))
            .map(|name| Self {
                application_id: manifest.application.id.clone(),
                name,
                launcher: launcher.clone(),
                command_dir: command_dir.clone(),
            })
            .collect()
    }
}

/// Puts every one of `commands` in place.
///
/// One that cannot be, because another program owns its name, does not stop
/// the others. Done when any was, with the rest logged; failed only when none
/// could be.
pub fn install_all(commands: &[Command], roots: &CommandRoots) -> Outcome {
    combine(commands.iter().map(|command| install(command, roots)))
}

/// Removes every one of `commands` that is ours.
pub fn remove_all(commands: &[Command], roots: &CommandRoots) -> Outcome {
    combine(commands.iter().map(|command| remove(command, roots)))
}

fn combine(outcomes: impl Iterator<Item = Outcome>) -> Outcome {
    let mut done = Vec::new();
    let mut failed = Vec::new();
    for outcome in outcomes {
        match outcome {
            Outcome::Done(paths) => done.extend(paths),
            Outcome::Failed(reason) => failed.push(reason),
            _ => {}
        }
    }
    if !done.is_empty() {
        for reason in &failed {
            tracing::warn!(reason, "a command was left out; the others are in place");
        }
        Outcome::Done(done)
    } else if !failed.is_empty() {
        Outcome::Failed(failed.join("; "))
    } else {
        Outcome::NothingToDo
    }
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Whether a command's name is free to take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// Nothing is there.
    Free,
    /// This application's own command is there, from an earlier install.
    Ours,
    /// Something else is there, and is left alone.
    Foreign(PathBuf),
}

/// Whether `command` can be put in place under `roots`.
///
/// On Windows the command lives inside the installation, which only this
/// application writes to, so it is never someone else's.
pub fn availability(command: &Command, roots: &CommandRoots) -> Availability {
    if cfg!(windows) { Availability::Free } else { script_availability(command, roots) }
}

/// Puts `command` where a terminal looks.
pub fn install(command: &Command, roots: &CommandRoots) -> Outcome {
    imp::install(command, roots)
}

/// Removes `command`, if it is ours.
pub fn remove(command: &Command, roots: &CommandRoots) -> Outcome {
    imp::remove(command, roots)
}

/// Where a declined command is recorded: beside the desktop entry's record.
pub fn preference_file(paths: &InstallPaths) -> PathBuf {
    paths.state_dir().join("command.json")
}

// ------------------------------------------------------------ the Unix script

/// Where the script for `command` goes.
pub fn script_path(command: &Command, roots: &CommandRoots) -> PathBuf {
    roots.bin.join(&command.name)
}

/// The line that marks a script as this application's.
pub fn ownership_mark(application_id: &str) -> String {
    format!("# Added by xPack for {application_id}. Removed when it is uninstalled.")
}

/// The script that starts `command`'s launcher.
///
/// `exec`, so the launcher replaces the shell rather than running under it:
/// its exit status, its signals and its process are the ones the user's
/// terminal sees. Each script names the command it is, so the launcher starts
/// that command's program; see [`xpack_core::paths::COMMAND_ENV`].
pub fn script(command: &Command) -> String {
    format!(
        "#!/bin/sh\n{}\n{}={} exec {} \"$@\"\n",
        ownership_mark(&command.application_id),
        xpack_core::paths::COMMAND_ENV,
        shell_quote(&command.name),
        shell_quote(&command.launcher.to_string_lossy())
    )
}

/// Quotes `value` for a POSIX shell: single quotes, which nothing inside
/// expands, with each single quote closed, escaped and reopened.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Whose the script at `command`'s path is.
///
/// Read without following links: a link is someone else's however it came to
/// be there, and so is anything that is not a plain file. A plain file is ours
/// only when its second line is this application's mark, exactly.
fn script_availability(command: &Command, roots: &CommandRoots) -> Availability {
    /// More than any script this module writes, so a large file is not read
    /// just to be found foreign.
    const MAX_SCRIPT_BYTES: u64 = 64 * 1024;

    let path = script_path(command, roots);
    let Ok(metadata) = std::fs::symlink_metadata(&path) else {
        return Availability::Free;
    };
    if !metadata.is_file() || metadata.len() > MAX_SCRIPT_BYTES {
        return Availability::Foreign(path);
    }
    let mark = ownership_mark(&command.application_id);
    match std::fs::read_to_string(&path) {
        Ok(text) if text.lines().nth(1) == Some(mark.as_str()) => Availability::Ours,
        _ => Availability::Foreign(path),
    }
}

#[cfg(unix)]
mod imp {
    use std::os::unix::fs::PermissionsExt;

    use super::{Availability, Command, CommandRoots, Outcome, script, script_availability};

    pub(super) fn install(command: &Command, roots: &CommandRoots) -> Outcome {
        let path = super::script_path(command, roots);
        if let Availability::Foreign(path) = script_availability(command, roots) {
            return Outcome::Failed(format!(
                "{} belongs to another program and was left alone",
                path.display()
            ));
        }
        if let Err(error) = xpack_core::atomic::create_dir_all(&roots.bin) {
            return Outcome::Failed(error.to_string());
        }
        if let Err(error) = xpack_core::atomic::write(&path, script(command).as_bytes()) {
            return Outcome::Failed(error.to_string());
        }
        if let Err(error) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        {
            return Outcome::Failed(format!("{}: {error}", path.display()));
        }
        Outcome::Done(vec![path])
    }

    /// Removes the script only when it is exactly the one this installation
    /// writes. Carrying this application's mark is enough to be replaced, so
    /// an update or a reinstall can refresh it, but not to be deleted: the
    /// same application installed a second time, elsewhere, points the
    /// command at itself, and uninstalling the first copy must leave it.
    pub(super) fn remove(command: &Command, roots: &CommandRoots) -> Outcome {
        let path = super::script_path(command, roots);
        match script_availability(command, roots) {
            Availability::Ours
                if std::fs::read_to_string(&path).is_ok_and(|text| text == script(command)) =>
            {
                super::super::remove_paths(vec![path])
            }
            Availability::Ours => {
                tracing::info!(path = %path.display(), "the command runs another copy; left alone");
                Outcome::NothingToDo
            }
            Availability::Free => Outcome::NothingToDo,
            Availability::Foreign(path) => {
                tracing::info!(path = %path.display(), "not our command; left alone");
                Outcome::NothingToDo
            }
        }
    }
}

// ------------------------------------------------------------ the Windows PATH

/// `value` with `dir` added at the end, or `None` when it is already there.
///
/// Entries are compared as Windows compares paths: without regard to case,
/// and with a trailing separator or surrounding space ignored. Every other
/// entry is left exactly as it was, including empty ones and unexpanded
/// `%VARIABLES%`.
pub fn path_with(value: &str, dir: &str) -> Option<String> {
    if value.split(';').any(|entry| same_entry(entry, dir)) {
        return None;
    }
    let kept = value.trim_end_matches(';');
    Some(if kept.is_empty() { dir.to_string() } else { format!("{kept};{dir}") })
}

/// `value` with every entry naming `dir` taken out, or `None` when there was
/// none. Nothing else changes.
pub fn path_without(value: &str, dir: &str) -> Option<String> {
    let entries: Vec<&str> = value.split(';').collect();
    let kept: Vec<&str> = entries.iter().copied().filter(|entry| !same_entry(entry, dir)).collect();
    (kept.len() != entries.len()).then(|| kept.join(";"))
}

fn same_entry(entry: &str, dir: &str) -> bool {
    fn normal(path: &str) -> String {
        path.trim().trim_end_matches(['\\', '/']).to_lowercase()
    }
    !entry.trim().is_empty() && normal(entry) == normal(dir)
}

#[cfg(windows)]
mod imp {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, RegType};
    use winreg::types::FromRegValue;

    use super::{Command, CommandRoots, Outcome, path_with, path_without};

    pub(super) fn install(command: &Command, roots: &CommandRoots) -> Outcome {
        let copy = command.command_dir.join(format!("{}.exe", command.name));
        if let Err(error) = xpack_core::atomic::create_dir_all(&command.command_dir) {
            return Outcome::Failed(error.to_string());
        }
        // Placed only when absent, like the launcher it copies: a running
        // copy cannot be replaced on Windows, and this one may be running.
        if !copy.exists()
            && let Err(error) = std::fs::copy(&command.launcher, &copy)
        {
            return Outcome::Failed(format!("{}: {error}", copy.display()));
        }
        let dir = command.command_dir.to_string_lossy();
        match edit_path(&roots.environment_key, |value| path_with(value, &dir)) {
            Ok(changed) => {
                if changed {
                    xpack_platform::announce_environment_change();
                }
                Outcome::Done(vec![copy])
            }
            Err(error) => Outcome::Failed(format!("the user's PATH: {error}")),
        }
    }

    /// Removes this command's copy, and the directory and its `PATH` entry
    /// once no other command of the application is left in it.
    pub(super) fn remove(command: &Command, roots: &CommandRoots) -> Outcome {
        let copy = command.command_dir.join(format!("{}.exe", command.name));
        let removed = copy.exists();
        if let Err(error) = xpack_core::atomic::remove_file_if_exists(&copy) {
            return Outcome::Failed(error.to_string());
        }
        let others_left = std::fs::read_dir(&command.command_dir)
            .is_ok_and(|mut entries| entries.next().is_some());
        if others_left {
            return if removed { Outcome::Done(vec![copy]) } else { Outcome::NothingToDo };
        }

        let dir = command.command_dir.to_string_lossy();
        let changed = match edit_path(&roots.environment_key, |value| path_without(value, &dir)) {
            Ok(changed) => changed,
            Err(error) => return Outcome::Failed(format!("the user's PATH: {error}")),
        };
        if changed {
            xpack_platform::announce_environment_change();
        }
        if let Err(error) = xpack_core::atomic::remove_dir_all_if_exists(&command.command_dir) {
            return Outcome::Failed(error.to_string());
        }
        if removed || changed { Outcome::Done(vec![copy]) } else { Outcome::NothingToDo }
    }

    /// Rewrites the `Path` value under `key` with `edit`, keeping its type.
    ///
    /// Returns whether it changed. `edit` returning `None` means "leave it".
    /// A value that is not a string is refused rather than overwritten: it is
    /// not what any Windows version writes, and replacing it would lose
    /// whatever someone put there.
    pub(super) fn edit_path(
        key: &str,
        edit: impl Fn(&str) -> Option<String>,
    ) -> std::io::Result<bool> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey_with_flags(key, KEY_READ | KEY_WRITE)?;
        let (current, kind) = match key.get_raw_value("Path") {
            Ok(raw) => {
                let kind = raw.vtype.clone();
                if !matches!(kind, RegType::REG_EXPAND_SZ | RegType::REG_SZ) {
                    return Err(std::io::Error::other("Path is not a string value"));
                }
                (String::from_reg_value(&raw)?, kind)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Expandable, as Windows creates it: entries that name
                // `%USERPROFILE%` keep working when someone adds one.
                (String::new(), RegType::REG_EXPAND_SZ)
            }
            Err(error) => return Err(error),
        };
        let Some(updated) = edit(&current) else {
            return Ok(false);
        };
        let bytes: Vec<u8> =
            updated.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect();
        key.set_raw_value("Path", &winreg::RegValue { bytes: bytes.into(), vtype: kind })?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(launcher: &str) -> Command {
        Command {
            application_id: "com.example.tool".into(),
            name: "mytool".into(),
            launcher: PathBuf::from(launcher),
            command_dir: PathBuf::from("/apps/com.example.tool/bin"),
        }
    }

    #[test]
    fn the_script_marks_itself_and_runs_the_launcher_with_every_argument() {
        let text = script(&command("/apps/com.example.tool/MyTool"));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "#!/bin/sh");
        assert_eq!(lines[1], ownership_mark("com.example.tool"));
        // Names itself, so the launcher starts this command's program.
        assert_eq!(lines[2], r#"XPACK_COMMAND='mytool' exec '/apps/com.example.tool/MyTool' "$@""#);
    }

    #[test]
    fn a_launcher_path_the_shell_would_otherwise_interpret_is_quoted() {
        // Spaces, `$`, backticks and a single quote: every one of them would
        // change what runs if the path were not quoted.
        let text = script(&command("/Users/o'neil/My $HOME/`x`/Tool"));
        assert!(text.contains(r#" exec '/Users/o'\''neil/My $HOME/`x`/Tool' "$@""#), "{text}");
    }

    #[test]
    fn adding_to_the_path_appends_once_and_leaves_every_other_entry_as_it_was() {
        let before = r"%USERPROFILE%\bin;C:\Tools;;D:\x";
        let after = path_with(before, r"C:\Apps\tool\bin").unwrap();
        assert_eq!(after, r"%USERPROFILE%\bin;C:\Tools;;D:\x;C:\Apps\tool\bin");

        // Already there, spelled differently: not added a second time.
        assert_eq!(path_with(&after, r"c:\apps\TOOL\bin\"), None);
        assert_eq!(path_with("", r"C:\a"), Some(r"C:\a".to_string()));
        assert_eq!(path_with(r"C:\x;", r"C:\a"), Some(r"C:\x;C:\a".to_string()));
    }

    #[test]
    fn removing_from_the_path_takes_out_only_that_entry() {
        let before = r"%USERPROFILE%\bin;C:\Apps\tool\bin;C:\Tools;;c:\apps\tool\BIN\";
        assert_eq!(
            path_without(before, r"C:\Apps\tool\bin").as_deref(),
            Some(r"%USERPROFILE%\bin;C:\Tools;")
        );
        // Not there: nothing to write back.
        assert_eq!(path_without(r"C:\Tools", r"C:\Apps\tool\bin"), None);
    }

    #[test]
    fn a_path_entry_that_only_resembles_ours_is_kept() {
        // A prefix, a longer path and an empty entry are all someone else's.
        let before = r"C:\Apps\tool;C:\Apps\tool\bin2;;C:\Apps\tool\bin\x";
        assert_eq!(path_without(before, r"C:\Apps\tool\bin"), None);
    }
}
