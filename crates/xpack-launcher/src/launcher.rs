//! Resolving the active version, starting it, and recording the result.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use xpack_core::state::UpdatePhase;
use xpack_core::{Error, InstallPaths, Manifest, Result, Version};
use xpack_install::Installer;
use xpack_platform::{InstallLock, LaunchRequest, launch};

/// Names the file an application creates to say it started successfully.
///
/// Set by the launcher in every application's environment. A file rather than
/// a socket or a pipe because xPack is runtime-independent: creating one is a
/// line of code in any language, needs no xPack library, and works identically
/// on every platform — where a named pipe on Windows would need Win32 calls
/// this workspace does not permit.
pub const HEALTH_FILE_ENV: &str = "XPACK_HEALTH_FILE";

/// How often a probationary process is checked for having exited.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// What happened to a probationary launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupResult {
    /// The application said for itself that it started.
    ///
    /// The only signal here that is not an inference. Everything else observes
    /// the process from outside and reasons about what that implies.
    ReportedHealthy,
    /// The process was still running when the startup window closed.
    SurvivedStartup,
    /// The window closed and the application never reported.
    ///
    /// Only reachable when the manifest requires a report. Without that, a
    /// process still running when the window closes is [`Self::SurvivedStartup`].
    NeverReported,
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
        !matches!(self, Self::FailedToStart { .. } | Self::NeverReported)
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

            // A version staged by the background updater becomes active here
            // and nowhere else. Activation starts a probation, and a probation
            // only means something while something watches the version start,
            // which is exactly what this process is about to do. Activating
            // from the updater instead would start a clock nobody is holding.
            let state = match &state.update {
                UpdatePhase::Staged { version } if installer.is_usable(version) => {
                    let staged = version.clone();
                    match installer.activate(&staged, false) {
                        Ok(()) => {
                            tracing::info!(version = %staged, "activating a staged version");
                            lock.load_or_new_state(&application)?
                        }
                        // A staged version that cannot be activated is not a
                        // reason to refuse to start: the version already
                        // running is fine, and it is better to open the
                        // application and try again next time than to leave a
                        // user with nothing.
                        Err(error) => {
                            tracing::warn!(version = %staged, %error, "could not activate the staged version");
                            state
                        }
                    }
                }
                _ => state,
            };

            let version = state.active()?.clone();
            if !installer.is_usable(&version) {
                return Err(Error::invalid(
                    "installation",
                    format!(
                        "{version} is active but its files are missing; reinstall it to repair"
                    ),
                ));
            }

            // A publisher marked a release mandatory, and the version about to
            // start is older than it. Refusing is the point: the alternative
            // is running something its author has said must not run, which for
            // the security release this exists for is the whole failure.
            //
            // Reached only when the required version is unusable — staged and
            // present, it was activated a few lines above. So this is a
            // genuinely broken installation, not an update waiting to happen,
            // and saying so beats starting anyway and hoping.
            // A required version that failed its own health check no longer
            // requires anything. Rollback is the safety net for a bad release,
            // and a net a publisher can switch off by setting one flag is not
            // a net: without this, shipping a mandatory version that crashes
            // on start leaves every installation unable to run *anything* —
            // the rollback moves to a working older version, and the
            // requirement then refuses to start it.
            //
            // Running the older version is strictly better than running
            // nothing, and the publisher who shipped the broken release is the
            // one who has to fix it.
            if let Some(required) = &state.required_version
                && &version < required
                && !state.is_bad(required)
            {
                return Err(Error::invalid(
                    "launch",
                    format!(
                        "version {required} is required and {version} is older; \
                         the required version is not installed and usable, so this \
                         installation needs repairing before it can start"
                    ),
                ));
            }

            let probation = state.update.is_probation();
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

        // Removed before the launch, never after: a file left by a previous
        // run of this same version would otherwise be read as this run's
        // report, and a version that crashes on every start would look healthy
        // forever.
        let health_file = self.paths.health_file(&version);
        let _ = std::fs::remove_file(&health_file);

        let request = LaunchRequest::new(self.paths.version_dir(&version), manifest.launch.clone())
            .with_user_arguments(arguments.to_vec())
            .with_launcher_environment(HEALTH_FILE_ENV, health_file.display().to_string());
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
        let startup = observe_startup_reporting(
            &mut child,
            window,
            &health_file,
            manifest.health.require_startup_report,
        )?;
        let outcome = self.record(&version, &startup)?;

        let exit_code = match (&startup, wait) {
            // Already exited; its status is known.
            (StartupResult::ExitedSuccessfully, _) => Some(0),
            (StartupResult::FailedToStart { code }, _) => *code,
            // Still running in every remaining case, reported or not.
            (_, true) => child.wait().map_err(|e| Error::Launch(e.to_string()))?.code(),
            (_, false) => None,
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
            StartupResult::NeverReported => {
                "never reported that it started, and the manifest requires it".to_string()
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
    let unused = std::path::Path::new("");
    observe_startup_reporting(child, window, unused, false)
}

/// Watches a probationary process, accepting a report from it as proof.
///
/// The report is checked before the exit status on every pass. An application
/// that reports and then exits has still started successfully; whatever
/// happened next is a different failure, and not one an update rollback can
/// fix.
///
/// `required` turns the startup window from a grace period into a deadline: a
/// process still running when it closes has failed rather than passed.
pub fn observe_startup_reporting(
    child: &mut std::process::Child,
    window: Duration,
    health_file: &Path,
    required: bool,
) -> Result<StartupResult> {
    let deadline = Instant::now() + window;
    let watching = !health_file.as_os_str().is_empty();

    loop {
        if watching && health_file.exists() {
            return Ok(StartupResult::ReportedHealthy);
        }
        match child.try_wait().map_err(|e| Error::Launch(e.to_string()))? {
            Some(status) if status.success() => {
                // A clean exit is proof enough on its own; requiring a report
                // from a process that has already finished successfully would
                // fail short-lived commands that did exactly what was asked.
                return Ok(StartupResult::ExitedSuccessfully);
            }
            Some(status) => return Ok(StartupResult::FailedToStart { code: status.code() }),
            None => {}
        }
        if Instant::now() >= deadline {
            return Ok(if required {
                StartupResult::NeverReported
            } else {
                StartupResult::SurvivedStartup
            });
        }
        std::thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

/// The names this installation's executables carry.
///
/// Read without taking the installation lock, which is safe for this one
/// field and only this one: it is written once, when the installation is
/// created, and never changed afterwards, so there is no moment at which a
/// reader could see it half way between two values. Taking the lock here
/// would instead contend with the launch this function runs just ahead of.
///
/// A state document that cannot be read at all falls back to xPack's own
/// names, which is what an installation predating named executables has.
fn installed_binary_names(paths: &InstallPaths) -> xpack_core::BinaryNames {
    xpack_core::store::load::<xpack_core::InstallState>(&paths.state_file())
        .map_or(xpack_core::BinaryNames::Xpack, |loaded| loaded.value.binary_names())
}

/// Returns the directory a launcher at `executable` would serve.
pub fn application_dir_for(executable: &Path) -> Result<&Path> {
    xpack_core::atomic::parent_dir(executable)
}

/// Starts the background updater, detached, and does not wait for it.
///
/// # Why the launcher does this
///
/// Something has to notice that an update exists. The alternatives are an
/// operating-system scheduler — a launchd agent, a systemd user unit, a
/// scheduled task — each of which has to be registered at install time, per
/// platform, and on macOS drags in the questions that come with shipping a
/// background agent. The launcher already runs every single time the user
/// opens their application, which is the one moment that needs no arranging.
///
/// The cost is honest: a user who never opens the application never updates.
/// For a desktop application that is the right trade, and a scheduled trigger
/// can be added later without changing anything here.
///
/// # Silent, reaped, and never fatal
///
/// The child gets no standard streams, because it shares a terminal with the
/// user's application and must never write to it. A failure to start it is
/// logged and otherwise ignored: an application must open whether or not its
/// updater does.
///
/// # Why a thread waits on it
///
/// `Command::spawn` does not detach — it forks a child of this process, and a
/// child that exits without being waited for stays in the process table as a
/// zombie until its parent ends. The launcher's parent is the user's
/// application session, which for a desktop application is hours or days, so
/// "spawn and forget" leaks an entry for the whole of it.
///
/// A thread blocked in `wait` costs one stack and ends the moment the updater
/// does. If the launcher exits first the thread goes with it and the updater
/// is reparented, which is the outcome "detached" was reaching for anyway.
pub fn spawn_updater(paths: &InstallPaths) -> bool {
    let updater = paths.updater_file_named(&installed_binary_names(paths));
    if !updater.is_file() {
        tracing::debug!(path = %updater.display(), "no updater is installed");
        return false;
    }

    let mut command = std::process::Command::new(&updater);
    command
        .arg("--application-dir")
        .arg(paths.root())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    without_a_console(&mut command);

    match command.spawn() {
        Ok(mut child) => {
            let pid = child.id();
            tracing::debug!(pid, "background updater started");
            std::thread::spawn(move || {
                let _ = child.wait();
                tracing::debug!(pid, "background updater finished");
            });
            true
        }
        Err(error) => {
            tracing::warn!(%error, "could not start the background updater");
            false
        }
    }
}

/// Stops Windows giving a background child a console window of its own.
///
/// The updater is a console-subsystem binary. Windows gives such a process a
/// console when its parent has none to inherit, and that console is a black
/// window that appears over the user's application and stays there for as long
/// as the update check runs. `CREATE_NO_WINDOW` suppresses it.
///
/// `DETACHED_PROCESS` would suppress it too, and is the flag the name
/// "detached" suggests. It is deliberately not used: the two are documented as
/// mutually exclusive, and `CREATE_NO_WINDOW` is the one that leaves the
/// child's standard handles alone — which matters, because the caller has
/// already redirected all three to null and a second mechanism fighting over
/// them buys nothing.
///
/// Detachment in the sense that matters — the updater outliving the launcher —
/// needs no flag on Windows. A child process there has no lifetime tie to its
/// parent; that tie is a Unix notion, and the thread waiting on the child
/// handles it.
#[cfg(windows)]
fn without_a_console(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;

    /// `CREATE_NO_WINDOW` from `processthreadsapi.h`. Spelled out rather than
    /// pulled from a Windows crate: one constant does not justify the
    /// dependency, and its value is part of a stable ABI.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    command.creation_flags(CREATE_NO_WINDOW);
}

/// Nothing to do: no Unix platform gives a spawned process a window.
#[cfg(not(windows))]
fn without_a_console(command: &mut std::process::Command) {
    let _ = command;
}
