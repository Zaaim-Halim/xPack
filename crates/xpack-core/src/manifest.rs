//! The package manifest: the document that a package signature covers.
//!
//! # Why the manifest is the trust anchor
//!
//! An xPack signature is computed over the **raw bytes** of `manifest.json` as
//! they appear in the archive — never over a re-serialised form. Anything the
//! client must not be able to be lied to about therefore has to live *inside*
//! this document:
//!
//! * the application id, so a validly signed package for app A cannot be
//!   served in place of app B,
//! * the version, so the anti-downgrade check cannot be bypassed by renaming
//!   a file,
//! * the target platform, so an x64 build cannot be delivered to an arm64 host,
//! * a SHA-256 for every payload file, so the archive body is covered too.
//!
//! Filenames and update-index entries are hints. They are never trusted.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::digest::Sha256Digest;
use crate::error::{Error, Result};
use crate::platform::Platform;
use crate::version::Version;

/// Archive entry holding the manifest.
pub const MANIFEST_ENTRY: &str = "manifest.json";
/// Archive entry holding the detached signature over [`MANIFEST_ENTRY`].
pub const SIGNATURE_ENTRY: &str = "manifest.sig";
/// Archive prefix under which every payload file is stored.
pub const PAYLOAD_PREFIX: &str = "payload/";
/// Highest [`FormatVersion`] this build can interpret.
pub const MAX_SUPPORTED_FORMAT_VERSION: u32 = 1;

/// Upper bound on a manifest document, to bound work before parsing.
pub const MAX_MANIFEST_BYTES: usize = 8 * 1024 * 1024;

/// Directory inside an installed version reserved for xPack's own metadata.
///
/// Each installed version keeps the manifest and signature it was installed
/// from, so it can later serve as a base for a differential update. A payload
/// must therefore never be able to write here, or a package could forge the
/// record of what it claims to be.
pub const RESERVED_METADATA_DIR: &str = ".xpack";

/// Longest payload path accepted, in bytes.
///
/// Shared with the extractor so both layers enforce one rule. Two limits for
/// the same thing drift apart, and a manifest that validates but cannot be
/// extracted is a confusing failure to diagnose.
pub const MAX_PAYLOAD_PATH_LEN: usize = 1024;

/// The package format version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FormatVersion(pub u32);

impl FormatVersion {
    /// The version this build writes.
    pub const CURRENT: Self = Self(MAX_SUPPORTED_FORMAT_VERSION);

    /// Fails closed when the package is newer than this build understands.
    ///
    /// A future format may add integrity rules — a new hash over a new
    /// section, say — that this binary would silently skip. Refusing to parse
    /// is the only safe response.
    pub fn ensure_supported(self) -> Result<()> {
        if self.0 == 0 {
            return Err(Error::invalid("manifest", "formatVersion must be at least 1"));
        }
        if self.0 > MAX_SUPPORTED_FORMAT_VERSION {
            return Err(Error::UnsupportedFormatVersion {
                found: self.0,
                supported: MAX_SUPPORTED_FORMAT_VERSION,
            });
        }
        Ok(())
    }
}

impl Default for FormatVersion {
    fn default() -> Self {
        Self::CURRENT
    }
}

/// Identity and human-facing metadata for the packaged application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Application {
    /// Stable reverse-DNS identifier, e.g. `com.example.myapp`.
    ///
    /// This doubles as a directory name and an IPC endpoint name, so its
    /// character set is validated strictly by [`Manifest::validate`].
    pub id: String,
    /// Display name shown in shortcuts and logs.
    pub name: String,
    /// Application version.
    pub version: Version,
    /// Optional one-line description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional publisher name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
}

/// How the launcher should start the application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LaunchSpec {
    /// Executable to run.
    ///
    /// A value containing a `/` is resolved relative to the installed version
    /// directory (a bundled runtime, e.g. `runtime/bin/java`). A bare name
    /// with no separator is resolved through the user's `PATH` (a system
    /// runtime, e.g. `java`). Both bundled and system runtimes are supported.
    pub executable: String,
    /// Arguments passed before any user-supplied arguments.
    #[serde(default)]
    pub arguments: Vec<String>,
    /// Working directory, relative to the version directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// Extra environment variables set for the child process.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
}

impl LaunchSpec {
    /// Returns `true` when the executable is resolved from the package itself.
    pub fn is_bundled(&self) -> bool {
        self.executable.contains('/') || self.executable.contains('\\')
    }
}

/// How a newly activated version proves it works.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HealthSpec {
    /// How long a version on probation has to prove it started.
    ///
    /// A version that is still running when this elapses is treated as
    /// healthy. This is the only startup signal available without the
    /// application cooperating, and it is a real one: the overwhelming
    /// majority of broken updates fail immediately — a missing runtime, an
    /// unreadable file, an incompatible library — not minutes later.
    #[serde(default = "default_startup_timeout")]
    pub startup_timeout_seconds: u64,
}

impl Default for HealthSpec {
    fn default() -> Self {
        Self { startup_timeout_seconds: default_startup_timeout() }
    }
}

fn default_startup_timeout() -> u64 {
    15
}

/// Where the updater looks for newer versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateSpec {
    /// Release channel, e.g. `stable`.
    #[serde(default = "default_channel")]
    pub channel: String,
    /// Base URL of the update index. Must be HTTPS outside of tests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl Default for UpdateSpec {
    fn default() -> Self {
        Self { channel: default_channel(), url: None }
    }
}

fn default_channel() -> String {
    "stable".to_string()
}

/// One file in the payload, with the digest the signature transitively covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PayloadFile {
    /// Path relative to the version root, always using `/` separators.
    pub path: String,
    /// Exact uncompressed size in bytes.
    pub size: u64,
    /// SHA-256 of the file contents.
    pub sha256: Sha256Digest,
    /// Unix permission bits, when the packaging host recorded them.
    ///
    /// Losing the executable bit is the single most common way a correctly
    /// extracted package still fails to launch, so it travels in the signed
    /// manifest rather than relying on archive metadata alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
}

/// Aggregate description of the payload.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PayloadSpec {
    /// Sum of every file's uncompressed size.
    ///
    /// Extraction enforces this as a hard budget, which is what stops a
    /// decompression bomb from filling the user's disk.
    pub total_size: u64,
    /// Every file in the payload, sorted by path.
    pub files: Vec<PayloadFile>,
}

/// A complete, signed package manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    /// Format version of this document.
    pub format_version: FormatVersion,
    /// Application identity.
    pub application: Application,
    /// Target platform.
    pub platform: Platform,
    /// Launch description.
    pub launch: LaunchSpec,
    /// Update configuration.
    #[serde(default)]
    pub update: UpdateSpec,
    /// How a newly activated version proves it started successfully.
    #[serde(default)]
    pub health: HealthSpec,
    /// Payload inventory.
    pub payload: PayloadSpec,
    /// Hex-encoded public key the publisher declares as theirs.
    ///
    /// **This is not a trust input.** A key carried inside a package is
    /// exactly as forgeable as the package around it: anyone can generate a
    /// key, sign their own package with it, and declare it here. Verifying a
    /// package against the key it ships with proves only that it is
    /// internally consistent, which is worth nothing on its own.
    ///
    /// It exists so that trust-on-first-use has something to pin. That mode
    /// accepts precisely this weakness — it trusts the first download and
    /// nothing after it, exactly as SSH host keys do. Every later update must
    /// verify against the pinned key, and an installation that already has
    /// pinned keys ignores this field entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_key: Option<String>,

    /// RFC 3339 timestamp recorded at packaging time, for diagnostics only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

impl Manifest {
    /// Parses and fully validates a manifest from its raw signed bytes.
    ///
    /// Callers must verify the signature over these same bytes *before*
    /// trusting the returned value; see `xpack_package::PackageReader`.
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(Error::invalid(
                "manifest",
                format!("document is {} bytes, limit is {MAX_MANIFEST_BYTES}", bytes.len()),
            ));
        }
        let manifest: Self =
            serde_json::from_slice(bytes).map_err(|e| Error::json(MANIFEST_ENTRY, e))?;
        manifest.format_version.ensure_supported()?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Serialises the manifest to the exact bytes that will be signed.
    pub fn to_signed_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes =
            serde_json::to_vec_pretty(self).map_err(|e| Error::json(MANIFEST_ENTRY, e))?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// Enforces every domain rule that the type system cannot.
    pub fn validate(&self) -> Result<()> {
        self.format_version.ensure_supported()?;
        validate_application_id(&self.application.id)?;

        if self.application.name.trim().is_empty() {
            return Err(Error::invalid("manifest", "application.name must not be empty"));
        }

        if self.launch.executable.trim().is_empty() {
            return Err(Error::invalid("manifest", "launch.executable must not be empty"));
        }
        if self.launch.is_bundled() {
            validate_relative_path("launch.executable", &self.launch.executable)?;
        }
        if let Some(dir) = &self.launch.working_directory {
            validate_relative_path("launch.workingDirectory", dir)?;
        }

        if let Some(url) = &self.update.url {
            validate_update_url(url)?;
        }

        self.validate_payload()
    }

    fn validate_payload(&self) -> Result<()> {
        let mut seen = std::collections::BTreeSet::new();
        let mut total: u64 = 0;

        for file in &self.payload.files {
            validate_relative_path("payload entry", &file.path)?;

            // Case-insensitive duplicate detection. On Windows and on
            // case-insensitive macOS volumes `App.exe` and `app.exe` are the
            // same file, so a manifest listing both would let one entry's
            // verified digest be silently replaced by the other's contents.
            let key = file.path.to_lowercase();
            if !seen.insert(key) {
                return Err(Error::invalid(
                    "manifest",
                    format!("payload lists {:?} more than once (case-insensitively)", file.path),
                ));
            }

            total = total.checked_add(file.size).ok_or_else(|| {
                Error::invalid("manifest", "payload total size overflows a 64-bit integer")
            })?;
        }

        if total != self.payload.total_size {
            return Err(Error::invalid(
                "manifest",
                format!(
                    "payload.totalSize is {} but the file list sums to {total}",
                    self.payload.total_size
                ),
            ));
        }

        // A bundled launch target must actually be present in the payload,
        // otherwise the failure surfaces only at first launch — after the
        // install has already been reported as successful.
        if self.launch.is_bundled() {
            let wanted = normalise_separators(&self.launch.executable);
            let present = self.payload.files.iter().any(|f| f.path == wanted);
            if !present {
                return Err(Error::invalid(
                    "manifest",
                    format!(
                        "launch.executable {:?} is not present in the payload",
                        self.launch.executable
                    ),
                ));
            }
        }

        Ok(())
    }

    /// Canonical artefact filename, e.g. `MyApp-1.2.0-linux-x64.xpkg`.
    pub fn package_file_name(&self) -> String {
        format!(
            "{}-{}-{}.xpkg",
            sanitize_file_stem(&self.application.name),
            self.application.version.to_directory_name(),
            self.platform
        )
    }
}

/// Validates an application id used as a directory and IPC endpoint name.
///
/// The character set is restricted to ASCII alphanumerics, `.`, `-` and `_`
/// and must start with an alphanumeric. That rules out path traversal, Windows
/// reserved characters, leading dashes that look like CLI flags, and the
/// pure-dot names `.` and `..`.
pub fn validate_application_id(id: &str) -> Result<()> {
    const MAX_LEN: usize = 128;

    if id.is_empty() || id.len() > MAX_LEN {
        return Err(Error::invalid(
            "application.id",
            format!("must be 1..={MAX_LEN} characters, got {}", id.len()),
        ));
    }
    if !id.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return Err(Error::invalid("application.id", "must start with a letter or digit"));
    }
    if let Some(bad) =
        id.chars().find(|c| !matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_'))
    {
        return Err(Error::invalid(
            "application.id",
            format!("contains disallowed character {bad:?}; allowed set is [A-Za-z0-9._-]"),
        ));
    }
    if id.contains("..") {
        return Err(Error::invalid("application.id", "must not contain '..'"));
    }
    Ok(())
}

/// Rejects anything that is not a plain relative path inside the package.
///
/// Public because the launcher must apply the same rule before executing
/// anything. A validator that only runs at packaging time is no defence for
/// the component that actually starts a process.
pub fn validate_relative_path(subject: &str, path: &str) -> Result<()> {
    let normalised = normalise_separators(path);
    if normalised.is_empty() {
        return Err(Error::invalid(subject, "must not be empty"));
    }
    if normalised.len() > MAX_PAYLOAD_PATH_LEN {
        return Err(Error::invalid(
            subject,
            format!("is {} bytes, limit is {MAX_PAYLOAD_PATH_LEN}", normalised.len()),
        ));
    }
    if normalised.starts_with('/') {
        return Err(Error::invalid(subject, format!("{path:?} must be relative")));
    }
    if normalised.split('/').any(|c| c == ".." || c == "." || c.is_empty()) {
        return Err(Error::invalid(
            subject,
            format!("{path:?} must not contain '.', '..' or empty path components"),
        ));
    }
    // `C:foo` is drive-relative on Windows and resolves outside the target.
    if normalised.len() >= 2 && normalised.as_bytes()[1] == b':' {
        return Err(Error::invalid(subject, format!("{path:?} must not be drive-qualified")));
    }
    if normalised.contains('\0') {
        return Err(Error::invalid(subject, format!("{path:?} contains a NUL byte")));
    }
    if normalised
        .split('/')
        .next()
        .is_some_and(|first| first.eq_ignore_ascii_case(RESERVED_METADATA_DIR))
    {
        return Err(Error::invalid(
            subject,
            format!("{path:?} uses the reserved {RESERVED_METADATA_DIR:?} directory"),
        ));
    }
    Ok(())
}

fn normalise_separators(path: &str) -> String {
    path.replace('\\', "/")
}

/// Requires HTTPS for update endpoints.
///
/// Plain HTTP is permitted only for `localhost`, which keeps the integration
/// test suite able to run a throwaway server without weakening the rule that
/// real deployments ship over TLS.
fn validate_update_url(url: &str) -> Result<()> {
    let is_localhost = url.starts_with("http://127.0.0.1")
        || url.starts_with("http://localhost")
        || url.starts_with("http://[::1]");
    if url.starts_with("https://") || is_localhost {
        Ok(())
    } else {
        Err(Error::invalid("update.url", format!("{url:?} must use https://")))
    }
}

fn sanitize_file_stem(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let trimmed = cleaned.trim_matches('-').to_string();
    if trimmed.is_empty() { "application".to_string() } else { trimmed }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::Sha256Digest;
    use crate::platform::{Arch, Os};

    fn digest(seed: u8) -> Sha256Digest {
        Sha256Digest::from_bytes([seed; 32])
    }

    fn sample() -> Manifest {
        Manifest {
            format_version: FormatVersion::CURRENT,
            application: Application {
                id: "com.example.myapp".into(),
                name: "My Application".into(),
                version: Version::parse("1.2.0").unwrap(),
                description: None,
                publisher: None,
            },
            platform: Platform::new(Os::Linux, Arch::X64),
            launch: LaunchSpec {
                executable: "runtime/bin/java".into(),
                arguments: vec!["-jar".into(), "application/app.jar".into()],
                working_directory: None,
                environment: BTreeMap::new(),
            },
            update: UpdateSpec::default(),
            health: HealthSpec::default(),
            signing_key: None,
            payload: PayloadSpec {
                total_size: 30,
                files: vec![
                    PayloadFile {
                        path: "application/app.jar".into(),
                        size: 20,
                        sha256: digest(1),
                        mode: Some(0o644),
                    },
                    PayloadFile {
                        path: "runtime/bin/java".into(),
                        size: 10,
                        sha256: digest(2),
                        mode: Some(0o755),
                    },
                ],
            },
            created_at: None,
        }
    }

    #[test]
    fn sample_manifest_round_trips_through_its_signed_bytes() {
        let manifest = sample();
        let bytes = manifest.to_signed_bytes().unwrap();
        assert_eq!(Manifest::from_slice(&bytes).unwrap(), manifest);
    }

    #[test]
    fn rejects_a_future_format_version_instead_of_ignoring_it() {
        let mut m = sample();
        m.format_version = FormatVersion(MAX_SUPPORTED_FORMAT_VERSION + 1);
        let bytes = serde_json::to_vec(&m).unwrap();
        let err = Manifest::from_slice(&bytes).unwrap_err();
        assert!(matches!(err, Error::UnsupportedFormatVersion { .. }), "got {err:?}");
        assert!(err.is_integrity_failure());
    }

    #[test]
    fn rejects_unknown_fields_so_typos_are_not_silently_dropped() {
        let bytes = br#"{"formatVersion":1,"applicatoin":{}}"#;
        assert!(Manifest::from_slice(bytes).is_err());
    }

    #[test]
    fn rejects_a_payload_path_longer_than_the_extractor_accepts() {
        // The manifest and the extractor must agree, or a package validates at
        // build time and then fails on the user's machine.
        let mut m = sample();
        m.payload.files[0].path = "a/".repeat(MAX_PAYLOAD_PATH_LEN);
        assert!(m.validate().unwrap_err().to_string().contains("limit is"));
    }

    #[test]
    fn rejects_payload_paths_inside_the_reserved_metadata_directory() {
        // A package that could write here would be able to forge the record of
        // which manifest a version was installed from.
        for reserved in [".xpack/manifest.json", ".XPack/manifest.sig", ".xpack/anything"] {
            let mut m = sample();
            m.payload.files[0].path = reserved.into();
            assert!(m.validate().is_err(), "{reserved:?} must be rejected");
        }
        // A name that merely starts with the same letters is fine.
        let mut m = sample();
        m.payload.files[0].path = ".xpackage/data".into();
        m.launch.executable = "runtime/bin/java".into();
        m.validate().expect("only the exact reserved name is blocked");
    }

    #[test]
    fn rejects_traversal_in_payload_paths() {
        for bad in ["../escape", "/etc/passwd", "a/../../b", "C:evil", "./x", "a//b"] {
            let mut m = sample();
            m.payload.files[0].path = bad.into();
            assert!(m.validate().is_err(), "{bad:?} should have been rejected");
        }
    }

    #[test]
    fn rejects_backslash_traversal_used_to_dodge_slash_checks() {
        let mut m = sample();
        m.payload.files[0].path = r"..\windows\system32".into();
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_case_insensitive_duplicate_payload_entries() {
        let mut m = sample();
        m.payload.files[0].path = "App/Data.bin".into();
        m.payload.files[1].path = "app/data.bin".into();
        m.launch.executable = "app/data.bin".into();
        let err = m.validate().unwrap_err();
        assert!(err.to_string().contains("more than once"), "got {err}");
    }

    #[test]
    fn rejects_total_size_that_disagrees_with_the_file_list() {
        let mut m = sample();
        m.payload.total_size = 1;
        assert!(m.validate().unwrap_err().to_string().contains("totalSize"));
    }

    #[test]
    fn rejects_a_bundled_launch_target_missing_from_the_payload() {
        let mut m = sample();
        m.launch.executable = "runtime/bin/python".into();
        assert!(m.validate().unwrap_err().to_string().contains("not present in the payload"));
    }

    #[test]
    fn allows_a_system_runtime_resolved_through_path() {
        let mut m = sample();
        m.launch.executable = "java".into();
        assert!(!m.launch.is_bundled());
        m.validate().expect("system runtimes are explicitly supported");
    }

    #[test]
    fn rejects_dangerous_application_ids() {
        for bad in ["", "..", "../etc", ".hidden", "-flag", "a/b", "a\\b", "app id", "a:b"] {
            assert!(validate_application_id(bad).is_err(), "{bad:?} should have been rejected");
        }
        for good in ["com.example.myapp", "App-1", "a", "x_y.z-1"] {
            validate_application_id(good).unwrap_or_else(|e| panic!("{good:?}: {e}"));
        }
    }

    #[test]
    fn requires_https_for_update_endpoints() {
        let mut m = sample();
        m.update.url = Some("http://updates.example.com".into());
        assert!(m.validate().is_err());
        m.update.url = Some("https://updates.example.com".into());
        m.validate().unwrap();
        m.update.url = Some("http://127.0.0.1:8080".into());
        m.validate().expect("loopback is allowed for local testing");
    }

    #[test]
    fn builds_a_canonical_artefact_name() {
        assert_eq!(sample().package_file_name(), "My-Application-1.2.0-linux-x64.xpkg");
    }
}
