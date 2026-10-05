//! Installers a publisher has signed with Authenticode.
//!
//! The files in `tests/signed/` are real: a minimal PE32+ executable with a
//! payload and trailer appended the way `xpack installer` appends them, then
//! signed with osslsigncode 2.14 and a throwaway self-signed certificate.
//! `osslsigncode verify` accepts both signed files. Their payloads are the
//! two strings below.
//!
//! - `aligned.exe`: the trailer ends on an eight-byte boundary, so the
//!   signature follows it directly.
//! - `padded.exe`: one byte longer, so the signing tool put seven zero bytes,
//!   the most it ever adds, between the trailer and the signature.
//! - `unsigned.exe`: `aligned.exe` before it was signed.

use std::path::{Path, PathBuf};

use xpack_installer::bundle::{self, Source};

const ALIGNED_PAYLOAD: &[u8] = b"a payload that is not a multiple of eight bytes long";
const PADDED_PAYLOAD: &[u8] = b"a payload that is not a multiple of eight bytes long!";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/signed").join(name)
}

/// A copy of a fixture, changed by `edit`, in a directory of its own.
fn altered(name: &str, edit: impl FnOnce(&mut Vec<u8>)) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    let mut bytes = std::fs::read(fixture(name)).unwrap();
    edit(&mut bytes);
    std::fs::write(&path, bytes).unwrap();
    (dir, path)
}

/// Where the security directory entry sits in a PE32+ file.
fn security_entry(bytes: &[u8]) -> usize {
    let pe = u32::from_le_bytes(bytes[0x3C..0x40].try_into().unwrap()) as usize;
    pe + 24 + 112 + 4 * 8
}

fn payload_of(path: &Path) -> Vec<u8> {
    let source = bundle::locate(path).unwrap();
    assert!(matches!(source, Source::Appended { .. }), "got {source:?}");
    bundle::read(path, &source).unwrap()
}

#[test]
fn a_signature_is_recognised_only_where_signing_puts_one() {
    assert!(bundle::is_signed(&fixture("aligned.exe")).unwrap());
    assert!(bundle::is_signed(&fixture("padded.exe")).unwrap());
    assert!(!bundle::is_signed(&fixture("unsigned.exe")).unwrap());
    let (_dir, path) = altered("aligned.exe", |bytes| bytes.push(0));
    assert!(!bundle::is_signed(&path).unwrap());
}

#[test]
fn a_signed_installer_still_finds_its_payload() {
    assert_eq!(payload_of(&fixture("aligned.exe")), ALIGNED_PAYLOAD);
}

#[test]
fn a_signed_installer_finds_its_payload_past_the_signing_padding() {
    assert_eq!(payload_of(&fixture("padded.exe")), PADDED_PAYLOAD);
}

#[test]
fn the_same_installer_unsigned_is_read_as_before() {
    assert_eq!(payload_of(&fixture("unsigned.exe")), ALIGNED_PAYLOAD);
}

#[test]
fn a_payload_changed_after_signing_is_refused() {
    // The attack: alter the payload of a signed installer and hope the
    // signature is not checked. Windows' own check fails on this file too
    // (osslsigncode reports a digest mismatch); this is the installer's.
    let (_dir, path) = altered("padded.exe", |bytes| {
        let at = bytes.windows(PADDED_PAYLOAD.len()).position(|w| w == PADDED_PAYLOAD).unwrap();
        bytes[at] ^= 0x01;
    });
    let source = bundle::locate(&path).unwrap();
    let err = bundle::read(&path, &source).unwrap_err();
    assert!(err.to_string().contains("checksum"), "got {err}");
}

#[test]
fn bytes_added_after_the_signature_are_not_mistaken_for_it() {
    // The signature no longer ends the file, so it is not where signing puts
    // one, and the end of the file holds no trailer either.
    let (_dir, path) = altered("aligned.exe", |bytes| bytes.extend_from_slice(b"appended later"));
    let err = bundle::locate(&path).unwrap_err();
    assert!(err.to_string().contains("unbuilt stub"), "got {err}");
}

#[test]
fn padding_that_is_not_zeros_is_not_skipped() {
    // Signing tools pad with zeros. Anything else between the trailer and the
    // signature means the file was not laid out by them.
    let (_dir, path) = altered("padded.exe", |bytes| {
        let entry = security_entry(bytes);
        let start = u32::from_le_bytes(bytes[entry..entry + 4].try_into().unwrap()) as usize;
        bytes[start - 1] = 0xFF;
    });
    let err = bundle::locate(&path).unwrap_err();
    assert!(err.to_string().contains("unbuilt stub"), "got {err}");
}

#[test]
fn a_forged_signature_entry_cannot_point_outside_the_file() {
    // Every value that would make the arithmetic wrap or run past the file:
    // none may panic, read out of bounds, or produce a payload.
    for (start, size) in
        [(u32::MAX, 1), (u32::MAX, u32::MAX), (0, u32::MAX), (1, 0), (0, 0), (40_000, 1)]
    {
        let (_dir, path) = altered("aligned.exe", |bytes| {
            let entry = security_entry(bytes);
            bytes[entry..entry + 4].copy_from_slice(&start.to_le_bytes());
            bytes[entry + 4..entry + 8].copy_from_slice(&size.to_le_bytes());
        });
        assert!(bundle::locate(&path).is_err(), "start {start}, size {size} produced a payload");
    }
}

#[test]
fn a_signature_entry_covering_the_whole_file_finds_nothing() {
    // Points the signature at offset zero: everything is "signature", nothing
    // is installer, and that must not underflow.
    let (_dir, path) = altered("aligned.exe", |bytes| {
        let entry = security_entry(bytes);
        let total = u32::try_from(bytes.len()).unwrap();
        bytes[entry..entry + 4].copy_from_slice(&0u32.to_le_bytes());
        bytes[entry + 4..entry + 8].copy_from_slice(&total.to_le_bytes());
    });
    assert!(bundle::locate(&path).is_err());
}

#[test]
fn a_header_pointing_outside_the_file_is_not_a_pe() {
    let (_dir, path) = altered("unsigned.exe", |bytes| {
        bytes[0x3C..0x40].copy_from_slice(&u32::MAX.to_le_bytes());
    });
    // Not read as a PE, so the trailer is looked for at the very end, where
    // the unsigned file has it.
    assert_eq!(payload_of(&path), ALIGNED_PAYLOAD);
}
