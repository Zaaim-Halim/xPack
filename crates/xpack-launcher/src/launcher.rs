//! Resolving the active version, starting it, and recording the result.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use xpack_core::{Error, InstallPaths, Manifest, Result, Version};
use xpack_install::Installer;
use xpack_platform::{InstallLock, LaunchRequest, launch};

/// How often a probationary process is checked for having exited.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// What happened to a probationary launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupResult {
    /// The process was still running when the startup window closed.
    SurvivedStartup,
    /// The process exited successfully inside the window.
    ExitedSuccessfully,
    /// The process exited with a failure inside the window.
    FailedToStart {
        /// Exit status, if the process had one.
        code: Option<i32>,
    },
}

impl StartupResult {
    /// Returns `true` when the version proved it starts.
    pub fn is_healthy(&self) -> bool {
        !matches!(self, Self::FailedToStart { .. })
    }
}

/// The result of a launch.
#[derive(Debug)]
pub struct Outcome {
    /// Version that was started.
    pub version: Version,
    /// How the probationary window resolved, when the version was on probation.
    pub startup: Option<StartupResult>,
    /// Version rolled back to, if the launch failed and a rollback happened.
    pub rolled_back_to: Option<Version>,
    /// The application's exit status, when the launcher waited for it.
    pub exit_code: Option<i32>,
}

/// Starts an installed application.
#[derive(Debug)]
pub struct Launcher {
    paths: InstallPaths,
}

impl Launcher {
    /// Builds a launcher for an explicit installation directory.
    pub fn for_application_dir(dir: impl Into<PathBuf>) -> Self {
        Self { paths: InstallPaths::from_application_dir(dir) }
    }

    /// Locates the installation from the launcher's own position on disk.
    ///
    /// The launcher is installed inside the application's directory, so the
    /// directory containing this executable *is* the installation root.
    /// Deriving it rather than embedding it at build time means one prebuilt
    /// launcher works for every application.
    ///
    /// `XPACK_APPLICATION_DIR` overrides this, which is what the tests use and
    /// what an unusual deployment can fall back on.
    pub fn discover() -> Result<Self> {
        if let Some(dir) = std::env::var_os("XPACK_APPLICATION_DIR") {
            return Ok(Self::for_application_dir(PathBuf::from(dir)));
        }

        let executable = std::env::current_exe()
            .map_err(|e| Error::Launch(format!("cannot locate the launcher itself: {e}")))?;
        let dir = xpack_core::atomic::parent_dir(&executable)?;
        Ok(Self::for_application_dir(dir))
    }

    /// The installation this launcher serves.
    pub fn paths(&self) -> &InstallPaths {
        &self.paths
    }

    /// Starts the active version, resolving probation if one is in progress.
    ///
    /// `wait` decides whether the launcher stays alive until the application
    /// exits and reports its status. A probationary launch always waits at
    /// least until the startup window closes, because that is the measurement.
    pub fn launch(&self, arguments: &[String], wait: bool) -> Result<Outcome> {
        // The lock is held only while state is read and written, never while
        // the application runs. Holding it for the lifetime of a user's
        // program would block every other xpack operation for as long as they
        // kept it open.
        let (version, manifest, probation) = {
            let lock = self.lock()?;
            let installer = Installer::new(&lock);
            installer.recover()?;

            let application = self.application_id()?;
            let state = lock.load_or_new_state(&application)?;
            let version = state.active()?.clone();

            if !installer.is_usable(&version) {
                return Err(Error::VersionNotInstalled(format!(
                    "{version} is active but its files are missing; reinstall it to repair"
                )));
            }

            let probation = !state.update.is_idle();
            if probation {
                // Recorded before the launch, so a crash that stops this
                // process from ever returning is still counted. A counter
                // incremented afterwards never sees the failure that mattered.
                let phase = installer.begin_attempt()?;
                if phase.attempts_exhausted() {
                    tracing::error!(%version, "version has used its startup attempts");
                    let rolled_back = installer.record_failure("exhausted its startup attempts")?;
                    return Ok(Outcome {
                        version,
                        startup: Some(StartupResult::FailedToStart { code: None }),
                        rolled_back_to: rolled_back,
                        exit_code: None,
                    });
                }
            }

            (version.clone(), self.read_manifest(&version)?, probation)
        };

        let request = LaunchRequest::new(self.paths.version_dir(&version), manifest.launch.clone())
            .with_user_arguments(arguments.to_vec());
        let mut child = launch(&request)?;

        if !probation {
            let exit_code = if wait {
                child.wait().map_err(|e| Error::Launch(e.to_string()))?.code()
            } else {
                None
            };
            return Ok(Outcome { version, startup: None, rolled_back_to: None, exit_code });
        }

        let window = Duration::from_secs(manifest.health.startup_timeout_seconds);
        let startup = observe_startup(&mut child, window)?;
        let outcome = self.record(&version, &startup)?;

        let exit_code = match (&startup, wait) {
            // Already exited; its status is known.
            (StartupResult::ExitedSuccessfully, _) => Some(0),
            (StartupResult::FailedToStart { code }, _) => *code,
            (StartupResult::SurvivedStartup, true) => {
                child.wait().map_err(|e| Error::Launch(e.to_string()))?.code()
            }
            (StartupResult::SurvivedStartup, false) => None,
        };

        Ok(Outcome { version, startup: Some(startup), rolled_back_to: outcome, exit_code })
    }

    /// Writes the probation result and rolls back if it failed.
    fn record(&self, version: &Version, startup: &StartupResult) -> Result<Option<Version>> {
        let lock = self.lock()?;
        let installer = Installer::new(&lock);

        if startup.is_healthy() {
            installer.commit_health()?;
            tracing::info!(%version, "version started successfully and is now committed");
            return Ok(None);
        }

        let reason = match startup {
            StartupResult::FailedToStart { code: Some(code) } => {
                format!("exited with status {code} during startup")
            }
            _ => "failed during startup".to_string(),
        };
        let rolled_back = installer.record_failure(reason)?;
        if let Some(target) = &rolled_back {
            tracing::warn!(%version, %target, "rolled back after a failed start");
        } else {
            tracing::error!(%version, "start failed and there is nothing to roll back to");
        }
        Ok(rolled_back)
    }

    fn lock(&self) -> Result<InstallLock> {
        InstallLock::acquire(&self.paths)
    }

    fn application_id(&self) -> Result<String> {
        self.paths
            .application_id()
            .map(ToString::to_string)
            .ok_or_else(|| Error::invalid("installation", "root has no application id"))
    }

    /// Reads the manifest recorded when this version was installed.
    ///
    /// Using the recorded manifest rather than re-reading a package means the
    /// launch matches exactly what was verified at install time.
    fn read_manifest(&self, version: &Version) -> Result<Manifest> {
        let path = self.paths.version_manifest_file(version);
        let bytes = std::fs::read(&path).map_err(|e| Error::io(&path, e))?;
        Manifest::from_slice(&bytes)
    }
}

/// Watches a probationary process until it exits or the window closes.
///
/// Polling rather than blocking, because the measurement is "did it survive
/// this long", and a blocking wait cannot be interrupted by a timer without
/// either a second thread or a platform-specific timed wait. Polling is
/// coarser and much easier to be sure of.
pub fn observe_startup(child: &mut std::process::Child, window: Duration) -> Result<StartupResult> {
    let deadline = Instant::now() + window;

    loop {
        match child.try_wait().map_err(|e| Error::Launch(e.to_string()))? {
            Some(status) if status.success() => return Ok(StartupResult::ExitedSuccessfully),
            Some(status) => return Ok(StartupResult::FailedToStart { code: status.code() }),
            None => {}
        }
        if Instant::now() >= deadline {
            return Ok(StartupResult::SurvivedStartup);
        }
        std::thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

/// Returns the directory a launcher at `executable` would serve.
pub fn application_dir_for(executable: &Path) -> Result<&Path> {
    xpack_core::atomic::parent_dir(executable)
}
