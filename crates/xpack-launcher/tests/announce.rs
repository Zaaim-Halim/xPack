//! Telling the user that a version is waiting.
//!
//! The dialog itself can only be judged on Windows. Everything leading to it
//! cannot: which version is announced, whether the publisher asked for a
//! prompt at all, what the notifier is told, and — the one that decides
//! whether this feature is tolerable to live with — that the same version is
//! never announced twice.
//!
//! So the notifier is replaced with a script that records its arguments. What
//! is tested here is the launcher's half of the conversation, which is the
//! half that would otherwise have no test at all.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use xpack_core::manifest::{
    Application, FormatVersion, LaunchSpec, PayloadSpec, PromptSpec, UpdateSeverity, UpdateSpec,
};
use xpack_core::{InstallPaths, Manifest, Platform, Version};
use xpack_install::{InstallOptions, Installer, TrustDecision, open_and_verify};
use xpack_platform::InstallLock;
use xpack_security::KeyPair;

fn v(text: &str) -> Version {
    Version::parse(text).unwrap()
}

/// Builds a package whose manifest carries the update settings under test.
fn build(dir: &Path, key: &KeyPair, version: &str, update: UpdateSpec) -> PathBuf {
    let payload = dir.join(format!("src-{version}"));
    fs::create_dir_all(payload.join("bin")).unwrap();
    let app = payload.join("bin/app");
    fs::write(&app, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&app, fs::Permissions::from_mode(0o755)).unwrap();

    let manifest = Manifest {
        format_version: FormatVersion::CURRENT,
        application: Application {
            id: "com.example.app".into(),
            name: "Demo App".into(),
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
            environment: BTreeMap::new(),
        },
        update,
        health: xpack_core::HealthSpec::default(),
        signing_key: None,
        desktop: xpack_core::DesktopSpec::default(),
        payload: PayloadSpec::default(),
        created_at: None,
    };
    let out = dir.join(format!("app-{version}.xpkg"));
    xpack_package::PackageBuilder::new(&payload, manifest).build(&out, key).unwrap();
    out
}

/// An update configuration that asks to be announced.
fn announcing() -> UpdateSpec {
    UpdateSpec {
        url: Some("https://updates.example.com/demo".into()),
        check_while_running: true,
        check_interval_minutes: Some(180),
        notify: true,
        severity: UpdateSeverity::Recommended,
        prompt: Some(PromptSpec {
            title: "New version".into(),
            message: "Version 1.1.0 is ready.".into(),
        }),
        ..UpdateSpec::default()
    }
}

struct World {
    dir: tempfile::TempDir,
    key: KeyPair,
    paths: InstallPaths,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let key = KeyPair::generate().unwrap();
        let paths = InstallPaths::new(dir.path(), "com.example.app").unwrap();
        let world = Self { dir, key, paths };
        world.install("1.0.0", UpdateSpec::default(), true);
        world
    }

    fn install(&self, version: &str, update: UpdateSpec, activate: bool) {
        let package = build(self.dir.path(), &self.key, version, update);
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

    /// Where the fake notifier writes what it was told.
    fn log(&self) -> PathBuf {
        self.paths.root().join("notifier.log")
    }

    /// Installs a stand-in notifier that records its arguments and exits with
    /// `code`, the way the real one reports the user's answer.
    fn install_fake_notifier(&self, code: i32) {
        let lock = InstallLock::acquire(&self.paths).unwrap();
        let names = lock.load_state().unwrap().value.binary_names();
        drop(lock);

        let path = self.paths.notifier_file_named(&names);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"{}\"\nexit {code}\n",
                self.log().display()
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Everything the notifier has been told, across every run.
    fn notifier_saw(&self) -> Vec<String> {
        fs::read_to_string(self.log())
            .unwrap_or_default()
            .lines()
            .map(ToString::to_string)
            .collect()
    }

    fn runs(&self) -> usize {
        self.notifier_saw().iter().filter(|line| line.as_str() == "--application").count()
    }

    fn announced(&self) -> Option<Version> {
        let lock = InstallLock::acquire(&self.paths).unwrap();
        lock.load_state().unwrap().value.announced_update
    }
}

#[test]
fn a_staged_version_that_asked_to_be_announced_reaches_the_notifier() {
    let world = World::new();
    world.install_fake_notifier(0);
    world.install("1.1.0", announcing(), false);

    xpack_launcher::announce_a_staged_version(&world.paths, None);

    let seen = world.notifier_saw();
    assert_eq!(world.runs(), 1, "{seen:?}");
    assert!(seen.contains(&"Demo App".to_string()), "{seen:?}");
    assert!(seen.contains(&"1.1.0".to_string()), "{seen:?}");
    assert!(seen.contains(&"recommended".to_string()), "{seen:?}");
    assert!(seen.contains(&"New version".to_string()), "{seen:?}");
    assert!(seen.contains(&"Version 1.1.0 is ready.".to_string()), "{seen:?}");
    assert_eq!(world.announced(), Some(v("1.1.0")));
}

#[test]
fn the_same_version_is_never_announced_twice() {
    // A staged version sits there until the application is restarted, which
    // may be days. Announcing it on every check would be a dialog every few
    // hours about something the user has already seen.
    let world = World::new();
    world.install_fake_notifier(0);
    world.install("1.1.0", announcing(), false);

    xpack_launcher::announce_a_staged_version(&world.paths, None);
    xpack_launcher::announce_a_staged_version(&world.paths, None);
    xpack_launcher::announce_a_staged_version(&world.paths, None);

    assert_eq!(world.runs(), 1, "{:?}", world.notifier_saw());
}

#[test]
fn a_newer_staged_version_is_announced_even_though_the_last_one_was() {
    let world = World::new();
    world.install_fake_notifier(0);
    world.install("1.1.0", announcing(), false);
    xpack_launcher::announce_a_staged_version(&world.paths, None);

    world.install("1.2.0", announcing(), false);
    xpack_launcher::announce_a_staged_version(&world.paths, None);

    assert_eq!(world.runs(), 2, "{:?}", world.notifier_saw());
    assert_eq!(world.announced(), Some(v("1.2.0")));
}

#[test]
fn a_dialog_that_could_not_be_shown_is_not_recorded_as_an_answer() {
    // Exit code 3 is "nothing was shown". Recording it would hide the update
    // behind a prompt the user never saw.
    let world = World::new();
    world.install_fake_notifier(3);
    world.install("1.1.0", announcing(), false);

    xpack_launcher::announce_a_staged_version(&world.paths, None);

    assert_eq!(world.runs(), 1);
    assert_eq!(world.announced(), None, "an unshown dialog was recorded as an announcement");
}

#[test]
fn a_user_who_said_later_is_not_asked_again_about_the_same_version() {
    let world = World::new();
    world.install_fake_notifier(1);
    world.install("1.1.0", announcing(), false);

    xpack_launcher::announce_a_staged_version(&world.paths, None);
    xpack_launcher::announce_a_staged_version(&world.paths, None);

    assert_eq!(world.runs(), 1, "{:?}", world.notifier_saw());
    assert_eq!(world.announced(), Some(v("1.1.0")));
}

#[test]
fn a_version_whose_publisher_asked_for_no_prompt_is_staged_in_silence() {
    let world = World::new();
    world.install_fake_notifier(0);
    world.install("1.1.0", UpdateSpec { notify: false, ..announcing() }, false);

    xpack_launcher::announce_a_staged_version(&world.paths, None);

    assert_eq!(world.runs(), 0, "a silent update opened a dialog");
    assert_eq!(world.announced(), None);
}

#[test]
fn an_installation_with_no_notifier_says_nothing_and_stays_quiet() {
    // Most installations: the publisher asked for a prompt but the binary was
    // never placed, because the package targets a platform with no dialog.
    let world = World::new();
    world.install("1.1.0", announcing(), false);

    xpack_launcher::announce_a_staged_version(&world.paths, None);

    assert_eq!(world.announced(), None);
}

#[test]
fn nothing_staged_means_nothing_to_say() {
    let world = World::new();
    world.install_fake_notifier(0);

    xpack_launcher::announce_a_staged_version(&world.paths, None);

    assert_eq!(world.runs(), 0);
}

/// A stand-in for the user's application: a process that sits there until
/// something asks it to stop.
fn a_running_application() -> std::process::Child {
    std::process::Command::new("sleep")
        .arg("60")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("a process to stand in for the application")
}

/// Waits briefly for a child to exit, so a failure is a failed assertion
/// rather than a test run that never finishes.
fn exited_within(child: &mut std::process::Child, limit: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + limit;
    while std::time::Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

fn handle(child: &std::process::Child) -> xpack_launcher::RunningApplication {
    xpack_launcher::RunningApplication {
        pid: child.id(),
        restart_requested: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        still_running: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
    }
}

#[test]
fn a_restart_is_offered_only_when_something_can_deliver_one() {
    let world = World::new();
    world.install_fake_notifier(1);
    world.install("1.1.0", announcing(), false);

    xpack_launcher::announce_a_staged_version(&world.paths, None);

    assert!(
        !world.notifier_saw().contains(&"--can-restart".to_string()),
        "a restart was offered with nothing able to perform one: {:?}",
        world.notifier_saw()
    );
}

#[test]
fn a_launcher_waiting_on_the_application_offers_the_restart() {
    let world = World::new();
    world.install_fake_notifier(1);
    world.install("1.1.0", announcing(), false);
    let mut app = a_running_application();

    xpack_launcher::announce_a_staged_version(&world.paths, Some(&handle(&app)));

    assert!(
        world.notifier_saw().contains(&"--can-restart".to_string()),
        "{:?}",
        world.notifier_saw()
    );
    let _ = app.kill();
    let _ = app.wait();
}

#[test]
fn accepting_asks_the_application_to_close_and_records_the_agreement() {
    let world = World::new();
    world.install_fake_notifier(0);
    world.install("1.1.0", announcing(), false);
    let mut app = a_running_application();
    let application = handle(&app);

    xpack_launcher::announce_a_staged_version(&world.paths, Some(&application));

    assert!(
        application.restart_requested.load(std::sync::atomic::Ordering::SeqCst),
        "the user accepted but nothing recorded it"
    );
    assert!(
        exited_within(&mut app, std::time::Duration::from_secs(5)),
        "the application was never asked to close"
    );
    let _ = app.wait();
}

#[test]
fn declining_leaves_the_application_running() {
    // "Later" has to mean later. An update that closed the application anyway
    // would be the last time a user read one of these dialogs.
    let world = World::new();
    world.install_fake_notifier(1);
    world.install("1.1.0", announcing(), false);
    let mut app = a_running_application();
    let application = handle(&app);

    xpack_launcher::announce_a_staged_version(&world.paths, Some(&application));

    assert!(!application.restart_requested.load(std::sync::atomic::Ordering::SeqCst));
    assert!(
        !exited_within(&mut app, std::time::Duration::from_millis(300)),
        "declining a restart closed the application"
    );
    let _ = app.kill();
    let _ = app.wait();
}

#[test]
fn a_dialog_that_never_opened_never_closes_the_application() {
    // Exit code 3: nothing was shown. Nobody agreed to anything.
    let world = World::new();
    world.install_fake_notifier(3);
    world.install("1.1.0", announcing(), false);
    let mut app = a_running_application();
    let application = handle(&app);

    xpack_launcher::announce_a_staged_version(&world.paths, Some(&application));

    assert!(!application.restart_requested.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!exited_within(&mut app, std::time::Duration::from_millis(300)));
    let _ = app.kill();
    let _ = app.wait();
}

#[test]
fn a_version_staged_by_the_startup_check_is_announced_too() {
    // The check the launcher runs at startup stages versions as readily as a
    // periodic one does. Announcing only what this thread staged itself would
    // leave that one unmentioned until the next check fell due — which, on a
    // three-hour interval, is three hours after it was ready to use.
    let world = World::new();
    world.install_fake_notifier(0);

    // Staged by something other than the periodic checker: no updater ran here.
    world.install("1.1.0", announcing(), false);

    xpack_launcher::announce_a_staged_version(&world.paths, None);

    assert_eq!(world.runs(), 1, "{:?}", world.notifier_saw());
    assert_eq!(world.announced(), Some(v("1.1.0")));
}

#[test]
fn a_launcher_that_will_not_restart_does_not_offer_to() {
    // The launch that follows a restart. Offering again would ask the
    // application to close with nothing left to start it again — the user's
    // application would simply disappear.
    let world = World::new();
    world.install_fake_notifier(1);
    world.install("1.1.0", announcing(), false);

    // This is what `Launcher::without_restart_offers` arranges: the checker
    // thread is given no handle on the application, so no restart is offered.
    xpack_launcher::announce_a_staged_version(&world.paths, None);

    assert_eq!(world.runs(), 1);
    assert!(
        !world.notifier_saw().contains(&"--can-restart".to_string()),
        "a restart was offered by a launcher that will not perform one: {:?}",
        world.notifier_saw()
    );
}

#[test]
fn a_handle_reports_when_its_application_has_gone() {
    // What stops the checker thread outliving the application it watches, and
    // with it a process id the operating system is free to hand to something
    // else.
    let mut app = a_running_application();
    let application = handle(&app);
    assert!(application.is_running());

    application.still_running.store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(!application.is_running());

    let _ = app.kill();
    let _ = app.wait();
}

#[test]
fn the_startup_check_is_not_started_either() {
    // `spawn_updater` asks as well, so an installation told not to check
    // starts no process at all rather than starting one that exits.
    let world = World::new();
    xpack_core::UpdatePolicy::automatic(false)
        .save(&world.paths)
        .expect("the policy to be written");

    assert!(!xpack_launcher::spawn_updater(&world.paths), "the updater was started anyway");
}
