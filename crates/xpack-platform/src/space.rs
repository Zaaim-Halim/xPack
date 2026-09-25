//! How much room a volume has.
//!
//! An install or an update that runs out of disk part-way is recovered from
//! safely, but late: the user finds out after waiting, and the disk is left
//! full for every other program until recovery cleans up. Asking first turns
//! that into a refusal while nothing has changed.

use std::path::{Path, PathBuf};

use xpack_core::{Error, Result};

/// Bytes free for this user on the volume holding `path`.
///
/// `path` need not exist yet: a first install asks about a directory it is
/// about to create, so the nearest existing ancestor is asked instead. The
/// figure is what this user may write, which on Unix excludes the space
/// reserved for the superuser.
pub fn available_space(path: &Path) -> Result<u64> {
    let existing = nearest_existing(path)?;
    imp::available(&existing).map_err(|e| Error::io(&existing, e))
}

/// Fails with [`Error::NotEnoughSpace`] when the volume holding `path` has
/// less than `needed` bytes free.
///
/// When the free space cannot be determined, it logs that and succeeds: a
/// check that could not be made is no reason to stop an install that would
/// most likely have fitted.
pub fn ensure_space(path: &Path, needed: u64, what: &str) -> Result<()> {
    match available_space(path) {
        Ok(available) if available < needed => Err(Error::NotEnoughSpace {
            what: what.to_string(),
            path: path.to_path_buf(),
            needed,
            available,
        }),
        Ok(_) => Ok(()),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "free space unknown; not checked");
            Ok(())
        }
    }
}

/// `path`, or the closest of its ancestors that exists.
fn nearest_existing(path: &Path) -> Result<PathBuf> {
    let mut candidate = path;
    loop {
        if candidate.exists() {
            return Ok(candidate.to_path_buf());
        }
        candidate = candidate.parent().filter(|p| !p.as_os_str().is_empty()).ok_or_else(|| {
            Error::invalid("path", format!("{} has no existing ancestor", path.display()))
        })?;
    }
}

#[cfg(unix)]
mod imp {
    use std::path::Path;

    pub(super) fn available(path: &Path) -> std::io::Result<u64> {
        let stats = rustix::fs::statvfs(path)?;
        // `f_bavail`, not `f_bfree`: the blocks this user may use, which
        // leaves out those reserved for the superuser.
        Ok(stats.f_bavail.saturating_mul(stats.f_frsize))
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    pub(super) fn available(path: &Path) -> std::io::Result<u64> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut available: u64 = 0;
        // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the
        // call, and `available` is a valid place for the one figure asked
        // for. The two other outputs are optional and passed as null, which
        // the API documents as "not wanted". Nothing is retained.
        #[allow(unsafe_code)]
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &raw mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(available)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_directory_has_some_room() {
        let dir = tempfile::tempdir().unwrap();
        assert!(available_space(dir.path()).unwrap() > 0);
    }

    #[test]
    fn a_directory_about_to_be_created_is_asked_about_through_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        let later = dir.path().join("not/yet/there");
        assert_eq!(
            available_space(&later).unwrap() / (1 << 20),
            available_space(dir.path()).unwrap() / (1 << 20),
            "not the same volume, to the nearest MiB"
        );
    }

    #[test]
    fn more_than_the_volume_has_is_refused_with_the_figures() {
        let dir = tempfile::tempdir().unwrap();
        let error = ensure_space(dir.path(), u64::MAX, "this version").unwrap_err();
        match error {
            Error::NotEnoughSpace { needed, available, .. } => {
                assert_eq!(needed, u64::MAX);
                assert!(available < needed);
            }
            other => panic!("expected NotEnoughSpace, got {other:?}"),
        }
    }

    #[test]
    fn what_fits_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        ensure_space(dir.path(), 1, "this version").unwrap();
    }
}
