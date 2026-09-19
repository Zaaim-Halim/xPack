//! The cross-process installation lock.
//!
//! Only one operation may mutate an installation at a time. Two updaters
//! extracting into the same directory, or an updater activating a version
//! while an uninstaller removes it, corrupt the installation in ways no amount
//! of verification downstream can repair.
//!
//! # The lock owns access to the state file
//!
//! The dangerous sequence is subtle: read `state.json`, *then* take the lock,
//! then write back — the write clobbers whatever the other process did in
//! between. Reviewing for that is unreliable, so the type system enforces it
//! instead. [`InstallLock`] is the only way to reach installation state:
//! [`InstallLock::load_state`] and [`InstallLock::save_state`] require the
//! guard, so "read before locking" does not compile.
//!
//! This is the same move the package reader makes for verification, where
//! extraction is only reachable from a verified package, for the same reason.
//!
//! # Two hazards this file exists to avoid
//!
//! **Never unlink the lock file.** On Unix the lock belongs to the inode, not
//! the path. Deleting the file and letting another process create a fresh one
//! at the same path gives that process a *different* inode to lock, and mutual
//! exclusion silently disappears. This was confirmed by experiment, not
//! assumed. The file is created once and never removed, not even on an error
//! path.
//!
//! **Never open the lock file with truncation.** `File::create` truncates,
//! which is a write to a file another process may be mid-operation with. The
//! lock is opened read-write, create-if-missing, and is never truncated.
//!
//! # Blocking policy
//!
//! Acquisition never blocks. A busy installation fails immediately with
//! [`Error::Locked`], because a command-line tool that reports "another
//! operation is already running" is far better than one that appears to hang.
//! Any future caller wanting to wait must do so explicitly, with its own
//! timeout and its own message.
//!
//! # The lock is not reentrant
//!
//! The lock is held per inode, not per handle, so a second acquisition from
//! the *same* process is refused exactly like one from a different process.
//! That is the safe behaviour — it cannot deadlock — but the resulting message
//! names "another operation", which is misleading when the caller is competing
//! with itself. Callers must acquire once and pass the guard down rather than
//! re-acquiring in a nested helper.

use std::fs::{File, OpenOptions, TryLockError};

use xpack_core::state::InstallState;
use xpack_core::store::Loaded;
use xpack_core::{Error, InstallPaths, Result};

/// An exclusive hold on one installation.
///
/// Releasing happens on drop. The lock is advisory, so it only excludes other
/// xPack processes — a user deleting the installation directory by hand is not
/// something any lock can prevent.
#[derive(Debug)]
pub struct InstallLock {
    /// Holding this handle holds the lock; dropping it releases.
    file: File,
    paths: InstallPaths,
}

impl InstallLock {
    /// Takes the lock, failing immediately if another process holds it.
    pub fn acquire(paths: &InstallPaths) -> Result<Self> {
        let dir = paths.state_dir();
        xpack_core::atomic::create_dir_all(&dir)?;
        let path = paths.lock_file();

        // Read-write, create-if-missing, and crucially *not* truncating.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| Error::io(&path, e))?;

        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(Error::Locked(path.display().to_string()));
            }
            Err(TryLockError::Error(e)) => return Err(Error::io(&path, e)),
        }

        tracing::debug!(lock = %path.display(), "installation lock acquired");
        Ok(Self { file, paths: paths.clone() })
    }

    /// The installation this lock protects.
    pub fn paths(&self) -> &InstallPaths {
        &self.paths
    }

    /// Reads installation state, recovering from the backup if necessary.
    ///
    /// Rejects state belonging to a different application. The layout and the
    /// document each carry an application id, and nothing else forces them to
    /// agree; a caller that mixed them would operate on one application's
    /// versions while believing it held another's.
    pub fn load_state(&self) -> Result<Loaded<InstallState>> {
        let loaded = InstallState::load(&self.paths.state_file())?;
        self.ensure_belongs(&loaded.value)?;
        Ok(loaded)
    }

    /// Reads state, or returns fresh state when nothing is installed yet.
    ///
    /// "Nothing installed" means **neither** the state document nor its backup
    /// exists. Testing only the primary would discard a perfectly good backup:
    /// an installation whose primary was lost would look brand new, orphaning
    /// every installed version and losing the record of which ones were
    /// healthy — exactly the failure the backup exists to prevent.
    ///
    /// A *corrupt* document is never treated as a first install either.
    /// Silently starting over would do the same damage, so it stays an error.
    pub fn load_or_new_state(&self, application_id: &str) -> Result<InstallState> {
        let path = self.paths.state_file();
        let backup = xpack_core::store::backup_path(&path);
        if path.exists() || backup.exists() {
            return Ok(self.load_state()?.value);
        }
        Ok(InstallState::new(application_id))
    }

    /// Writes installation state atomically.
    ///
    /// Refuses state belonging to a different application, so one
    /// application's records can never be written into another's directory.
    pub fn save_state(&self, state: &InstallState) -> Result<()> {
        self.ensure_belongs(state)?;
        state.save(&self.paths.state_file())
    }

    /// Confirms a state document belongs to the installation this lock holds.
    fn ensure_belongs(&self, state: &InstallState) -> Result<()> {
        let Some(expected) = self.paths.application_id() else {
            return Ok(());
        };
        if state.application_id == expected {
            return Ok(());
        }
        Err(Error::invalid(
            "installation state",
            format!(
                "document belongs to {:?} but this installation is {expected:?}",
                state.application_id
            ),
        ))
    }
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        // Releasing is best-effort: closing the handle releases it anyway, and
        // there is nothing useful to do with a failure while unwinding.
        //
        // The lock file itself is deliberately *not* removed. On Unix the lock
        // belongs to the inode, so unlinking would let the next process lock a
        // fresh inode at the same path and defeat mutual exclusion entirely.
        let _ = self.file.unlock();
    }
}
