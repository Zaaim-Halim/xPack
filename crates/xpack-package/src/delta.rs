//! Differential updates: shipping only what changed.
//!
//! A bundled runtime is most of a package and almost never changes. A `JavaFX`
//! application with a 150 MB JDK and a 2 MB jar ships 150 MB to deliver a
//! two-megabyte fix, every release, to every user. A delta ships the jar.
//!
//! # File-level, not binary diffs
//!
//! This carries whole files that changed and reuses the rest from the version
//! already installed. It does **not** compute binary patches between file
//! versions, and that is a decision rather than a stage.
//!
//! For the case above, file-level reuse captures essentially all of the
//! benefit: the JDK's thousands of files are byte-identical, so nothing about
//! them is downloaded, and the one file that changed is sent whole. A binary
//! patch of the jar on top would save a fraction of two megabytes.
//!
//! It also avoids the trap that makes binary diffing hard. A `.jar` is a zip,
//! and so is almost every other application artefact; a one-line source change
//! rewrites the whole compressed stream, so a naive patch between two versions
//! of a compressed file is barely smaller than the file. Chrome's Courgette
//! and Android's file-by-file patching exist precisely to work around this, by
//! decompressing, diffing and recompressing *deterministically* — which
//! requires reproducing the exact compressor the publisher used. That is a
//! large amount of machinery for the remaining fraction.
//!
//! # The security invariant
//!
//! **A delta carries the target version's manifest and signature, byte for
//! byte identical to the full package's, and the assembled result is verified
//! file by file against that signed manifest.**
//!
//! Everything follows from it:
//!
//! * A delta needs **no signature of its own**. What is signed is the outcome,
//!   not the transport.
//! * [`DeltaPlan`] is unsigned and attacker-controlled, so it is a *hint*:
//!   which version to look for. Nothing it says is trusted. The signature over
//!   the manifest is checked before the plan is allowed to influence anything.
//! * A file reused from the installed version is hashed as it is copied, by
//!   the same code that hashes an extracted one. A tampered file on disk fails
//!   the target manifest's digest.
//! * A delta that omits a file cannot produce a short installation: assembly
//!   counts what it wrote from both sources and requires it to equal what the
//!   manifest declares.
//!
//! The worst a hostile delta achieves is a failed assembly, which the caller
//! answers by downloading the full package.
//!
//! # Failure is never fatal
//!
//! A delta is an optimisation. A missing base version, a pruned one, a file
//! that no longer hashes correctly, a malformed archive — every one of them is
//! an error from assembly and a signal to the caller to fetch the full
//! package. Nothing here may leave a partial tree behind for a later step to
//! mistake for a finished one.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use xpack_core::manifest::PAYLOAD_PREFIX;
use xpack_core::progress::{NoProgress, ProgressEvent, ProgressReporter};
use xpack_core::{Error, Manifest, Result, Version, atomic};

use crate::entry_path::safe_payload_path;
use crate::reader::{VerifiedPackage, write_verified_entry};

/// Archive entry naming the version a delta applies to.
pub const DELTA_ENTRY: &str = "delta.json";

/// Largest delta plan accepted, to bound work before parsing.
pub const MAX_PLAN_BYTES: usize = 64 * 1024;

/// Conventional extension for a delta package.
pub const DELTA_EXTENSION: &str = "xpkgd";

/// What a delta says about itself.
///
/// **Unsigned and untrusted.** It exists so the updater can tell, before
/// downloading, which installed version a delta is good for. Every value is
/// re-checked against the signed manifest or against installation state
/// before it can affect the result.
///
/// Deliberately minimal. An earlier shape also listed which files were
/// expected to come from the base; it was removed because assembly can derive
/// that — anything the manifest declares and the archive does not carry comes
/// from the base — and a field that is never read is a field an attacker can
/// still lie in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeltaPlan {
    /// Format of this document.
    pub format_version: u32,
    /// The version this delta must be applied to.
    pub base_version: Version,
}

impl DeltaPlan {
    /// Format written by this build.
    pub const CURRENT_VERSION: u32 = 1;

    /// Creates a plan naming the base version.
    pub fn new(base_version: Version) -> Self {
        Self { format_version: Self::CURRENT_VERSION, base_version }
    }

    /// Parses a plan, bounded before allocation.
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_PLAN_BYTES {
            return Err(Error::invalid("delta plan", "document is too large"));
        }
        let plan: Self = serde_json::from_slice(bytes).map_err(|e| Error::json("delta plan", e))?;
        if plan.format_version > Self::CURRENT_VERSION {
            return Err(Error::invalid(
                "delta plan",
                format!(
                    "is format {} but this build understands {}",
                    plan.format_version,
                    Self::CURRENT_VERSION
                ),
            ));
        }
        Ok(plan)
    }

    /// Serialises the plan.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec_pretty(self).map_err(|e| Error::json("delta plan", e))
    }
}

/// A delta whose signature verified, ready to assemble.
///
/// A distinct type from [`VerifiedPackage`] on purpose: a delta is incomplete
/// by construction, so extracting it as if it were a whole package would fail
/// — and it should be impossible to try rather than merely unsuccessful.
pub struct VerifiedDelta {
    package: VerifiedPackage,
    plan: DeltaPlan,
}

impl VerifiedDelta {
    /// Wraps a verified package as a delta.
    pub(crate) fn new(package: VerifiedPackage, plan: DeltaPlan) -> Self {
        Self { package, plan }
    }

    /// The target version's signed manifest.
    pub fn manifest(&self) -> &Manifest {
        self.package.manifest()
    }

    /// The exact manifest bytes the signature was verified over.
    pub fn manifest_bytes(&self) -> &[u8] {
        self.package.manifest_bytes()
    }

    /// The signature over those bytes.
    pub fn signature(&self) -> &xpack_security::Signature {
        self.package.signature()
    }

    /// The key that signed it.
    pub fn signing_key(&self) -> &xpack_security::PublicKey {
        self.package.signing_key()
    }

    /// The version this delta expects to be applied to.
    pub fn base_version(&self) -> &Version {
        &self.plan.base_version
    }

    /// Refuses a delta that does not apply to the version actually installed.
    ///
    /// The caller passes what **state** says is installed; the delta's claim is
    /// checked against that, never the other way round. Resolving the base
    /// from the delta would let whoever serves the index choose which
    /// directory is read.
    pub fn ensure_applies_to(&self, installed: &Version) -> Result<()> {
        if &self.plan.base_version == installed {
            return Ok(());
        }
        Err(Error::invalid(
            "delta",
            format!("applies to version {} but {installed} is installed", self.plan.base_version),
        ))
    }

    /// Assembles the target version into `destination`.
    pub fn assemble_to(&mut self, destination: &Path, base_dir: &Path) -> Result<()> {
        self.assemble_to_with_progress(destination, base_dir, &NoProgress)
    }

    /// Assembles, reporting each file as it is written and verified.
    ///
    /// Leaves nothing behind on failure. A half-assembled tree would be
    /// indistinguishable from a finished one to a later step, and the caller's
    /// answer to a failed delta is to fetch the full package into the same
    /// place.
    pub fn assemble_to_with_progress(
        &mut self,
        destination: &Path,
        base_dir: &Path,
        progress: &dyn ProgressReporter,
    ) -> Result<()> {
        atomic::remove_dir_all_if_exists(destination)?;
        atomic::create_dir_all(destination)?;

        match self.assemble_inner(destination, base_dir, progress) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = atomic::remove_dir_all_if_exists(destination);
                Err(e)
            }
        }
    }

    fn assemble_inner(
        &mut self,
        destination: &Path,
        base_dir: &Path,
        progress: &dyn ProgressReporter,
    ) -> Result<()> {
        let (archive, manifest) = self.package.archive_and_manifest();
        let declared = manifest.payload.files.clone();
        let budget = manifest.payload.total_size;

        let mut written: u64 = 0;
        let mut assembled = 0usize;
        let mut reused = 0usize;
        // One per assembly, so the filesystem is asked what it supports once
        // rather than once per file.
        let sharer = xpack_platform::sharing::Sharer::safe();
        let mut sharing_storage = 0usize;
        let mut verified_dirs = std::collections::BTreeSet::new();

        for expected in &declared {
            // Re-derived from the signed manifest, not taken from any archive
            // entry name, so the destination path is decided by signed data.
            let entry_name = format!("{PAYLOAD_PREFIX}{}", expected.path);
            let Some(safe) = safe_payload_path(&entry_name)? else {
                return Err(Error::Integrity(format!(
                    "the signed manifest declares {:?}, which is not a usable payload path",
                    expected.path
                )));
            };

            written =
                written.checked_add(expected.size).filter(|w| *w <= budget).ok_or_else(|| {
                    Error::Integrity(format!(
                        "payload exceeds its declared total of {budget} bytes"
                    ))
                })?;

            // Present in the delta means changed; absent means reuse it from
            // the version already on disk. Either way the bytes are hashed as
            // they are written, by the same function.
            if let Ok(mut entry) = archive.by_name(&entry_name) {
                write_verified_entry(&mut entry, &safe, expected, destination, &mut verified_dirs)?;
            } else {
                let source = join_relative(base_dir, &expected.path);
                if !source.is_file() {
                    return Err(Error::invalid(
                        "delta",
                        format!(
                            "{} is needed from the installed version and is not there",
                            source.display()
                        ),
                    ));
                }
                // Adopted rather than copied. The bytes are already on this
                // disk and identical, so writing them again buys nothing but
                // a second copy of a runtime the user is keeping anyway.
                let placement = crate::reader::place_verified_entry(
                    &source,
                    &sharer,
                    &safe,
                    expected,
                    destination,
                    &mut verified_dirs,
                )?;
                if placement.shares_storage() {
                    sharing_storage += 1;
                }
                reused += 1;
            }

            assembled += 1;
            progress.report(&ProgressEvent::ExtractionProgress {
                files_completed: assembled,
                files_total: declared.len(),
                bytes_completed: written,
                bytes_total: budget,
            });
        }

        // The assertion that makes "a delta cannot omit a file" true. It
        // counts both sources, so neither a short archive nor a missing base
        // file can produce an installation the manifest does not describe.
        if assembled != declared.len() {
            return Err(Error::Integrity(format!(
                "assembled {assembled} files but the manifest declares {}",
                declared.len()
            )));
        }

        atomic::sync_dir(destination)?;
        tracing::debug!(
            destination = %destination.display(),
            files = assembled,
            reused,
            // How many of the reused files cost no disk, which is the whole
            // reason for adopting them rather than copying.
            sharing_storage,
            downloaded = assembled - reused,
            "delta assembled and verified"
        );
        Ok(())
    }
}

/// Joins a manifest-relative path, which always uses `/`, onto a directory.
fn join_relative(base: &Path, relative: &str) -> PathBuf {
    let mut joined = base.to_path_buf();
    for component in relative.replace('\\', "/").split('/') {
        joined.push(component);
    }
    joined
}

/// What building a delta produced.
#[derive(Debug, Clone)]
pub struct BuiltDelta {
    /// Where the delta was written.
    pub path: PathBuf,
    /// Version it applies to.
    pub base_version: Version,
    /// Version it produces.
    pub target_version: Version,
    /// Files carried because they changed or are new.
    pub changed: usize,
    /// Files the installed version already has.
    pub reused: usize,
    /// Size of the delta in bytes.
    pub size: u64,
    /// Size of the full package it replaces.
    pub full_size: u64,
}

impl BuiltDelta {
    /// Fraction of the full package's size this delta is, as a percentage.
    pub fn percentage_of_full(&self) -> u64 {
        if self.full_size == 0 {
            return 100;
        }
        self.size.saturating_mul(100) / self.full_size
    }
}

/// Builds a delta that turns `base` into `target`.
///
/// Both arguments are finished `.xpkg` files: a delta is derived from what was
/// actually published, never from a working tree, so it cannot describe a
/// transition between two things no user ever had.
///
/// # What ends up inside
///
/// The target's manifest and signature, **copied byte for byte** — not
/// re-serialised, because re-serialising a parsed manifest produces different
/// bytes and the signature would no longer verify. Then a plan naming the base
/// version, and the payload files whose digest differs from the base's, or
/// which the base does not have at all.
///
/// A file is reused when its path and its SHA-256 both match. Comparing
/// digests rather than sizes or timestamps is what makes the decision exact:
/// two files that hash the same are the same file, and the assembled result is
/// checked against the target manifest regardless.
pub fn build(base: &Path, target: &Path, output: &Path) -> Result<BuiltDelta> {
    use std::io::Write;

    let base_manifest = read_manifest(base)?;
    let target_manifest = read_manifest(target)?;

    if base_manifest.application.id != target_manifest.application.id {
        return Err(Error::invalid(
            "delta",
            format!(
                "{:?} and {:?} are different applications",
                base_manifest.application.id, target_manifest.application.id
            ),
        ));
    }
    if base_manifest.platform != target_manifest.platform {
        return Err(Error::invalid(
            "delta",
            format!(
                "{} and {} target different platforms",
                base_manifest.platform, target_manifest.platform
            ),
        ));
    }
    if base_manifest.application.version == target_manifest.application.version {
        return Err(Error::invalid(
            "delta",
            format!("both packages are version {}", base_manifest.application.version),
        ));
    }

    let already: std::collections::BTreeMap<&str, &xpack_core::digest::Sha256Digest> =
        base_manifest.payload.files.iter().map(|f| (f.path.as_str(), &f.sha256)).collect();

    let changed: Vec<&xpack_core::PayloadFile> = target_manifest
        .payload
        .files
        .iter()
        .filter(|f| already.get(f.path.as_str()).is_none_or(|digest| **digest != f.sha256))
        .collect();

    let mut source = open_archive(target)?;
    let manifest_bytes = raw_entry(&mut source, xpack_core::MANIFEST_ENTRY)?;
    let signature_bytes = raw_entry(&mut source, xpack_core::SIGNATURE_ENTRY)?;
    let plan = DeltaPlan::new(base_manifest.application.version.clone());

    if let Some(parent) = output.parent() {
        atomic::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(output).map_err(|e| Error::io(output, e))?;
    let mut writer = zip::ZipWriter::new(std::io::BufWriter::new(file));
    let stored = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::DEFAULT);
    let deflated = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::DEFAULT);

    // Byte for byte, so the signature still verifies.
    writer.start_file(xpack_core::MANIFEST_ENTRY, stored).map_err(|e| zip_error(&e))?;
    writer.write_all(&manifest_bytes).map_err(Error::BareIo)?;
    writer.start_file(xpack_core::SIGNATURE_ENTRY, stored).map_err(|e| zip_error(&e))?;
    writer.write_all(&signature_bytes).map_err(Error::BareIo)?;
    writer.start_file(DELTA_ENTRY, stored).map_err(|e| zip_error(&e))?;
    writer.write_all(&plan.to_bytes()?).map_err(Error::BareIo)?;

    for entry in &changed {
        let name = format!("{PAYLOAD_PREFIX}{}", entry.path);
        let bytes = raw_entry(&mut source, &name)?;
        writer.start_file(&name, deflated).map_err(|e| zip_error(&e))?;
        writer.write_all(&bytes).map_err(Error::BareIo)?;
    }

    let finished = writer.finish().map_err(|e| zip_error(&e))?;
    let handle = finished.into_inner().map_err(|e| Error::io(output, e.into_error()))?;
    handle.sync_all().map_err(|e| Error::io(output, e))?;
    atomic::sync_dir(atomic::parent_dir(output)?)?;

    let size = std::fs::metadata(output).map_err(|e| Error::io(output, e))?.len();
    let full_size = std::fs::metadata(target).map_err(|e| Error::io(target, e))?.len();

    tracing::info!(
        delta = %output.display(),
        from = %base_manifest.application.version,
        to = %target_manifest.application.version,
        changed = changed.len(),
        reused = target_manifest.payload.files.len() - changed.len(),
        "delta built"
    );

    Ok(BuiltDelta {
        path: output.to_path_buf(),
        base_version: base_manifest.application.version,
        target_version: target_manifest.application.version,
        changed: changed.len(),
        reused: target_manifest.payload.files.len() - changed.len(),
        size,
        full_size,
    })
}

/// Reads a package's manifest without verifying it.
///
/// Safe here because building a delta decides only *what to ship*. The
/// manifest that governs installation is the one copied into the delta, and it
/// is verified on the user's machine against a key pinned there.
fn read_manifest(package: &Path) -> Result<Manifest> {
    crate::PackageReader::open(package)?.peek_manifest_unverified()
}

fn open_archive(path: &Path) -> Result<zip::ZipArchive<std::io::BufReader<std::fs::File>>> {
    let file = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| Error::invalid("package", format!("{}: {e}", path.display())))
}

/// Reads one archive entry whole.
fn raw_entry(
    archive: &mut zip::ZipArchive<std::io::BufReader<std::fs::File>>,
    name: &str,
) -> Result<Vec<u8>> {
    use std::io::Read;

    let mut entry = archive
        .by_name(name)
        .map_err(|e| Error::invalid("package", format!("{name} is missing: {e}")))?;
    let mut bytes = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or(0));
    entry.read_to_end(&mut bytes).map_err(Error::BareIo)?;
    Ok(bytes)
}

fn zip_error(e: &zip::result::ZipError) -> Error {
    Error::invalid("delta archive", e.to_string())
}

impl std::fmt::Debug for VerifiedDelta {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifiedDelta")
            .field("base_version", &self.plan.base_version)
            .field("target", &self.manifest().application.version)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_round_trips() {
        let plan = DeltaPlan::new(Version::parse("1.0.0").unwrap());
        assert_eq!(DeltaPlan::from_slice(&plan.to_bytes().unwrap()).unwrap(), plan);
    }

    #[test]
    fn a_plan_from_a_newer_build_is_refused_rather_than_misread() {
        let json = br#"{"formatVersion": 99, "baseVersion": "1.0.0"}"#;
        let err = DeltaPlan::from_slice(json).unwrap_err();
        assert!(err.to_string().contains("understands"), "got {err}");
    }

    #[test]
    fn an_oversized_plan_is_refused_before_parsing() {
        let huge = vec![b'x'; MAX_PLAN_BYTES + 1];
        assert!(DeltaPlan::from_slice(&huge).is_err());
    }

    #[test]
    fn an_unknown_field_is_refused() {
        // `deny_unknown_fields`: a field this build ignores is a field a
        // future one gives meaning to, and silently dropping it would make an
        // old client disagree with a new one about what a delta said.
        let json = br#"{"formatVersion": 1, "baseVersion": "1.0.0", "reused": ["a"]}"#;
        assert!(DeltaPlan::from_slice(json).is_err());
    }

    #[test]
    fn the_saving_is_reported_against_the_full_package() {
        let built = BuiltDelta {
            path: PathBuf::from("d.xpkgd"),
            base_version: Version::parse("1.0.0").unwrap(),
            target_version: Version::parse("1.1.0").unwrap(),
            changed: 1,
            reused: 99,
            size: 5,
            full_size: 100,
        };
        assert_eq!(built.percentage_of_full(), 5);
    }

    #[test]
    fn a_zero_sized_full_package_does_not_divide_by_zero() {
        let built = BuiltDelta {
            path: PathBuf::from("d.xpkgd"),
            base_version: Version::parse("1.0.0").unwrap(),
            target_version: Version::parse("1.1.0").unwrap(),
            changed: 0,
            reused: 0,
            size: 0,
            full_size: 0,
        };
        assert_eq!(built.percentage_of_full(), 100);
    }
}
