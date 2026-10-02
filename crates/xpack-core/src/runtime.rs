//! xPack's own programs in an installation, and replacing them.
//!
//! Beside the application's versions, an installation holds xPack's runtime
//! programs: the launcher, the updater, the uninstaller and the others. They
//! come from the xPack release that built the installer, which is recorded
//! (see [`InstallState::runtime_version`]) so that a later installer can
//! replace them with newer ones, and never with older ones.
//!
//! Replacing them is several renames, one per program, and must never leave
//! an installation with some programs from one release and some from
//! another, or with a program missing. So the replacement is described first,
//! in a [`ReplacementJournal`] written to disk before any file moves. Whoever
//! finds that journal later, after a crash, knows exactly which files belong
//! to it and can finish the replacement or undo it.
//!
//! [`InstallState::runtime_version`]: crate::InstallState::runtime_version

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::digest::Sha256Digest;
use crate::error::{Error, Result};
use crate::version::Version;

/// The xPack release this build is.
///
/// Every crate of the workspace carries the same version, so the programs an
/// installer places and the installer itself always agree on it.
pub const XPACK_RELEASE: &str = env!("CARGO_PKG_VERSION");

/// [`XPACK_RELEASE`], parsed.
///
/// # Errors
///
/// Only if the workspace were given a version that is not `SemVer`, which a
/// test of this crate rules out.
pub fn xpack_release() -> Result<Version> {
    Version::parse(XPACK_RELEASE)
}

/// What placing a release's runtime programs would be for an installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeChange {
    /// The incoming programs are newer, or the installation does not say
    /// which release its own came from: replace them.
    Upgrade,
    /// The same release: nothing to replace.
    Same,
    /// The incoming programs are older: keep the installed ones, which may
    /// read formats the older ones cannot.
    Downgrade,
}

/// The format of [`ReplacementJournal`] documents this build writes and reads.
///
/// A journal is read by whichever xPack next opens the installation, which
/// can be a newer or an older one. One that names a format it does not know
/// is not guessed at: see [`ReplacementJournal::validate`].
pub const REPLACEMENT_JOURNAL_FORMAT: u32 = 1;

/// A replacement of runtime programs, recorded before any of them moves.
///
/// Each program is replaced by three renames: the new one is written beside
/// it under [`Self::staged`], the old one is moved to [`Self::set_aside`], and
/// the new one is moved into place. Both names carry [`Self::id`], so a
/// program set aside by an earlier replacement that is still running (which
/// Windows does not let anyone delete) never stands in the way of this one.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplacementJournal {
    /// See [`REPLACEMENT_JOURNAL_FORMAT`].
    pub format: u32,
    /// Distinguishes this replacement's files from any other's: decimal
    /// digits, used in file names.
    pub id: String,
    /// The release whose programs are being replaced; absent when the
    /// installation did not record one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Version>,
    /// The release whose programs replace them.
    pub to: Version,
    /// One per program, in the order they are swapped.
    pub entries: Vec<ReplacementEntry>,
}

/// One program in a [`ReplacementJournal`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplacementEntry {
    /// Where the program lives, relative to the installation's root, with `/`
    /// between components: `My App` or `bin/myapp.exe`.
    pub destination: String,
    /// The new program's digest, which is how a later reader tells whether
    /// the destination already holds the new program or still the old one.
    pub sha256: Sha256Digest,
}

impl ReplacementJournal {
    /// Describes a replacement of `entries` by the programs of `to`.
    ///
    /// `id` is taken from the clock by the caller; anything that is all
    /// decimal digits will do, as long as two replacements of one
    /// installation never share it.
    pub fn new(
        id: impl Into<String>,
        from: Option<Version>,
        to: Version,
        entries: Vec<ReplacementEntry>,
    ) -> Self {
        Self { format: REPLACEMENT_JOURNAL_FORMAT, id: id.into(), from, to, entries }
    }

    /// Checks the journal is one this build can act on safely.
    ///
    /// A journal is only ever written by xPack, but it lives on disk, and the
    /// files it names are renamed by whoever recovers from it. So nothing it
    /// says is trusted: every destination must be a plain path inside the
    /// installation, and every name it leads to one this module derives.
    ///
    /// # Errors
    ///
    /// When the format is not [`REPLACEMENT_JOURNAL_FORMAT`], the id is not
    /// decimal digits, there are no entries, or an entry's destination is
    /// absolute, empty, leaves the root, names a directory component of `.`
    /// or `..`, uses `\`, appears twice, or is itself a staged or set-aside
    /// name.
    pub fn validate(&self) -> Result<()> {
        if self.format != REPLACEMENT_JOURNAL_FORMAT {
            return Err(Error::invalid(
                "replacement journal",
                format!(
                    "format {} is not one this xPack reads ({REPLACEMENT_JOURNAL_FORMAT}); \
                     run the newest installer of this application",
                    self.format
                ),
            ));
        }
        if self.id.is_empty() || !self.id.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error::invalid(
                "replacement journal",
                format!("id {:?} is not decimal digits", self.id),
            ));
        }
        if self.entries.is_empty() {
            return Err(Error::invalid("replacement journal", "it replaces nothing"));
        }
        let mut seen = BTreeSet::new();
        for entry in &self.entries {
            validate_destination(&entry.destination)?;
            if !seen.insert(entry.destination.as_str()) {
                return Err(Error::invalid(
                    "replacement journal",
                    format!("{:?} is listed twice", entry.destination),
                ));
            }
        }
        Ok(())
    }

    /// Where `entry`'s program lives, under `root`.
    pub fn destination(&self, root: &Path, entry: &ReplacementEntry) -> PathBuf {
        root.join(&entry.destination)
    }

    /// Where `entry`'s new program is written before it is moved into place:
    /// `.<name>.xpack-new-<id>`, beside the destination.
    pub fn staged(&self, root: &Path, entry: &ReplacementEntry) -> PathBuf {
        self.sibling(root, entry, |name| format!(".{name}.xpack-new-{}", self.id))
    }

    /// Where `entry`'s old program is moved before the new one takes its
    /// place: `<name>.xpack-old-<id>`, beside the destination.
    pub fn set_aside(&self, root: &Path, entry: &ReplacementEntry) -> PathBuf {
        self.sibling(root, entry, |name| format!("{name}.xpack-old-{}", self.id))
    }

    fn sibling(
        &self,
        root: &Path,
        entry: &ReplacementEntry,
        name: impl FnOnce(&str) -> String,
    ) -> PathBuf {
        let destination = self.destination(root, entry);
        let file = destination.file_name().map(|n| n.to_string_lossy().into_owned());
        destination.with_file_name(name(file.as_deref().unwrap_or_default()))
    }
}

/// Checks one destination: relative, inside the root, plain components only,
/// and not a name this module gives to a staged or set-aside file.
fn validate_destination(destination: &str) -> Result<()> {
    let refuse = |why: &str| {
        Err(Error::invalid("replacement journal", format!("destination {destination:?} {why}")))
    };
    if destination.is_empty() {
        return refuse("is empty");
    }
    // Written with `/` on every platform, so the same journal means the same
    // files wherever it is read; a `\` would be a separator on Windows only.
    if destination.contains('\\') || destination.contains('\0') {
        return refuse("contains a backslash or a NUL");
    }
    let path = Path::new(destination);
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir | Component::ParentDir => {
                return refuse("has a `.` or `..` component");
            }
            Component::RootDir | Component::Prefix(_) => return refuse("is not relative"),
        }
    }
    // Checked on the text as written, not only through `components`, which
    // quietly normalises `a//b`, a trailing `/` and a `.` in the middle into a
    // path that is not the one written.
    for part in destination.split('/') {
        match part {
            "" => return refuse("has an empty component"),
            "." | ".." => return refuse("has a `.` or `..` component"),
            _ => {}
        }
    }
    let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    if name.starts_with('.') || name.contains(".xpack-new") || name.contains(".xpack-old") {
        return refuse("is a staged or set-aside name, not a program");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: u8) -> Sha256Digest {
        Sha256Digest::from_bytes([byte; crate::SHA256_LEN])
    }

    fn entry(destination: &str) -> ReplacementEntry {
        ReplacementEntry { destination: destination.into(), sha256: digest(7) }
    }

    fn journal(destinations: &[&str]) -> ReplacementJournal {
        ReplacementJournal::new(
            "1790972582292405000",
            Some(Version::parse("0.6.1").unwrap()),
            Version::parse("0.7.0").unwrap(),
            destinations.iter().map(|d| entry(d)).collect(),
        )
    }

    #[test]
    fn the_workspace_release_is_a_version() {
        assert_eq!(xpack_release().unwrap().to_string(), XPACK_RELEASE);
    }

    #[test]
    fn a_journal_survives_being_written_and_read_back() {
        let written = journal(&["My App", "My App Updater", "bin/myapp.exe"]);
        let read: ReplacementJournal =
            serde_json::from_slice(&serde_json::to_vec(&written).unwrap()).unwrap();

        read.validate().unwrap();
        assert_eq!(read.format, REPLACEMENT_JOURNAL_FORMAT);
        assert_eq!(read.id, written.id);
        assert_eq!(read.from, written.from);
        assert_eq!(read.to, written.to);
        let destinations: Vec<_> = read.entries.iter().map(|e| e.destination.as_str()).collect();
        assert_eq!(destinations, ["My App", "My App Updater", "bin/myapp.exe"]);
        assert_eq!(read.entries[0].sha256.to_hex(), digest(7).to_hex());
    }

    #[test]
    fn an_installation_with_no_recorded_release_writes_no_from() {
        let mut written = journal(&["My App"]);
        written.from = None;
        let json = serde_json::to_string(&written).unwrap();
        assert!(!json.contains("\"from\""), "{json}");
        let read: ReplacementJournal = serde_json::from_str(&json).unwrap();
        assert_eq!(read.from, None);
    }

    #[test]
    fn staged_and_set_aside_files_sit_beside_the_program_and_carry_the_id() {
        let journal = journal(&["bin/myapp.exe"]);
        let root = Path::new("/apps/example");
        let entry = &journal.entries[0];

        assert_eq!(journal.destination(root, entry), root.join("bin/myapp.exe"));
        assert_eq!(
            journal.staged(root, entry),
            root.join("bin/.myapp.exe.xpack-new-1790972582292405000")
        );
        assert_eq!(
            journal.set_aside(root, entry),
            root.join("bin/myapp.exe.xpack-old-1790972582292405000")
        );
    }

    #[test]
    fn a_journal_from_another_format_is_not_acted_on() {
        let mut journal = journal(&["My App"]);
        journal.format = REPLACEMENT_JOURNAL_FORMAT + 1;
        let error = journal.validate().unwrap_err().to_string();
        assert!(error.contains("newest installer"), "{error}");
    }

    #[test]
    fn a_field_this_build_does_not_know_is_refused_rather_than_ignored() {
        // A newer xPack that needs more than this build reads must raise the
        // format; one that did not would still not be half understood here.
        let json = r#"{"format":1,"id":"1","to":"0.7.0","entries":[],"later":true}"#;
        assert!(serde_json::from_str::<ReplacementJournal>(json).is_err());
    }

    #[test]
    fn a_journal_cannot_name_a_file_outside_the_installation() {
        // Recovery renames whatever a journal names. One edited on disk, or
        // written by a confused process, must not make it touch anything else.
        for destination in [
            "../outside",
            "bin/../../outside",
            "/etc/passwd",
            "./My App",
            "bin/./myapp.exe",
            "bin\\myapp.exe",
            "C:\\Windows\\notepad.exe",
            "",
            "bin//myapp.exe",
            "bin/",
            "My App\0",
        ] {
            assert!(journal(&[destination]).validate().is_err(), "{destination:?} was accepted");
        }
    }

    #[test]
    fn a_journal_cannot_name_a_staged_or_set_aside_file_as_a_program() {
        for destination in
            [".My App.xpack-new-1", "My App.xpack-old-1", "bin/myapp.exe.xpack-old-12", ".hidden"]
        {
            assert!(journal(&[destination]).validate().is_err(), "{destination:?} was accepted");
        }
    }

    #[test]
    fn a_journal_needs_a_digit_id_programs_to_replace_and_no_repeats() {
        let mut bad_id = journal(&["My App"]);
        bad_id.id = "12/../3".into();
        assert!(bad_id.validate().is_err());
        bad_id.id = String::new();
        assert!(bad_id.validate().is_err());

        assert!(journal(&[]).validate().is_err());
        assert!(journal(&["My App", "My App"]).validate().is_err());
    }
}
