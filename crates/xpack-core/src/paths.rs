//! On-disk layout of an installation.
//!
//! The default root is **per-user**, not system-wide. That is a deliberate
//! security and reliability choice: a per-user install needs no UAC elevation
//! on Windows, no `sudo` on Unix, and no `SeCreateSymbolicLinkPrivilege` — so
//! the update path never has to prompt, and an unprivileged attacker gains
//! nothing by racing a privileged installer.
//!
//! ```text
//! <root>/<application-id>/
//! ├── config/trust.json     pinned signing keys for this installation
//! ├── versions/<version>/   extracted payloads, one directory per version
//! ├── state/state.json      authoritative active version + update phase
//! ├── state/update.lock     cross-process installation lock
//! ├── state/logs/           component logs
//! ├── downloads/            partial downloads, never trusted
//! └── staging/              half-extracted versions, deleted on recovery
//! ```

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::manifest::validate_application_id;
use crate::version::Version;

/// Resolved paths for a single installed application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPaths {
    root: PathBuf,
}

impl InstallPaths {
    /// Builds the layout for `application_id` under an explicit root.
    ///
    /// The id is re-validated here even though the manifest already checked
    /// it, because this constructor is also reachable from CLI arguments.
    pub fn new(install_root: &Path, application_id: &str) -> Result<Self> {
        validate_application_id(application_id)?;
        Ok(Self { root: install_root.join(application_id) })
    }

    /// Builds the layout under the platform's default per-user data directory.
    pub fn user_default(application_id: &str) -> Result<Self> {
        Self::new(&default_install_root()?, application_id)
    }

    /// Wraps an already-resolved application directory.
    pub fn from_application_dir(dir: impl Into<PathBuf>) -> Self {
        Self { root: dir.into() }
    }

    /// The application's root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory holding every installed version.
    pub fn versions_dir(&self) -> PathBuf {
        self.root.join("versions")
    }

    /// Directory holding one specific version's payload.
    pub fn version_dir(&self, version: &Version) -> PathBuf {
        self.versions_dir().join(version.to_directory_name())
    }

    /// Directory holding mutable installation state.
    pub fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }

    /// The authoritative state document.
    pub fn state_file(&self) -> PathBuf {
        self.state_dir().join("state.json")
    }

    /// The cross-process installation lock file.
    pub fn lock_file(&self) -> PathBuf {
        self.state_dir().join("update.lock")
    }

    /// Directory for component logs.
    pub fn logs_dir(&self) -> PathBuf {
        self.state_dir().join("logs")
    }

    /// Directory for installation configuration.
    pub fn config_dir(&self) -> PathBuf {
        self.root.join("config")
    }

    /// The pinned signing-key store.
    pub fn trust_file(&self) -> PathBuf {
        self.config_dir().join("trust.json")
    }

    /// Directory for in-progress downloads. Contents are never trusted.
    pub fn downloads_dir(&self) -> PathBuf {
        self.root.join("downloads")
    }

    /// Staging path for a version being extracted.
    ///
    /// Extraction happens here and the directory is renamed into place only
    /// once every hash has been verified, so a crash can never leave a
    /// half-written tree under `versions/` that looks complete.
    pub fn staging_dir(&self, version: &Version) -> PathBuf {
        self.root.join("staging").join(version.to_directory_name())
    }

    /// Parent of all staging directories.
    pub fn staging_root(&self) -> PathBuf {
        self.root.join("staging")
    }

    /// The derived `current` symlink/junction.
    ///
    /// Convenience only — shortcuts and external scripts can point at a stable
    /// path. [`crate::state::InstallState`] remains the authority; see that
    /// module for why.
    pub fn current_link(&self) -> PathBuf {
        self.root.join("current")
    }

    /// Returns `true` when this looks like an initialised installation.
    pub fn is_installed(&self) -> bool {
        self.state_file().is_file()
    }
}

/// The default per-user root that holds every xPack application directory.
///
/// Honours `XPACK_INSTALL_ROOT`, which the test suite and system integrators
/// use to relocate installations without touching the user's real data.
pub fn default_install_root() -> Result<PathBuf> {
    if let Some(overridden) = std::env::var_os("XPACK_INSTALL_ROOT") {
        let path = PathBuf::from(overridden);
        if path.as_os_str().is_empty() {
            return Err(Error::invalid("XPACK_INSTALL_ROOT", "must not be empty"));
        }
        return Ok(path);
    }

    let dirs = directories::BaseDirs::new().ok_or_else(|| {
        Error::Unsupported("locating a home directory for the current user".to_string())
    })?;
    Ok(dirs.data_local_dir().join("xpack"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> InstallPaths {
        InstallPaths::new(Path::new("/opt/xpack"), "com.example.app").unwrap()
    }

    #[test]
    fn nests_every_path_under_the_application_directory() {
        let p = paths();
        assert!(p.root().ends_with("com.example.app"));
        for path in [p.state_file(), p.trust_file(), p.downloads_dir(), p.lock_file()] {
            assert!(path.starts_with(p.root()), "{} escaped the root", path.display());
        }
    }

    #[test]
    fn refuses_to_build_paths_from_a_traversing_application_id() {
        assert!(InstallPaths::new(Path::new("/opt/xpack"), "../../etc").is_err());
        assert!(InstallPaths::new(Path::new("/opt/xpack"), "/absolute").is_err());
    }

    #[test]
    fn separates_staging_from_installed_versions() {
        let p = paths();
        let v = Version::parse("1.2.0").unwrap();
        assert_ne!(p.staging_dir(&v), p.version_dir(&v));
        assert!(!p.staging_dir(&v).starts_with(p.versions_dir()));
    }

    #[test]
    fn version_directories_are_named_after_the_version() {
        let p = paths();
        assert!(p.version_dir(&Version::parse("1.2.0-rc.1").unwrap()).ends_with("1.2.0-rc.1"));
    }
}
