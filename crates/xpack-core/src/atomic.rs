//! Crash-safe file primitives.
//!
//! xPack promises that a power cut never leaves an installation without a
//! resolvable active version. That promise is only real if writes are both
//! *atomic* (a reader sees the old or the new content, never a mix) and
//! *durable* (the rename survives the write-back cache).
//!
//! The sequence implemented here is the standard one:
//!
//! 1. write the payload to a sibling temporary file,
//! 2. `fsync` that file so its bytes reach stable storage,
//! 3. `rename` it over the destination — atomic on NTFS and on POSIX,
//! 4. `fsync` the *parent directory* so the rename itself is durable.
//!
//! Step 4 is the one that is usually skipped, and skipping it means the
//! directory entry can still be lost after a crash even though the data was
//! flushed. Windows offers no directory handle to sync, so that step is a
//! documented no-op there.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Error, Result};

/// Creates `dir` and every missing parent.
pub fn create_dir_all(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))
}

/// Atomically and durably replaces `path` with `contents`.
///
/// The temporary file is created in the destination directory so the rename
/// never crosses a filesystem boundary, which would make it non-atomic.
pub fn write(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        Error::invalid("path", format!("{} has no parent directory", path.display()))
    })?;
    create_dir_all(parent)?;

    let temp = temp_sibling(path)?;
    // Any early return from here on leaves the temporary file behind; the
    // guard removes it so a failed write cannot litter the installation.
    let cleanup = TempGuard(&temp);

    {
        let mut file = File::create(&temp).map_err(|e| Error::io(&temp, e))?;
        file.write_all(contents).map_err(|e| Error::io(&temp, e))?;
        file.sync_all().map_err(|e| Error::io(&temp, e))?;
    }

    fs::rename(&temp, path).map_err(|e| Error::io(path, e))?;
    cleanup.defuse();

    sync_dir(parent)
}

/// Serialises `value` as pretty JSON and writes it atomically.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|e| Error::json(path.display().to_string(), e))?;
    bytes.push(b'\n');
    write(path, &bytes)
}

/// Reads and deserialises a JSON document.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).map_err(|e| Error::io(path, e))?;
    serde_json::from_slice(&bytes).map_err(|e| Error::json(path.display().to_string(), e))
}

/// Flushes a directory entry to stable storage.
///
/// On Windows there is no supported way to obtain a directory handle through
/// `std::fs`, so this is a no-op; NTFS metadata journalling covers the common
/// crash cases there.
pub fn sync_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let handle = File::open(dir).map_err(|e| Error::io(dir, e))?;
        handle.sync_all().map_err(|e| Error::io(dir, e))?;
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
    Ok(())
}

/// Removes a directory tree, treating "already gone" as success.
///
/// Recovery paths call this on partially extracted versions, where a missing
/// directory is the desired end state rather than an error.
pub fn remove_dir_all_if_exists(dir: &Path) -> Result<()> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(dir, e)),
    }
}

/// Removes a file, treating "already gone" as success.
pub fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(path, e)),
    }
}

/// Builds a unique temporary sibling path for `path`.
///
/// Uniqueness comes from the process id plus a monotonic counter, so two
/// concurrent xPack processes cannot collide even without the install lock.
fn temp_sibling(path: &Path) -> Result<std::path::PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let name = path
        .file_name()
        .ok_or_else(|| Error::invalid("path", format!("{} has no file name", path.display())))?;
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut temp = name.to_os_string();
    temp.push(format!(".xpack-tmp-{}-{seq}", std::process::id()));
    Ok(path.with_file_name(temp))
}

/// Deletes a temporary file unless the operation succeeded.
struct TempGuard<'p>(&'p Path);

impl TempGuard<'_> {
    /// Consumes the guard without deleting, once the rename has succeeded.
    fn defuse(self) {
        std::mem::forget(self);
    }
}

impl Drop for TempGuard<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_existing_content_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        write(&path, b"first").unwrap();
        write(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
    }

    #[test]
    fn creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/c/state.json");
        write(&path, b"x").unwrap();
        assert!(path.exists());
    }

    #[test]
    fn leaves_no_temporary_files_behind() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("f"), b"x").unwrap();
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("xpack-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temporary files leaked: {leftovers:?}");
    }

    #[test]
    fn json_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.json");
        write_json(&path, &vec!["a".to_string(), "b".to_string()]).unwrap();
        let back: Vec<String> = read_json(&path).unwrap();
        assert_eq!(back, ["a", "b"]);
    }

    #[test]
    fn removal_helpers_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        remove_file_if_exists(&dir.path().join("nope")).unwrap();
        remove_dir_all_if_exists(&dir.path().join("nope-dir")).unwrap();
    }
}
