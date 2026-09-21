//! The background updater.
//!
//! This is the piece that makes updates happen on a user's machine without
//! anyone asking. The launcher starts it, detached, every time the application
//! opens; it decides whether a check is due, and if one is, fetches and stages
//! a new version.
//!
//! # It stages; it does not activate
//!
//! A staged version is complete, verified and sitting in `versions/`, with
//! nothing about the running application changed. The launcher activates it at
//! the next start.
//!
//! Activating here instead would be one step shorter and wrong for two
//! reasons. Activation puts a version on probation, and probation is only
//! meaningful while something watches the version start — which is the
//! launcher's job and cannot be done from a process that exits immediately.
//! The attempt budget is small and bounded, so unobserved activations spend it
//! and can roll back a version nobody ever tried. Separately, the `current`
//! link is documented as a stable path for shortcuts and scripts; moving it
//! under a running application is exactly the breakage it exists to avoid.
//!
//! This is also what Chrome, Firefox and Windows do, for the same reasons.
//!
//! # It is polite about asking
//!
//! Running on every application start means a user who opens their application
//! twenty times a day would otherwise make twenty requests. The last check is
//! recorded in installation state and a new one is refused until the interval
//! has passed.
//!
//! The interval is the publisher's to choose, from the signed manifest, and
//! four hours only when they said nothing. It governs every check, including
//! this one: an installation that never looks while it runs still asks its
//! server no more often than the interval allows. The launcher paces its own
//! checks by the same value, so the two cannot disagree about when a check is
//! due.
//!
//! # It is quiet
//!
//! Nobody is watching. There is no terminal, no progress bar and no prompt:
//! everything goes to the installation's log file, and the exit code carries
//! the outcome for anything that does care.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use xpack_core::progress::{NoProgress, ProgressEvent, ProgressReporter};
use xpack_core::{Error, InstallPaths, Result, UpdateSpec, Version};
use xpack_platform::InstallLock;
use xpack_update::{UpdateOptions, UpdateTransport, Updater};

/// How long to wait between asking the server, when nothing says otherwise.
///
/// The same number the manifest falls back to, named here as well because this
/// is what applies when there is no manifest to read at all — an installation
/// with no active version, or one whose manifest cannot be read.
///
/// A publisher who wants a different rate says so in the signed manifest, and
/// a caller who wants one for a single run passes it to
/// [`BackgroundUpdater::every`].
pub const DEFAULT_INTERVAL: Duration =
    Duration::from_secs(xpack_core::manifest::DEFAULT_CHECK_INTERVAL_MINUTES as u64 * 60);

/// What a run of the updater did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The interval has not elapsed; the server was not contacted.
    NotDue,
    /// The server was asked and had nothing newer.
    UpToDate,
    /// A version was downloaded, verified and staged.
    Staged(Version),
    /// A check was made but the installation declares no update server.
    NoServerConfigured,
}

/// Decides whether to check, and checks.
pub struct BackgroundUpdater<'a> {
    paths: &'a InstallPaths,
    transport: &'a dyn UpdateTransport,
    progress: &'a dyn ProgressReporter,
    interval: Option<Duration>,
    force: bool,
}

impl<'a> BackgroundUpdater<'a> {
    /// Builds an updater for an installation.
    pub fn new(paths: &'a InstallPaths, transport: &'a dyn UpdateTransport) -> Self {
        Self { paths, transport, progress: &NoProgress, interval: None, force: false }
    }

    /// Reports each stage to `progress` as it happens.
    ///
    /// How a desktop application shows an update it did not start: spawn this
    /// binary with `--progress json`, read the stream, render in its own
    /// toolkit.
    #[must_use]
    pub fn reporting_to(mut self, progress: &'a dyn ProgressReporter) -> Self {
        self.progress = progress;
        self
    }

    /// Sets the minimum time between checks, overriding the manifest.
    ///
    /// For a caller that has its own reason to choose — a test, or an operator
    /// running the binary by hand. Left alone, the interval comes from the
    /// publisher's signed manifest and falls back to [`DEFAULT_INTERVAL`].
    #[must_use]
    pub fn every(mut self, interval: Duration) -> Self {
        self.interval = Some(interval);
        self
    }

    /// Checks regardless of when the last one happened.
    #[must_use]
    pub fn forced(mut self, force: bool) -> Self {
        self.force = force;
        self
    }

    /// Runs one update cycle.
    pub fn run(&self) -> Result<Outcome> {
        let application = self.application_id()?;
        let now = unix_seconds();

        // The due check and the record of having checked are written in one
        // locked window, before the network is touched. Two updaters starting
        // at once would otherwise both find a check due and both make one.
        let url = {
            let lock = InstallLock::acquire(self.paths)?;
            let mut state = lock.load_or_new_state(&application)?;

            // Read before the due check, because the interval it decides with
            // is one of the things the manifest declares. A local file read,
            // under a lock this window holds anyway.
            let spec = self.update_spec(&state)?;
            let interval = self.interval_for(spec.as_ref());

            if !self.force && !state.update_check_is_due(now, interval.as_secs()) {
                tracing::debug!(
                    interval_seconds = interval.as_secs(),
                    "an update check is not due yet"
                );
                return Ok(Outcome::NotDue);
            }

            self.progress
                .report(&ProgressEvent::CheckingForUpdate { application: application.clone() });

            let Some(url) = spec.and_then(|spec| spec.url) else {
                return Ok(Outcome::NoServerConfigured);
            };

            // Recorded before the attempt, not after. A server that hangs until
            // the transport gives up would otherwise leave the check unrecorded
            // and be retried on the very next application start.
            state.last_update_check = Some(now);
            lock.save_state(&state)?;
            url
        };

        // Staged, never activated. See the module documentation.
        // No launcher or updater is supplied: this runs *inside* an
        // installation that already has both, and replacing the binary this
        // process is executing from is precisely the write Windows refuses.
        let options = UpdateOptions {
            allow_downgrade: false,
            activate: false,
            launcher: None,
            gui_launcher: None,
            updater: None,
            uninstaller: None,
            notifier: None,
            // Nobody is watching this run, which is exactly the case a staged
            // rollout exists to hold back.
            respect_rollout: true,
        };

        let result = Updater::new(self.paths, self.transport)
            .reporting_to(self.progress)
            .update(&url, &options);

        match result {
            Ok(Some(version)) => {
                tracing::info!(%version, "a new version is staged and will be used at next start");
                Ok(Outcome::Staged(version))
            }
            Ok(None) => {
                tracing::debug!("no newer version is available");
                Ok(Outcome::UpToDate)
            }
            // Reported before returning, so a consumer watching the stream
            // learns why it stopped rather than seeing it simply end.
            Err(error) => {
                self.progress.report(&ProgressEvent::Failed { reason: error.to_string() });
                Err(error)
            }
        }
    }

    /// What the active version's signed manifest says about updating.
    ///
    /// Taken from the manifest rather than from a caller, so the server and
    /// the rate are whatever the publisher signed rather than whatever a
    /// process on the machine happened to pass in.
    ///
    /// `None` when there is no active version or its manifest cannot be read.
    /// An installation in that state has nothing to check against, and every
    /// caller here treats it as "not configured" rather than as a failure.
    fn update_spec(&self, state: &xpack_core::InstallState) -> Result<Option<UpdateSpec>> {
        let Some(version) = &state.current_version else {
            return Ok(None);
        };
        let manifest_file = self.paths.version_manifest_file(version);
        let Ok(bytes) = std::fs::read(&manifest_file) else {
            return Ok(None);
        };
        Ok(Some(xpack_core::Manifest::from_slice(&bytes)?.update))
    }

    /// The interval in force: the caller's, then the publisher's, then ours.
    ///
    /// The publisher's value has to reach this gate and not only the launcher
    /// that spawns the process. A manifest asking for a check every thirty
    /// minutes, against a gate still holding out for four hours, would spawn
    /// this binary every thirty minutes to be told each time that nothing is
    /// due yet.
    ///
    /// It applies to this check whether or not the installation also checks
    /// while it runs: the interval says how often the server may be asked, and
    /// a startup check is asking.
    fn interval_for(&self, spec: Option<&UpdateSpec>) -> Duration {
        self.interval.or_else(|| spec.map(UpdateSpec::check_interval)).unwrap_or(DEFAULT_INTERVAL)
    }

    fn application_id(&self) -> Result<String> {
        self.paths
            .application_id()
            .map(ToString::to_string)
            .ok_or_else(|| Error::invalid("installation", "root has no application id"))
    }
}

/// Seconds since the Unix epoch, saturating at zero before it.
///
/// A clock set before 1970 is a broken clock, not a negative instant, and the
/// only consumer of this is an interval comparison that treats zero as "very
/// long ago" — which is the right reading of a clock that cannot be trusted.
pub fn unix_seconds() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
