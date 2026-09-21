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
use std::time::Duration;

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

    /// Whether the application must say for itself that it started.
    ///
    /// Left off, surviving the startup window counts as healthy. That is an
    /// approximation and a known one: an application that starts, paints a
    /// window and is broken in every other respect passes it.
    ///
    /// Turned on, surviving is no longer enough — the application has to
    /// report, and a version that does not is rolled back. It reports by
    /// creating the file named in `XPACK_HEALTH_FILE`, which the launcher sets
    /// in its environment. A file rather than a socket because xPack is
    /// runtime-independent: creating one is a line of code in every language a
    /// payload might be written in, and needs no library from xPack at all.
    ///
    /// Off by default, because a publisher who has not added that line would
    /// otherwise find every update rolled back.
    #[serde(default)]
    pub require_startup_report: bool,
}

impl Default for HealthSpec {
    fn default() -> Self {
        Self { startup_timeout_seconds: default_startup_timeout(), require_startup_report: false }
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

    /// Whether this release must be applied before the application runs again.
    ///
    /// For a security release the publisher needs more than "it will be picked
    /// up eventually". Setting this records, once the version is installed and
    /// its signature checked, that nothing older may start.
    ///
    /// # It is read from the signed manifest, never from the index
    ///
    /// The update index is served by whoever controls the server and is
    /// trusted for nothing. A server that could declare releases mandatory
    /// could stop an application starting at will. So the flag takes effect
    /// only once the package carrying it has been downloaded and verified —
    /// the publisher's signature is what makes it true.
    ///
    /// # What it does not mean
    ///
    /// It does not make the user wait at startup for a download. A mandatory
    /// version is staged in the background like any other and enforced from
    /// the next launch, when applying it costs one state write. Blocking a
    /// user behind a network transfer, with their application unopened and
    /// nothing drawing a window yet, would be a worse failure than one more
    /// session on the old version.
    #[serde(default)]
    pub mandatory: bool,

    /// Whether to keep looking for updates while the application is open.
    ///
    /// Off by default: the application is checked when it starts, and whatever
    /// that check finds is applied at the next start. That is what every
    /// installation did before this field existed, and what most desktop
    /// applications want.
    ///
    /// On, the launcher keeps looking for as long as the application runs. It
    /// is still not a promise that anything is checked on a schedule — an
    /// application nobody opens is never checked, because nothing is running
    /// to do the checking.
    ///
    /// # Why this is a switch of its own rather than an interval that may be
    /// absent
    ///
    /// The two questions are different: *whether* to look while running, and
    /// *how often* the server may be asked. Folding them into one field made
    /// the interval load-bearing in a way nobody would guess — a release that
    /// left the number out turned the whole feature off, silently, and the
    /// next release to leave it out would do it again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub check_while_running: bool,

    /// Shortest time between asking the update server anything, in minutes.
    ///
    /// Governs every check, not only the ones made while the application runs:
    /// an application opened twenty times a day makes at most one request per
    /// interval, whether those checks come from the launcher starting or from
    /// [`Self::check_while_running`].
    ///
    /// Absent means [`DEFAULT_CHECK_INTERVAL_MINUTES`]. Below
    /// [`MIN_CHECK_INTERVAL_MINUTES`] is raised to it rather than refused: the
    /// value costs a user bandwidth and a publisher server load, and a release
    /// cannot appear fast enough for a shorter one to find anything. Refusing
    /// instead would mean a whole package rejected over a number with an
    /// obvious, harmless reading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_interval_minutes: Option<u32>,

    /// How urgent this release is, for the prompt only.
    ///
    /// See [`UpdateSpec::effective_severity`], which is what a prompt should
    /// ask: a mandatory release is never shown as optional, whatever this says.
    #[serde(default, skip_serializing_if = "is_default_severity")]
    pub severity: UpdateSeverity,

    /// Whether a background check that stages this version may tell the user.
    ///
    /// Off by default, because a prompt is an interruption and a publisher who
    /// has not asked for one has not agreed to interrupt their users. With it
    /// off, an update is staged in silence and applied at the next start,
    /// which is what every installation did before prompting existed.
    #[serde(default)]
    pub notify: bool,

    /// What the prompt says. Absent leaves the wording to whatever shows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<PromptSpec>,
}

/// Whether the severity is the default, for `skip_serializing_if`.
///
/// Takes a reference because serde requires that signature, not because the
/// value is large enough to warrant one.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_default_severity(severity: &UpdateSeverity) -> bool {
    *severity == UpdateSeverity::default()
}

impl Default for UpdateSpec {
    fn default() -> Self {
        Self {
            channel: default_channel(),
            url: None,
            mandatory: false,
            check_while_running: false,
            check_interval_minutes: None,
            severity: UpdateSeverity::default(),
            notify: false,
            prompt: None,
        }
    }
}

impl UpdateSpec {
    /// The shortest time that may pass between asking the update server.
    ///
    /// Always an answer: what the publisher asked for, raised to
    /// [`MIN_CHECK_INTERVAL_MINUTES`] if it was shorter than that, or
    /// [`DEFAULT_CHECK_INTERVAL_MINUTES`] when they said nothing.
    ///
    /// Whether anything checks while the application runs is a separate
    /// question, answered by [`Self::check_while_running`]. This says only how
    /// often, and it governs the check made at startup too.
    pub fn check_interval(&self) -> Duration {
        let minutes = self.check_interval_minutes.unwrap_or(DEFAULT_CHECK_INTERVAL_MINUTES);
        Duration::from_secs(u64::from(minutes.max(MIN_CHECK_INTERVAL_MINUTES)) * 60)
    }

    /// The severity a prompt should actually use.
    ///
    /// A mandatory release is critical whether or not it says so. Nothing
    /// older may start once it is installed, so offering a user "later" would
    /// be offering them a choice that has already been taken away — and a
    /// publisher who sets one flag and forgets the other should not produce a
    /// dialog that contradicts what the launcher is about to do.
    pub fn effective_severity(&self) -> UpdateSeverity {
        if self.mandatory { UpdateSeverity::Critical } else { self.severity }
    }
}

/// How urgent a release is, and therefore what a user may do about it.
///
/// Display only: it decides what an update prompt offers, never what is
/// installed or verified. Enforcement is [`UpdateSpec::mandatory`], which is a
/// separate flag because refusing to start an old version and interrupting a
/// user are different decisions with different costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateSeverity {
    /// Take it whenever. The default, and what an unset field means.
    ///
    /// Lowest on purpose: a publisher who said nothing has not asked to
    /// interrupt anyone, and guessing otherwise on their behalf is how an
    /// updater becomes the thing users disable.
    #[default]
    Optional,
    /// Worth taking soon.
    Recommended,
    /// Should be taken now; a prompt offers no way to decline.
    Critical,
}

/// What an update prompt says, written by the publisher.
///
/// Both fields are shown to a user, so both are bounded and screened at
/// validation. A manifest is signed, which makes this the publisher's own
/// text — but a signature proves authorship, not that the string is a sane
/// thing to put in a dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromptSpec {
    /// One line, shown as the dialog's title.
    pub title: String,
    /// The body. Line breaks are allowed; nothing else that is not printable.
    pub message: String,
}

/// Longest prompt title accepted, in characters.
///
/// A title is one line in a dialog the operating system sizes itself. Longer
/// than this is either a mistake or a message in the wrong field, and both
/// look like a bug to the user staring at a truncated sentence.
pub const MAX_PROMPT_TITLE_CHARS: usize = 120;

/// Longest prompt message accepted, in characters.
pub const MAX_PROMPT_MESSAGE_CHARS: usize = 2000;

/// Shortest check interval an installation will honour, in minutes.
///
/// Fifteen minutes is far below any interval a real publisher wants and far
/// above the rate at which asking a server is free.
pub const MIN_CHECK_INTERVAL_MINUTES: u32 = 15;

/// How often an installation asks its update server when nobody said.
///
/// Four hours is frequent enough that a security fix reaches an active user
/// the same day, and rare enough that a busy user's machine is not a burden on
/// the publisher's server.
pub const DEFAULT_CHECK_INTERVAL_MINUTES: u32 = 4 * 60;

fn default_channel() -> String {
    "stable".to_string()
}

/// How the application should appear in the user's desktop environment.
///
/// Every field is optional and the whole section defaults to "do nothing".
/// A menu entry is a change to the user's machine outside the installation
/// directory, so it happens because a publisher asked for it, never because a
/// package was silently assumed to want one.
///
/// # One description, three very different mechanisms
///
/// A Start-Menu shortcut, a freedesktop `.desktop` entry and a macOS
/// application bundle have almost nothing in common as files. What they have
/// in common is the *intent*, and that is what this records: the publisher
/// says "this application should be openable from the desktop, here is its
/// icon and roughly what kind of program it is", and each platform's installer
/// renders that in its own terms.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DesktopSpec {
    /// Whether to create a menu entry, shortcut or application bundle.
    #[serde(default)]
    pub shortcut: bool,

    /// Icon for the entry, as a payload-relative path.
    ///
    /// Left out, each platform falls back to whatever it shows for a program
    /// with no icon. Supplying one is strongly preferred: an unnamed generic
    /// icon in a user's application menu is how an installation looks broken.
    ///
    /// The format that works differs per platform — `.ico` on Windows,
    /// `.icns` on macOS, PNG or SVG on Linux — which is why this is a path
    /// into the payload rather than something xPack converts. A package that
    /// targets one platform ships the icon that platform reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,

    /// freedesktop categories for the Linux menu entry, e.g. `Development`.
    ///
    /// Ignored on Windows and macOS, neither of which has an equivalent
    /// concept that a publisher declares. Left empty, the entry still appears;
    /// it simply lands in whatever catch-all the desktop environment uses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,

    /// Whether the application needs a terminal to be useful.
    ///
    /// Set, the Linux entry is marked `Terminal=true` and Windows points the
    /// shortcut at the console launcher rather than the windowed one, so the
    /// user gets the window the program expects to write to. Left unset, the
    /// shortcut opens the application with no console attached.
    #[serde(default)]
    pub terminal: bool,
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
    /// How the application appears in the user's desktop environment.
    #[serde(default)]
    pub desktop: DesktopSpec,
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
        if let Some(prompt) = &self.update.prompt {
            validate_prompt(prompt)?;
        }

        if let Some(icon) = &self.desktop.icon {
            validate_relative_path("desktop.icon", icon)?;
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

        // Same rule as the launch executable below, and for the same reason:
        // an icon named but not shipped produces an entry in the user's menu
        // with a missing image, long after the install reported success.
        if let Some(icon) = &self.desktop.icon {
            let wanted = normalise_separators(icon);
            if !self.payload.files.iter().any(|f| f.path == wanted) {
                return Err(Error::invalid(
                    "manifest",
                    format!("desktop.icon {icon:?} is not present in the payload"),
                ));
            }
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
/// Rejects prompt text that cannot be shown to a user as written.
///
/// Every rule here is about the dialog, not about trust. The manifest is
/// signed, so this text is the publisher's — but a control character in the
/// middle of a sentence renders as a box or vanishes, and an empty title
/// produces a dialog labelled with nothing. Both are caught at packaging time,
/// where the publisher can still fix them, rather than on a user's screen.
///
/// Line breaks are allowed in the message because a dialog can show
/// paragraphs, and refused in the title because it cannot.
fn validate_prompt(prompt: &PromptSpec) -> Result<()> {
    if prompt.title.trim().is_empty() {
        return Err(Error::invalid("manifest", "update.prompt.title must not be empty"));
    }
    if prompt.message.trim().is_empty() {
        return Err(Error::invalid("manifest", "update.prompt.message must not be empty"));
    }

    let title_length = prompt.title.chars().count();
    if title_length > MAX_PROMPT_TITLE_CHARS {
        return Err(Error::invalid(
            "manifest",
            format!(
                "update.prompt.title is {title_length} characters, limit is \
                 {MAX_PROMPT_TITLE_CHARS}"
            ),
        ));
    }
    let message_length = prompt.message.chars().count();
    if message_length > MAX_PROMPT_MESSAGE_CHARS {
        return Err(Error::invalid(
            "manifest",
            format!(
                "update.prompt.message is {message_length} characters, limit is \
                 {MAX_PROMPT_MESSAGE_CHARS}"
            ),
        ));
    }

    if prompt.title.chars().any(char::is_control) {
        return Err(Error::invalid(
            "manifest",
            "update.prompt.title contains a control character; a dialog title is one line",
        ));
    }
    if prompt.message.chars().any(|c| c.is_control() && c != '\n') {
        return Err(Error::invalid(
            "manifest",
            "update.prompt.message contains a control character other than a line break",
        ));
    }
    Ok(())
}

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
            desktop: DesktopSpec::default(),
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
    fn a_publisher_who_says_nothing_gets_the_least_urgent_severity() {
        // Silence is not a request to interrupt anyone.
        assert_eq!(UpdateSpec::default().severity, UpdateSeverity::Optional);
        assert_eq!(UpdateSpec::default().effective_severity(), UpdateSeverity::Optional);
        assert!(!UpdateSpec::default().notify);
        assert_eq!(UpdateSpec::default().prompt, None);
    }

    #[test]
    fn a_mandatory_release_is_critical_even_when_it_forgot_to_say_so() {
        // Offering "later" for a version the launcher will require anyway is
        // offering a choice that no longer exists.
        let spec = UpdateSpec {
            mandatory: true,
            severity: UpdateSeverity::Optional,
            ..UpdateSpec::default()
        };
        assert_eq!(spec.effective_severity(), UpdateSeverity::Critical);
    }

    #[test]
    fn severity_and_prompt_survive_the_signed_bytes() {
        let mut manifest = sample();
        manifest.update.severity = UpdateSeverity::Critical;
        manifest.update.notify = true;
        manifest.update.prompt = Some(PromptSpec {
            title: "Security update".into(),
            message: "This release fixes a problem.\n\nRestart when you can.".into(),
        });
        let bytes = manifest.to_signed_bytes().unwrap();
        let back = Manifest::from_slice(&bytes).unwrap();
        assert_eq!(back, manifest);
        assert_eq!(back.update.effective_severity(), UpdateSeverity::Critical);
    }

    #[test]
    fn severity_is_spelled_in_lowercase_in_the_document() {
        let mut manifest = sample();
        manifest.update.severity = UpdateSeverity::Recommended;
        let json = String::from_utf8(manifest.to_signed_bytes().unwrap()).unwrap();
        assert!(json.contains("\"severity\": \"recommended\""), "{json}");
    }

    #[test]
    fn a_default_severity_is_left_out_of_the_document() {
        // Keeps every manifest published before prompting existed byte-identical.
        let json = String::from_utf8(sample().to_signed_bytes().unwrap()).unwrap();
        assert!(!json.contains("severity"), "{json}");
        assert!(!json.contains("prompt"), "{json}");
    }

    #[test]
    fn an_empty_prompt_title_is_refused_at_packaging_time() {
        let mut manifest = sample();
        manifest.update.prompt =
            Some(PromptSpec { title: "   ".into(), message: "Something".into() });
        let err = manifest.validate().unwrap_err();
        assert!(format!("{err}").contains("title"), "got {err}");
    }

    #[test]
    fn an_over_long_prompt_title_is_refused() {
        let mut manifest = sample();
        manifest.update.prompt = Some(PromptSpec {
            title: "x".repeat(MAX_PROMPT_TITLE_CHARS + 1),
            message: "Something".into(),
        });
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn an_over_long_prompt_message_is_refused() {
        let mut manifest = sample();
        manifest.update.prompt = Some(PromptSpec {
            title: "Update".into(),
            message: "x".repeat(MAX_PROMPT_MESSAGE_CHARS + 1),
        });
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn a_control_character_never_reaches_a_dialog() {
        // It renders as a box, or as nothing, and either way the user reads a
        // sentence with a hole in it.
        let mut manifest = sample();
        manifest.update.prompt =
            Some(PromptSpec { title: "Update\u{7}now".into(), message: "Something".into() });
        assert!(manifest.validate().is_err());

        manifest.update.prompt =
            Some(PromptSpec { title: "Update".into(), message: "Before\u{0}after".into() });
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn a_line_break_is_allowed_in_the_message_but_not_the_title() {
        let mut manifest = sample();
        manifest.update.prompt = Some(PromptSpec {
            title: "Update".into(),
            message: "First paragraph.\n\nSecond paragraph.".into(),
        });
        manifest.validate().unwrap();

        manifest.update.prompt =
            Some(PromptSpec { title: "Two\nlines".into(), message: "Something".into() });
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn severity_orders_from_least_to_most_urgent() {
        // The prompt picks its buttons by comparing these.
        assert!(UpdateSeverity::Optional < UpdateSeverity::Recommended);
        assert!(UpdateSeverity::Recommended < UpdateSeverity::Critical);
    }

    #[test]
    fn checking_while_running_is_off_until_a_publisher_asks_for_it() {
        // The default behaviour, and what every installation did before the
        // field existed: one check when the application starts.
        assert!(!UpdateSpec::default().check_while_running);
    }

    #[test]
    fn an_absent_interval_means_the_default_rather_than_no_checking() {
        // The interval says how often, never whether. A release that leaves
        // the number out still checks -- at the default rate -- because the
        // switch above is what turns checking on and off.
        assert_eq!(
            UpdateSpec::default().check_interval(),
            Duration::from_secs(u64::from(DEFAULT_CHECK_INTERVAL_MINUTES) * 60)
        );
    }

    #[test]
    fn forgetting_the_interval_cannot_turn_the_feature_off() {
        // The failure this split exists to prevent: an interval that was also
        // the switch meant a release which left the number out stopped its
        // installations checking at all, and said nothing about it.
        let asked = UpdateSpec { check_while_running: true, ..UpdateSpec::default() };
        assert!(asked.check_while_running, "the switch is not the interval's to flip");
        assert_eq!(asked.check_interval(), UpdateSpec::default().check_interval());
    }

    #[test]
    fn a_declared_interval_is_what_is_asked_for() {
        let spec = UpdateSpec { check_interval_minutes: Some(180), ..UpdateSpec::default() };
        assert_eq!(spec.check_interval(), Duration::from_secs(3 * 60 * 60));
    }

    #[test]
    fn an_interval_below_the_floor_is_raised_to_it_rather_than_refused() {
        let mut manifest = sample();
        manifest.update.check_interval_minutes = Some(1);
        // The package is still valid: a number with an obvious harmless
        // reading must not cost a user their update.
        manifest.validate().unwrap();
        assert_eq!(
            manifest.update.check_interval(),
            Duration::from_secs(u64::from(MIN_CHECK_INTERVAL_MINUTES) * 60)
        );
    }

    #[test]
    fn zero_is_the_floor_too_rather_than_a_hidden_off_switch() {
        // It used to mean "never check", which made one field answer two
        // questions. The switch answers one of them now, so this is just a
        // number that is too small.
        let spec = UpdateSpec { check_interval_minutes: Some(0), ..UpdateSpec::default() };
        assert_eq!(
            spec.check_interval(),
            Duration::from_secs(u64::from(MIN_CHECK_INTERVAL_MINUTES) * 60)
        );
    }

    #[test]
    fn a_very_large_interval_does_not_overflow_the_duration() {
        let spec = UpdateSpec { check_interval_minutes: Some(u32::MAX), ..UpdateSpec::default() };
        assert_eq!(spec.check_interval(), Duration::from_secs(u64::from(u32::MAX) * 60));
    }

    #[test]
    fn a_manifest_without_either_field_still_parses() {
        // Every package published before they existed looks like this, and
        // each one must keep working against a client that knows about them.
        let mut manifest = serde_json::to_value(sample()).unwrap();
        assert!(manifest["update"].get("checkWhileRunning").is_none());
        assert!(manifest["update"].get("checkIntervalMinutes").is_none());
        manifest["update"]["url"] = serde_json::json!("https://updates.example.com");
        let bytes = serde_json::to_vec(&manifest).unwrap();

        let back = Manifest::from_slice(&bytes).unwrap();
        assert!(!back.update.check_while_running);
        assert_eq!(back.update.check_interval(), UpdateSpec::default().check_interval());
    }

    #[test]
    fn both_fields_survive_the_signed_bytes() {
        let mut manifest = sample();
        manifest.update.check_while_running = true;
        manifest.update.check_interval_minutes = Some(180);
        let bytes = manifest.to_signed_bytes().unwrap();
        let back = Manifest::from_slice(&bytes).unwrap();
        assert_eq!(back, manifest);
        assert!(back.update.check_while_running);
        assert_eq!(back.update.check_interval(), Duration::from_secs(3 * 60 * 60));
    }

    #[test]
    fn the_fields_are_named_in_camel_case_like_every_other_one() {
        let mut manifest = sample();
        manifest.update.check_while_running = true;
        manifest.update.check_interval_minutes = Some(60);
        let json = String::from_utf8(manifest.to_signed_bytes().unwrap()).unwrap();
        assert!(json.contains("\"checkWhileRunning\": true"), "{json}");
        assert!(json.contains("\"checkIntervalMinutes\": 60"), "{json}");
    }

    #[test]
    fn a_release_that_asks_for_nothing_writes_neither_field() {
        // Keeps a manifest from a publisher who never heard of either one
        // byte-identical to what it was.
        let json = String::from_utf8(sample().to_signed_bytes().unwrap()).unwrap();
        assert!(!json.contains("checkWhileRunning"), "{json}");
        assert!(!json.contains("checkIntervalMinutes"), "{json}");
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
