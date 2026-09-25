//! The launcher's health policy, against processes that behave differently.

use xpack_launcher::Launcher;

// Launching a real process and observing a startup window is Unix-shaped here;
// the Windows equivalents belong with a Windows CI run rather than being
// silently skipped. Scoped so the Windows build stays warning-free.
#[cfg(unix)]
use std::collections::BTreeMap;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
use xpack_core::manifest::{
    Application, FormatVersion, HealthSpec, LaunchSpec, PayloadSpec, UpdateSpec,
};
#[cfg(unix)]
use xpack_core::{InstallPaths, Manifest, Platform, Version};
#[cfg(unix)]
use xpack_install::{InstallOptions, Installer, TrustDecision, open_and_verify};
#[cfg(unix)]
use xpack_launcher::StartupResult;
#[cfg(unix)]
use xpack_package::PackageBuilder;
#[cfg(unix)]
use xpack_platform::InstallLock;
#[cfg(unix)]
use xpack_security::KeyPair;

#[cfg(unix)]
fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

/// How the payload behaves once started.
#[cfg(unix)]
#[derive(Clone, Copy)]
enum Behaviour {
    /// Exits zero immediately.
    ExitsCleanly,
    /// Exits non-zero immediately.
    CrashesImmediately,
    /// Keeps running well past any startup window.
    StaysRunning,
    /// Reports that it started, then keeps running.
    ReportsThenRuns,
    /// Reports that it started, then exits with a failure: a command-line
    /// tool whose answer this time was "no".
    ReportsThenFails,
    /// Keeps running but never reports — the case the report exists to catch.
    RunsWithoutReporting,
    /// Writes the installation directory it was told about, then exits.
    RecordsWhereItLives,
}

#[cfg(unix)]
fn script(behaviour: Behaviour, version: &str) -> String {
    match behaviour {
        Behaviour::ExitsCleanly => format!("#!/bin/sh\necho started {version}\nexit 0\n"),
        Behaviour::CrashesImmediately => {
            format!("#!/bin/sh\necho {version} failing\nexit 3\n")
        }
        Behaviour::StaysRunning => format!("#!/bin/sh\necho {version} running\nsleep 30\n"),
        Behaviour::ReportsThenRuns => {
            format!("#!/bin/sh\necho {version} running\n: > \"$XPACK_HEALTH_FILE\"\nsleep 30\n")
        }
        Behaviour::ReportsThenFails => {
            format!("#!/bin/sh\necho {version} refused\n: > \"$XPACK_HEALTH_FILE\"\nexit 3\n")
        }
        Behaviour::RunsWithoutReporting => {
            format!("#!/bin/sh\necho {version} running but silent\nsleep 30\n")
        }
        // Writing *into* the directory proves two things at once: that the
        // variable was set, and that it names somewhere that actually exists.
        Behaviour::RecordsWhereItLives => concat!(
            "#!/bin/sh\n",
            "printf '%s' \"$XPACK_APPLICATION_DIR\" > \"$XPACK_APPLICATION_DIR/observed-dir\"\n",
            "exit 0\n"
        )
        .to_string(),
    }
}

#[cfg(unix)]
fn build(dir: &Path, key: &KeyPair, version: &str, behaviour: Behaviour, timeout: u64) -> PathBuf {
    build_with(dir, key, version, behaviour, timeout, false)
}

#[cfg(unix)]
fn build_with(
    dir: &Path,
    key: &KeyPair,
    version: &str,
    behaviour: Behaviour,
    timeout: u64,
    require_report: bool,
) -> PathBuf {
    build_all(dir, key, version, behaviour, timeout, require_report, BTreeMap::new())
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn build_all(
    dir: &Path,
    key: &KeyPair,
    version: &str,
    behaviour: Behaviour,
    timeout: u64,
    require_report: bool,
    manifest_environment: BTreeMap<String, String>,
) -> PathBuf {
    let payload = dir.join(format!("src-{version}"));
    fs::create_dir_all(payload.join("bin")).unwrap();
    let app = payload.join("bin/app");
    fs::write(&app, script(behaviour, version)).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&app, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let manifest = Manifest {
        format_version: FormatVersion::CURRENT,
        application: Application {
            id: "com.example.app".into(),
            name: "App".into(),
            version: v(version),
            description: None,
            publisher: None,
        },
        platform: Platform::host().unwrap(),
        launch: LaunchSpec {
            executable: "bin/app".into(),
            arguments: vec![],
            working_directory: None,
            keep_working_directory: false,
            environment: manifest_environment,
        },
        update: UpdateSpec::default(),
        health: HealthSpec {
            startup_timeout_seconds: timeout,
            require_startup_report: require_report,
        },
        signing_key: None,
        desktop: xpack_core::DesktopSpec::default(),
        payload: PayloadSpec::default(),
        created_at: None,
        command: None,
        commands: Vec::new(),
    };
    let out = dir.join(format!("app-{version}.xpkg"));
    PackageBuilder::new(&payload, manifest).build(&out, key).unwrap();
    out
}

#[cfg(unix)]
struct World {
    dir: tempfile::TempDir,
    key: KeyPair,
    paths: InstallPaths,
}

#[cfg(unix)]
impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let key = KeyPair::generate().unwrap();
        let paths = InstallPaths::new(dir.path(), "com.example.app").unwrap();
        Self { dir, key, paths }
    }

    fn install(&self, version: &str, behaviour: Behaviour, timeout: u64) {
        let package = build(self.dir.path(), &self.key, version, behaviour, timeout);
        let lock = InstallLock::acquire(&self.paths).unwrap();
        let mut verified =
            open_and_verify(&package, &lock, &TrustDecision::Explicit(self.key.public())).unwrap();
        Installer::new(&lock)
            .install(
                &mut verified,
                &InstallOptions { activate: true, allow_downgrade: false, ..Default::default() },
            )
            .unwrap();
    }

    /// Installs a version without activating it, leaving it staged.
    /// Installs a version whose manifest demands a startup report.
    fn install_requiring_report(&self, version: &str, behaviour: Behaviour, timeout: u64) {
        let package = build_with(self.dir.path(), &self.key, version, behaviour, timeout, true);
        let lock = InstallLock::acquire(&self.paths).unwrap();
        let mut verified =
            open_and_verify(&package, &lock, &TrustDecision::Explicit(self.key.public())).unwrap();
        Installer::new(&lock)
            .install(
                &mut verified,
                &InstallOptions { activate: true, allow_downgrade: false, ..Default::default() },
            )
            .unwrap();
    }

    fn stage(&self, version: &str, behaviour: Behaviour, timeout: u64) {
        let package = build(self.dir.path(), &self.key, version, behaviour, timeout);
        let lock = InstallLock::acquire(&self.paths).unwrap();
        let mut verified =
            open_and_verify(&package, &lock, &TrustDecision::Explicit(self.key.public())).unwrap();
        Installer::new(&lock)
            .install(
                &mut verified,
                &InstallOptions { activate: false, allow_downgrade: false, ..Default::default() },
            )
            .unwrap();
    }

    fn status_of(&self, version: &str) -> xpack_core::state::VersionStatus {
        let lock = InstallLock::acquire(&self.paths).unwrap();
        let state = lock.load_state().unwrap().value;
        state.record(&Version::parse(version).unwrap()).unwrap().status
    }

    fn launcher(&self) -> Launcher {
        Launcher::for_application_dir(self.paths.root())
    }

    fn active(&self) -> Option<Version> {
        let lock = InstallLock::acquire(&self.paths).unwrap();
        lock.load_state().unwrap().value.current_version
    }
}

#[cfg(unix)]
#[test]
fn a_first_install_launches_without_probation() {
    let world = World::new();
    world.install("1.0.0", Behaviour::ExitsCleanly, 5);

    let outcome = world.launcher().launch(&[], true).unwrap();
    assert_eq!(outcome.version, v("1.0.0"));
    assert!(outcome.startup.is_none(), "a first install has nothing to roll back to");
    assert_eq!(outcome.exit_code, Some(0));
}

#[cfg(unix)]
#[test]
fn a_version_that_keeps_running_is_committed_when_its_window_closes() {
    // The policy that matters for a desktop application: it started and stayed
    // up. Waiting for it to exit would only confirm health when the user quits.
    let world = World::new();
    world.install("1.0.0", Behaviour::ExitsCleanly, 1);
    world.launcher().launch(&[], true).unwrap();
    world.install("1.1.0", Behaviour::StaysRunning, 1);

    let started = Instant::now();
    let outcome = world.launcher().launch(&[], false).unwrap();
    let elapsed = started.elapsed();

    assert_eq!(outcome.startup, Some(StartupResult::SurvivedStartup));
    assert!(outcome.rolled_back_to.is_none());
    assert_eq!(world.active(), Some(v("1.1.0")), "a surviving version stays active");

    // It must wait for the window, and must not wait for the process itself.
    assert!(elapsed >= Duration::from_secs(1), "the window must actually elapse");
    assert!(elapsed < Duration::from_secs(20), "it must not wait for a long-running app");

    let lock = InstallLock::acquire(&world.paths).unwrap();
    let state = lock.load_state().unwrap().value;
    assert!(state.update.is_idle(), "probation must be resolved");
    assert!(state.is_good(&v("1.1.0")));
}

#[cfg(unix)]
#[test]
fn a_version_that_dies_immediately_is_rolled_back() {
    let world = World::new();
    world.install("1.0.0", Behaviour::ExitsCleanly, 5);
    world.launcher().launch(&[], true).unwrap();
    world.install("1.1.0", Behaviour::CrashesImmediately, 5);

    let outcome = world.launcher().launch(&[], true).unwrap();
    assert_eq!(outcome.startup, Some(StartupResult::FailedToStart { code: Some(3) }));
    assert_eq!(outcome.rolled_back_to, Some(v("1.0.0")));
    assert_eq!(world.active(), Some(v("1.0.0")));

    let lock = InstallLock::acquire(&world.paths).unwrap();
    assert!(lock.load_state().unwrap().value.is_bad(&v("1.1.0")));
}

#[cfg(unix)]
#[test]
fn a_crash_is_detected_without_waiting_for_the_whole_window() {
    // A failing version must roll back promptly, not after a long timeout.
    let world = World::new();
    world.install("1.0.0", Behaviour::ExitsCleanly, 5);
    world.launcher().launch(&[], true).unwrap();
    world.install("1.1.0", Behaviour::CrashesImmediately, 30);

    let started = Instant::now();
    world.launcher().launch(&[], true).unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "detection must not wait out a 30 second window"
    );
}

#[cfg(unix)]
#[test]
fn a_short_lived_command_exiting_zero_inside_the_window_is_healthy() {
    let world = World::new();
    world.install("1.0.0", Behaviour::ExitsCleanly, 5);
    world.launcher().launch(&[], true).unwrap();
    world.install("1.1.0", Behaviour::ExitsCleanly, 5);

    let outcome = world.launcher().launch(&[], true).unwrap();
    assert_eq!(outcome.startup, Some(StartupResult::ExitedSuccessfully));
    assert_eq!(world.active(), Some(v("1.1.0")));
}

#[cfg(unix)]
#[test]
fn repeated_failed_starts_stop_rather_than_looping() {
    // Without a bound, a version that always crashes would be retried forever.
    let world = World::new();
    world.install("1.0.0", Behaviour::ExitsCleanly, 5);
    world.launcher().launch(&[], true).unwrap();
    world.install("1.1.0", Behaviour::CrashesImmediately, 5);

    let first = world.launcher().launch(&[], true).unwrap();
    assert_eq!(first.rolled_back_to, Some(v("1.0.0")));

    // 1.1.0 is quarantined, so the next launch runs 1.0.0 and does not retry.
    let second = world.launcher().launch(&[], true).unwrap();
    assert_eq!(second.version, v("1.0.0"));
    assert!(second.rolled_back_to.is_none());
}

#[cfg(unix)]
#[test]
fn an_exhausted_probation_rolls_back_without_launching_again() {
    // The case the attempt counter exists for. A crash — or a power cut —
    // between starting a probationary version and resolving its health leaves
    // the probation unresolved. Without a bound the launcher would retry that
    // version on every start, forever. With one, it gives up and rolls back.
    //
    // The version must not even be started once the budget is gone, which a
    // marker file proves.
    let world = World::new();
    world.install("1.0.0", Behaviour::ExitsCleanly, 5);
    world.launcher().launch(&[], true).unwrap();

    // A payload that records the fact it ran.
    let marker = world.dir.path().join("did-run.txt");
    let payload = world.dir.path().join("src-1.1.0/bin");
    fs::create_dir_all(&payload).unwrap();
    fs::write(payload.join("app"), format!("#!/bin/sh\ntouch {}\nsleep 30\n", marker.display()))
        .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(payload.join("app"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let manifest = Manifest {
        format_version: FormatVersion::CURRENT,
        application: Application {
            id: "com.example.app".into(),
            name: "App".into(),
            version: v("1.1.0"),
            description: None,
            publisher: None,
        },
        platform: Platform::host().unwrap(),
        launch: LaunchSpec {
            executable: "bin/app".into(),
            arguments: vec![],
            working_directory: None,
            keep_working_directory: false,
            environment: BTreeMap::new(),
        },
        update: UpdateSpec::default(),
        health: HealthSpec { startup_timeout_seconds: 30, ..Default::default() },
        signing_key: None,
        desktop: xpack_core::DesktopSpec::default(),
        payload: PayloadSpec::default(),
        created_at: None,
        command: None,
        commands: Vec::new(),
    };
    let package = world.dir.path().join("probation.xpkg");
    PackageBuilder::new(&world.dir.path().join("src-1.1.0"), manifest)
        .build(&package, &world.key)
        .unwrap();

    let lock = InstallLock::acquire(&world.paths).unwrap();
    let mut verified =
        open_and_verify(&package, &lock, &TrustDecision::Explicit(world.key.public())).unwrap();
    Installer::new(&lock)
        .install(
            &mut verified,
            &InstallOptions { activate: true, allow_downgrade: false, ..Default::default() },
        )
        .unwrap();

    // Model interruptions that already consumed the budget.
    let mut state = lock.load_state().unwrap().value;
    state.update = xpack_core::state::UpdatePhase::PendingVerification {
        version: v("1.1.0"),
        rollback_to: v("1.0.0"),
        attempts: xpack_core::state::MAX_ACTIVATION_ATTEMPTS,
    };
    lock.save_state(&state).unwrap();
    drop(lock);

    let outcome = world.launcher().launch(&[], true).unwrap();
    assert_eq!(outcome.rolled_back_to, Some(v("1.0.0")), "probation must terminate");
    assert!(!marker.exists(), "the exhausted version must not be started again");
    assert_eq!(world.active(), Some(v("1.0.0")));
}

#[cfg(unix)]
#[test]
fn each_probationary_start_is_counted_before_the_application_runs() {
    // Counting afterwards would never record the crash that stopped the
    // launcher from getting that far — which is exactly the failure the
    // counter exists to survive.
    //
    // Proven from inside: the payload reads the state file itself and records
    // what it saw. If the attempt is visible there, it was persisted before
    // the process started.
    let world = World::new();
    world.install("1.0.0", Behaviour::ExitsCleanly, 5);
    world.launcher().launch(&[], true).unwrap();

    let observed = world.dir.path().join("state-as-seen-by-the-app.json");
    let payload = world.dir.path().join("src-1.1.0/bin");
    fs::create_dir_all(&payload).unwrap();
    fs::write(
        payload.join("app"),
        format!(
            "#!/bin/sh\ncat {} > {}\nexit 0\n",
            world.paths.state_file().display(),
            observed.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(payload.join("app"), fs::Permissions::from_mode(0o755)).unwrap();
    }

    let manifest = Manifest {
        format_version: FormatVersion::CURRENT,
        application: Application {
            id: "com.example.app".into(),
            name: "App".into(),
            version: v("1.1.0"),
            description: None,
            publisher: None,
        },
        platform: Platform::host().unwrap(),
        launch: LaunchSpec {
            executable: "bin/app".into(),
            arguments: vec![],
            working_directory: None,
            keep_working_directory: false,
            environment: BTreeMap::new(),
        },
        update: UpdateSpec::default(),
        health: HealthSpec { startup_timeout_seconds: 5, ..Default::default() },
        signing_key: None,
        desktop: xpack_core::DesktopSpec::default(),
        payload: PayloadSpec::default(),
        created_at: None,
        command: None,
        commands: Vec::new(),
    };
    let package = world.dir.path().join("counted.xpkg");
    PackageBuilder::new(&world.dir.path().join("src-1.1.0"), manifest)
        .build(&package, &world.key)
        .unwrap();

    let lock = InstallLock::acquire(&world.paths).unwrap();
    let mut verified =
        open_and_verify(&package, &lock, &TrustDecision::Explicit(world.key.public())).unwrap();
    Installer::new(&lock)
        .install(
            &mut verified,
            &InstallOptions { activate: true, allow_downgrade: false, ..Default::default() },
        )
        .unwrap();
    drop(lock);

    world.launcher().launch(&[], true).unwrap();

    let seen = fs::read_to_string(&observed).expect("the application must have run");
    assert!(
        seen.contains("PENDING_VERIFICATION"),
        "the application should have observed itself on probation: {seen}"
    );
    assert!(
        seen.contains("\"attempts\": 1"),
        "the attempt must already be persisted when the application starts: {seen}"
    );
}

#[cfg(unix)]
#[test]
fn arguments_reach_the_application_unchanged() {
    let world = World::new();
    let dir = world.dir.path();
    let payload = dir.join("src-args/bin");
    fs::create_dir_all(&payload).unwrap();
    let marker = dir.join("args.txt");
    fs::write(
        payload.join("app"),
        format!("#!/bin/sh\nprintf '%s' \"$*\" > {}\n", marker.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(payload.join("app"), fs::Permissions::from_mode(0o755)).unwrap();
    }

    let manifest = Manifest {
        format_version: FormatVersion::CURRENT,
        application: Application {
            id: "com.example.app".into(),
            name: "App".into(),
            version: v("1.0.0"),
            description: None,
            publisher: None,
        },
        platform: Platform::host().unwrap(),
        launch: LaunchSpec {
            executable: "bin/app".into(),
            arguments: vec!["--managed".into()],
            working_directory: None,
            keep_working_directory: false,
            environment: BTreeMap::new(),
        },
        update: UpdateSpec::default(),
        health: HealthSpec::default(),
        signing_key: None,
        desktop: xpack_core::DesktopSpec::default(),
        payload: PayloadSpec::default(),
        created_at: None,
        command: None,
        commands: Vec::new(),
    };
    let package = dir.join("args.xpkg");
    PackageBuilder::new(&dir.join("src-args"), manifest).build(&package, &world.key).unwrap();

    let lock = InstallLock::acquire(&world.paths).unwrap();
    let mut verified =
        open_and_verify(&package, &lock, &TrustDecision::Explicit(world.key.public())).unwrap();
    Installer::new(&lock)
        .install(
            &mut verified,
            &InstallOptions { activate: true, allow_downgrade: false, ..Default::default() },
        )
        .unwrap();
    drop(lock);

    world.launcher().launch(&["--user".to_string(), "--flag=1".to_string()], true).unwrap();

    assert_eq!(
        fs::read_to_string(&marker).unwrap(),
        "--managed --user --flag=1",
        "manifest arguments first, then the user's, unchanged"
    );
}

#[cfg(unix)]
#[test]
fn an_active_version_whose_files_are_gone_fails_clearly() {
    let world = World::new();
    world.install("1.0.0", Behaviour::ExitsCleanly, 5);
    fs::remove_dir_all(world.paths.version_dir(&v("1.0.0"))).unwrap();

    let err = world.launcher().launch(&[], true).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("files are missing"), "got {message}");
    // "Clearly" is the claim this test makes, so check the sentence actually
    // reads as one. An explanation fed through a variant that wraps it ends up
    // as "... reinstall it to repair is not installed".
    assert!(
        !message.contains("repair is not installed"),
        "the message is mangled by its error variant: {message}"
    );
}

#[test]
fn the_launcher_locates_its_installation_from_its_own_path() {
    use std::fs;

    // Nothing is embedded at build time, so one prebuilt launcher serves every
    // application: the directory holding the executable is the installation.
    let dir = tempfile::tempdir().unwrap();
    let app_dir = dir.path().join("com.example.app");
    fs::create_dir_all(&app_dir).unwrap();
    let executable = app_dir.join("xpack-launcher");
    fs::write(&executable, b"").unwrap();

    let resolved = xpack_launcher::launcher::application_dir_for(&executable).unwrap();
    assert_eq!(resolved, app_dir);

    let launcher = Launcher::for_application_dir(resolved);
    assert_eq!(launcher.paths().application_id(), Some("com.example.app"));
}

#[test]
fn a_commands_copy_of_the_launcher_serves_the_installation_above_it() {
    // A Windows command is a copy of the launcher in `<root>/bin`, on the
    // user's PATH. Taking `bin` for the installation, it would find no state
    // and fail every time the command is typed.
    let dir = tempfile::tempdir().unwrap();
    let app_dir = dir.path().join("com.example.app");
    std::fs::create_dir_all(app_dir.join("state")).unwrap();
    std::fs::create_dir_all(app_dir.join("bin")).unwrap();
    let copy = app_dir.join("bin").join("mytool.exe");
    std::fs::write(&copy, b"").unwrap();

    assert_eq!(xpack_launcher::launcher::application_dir_for(&copy).unwrap(), app_dir);
}

#[test]
fn a_launcher_knows_which_command_it_was_started_as() {
    use xpack_launcher::launcher::requested_command;
    let root = std::path::Path::new("/apps/com.example.app");

    // Windows: the copy's own name, whatever the environment says.
    let copy = root.join("bin").join("cargo-xpack.exe");
    assert_eq!(requested_command(&copy, Some("xpack".into())).as_deref(), Some("cargo-xpack"));

    // Unix: the script names itself.
    let launcher = root.join("xPack");
    assert_eq!(
        requested_command(&launcher, Some("cargo-xpack".into())).as_deref(),
        Some("cargo-xpack")
    );

    // Neither: an ordinary start.
    assert_eq!(requested_command(&launcher, None), None);
    assert_eq!(requested_command(&launcher, Some("".into())), None);
}

#[cfg(unix)]
#[test]
fn a_launcher_does_not_hold_the_lock_while_the_application_runs() {
    // Holding it would block every other xpack operation for as long as the
    // user kept their application open.
    let world = World::new();
    world.install("1.0.0", Behaviour::StaysRunning, 1);

    let outcome = world.launcher().launch(&[], false).unwrap();
    assert_eq!(outcome.version, v("1.0.0"));

    InstallLock::acquire(&world.paths)
        .expect("the lock must be free while the application is still running");
}

#[test]
#[cfg(unix)]
fn a_staged_version_is_activated_at_the_next_launch() {
    // The background updater stages; the launcher activates. This is the join
    // between the two, and it is the only place activation happens for an
    // update nobody watched being downloaded.
    let world = World::new();
    world.install("1.0.0", Behaviour::StaysRunning, 1);
    world.stage("1.1.0", Behaviour::StaysRunning, 1);
    assert_eq!(world.active(), Some(Version::parse("1.0.0").unwrap()));

    let outcome = world.launcher().launch(&[], false).unwrap();

    assert_eq!(outcome.version, Version::parse("1.1.0").unwrap(), "the staged version did not run");
    assert_eq!(world.active(), Some(Version::parse("1.1.0").unwrap()));
}

#[test]
#[cfg(unix)]
fn a_staged_version_that_fails_rolls_back_and_leaves_the_old_one_good() {
    // The whole reason the updater does not activate: the version it staged
    // gets tried while something is watching, and a bad one is undone.
    let world = World::new();
    world.install("1.0.0", Behaviour::StaysRunning, 1);
    world.stage("1.1.0", Behaviour::CrashesImmediately, 1);

    let outcome = world.launcher().launch(&[], true).unwrap();

    assert_eq!(outcome.rolled_back_to, Some(Version::parse("1.0.0").unwrap()));
    assert_eq!(world.active(), Some(Version::parse("1.0.0").unwrap()));
    assert_eq!(
        world.status_of("1.0.0"),
        xpack_core::state::VersionStatus::Good,
        "the version rolled back to was itself quarantined"
    );
    assert_eq!(world.status_of("1.1.0"), xpack_core::state::VersionStatus::Bad);
}

#[test]
#[cfg(unix)]
fn a_download_in_flight_does_not_put_the_active_version_on_trial() {
    // The updater releases the installation lock across its download, so the
    // launcher genuinely runs while the phase says `Downloading`. Reading that
    // as a probation would spend the attempt budget and, on a non-zero exit,
    // quarantine a version that works.
    let world = World::new();
    world.install("1.0.0", Behaviour::CrashesImmediately, 1);

    {
        let lock = InstallLock::acquire(&world.paths).unwrap();
        let mut state = lock.load_state().unwrap().value;
        state.update = xpack_core::state::UpdatePhase::Downloading {
            version: Version::parse("1.1.0").unwrap(),
        };
        lock.save_state(&state).unwrap();
    }

    let outcome = world.launcher().launch(&[], true).unwrap();

    assert!(outcome.rolled_back_to.is_none());
    assert_eq!(
        world.status_of("1.0.0"),
        xpack_core::state::VersionStatus::Good,
        "a working version was quarantined because a download was in flight"
    );
}

#[test]
#[cfg(unix)]
fn a_version_that_reported_its_start_is_kept_whatever_it_exits_with() {
    // A command-line tool's exit status is its answer. Having said it started,
    // a version whose first run answered "no" is not a failed update, and
    // rolling it back would undo a good release over a refused input.
    let world = World::new();
    world.install("1.0.0", Behaviour::ExitsCleanly, 5);
    world.stage("1.1.0", Behaviour::ReportsThenFails, 5);

    let outcome = world.launcher().launch(&[], true).unwrap();

    assert_eq!(outcome.startup, Some(StartupResult::ReportedHealthy));
    assert!(outcome.rolled_back_to.is_none(), "rolled back to {:?}", outcome.rolled_back_to);
    assert_eq!(outcome.exit_code, Some(3), "the tool's own answer is passed on");
    assert_eq!(world.active(), Some(v("1.1.0")));
}

#[test]
#[cfg(unix)]
fn an_application_that_reports_is_healthy_without_waiting_out_the_window() {
    // The report is proof, not evidence. A version that sends one does not
    // have to survive a fifteen-second stare to be believed.
    let world = World::new();
    world.install("1.0.0", Behaviour::StaysRunning, 1);
    world.stage("1.1.0", Behaviour::ReportsThenRuns, 30);

    let started = Instant::now();
    let outcome = world.launcher().launch(&[], false).unwrap();

    assert_eq!(outcome.startup, Some(StartupResult::ReportedHealthy));
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "waited {:?} despite an explicit report",
        started.elapsed()
    );
    assert_eq!(world.active(), Some(Version::parse("1.1.0").unwrap()));
}

#[test]
#[cfg(unix)]
fn a_version_that_never_reports_is_rolled_back_when_the_manifest_requires_one() {
    // This is the approximation being closed: the application runs happily and
    // is still rolled back, because it never said it was working.
    let world = World::new();
    world.install("1.0.0", Behaviour::StaysRunning, 1);

    world.install_requiring_report("1.1.0", Behaviour::RunsWithoutReporting, 1);

    let outcome = world.launcher().launch(&[], false).unwrap();

    assert_eq!(outcome.startup, Some(StartupResult::NeverReported));
    assert_eq!(outcome.rolled_back_to, Some(Version::parse("1.0.0").unwrap()));
    assert_eq!(world.status_of("1.1.0"), xpack_core::state::VersionStatus::Bad);
}

#[test]
#[cfg(unix)]
fn a_report_is_not_required_unless_the_manifest_says_so() {
    // Every application that has not added the line must keep working exactly
    // as it did, or turning this on would roll back every existing update.
    let world = World::new();
    world.install("1.0.0", Behaviour::StaysRunning, 1);
    world.stage("1.1.0", Behaviour::RunsWithoutReporting, 1);

    let outcome = world.launcher().launch(&[], false).unwrap();

    assert_eq!(outcome.startup, Some(StartupResult::SurvivedStartup));
    assert!(outcome.rolled_back_to.is_none());
    assert_eq!(world.active(), Some(Version::parse("1.1.0").unwrap()));
}

#[test]
#[cfg(unix)]
fn a_stale_report_from_a_previous_run_does_not_make_a_silent_version_look_healthy() {
    // The invariant, not just the file deletion: a version that never reports
    // must still be judged as never reporting, however many reports it left
    // behind on earlier runs. Otherwise one good run long ago vouches for a
    // version forever.
    let world = World::new();
    world.install("1.0.0", Behaviour::StaysRunning, 1);

    world.install_requiring_report("1.1.0", Behaviour::RunsWithoutReporting, 1);

    // A report 1.1.0 left behind on some earlier, working run.
    let stale = world.paths.health_file(&Version::parse("1.1.0").unwrap());
    fs::create_dir_all(stale.parent().unwrap()).unwrap();
    fs::write(&stale, b"").unwrap();

    let outcome = world.launcher().launch(&[], false).unwrap();

    assert_eq!(
        outcome.startup,
        Some(StartupResult::NeverReported),
        "a stale report was accepted as this run's"
    );
    assert_eq!(outcome.rolled_back_to, Some(Version::parse("1.0.0").unwrap()));
}

#[cfg(unix)]
impl World {
    /// Installs a version whose signed manifest marks the release mandatory.
    fn install_mandatory(&self, version: &str, behaviour: Behaviour, activate: bool) {
        let package = build_mandatory(self.dir.path(), &self.key, version, behaviour);
        let lock = InstallLock::acquire(&self.paths).unwrap();
        let mut verified =
            open_and_verify(&package, &lock, &TrustDecision::Explicit(self.key.public())).unwrap();
        Installer::new(&lock)
            .install(
                &mut verified,
                &InstallOptions { activate, allow_downgrade: false, ..Default::default() },
            )
            .unwrap();
    }

    fn required(&self) -> Option<Version> {
        let lock = InstallLock::acquire(&self.paths).unwrap();
        lock.load_state().unwrap().value.required_version
    }
}

#[cfg(unix)]
fn build_mandatory(dir: &Path, key: &KeyPair, version: &str, behaviour: Behaviour) -> PathBuf {
    let payload = dir.join(format!("src-{version}"));
    fs::create_dir_all(payload.join("bin")).unwrap();
    let app = payload.join("bin/app");
    fs::write(&app, script(behaviour, version)).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&app, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let manifest = Manifest {
        format_version: FormatVersion::CURRENT,
        application: Application {
            id: "com.example.app".into(),
            name: "Example".into(),
            version: Version::parse(version).unwrap(),
            description: None,
            publisher: None,
        },
        platform: Platform::host().unwrap(),
        launch: LaunchSpec {
            executable: "bin/app".into(),
            arguments: vec![],
            working_directory: None,
            keep_working_directory: false,
            environment: BTreeMap::new(),
        },
        update: UpdateSpec { mandatory: true, ..UpdateSpec::default() },
        health: HealthSpec { startup_timeout_seconds: 1, ..Default::default() },
        signing_key: None,
        desktop: xpack_core::DesktopSpec::default(),
        payload: PayloadSpec::default(),
        created_at: None,
        command: None,
        commands: Vec::new(),
    };
    let out = dir.join(format!("mandatory-{version}.xpkg"));
    PackageBuilder::new(&payload, manifest).build(&out, key).unwrap();
    out
}

#[test]
#[cfg(unix)]
fn a_mandatory_release_records_the_version_that_must_run() {
    let world = World::new();
    world.install("1.0.0", Behaviour::StaysRunning, 1);
    world.install_mandatory("1.1.0", Behaviour::StaysRunning, false);

    assert_eq!(world.required(), Some(Version::parse("1.1.0").unwrap()));
}

#[test]
#[cfg(unix)]
fn a_mandatory_release_staged_in_the_background_applies_at_the_next_launch() {
    // The flag does not make a user wait at startup. It is staged like any
    // other version and costs one state write to apply.
    let world = World::new();
    world.install("1.0.0", Behaviour::StaysRunning, 1);
    world.install_mandatory("1.1.0", Behaviour::StaysRunning, false);
    assert_eq!(world.active(), Some(Version::parse("1.0.0").unwrap()));

    let outcome = world.launcher().launch(&[], false).unwrap();

    assert_eq!(outcome.version, Version::parse("1.1.0").unwrap());
    assert_eq!(world.active(), Some(Version::parse("1.1.0").unwrap()));
}

#[test]
#[cfg(unix)]
fn nothing_older_than_a_required_version_is_allowed_to_start() {
    // The point of the flag: a version its publisher has said must not run
    // does not run, even when it is sitting there working perfectly.
    let world = World::new();
    world.install("1.0.0", Behaviour::StaysRunning, 1);
    world.install_mandatory("1.1.0", Behaviour::StaysRunning, true);

    // The required version's files go missing, so it cannot be activated and
    // the installation falls back to something older.
    std::fs::remove_dir_all(world.paths.version_dir(&Version::parse("1.1.0").unwrap())).unwrap();
    {
        let lock = InstallLock::acquire(&world.paths).unwrap();
        let mut state = lock.load_state().unwrap().value;
        state.current_version = Some(Version::parse("1.0.0").unwrap());
        state.update = xpack_core::state::UpdatePhase::Idle;
        lock.save_state(&state).unwrap();
    }

    let error = world.launcher().launch(&[], false).unwrap_err();

    let message = error.to_string();
    assert!(message.contains("1.1.0") && message.contains("required"), "got: {message}");
}

#[test]
#[cfg(unix)]
fn an_ordinary_release_never_sets_a_requirement() {
    // Every application that has not opted in must be unaffected.
    let world = World::new();
    world.install("1.0.0", Behaviour::StaysRunning, 1);
    world.stage("1.1.0", Behaviour::StaysRunning, 1);

    assert_eq!(world.required(), None);
}

#[test]
#[cfg(unix)]
fn a_broken_mandatory_release_does_not_brick_the_installation() {
    // The failure this guards against: a publisher ships a mandatory version
    // that crashes on start. The rollback moves to a working older version,
    // and the requirement then refuses to start it — leaving the user unable
    // to run anything at all, which is worse than the problem the flag exists
    // to solve.
    let world = World::new();
    world.install("1.0.0", Behaviour::StaysRunning, 1);
    world.install_mandatory("2.0.0", Behaviour::CrashesImmediately, true);
    assert_eq!(world.required(), Some(Version::parse("2.0.0").unwrap()));

    // 2.0.0 fails its health check and is rolled back.
    let first = world.launcher().launch(&[], true).unwrap();
    assert_eq!(first.rolled_back_to, Some(Version::parse("1.0.0").unwrap()));
    assert_eq!(world.status_of("2.0.0"), xpack_core::state::VersionStatus::Bad);

    // The requirement must now be void: a version that could not start cannot
    // go on requiring anything.
    let second = world.launcher().launch(&[], false).unwrap();
    assert_eq!(
        second.version,
        Version::parse("1.0.0").unwrap(),
        "the installation was bricked by its own mandatory release"
    );
}

#[test]
#[cfg(unix)]
fn a_requirement_still_stands_when_the_required_version_merely_went_missing() {
    // The other half: voiding the requirement only because the version failed
    // must not void it whenever the version is simply absent, or the flag
    // would mean nothing — deleting a directory would be enough to escape it.
    let world = World::new();
    world.install("1.0.0", Behaviour::StaysRunning, 1);
    world.install_mandatory("2.0.0", Behaviour::StaysRunning, true);

    // 2.0.0 is healthy and required, but its files are removed and the active
    // pointer is put back to 1.0.0 — a damaged installation, not a bad release.
    let required = Version::parse("2.0.0").unwrap();
    fs::remove_dir_all(world.paths.version_dir(&required)).unwrap();
    {
        let lock = InstallLock::acquire(&world.paths).unwrap();
        let mut state = lock.load_state().unwrap().value;
        state.current_version = Some(Version::parse("1.0.0").unwrap());
        state.update = xpack_core::state::UpdatePhase::Idle;
        lock.save_state(&state).unwrap();
    }
    assert_ne!(world.status_of("2.0.0"), xpack_core::state::VersionStatus::Bad);

    let error = world.launcher().launch(&[], false).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("2.0.0") && message.contains("required"),
        "a missing requirement was silently ignored: {message}"
    );
}

#[cfg(unix)]
#[test]
fn the_application_is_told_where_its_installation_is() {
    // Without this an application can only reach its own installation by
    // walking up from its working directory, past the version directory —
    // two levels that nothing in the layout promises to keep stable.
    let world = World::new();
    world.install("1.0.0", Behaviour::RecordsWhereItLives, 5);

    Launcher::for_application_dir(world.paths.root()).launch(&[], true).unwrap();

    let observed = std::fs::read_to_string(world.paths.root().join("observed-dir")).unwrap();
    assert_eq!(std::path::Path::new(&observed), world.paths.root());
}

#[cfg(unix)]
#[test]
fn a_manifest_cannot_lie_to_the_application_about_where_it_lives() {
    // The manifest's environment is applied first and the launcher's last, so
    // a package cannot point its own application at another installation —
    // which is the same rule that stops one redirecting the health report.
    let world = World::new();
    let mut hostile = BTreeMap::new();
    hostile.insert("XPACK_APPLICATION_DIR".to_string(), "/somewhere/else".to_string());

    let package = build_all(
        world.dir.path(),
        &world.key,
        "1.0.0",
        Behaviour::RecordsWhereItLives,
        5,
        false,
        hostile,
    );
    let lock = InstallLock::acquire(&world.paths).unwrap();
    let mut verified =
        open_and_verify(&package, &lock, &TrustDecision::Explicit(world.key.public())).unwrap();
    Installer::new(&lock)
        .install(&mut verified, &InstallOptions { activate: true, ..Default::default() })
        .unwrap();
    drop(lock);

    Launcher::for_application_dir(world.paths.root()).launch(&[], true).unwrap();

    let observed = std::fs::read_to_string(world.paths.root().join("observed-dir")).unwrap();
    assert_eq!(std::path::Path::new(&observed), world.paths.root());
}

#[cfg(unix)]
#[test]
fn a_start_waits_for_a_brief_holder_of_the_installation_rather_than_failing() {
    // The background updater the launcher has just started takes the lock
    // for the moment it records a check. A start that lost that race used to
    // refuse to open the application at all.
    let world = World::new();
    world.install("1.0.0", Behaviour::ExitsCleanly, 5);

    let held = InstallLock::acquire(&world.paths).unwrap();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        drop(held);
    });
    let outcome = world.launcher().launch(&[], true);
    releaser.join().unwrap();

    let outcome = outcome.expect("the start gave up while the lock was briefly held");
    assert_eq!(outcome.version, v("1.0.0"));
    assert_eq!(outcome.exit_code, Some(0));
}
