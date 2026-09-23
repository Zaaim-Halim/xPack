//! Building a signed `.xpkg` from a directory tree.

use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use xpack_core::digest::Sha256Digest;
use xpack_core::manifest::{MANIFEST_ENTRY, PAYLOAD_PREFIX, SIGNATURE_ENTRY};
use xpack_core::{Error, Manifest, PayloadFile, PayloadSpec, Result};
use xpack_security::{KeyPair, sha256_reader, sign};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::entry_path::safe_payload_path;

/// A package that has just been written.
#[derive(Debug, Clone)]
pub struct PackedPackage {
    /// Where the `.xpkg` was written.
    pub path: PathBuf,
    /// The manifest that was signed.
    pub manifest: Manifest,
    /// SHA-256 of the finished `.xpkg`, for publishing in the update index.
    pub sha256: Sha256Digest,
    /// Size of the finished `.xpkg` in bytes.
    pub size: u64,
}

/// Builds a signed package from a payload directory.
pub struct PackageBuilder<'a> {
    payload_root: &'a Path,
    manifest: Manifest,
    /// Whether this host can read Unix permission bits.
    ///
    /// Defaults to [`HOST_RECORDS_UNIX_MODES`] and is only ever changed by
    /// this module's own tests. Without it the refusal below is unreachable on
    /// a Unix machine, which is every machine this is developed on — so the
    /// one branch that exists to prevent a broken release would ship having
    /// never run.
    host_records_modes: bool,
}

impl<'a> PackageBuilder<'a> {
    /// Prepares a build from `payload_root` using `manifest` as the template.
    ///
    /// The template's payload inventory is ignored and recomputed from what is
    /// actually on disk: a manifest that disagrees with its own archive would
    /// produce a package that fails verification on every client.
    pub fn new(payload_root: &'a Path, manifest: Manifest) -> Self {
        Self { payload_root, manifest, host_records_modes: HOST_RECORDS_UNIX_MODES }
    }

    /// Hashes the payload tree and writes a signed `.xpkg` to `output`.
    pub fn build(mut self, output: &Path, key: &KeyPair) -> Result<PackedPackage> {
        if !self.payload_root.is_dir() {
            return Err(Error::invalid(
                "payload root",
                format!("{} is not a directory", self.payload_root.display()),
            ));
        }

        // Before any work: a build that cannot record permission bits for a
        // Unix target produces a package whose entry point will not be
        // executable. Refusing here is the only moment it can be reported to
        // the person who can do something about it.
        ensure_modes_are_recordable(
            self.manifest.platform,
            self.host_records_modes,
            self.manifest.launch.is_bundled(),
        )?;

        let files = self.collect_payload()?;
        let total_size = files.iter().try_fold(0u64, |acc, f| {
            acc.checked_add(f.size)
                .ok_or_else(|| Error::invalid("payload", "total size overflows a 64-bit integer"))
        })?;
        self.manifest.payload = PayloadSpec { total_size, files };

        // Record the signing key so trust-on-first-use has something to pin.
        // Written before validation and signing so it is covered by both.
        self.manifest.signing_key = Some(key.public().to_hex());

        // Validate before signing. Signing an invalid manifest would produce a
        // package that is cryptographically sound and semantically broken —
        // the worst possible combination, because every client would accept
        // the signature and then fail at install time.
        self.manifest.validate()?;

        let manifest_bytes = self.manifest.to_signed_bytes()?;
        let signature = sign(key, &manifest_bytes);

        self.write_archive(output, &manifest_bytes, &signature.to_hex())?;

        let mut reader = BufReader::new(File::open(output).map_err(|e| Error::io(output, e))?);
        let (sha256, size) = sha256_reader(&mut reader)?;

        tracing::info!(
            package = %output.display(),
            version = %self.manifest.application.version,
            files = self.manifest.payload.files.len(),
            bytes = size,
            "package built"
        );

        Ok(PackedPackage { path: output.to_path_buf(), manifest: self.manifest, sha256, size })
    }

    /// Walks the payload tree, hashing every file.
    fn collect_payload(&self) -> Result<Vec<PayloadFile>> {
        let mut files = Vec::new();
        // Resolved once, and compared against resolved link targets, because
        // the root itself is often reached through a link: on macOS the
        // per-user temporary directory is one, so a textual comparison would
        // call every link in a payload built there an escape.
        let payload_root =
            self.payload_root.canonicalize().map_err(|e| Error::io(self.payload_root, e))?;

        for entry in
            walkdir::WalkDir::new(self.payload_root).follow_links(false).sort_by_file_name()
        {
            let entry = entry.map_err(|e| {
                Error::invalid("payload root", format!("cannot walk payload tree: {e}"))
            })?;
            let file_type = entry.file_type();

            if file_type.is_dir() {
                continue;
            }
            // A symlink is stored as the regular file it points at, never as a
            // link: an entry the extractor saw as a link would be an arbitrary
            // write primitive on the target machine. Reading through it here
            // keeps that property while letting a payload contain one.
            //
            // This is what makes a bundled runtime packageable at all. `jlink`
            // emits an image in which every module's licence files are links to
            // the copies in `java.base`, so refusing links outright refuses
            // every JDK built the standard way.
            if file_type.is_symlink() {
                ensure_link_stays_inside(&payload_root, entry.path())?;
            } else if !file_type.is_file() {
                return Err(Error::invalid(
                    "payload",
                    format!("{} is not a regular file", entry.path().display()),
                ));
            }

            // Nothing that is an xPack private key may ever be packaged. See
            // `contains_private_key_material`.
            if contains_private_key_material(entry.path())? {
                return Err(Error::invalid(
                    "payload",
                    format!(
                        "{} is an xPack private signing key; packaging it would publish the \
                         key that authorises every future update of this application",
                        entry.path().display()
                    ),
                ));
            }

            files.push(self.describe(entry.path())?);
        }

        if files.is_empty() {
            return Err(Error::invalid("payload", "contains no files"));
        }
        Ok(files)
    }

    /// Hashes one file and records the metadata the manifest must carry.
    fn describe(&self, path: &Path) -> Result<PayloadFile> {
        let relative = path.strip_prefix(self.payload_root).map_err(|_| {
            Error::invalid("payload", format!("{} is outside the payload root", path.display()))
        })?;

        let entry: String = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");

        // Run the *same* validator the extractor uses. Rejecting a hostile or
        // unportable name at packaging time turns a user-facing install
        // failure into a build-time error the publisher can actually fix.
        safe_payload_path(&format!("{PAYLOAD_PREFIX}{entry}"))?.ok_or_else(|| {
            Error::invalid("payload", format!("{entry:?} is not a usable payload entry"))
        })?;

        let mut reader = BufReader::new(File::open(path).map_err(|e| Error::io(path, e))?);
        let (sha256, size) = sha256_reader(&mut reader)?;

        Ok(PayloadFile { path: entry, size, sha256, mode: unix_mode(path)? })
    }

    /// Writes the ZIP container.
    ///
    /// The manifest and signature are stored uncompressed and first, so a
    /// verifier can reach them with a single short read instead of
    /// decompressing attacker-controlled data before authenticating anything.
    fn write_archive(&self, output: &Path, manifest: &[u8], signature: &str) -> Result<()> {
        xpack_core::atomic::create_dir_all(xpack_core::atomic::parent_dir(output)?)?;

        let file = File::create(output).map_err(|e| Error::io(output, e))?;
        let mut zip = ZipWriter::new(BufWriter::new(file));

        // Timestamps are pinned rather than left to the zip crate's default.
        // A package must be reproducible: two builds of the same payload have
        // to produce identical bytes, so a release can be shown to come from
        // the source it claims. Taking the default would make that property
        // depend on a dependency's choice, which could change in a patch
        // release and break it silently.
        let stored = SimpleFileOptions::default()
            .last_modified_time(REPRODUCIBLE_TIMESTAMP)
            .compression_method(CompressionMethod::Stored);
        let deflated = SimpleFileOptions::default()
            .last_modified_time(REPRODUCIBLE_TIMESTAMP)
            .compression_method(CompressionMethod::Deflated)
            .large_file(true);

        zip.start_file(MANIFEST_ENTRY, stored).map_err(|e| zip_error(&e))?;
        zip.write_all(manifest).map_err(|e| Error::io(output, e))?;

        zip.start_file(SIGNATURE_ENTRY, stored).map_err(|e| zip_error(&e))?;
        zip.write_all(signature.as_bytes()).map_err(|e| Error::io(output, e))?;
        zip.write_all(b"\n").map_err(|e| Error::io(output, e))?;

        for file in &self.manifest.payload.files {
            let source =
                self.payload_root.join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR));
            let mut options = deflated;
            if let Some(mode) = file.mode {
                options = options.unix_permissions(mode);
            }

            zip.start_file(format!("{PAYLOAD_PREFIX}{}", file.path), options)
                .map_err(|e| zip_error(&e))?;
            let mut reader =
                BufReader::new(File::open(&source).map_err(|e| Error::io(&source, e))?);
            std::io::copy(&mut reader, &mut zip).map_err(|e| Error::io(&source, e))?;
        }

        let mut writer = zip.finish().map_err(|e| zip_error(&e))?;
        writer.flush().map_err(|e| Error::io(output, e))?;
        // The package is an artefact other machines will trust, so it is
        // flushed to stable storage before the build reports success.
        let file = writer.into_inner().map_err(|e| Error::io(output, e.into_error()))?;
        file.sync_all().map_err(|e| Error::io(output, e))?;
        xpack_core::atomic::sync_dir(xpack_core::atomic::parent_dir(output)?)
    }
}

/// The timestamp every entry in a package carries.
///
/// A fixed value, not the file's own modification time, so that packaging the
/// same payload twice produces byte-identical output. Modification times are
/// not preserved by a `git clone`, a CI checkout or a file copy, so recording
/// them would make a package differ between builds for reasons that have
/// nothing to do with its contents.
///
/// 1980-01-01 is the zero of the MS-DOS timestamp the zip format stores, and
/// therefore the only value guaranteed to round-trip through every reader.
const REPRODUCIBLE_TIMESTAMP: zip::DateTime = zip::DateTime::DEFAULT;

/// Largest file examined for key material.
///
/// A key file is a few hundred bytes. Bounding the scan keeps the cost
/// proportionate on a payload containing a multi-gigabyte runtime, where
/// parsing every file as JSON would be absurd.
const MAX_KEY_SCAN_BYTES: u64 = 8 * 1024;

/// Returns `true` when a payload file is an xPack private signing key.
///
/// # The accident this exists to stop
///
/// `xpack keygen` writes `xpack-signing.json` into the working directory, and
/// `xpack pack .` packages the working directory. Those two defaults compose
/// into shipping the private signing key to every user, inside a package whose
/// signature verifies perfectly — and anyone holding it can forge updates for
/// the application from then on. It is silent, it is plausible, and it is
/// unrecoverable without rotating the key and every pinned copy of it.
///
/// # Why by content rather than by name
///
/// A name check is both too narrow and too wide: it misses a key that was
/// renamed or copied, and it would refuse an application's own `config.key`
/// that has nothing to do with signing.
///
/// # Why the whole shape, not just `privateKey`
///
/// Matching a lone `privateKey` field would refuse an application's own
/// configuration that happens to use that name for something unrelated — and a
/// rule that blocks legitimate releases is a rule people route around. All
/// three fields together identify an xPack key file and effectively nothing
/// else, which is what makes it safe to have no override.
///
/// The bound is honest about what this is: it catches the *accident* — the
/// real key file, sitting in the tree — not a determined attempt to smuggle
/// key material past it, which no payload scan can promise. The second check,
/// in the CLI, covers the signing key by path regardless of its contents.
///
/// # No escape hatch
///
/// There is no flag to override this. A package containing the key that signs
/// it has no legitimate use, and an override would exist only to be found in a
/// tutorial and copied.
fn contains_private_key_material(path: &Path) -> Result<bool> {
    let metadata = std::fs::metadata(path).map_err(|e| Error::io(path, e))?;
    if metadata.len() > MAX_KEY_SCAN_BYTES {
        return Ok(false);
    }

    let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;

    // Cheap rejection first: the overwhelming majority of small payload files
    // are not JSON and never reach the parser.
    if !bytes.windows(PRIVATE_KEY_FIELD.len()).any(|w| w == PRIVATE_KEY_FIELD) {
        return Ok(false);
    }

    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Ok(false);
    };

    let has_private_key =
        value.get("privateKey").and_then(serde_json::Value::as_str).is_some_and(|k| !k.is_empty());
    let has_algorithm = value.get("algorithm").is_some_and(serde_json::Value::is_string);
    let has_format_version = value.get("formatVersion").is_some_and(serde_json::Value::is_number);

    Ok(has_private_key && has_algorithm && has_format_version)
}

/// The field that distinguishes a secret key file from a public one.
const PRIVATE_KEY_FIELD: &[u8] = b"\"privateKey\"";

/// Whether this host can read the Unix permission bits of a payload file.
///
/// `cfg!` rather than `#[cfg]` so both branches compile everywhere and the
/// rule below can be tested on any machine.
pub const HOST_RECORDS_UNIX_MODES: bool = cfg!(unix);

/// Refuses a build that would ship a Unix package with no executable bits.
///
/// # The failure this prevents
///
/// Windows has no permission bits to read, so a package built there for a Unix
/// target records `mode: None` for every file. Extraction then falls back to
/// `0o644`, and the bundled interpreter — `runtime/bin/java`, or whatever the
/// launch executable names — arrives without `+x`. The install succeeds, the
/// package verifies, and the application cannot start, with a permission error
/// that names the interpreter rather than the packaging host that caused it.
///
/// That is a long way from the mistake, so it is caught here instead.
///
/// # Why only when the launch executable is bundled
///
/// A bundled launch executable is a file xPack itself is going to run, so a
/// missing `+x` is certain to break it. A package whose launch executable
/// comes from `PATH` may still contain scripts that want the bit, but it may
/// equally contain nothing but data — refusing that outright would block
/// builds that are perfectly correct. Those get a warning instead.
///
/// # Why the host capability is a parameter
///
/// The same reason `install_root_from` takes its override as one: a function
/// that consulted `cfg!(unix)` internally could only be tested on the host it
/// happened to be compiled for, so the branch that matters would ship
/// unexercised on the machine this is developed on.
pub fn ensure_modes_are_recordable(
    target: xpack_core::Platform,
    host_records_modes: bool,
    launch_is_bundled: bool,
) -> Result<()> {
    use xpack_core::Os;

    let target_needs_modes = matches!(target.os, Os::Linux | Os::Macos);
    if host_records_modes || !target_needs_modes {
        return Ok(());
    }

    if launch_is_bundled {
        return Err(Error::invalid(
            "platform",
            format!(
                "this host cannot read Unix permission bits, so a {target} package built here                  would ship its launch executable without the executable bit and the                  application would not start; build {target} packages on a Unix host or in CI"
            ),
        ));
    }

    tracing::warn!(
        %target,
        "this host cannot read Unix permission bits; no payload file will carry one, and          anything in this package that needs to be executable will not be"
    );
    Ok(())
}

#[cfg(unix)]
fn unix_mode(path: &Path) -> Result<Option<u32>> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::metadata(path).map_err(|e| Error::io(path, e))?;
    Ok(Some(metadata.permissions().mode() & 0o777))
}

// Mirrors the Unix version's signature, which reads metadata and can fail.
#[allow(clippy::unnecessary_wraps)]
#[cfg(not(unix))]
fn unix_mode(path: &Path) -> Result<Option<u32>> {
    // Windows has no Unix mode bits to record. Packages built on Windows for a
    // Unix target therefore carry no mode, and the extractor falls back to a
    // safe default; publishers targeting Unix should build on Unix or on CI.
    let _ = path;
    Ok(None)
}

/// Accepts a symlink only when it resolves to a regular file inside the payload.
///
/// Everything downstream reads through the link — `File::open` and
/// `fs::metadata` both follow one — so an accepted link is hashed, recorded and
/// stored as an ordinary file and the archive never contains a link entry. That
/// leaves the extractor's own refusal of link entries untouched.
///
/// A link that leaves the payload is refused. Following one would pull a file
/// off the packaging machine into a package the publisher never assembled and
/// will not think to review, which is how a key, a credential or a host
/// configuration file gets published. A link that does not resolve, or that
/// resolves to something other than a regular file, is refused for the same
/// reason the walk refuses those directly: the manifest can only describe bytes.
fn ensure_link_stays_inside(payload_root: &Path, link: &Path) -> Result<()> {
    let target = link.canonicalize().map_err(|e| {
        Error::invalid(
            "payload",
            format!("{} is a symbolic link that does not resolve: {e}", link.display()),
        )
    })?;

    if !target.starts_with(payload_root) {
        return Err(Error::invalid(
            "payload",
            format!(
                "{} is a symbolic link to {}, which is outside the payload; xPack packages \
                 only files the payload itself contains",
                link.display(),
                target.display()
            ),
        ));
    }

    if !target.is_file() {
        return Err(Error::invalid(
            "payload",
            format!(
                "{} is a symbolic link to {}, which is not a regular file",
                link.display(),
                target.display()
            ),
        ));
    }

    Ok(())
}

fn zip_error(e: &zip::result::ZipError) -> Error {
    Error::invalid("package archive", e.to_string())
}

#[cfg(test)]
mod build_rule_tests {
    use super::*;
    use xpack_core::{Arch, Os, Platform};

    fn target(os: Os) -> Platform {
        Platform::new(os, Arch::X64)
    }

    /// A platform this host is actually allowed to build for.
    ///
    /// The tests below are about rules that have nothing to do with the
    /// target -- a key left in the payload, bytes that must not differ
    /// between builds -- and a Unix target names a package a Windows host
    /// refuses to build at all, because it cannot record the executable bit.
    /// Those tests then fail on the refusal without ever reaching the rule
    /// they exist for.
    ///
    /// Which is exactly what happened: they passed on every machine that
    /// could build a Linux package and failed on the one that could not.
    fn buildable_here() -> Platform {
        Platform::host().expect("a host platform this build knows about")
    }

    #[test]
    fn a_unix_host_may_build_for_anything() {
        for os in [Os::Linux, Os::Macos, Os::Windows] {
            assert!(ensure_modes_are_recordable(target(os), true, true).is_ok());
        }
    }

    #[test]
    fn a_host_without_modes_may_still_build_for_windows() {
        // Windows has no permission bits to lose, so nothing is dropped.
        assert!(ensure_modes_are_recordable(target(Os::Windows), false, true).is_ok());
    }

    #[test]
    fn a_host_without_modes_is_refused_a_unix_target_with_a_bundled_launcher() {
        // This is the build that produces a package which installs cleanly,
        // verifies, and cannot start.
        for os in [Os::Linux, Os::Macos] {
            let err = ensure_modes_are_recordable(target(os), false, true).unwrap_err();
            let text = err.to_string();
            assert!(text.contains("executable bit"), "got {text}");
            assert!(text.contains(&target(os).to_string()), "got {text}");
        }
    }

    #[test]
    fn a_host_without_modes_may_build_a_unix_target_that_runs_a_system_runtime() {
        // Nothing xPack itself executes lives in the payload, so a missing
        // bit is not certain to break anything. It warns rather than refuses.
        assert!(ensure_modes_are_recordable(target(Os::Linux), false, false).is_ok());
    }

    #[test]
    fn every_host_may_build_for_itself() {
        // Why the build-rule tests below name this host's own platform rather
        // than a Unix one. A machine that cannot record a mode bit cannot
        // build a package for a system that needs one -- but it can always
        // build for the system it is, which is the property that lets a test
        // about something else pick a target and not think about it again.
        assert!(
            ensure_modes_are_recordable(buildable_here(), HOST_RECORDS_UNIX_MODES, true).is_ok(),
            "this host cannot package for itself"
        );
    }

    #[test]
    fn the_host_capability_matches_this_platform() {
        assert_eq!(HOST_RECORDS_UNIX_MODES, cfg!(unix));
    }

    /// A payload with one bundled executable, and a manifest that launches it.
    fn fixture(platform: Platform) -> (tempfile::TempDir, Manifest) {
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("payload/bin");
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::write(payload.join("app"), b"#!/bin/sh\nexit 0\n").unwrap();

        let manifest = Manifest {
            format_version: xpack_core::FormatVersion::default(),
            application: xpack_core::Application {
                id: "com.example.app".into(),
                name: "Example".into(),
                version: xpack_core::Version::parse("1.0.0").unwrap(),
                description: None,
                publisher: None,
            },
            platform,
            launch: xpack_core::LaunchSpec {
                executable: "bin/app".into(),
                arguments: Vec::new(),
                working_directory: None,
                keep_working_directory: false,
                environment: std::collections::BTreeMap::new(),
            },
            update: xpack_core::UpdateSpec::default(),
            health: xpack_core::HealthSpec::default(),
            desktop: xpack_core::DesktopSpec::default(),
            payload: xpack_core::PayloadSpec::default(),
            signing_key: None,
            created_at: None,
            command: None,
        };
        (dir, manifest)
    }

    #[test]
    fn a_build_that_would_drop_the_executable_bit_is_refused_and_writes_nothing() {
        // Drives the real `build`, not just the rule. On a Unix host the
        // refusing branch is otherwise unreachable, so it would ship having
        // never run anywhere.
        let (dir, manifest) = fixture(target(Os::Linux));
        let key = xpack_security::KeyPair::generate().unwrap();
        let output = dir.path().join("out.xpkg");

        let payload_root = dir.path().join("payload");
        let mut builder = PackageBuilder::new(&payload_root, manifest);
        builder.host_records_modes = false;

        let err = builder.build(&output, &key).unwrap_err();
        assert!(err.to_string().contains("executable bit"), "got {err}");

        // The refusal happens before any work, so a failed build leaves no
        // half-written package for a release script to pick up.
        assert!(!output.exists(), "a refused build wrote {}", output.display());
    }

    #[test]
    fn a_private_signing_key_in_the_payload_is_refused() {
        // Performs the accident rather than asserting a comment: `xpack
        // keygen` writes into the working directory, `xpack pack .` packages
        // the working directory, and the result would publish the key that
        // authorises every future update.
        let (dir, manifest) = fixture(buildable_here());
        let key = xpack_security::KeyPair::generate().unwrap();
        let payload_root = dir.path().join("payload");

        // A real key file, written by the real code that writes them.
        key.save(&payload_root.join("xpack-signing.json")).unwrap();

        let output = dir.path().join("out.xpkg");
        let err = PackageBuilder::new(&payload_root, manifest).build(&output, &key).unwrap_err();

        assert!(err.to_string().contains("private signing key"), "got {err}");
        assert!(!output.exists(), "a package was written anyway");
    }

    #[test]
    fn a_renamed_private_key_is_still_refused() {
        // The check is on content, so copying the key to `assets/data.bin`
        // does not get it past.
        let (dir, manifest) = fixture(buildable_here());
        let key = xpack_security::KeyPair::generate().unwrap();
        let payload_root = dir.path().join("payload");

        let real = dir.path().join("signing.json");
        key.save(&real).unwrap();
        std::fs::create_dir_all(payload_root.join("assets")).unwrap();
        std::fs::copy(&real, payload_root.join("assets/data.bin")).unwrap();

        let err = PackageBuilder::new(&payload_root, manifest)
            .build(&dir.path().join("out.xpkg"), &key)
            .unwrap_err();
        assert!(err.to_string().contains("private signing key"), "got {err}");
    }

    #[test]
    fn a_public_key_in_the_payload_is_allowed() {
        // Publishing the public key is normal — an application may ship it to
        // pin its own updates. Only the secret half is refused.
        let (dir, manifest) = fixture(buildable_here());
        let key = xpack_security::KeyPair::generate().unwrap();
        let payload_root = dir.path().join("payload");
        key.public().save(&payload_root.join("trusted.pub.json")).unwrap();

        PackageBuilder::new(&payload_root, manifest)
            .build(&dir.path().join("out.xpkg"), &key)
            .expect("a public key is not secret");
    }

    #[test]
    fn an_application_config_using_the_name_private_key_is_not_refused() {
        // A rule that blocks legitimate releases is a rule people route
        // around, so the match requires the whole xPack key shape.
        let (dir, manifest) = fixture(buildable_here());
        let key = xpack_security::KeyPair::generate().unwrap();
        let payload_root = dir.path().join("payload");
        std::fs::write(
            payload_root.join("app-config.json"),
            br#"{"privateKey": "the app's own unrelated setting"}"#,
        )
        .unwrap();

        PackageBuilder::new(&payload_root, manifest)
            .build(&dir.path().join("out.xpkg"), &key)
            .expect("an unrelated config must still package");
    }

    #[test]
    fn packing_the_same_payload_twice_produces_identical_bytes() {
        // Reproducibility is a property a release pipeline relies on to prove
        // an artefact came from the source it claims. Nothing asserted it, so
        // a dependency changing its default timestamp would have broken it
        // silently.
        let (dir, manifest) = fixture(buildable_here());
        let key = xpack_security::KeyPair::generate().unwrap();
        let payload_root = dir.path().join("payload");

        let first = dir.path().join("first.xpkg");
        let second = dir.path().join("second.xpkg");

        PackageBuilder::new(&payload_root, manifest.clone()).build(&first, &key).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        PackageBuilder::new(&payload_root, manifest).build(&second, &key).unwrap();

        assert_eq!(
            std::fs::read(&first).unwrap(),
            std::fs::read(&second).unwrap(),
            "two builds of the same payload differ; the package is not reproducible"
        );
    }

    #[test]
    fn the_same_build_succeeds_on_a_host_that_records_modes() {
        // The control: nothing else about this build is wrong.
        let (dir, manifest) = fixture(target(Os::Linux));
        let key = xpack_security::KeyPair::generate().unwrap();
        let output = dir.path().join("out.xpkg");

        let payload_root = dir.path().join("payload");
        let mut builder = PackageBuilder::new(&payload_root, manifest);
        builder.host_records_modes = true;

        builder.build(&output, &key).expect("this build is otherwise valid");
        assert!(output.is_file());
    }
}
