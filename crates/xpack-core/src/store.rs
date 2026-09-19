//! Corruption-tolerant storage for small state documents.
//!
//! [`crate::atomic`] guarantees that a write is all-or-nothing. It cannot
//! guarantee that what was written is still readable months later: disks
//! develop bad sectors, filesystems lose blocks after an unclean shutdown, and
//! backup tooling truncates files. A state document that fails to parse leaves
//! an installation with no resolvable version — the exact failure this system
//! exists to prevent.
//!
//! This module adds two cheap defences on top of atomic writes:
//!
//! 1. **A checksum**, so corruption is detected as corruption rather than
//!    surfacing as a confusing parse error halfway through a field.
//! 2. **A backup copy**, rotated only from a document that verified, so a
//!    corrupt primary can fall back to the last known-good state instead of
//!    failing the operation.
//!
//! # The checksum is not a security control
//!
//! CRC-32 is used deliberately, and a cryptographic hash would be *worse* —
//! not because it is slower, but because it would imply a guarantee that does
//! not exist. Anyone able to modify the state file can recompute any checksum
//! stored beside it, so no checksum here defends against tampering. The threat
//! being addressed is accidental corruption, and CRC-32 detects that
//! comprehensively while adding no dependency.
//!
//! Authenticity of *packages* is established by Ed25519 signatures over signed
//! manifests. This file is local bookkeeping and is not part of that chain.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::value::RawValue;

use crate::atomic;
use crate::error::{Error, Result};

/// Envelope format version, so the wrapper itself can evolve.
const ENVELOPE_VERSION: u32 = 1;

/// A document read back from disk, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded<T> {
    /// The deserialised document.
    pub value: T,
    /// `true` when the primary file was unusable and the backup was used.
    ///
    /// Callers must treat this as a loud warning: the installation is running
    /// on recovered state and the disk may be failing.
    pub recovered_from_backup: bool,
}

/// Returns the backup path that accompanies `path`.
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    path.with_file_name(name)
}

/// Writes `value` atomically, rotating the previous good copy to `<path>.bak`.
///
/// Rotation happens **only** when the existing primary still verifies. Copying
/// a corrupt primary over the backup would destroy the one good copy left.
pub fn save<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let body =
        // Pretty-printed deliberately: the reason state lives in JSON rather
        // than a database is that a human can read and repair it.
        serde_json::to_string_pretty(value)
            .map_err(|e| Error::json(path.display().to_string(), e))?;
    let checksum = format!("crc32:{:08x}", crc32(body.as_bytes()));

    let document =
        RawValue::from_string(body).map_err(|e| Error::json(path.display().to_string(), e))?;
    let envelope = Envelope { version: ENVELOPE_VERSION, checksum, document: &document };

    if let Ok(existing) = std::fs::read(path)
        && verify_envelope(&existing).is_ok()
    {
        atomic::write(&backup_path(path), &existing)?;
    }

    let mut bytes = serde_json::to_vec_pretty(&envelope)
        .map_err(|e| Error::json(path.display().to_string(), e))?;
    bytes.push(b'\n');
    atomic::write(path, &bytes)
}

/// Reads a document, falling back to the backup if the primary is unusable.
pub fn load<T: DeserializeOwned>(path: &Path) -> Result<Loaded<T>> {
    let primary = read_verified::<T>(path);
    match primary {
        Ok(value) => Ok(Loaded { value, recovered_from_backup: false }),
        Err(primary_error) => {
            let backup = backup_path(path);
            match read_verified::<T>(&backup) {
                Ok(value) => Ok(Loaded { value, recovered_from_backup: true }),
                // Report the *primary* failure: the backup is a fallback, and
                // saying "backup not found" would hide the real problem.
                Err(_) => Err(primary_error),
            }
        }
    }
}

/// Returns `true` when a readable, verifying document exists at `path`.
pub fn exists(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|bytes| verify_envelope(&bytes).is_ok())
}

fn read_verified<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
    let document = verify_envelope(&bytes).map_err(|reason| {
        Error::invalid("state document", format!("{}: {reason}", path.display()))
    })?;
    serde_json::from_str(&document).map_err(|e| Error::json(path.display().to_string(), e))
}

/// Parses an envelope and checks its checksum, returning the document JSON.
fn verify_envelope(bytes: &[u8]) -> std::result::Result<String, String> {
    let envelope: OwnedEnvelope =
        serde_json::from_slice(bytes).map_err(|e| format!("is not a valid envelope: {e}"))?;

    if envelope.version > ENVELOPE_VERSION {
        return Err(format!(
            "envelope version {} is newer than the supported version {ENVELOPE_VERSION}",
            envelope.version
        ));
    }

    let body = envelope.document.get();
    let expected = format!("crc32:{:08x}", crc32(body.as_bytes()));
    if envelope.checksum != expected {
        return Err(format!(
            "checksum mismatch (recorded {}, computed {expected}); the file is corrupt",
            envelope.checksum
        ));
    }

    Ok(body.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Envelope<'doc> {
    #[serde(rename = "envelopeVersion")]
    version: u32,
    checksum: String,
    document: &'doc RawValue,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OwnedEnvelope {
    #[serde(rename = "envelopeVersion")]
    version: u32,
    checksum: String,
    document: Box<RawValue>,
}

/// CRC-32 (IEEE 802.3, reflected polynomial `0xEDB88320`).
///
/// Implemented inline rather than pulled in as a dependency: it is fifteen
/// lines, it is a frozen standard, and the check value below pins it.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct Doc {
        name: String,
        count: u32,
    }

    fn doc(count: u32) -> Doc {
        Doc { name: "state".into(), count }
    }

    #[test]
    fn crc32_matches_the_standard_check_value() {
        // The check value published with the CRC-32 specification.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn round_trips_a_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        save(&path, &doc(1)).unwrap();

        let loaded: Loaded<Doc> = load(&path).unwrap();
        assert_eq!(loaded.value, doc(1));
        assert!(!loaded.recovered_from_backup);
    }

    #[test]
    fn first_save_creates_no_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        save(&path, &doc(1)).unwrap();
        assert!(!backup_path(&path).exists(), "nothing good existed to back up yet");
    }

    #[test]
    fn second_save_rotates_the_previous_document_to_the_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        save(&path, &doc(1)).unwrap();
        save(&path, &doc(2)).unwrap();

        assert_eq!(load::<Doc>(&path).unwrap().value, doc(2));
        assert_eq!(read_verified::<Doc>(&backup_path(&path)).unwrap(), doc(1));
    }

    #[test]
    fn detects_a_single_flipped_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        save(&path, &doc(7)).unwrap();

        let mut bytes = std::fs::read(&path).unwrap();
        let pos = bytes.windows(1).position(|w| w == b"7").expect("value byte present");
        bytes[pos] = b'8';
        std::fs::write(&path, &bytes).unwrap();

        let err = read_verified::<Doc>(&path).unwrap_err().to_string();
        assert!(err.contains("checksum mismatch"), "got {err}");
    }

    #[test]
    fn recovers_from_the_backup_when_the_primary_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        save(&path, &doc(1)).unwrap();
        save(&path, &doc(2)).unwrap();

        std::fs::write(&path, b"{ this is not json").unwrap();

        let loaded: Loaded<Doc> = load(&path).unwrap();
        assert_eq!(loaded.value, doc(1));
        assert!(loaded.recovered_from_backup, "caller must be told recovery happened");
    }

    #[test]
    fn recovers_from_a_truncated_primary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        save(&path, &doc(1)).unwrap();
        save(&path, &doc(2)).unwrap();

        // The classic power-loss artefact on a filesystem without atomicity.
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();

        assert_eq!(load::<Doc>(&path).unwrap().value, doc(1));
    }

    #[test]
    fn a_corrupt_primary_never_overwrites_a_good_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        save(&path, &doc(1)).unwrap();
        save(&path, &doc(2)).unwrap();
        std::fs::write(&path, b"corrupt").unwrap();

        // Saving again must not rotate the corrupt primary over the backup,
        // which would destroy the only remaining good copy.
        save(&path, &doc(3)).unwrap();
        assert_eq!(read_verified::<Doc>(&backup_path(&path)).unwrap(), doc(1));
        assert_eq!(load::<Doc>(&path).unwrap().value, doc(3));
    }

    #[test]
    fn reports_the_primary_failure_when_both_copies_are_unusable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        save(&path, &doc(1)).unwrap();
        save(&path, &doc(2)).unwrap();
        std::fs::write(&path, b"corrupt").unwrap();
        std::fs::write(backup_path(&path), b"also corrupt").unwrap();

        let err = load::<Doc>(&path).unwrap_err().to_string();
        assert!(err.contains("state.json"), "error must name the primary: {err}");
        assert!(!err.contains(".bak"), "must not blame the backup: {err}");
    }

    #[test]
    fn a_missing_document_is_an_ordinary_not_found_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.json");
        assert!(load::<Doc>(&path).is_err());
        assert!(!exists(&path));
    }

    #[test]
    fn rejects_an_envelope_from_a_newer_xpack() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            br#"{"envelopeVersion":99,"checksum":"crc32:00000000","document":{}}"#,
        )
        .unwrap();
        let err = read_verified::<Doc>(&path).unwrap_err().to_string();
        assert!(err.contains("newer than the supported version"), "got {err}");
    }

    #[test]
    fn exists_is_false_for_a_corrupt_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        save(&path, &doc(1)).unwrap();
        assert!(exists(&path));
        std::fs::write(&path, b"corrupt").unwrap();
        assert!(!exists(&path));
    }
}
