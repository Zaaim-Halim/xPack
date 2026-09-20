//! Putting a copy of a file somewhere without paying for the bytes twice.
//!
//! Every installed version holds a complete payload, and for an application
//! that bundles a runtime almost all of that payload is identical between
//! versions. Keeping the active version and its rollback target therefore
//! costs twice the runtime on disk, every time, for bytes that are the same.
//!
//! There are three ways to put a file's contents at a second path, and they
//! are not interchangeable:
//!
//! | | Costs disk | Survives a write to either copy | Where |
//! | --- | --- | --- | --- |
//! | Clone (copy-on-write) | nothing | yes | APFS, btrfs, XFS, `ReFS` |
//! | Hard link | nothing | **no** | anywhere POSIX, and NTFS |
//! | Copy | everything | yes | anywhere |
//!
//! A clone is what you want and is not always available. A hard link is
//! available almost everywhere and **aliases**: writing through one path
//! changes what the other path sees, which across two installed versions
//! means corrupting the one a user would roll back to. A copy is always
//! correct and always expensive.
//!
//! So the choice is made per filesystem, at run time, by trying the cheapest
//! safe thing and remembering what happened — rather than by guessing from
//! the operating system's name, which says nothing about the filesystem a
//! user's home directory happens to be on.

use std::cell::Cell;
use std::path::Path;

use xpack_core::{Error, Result};

/// How a file's contents were placed at a second path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Copy-on-write. Shares storage until either side is written to.
    Cloned,
    /// A second name for the same data. Shares storage permanently.
    Linked,
    /// Independent bytes.
    Copied,
}

impl Placement {
    /// Whether this placement left the two paths sharing storage.
    pub fn shares_storage(self) -> bool {
        matches!(self, Self::Cloned | Self::Linked)
    }

    /// Whether writing through one path would change what the other sees.
    ///
    /// True only of a hard link. A clone breaks its own sharing on the first
    /// write, which is the property that makes it safe to use unasked.
    pub fn aliases(self) -> bool {
        matches!(self, Self::Linked)
    }
}

/// How much sharing a caller is willing to accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sharing {
    /// Only mechanisms where a later write to one path cannot affect the
    /// other. Clones where the filesystem has them, copies everywhere else.
    ///
    /// Safe to use without the caller doing anything further.
    Safe,
    /// Also hard links.
    ///
    /// Saves the same disk on the filesystems that have no clones, which is
    /// most Linux and nearly all Windows installations. The cost is that the
    /// two paths become the same file: a caller choosing this is promising
    /// that nothing will write to either of them, and is expected to enforce
    /// that rather than hope for it.
    Aliasing,
}

/// Places files, remembering what this filesystem turned out to support.
///
/// One of these covers one destination tree. The first file probes; the rest
/// go straight to whatever worked, so an unsupported mechanism costs one
/// failed call rather than one per file — which for a runtime image is the
/// difference between one and several thousand.
pub struct Sharer {
    sharing: Sharing,
    can_clone: Cell<Option<bool>>,
    can_link: Cell<Option<bool>>,
}

impl Sharer {
    /// A sharer that will not alias.
    pub fn safe() -> Self {
        Self::new(Sharing::Safe)
    }

    /// A sharer permitted to use `sharing`.
    pub fn new(sharing: Sharing) -> Self {
        Self { sharing, can_clone: Cell::new(None), can_link: Cell::new(None) }
    }

    /// What this sharer is permitted to do.
    pub fn sharing(&self) -> Sharing {
        self.sharing
    }

    /// Puts the contents of `source` at `destination`.
    ///
    /// `destination` must not already exist: both cloning and linking refuse
    /// to replace a file, and a copy that silently did would be a different
    /// operation from the other two.
    pub fn place(&self, source: &Path, destination: &Path) -> Result<Placement> {
        // Checked rather than left to the mechanisms, which disagree: a
        // clone and a link both refuse an existing destination while a copy
        // overwrites it. Without this, which of those happened would depend
        // on the filesystem, and a caller could not reason about either.
        if destination.symlink_metadata().is_ok() {
            return Err(Error::invalid(
                "destination",
                format!("{} already exists", destination.display()),
            ));
        }

        if self.can_clone.get() != Some(false) {
            match reflink_copy::reflink(source, destination) {
                Ok(()) => {
                    self.can_clone.set(Some(true));
                    return Ok(Placement::Cloned);
                }
                Err(_) if self.can_clone.get().is_none() => {
                    // The first failure is the probe: this filesystem, or
                    // this pair of filesystems, has no clones. Later files
                    // do not try again.
                    self.can_clone.set(Some(false));
                }
                Err(e) => return Err(Error::io(destination, e)),
            }
        }

        if self.sharing == Sharing::Aliasing && self.can_link.get() != Some(false) {
            match std::fs::hard_link(source, destination) {
                Ok(()) => {
                    self.can_link.set(Some(true));
                    return Ok(Placement::Linked);
                }
                Err(_) if self.can_link.get().is_none() => {
                    self.can_link.set(Some(false));
                }
                Err(e) => return Err(Error::io(destination, e)),
            }
        }

        std::fs::copy(source, destination).map_err(|e| Error::io(destination, e))?;
        Ok(Placement::Copied)
    }
}

impl Default for Sharer {
    fn default() -> Self {
        Self::safe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn source(dir: &Path, contents: &[u8]) -> std::path::PathBuf {
        let path = dir.join("source");
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn the_contents_arrive_however_they_were_placed() {
        for sharing in [Sharing::Safe, Sharing::Aliasing] {
            let dir = tempfile::tempdir().unwrap();
            let from = source(dir.path(), b"the payload");
            let to = dir.path().join("destination");

            Sharer::new(sharing).place(&from, &to).unwrap();

            assert_eq!(fs::read(&to).unwrap(), b"the payload");
        }
    }

    #[test]
    fn a_safe_sharer_never_aliases() {
        // The whole point of the distinction: whatever this filesystem
        // supports, a safe sharer must not produce two names for one file.
        let dir = tempfile::tempdir().unwrap();
        let from = source(dir.path(), b"original");
        let to = dir.path().join("destination");

        let placement = Sharer::safe().place(&from, &to).unwrap();

        assert!(!placement.aliases(), "a safe sharer produced {placement:?}");
    }

    #[test]
    fn writing_to_one_side_of_a_safe_placement_leaves_the_other_alone() {
        // Stated as behaviour rather than as a property of the mechanism,
        // because this is the guarantee a caller is relying on.
        let dir = tempfile::tempdir().unwrap();
        let from = source(dir.path(), b"original");
        let to = dir.path().join("destination");
        Sharer::safe().place(&from, &to).unwrap();

        fs::write(&from, b"rewritten").unwrap();

        assert_eq!(fs::read(&to).unwrap(), b"original");
    }

    #[test]
    fn an_aliasing_placement_is_honest_about_what_it_did() {
        let dir = tempfile::tempdir().unwrap();
        let from = source(dir.path(), b"original");
        let to = dir.path().join("destination");

        let placement = Sharer::new(Sharing::Aliasing).place(&from, &to).unwrap();

        // Whichever it managed, a caller can ask whether it has to protect
        // the result from being written to.
        if placement.aliases() {
            fs::write(&from, b"rewritten").unwrap();
            assert_eq!(
                fs::read(&to).unwrap(),
                b"rewritten",
                "a placement that says it aliases must actually alias"
            );
        } else {
            fs::write(&from, b"rewritten").unwrap();
            assert_eq!(fs::read(&to).unwrap(), b"original");
        }
    }

    #[test]
    fn a_destination_that_already_exists_is_refused() {
        // Cloning and linking both refuse to replace a file. A copy would
        // happily overwrite, so letting it would make the three mechanisms
        // behave differently in a way a caller could not see.
        let dir = tempfile::tempdir().unwrap();
        let from = source(dir.path(), b"original");
        let to = dir.path().join("destination");
        fs::write(&to, b"already here").unwrap();

        assert!(Sharer::safe().place(&from, &to).is_err());
        assert_eq!(fs::read(&to).unwrap(), b"already here");
    }

    #[test]
    fn a_missing_source_is_an_error_rather_than_an_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("destination");

        assert!(Sharer::safe().place(&dir.path().join("absent"), &to).is_err());
        assert!(!to.exists());
    }

    #[test]
    fn the_filesystem_is_probed_once_and_not_once_per_file() {
        // A runtime image is thousands of files. Retrying an unsupported
        // syscall for each of them is the difference between one failed call
        // and several thousand.
        let dir = tempfile::tempdir().unwrap();
        let from = source(dir.path(), b"payload");
        let sharer = Sharer::safe();

        let first = sharer.place(&from, &dir.path().join("a")).unwrap();
        let second = sharer.place(&from, &dir.path().join("b")).unwrap();

        // Whatever it decided, it decided the same way twice.
        assert_eq!(first, second);
    }

    #[test]
    fn a_copy_is_always_available_as_the_last_resort() {
        // Nothing here may fail for want of a filesystem feature.
        let dir = tempfile::tempdir().unwrap();
        let from = source(dir.path(), b"payload");
        let sharer = Sharer::safe();
        // Force both faster paths off, as an unsupporting filesystem would.
        sharer.can_clone.set(Some(false));
        sharer.can_link.set(Some(false));

        let placement = sharer.place(&from, &dir.path().join("destination")).unwrap();

        assert_eq!(placement, Placement::Copied);
    }

    #[test]
    fn only_a_link_is_reported_as_sharing_and_aliasing_together() {
        assert!(Placement::Cloned.shares_storage() && !Placement::Cloned.aliases());
        assert!(Placement::Linked.shares_storage() && Placement::Linked.aliases());
        assert!(!Placement::Copied.shares_storage() && !Placement::Copied.aliases());
    }
}
