//! The payload an installer carries, and how it finds it.
//!
//! An installer is one file a user runs. Everything it needs — the signed
//! package, the launcher, the updater, the uninstaller, and the publisher's
//! key — travels inside it.
//!
//! # Two layouts, one stub
//!
//! ```text
//! Windows and Linux          macOS
//! ─────────────────          ─────
//! MyApp-installer.exe        MyApp Installer.app/
//!   ├─ stub                    └─ Contents/
//!   ├─ payload                      ├─ MacOS/installer      (the stub)
//!   └─ trailer                      └─ Resources/payload    (the bundle)
//! ```
//!
//! The difference is not a preference. Appending to a Mach-O leaves a file
//! that still *runs* — the kernel validates the pages it maps, and the extra
//! bytes are not mapped — but its code signature no longer covers the whole
//! file, so `codesign` rejects it and re-signing does not repair it. A
//! downloaded installer must satisfy Gatekeeper, and an unsignable one never
//! will. Measured, not assumed.
//!
//! So on macOS the executable is left pristine and the payload sits beside it
//! in the bundle, which is the shape Apple's tooling expects anyway. One stub
//! serves both: it looks for an appended payload, then for a sidecar.
//!
//! # The trailer is read from the end, never searched for
//!
//! A fixed-size record at the very end of the file. Searching for the magic
//! instead would be a bug waiting to happen: the constant is compiled into the
//! stub, so the stub's own read-only data contains it, and so might a
//! compressed payload by coincidence.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use xpack_core::digest::Sha256Digest;
use xpack_core::{Error, Result};

/// Marks the end of an installer that carries an appended payload.
pub const MAGIC: [u8; 8] = *b"XPACKBDL";

/// Format of the trailer this build writes and understands.
pub const TRAILER_VERSION: u32 = 1;

/// Size of the fixed record at the end of an appended installer.
///
/// magic (8) + version (4) + payload length (8) + payload digest (32).
///
/// A `usize` because every use is a buffer length or a slice index; the two
/// places that need it as a file offset convert explicitly.
pub const TRAILER_LEN: usize = 8 + 4 + 8 + 32;

/// File name of the payload when it sits beside the stub rather than inside it.
pub const SIDECAR_NAME: &str = "xpack-payload.bundle";

/// Entry in the payload archive holding the install plan.
pub const PLAN_ENTRY: &str = "install.json";

/// Entry in the payload archive holding the application package.
pub const PACKAGE_ENTRY: &str = "application.xpkg";

/// Entry in the payload archive holding the licence the wizard shows.
///
/// The one piece of the wizard's content the signed package does not carry.
/// Everything else it shows — name, publisher, icon — is read from the
/// verified package itself.
pub const LICENCE_ENTRY: &str = "ui/license.txt";

/// The largest licence carried, in bytes. Longer than any licence a person
/// reads in a wizard, and short enough that a window can show it whole.
pub const MAX_LICENCE_BYTES: usize = 256 * 1024;

/// Directory in the payload archive holding the xPack runtime binaries.
pub const BINARY_PREFIX: &str = "bin/";

/// What the installer should do once it has its payload.
///
/// Written at build time by `xpack installer` and read by the stub. It is
/// **not** a trust input: the package inside is verified by its own Ed25519
/// signature against [`Self::signing_key`], which is what makes the whole
/// artefact trustworthy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallPlan {
    /// Format of this document.
    pub format_version: u32,
    /// Application being installed, for reporting before the package is read.
    pub application_id: String,
    /// Display name, for what the installer prints.
    pub application_name: String,
    /// Version carried by the package.
    pub version: String,
    /// Hex-encoded public key the package must verify against.
    ///
    /// # Why the key is pinned here rather than trusted on first use
    ///
    /// Trust-on-first-use accepts whatever signed the first package it sees,
    /// which is exactly the download an attacker who can tamper with a
    /// download would tamper with. Carrying the key in the artefact the
    /// publisher built and signed means the installer verifies against the
    /// publisher's key or refuses — and the same key is pinned into the
    /// installation, so every later update is held to it too.
    pub signing_key: String,
    /// Whether the installed version should be made active immediately.
    pub activate: bool,
    /// How the installation wizard looks, where the publisher said.
    ///
    /// Absent, the wizard is the recommended one and every choice in it
    /// defaults to what the installer does without a window. Not a trust
    /// input either: nothing in it can change what gets installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<xpack_installer_ui::UiPlan>,
}

impl InstallPlan {
    /// The newest format this build reads and writes.
    pub const CURRENT_VERSION: u32 = 2;

    /// The format a plan with these settings is written in.
    ///
    /// The oldest one that can express them, so that a plan with no wizard
    /// settings stays readable by a stub built before they existed.
    pub fn format_for(ui: Option<&xpack_installer_ui::UiPlan>) -> u32 {
        if ui.is_some() { Self::CURRENT_VERSION } else { 1 }
    }

    /// Rejects a plan from a newer installer than this one understands.
    pub fn ensure_supported(&self) -> Result<()> {
        if self.format_version > Self::CURRENT_VERSION {
            return Err(Error::invalid(
                "install plan",
                format!(
                    "is format {} but this installer understands {}",
                    self.format_version,
                    Self::CURRENT_VERSION
                ),
            ));
        }
        Ok(())
    }
}

/// The fixed record at the end of an appended installer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trailer {
    /// Length of the payload immediately preceding this record.
    pub payload_len: u64,
    /// SHA-256 of those bytes.
    pub payload_sha256: Sha256Digest,
}

impl Trailer {
    /// Serialises the record.
    pub fn to_bytes(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(TRAILER_LEN);
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&TRAILER_VERSION.to_le_bytes());
        out.extend_from_slice(&self.payload_len.to_le_bytes());
        out.extend_from_slice(self.payload_sha256.as_bytes());
        out
    }

    /// Parses the last [`TRAILER_LEN`] bytes of a file.
    ///
    /// Returns `None` rather than an error when the magic is absent: a stub
    /// with no payload appended is not corrupt, it is a stub that expects its
    /// payload beside it instead.
    ///
    /// A *present* magic with anything else wrong is an error, because at that
    /// point the file claims to be an installer and is not one.
    pub fn parse(bytes: &[u8]) -> Result<Option<Self>> {
        if bytes.len() != TRAILER_LEN || bytes[..8] != MAGIC {
            return Ok(None);
        }

        let version = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        if version != TRAILER_VERSION {
            return Err(Error::invalid(
                "installer",
                format!(
                    "carries a format {version} payload; this build understands {TRAILER_VERSION}"
                ),
            ));
        }

        let payload_len = u64::from_le_bytes([
            bytes[12], bytes[13], bytes[14], bytes[15], bytes[16], bytes[17], bytes[18], bytes[19],
        ]);
        if payload_len == 0 {
            return Err(Error::invalid("installer", "declares an empty payload"));
        }

        let mut digest = [0u8; 32];
        digest.copy_from_slice(&bytes[20..52]);

        Ok(Some(Self { payload_len, payload_sha256: Sha256Digest::from_bytes(digest) }))
    }
}

/// Where a payload was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Appended to the running executable, at this offset.
    Appended {
        /// Byte offset of the payload within the executable.
        offset: u64,
        /// Length of the payload.
        length: u64,
        /// Digest the trailer claims.
        sha256: Sha256Digest,
    },
    /// A separate file beside the executable.
    Sidecar(PathBuf),
}

/// Paths a stub looks in for a sidecar payload, in order.
///
/// A pure function of the executable's own path, so the macOS bundle layout
/// can be tested on any host — the same reason `Roots` and
/// `ensure_launch_paths_fit` take their inputs rather than reading the
/// environment.
///
/// `Contents/Resources` comes first because that is where the payload lives in
/// an application bundle, and a bundle is the only layout on which a sidecar
/// is the *expected* arrangement rather than a fallback.
pub fn sidecar_candidates(executable: &Path) -> Vec<PathBuf> {
    let Some(directory) = executable.parent() else {
        return Vec::new();
    };

    let mut candidates = Vec::new();

    // `…/Contents/MacOS/installer` -> `…/Contents/Resources/<payload>`
    if directory.file_name().is_some_and(|name| name == "MacOS")
        && let Some(contents) = directory.parent()
    {
        candidates.push(contents.join("Resources").join(SIDECAR_NAME));
    }

    candidates.push(directory.join(SIDECAR_NAME));
    candidates
}

/// Finds this installer's payload.
///
/// Appended first, then beside the executable. Both are legitimate; which one
/// is used is a property of how the installer was built, not of what the user
/// did.
pub fn locate(executable: &Path) -> Result<Source> {
    if let Some(appended) = locate_appended(executable)? {
        return Ok(appended);
    }

    for candidate in sidecar_candidates(executable) {
        if candidate.is_file() {
            return Ok(Source::Sidecar(candidate));
        }
    }

    Err(Error::invalid(
        "installer",
        format!(
            "carries no payload and none was found beside {}; this file is an unbuilt stub \
             rather than a finished installer",
            executable.display()
        ),
    ))
}

/// Reads the trailer from the end of `executable`, if there is one.
fn locate_appended(executable: &Path) -> Result<Option<Source>> {
    let mut file = std::fs::File::open(executable).map_err(|e| Error::io(executable, e))?;
    let total = file.metadata().map_err(|e| Error::io(executable, e))?.len();
    let trailer_len = u64::try_from(TRAILER_LEN).expect("the trailer is a few dozen bytes");
    if total <= trailer_len {
        return Ok(None);
    }

    // Seek from the end. Searching the file for the magic would find the copy
    // compiled into this very binary.
    let back = i64::try_from(TRAILER_LEN).expect("the trailer is a few dozen bytes");
    file.seek(SeekFrom::End(-back)).map_err(|e| Error::io(executable, e))?;
    let mut bytes = vec![0u8; TRAILER_LEN];
    file.read_exact(&mut bytes).map_err(|e| Error::io(executable, e))?;

    let Some(trailer) = Trailer::parse(&bytes)? else {
        return Ok(None);
    };

    let payload_end = total - trailer_len;
    let offset = payload_end.checked_sub(trailer.payload_len).ok_or_else(|| {
        Error::invalid(
            "installer",
            format!(
                "declares a {}-byte payload but the file is only {total} bytes; it is truncated",
                trailer.payload_len
            ),
        )
    })?;

    Ok(Some(Source::Appended {
        offset,
        length: trailer.payload_len,
        sha256: trailer.payload_sha256,
    }))
}

/// Reads the payload bytes a [`Source`] names, checking the declared digest.
///
/// The digest is an integrity check on the container, not a trust decision: it
/// catches a truncated or corrupted download, which would otherwise surface as
/// a confusing archive error halfway through installing. What makes the
/// contents *trustworthy* is the Ed25519 signature on the package inside,
/// verified against the key the plan pins.
pub fn read(executable: &Path, source: &Source) -> Result<Vec<u8>> {
    match source {
        Source::Appended { offset, length, sha256 } => {
            let mut file = std::fs::File::open(executable).map_err(|e| Error::io(executable, e))?;
            file.seek(SeekFrom::Start(*offset)).map_err(|e| Error::io(executable, e))?;

            let length = usize::try_from(*length).map_err(|_| {
                Error::invalid("installer", "payload is too large for this machine")
            })?;
            let mut bytes = vec![0u8; length];
            file.read_exact(&mut bytes).map_err(|e| Error::io(executable, e))?;

            let actual = xpack_security::sha256(&bytes);
            if actual != *sha256 {
                return Err(Error::Integrity(
                    "this installer's payload does not match its own checksum; the download is \
                     corrupt or incomplete"
                        .to_string(),
                ));
            }
            Ok(bytes)
        }
        Source::Sidecar(path) => std::fs::read(path).map_err(|e| Error::io(path, e)),
    }
}

/// Builds the payload archive an installer carries.
///
/// Lives beside the code that reads it, deliberately. A writer in one crate
/// and a reader in another is how an entry name or a compression choice comes
/// to disagree, and the symptom would be an installer that fails on a user's
/// machine and nowhere else.
pub fn build(plan: &InstallPlan, package: &Path, binaries: &[PathBuf]) -> Result<Vec<u8>> {
    build_with_licence(plan, package, binaries, None)
}

/// [`build`], also carrying the licence the wizard shows.
pub fn build_with_licence(
    plan: &InstallPlan,
    package: &Path,
    binaries: &[PathBuf],
    licence: Option<&str>,
) -> Result<Vec<u8>> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    // A fixed timestamp, for the same reason a package uses one: two
    // installers built from the same inputs should be identical files.
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::DEFAULT);

    let document = serde_json::to_vec_pretty(plan).map_err(|e| Error::json("install plan", e))?;
    writer.start_file(PLAN_ENTRY, options).map_err(|e| zip_error(&e))?;
    writer.write_all(&document).map_err(|e| Error::io(Path::new(PLAN_ENTRY), e))?;

    let package_bytes = std::fs::read(package).map_err(|e| Error::io(package, e))?;
    writer.start_file(PACKAGE_ENTRY, options).map_err(|e| zip_error(&e))?;
    writer.write_all(&package_bytes).map_err(|e| Error::io(package, e))?;

    for binary in binaries {
        let name = binary
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::invalid("binary", format!("{} has no name", binary.display())))?;
        let bytes = std::fs::read(binary).map_err(|e| Error::io(binary, e))?;
        if bytes.is_empty() {
            return Err(Error::invalid(
                "binary",
                format!("{} is empty and cannot be an executable", binary.display()),
            ));
        }
        writer.start_file(format!("{BINARY_PREFIX}{name}"), options).map_err(|e| zip_error(&e))?;
        writer.write_all(&bytes).map_err(|e| Error::io(binary, e))?;
    }

    if let Some(licence) = licence {
        check_licence(licence.as_bytes())?;
        writer.start_file(LICENCE_ENTRY, options).map_err(|e| zip_error(&e))?;
        writer.write_all(licence.as_bytes()).map_err(|e| Error::io(Path::new(LICENCE_ENTRY), e))?;
    }

    let cursor = writer.finish().map_err(|e| zip_error(&e))?;
    Ok(cursor.into_inner())
}

/// Checks licence text: present, within the size limit, and UTF-8.
///
/// Applied when an installer is built, so a publisher hears about it, and
/// again when one is unpacked, because the payload is not signed.
pub fn check_licence(bytes: &[u8]) -> Result<&str> {
    if bytes.len() > MAX_LICENCE_BYTES {
        return Err(Error::invalid(
            "licence",
            format!("is {} bytes; the limit is {MAX_LICENCE_BYTES}", bytes.len()),
        ));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|e| Error::invalid("licence", format!("is not UTF-8 text: {e}")))?;
    if text.trim().is_empty() {
        return Err(Error::invalid("licence", "is empty"));
    }
    Ok(text)
}

fn zip_error(e: &zip::result::ZipError) -> Error {
    Error::invalid("installer payload", e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trailer() -> Trailer {
        Trailer { payload_len: 1234, payload_sha256: Sha256Digest::from_bytes([7; 32]) }
    }

    #[test]
    fn a_trailer_round_trips() {
        let bytes = trailer().to_bytes();
        assert_eq!(bytes.len(), TRAILER_LEN);
        assert_eq!(Trailer::parse(&bytes).unwrap(), Some(trailer()));
    }

    #[test]
    fn a_plain_stub_is_not_treated_as_corrupt() {
        // A stub with no payload is not broken; its payload is meant to be
        // beside it. Reporting corruption would send someone hunting a
        // download problem that does not exist.
        let bytes = vec![0u8; TRAILER_LEN];
        assert_eq!(Trailer::parse(&bytes).unwrap(), None);
    }

    #[test]
    fn a_short_read_is_not_a_trailer() {
        assert_eq!(Trailer::parse(&[]).unwrap(), None);
        assert_eq!(Trailer::parse(&MAGIC).unwrap(), None);
    }

    #[test]
    fn a_newer_trailer_format_is_refused_rather_than_misread() {
        let mut bytes = trailer().to_bytes();
        bytes[8..12].copy_from_slice(&(TRAILER_VERSION + 1).to_le_bytes());

        let err = Trailer::parse(&bytes).unwrap_err();
        assert!(err.to_string().contains("understands"), "got {err}");
    }

    #[test]
    fn an_empty_payload_is_refused() {
        let mut bytes = trailer().to_bytes();
        bytes[12..20].copy_from_slice(&0u64.to_le_bytes());
        assert!(Trailer::parse(&bytes).is_err());
    }

    #[test]
    fn a_bundle_looks_in_resources_before_beside_the_executable() {
        // The macOS layout, testable on any host because the path is the only
        // input.
        let exe = Path::new("/Applications/My Installer.app/Contents/MacOS/installer");
        let candidates = sidecar_candidates(exe);

        assert_eq!(
            candidates[0],
            Path::new("/Applications/My Installer.app/Contents/Resources").join(SIDECAR_NAME)
        );
        assert_eq!(
            candidates[1],
            Path::new("/Applications/My Installer.app/Contents/MacOS").join(SIDECAR_NAME)
        );
    }

    #[test]
    fn an_ordinary_executable_looks_only_beside_itself() {
        let candidates = sidecar_candidates(Path::new("/opt/tools/installer"));
        assert_eq!(candidates, vec![Path::new("/opt/tools").join(SIDECAR_NAME)]);
    }

    #[test]
    fn a_plan_from_a_newer_installer_is_refused() {
        let plan = InstallPlan {
            format_version: InstallPlan::CURRENT_VERSION + 1,
            application_id: "com.example.app".into(),
            application_name: "Example".into(),
            version: "1.0.0".into(),
            signing_key: "00".repeat(32),
            activate: true,
            ui: None,
        };
        assert!(plan.ensure_supported().is_err());
    }

    /// A format 1 plan, exactly as a stub built before wizard settings wrote it.
    const FORMAT_ONE: &str = r#"{
        "formatVersion": 1,
        "applicationId": "com.example.app",
        "applicationName": "Example",
        "version": "1.0.0",
        "signingKey": "0000000000000000000000000000000000000000000000000000000000000000",
        "activate": true
    }"#;

    #[test]
    fn a_plan_from_before_wizard_settings_is_still_read() {
        let plan: InstallPlan = serde_json::from_str(FORMAT_ONE).expect("a format 1 plan");
        plan.ensure_supported().expect("still supported");
        assert_eq!(plan.ui, None);
    }

    #[test]
    fn a_plan_without_wizard_settings_is_written_in_the_old_format() {
        // So a stub built before the settings existed can still read it: that
        // stub refuses any field it does not know, and any newer format.
        assert_eq!(InstallPlan::format_for(None), 1);
        let plan: InstallPlan = serde_json::from_str(FORMAT_ONE).expect("a format 1 plan");
        let written = serde_json::to_string(&plan).expect("serialises");
        assert!(!written.contains("\"ui\""), "{written}");
    }

    #[test]
    fn a_plan_with_wizard_settings_says_so_in_its_format() {
        let ui = xpack_installer_ui::UiPlan::default();
        assert_eq!(InstallPlan::format_for(Some(&ui)), 2);
    }
}
