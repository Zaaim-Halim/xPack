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
}

#[cfg(unix)]
fn script(behaviour: Behaviour, version: &str) -> String {
    match behaviour {
        Behaviour::ExitsCleanly => format!("#!/bin/sh\necho started {version}\nexit 0\n"),
        Behaviour::CrashesImmediately => {
            format!("#!/bin/sh\necho {version} failing\nexit 3\n")
        }
        Behaviour::StaysRunning => format!("#!/bin/sh\necho {version} running\nsleep 30\n"),
    }
}

#[cfg(unix)]
fn build(dir: &Path, key: &KeyPair, version: &str, behaviour: Behaviour, timeout: u64) -> PathBuf {
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
            environment: BTreeMap::new(),
        },
        update: UpdateSpec::default(),
        health: HealthSpec { startup_timeout_seconds: timeout },
        signing_key: None,
        payload: PayloadSpec::default(),
        created_at: None,
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
            .install(&mut verified, &InstallOptions { activate: true, allow_downgrade: false })
            .unwrap();
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
            environment: BTreeMap::new(),
        },
        update: UpdateSpec::default(),
        health: HealthSpec { startup_timeout_seconds: 30 },
        signing_key: None,
        payload: PayloadSpec::default(),
        created_at: None,
    };
    let package = world.dir.path().join("probation.xpkg");
    PackageBuilder::new(&world.dir.path().join("src-1.1.0"), manifest)
        .build(&package, &world.key)
        .unwrap();

    let lock = InstallLock::acquire(&world.paths).unwrap();
    let mut verified =
        open_and_verify(&package, &lock, &TrustDecision::Explicit(world.key.public())).unwrap();
    Installer::new(&lock)
        .install(&mut verified, &InstallOptions { activate: true, allow_downgrade: false })
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
            environment: BTreeMap::new(),
        },
        update: UpdateSpec::default(),
        health: HealthSpec { startup_timeout_seconds: 5 },
        signing_key: None,
        payload: PayloadSpec::default(),
        created_at: None,
    };
    let package = world.dir.path().join("counted.xpkg");
    PackageBuilder::new(&world.dir.path().join("src-1.1.0"), manifest)
        .build(&package, &world.key)
        .unwrap();

    let lock = InstallLock::acquire(&world.paths).unwrap();
    let mut verified =
        open_and_verify(&package, &lock, &TrustDecision::Explicit(world.key.public())).unwrap();
    Installer::new(&lock)
        .install(&mut verified, &InstallOptions { activate: true, allow_downgrade: false })
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
            environment: BTreeMap::new(),
        },
        update: UpdateSpec::default(),
        health: HealthSpec::default(),
        signing_key: None,
        payload: PayloadSpec::default(),
        created_at: None,
    };
    let package = dir.join("args.xpkg");
    PackageBuilder::new(&dir.join("src-args"), manifest).build(&package, &world.key).unwrap();

    let lock = InstallLock::acquire(&world.paths).unwrap();
    let mut verified =
        open_and_verify(&package, &lock, &TrustDecision::Explicit(world.key.public())).unwrap();
    Installer::new(&lock)
        .install(&mut verified, &InstallOptions { activate: true, allow_downgrade: false })
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
    assert!(err.to_string().contains("files are missing"), "got {err}");
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
