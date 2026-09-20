//! Verifying and extracting a `.xpkg`.
//!
//! # Type-state: verification is not optional
//!
//! [`PackageReader`] cannot extract anything. Extraction lives on
//! [`VerifiedPackage`], and the only way to obtain one is
//! [`PackageReader::verify`]. A caller who forgets to check the signature does
//! not get an insecure install — they get a compile error.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};

use xpack_core::atomic;
use xpack_core::manifest::{MANIFEST_ENTRY, MAX_MANIFEST_BYTES, SIGNATURE_ENTRY};
use xpack_core::progress::{NoProgress, ProgressEvent, ProgressReporter};
use xpack_core::{Error, Manifest, Platform, Result};
use xpack_security::Hasher;
use xpack_security::keys::PublicKey;
use xpack_security::signature::{self, Signature};
use xpack_security::trust::TrustStore;
use zip::ZipArchive;

use crate::entry_path::{SafePath, safe_payload_path};

/// Longest detached signature document accepted, in bytes.
const MAX_SIGNATURE_BYTES: usize = 512;

/// Copy buffer for extraction.
const CHUNK: usize = 64 * 1024;

type Archive = ZipArchive<BufReader<File>>;

/// An opened but **unverified** package.
pub struct PackageReader {
    archive: Archive,
    path: PathBuf,
}

impl PackageReader {
    /// Opens a `.xpkg` without trusting anything inside it.
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(|e| Error::io(path, e))?;
        let archive = ZipArchive::new(BufReader::new(file)).map_err(|e| {
            Error::invalid("package", format!("{} is not a valid .xpkg: {e}", path.display()))
        })?;
        Ok(Self { archive, path: path.to_path_buf() })
    }

    /// Parses the manifest **without checking the signature**.
    ///
    /// The deliberately awkward name is the point: the result is
    /// attacker-controlled. It exists so `xpack inspect` can describe an
    /// untrusted file and so trust-on-first-use can show an operator the key
    /// they are about to pin. Nothing is ever installed from it.
    pub fn peek_manifest_unverified(&mut self) -> Result<Manifest> {
        Manifest::from_slice(&self.read_manifest_bytes()?)
    }

    /// The signing key the package declares, **without verifying anything**.
    ///
    /// Only trust-on-first-use should call this, and only to decide which key
    /// to pin. The returned key proves nothing by itself; the caller must
    /// still verify the package against it, and doing so proves only internal
    /// consistency.
    pub fn declared_signing_key_unverified(&mut self) -> Result<PublicKey> {
        let manifest = self.peek_manifest_unverified()?;
        let declared = manifest.signing_key.ok_or_else(|| {
            Error::Integrity(
                "the package declares no signing key, so there is nothing to pin on first use"
                    .to_string(),
            )
        })?;
        PublicKey::parse_hex(&declared)
    }

    /// Verifies the package against a pinned trust store.
    ///
    /// Order is fixed: raw bytes, then signature, then parse. Parsing first
    /// would mean acting on unauthenticated input, and re-serialising a parsed
    /// manifest to verify it would break on any serialiser whitespace or
    /// key-order difference.
    pub fn verify(mut self, trust: &TrustStore) -> Result<VerifiedPackage> {
        let manifest_bytes = self.read_manifest_bytes()?;
        let signature = self.read_signature()?;

        let signing_key = trust
            .verify(&manifest_bytes, &signature)
            .map_err(|e| Error::Integrity(format!("{}: {e}", self.path.display())))?;

        self.finish_verification(&manifest_bytes, signature, signing_key)
    }

    /// Verifies against an explicit key list, bypassing the trust store.
    ///
    /// Used by `xpack verify --key` and by the first install, where the pin
    /// does not exist yet.
    pub fn verify_with_keys(mut self, keys: &[PublicKey]) -> Result<VerifiedPackage> {
        let manifest_bytes = self.read_manifest_bytes()?;
        let signature = self.read_signature()?;
        let signing_key = signature::verify_any(keys, &manifest_bytes, &signature)
            .map_err(|e| Error::Integrity(format!("{}: {e}", self.path.display())))?
            .clone();
        self.finish_verification(&manifest_bytes, signature, signing_key)
    }

    fn finish_verification(
        mut self,
        manifest_bytes: &[u8],
        signature: Signature,
        signing_key: PublicKey,
    ) -> Result<VerifiedPackage> {
        // Only now are the bytes authentic, so only now may they be parsed.
        let manifest = Manifest::from_slice(manifest_bytes)?;
        self.check_archive_matches_manifest(&manifest)?;

        tracing::debug!(
            package = %self.path.display(),
            application = %manifest.application.id,
            version = %manifest.application.version,
            key = %signing_key.fingerprint(),
            "package signature verified"
        );

        Ok(VerifiedPackage {
            archive: self.archive,
            path: self.path,
            manifest,
            manifest_bytes: manifest_bytes.to_vec(),
            signature,
            signing_key,
        })
    }

    /// Reads the raw manifest bytes, bounded before allocation.
    fn read_manifest_bytes(&mut self) -> Result<Vec<u8>> {
        self.read_entry(MANIFEST_ENTRY, MAX_MANIFEST_BYTES)
    }

    fn read_signature(&mut self) -> Result<Signature> {
        let bytes = self.read_entry(SIGNATURE_ENTRY, MAX_SIGNATURE_BYTES)?;
        let text = String::from_utf8(bytes).map_err(|_| {
            Error::Integrity(format!("{}: signature is not valid UTF-8", self.path.display()))
        })?;
        Signature::parse_hex(&text)
    }

    /// Reads a small entry, refusing to allocate more than `limit` bytes.
    ///
    /// The declared uncompressed size is attacker-controlled, so it is used
    /// only to fail fast; the read itself is independently capped.
    fn read_entry(&mut self, name: &str, limit: usize) -> Result<Vec<u8>> {
        let entry = self.archive.by_name(name).map_err(|e| {
            Error::invalid("package", format!("{} has no {name} entry: {e}", self.path.display()))
        })?;

        if entry.size() > limit as u64 {
            return Err(Error::invalid(
                "package",
                format!("{name} declares {} bytes, limit is {limit}", entry.size()),
            ));
        }

        let mut buffer = Vec::new();
        entry.take(limit as u64 + 1).read_to_end(&mut buffer).map_err(Error::BareIo)?;
        if buffer.len() > limit {
            return Err(Error::invalid("package", format!("{name} exceeds {limit} bytes")));
        }
        Ok(buffer)
    }

    /// Rejects any archive whose entry set disagrees with the signed manifest.
    ///
    /// The manifest authenticates the files it lists. An archive carrying an
    /// *extra* entry is not covered by the signature at all, so it must not be
    /// extracted — and its presence means the package is not what was signed.
    fn check_archive_matches_manifest(&mut self, manifest: &Manifest) -> Result<()> {
        let declared: BTreeMap<&str, &xpack_core::PayloadFile> =
            manifest.payload.files.iter().map(|f| (f.path.as_str(), f)).collect();
        let mut seen = std::collections::BTreeSet::new();

        for index in 0..self.archive.len() {
            let entry = self.archive.by_index_raw(index).map_err(|e| {
                Error::invalid("package", format!("cannot read archive entry {index}: {e}"))
            })?;
            let name = entry.name().to_string();

            if name == MANIFEST_ENTRY || name == SIGNATURE_ENTRY {
                continue;
            }

            // A symlink entry hands the extractor an arbitrary-write primitive:
            // create `link -> /etc/cron.d`, then write through it.
            if is_symlink(entry.unix_mode()) {
                return Err(Error::UnsafeEntry {
                    entry: name,
                    reason: "archive contains a symbolic link".to_string(),
                });
            }

            let Some(safe) = safe_payload_path(&name)? else {
                continue; // the payload/ directory entry itself
            };
            if entry.is_dir() {
                continue;
            }

            if !declared.contains_key(safe.as_str()) {
                return Err(Error::Integrity(format!(
                    "archive contains {:?}, which the signed manifest does not cover",
                    safe.as_str()
                )));
            }
            if !seen.insert(safe.as_str().to_string()) {
                return Err(Error::Integrity(format!(
                    "archive contains {:?} more than once",
                    safe.as_str()
                )));
            }
        }

        if seen.len() != declared.len() {
            let missing: Vec<&str> =
                declared.keys().copied().filter(|p| !seen.contains(*p)).take(5).collect();
            return Err(Error::Integrity(format!(
                "archive is missing {} manifest file(s), e.g. {missing:?}",
                declared.len() - seen.len()
            )));
        }

        Ok(())
    }
}

/// A package whose signature verified against a trusted key.
///
/// Holds the open archive, so extraction cannot be pointed at a different
/// file than the one that was verified.
pub struct VerifiedPackage {
    archive: Archive,
    path: PathBuf,
    manifest: Manifest,
    /// The exact bytes the signature was verified over.
    manifest_bytes: Vec<u8>,
    signature: Signature,
    signing_key: PublicKey,
}

impl VerifiedPackage {
    /// The authenticated manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The exact manifest bytes the signature was verified over.
    ///
    /// Callers writing these to disk must write them verbatim. Re-serialising
    /// a parsed manifest would produce different bytes, and the signature
    /// beside them would no longer verify.
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }

    /// The signature over [`Self::manifest_bytes`].
    pub fn signature(&self) -> &Signature {
        &self.signature
    }

    /// The trusted key whose signature matched.
    pub fn signing_key(&self) -> &PublicKey {
        &self.signing_key
    }

    /// Path of the `.xpkg` on disk.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Rejects a package built for a different operating system or CPU.
    pub fn ensure_installable_on(&self, host: Platform) -> Result<()> {
        if self.manifest.platform.accepts(host) {
            Ok(())
        } else {
            Err(Error::PlatformMismatch {
                package: self.manifest.platform.to_string(),
                host: host.to_string(),
            })
        }
    }

    /// Extracts the payload into `destination`, verifying every file.
    ///
    /// `destination` must be a staging directory, not a live version
    /// directory: a file whose hash fails is only detected part-way through,
    /// so the caller promotes the tree into place only after this returns
    /// `Ok`. The whole directory is removed on any failure, leaving nothing
    /// half-written for a later run to mistake for a complete install.
    pub fn extract_to(&mut self, destination: &Path) -> Result<()> {
        self.extract_to_with_progress(destination, &NoProgress)
    }

    /// Extracts, reporting each file as it is written and verified.
    ///
    /// [`Self::extract_to`] is this with a reporter that discards everything,
    /// so both paths run identical code. Verification is deliberately not
    /// parameterised: the reporter is told what happened and has no way to
    /// influence it.
    pub fn extract_to_with_progress(
        &mut self,
        destination: &Path,
        progress: &dyn ProgressReporter,
    ) -> Result<()> {
        atomic::remove_dir_all_if_exists(destination)?;
        atomic::create_dir_all(destination)?;

        match self.extract_inner(destination, progress) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = atomic::remove_dir_all_if_exists(destination);
                Err(e)
            }
        }
    }

    fn extract_inner(&mut self, destination: &Path, progress: &dyn ProgressReporter) -> Result<()> {
        let declared: BTreeMap<String, xpack_core::PayloadFile> =
            self.manifest.payload.files.iter().map(|f| (f.path.clone(), f.clone())).collect();

        let budget = self.manifest.payload.total_size;
        let mut written: u64 = 0;
        let mut extracted = 0usize;
        // Directories already proven free of symlinked components. Checking
        // once per directory rather than once per file keeps the cost
        // proportional to the tree's shape, not to its file count.
        let mut verified_dirs: BTreeSet<PathBuf> = BTreeSet::new();

        for index in 0..self.archive.len() {
            let mut entry = self.archive.by_index(index).map_err(|e| {
                Error::invalid("package", format!("cannot read archive entry {index}: {e}"))
            })?;
            let name = entry.name().to_string();

            if name == MANIFEST_ENTRY || name == SIGNATURE_ENTRY {
                continue;
            }
            let Some(safe) = safe_payload_path(&name)? else {
                continue;
            };
            if entry.is_dir() {
                continue;
            }

            // `check_archive_matches_manifest` already proved this lookup
            // succeeds; the error path is kept so a future refactor that
            // reorders the two cannot silently extract an uncovered file.
            let expected = declared.get(safe.as_str()).ok_or_else(|| {
                Error::Integrity(format!("{:?} is not covered by the manifest", safe.as_str()))
            })?;

            written =
                written.checked_add(expected.size).filter(|w| *w <= budget).ok_or_else(|| {
                    Error::Integrity(format!(
                        "payload exceeds its declared total of {budget} bytes"
                    ))
                })?;

            self::write_verified_entry(
                &mut entry,
                &safe,
                expected,
                destination,
                &mut verified_dirs,
            )?;
            extracted += 1;
            progress.report(&ProgressEvent::ExtractionProgress {
                files_completed: extracted,
                files_total: declared.len(),
                bytes_completed: written,
                bytes_total: budget,
            });
        }

        if extracted != declared.len() {
            return Err(Error::Integrity(format!(
                "extracted {extracted} files but the manifest declares {}",
                declared.len()
            )));
        }

        atomic::sync_dir(destination)?;
        tracing::debug!(
            destination = %destination.display(),
            files = extracted,
            bytes = written,
            "payload extracted and verified"
        );
        Ok(())
    }
}

/// Streams one entry to disk, hashing as it goes.
///
/// The hash is computed over what is actually written, and the file is
/// discarded if it does not match. Hashing the archive entry separately from
/// writing it would leave a window in which the two could differ.
fn write_verified_entry(
    entry: &mut impl Read,
    safe: &SafePath,
    expected: &xpack_core::PayloadFile,
    destination: &Path,
    verified_dirs: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let target = safe.resolve(destination);
    let parent = atomic::parent_dir(&target)?;
    atomic::create_dir_all(parent)?;
    ensure_no_symlinked_component(destination, safe, verified_dirs)?;

    let mut hasher = Hasher::new();
    let mut total: u64 = 0;
    let mut buffer = vec![0u8; CHUNK];

    {
        // `create_new` rather than `create`: staging was emptied before
        // extraction, so a file already existing here means two manifest
        // entries resolved to the same path on this filesystem. That happens
        // whenever the entry names differ as strings but not as paths —
        // Unicode NFC vs NFD (the same file on APFS), case differences on
        // Windows and case-insensitive volumes, and anything else a
        // filesystem folds together.
        //
        // Silently allowing the second write would leave the last entry's
        // content at a path the manifest attributes to the first entry's
        // hash, so the extracted tree would no longer match the manifest it
        // was verified against. Refusing catches every folding rule the local
        // filesystem implements without xPack having to model any of them.
        let file = File::create_new(&target).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                Error::Integrity(format!(
                    "{:?} collides with another payload entry on this filesystem; \
                     two manifest entries resolve to the same file",
                    safe.as_str()
                ))
            } else {
                Error::io(&target, e)
            }
        })?;
        let mut writer = BufWriter::new(file);

        loop {
            let read = entry.read(&mut buffer).map_err(Error::BareIo)?;
            if read == 0 {
                break;
            }
            total = total.checked_add(read as u64).ok_or_else(|| {
                Error::Integrity(format!("{:?} is implausibly large", safe.as_str()))
            })?;
            // Stop the moment the stream exceeds its signed size, rather than
            // filling the disk and discovering the mismatch afterwards.
            if total > expected.size {
                return Err(Error::Integrity(format!(
                    "{:?} is larger than the {} bytes declared in the manifest",
                    safe.as_str(),
                    expected.size
                )));
            }
            hasher.update(&buffer[..read]);
            writer.write_all(&buffer[..read]).map_err(|e| Error::io(&target, e))?;
        }

        writer.flush().map_err(|e| Error::io(&target, e))?;
        let file = writer.into_inner().map_err(|e| Error::io(&target, e.into_error()))?;
        file.sync_all().map_err(|e| Error::io(&target, e))?;
    }

    if total != expected.size {
        return Err(Error::Integrity(format!(
            "{:?} is {total} bytes but the manifest declares {}",
            safe.as_str(),
            expected.size
        )));
    }

    let actual = hasher.finish();
    xpack_security::verify_digest(safe.as_str(), actual, expected.sha256)?;

    apply_mode(&target, expected.mode)
}

/// Restores the recorded permission bits.
///
/// Without this the executable bit is lost and the launcher fails with a
/// permission error that looks nothing like its actual cause. The manifest is
/// the source of truth, not the archive header, because the manifest is signed.
#[cfg(unix)]
fn apply_mode(path: &Path, mode: Option<u32>) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    // Mask to the permission bits: setuid, setgid and the sticky bit must
    // never be honoured from package metadata.
    let requested = mode.unwrap_or(0o644) & 0o777;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(requested))
        .map_err(|e| Error::io(path, e))
}

// Mirrors the Unix version's signature, which can genuinely fail. Splitting
// the two would move the platform branch out to the call site.
#[allow(clippy::unnecessary_wraps)]
#[cfg(not(unix))]
fn apply_mode(path: &Path, mode: Option<u32>) -> Result<()> {
    let _ = (path, mode);
    Ok(())
}

/// Rejects a target whose parent directories are not all real directories.
///
/// `File::create_new` refuses to follow a symlink at the *final* component, so
/// the file itself cannot be redirected. A symlinked *parent* is not covered:
/// if `staging/sub` is replaced by a link pointing elsewhere, writing
/// `staging/sub/file` lands outside the staging root entirely.
///
/// Extraction creates every one of these directories itself, into a staging
/// tree that was emptied first, so a symlink here means another process is
/// interfering with the installation while it runs.
///
/// This narrows the race rather than eliminating it: a component could still be
/// swapped between this check and the write. Closing it completely requires
/// resolving each component with `openat` and `O_NOFOLLOW`, which `std` does
/// not expose. The residual window is documented rather than hidden.
///
/// # Testing note
///
/// The logic below is unit-tested directly. Its *call site* is not covered,
/// and cannot easily be: [`VerifiedPackage::extract_to`] empties the staging
/// directory before extracting, so a symlink planted beforehand is destroyed
/// and never reaches this check. Removing the call from the extraction path
/// therefore fails no test. That is recorded here rather than papered over
/// with an integration test that would pass whether or not the guard runs.
///
/// The guard exists for the case the emptying cannot cover: a symlink planted
/// *during* extraction, after the directory was created and before the file is
/// written. Exercising that deterministically needs interposition the public
/// API does not offer.
fn ensure_no_symlinked_component(
    root: &Path,
    safe: &SafePath,
    verified: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let mut current = root.to_path_buf();
    let mut components: Vec<&str> = safe.as_str().split('/').collect();
    components.pop(); // the file itself is protected by `create_new`

    for component in components {
        current.push(component);
        if verified.contains(&current) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&current).map_err(|e| Error::io(&current, e))?;
        if metadata.file_type().is_symlink() {
            return Err(Error::UnsafeEntry {
                entry: safe.as_str().to_string(),
                reason: format!(
                    "{} is a symbolic link; extraction would write outside the staging directory",
                    current.display()
                ),
            });
        }
        if !metadata.is_dir() {
            return Err(Error::UnsafeEntry {
                entry: safe.as_str().to_string(),
                reason: format!("{} is not a directory", current.display()),
            });
        }
        verified.insert(current.clone());
    }
    Ok(())
}

/// Returns `true` when ZIP external attributes describe a symbolic link.
fn is_symlink(unix_mode: Option<u32>) -> bool {
    const S_IFMT: u32 = 0o170_000;
    const S_IFLNK: u32 = 0o120_000;
    unix_mode.is_some_and(|m| m & S_IFMT == S_IFLNK)
}

impl VerifiedPackage {
    /// Total uncompressed payload size declared by the signed manifest.
    pub fn payload_size(&self) -> u64 {
        self.manifest.payload.total_size
    }
}

impl std::fmt::Debug for VerifiedPackage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifiedPackage")
            .field("path", &self.path)
            .field("application", &self.manifest.application.id)
            .field("version", &self.manifest.application.version.to_string())
            .field("signingKey", &self.signing_key.fingerprint())
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for PackageReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackageReader").field("path", &self.path).finish_non_exhaustive()
    }
}

// `Seek` is required by `ZipArchive`; this assertion keeps the bound visible
// if the reader type is ever changed.
const _: fn() = || {
    fn assert_seek<T: Seek>() {}
    assert_seek::<BufReader<File>>();
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry_path::safe_payload_path;

    fn staging() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("staging");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(root.join("application")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        (dir, root, outside)
    }

    fn check(root: &Path, entry: &str, seen: &mut BTreeSet<PathBuf>) -> Result<()> {
        let safe = safe_payload_path(entry).unwrap().unwrap();
        ensure_no_symlinked_component(root, &safe, seen)
    }

    #[test]
    fn accepts_ordinary_directories() {
        let (_guard, root, _) = staging();
        let mut seen = BTreeSet::new();
        check(&root, "payload/application/app.jar", &mut seen).unwrap();
        assert!(seen.contains(&root.join("application")), "the directory must be cached");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlinked_parent_directory() {
        // `create_new` protects only the final component. A symlinked parent
        // redirects every file written beneath it outside the staging root.
        let (_guard, root, outside) = staging();
        std::fs::remove_dir(root.join("application")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("application")).unwrap();

        let mut seen = BTreeSet::new();
        let err = check(&root, "payload/application/app.jar", &mut seen).unwrap_err();
        assert!(err.is_integrity_failure(), "got {err:?}");
        assert!(err.to_string().contains("symbolic link"), "got {err}");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlink_deep_in_the_path() {
        let (_guard, root, outside) = staging();
        std::fs::create_dir_all(root.join("runtime")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("runtime/bin")).unwrap();

        let mut seen = BTreeSet::new();
        assert!(check(&root, "payload/runtime/bin/java", &mut seen).is_err());
    }

    #[test]
    fn rejects_a_parent_that_is_a_regular_file() {
        let (_guard, root, _) = staging();
        std::fs::write(root.join("notadir"), b"x").unwrap();
        let mut seen = BTreeSet::new();
        let err = check(&root, "payload/notadir/file", &mut seen).unwrap_err();
        assert!(err.to_string().contains("not a directory"), "got {err}");
    }

    #[test]
    fn a_top_level_entry_has_no_parent_to_check() {
        let (_guard, root, _) = staging();
        let mut seen = BTreeSet::new();
        check(&root, "payload/top.txt", &mut seen).unwrap();
        assert!(seen.is_empty());
    }

    #[test]
    fn each_directory_is_checked_only_once() {
        let (_guard, root, _) = staging();
        let mut seen = BTreeSet::new();
        check(&root, "payload/application/a.jar", &mut seen).unwrap();
        let after_first = seen.len();
        check(&root, "payload/application/b.jar", &mut seen).unwrap();
        assert_eq!(seen.len(), after_first, "the cache must prevent repeated work");
    }
}
