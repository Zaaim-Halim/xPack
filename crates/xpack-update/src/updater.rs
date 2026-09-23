//! The update engine.
//!
//! # Locking
//!
//! The installation lock is **released across the download** and held only for
//! the state writes and the install. A download can take half an hour, and
//! holding the lock throughout would mean a user could not start their own
//! application while it ran — the launcher would fail with "another operation
//! is already running" because an update was quietly fetching in the
//! background.
//!
//! Releasing it costs two things, and both are paid rather than assumed away.
//!
//! **The decision goes stale.** What to install is decided before the download
//! and acted on after it, with minutes in between during which another process
//! may have installed that version, rolled back to a newer one, or cleared the
//! phase. So the decision is made twice: once to start, and again under the
//! lock before anything is installed. A mismatch discards the download rather
//! than installing against state that is no longer true.
//!
//! **Recovery cannot tell a live download from abandoned debris.** Its rule
//! for the `Downloading` phase is to clear the downloads directory, which
//! would delete the file a running download is writing. A
//! [`xpack_platform::DownloadLease`] answers whether anyone is
//! still there, and the operating system answers it: the lease is an advisory
//! file lock, released the instant the holding process ends. The lease is
//! taken before the installation lock is released and dropped after it is
//! re-acquired, so there is no window in which the phase says `Downloading`,
//! the lease is free, and a download is nonetheless in progress.
//!
//! A caller must not hold the installation lock when calling into this
//! module. The lock is not reentrant, so it would deadlock against itself.

use std::io::Write;

use xpack_core::atomic;
use xpack_core::progress::{NoProgress, ProgressEvent, ProgressReporter};
use xpack_core::state::UpdatePhase;
use xpack_core::{Error, InstallPaths, Platform, Result, Version};
use xpack_install::{InstallOptions, Installer, TrustDecision, open_and_verify};
use xpack_platform::{DownloadLease, InstallLock};

use crate::index::{MAX_INDEX_BYTES, UpdateIndex};
use crate::reporting::ProgressWriter;
use crate::rollout;
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
    /// Uninstaller to place in the installation root, if it has none.
    pub uninstaller: Option<std::path::PathBuf>,
    /// Background updater to place in the installation root, if it has none.
    pub updater: Option<std::path::PathBuf>,
    /// Update notifier to place in the installation root, if it has none.
    ///
    /// Carried for the same reason as the launcher: an update that turns out
    /// to be a first install must not produce an installation missing a binary
    /// every other install would have placed. The installer still decides
    /// whether the package wants one.
    ///
    /// A background update passes nothing here, because it runs from inside an
    /// installation and has no copy of the binary to place.
    pub notifier: Option<std::path::PathBuf>,
    /// Launcher binary to place in the installation root, if it has none.
    ///
    /// An update normally finds one already there and leaves it alone. This
    /// matters for the case where an update is also the first install, so the
    /// resulting installation is not left without an entry point.
    pub launcher: Option<std::path::PathBuf>,
    /// Windowed launcher to place beside the console one, if it has none.
    ///
    /// Only meaningful on Windows. Carried for the same reason as the
    /// launcher: an update that turns out to be a first install must not
    /// produce an installation missing a binary every later install would
    /// have placed.
    pub gui_launcher: Option<std::path::PathBuf>,

    /// Whether a staged rollout may hold this installation back.
    ///
    /// An unattended check must respect it — that is the whole point of a
    /// staged rollout. A person who typed "check for updates" must not: they
    /// asked for the newest version, and answering "there is one, but not for
    /// you" would be both unhelpful and impossible to explain.
    ///
    /// Sparkle draws the line in the same place, for the same reason.
    pub respect_rollout: bool,
}

/// Which artefact an attempt fetches.
///
/// A delta and a full package are the same operation — download a file,
/// verify it against the publisher's signature, install it — differing only in
/// what turns the file into a version directory. Modelling the difference as a
/// value rather than a second code path is what keeps the download rules, the
/// lock windows and the phase writes identical for both.
#[derive(Debug, Clone, Copy)]
enum Fetch<'a> {
    /// The whole package.
    Full,
    /// A delta, rebuilt against the version already installed.
    Delta(&'a crate::index::DeltaRef),
}

impl Fetch<'_> {
    /// The file to request, relative to the index.
    fn file<'i>(self, index: &'i UpdateIndex) -> &'i str
    where
        Self: 'i,
    {
        match self {
            Self::Full => &index.package.file,
            Self::Delta(delta) => &delta.file,
        }
    }

    /// The size the index claims, which bounds the download.
    fn size(self, index: &UpdateIndex) -> u64 {
        match self {
            Self::Full => index.package.size,
            Self::Delta(delta) => delta.size,
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Self::Full => "package",
            Self::Delta(_) => "delta",
        }
    }
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
///
/// Takes the installation's paths rather than a held lock, because it acquires
/// the lock in short windows of its own. See the module documentation for why.
pub struct Updater<'a> {
    paths: &'a InstallPaths,
    transport: &'a dyn UpdateTransport,
    progress: &'a dyn ProgressReporter,
}

impl<'a> Updater<'a> {
    /// Wraps an installation and a transport, reporting nothing.
    ///
    /// The caller must **not** hold the installation lock. This type acquires
    /// it itself, and the lock is not reentrant, so a caller holding one would
    /// deadlock against itself on the first window.
    pub fn new(paths: &'a InstallPaths, transport: &'a dyn UpdateTransport) -> Self {
        Self { paths, transport, progress: &NoProgress }
    }

    /// Reports progress to `progress` as the operation runs.
    ///
    /// An update takes minutes, and nothing watching it can poll a function
    /// that has not returned. A splash screen, a status line or a progress bar
    /// subscribes here.
    #[must_use]
    pub fn reporting_to(mut self, progress: &'a dyn ProgressReporter) -> Self {
        self.progress = progress;
        self
    }

    /// Asks the server what it has, without downloading a package.
    ///
    /// Returns `Ok(None)` when the installation is already current.
    pub fn check(&self, base_url: &str, options: &UpdateOptions) -> Result<Option<Available>> {
        let application = self.application_id()?;
        self.progress
            .report(&ProgressEvent::CheckingForUpdate { application: application.clone() });

        // The lock is held only to read. Fetching the index over the network
        // while holding it would block every other command for as long as a
        // server chose to be slow.
        let state = {
            let lock = self.lock()?;
            lock.load_or_new_state(&application)?
        };
        let channel = self.channel(&state)?;
        let index = self.fetch_index(base_url, &application, &channel)?;

        // Checked before anything is downloaded: the cheapest possible
        // rejection, and it stops a hostile index spending a user's bandwidth.
        if let Err(e) = state.ensure_not_downgrade(&index.version, options.allow_downgrade) {
            tracing::debug!(offered = %index.version, "server offered nothing newer");
            return match e {
                Error::DowngradeRejected { .. } => {
                    if let Some(current) = &state.current_version {
                        self.progress.report(&ProgressEvent::UpToDate { version: current.clone() });
                    }
                    Ok(None)
                }
                other => Err(other),
            };
        }

        if self.held_back_by_rollout(&application, &index, options)? {
            if let Some(current) = &state.current_version {
                self.progress.report(&ProgressEvent::UpToDate { version: current.clone() });
            }
            return Ok(None);
        }

        self.progress.report(&ProgressEvent::UpdateAvailable {
            version: index.version.clone(),
            total_bytes: declared_size(index.package.size),
        });

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
    ///
    /// # Lock windows
    ///
    /// Three short ones, with the download outside all of them:
    ///
    /// 1. recover, read state, learn the channel
    /// 2. re-check against fresh state, take the download lease, mark the phase
    /// 3. re-validate, verify, install
    ///
    /// Window 2 decides against state that may be minutes old by the time
    /// window 3 runs, so window 3 checks again rather than trusting it.
    pub fn update(&self, base_url: &str, options: &UpdateOptions) -> Result<Option<Version>> {
        let application = self.application_id()?;

        // --- Window 1 -----------------------------------------------------
        let state = {
            let lock = self.lock()?;
            Installer::new(&lock).recover()?;
            lock.load_or_new_state(&application)?
        };
        let channel = self.channel(&state)?;

        // --- Unlocked: the index ------------------------------------------
        let index = self.fetch_index(base_url, &application, &channel)?;
        if state.ensure_not_downgrade(&index.version, options.allow_downgrade).is_err() {
            return Ok(None);
        }

        // Before the download, not after: an installation outside the rollout
        // must not spend the user's bandwidth on a package it will not install.
        if self.held_back_by_rollout(&application, &index, options)? {
            return Ok(None);
        }

        // A delta only exists for the version actually installed, and state
        // is what says which that is. Asking the index first would let
        // whoever serves it choose which directory gets read.
        let installed = state.current_version.clone();
        let chosen = installed.as_ref().and_then(|version| index.delta_from(version));

        if let Some(delta) = chosen {
            match self.attempt(&index, base_url, options, Fetch::Delta(delta)) {
                Ok(outcome) => return Ok(outcome),
                Err(error) => {
                    // Never fatal. A delta is an optimisation: a pruned base, a
                    // file that no longer hashes, a malformed archive or a
                    // server serving nonsense all end the same way — fetch the
                    // whole package, which needs nothing from this machine.
                    tracing::warn!(
                        %error,
                        from = %delta.from,
                        to = %index.version,
                        "the delta could not be used; falling back to the full package"
                    );
                }
            }
        }

        self.attempt(&index, base_url, options, Fetch::Full)
    }

    /// One download-and-install attempt, for a delta or the full package.
    ///
    /// Self-contained on purpose: it takes its own download lease and leaves
    /// the phase clear however it ends, so a failed delta attempt is followed
    /// by a full one starting from exactly the state it would have found if
    /// the delta had never been offered.
    fn attempt(
        &self,
        index: &UpdateIndex,
        base_url: &str,
        options: &UpdateOptions,
        fetch: Fetch<'_>,
    ) -> Result<Option<Version>> {
        let application = self.application_id()?;

        // --- Window 2 -----------------------------------------------------
        // The lease is taken here, before this window closes, so that recovery
        // in another process can never see `Downloading` with a free lease
        // while this download is genuinely running.
        let lease = {
            let lock = self.lock()?;
            let mut state = lock.load_or_new_state(&application)?;
            let installer = Installer::new(&lock);

            if state.ensure_not_downgrade(&index.version, options.allow_downgrade).is_err() {
                return Ok(None);
            }
            if state.record(&index.version).is_some() && installer.is_usable(&index.version) {
                tracing::debug!(version = %index.version, "offered version is already installed");
                return Ok(None);
            }

            let Some(lease) = DownloadLease::acquire(self.paths)? else {
                return Err(Error::Locked(format!(
                    "{} (a download is already in progress)",
                    self.paths.download_lock_file().display()
                )));
            };

            state.update = UpdatePhase::Downloading { version: index.version.clone() };
            lock.save_state(&state)?;
            lease
        };

        // --- Unlocked: the download ---------------------------------------
        let downloaded = match self.download(index, base_url, fetch) {
            Ok(path) => path,
            Err(e) => {
                // The lease is still held, so this reset cannot race a
                // recovery that would otherwise clear the directory first.
                self.reset_phase_taking_the_lock()?;
                return Err(e);
            }
        };

        // --- Window 3 -----------------------------------------------------
        let lock = self.lock()?;
        let result = self.finish(&lock, index, &downloaded, options, fetch);

        // Released only after the installation lock is held again, closing the
        // window in the other direction.
        drop(lease);
        result
    }

    /// Verifies and installs a package that has finished downloading.
    ///
    /// Runs under the installation lock, with the download lease still held.
    fn finish(
        &self,
        lock: &InstallLock,
        index: &UpdateIndex,
        downloaded: &std::path::Path,
        options: &UpdateOptions,
        fetch: Fetch<'_>,
    ) -> Result<Option<Version>> {
        let application = self.application_id()?;
        let mut state = lock.load_or_new_state(&application)?;
        let installer = Installer::new(lock);

        // The decision to download was made before the download started, and a
        // download takes minutes. Anything could have happened to the
        // installation since: another process installed this version, rolled
        // back to a newer one, or cleared the phase. Re-checking is what makes
        // releasing the lock safe.
        match &state.update {
            UpdatePhase::Downloading { version } if version == &index.version => {}
            other => {
                let _ = atomic::remove_file_if_exists(downloaded);
                return Err(Error::invalid(
                    "update",
                    format!(
                        "the installation changed while {} was downloading (now {}); \
                         discarding it rather than installing against stale state",
                        index.version,
                        other.describe()
                    ),
                ));
            }
        }
        if state.ensure_not_downgrade(&index.version, options.allow_downgrade).is_err() {
            let _ = atomic::remove_file_if_exists(downloaded);
            self.clear_phase(lock)?;
            return Ok(None);
        }
        if state.record(&index.version).is_some() && installer.is_usable(&index.version) {
            let _ = atomic::remove_file_if_exists(downloaded);
            self.clear_phase(lock)?;
            return Ok(None);
        }

        // Verification is the same code path a local install uses. A second
        // implementation of this check would eventually disagree with the
        // first, and the weaker one would be the bug.
        state.update = UpdatePhase::Verifying { version: index.version.clone() };
        lock.save_state(&state)?;
        self.progress.report(&ProgressEvent::Verifying { version: index.version.clone() });

        // A delta and a package verify against the same pinned keys and the
        // same signature over the same manifest; only the shape of what was
        // downloaded differs.
        let mut delta = None;
        let mut package = None;
        let produced = match fetch {
            Fetch::Full => match open_and_verify(downloaded, lock, &TrustDecision::UsePinned) {
                Ok(p) => {
                    let version = p.manifest().application.version.clone();
                    package = Some(p);
                    version
                }
                Err(e) => {
                    // A package that fails verification is not retried; it is
                    // destroyed. Leaving it on disk invites a later code path
                    // to find it and treat it as trustworthy.
                    let _ = atomic::remove_file_if_exists(downloaded);
                    self.clear_phase(lock)?;
                    return Err(e);
                }
            },
            Fetch::Delta(_) => match xpack_install::open_and_verify_delta(downloaded, lock) {
                Ok(d) => {
                    let version = d.manifest().application.version.clone();
                    delta = Some(d);
                    version
                }
                Err(e) => {
                    let _ = atomic::remove_file_if_exists(downloaded);
                    self.clear_phase(lock)?;
                    return Err(e);
                }
            },
        };

        // The index said one thing; the signed manifest says what is true.
        // Checked for a delta too: the manifest it carries is the target's, so
        // a delta claiming to produce a version it does not is caught here
        // rather than after assembling the wrong tree.
        if produced != index.version {
            let _ = atomic::remove_file_if_exists(downloaded);
            self.clear_phase(lock)?;
            return Err(Error::Integrity(format!(
                "the index offered {} but the signed {} contains {produced}",
                index.version,
                fetch.describe()
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
        self.clear_phase(lock)?;

        let install_options = InstallOptions {
            allow_downgrade: options.allow_downgrade,
            activate: options.activate,
            launcher: options.launcher.clone(),
            gui_launcher: options.gui_launcher.clone(),
            updater: options.updater.clone(),
            uninstaller: options.uninstaller.clone(),
            notifier: options.notifier.clone(),
            // The user's real directories. An update is a real installation,
            // not a test, and the entry it refreshes is the one in their menu.
            desktop_roots: None,
            // An update never asks. The choice made at the first install, if
            // any, is recorded and honoured by the installer.
            desktop_entry: None,
            // The same for the command: never asked, a recorded decline
            // honoured, and the user's real `~/.local/bin` and `PATH`.
            command: None,
            command_roots: None,
        };
        self.progress.report(&ProgressEvent::Installing { version: index.version.clone() });

        let outcome = if let Some(delta) = delta.as_mut() {
            // The base is the version state says is installed, and the delta's
            // own claim is checked against it inside `DeltaSource`. Reading the
            // base from the delta instead would let the server pick the
            // directory that gets rebuilt from.
            let base_version = state.current_version.clone().ok_or_else(|| {
                Error::invalid("update", "a delta cannot apply: nothing is installed")
            })?;
            let base_dir = self.paths.version_dir(&base_version);
            let mut source = xpack_install::DeltaSource::new(delta, &base_version, base_dir)?;
            installer.install_from_with_progress(&mut source, &install_options, self.progress)?
        } else {
            let package = package.as_mut().expect("one of the two was verified above");
            installer.install_from_with_progress(package, &install_options, self.progress)?
        };

        atomic::remove_file_if_exists(downloaded)?;
        tracing::info!(version = %outcome.version, "update installed");
        self.progress.report(&ProgressEvent::Completed { version: outcome.version.clone() });
        Ok(Some(outcome.version))
    }

    /// Returns `true` when a staged rollout excludes this installation.
    ///
    /// Takes the lock briefly, because the first call on an installation
    /// generates its rollout identifier and has to persist it. Every later
    /// call reads the stored value and writes nothing.
    fn held_back_by_rollout(
        &self,
        application: &str,
        index: &UpdateIndex,
        options: &UpdateOptions,
    ) -> Result<bool> {
        if !options.respect_rollout {
            return Ok(false);
        }

        let percentage = rollout::declared_percentage(index.rollout);
        if percentage >= rollout::FULLY_ROLLED_OUT {
            // The overwhelmingly common case: a fully published release. No
            // identifier is needed, so an installation that only ever sees
            // ordinary releases never acquires one.
            return Ok(false);
        }

        let lock = self.lock()?;
        let mut state = lock.load_or_new_state(application)?;
        let (rollout_id, created) = state.rollout_id_or_create(rollout::new_rollout_id)?;
        if created {
            lock.save_state(&state)?;
        }
        drop(lock);

        let included = rollout::is_in_rollout(&rollout_id, &index.version, percentage);
        if !included {
            tracing::info!(
                version = %index.version,
                percentage,
                "a newer version exists but this installation is not yet in its staged rollout"
            );
        }
        Ok(!included)
    }

    /// Fetches and validates the index for a channel.
    ///
    /// Everything it returns is still untrusted: the index chooses what to
    /// download, and only the publisher's signature decides what is installed.
    fn fetch_index(&self, base_url: &str, application: &str, channel: &str) -> Result<UpdateIndex> {
        let url = index_url(base_url, channel);
        let bytes = self.transport.fetch_to_vec(&url, MAX_INDEX_BYTES)?;
        let index = UpdateIndex::from_slice(&bytes)?;
        index.ensure_matches(application, Platform::host()?, channel)?;
        Ok(index)
    }

    /// Downloads the package the index names, bounded while streaming.
    ///
    /// Writes no state: it runs with the installation lock released, so the
    /// phase it would write could not be trusted by the time it landed. The
    /// phase is set before this is called and resolved after it returns.
    fn download(
        &self,
        index: &UpdateIndex,
        base_url: &str,
        fetch: Fetch<'_>,
    ) -> Result<std::path::PathBuf> {
        let downloads = self.paths.downloads_dir();
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

        // A declared size of zero is either a broken publisher or a hostile
        // server buying itself the absolute ceiling to write with. Neither is
        // worth accepting: the index must say how big the package is.
        if fetch.size(index) == 0 {
            return Err(Error::Transport(format!(
                "the update index declares no size for the {}; refusing to download an \
                 unbounded response",
                fetch.describe()
            )));
        }
        let limit = fetch.size(index).min(MAX_PACKAGE_BYTES);
        let url = package_url(base_url, fetch.file(index));

        self.progress.report(&ProgressEvent::DownloadStarted {
            version: index.version.clone(),
            total_bytes: declared_size(fetch.size(index)),
        });

        let result = (|| -> Result<u64> {
            let file = std::fs::File::create(&target).map_err(|e| Error::io(&target, e))?;
            let mut writer = std::io::BufWriter::new(file);
            // Wrapping the sink rather than changing the transport's signature
            // keeps the seam the hostile-server tests are built on untouched.
            let mut reporting =
                ProgressWriter::new(&mut writer, self.progress, declared_size(index.package.size));
            let written = self.transport.fetch(&url, limit, &mut reporting)?;
            reporting.flush().map_err(|e| Error::io(&target, e))?;
            writer.flush().map_err(|e| Error::io(&target, e))?;
            let file = writer.into_inner().map_err(|e| Error::io(&target, e.into_error()))?;
            file.sync_all().map_err(|e| Error::io(&target, e))?;
            Ok(written)
        })();

        match result {
            Ok(written) => {
                tracing::info!(bytes = written, version = %index.version, "package downloaded");
                self.progress.report(&ProgressEvent::DownloadCompleted { bytes: written });
                Ok(target)
            }
            Err(e) => {
                // A truncated or oversized download leaves nothing behind.
                let _ = atomic::remove_file_if_exists(&target);
                Err(e)
            }
        }
    }

    /// Acquires the installation lock for one short window.
    fn lock(&self) -> Result<InstallLock> {
        InstallLock::acquire(self.paths)
    }

    /// Returns the installation to idle after a failed step, taking the lock.
    ///
    /// Only for callers that do **not** already hold it. The lock is not
    /// reentrant, so calling this from inside a locked window deadlocks the
    /// process against itself; those callers want [`Self::clear_phase`].
    fn reset_phase_taking_the_lock(&self) -> Result<()> {
        let lock = self.lock()?;
        self.clear_phase(&lock)
    }

    /// Returns the installation to idle under a lock the caller already holds.
    fn clear_phase(&self, lock: &InstallLock) -> Result<()> {
        let application = self.application_id()?;
        let mut state = lock.load_or_new_state(&application)?;
        state.update = UpdatePhase::Idle;
        lock.save_state(&state)
    }

    fn application_id(&self) -> Result<String> {
        self.paths
            .application_id()
            .map(ToString::to_string)
            .ok_or_else(|| Error::invalid("installation", "root has no application id"))
    }

    /// The channel this installation follows, from the active version's manifest.
    fn channel(&self, state: &xpack_core::InstallState) -> Result<String> {
        let Some(version) = &state.current_version else {
            return Ok("stable".to_string());
        };
        let manifest_file = self.paths.version_manifest_file(version);
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

/// A declared size, or `None` when the server gave nothing usable.
///
/// Zero means the server did not say. Passing it through as `Some(0)` would
/// make a consumer render a bar against a total of nothing.
fn declared_size(size: u64) -> Option<u64> {
    (size > 0).then_some(size)
}
