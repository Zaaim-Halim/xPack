//! The self-contained installer: one file a user runs to get an application.
//!
//! Every other xPack component assumes an installation already exists. This is
//! the one that creates it, on a machine where nothing of xPack is present and
//! the developer CLI never will be.
//!
//! # What it does, and what it deliberately does not
//!
//! It unpacks its own payload, verifies the application package against the
//! key the publisher pinned into it, and hands the result to
//! [`xpack_install`] — the same installer the CLI and the background updater
//! use. It does not extract archives itself, does not decide trust for itself
//! and does not write installation state for itself.
//!
//! That is the whole design rule. An installer with its own copy of the
//! install path is an installer that misses the next fix to it, and the
//! failure would appear on exactly the machines nobody can reproduce: the ones
//! where the application was installed once and never touched again.
//!
//! # No window
//!
//! It reports on the console and returns an exit code. xPack draws no windows:
//! a second, foreign-looking one that could not be themed or localised to
//! match the application would be worse than none, and it would put a
//! windowing stack inside a binary that has to stay small and cross-compile
//! cleanly. A publisher wanting a graphical installer wraps this.

pub mod bundle;

use std::path::{Path, PathBuf};

use xpack_core::{Error, InstallPaths, Result};
use xpack_install::{InstallOptions, Installer, TrustDecision, open_and_verify};
use xpack_platform::InstallLock;

use bundle::{BINARY_PREFIX, InstallPlan, PACKAGE_ENTRY, PLAN_ENTRY};

/// What an installation run produced.
#[derive(Debug)]
pub struct Outcome {
    /// Where the application was installed.
    pub root: PathBuf,
    /// Version now installed.
    pub version: xpack_core::Version,
    /// Whether it was made active.
    pub activated: bool,
    /// The launcher a user should run.
    pub launcher: PathBuf,
    /// What happened to the desktop entry, if the package asked for one.
    pub desktop: xpack_install::DesktopOutcome,
}

/// The payload, unpacked into a temporary directory.
///
/// Held together with the directory so the files outlive the archive and are
/// cleaned up when the installer exits, however it exits.
pub struct Payload {
    _directory: tempfile::TempDir,
    plan: InstallPlan,
    package: PathBuf,
    binaries: Vec<PathBuf>,
}

impl Payload {
    /// The install plan the publisher wrote at build time.
    pub fn plan(&self) -> &InstallPlan {
        &self.plan
    }

    /// Unpacks a payload archive into a fresh temporary directory.
    ///
    /// Entry names are validated before use: this archive is the one part of
    /// an installer an attacker could rewrite without touching a signature, so
    /// a crafted name must not be able to write outside the temporary
    /// directory. The same rule the package reader applies, applied here.
    pub fn unpack(bytes: &[u8]) -> Result<Self> {
        let directory =
            tempfile::tempdir().map_err(|e| Error::io(Path::new("a temporary directory"), e))?;
        let root = directory.path().to_path_buf();

        let cursor = std::io::Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cursor)
            .map_err(|e| Error::invalid("installer payload", e.to_string()))?;

        let mut plan = None;
        let mut package = None;
        let mut binaries = Vec::new();

        for index in 0..archive.len() {
            let mut entry = archive
                .by_index(index)
                .map_err(|e| Error::invalid("installer payload", e.to_string()))?;
            if entry.is_dir() {
                continue;
            }

            let name = entry.name().to_string();
            let destination = safe_join(&root, &name)?;

            if let Some(parent) = destination.parent() {
                xpack_core::atomic::create_dir_all(parent)?;
            }
            let mut out =
                std::fs::File::create(&destination).map_err(|e| Error::io(&destination, e))?;
            std::io::copy(&mut entry, &mut out).map_err(|e| Error::io(&destination, e))?;
            drop(out);

            if name == PLAN_ENTRY {
                let document: InstallPlan = xpack_core::atomic::read_json(&destination)?;
                document.ensure_supported()?;
                plan = Some(document);
            } else if name == PACKAGE_ENTRY {
                package = Some(destination);
            } else if name.starts_with(BINARY_PREFIX) {
                make_executable(&destination)?;
                binaries.push(destination);
            }
        }

        let plan = plan.ok_or_else(|| {
            Error::invalid("installer payload", format!("contains no {PLAN_ENTRY}"))
        })?;
        let package = package.ok_or_else(|| {
            Error::invalid("installer payload", format!("contains no {PACKAGE_ENTRY}"))
        })?;

        binaries.sort();
        Ok(Self { _directory: directory, plan, package, binaries })
    }

    /// Installs the application into `root`.
    ///
    /// `root` is the directory holding every xPack application for this user,
    /// not this application's own directory — the layout appends the
    /// application id itself, exactly as every other entry point does.
    pub fn install_into(&self, root: &Path) -> Result<Outcome> {
        let paths = InstallPaths::new(root, &self.plan.application_id)?;
        let key = xpack_security::PublicKey::parse_hex(&self.plan.signing_key)?;

        let lock = InstallLock::acquire(&paths)?;

        // Pinned, never trust-on-first-use. The key travelled inside the
        // artefact the publisher built, so the package is held to *their* key
        // rather than to whichever key happened to sign the first download.
        // The same key is pinned into the installation, so every later update
        // is held to it too.
        let mut verified = open_and_verify(&self.package, &lock, &TrustDecision::Explicit(key))?;

        let options = InstallOptions {
            activate: self.plan.activate,
            allow_downgrade: false,
            launcher: self.binary("xpack-launcher"),
            gui_launcher: self.binary("xpack-launcherw"),
            updater: self.binary("xpack-updater"),
            uninstaller: self.binary("xpack-uninstaller"),
            desktop_roots: None,
        };

        let installed = Installer::new(&lock).install(&mut verified, &options)?;

        // Read after the install, because that is when an installation is
        // given the names its executables carry. Reporting the path this
        // binary was built expecting would print one the user cannot run.
        let names = lock
            .load_or_new_state(&self.plan.application_id)
            .map_or(xpack_core::BinaryNames::Xpack, |state| state.binary_names());

        Ok(Outcome {
            root: paths.root().to_path_buf(),
            version: installed.version,
            activated: installed.activated,
            launcher: paths.shortcut_target_named(&names),
            desktop: installed.desktop,
        })
    }

    /// Finds a runtime binary in the payload by its stem.
    ///
    /// Matched on the file stem so one lookup works whether or not the name
    /// carries `.exe`.
    fn binary(&self, stem: &str) -> Option<PathBuf> {
        self.binaries
            .iter()
            .find(|path| path.file_stem().is_some_and(|found| found == stem))
            .cloned()
    }
}

/// Joins an archive entry name onto a directory, refusing anything that escapes.
///
/// The payload archive carries no signature of its own — the signature that
/// matters is on the package *inside* it — so a tampered installer could name
/// an entry `../../../.bashrc`. Unpacking happens before any verification can,
/// which is precisely why the name has to be checked here.
fn safe_join(root: &Path, name: &str) -> Result<PathBuf> {
    let normalised = name.replace('\\', "/");
    if normalised.is_empty() || normalised.starts_with('/') {
        return Err(Error::invalid(
            "installer payload",
            format!("{name:?} is not a relative path"),
        ));
    }

    let mut joined = root.to_path_buf();
    for component in normalised.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(Error::invalid(
                "installer payload",
                format!("{name:?} contains a traversing path component"),
            ));
        }
        // A drive-qualified component resolves outside the target on Windows.
        if component.len() >= 2 && component.as_bytes()[1] == b':' {
            return Err(Error::invalid(
                "installer payload",
                format!("{name:?} is drive-qualified"),
            ));
        }
        joined.push(component);
    }
    Ok(joined)
}

/// Marks an unpacked runtime binary executable.
///
/// The archive's own mode bits are not consulted: this file is about to be
/// copied into an installation by the installer, which sets the final
/// permissions itself. This only has to make it runnable here.
#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| Error::io(path, e))
}

// Mirrors the Unix version's signature, which genuinely can fail.
#[allow(clippy::unnecessary_wraps)]
#[cfg(not(unix))]
fn make_executable(path: &Path) -> Result<()> {
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_entry_name_joins_under_the_root() {
        let root = Path::new("/tmp/unpack");
        assert_eq!(safe_join(root, "bin/xpack-launcher").unwrap(), root.join("bin/xpack-launcher"));
        assert_eq!(safe_join(root, PLAN_ENTRY).unwrap(), root.join(PLAN_ENTRY));
    }

    #[test]
    fn a_traversing_entry_name_is_refused() {
        // The payload archive carries no signature of its own, and unpacking
        // happens before anything can be verified.
        let root = Path::new("/tmp/unpack");
        for name in ["../escape", "bin/../../escape", "a/./b", "..", "bin//x"] {
            assert!(safe_join(root, name).is_err(), "{name:?} was accepted");
        }
    }

    #[test]
    fn an_absolute_entry_name_is_refused() {
        let root = Path::new("/tmp/unpack");
        assert!(safe_join(root, "/etc/passwd").is_err());
        assert!(safe_join(root, "").is_err());
    }

    #[test]
    fn a_backslash_separated_name_is_refused_when_it_traverses() {
        // Windows separators are normalised first, so `..\..\x` cannot slip
        // past a check that only looked for forward slashes.
        let root = Path::new("/tmp/unpack");
        assert!(safe_join(root, r"..\..\escape").is_err());
        assert_eq!(safe_join(root, r"bin\app").unwrap(), root.join("bin/app"));
    }

    #[test]
    fn a_drive_qualified_component_is_refused() {
        assert!(safe_join(Path::new("/tmp/unpack"), "C:evil").is_err());
    }
}
