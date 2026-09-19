//! The update engine.
//!
//! # Locking
//!
//! The installation lock is held for the whole operation, including the
//! download. That blocks other xpack commands for the duration, which is the
//! right trade for a command a person deliberately invoked: it guarantees the
//! version being installed is decided against the state that is still true
//! when the install happens, and it uses the persisted update phases exactly
//! as recovery expects to find them.
//!
//! It is the wrong trade for a background updater that should download while
//! the application runs. Doing that needs the lock split — held for the state
//! writes and the install, released across the download — along with rules for
//! a partial file another process's recovery may delete underneath it. That is
//! deliberately not attempted here.

use std::io::Write;

use xpack_core::atomic;
use xpack_core::state::UpdatePhase;
use xpack_core::{Error, Platform, Result, Version};
use xpack_install::{InstallOptions, Installer, TrustDecision, open_and_verify};
use xpack_platform::InstallLock;

use crate::index::{MAX_INDEX_BYTES, UpdateIndex};
use crate::transport::UpdateTransport;

/// Absolute ceiling on a downloaded package, whatever the index claims.
///
/// The index's declared size bounds a well-behaved server. This bounds a
/// hostile one that declares something enormous.
pub const MAX_PACKAGE_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Choices a caller makes when updating.
#[derive(Debug, Clone, Default)]
pub struct UpdateOptions {
    /// Permit moving to a version that is not newer.
    pub allow_downgrade: bool,
    /// Activate the new version once installed.
    pub activate: bool,
}

/// An update the server offers and this installation would accept.
#[derive(Debug, Clone)]
pub struct Available {
    /// Version offered.
    pub version: Version,
    /// Version currently active, if any.
    pub current: Option<Version>,
    /// Size the index claims, for showing a user before committing to it.
    pub declared_size: u64,
    /// Release notes URL, for display only.
    pub release_notes: Option<String>,
}

/// Drives update checking and application.
pub struct Updater<'a> {
    lock: &'a InstallLock,
    transport: &'a dyn UpdateTransport,
}

impl<'a> Updater<'a> {
    /// Wraps a held lock and a transport.
    pub fn new(lock: &'a InstallLock, transport: &'a dyn UpdateTransport) -> Self {
        Self { lock, transport }
    }

    /// Asks the server what it has, without downloading a package.
    ///
    /// Returns `Ok(None)` when the installation is already current.
    pub fn check(&self, base_url: &str, options: &UpdateOptions) -> Result<Option<Available>> {
        let application = self.application_id()?;
        let state = self.lock.load_or_new_state(&application)?;
        let channel = self.channel(&state)?;

        let url = index_url(base_url, &channel);
        let bytes = self.transport.fetch_to_vec(&url, MAX_INDEX_BYTES)?;

        // Untrusted from here until a signature says otherwise.
        let index = UpdateIndex::from_slice(&bytes)?;
        index.ensure_matches(&application, Platform::host()?, &channel)?;

        // Checked before anything is downloaded: the cheapest possible
        // rejection, and it stops a hostile index spending a user's bandwidth.
        if let Err(e) = state.ensure_not_downgrade(&index.version, options.allow_downgrade) {
            tracing::debug!(offered = %index.version, "server offered nothing newer");
            return match e {
                Error::DowngradeRejected { .. } => Ok(None),
                other => Err(other),
            };
        }

        Ok(Some(Available {
            version: index.version.clone(),
            current: state.current_version.clone(),
            declared_size: index.package.size,
            release_notes: index.release_notes.clone(),
        }))
    }

    /// Checks, downloads, verifies and installs in one operation.
    ///
    /// Returns `Ok(None)` when there was nothing to do.
    pub fn update(&self, base_url: &str, options: &UpdateOptions) -> Result<Option<Version>> {
        let installer = Installer::new(self.lock);
        installer.recover()?;

        let application = self.application_id()?;
        let mut state = self.lock.load_or_new_state(&application)?;
        let channel = self.channel(&state)?;

        let url = index_url(base_url, &channel);
        let bytes = self.transport.fetch_to_vec(&url, MAX_INDEX_BYTES)?;
        let index = UpdateIndex::from_slice(&bytes)?;
        index.ensure_matches(&application, Platform::host()?, &channel)?;

        if state.ensure_not_downgrade(&index.version, options.allow_downgrade).is_err() {
            return Ok(None);
        }
        if state.record(&index.version).is_some() && installer.is_usable(&index.version) {
            tracing::debug!(version = %index.version, "offered version is already installed");
            return Ok(None);
        }

        let downloaded = self.download(&mut state, &index, base_url)?;

        // Verification is the same code path a local install uses. A second
        // implementation of this check would eventually disagree with the
        // first, and the weaker one would be the bug.
        state.update = UpdatePhase::Verifying { version: index.version.clone() };
        self.lock.save_state(&state)?;

        let verify = open_and_verify(&downloaded, self.lock, &TrustDecision::UsePinned);
        let mut verified = match verify {
            Ok(package) => package,
            Err(e) => {
                // A package that fails verification is not retried; it is
                // destroyed. Leaving it on disk invites a later code path to
                // find it and treat it as trustworthy.
                let _ = atomic::remove_file_if_exists(&downloaded);
                self.reset_phase()?;
                return Err(e);
            }
        };

        // The index said one thing; the signed manifest says what is true.
        if verified.manifest().application.version != index.version {
            let _ = atomic::remove_file_if_exists(&downloaded);
            self.reset_phase()?;
            return Err(Error::Integrity(format!(
                "the index offered {} but the signed package contains {}",
                index.version,
                verified.manifest().application.version
            )));
        }

        // The Verifying phase has done its job: the package is downloaded and
        // its signature checked. It must be cleared *before* the installer
        // runs, because the installer recovers first and recovery's rule for
        // Verifying is to clear the downloads directory — which still holds
        // the file this operation is reading from.
        //
        // On Unix an unlinked file stays readable through its open handle, so
        // leaving this in place would appear to work here and fail on Windows,
        // where deleting an open file is refused outright.
        self.reset_phase()?;

        let install_options =
            InstallOptions { allow_downgrade: options.allow_downgrade, activate: options.activate };
        let outcome = installer.install(&mut verified, &install_options)?;

        atomic::remove_file_if_exists(&downloaded)?;
        tracing::info!(version = %outcome.version, "update installed");
        Ok(Some(outcome.version))
    }

    /// Downloads the package the index names, bounded while streaming.
    fn download(
        &self,
        state: &mut xpack_core::InstallState,
        index: &UpdateIndex,
        base_url: &str,
    ) -> Result<std::path::PathBuf> {
        let paths = self.lock.paths();
        let downloads = paths.downloads_dir();
        atomic::create_dir_all(&downloads)?;

        // The local name is derived, never taken from the index: package.file
        // is attacker-controlled and would otherwise become a path inside the
        // installation.
        let target = downloads.join(index.local_file_name());
        // A partial file from an earlier attempt is discarded rather than
        // resumed. Resuming is safe because the result is hash-verified
        // regardless, but it needs rules for a partial whose target version has
        // since changed, and re-downloading is the honest simple answer.
        atomic::remove_file_if_exists(&target)?;

        state.update = UpdatePhase::Downloading { version: index.version.clone() };
        self.lock.save_state(state)?;

        // A declared size of zero is either a broken publisher or a hostile
        // server buying itself the absolute ceiling to write with. Neither is
        // worth accepting: the index must say how big the package is.
        if index.package.size == 0 {
            self.reset_phase()?;
            return Err(Error::Transport(
                "the update index declares no package size; refusing to download an unbounded \
                 response"
                    .to_string(),
            ));
        }
        let limit = index.package.size.min(MAX_PACKAGE_BYTES);
        let url = package_url(base_url, &index.package.file);

        let result = (|| -> Result<u64> {
            let file = std::fs::File::create(&target).map_err(|e| Error::io(&target, e))?;
            let mut writer = std::io::BufWriter::new(file);
            let written = self.transport.fetch(&url, limit, &mut writer)?;
            writer.flush().map_err(|e| Error::io(&target, e))?;
            let file = writer.into_inner().map_err(|e| Error::io(&target, e.into_error()))?;
            file.sync_all().map_err(|e| Error::io(&target, e))?;
            Ok(written)
        })();

        match result {
            Ok(written) => {
                tracing::info!(bytes = written, version = %index.version, "package downloaded");
                Ok(target)
            }
            Err(e) => {
                // A truncated or oversized download leaves nothing behind.
                let _ = atomic::remove_file_if_exists(&target);
                self.reset_phase()?;
                Err(e)
            }
        }
    }

    /// Returns the installation to idle after a failed step.
    fn reset_phase(&self) -> Result<()> {
        let application = self.application_id()?;
        let mut state = self.lock.load_or_new_state(&application)?;
        state.update = UpdatePhase::Idle;
        self.lock.save_state(&state)
    }

    fn application_id(&self) -> Result<String> {
        self.lock
            .paths()
            .application_id()
            .map(ToString::to_string)
            .ok_or_else(|| Error::invalid("installation", "root has no application id"))
    }

    /// The channel this installation follows, from the active version's manifest.
    fn channel(&self, state: &xpack_core::InstallState) -> Result<String> {
        let Some(version) = &state.current_version else {
            return Ok("stable".to_string());
        };
        let manifest_file = self.lock.paths().version_manifest_file(version);
        let Ok(bytes) = std::fs::read(&manifest_file) else {
            return Ok("stable".to_string());
        };
        Ok(xpack_core::Manifest::from_slice(&bytes)?.update.channel)
    }
}

/// Builds the index URL for a channel.
fn index_url(base_url: &str, channel: &str) -> String {
    format!("{}/{channel}.json", base_url.trim_end_matches('/'))
}

/// Builds the package URL from the index's file reference.
///
/// An absolute URL is used as given, so a CDN can serve packages from a
/// different host than the index; it is still required to be HTTPS by the
/// transport. Anything else is treated as a name relative to the base.
fn package_url(base_url: &str, file: &str) -> String {
    if file.starts_with("https://") || file.starts_with("http://") {
        return file.to_string();
    }
    format!("{}/{}", base_url.trim_end_matches('/'), file.trim_start_matches('/'))
}
