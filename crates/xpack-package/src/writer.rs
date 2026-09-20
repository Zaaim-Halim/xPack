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
}

impl<'a> PackageBuilder<'a> {
    /// Prepares a build from `payload_root` using `manifest` as the template.
    ///
    /// The template's payload inventory is ignored and recomputed from what is
    /// actually on disk: a manifest that disagrees with its own archive would
    /// produce a package that fails verification on every client.
    pub fn new(payload_root: &'a Path, manifest: Manifest) -> Self {
        Self { payload_root, manifest }
    }

    /// Hashes the payload tree and writes a signed `.xpkg` to `output`.
    pub fn build(mut self, output: &Path, key: &KeyPair) -> Result<PackedPackage> {
        if !self.payload_root.is_dir() {
            return Err(Error::invalid(
                "payload root",
                format!("{} is not a directory", self.payload_root.display()),
            ));
        }

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
            // Symlinks are refused rather than followed or stored. Following
            // one would silently pull in a file from outside the payload;
            // storing one would hand the extractor an arbitrary write
            // primitive on the target machine.
            if file_type.is_symlink() {
                return Err(Error::invalid(
                    "payload",
                    format!(
                        "{} is a symbolic link; xPack packages must contain regular files only",
                        entry.path().display()
                    ),
                ));
            }
            if !file_type.is_file() {
                return Err(Error::invalid(
                    "payload",
                    format!("{} is not a regular file", entry.path().display()),
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

        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let deflated = SimpleFileOptions::default()
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

fn zip_error(e: &zip::result::ZipError) -> Error {
    Error::invalid("package archive", e.to_string())
}
