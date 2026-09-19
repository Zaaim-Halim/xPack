//! Archive entry path validation — the defence against Zip Slip.
//!
//! An archive entry name is fully attacker-controlled. Joining it onto a
//! destination directory without validation is the "Zip Slip" vulnerability:
//! an entry called `../../../../etc/cron.d/backdoor` escapes the extraction
//! root and writes anywhere the process can reach. For an installer that runs
//! at user login, that is a complete compromise.
//!
//! This module is the only place in xPack that turns an archive name into a
//! path, and it rejects rather than sanitises. Silently rewriting a hostile
//! name would make a tampered package install "successfully", when the correct
//! answer is to refuse the package outright.

use std::path::{Component, Path, PathBuf};

use xpack_core::manifest::{MAX_PAYLOAD_PATH_LEN, PAYLOAD_PREFIX};
use xpack_core::{Error, Result};

/// Longest raw entry name accepted, including the `payload/` prefix.
///
/// Bounds work before the prefix is stripped. The relative path that remains is
/// held to [`MAX_PAYLOAD_PATH_LEN`], the same limit the manifest enforces, so a
/// package cannot validate at build time and then fail to extract.
const MAX_ENTRY_LEN: usize = MAX_PAYLOAD_PATH_LEN + PAYLOAD_PREFIX.len();

/// Windows device names, which resolve to hardware rather than to a file.
///
/// These are reserved with *any* extension and in any case, so `CON`, `con.txt`
/// and `CoN.tar.gz` are all refused. Writing to one on Windows can hang the
/// installer or emit data to a serial port.
const WINDOWS_RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// An archive entry name proven safe to join onto an extraction root.
///
/// The only constructor is [`safe_payload_path`], so a value of this type
/// cannot exist without having passed every check.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SafePath {
    relative: String,
}

impl SafePath {
    /// The validated path, relative to the version root, using `/` separators.
    ///
    /// This is the form that appears in the signed manifest, so manifest
    /// lookups and archive lookups always agree.
    pub fn as_str(&self) -> &str {
        &self.relative
    }

    /// Resolves the entry against an extraction root.
    ///
    /// The join is safe because every component was validated to be a plain
    /// name: no `..`, no root, no prefix, no device name.
    pub fn resolve(&self, root: &Path) -> PathBuf {
        let mut out = root.to_path_buf();
        for component in self.relative.split('/') {
            out.push(component);
        }
        out
    }
}

/// Validates a `payload/`-prefixed archive entry name.
///
/// Returns `Ok(None)` for the payload directory entry itself, which is
/// structural rather than a file.
pub fn safe_payload_path(entry: &str) -> Result<Option<SafePath>> {
    let reject = |reason: &str| -> Error {
        Error::UnsafeEntry { entry: entry.to_string(), reason: reason.to_string() }
    };

    if entry.len() > MAX_ENTRY_LEN {
        return Err(reject("entry name exceeds the maximum length"));
    }
    if entry.contains('\0') {
        return Err(reject("entry name contains a NUL byte"));
    }

    let Some(relative) = entry.strip_prefix(PAYLOAD_PREFIX) else {
        return Err(reject("entry is outside the payload/ directory"));
    };
    if relative.is_empty() {
        return Ok(None);
    }

    validate_relative(relative, &reject).map(Some)
}

fn validate_relative(relative: &str, reject: &impl Fn(&str) -> Error) -> Result<SafePath> {
    // Backslash is a separator on Windows, so an entry written with backslashes
    // would bypass a check that only ever looked at '/'. Normalise first, then
    // validate the normalised form — never the other way round.
    let normalised = relative.replace('\\', "/");

    if normalised.len() > MAX_PAYLOAD_PATH_LEN {
        return Err(reject("entry path exceeds the maximum length"));
    }
    if normalised.starts_with('/') {
        return Err(reject("entry is an absolute path"));
    }
    // `C:foo` is drive-relative on Windows and resolves against that drive's
    // current directory, which is outside the extraction root.
    if normalised.len() >= 2 && normalised.as_bytes()[1] == b':' {
        return Err(reject("entry is drive-qualified"));
    }

    let mut components = Vec::new();
    for part in normalised.split('/') {
        match part {
            "" => return Err(reject("entry contains an empty path component")),
            "." => return Err(reject("entry contains a '.' component")),
            ".." => return Err(reject("entry escapes the extraction root via '..'")),
            other => {
                validate_component(other, reject)?;
                components.push(other);
            }
        }
    }

    if components.is_empty() {
        return Err(reject("entry has no path components"));
    }

    let rebuilt = components.join("/");

    // Belt and braces: ask the platform's own path parser whether the result
    // still contains anything but plain names. This catches any encoding the
    // string checks above did not anticipate.
    for component in Path::new(&rebuilt).components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(reject("entry resolves to something other than a plain relative path"));
        }
    }

    Ok(SafePath { relative: rebuilt })
}

fn validate_component(component: &str, reject: &impl Fn(&str) -> Error) -> Result<()> {
    // Windows strips trailing dots and spaces when resolving a name, so
    // `evil.exe.` and `evil.exe ` both open `evil.exe`. Allowing them would let
    // one manifest entry's verified digest be written to a different file.
    if component.ends_with('.') || component.ends_with(' ') {
        return Err(reject("entry component ends with a dot or space, which Windows strips"));
    }
    if component.chars().any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*')) {
        return Err(reject("entry component contains a character Windows cannot represent"));
    }
    if component.chars().any(|c| (c as u32) < 0x20) {
        return Err(reject("entry component contains a control character"));
    }

    let stem = component.split('.').next().unwrap_or(component);
    if WINDOWS_RESERVED.iter().any(|r| stem.eq_ignore_ascii_case(r)) {
        return Err(reject("entry component is a reserved Windows device name"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(entry: &str) -> SafePath {
        safe_payload_path(entry)
            .unwrap_or_else(|e| panic!("{entry:?} should be accepted: {e}"))
            .unwrap_or_else(|| panic!("{entry:?} should be a file"))
    }

    fn rejected(entry: &str) {
        assert!(safe_payload_path(entry).is_err(), "{entry:?} should have been rejected");
    }

    #[test]
    fn accepts_ordinary_nested_paths() {
        assert_eq!(ok("payload/application/app.jar").as_str(), "application/app.jar");
        assert_eq!(ok("payload/a").as_str(), "a");
        assert_eq!(ok("payload/runtime/bin/java").as_str(), "runtime/bin/java");
    }

    #[test]
    fn treats_the_payload_directory_entry_as_structural() {
        assert_eq!(safe_payload_path("payload/").unwrap(), None);
    }

    #[test]
    fn rejects_classic_zip_slip_payloads() {
        rejected("payload/../../../../etc/cron.d/backdoor");
        rejected("payload/../evil");
        rejected("payload/a/../../b");
        rejected("payload/a/b/../../../c");
    }

    #[test]
    fn rejects_backslash_traversal_that_dodges_slash_only_checks() {
        rejected(r"payload/..\..\Windows\System32\evil.dll");
        rejected(r"payload/a\..\..\b");
    }

    #[test]
    fn rejects_absolute_and_drive_qualified_paths() {
        rejected("payload//etc/passwd");
        rejected("payload/C:/Windows/evil.dll");
        rejected("payload/C:evil");
    }

    #[test]
    fn rejects_entries_outside_the_payload_directory() {
        rejected("manifest.json");
        rejected("../manifest.json");
        rejected("payloadx/file");
        rejected("");
    }

    #[test]
    fn rejects_windows_reserved_device_names_with_any_extension() {
        rejected("payload/CON");
        rejected("payload/con.txt");
        rejected("payload/nested/AUX.tar.gz");
        rejected("payload/LPT1");
        rejected("payload/CoM9.dat");
        // A name that merely starts with a reserved stem is fine.
        ok("payload/console.log");
        ok("payload/aux-data.bin");
    }

    #[test]
    fn rejects_trailing_dots_and_spaces_that_windows_silently_strips() {
        rejected("payload/evil.exe.");
        rejected("payload/evil.exe ");
        rejected("payload/dir./file");
    }

    #[test]
    fn rejects_characters_windows_cannot_represent() {
        for bad in ["a<b", "a>b", "a\"b", "a|b", "a?b", "a*b"] {
            rejected(&format!("payload/{bad}"));
        }
    }

    #[test]
    fn rejects_control_characters_and_nul() {
        rejected("payload/a\u{1}b");
        rejected("payload/a\0b");
    }

    #[test]
    fn rejects_absurdly_long_entry_names() {
        rejected(&format!("payload/{}", "a".repeat(MAX_ENTRY_LEN)));
    }

    #[test]
    fn enforces_the_same_path_limit_the_manifest_does() {
        // One rule, one limit. A path the manifest accepts must be extractable.
        rejected(&format!("payload/{}", "a".repeat(MAX_PAYLOAD_PATH_LEN + 1)));
        ok(&format!("payload/{}", "a".repeat(MAX_PAYLOAD_PATH_LEN)));
    }

    #[test]
    fn every_accepted_entry_resolves_inside_the_root() {
        let root = Path::new("/install/versions/1.0.0");
        for entry in ["payload/a", "payload/a/b/c.txt", "payload/runtime/bin/java"] {
            let resolved = ok(entry).resolve(root);
            assert!(resolved.starts_with(root), "{} escaped the root", resolved.display());
            assert!(resolved.components().all(|c| !matches!(c, Component::ParentDir)));
        }
    }
}
