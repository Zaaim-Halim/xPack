//! Removing an installation from inside it.
//!
//! # The problem this crate exists for
//!
//! An uninstaller ships inside the directory it has to delete. On Windows that
//! is not allowed: the kernel holds the executable image open for as long as
//! the process lives, so a running binary cannot delete itself, and a
//! directory containing it cannot be removed either.
//!
//! Of the usual answers, one is unavailable here. `MoveFileEx` with
//! `MOVEFILE_DELAY_UNTIL_REBOOT` would schedule the deletion, but it is a
//! Win32 call and this workspace forbids unsafe code — and it leaves the
//! installation standing until the machine restarts, which is a poor answer to
//! "uninstall this".
//!
//! So the uninstaller copies itself somewhere else and lets the copy do the
//! work, which is what most Windows installers do and needs nothing unsafe.
//!
//! # The same path on every platform
//!
//! Unix has no such restriction: a running executable can be unlinked, and the
//! inode survives until the process exits. The relocation could therefore be
//! skipped there.
//!
//! It is not. A code path that only ever runs on the platform this project
//! cannot test locally is exactly the kind of seam that has produced most of
//! the bugs here. Relocating everywhere costs one short-lived process on a
//! one-off user action, and buys a Windows-critical path that every Unix test
//! run exercises.

use std::path::{Path, PathBuf};

use xpack_core::{Error, InstallPaths, Result};

/// What the running uninstaller should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Remove the installation from this process.
    RemoveNow,
    /// Copy this binary to `to` and let that copy remove the installation.
    Relocate {
        /// Where the copy should be written.
        to: PathBuf,
    },
}

/// Decides whether this process can safely delete the installation.
///
/// `already_relocated` is what stops a copy from copying itself forever.
pub fn plan(paths: &InstallPaths, executable: &Path, already_relocated: bool) -> Result<Plan> {
    if already_relocated || !is_inside(executable, paths.root()) {
        return Ok(Plan::RemoveNow);
    }
    Ok(Plan::Relocate { to: relocation_target(paths)? })
}

/// Returns `true` when `executable` lives inside `root`.
///
/// Compared after canonicalising, because a path reached through a symlink or
/// containing `..` would otherwise look like it was somewhere it is not, and
/// the answer decides whether a directory gets deleted.
fn is_inside(executable: &Path, root: &Path) -> bool {
    let Ok(executable) = executable.canonicalize() else {
        // Unknown means "assume it is inside". The safe failure here is an
        // unnecessary relocation, not an attempt to delete a live binary.
        return true;
    };
    let Ok(root) = root.canonicalize() else {
        return false;
    };
    executable.starts_with(&root)
}

/// Picks a private directory outside the installation to run from.
fn relocation_target(paths: &InstallPaths) -> Result<PathBuf> {
    let application = paths.application_id().unwrap_or("xpack");
    let unique = format!("xpack-uninstall-{application}-{}", std::process::id());
    let dir = std::env::temp_dir().join(unique);
    xpack_core::atomic::create_dir_all(&dir)?;
    Ok(dir.join(format!("xpack-uninstaller{}", std::env::consts::EXE_SUFFIX)))
}

/// Copies this binary to `destination` and makes it runnable.
pub fn relocate(executable: &Path, destination: &Path) -> Result<()> {
    let bytes = std::fs::read(executable).map_err(|e| Error::io(executable, e))?;
    if bytes.is_empty() {
        return Err(Error::invalid("uninstaller", "this binary reads as empty"));
    }
    xpack_core::atomic::write(destination, &bytes)?;
    set_executable(destination)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| Error::io(path, e))
}

#[cfg(not(unix))]
fn set_executable(path: &Path) -> Result<()> {
    let _ = path;
    Ok(())
}

/// Removes the copy this process is running from, if it can.
///
/// Best effort by design. On Unix the file is unlinked immediately and the
/// running process keeps its open inode. On Windows the image is locked until
/// this process exits, so a small binary is left in the system temporary
/// directory for the operating system to clear — which is what temporary
/// directories are for, and a far better outcome than leaving the installation
/// itself behind.
pub fn clean_up_relocated_copy(executable: &Path) {
    if std::fs::remove_file(executable).is_err() {
        tracing::debug!(
            path = %executable.display(),
            "the relocated uninstaller could not remove itself; the system will clear it"
        );
        return;
    }
    if let Some(parent) = executable.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths_in(root: &Path) -> InstallPaths {
        InstallPaths::new(root, "com.example.app").unwrap()
    }

    #[test]
    fn a_binary_inside_the_installation_must_relocate() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        std::fs::create_dir_all(paths.root()).unwrap();
        let exe = paths.root().join("xpack-uninstaller");
        std::fs::write(&exe, b"binary").unwrap();

        assert!(matches!(plan(&paths, &exe, false).unwrap(), Plan::Relocate { .. }));
    }

    #[test]
    fn a_binary_outside_the_installation_removes_it_directly() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        std::fs::create_dir_all(paths.root()).unwrap();
        let exe = dir.path().join("elsewhere");
        std::fs::write(&exe, b"binary").unwrap();

        assert_eq!(plan(&paths, &exe, false).unwrap(), Plan::RemoveNow);
    }

    #[test]
    fn a_copy_that_already_relocated_never_relocates_again() {
        // Without this the uninstaller copies itself forever.
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        std::fs::create_dir_all(paths.root()).unwrap();
        let exe = paths.root().join("xpack-uninstaller");
        std::fs::write(&exe, b"binary").unwrap();

        assert_eq!(plan(&paths, &exe, true).unwrap(), Plan::RemoveNow);
    }

    #[test]
    fn an_unreadable_executable_path_is_assumed_to_be_inside() {
        // The safe failure is an unnecessary copy, never deleting a live binary.
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        std::fs::create_dir_all(paths.root()).unwrap();

        let missing = dir.path().join("does-not-exist");
        assert!(matches!(plan(&paths, &missing, false).unwrap(), Plan::Relocate { .. }));
    }

    #[test]
    fn a_relocated_copy_is_byte_identical_and_runnable() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::write(&source, b"#!/bin/sh\nexit 0\n").unwrap();
        let destination = dir.path().join("copy");

        relocate(&source, &destination).unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), std::fs::read(&source).unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&destination).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "not executable: {mode:o}");
        }
    }

    #[test]
    fn an_empty_binary_is_refused_rather_than_copied() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("empty");
        std::fs::write(&source, b"").unwrap();
        let destination = dir.path().join("copy");

        assert!(relocate(&source, &destination).is_err());
        assert!(!destination.exists());
    }
}
