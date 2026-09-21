//! Resolving the active version, starting it, and recording the result.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use xpack_core::state::UpdatePhase;
use xpack_core::{Error, InstallPaths, Manifest, Result, Version};
use xpack_install::Installer;
use xpack_platform::{InstallLock, LaunchRequest, launch, request_close};

/// Names the file an application creates to say it started successfully.
///
/// Set by the launcher in every application's environment. A file rather than
/// a socket or a pipe because xPack is runtime-independent: creating one is a
/// line of code in any language, needs no xPack library, and works identically
/// on every platform — where a named pipe on Windows would need Win32 calls
/// this workspace does not permit.
pub const HEALTH_FILE_ENV: &str = "XPACK_HEALTH_FILE";

/// Where the application's installation is, told to the application itself.
///
/// The same variable the runtime binaries read to decide which installation
/// they belong to, which is deliberate: an application that spawns
/// `xpack-updater` to show its own progress passes it down, and the updater
/// works on the installation that started it rather than on whichever one it
/// would have found by itself.
///
/// Without it an application can only reach its own installation by walking
/// up from its working directory — two levels, past the version directory —
/// which no part of the layout promises to keep stable.
pub const APPLICATION_DIR_ENV: &str = xpack_core::paths::APPLICATION_DIR_ENV;

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
    /// Whether the user accepted a restart while the application was running.
    ///
    /// Set by the update prompt, and only ever while this process waited for
    /// the application. The caller decides what to do with it; this type
    /// reports what the user asked for, it does not act on it.
    pub restart_requested: bool,
}

/// Starts an installed application.
#[derive(Debug)]
pub struct Launcher {
    paths: InstallPaths,
    offer_restart: bool,
}

impl Launcher {
    /// Builds a launcher for an explicit installation directory.
    pub fn for_application_dir(dir: impl Into<PathBuf>) -> Self {
        Self { paths: InstallPaths::from_application_dir(dir), offer_restart: true }
    }

    /// Stops an update prompt offering to restart the application.
    ///
    /// For the launch that *follows* a restart. This process restarts an
    /// application once and once only, so a second prompt offering one would
    /// be offering something that will not happen — and worse than not
    /// happening: the application would be asked to close, do so, and never
    /// come back, because nothing is left to start it again.
    ///
    /// Checks continue exactly as before. The prompt simply says the update
    /// will be used at the next start, which is then the truth.
    #[must_use]
    pub fn without_restart_offers(mut self) -> Self {
        self.offer_restart = false;
        self
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

            ensure_not_older_than_required(&state, &version)?;

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
                        restart_requested: false,
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
            .with_launcher_environment(HEALTH_FILE_ENV, health_file.display().to_string())
            .with_launcher_environment(
                APPLICATION_DIR_ENV,
                self.paths.root().display().to_string(),
            );
        let mut child = launch(&request)?;

        let watch = self.watch_for_updates(&manifest, child.id(), wait);

        if !probation {
            let exit_code = if wait {
                child.wait().map_err(|e| Error::Launch(e.to_string()))?.code()
            } else {
                None
            };
            return Ok(Outcome {
                version,
                startup: None,
                rolled_back_to: None,
                exit_code,
                restart_requested: watch.finished(),
            });
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

        Ok(Outcome {
            version,
            startup: Some(startup),
            rolled_back_to: outcome,
            exit_code,
            restart_requested: watch.finished(),
        })
    }

    /// Starts periodic update checks for the application just launched.
    ///
    /// Returns the flag an update prompt sets when the user agrees to restart,
    /// which stays false for every installation that does not check
    /// periodically — there is nothing to set it.
    ///
    /// Only when this process is waiting on the application: a checker thread
    /// in a launcher about to return would be killed before its first tick,
    /// and a prompt would have nothing able to restart anything.
    ///
    /// Both answers come from the manifest of the version that just started —
    /// whether to look at all, and how often. That release is the one whose
    /// publisher is answering for how much of their server's time this
    /// installation takes.
    fn watch_for_updates(&self, manifest: &Manifest, pid: u32, wait: bool) -> Watch {
        let watch = Watch {
            restart_requested: Arc::new(AtomicBool::new(false)),
            still_running: Arc::new(AtomicBool::new(true)),
        };

        if wait && manifest.update.check_while_running && manifest.update.url.is_some() {
            let interval = manifest.update.check_interval();
            // The handle is what lets a prompt offer a restart at all: this
            // process knows the application's pid and is the thing that will
            // start it again, so it is the only one in a position to promise
            // one. Withheld on the launch that follows a restart, which is a
            // launch that will not perform another.
            let application = self.offer_restart.then(|| RunningApplication {
                pid,
                restart_requested: Arc::clone(&watch.restart_requested),
                still_running: Arc::clone(&watch.still_running),
            });
            spawn_periodic_update_checks(&self.paths, interval, application);
        }

        watch
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
    let Some(mut child) = spawn_updater_process(paths) else {
        return false;
    };
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
        tracing::debug!(pid, "background updater finished");
    });
    true
}

/// Starts the updater and hands back the child, or `None` if it did not start.
///
/// Every caller reaps the process one way or another. Which way is the only
/// thing they differ in: the launch-time check leaves a thread to wait on it,
/// while the periodic checker waits inline because it has nothing else to do
/// until the check finishes.
fn spawn_updater_process(paths: &InstallPaths) -> Option<std::process::Child> {
    let updater = paths.updater_file_named(&installed_binary_names(paths));
    if !updater.is_file() {
        tracing::debug!(path = %updater.display(), "no updater is installed");
        return None;
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
        Ok(child) => {
            tracing::debug!(pid = child.id(), "background updater started");
            Some(child)
        }
        Err(error) => {
            tracing::warn!(%error, "could not start the background updater");
            None
        }
    }
}

/// How often the periodic checker wakes to see whether a check is due.
///
/// Deliberately much shorter than any interval it serves. A thread cannot
/// simply sleep for the whole interval: `sleep` counts monotonic time, which
/// stops while a machine is suspended, so a laptop closed for the night would
/// wake with hours left on a timer that was supposed to have fired. Waking
/// often and comparing the wall clock instead costs one small file read a
/// minute and cannot drift.
const CHECK_TICK: Duration = Duration::from_secs(60);

/// Refuses to start a version older than one a publisher marked mandatory.
///
/// The alternative is running something its author has said must not run,
/// which for the security release this exists for is the whole failure.
///
/// Reached only when the required version is unusable — staged and present, it
/// is activated before this — so it means a genuinely broken installation
/// rather than an update waiting to happen, and saying so beats starting
/// anyway and hoping.
///
/// # A required version that cannot start requires nothing
///
/// Rollback is the safety net for a bad release, and a net a publisher can
/// switch off by setting one flag is not a net. Without the check for a
/// version already known bad, shipping a mandatory release that crashes on
/// start would leave every installation unable to run *anything*: the rollback
/// moves to a working older version, and the requirement then refuses to start
/// it. Running the older version is strictly better than running nothing, and
/// the publisher who shipped the broken release is the one who has to fix it.
fn ensure_not_older_than_required(
    state: &xpack_core::InstallState,
    version: &Version,
) -> Result<()> {
    if let Some(required) = &state.required_version
        && version < required
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
    Ok(())
}

/// What a launch keeps hold of while the application runs.
///
/// Two flags shared with the checker thread: one it sets when the user agrees
/// to a restart, and one this side clears when the application exits so the
/// thread knows to stop.
struct Watch {
    restart_requested: Arc<AtomicBool>,
    still_running: Arc<AtomicBool>,
}

impl Watch {
    /// Records that the application has exited, and reports what was asked for.
    fn finished(&self) -> bool {
        self.still_running.store(false, Ordering::SeqCst);
        self.restart_requested.load(Ordering::SeqCst)
    }
}

/// The application this launcher started, as the checker thread sees it.
///
/// Carries the two things a prompt needs in order to offer a restart: which
/// process to ask to close, and somewhere to record that the user agreed. Both
/// belong to the launcher, which is why a prompt can only offer a restart when
/// a launcher is waiting — nothing else is in a position to deliver one.
#[derive(Debug, Clone)]
pub struct RunningApplication {
    /// Process id of the running application.
    pub pid: u32,
    /// Set when the user accepts a restart.
    pub restart_requested: Arc<AtomicBool>,
    /// Cleared once the application has exited.
    ///
    /// The checker thread outlives a single run of the application: after a
    /// restart the launcher starts a second one in the same process, and the
    /// thread watching the first would otherwise keep ticking forever against
    /// a process that no longer exists. Asking a dead process to close is not
    /// harmless — the operating system reuses process ids, so the request
    /// could reach something else entirely.
    pub still_running: Arc<AtomicBool>,
}

impl RunningApplication {
    /// Whether the application this handle describes is still running.
    pub fn is_running(&self) -> bool {
        self.still_running.load(Ordering::SeqCst)
    }
}

/// Runs update checks for as long as the application does.
///
/// Starts a thread that spawns the one-shot updater whenever `interval` has
/// passed since the last check. The updater stays a program that runs and
/// exits: nothing here holds a connection, keeps a scheduler, or survives the
/// application it was started for.
///
/// # Why the launcher and not a scheduler
///
/// Registering a launchd agent, a systemd unit or a scheduled task means
/// writing to the user's machine outside the installation directory, per
/// platform, at install time — and on macOS it drags in everything that comes
/// with shipping a background agent. This process is already running for
/// exactly as long as the application is open, which is the window a periodic
/// check is useful in anyway.
///
/// The cost is stated plainly: closing the application stops the checking.
/// Whatever was in flight finishes, because the updater is its own process,
/// but no further check happens until the application is opened again.
pub fn spawn_periodic_update_checks(
    paths: &InstallPaths,
    interval: Duration,
    application: Option<RunningApplication>,
) {
    let paths = paths.clone();
    tracing::debug!(interval_seconds = interval.as_secs(), "periodic update checks are on");

    // Detached on purpose, and never joined. It holds nothing that has to be
    // released, so ending with the process is the whole of its shutdown.
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(CHECK_TICK);

            // The application this thread was started for has exited, so the
            // thread has nothing left to watch. Ending here rather than
            // looping keeps a restarted application from being watched by two
            // threads, the older of which holds a process id that has since
            // been reused by something unrelated.
            if application.as_ref().is_some_and(|application| !application.is_running()) {
                tracing::debug!("the application has exited; periodic checks are done");
                return;
            }

            if check_is_due(&paths, interval) {
                // Waited on rather than fired and forgotten, so a slow
                // download cannot have a second updater started on top of it.
                // The updater takes the installation lock and would refuse
                // anyway; this keeps it from having to.
                if let Some(mut child) = spawn_updater_process(&paths) {
                    let _ = child.wait();
                }
            }

            // Every tick, and deliberately not only after a check this thread
            // ran. A version is announced because it is staged, not because of
            // which process staged it: the check that runs at startup stages
            // versions too, and tying the announcement to this thread's own
            // work would leave one of those unmentioned until the next check
            // fell due — a whole interval after it was ready to use.
            announce_a_staged_version(&paths, application.as_ref());
        }
    });
}

/// Tells the user about a staged version, once, if anything asked us to.
///
/// Does nothing at all unless three things line up: a version is staged, its
/// publisher asked for a prompt, and a notifier binary was installed. Most
/// installations fail the second or third of those and this returns having
/// touched nothing.
///
/// # The lock is not held while the dialog is open
///
/// A dialog stays open for as long as the user ignores it, which can be hours.
/// Holding the installation lock across it would block every other xPack
/// operation for that whole time — an install, a rollback, the next check —
/// so state is read, the lock is released, and it is taken again only to
/// record that the announcement happened.
pub fn announce_a_staged_version(paths: &InstallPaths, application: Option<&RunningApplication>) {
    let Some(announcement) = pending_announcement(paths) else {
        return;
    };

    let notifier = announcement.notifier.clone();
    let version = announcement.version.clone();
    let Some(answer) = run_notifier(&notifier, &announcement, application.is_some()) else {
        return;
    };

    // Recorded only when a dialog actually opened. An answer of "nothing was
    // shown" is not the user declining, and treating it as one would hide the
    // update behind a prompt that never appeared.
    if let Err(error) = record_announcement(paths, &version) {
        tracing::warn!(%error, %version, "could not record that the update was announced");
    }
    tracing::info!(%version, answer = answer.describe(), "the user was told about a staged version");

    if answer == Answer::Apply
        && let Some(application) = application
    {
        begin_restart(application, &version);
    }
}

/// Records that a restart was agreed to, and asks the application to close.
///
/// The order matters. The flag is set first, because the application may exit
/// the instant it is asked and the launcher waiting on it must already know
/// why. Asking first and recording second leaves a window where the process is
/// gone and nothing says a restart was wanted.
///
/// # The application may say no
///
/// What is sent is a request, not a kill: the application runs its own
/// shutdown, and an application with unsaved work is expected to ask its user
/// about it and to stay running if they cancel. That is not an error here. The
/// flag stays set, so if the user closes the application an hour later it
/// comes back on the new version — and if they never do, the update is applied
/// at the next start like any other.
fn begin_restart(application: &RunningApplication, version: &Version) {
    application.restart_requested.store(true, Ordering::SeqCst);
    tracing::info!(%version, pid = application.pid, "the user agreed to restart");

    if let Err(error) = request_close(application.pid) {
        // Usually means it has already exited, which is the outcome that was
        // being asked for.
        tracing::debug!(%error, pid = application.pid, "the application did not take the request");
    }
}

/// What a pending announcement needs to say, gathered under one lock window.
struct Announcement {
    version: Version,
    application: String,
    severity: xpack_core::UpdateSeverity,
    title: Option<String>,
    message: Option<String>,
    icon: Option<PathBuf>,
    notifier: PathBuf,
}

/// Decides whether there is an announcement to make, and collects it.
///
/// Returns `None` for every ordinary installation: nothing staged, nothing
/// asked for a prompt, already announced, or no notifier on disk.
fn pending_announcement(paths: &InstallPaths) -> Option<Announcement> {
    // A lock-free rejection first, because this runs on every tick and almost
    // every tick has nothing to say. Reading the state file without the lock
    // can only be stale, never torn — it is written by atomic replacement —
    // and everything it decides here is checked again under the lock below.
    let quick = xpack_core::InstallState::load(&paths.state_file()).ok()?;
    match &quick.value.update {
        UpdatePhase::Staged { version } if quick.value.update_needs_announcing(version) => {}
        _ => return None,
    }

    let lock = InstallLock::acquire(paths).ok()?;
    let application = paths.application_id()?;
    let state = lock.load_or_new_state(application).ok()?;

    let UpdatePhase::Staged { version } = &state.update else {
        return None;
    };
    if !state.update_needs_announcing(version) {
        return None;
    }

    // The staged version's own manifest, not the running one's: a release
    // describes its own urgency, and it is the document the publisher signed
    // over that version.
    let manifest_file = paths.version_manifest_file(version);
    let bytes = std::fs::read(&manifest_file).ok()?;
    let manifest = Manifest::from_slice(&bytes).ok()?;
    if !manifest.update.notify {
        return None;
    }

    let notifier = paths.notifier_file_named(&state.binary_names());
    if !notifier.is_file() {
        // Said plainly, and at a level somebody reads, because this is the one
        // update setting a release cannot turn on for itself. Everything else
        // about updating travels in the manifest and takes effect as soon as
        // that release is installed; a dialog is a binary, and the updater
        // runs from inside the installation with no copy of one to place.
        //
        // So a publisher who adds prompting after an installation was made
        // gets silence, and would go on getting it with nothing to read. This
        // is the line that tells them a new installer is what it takes.
        tracing::warn!(
            version = %version,
            path = %notifier.display(),
            "this version asks to announce itself, but the installation was set up without a \
             dialog; only an installer can add one, so the update will be applied quietly at \
             the next start"
        );
        return None;
    }

    // The copy kept in the installation root, not the one inside the version
    // directory: the dialog should still have an icon after the next update
    // replaces that directory.
    let icon = xpack_install::integration::icon_destination(paths, &manifest.desktop)
        .filter(|path| path.is_file());

    let prompt = manifest.update.prompt.clone();
    Some(Announcement {
        version: version.clone(),
        application: manifest.application.name.clone(),
        severity: manifest.update.effective_severity(),
        title: prompt.as_ref().map(|prompt| prompt.title.clone()),
        message: prompt.map(|prompt| prompt.message),
        icon,
        notifier,
    })
}

/// Runs the notifier and waits for the user, returning how they answered.
///
/// `None` means no dialog was shown — the binary would not start, or reported
/// that it could not open one — which is not an answer and must not be
/// recorded as though it were.
fn run_notifier(notifier: &Path, announcement: &Announcement, can_restart: bool) -> Option<Answer> {
    let mut command = std::process::Command::new(notifier);
    command
        .arg("--application")
        .arg(&announcement.application)
        .arg("--version")
        .arg(announcement.version.to_string())
        .arg("--severity")
        .arg(severity_argument(announcement.severity))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(title) = &announcement.title {
        command.arg("--title").arg(title);
    }
    if let Some(message) = &announcement.message {
        command.arg("--message").arg(message);
    }
    if let Some(icon) = &announcement.icon {
        command.arg("--icon").arg(icon);
    }
    // Offered only when this process is waiting on the application and can
    // therefore start it again. Without that the dialog says the update will
    // be used at the next start, which is then the truth.
    if can_restart {
        command.arg("--can-restart");
    }
    without_a_console(&mut command);

    let status = match command.status() {
        Ok(status) => status,
        Err(error) => {
            tracing::warn!(%error, "could not start the update notifier");
            return None;
        }
    };

    match status.code() {
        Some(0) => Some(Answer::Apply),
        Some(1) => Some(Answer::Later),
        Some(other) => {
            tracing::warn!(code = other, "the notifier showed nothing");
            None
        }
        None => {
            tracing::warn!("the notifier was killed before it answered");
            None
        }
    }
}

/// What the user said, as the notifier's exit code reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// Apply it now.
    Apply,
    /// Not yet.
    Later,
}

impl Answer {
    fn describe(self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::Later => "later",
        }
    }
}

/// The severity as the notifier's command line spells it.
fn severity_argument(severity: xpack_core::UpdateSeverity) -> &'static str {
    match severity {
        xpack_core::UpdateSeverity::Optional => "optional",
        xpack_core::UpdateSeverity::Recommended => "recommended",
        xpack_core::UpdateSeverity::Critical => "critical",
    }
}

/// Records that the user has been told, so it is not said again.
fn record_announcement(paths: &InstallPaths, version: &Version) -> Result<()> {
    let lock = InstallLock::acquire(paths)?;
    let application = paths
        .application_id()
        .ok_or_else(|| Error::invalid("installation", "root has no application id"))?;
    let mut state = lock.load_or_new_state(application)?;
    state.announced_update = Some(version.clone());
    lock.save_state(&state)
}

/// Whether the installation is due another check, read without the lock.
///
/// A hint, not a decision. The updater re-reads the same field under the
/// installation lock and decides for itself, so the worst a stale answer here
/// can do is start a process that exits again immediately. Taking the lock for
/// a read every minute would mean contending with installs and rollbacks for
/// no gain.
///
/// An installation whose state cannot be read is not due. It is either being
/// written at this instant or genuinely broken, and neither is a reason for a
/// background thread to start spawning processes.
fn check_is_due(paths: &InstallPaths, interval: Duration) -> bool {
    let Ok(loaded) = xpack_core::InstallState::load(&paths.state_file()) else {
        return false;
    };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs());
    loaded.value.update_check_is_due(now, interval.as_secs())
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

#[cfg(test)]
mod tests {
    use super::*;
    use xpack_core::InstallState;

    /// An installation directory with a state file in it.
    fn installation(last_check: Option<u64>) -> (tempfile::TempDir, InstallPaths) {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let paths = InstallPaths::from_application_dir(dir.path());
        let state_file = paths.state_file();
        std::fs::create_dir_all(state_file.parent().expect("a state directory"))
            .expect("the state directory");
        let mut state = InstallState::new("com.example.app");
        state.last_update_check = last_check;
        state.save(&state_file).expect("state to be written");
        (dir, paths)
    }

    /// Seconds since the epoch, as the checker itself reads the clock.
    fn now() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).expect("a clock after 1970").as_secs()
    }

    #[test]
    fn an_installation_checked_moments_ago_is_not_due() {
        let (_dir, paths) = installation(Some(now()));
        assert!(!check_is_due(&paths, Duration::from_secs(3 * 60 * 60)));
    }

    #[test]
    fn an_installation_last_checked_before_the_interval_is_due() {
        let (_dir, paths) = installation(Some(now() - 4 * 60 * 60));
        assert!(check_is_due(&paths, Duration::from_secs(3 * 60 * 60)));
    }

    #[test]
    fn a_clock_that_moved_backwards_makes_a_check_due_rather_than_never_due() {
        // A machine whose clock is corrected backwards by a year would
        // otherwise stop checking until the year had passed again.
        let (_dir, paths) = installation(Some(now() + 365 * 24 * 60 * 60));
        assert!(check_is_due(&paths, Duration::from_secs(3 * 60 * 60)));
    }

    #[test]
    fn an_installation_with_no_state_file_is_not_due() {
        // Nothing here should start spawning processes against an
        // installation it cannot read. The launch-time check already ran.
        let dir = tempfile::tempdir().expect("a temporary directory");
        let paths = InstallPaths::from_application_dir(dir.path());
        assert!(!check_is_due(&paths, Duration::from_secs(3 * 60 * 60)));
    }

    #[test]
    fn an_unreadable_state_file_is_not_due_rather_than_always_due() {
        let (_dir, paths) = installation(Some(now()));
        std::fs::write(paths.state_file(), b"{ not json").expect("the state file to be replaced");
        // The backup left by the save above is deliberately removed too, so
        // this tests the case where nothing can be recovered.
        for entry in std::fs::read_dir(paths.state_file().parent().expect("a state directory"))
            .expect("the state directory to be readable")
        {
            let entry = entry.expect("a directory entry");
            if entry.path() != paths.state_file() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        assert!(!check_is_due(&paths, Duration::from_secs(3 * 60 * 60)));
    }

    #[test]
    fn a_never_checked_installation_is_due_immediately() {
        let (_dir, paths) = installation(None);
        assert!(check_is_due(&paths, Duration::from_secs(3 * 60 * 60)));
    }
}
