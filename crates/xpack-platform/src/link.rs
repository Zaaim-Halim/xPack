//! The derived `current` link.
//!
//! A stable path such as `<install>/current/bin/app` is convenient for
//! shortcuts, scripts and desktop entries. It is **not** the authority on
//! which version runs — the installation state file is, because a link cannot
//! be swapped atomically on every platform.
//!
//! Everything here is therefore best effort. A missing or stale link is a
//! degraded convenience, never a broken installation, and [`LinkOutcome`]
//! exists to make that impossible to misread at the call site: it is not a
//! `Result`, so a caller cannot propagate it with `?` and accidentally fail an
//! install because a cosmetic link could not be written.

use std::path::Path;

use xpack_core::{InstallPaths, Version};

/// Distinguishes concurrent temporary link names within one process.
///
/// Unix-only: the Windows path reports the link as unsupported and never
/// creates a temporary.
#[cfg(unix)]
static LINK_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// What happened when the link was updated.
///
/// Deliberately not a `Result`. Failing an installation because a convenience
/// link could not be created would be a worse outcome than the missing link.
#[derive(Debug)]
pub enum LinkOutcome {
    /// The link now points at the requested version.
    Updated,
    /// This platform cannot create the link without unsafe code.
    Unsupported(String),
    /// The link could not be written; the installation is still valid.
    Failed(String),
}

impl LinkOutcome {
    /// Returns `true` when the link is now correct.
    pub fn is_updated(&self) -> bool {
        matches!(self, Self::Updated)
    }

    /// Logs the outcome at a level matching its severity.
    pub fn log(&self) {
        match self {
            Self::Updated => tracing::debug!("current link updated"),
            Self::Unsupported(reason) => {
                tracing::debug!(reason, "current link not supported on this platform");
            }
            Self::Failed(reason) => {
                tracing::warn!(
                    reason,
                    "current link could not be updated; installation is unaffected"
                );
            }
        }
    }
}

/// Points the `current` link at `version`.
///
/// Refuses to create a link to a version directory that does not exist. A
/// dangling `current` is worse than a missing one: a shortcut or script
/// following it fails with a confusing "no such file" pointing at a path that
/// looks correct, rather than simply not being there.
pub fn update_current_link(paths: &InstallPaths, version: &Version) -> LinkOutcome {
    let target = paths.version_dir(version);
    if !target.is_dir() {
        return LinkOutcome::Failed(format!(
            "{} is not an installed version directory",
            target.display()
        ));
    }
    let link = paths.current_link();
    platform_update(&target, &link)
}

/// Replaces a symlink atomically.
///
/// The link is created under a temporary name in the same directory and then
/// renamed over the destination. `rename` replaces atomically on POSIX, so a
/// reader sees either the old target or the new one.
///
/// Removing the old link and creating a new one would leave a window in which
/// `current` does not exist — the same non-atomic delete-then-create that made
/// junction swapping unsuitable as the source of truth in the first place.
#[cfg(unix)]
fn platform_update(target: &Path, link: &Path) -> LinkOutcome {
    use std::os::unix::fs::symlink;

    let Some(parent) = link.parent() else {
        return LinkOutcome::Failed(format!("{} has no parent directory", link.display()));
    };
    if let Err(e) = std::fs::create_dir_all(parent) {
        return LinkOutcome::Failed(format!("{}: {e}", parent.display()));
    }

    // The process id alone is not unique enough: two threads in one process
    // would pick the same temporary name and clobber each other mid-swap.
    let sequence = LINK_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let mut temporary = link.as_os_str().to_os_string();
    temporary.push(format!(".xpack-link-{}-{sequence}", std::process::id()));
    let temporary = std::path::PathBuf::from(temporary);

    let _ = std::fs::remove_file(&temporary);
    if let Err(e) = symlink(target, &temporary) {
        return LinkOutcome::Failed(format!("{}: {e}", temporary.display()));
    }
    match std::fs::rename(&temporary, link) {
        Ok(()) => LinkOutcome::Updated,
        Err(e) => {
            let _ = std::fs::remove_file(&temporary);
            LinkOutcome::Failed(format!("{}: {e}", link.display()))
        }
    }
}

/// Windows needs a directory junction, which needs unsafe code.
///
/// A symbolic link would be the obvious alternative, but creating one requires
/// either administrator rights or Developer Mode, neither of which an
/// unprivileged per-user installation can assume. A junction has no such
/// requirement, but creating one means `DeviceIoControl` with
/// `FSCTL_SET_REPARSE_POINT`, and this workspace forbids unsafe code.
///
/// Reporting the link as unsupported is the honest outcome: the installation
/// works exactly as well without it, because the state file is authoritative.
#[cfg(not(unix))]
fn platform_update(target: &Path, link: &Path) -> LinkOutcome {
    let _ = (target, link);
    LinkOutcome::Unsupported(
        "creating a directory junction requires unsafe Win32 calls, which this build excludes"
            .to_string(),
    )
}
