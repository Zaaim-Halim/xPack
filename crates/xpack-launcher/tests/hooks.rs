//! A package's hooks at a start: a staged version applied with its update
//! hooks, confirmed after a good start, rolled back with its rollback hooks
//! after a bad one, and an update cut short finished or undone.
//!
//! Real starts of real processes, so Unix-shaped like the rest of the
//! launcher's tests, and the real `xpack-hook` from the workspace's build.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use xpack_core::manifest::{
    Application, FormatVersion, HealthSpec, LaunchSpec, PayloadSpec, UpdateSpec,
};
use xpack_core::state::VersionStatus;
use xpack_core::{InstallPaths, InstallState, Manifest, Platform, Version};
use xpack_install::{InstallOptions, Installer, TrustDecision, open_and_verify};
use xpack_launcher::Launcher;
use xpack_package::PackageBuilder;
use xpack_platform::InstallLock;
use xpack_security::KeyPair;

fn engine() -> Option<PathBuf> {
    let test = std::env::current_exe().unwrap();
    let dir = test.parent()?.parent()?;
    let engine = dir.join("xpack-hook");
    if engine.is_file() {
        return Some(engine);
    }
    assert!(std::env::var_os("CI").is_none(), "{} is not built", engine.display());
    eprintln!("skipped: {} is not built; run `cargo build --workspace` first", engine.display());
    None
}

fn v(text: &str) -> Version {
    Version::parse(text).unwrap()
}

/// Appends `<point>(<cause>);` to `dataDir/marks`.
const MARK: &str = "export function main(ctx) {
    const file = ctx.path(ctx.dataDir, 'marks');
    const before = ctx.file.exists(file) ? ctx.file.read(file) : '';
    const cause = ctx.cause ? '(' + ctx.cause + ')' : '';
    ctx.file.write(file, before + ctx.operation + '.' + ctx.when + cause + ';');
}";

const FAIL: &str = "export function main() { throw new Error('refused by the hook'); }";

/// Keeps working for eight seconds, then marks that it finished.
const BUSY: &str = "export function main(ctx) {
    const end = Date.now() + 8000;
    while (Date.now() < end) {}
    ctx.file.write(ctx.path(ctx.dataDir, 'busy-done'), 'yes');
}";

/// The application: starts well, or exits with a failure at once.
#[derive(Clone, Copy)]
enum App {
    Starts,
    Crashes,
}

struct World {
    dir: tempfile::TempDir,
    key: KeyPair,
    paths: InstallPaths,
    engine: PathBuf,
}

impl World {
    fn new() -> Option<Self> {
        let engine = engine()?;
        let dir = tempfile::tempdir().unwrap();
        let paths = InstallPaths::new(dir.path(), "com.example.app").unwrap();
        Some(Self { dir, key: KeyPair::generate().unwrap(), paths, engine })
    }

    fn package(&self, version: &str, app: App, hooks: serde_json::Value) -> PathBuf {
        let payload = self.dir.path().join(format!("src-{version}"));
        fs::create_dir_all(payload.join("bin")).unwrap();
        fs::create_dir_all(payload.join("xpack/hooks")).unwrap();
        let script = match app {
            App::Starts => "#!/bin/sh\nexit 0\n",
            App::Crashes => "#!/bin/sh\nexit 3\n",
        };
        let binary = payload.join("bin/app");
        fs::write(&binary, script).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::write(payload.join("xpack/hooks/mark.js"), MARK).unwrap();
        fs::write(payload.join("xpack/hooks/fail.js"), FAIL).unwrap();
        fs::write(payload.join("xpack/hooks/busy.js"), BUSY).unwrap();
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
                environment: BTreeMap::new(),
            },
            update: UpdateSpec::default(),
            health: HealthSpec { startup_timeout_seconds: 5, require_startup_report: false },
            signing_key: None,
            desktop: xpack_core::DesktopSpec::default(),
            payload: PayloadSpec::default(),
            created_at: None,
            command: None,
            commands: Vec::new(),
            instance: xpack_core::InstanceSpec::default(),
            hooks: serde_json::from_value(hooks).unwrap(),
        };
        let manifest = Manifest { format_version: manifest.required_format_version(), ..manifest };
        let out = self.dir.path().join(format!("app-{version}.xpkg"));
        PackageBuilder::new(&payload, manifest).build(&out, &self.key).unwrap();
        out
    }

    fn install(&self, package: &Path, activate: bool) {
        let lock = InstallLock::acquire(&self.paths).unwrap();
        let mut verified =
            open_and_verify(package, &lock, &TrustDecision::Explicit(self.key.public())).unwrap();
        let options = InstallOptions {
            activate,
            hook_engine: Some(self.engine.clone()),
            ..Default::default()
        };
        Installer::new(&lock).install(&mut verified, &options).unwrap();
    }

    /// Version 1.0.0, without hooks, active and good; then `version` staged.
    fn staged(&self, version: &str, app: App, hooks: serde_json::Value) {
        self.install(&self.package("1.0.0", App::Starts, serde_json::json!({})), true);
        self.install(&self.package(version, app, hooks), false);
    }

    fn launch(&self) -> xpack_launcher::Outcome {
        Launcher::for_application_dir(self.paths.root()).launch(&[], false).unwrap()
    }

    fn marks(&self) -> String {
        fs::read_to_string(self.paths.data_dir().join("marks")).unwrap_or_default()
    }

    fn state(&self) -> InstallState {
        InstallState::load(&self.paths.state_file()).unwrap().value
    }
}

fn every_update_moment() -> serde_json::Value {
    serde_json::json!({
        "update": [
            { "when": "before", "script": "xpack/hooks/mark.js" },
            { "when": "after", "script": "xpack/hooks/mark.js" },
            { "when": "confirmed", "script": "xpack/hooks/mark.js" }
        ],
        "rollback": [
            { "when": "before", "script": "xpack/hooks/mark.js" },
            { "when": "after", "script": "xpack/hooks/mark.js" }
        ]
    })
}

#[test]
fn a_staged_version_is_applied_with_its_hooks_and_confirmed_once_after_a_good_start() {
    let Some(world) = World::new() else { return };
    world.staged("1.1.0", App::Starts, every_update_moment());

    let outcome = world.launch();
    assert_eq!(outcome.version, v("1.1.0"));
    assert_eq!(world.marks(), "update.before;update.after;update.confirmed;");
    assert_eq!(world.state().record(&v("1.1.0")).unwrap().status, VersionStatus::Good);

    // Later starts run none of them again.
    world.launch();
    world.launch();
    assert_eq!(world.marks(), "update.before;update.after;update.confirmed;");
}

#[test]
fn a_version_that_fails_to_start_is_rolled_back_with_its_rollback_hooks_and_never_confirmed() {
    let Some(world) = World::new() else { return };
    world.staged("1.1.0", App::Crashes, every_update_moment());

    let outcome = world.launch();
    assert_eq!(outcome.rolled_back_to, Some(v("1.0.0")));
    assert_eq!(
        world.marks(),
        "update.before;update.after;rollback.before(failedToStart);rollback.after(failedToStart);"
    );
    assert!(world.state().is_bad(&v("1.1.0")));
}

#[test]
fn a_failed_update_before_hook_at_a_start_keeps_the_old_version_and_starts_it() {
    let Some(world) = World::new() else { return };
    let hooks = serde_json::json!({
        "update": { "when": "before", "script": "xpack/hooks/fail.js" },
        "rollback": [
            { "when": "before", "script": "xpack/hooks/mark.js" },
            { "when": "after", "script": "xpack/hooks/mark.js" }
        ]
    });
    world.staged("1.1.0", App::Starts, hooks);

    let outcome = world.launch();
    assert_eq!(outcome.version, v("1.0.0"));
    assert_eq!(world.marks(), "rollback.before(hookFailed);rollback.after(hookFailed);");
    assert!(world.state().is_bad(&v("1.1.0")));
}

#[test]
fn a_failed_update_after_hook_at_a_start_rolls_back_and_starts_the_old_version() {
    let Some(world) = World::new() else { return };
    let hooks = serde_json::json!({
        "update": [
            { "when": "after", "script": "xpack/hooks/fail.js" },
            { "when": "confirmed", "script": "xpack/hooks/mark.js" }
        ],
        "rollback": { "when": "after", "script": "xpack/hooks/mark.js" }
    });
    world.staged("1.1.0", App::Starts, hooks);

    let outcome = world.launch();
    assert_eq!(outcome.version, v("1.0.0"));
    assert_eq!(world.marks(), "rollback.after(hookFailed);");
    assert!(world.state().is_bad(&v("1.1.0")));
}

/// Activates the staged 1.1.0 without running hooks, as a machine stopped
/// between activation and `update.after` leaves it.
fn activate_without_hooks(world: &World) {
    let lock = InstallLock::acquire(&world.paths).unwrap();
    Installer::new(&lock).activate(&v("1.1.0"), false).unwrap();
}

/// Appends a line for 1.1.0's `mark.js` at `point` to the record.
fn record(world: &World, point: &str, event: xpack_core::hooks::HookEvent) {
    let manifest =
        Manifest::from_slice(&fs::read(world.paths.version_manifest_file(&v("1.1.0"))).unwrap())
            .unwrap();
    let sha256 =
        manifest.payload.files.iter().find(|f| f.path == "xpack/hooks/mark.js").unwrap().sha256;
    xpack_core::hooks::append(
        &world.paths.hook_record_file(),
        &xpack_core::hooks::HookRecordLine {
            version: v("1.1.0"),
            point: point.parse().unwrap(),
            script: "xpack/hooks/mark.js".into(),
            sha256,
            event,
            at: 1_790_972_643,
            seconds: None,
        },
    )
    .unwrap();
}

#[test]
fn update_after_hooks_that_never_ran_run_at_the_next_start() {
    let Some(world) = World::new() else { return };
    world.staged("1.1.0", App::Starts, every_update_moment());
    // `update.before` succeeded, the version was made active, and the
    // machine stopped before `update.after` began.
    record(&world, "update.before", xpack_core::hooks::HookEvent::Started);
    record(&world, "update.before", xpack_core::hooks::HookEvent::Succeeded);
    activate_without_hooks(&world);

    let outcome = world.launch();
    assert_eq!(outcome.version, v("1.1.0"));
    // Only what had not run: never `update.before` a second time.
    assert_eq!(world.marks(), "update.after;update.confirmed;");
}

#[test]
fn update_after_hooks_cut_short_roll_the_version_back_at_the_next_start() {
    let Some(world) = World::new() else { return };
    world.staged("1.1.0", App::Starts, every_update_moment());
    // As a power cut while `update.after` ran leaves the record.
    record(&world, "update.before", xpack_core::hooks::HookEvent::Started);
    record(&world, "update.before", xpack_core::hooks::HookEvent::Succeeded);
    record(&world, "update.after", xpack_core::hooks::HookEvent::Started);
    activate_without_hooks(&world);

    let outcome = world.launch();
    assert_eq!(outcome.version, v("1.0.0"));
    // Its update hooks had started, so its rollback hooks run; the update
    // hooks themselves never run again.
    assert_eq!(world.marks(), "rollback.before(hookFailed);rollback.after(hookFailed);");
    assert!(world.state().is_bad(&v("1.1.0")));
}

#[test]
fn an_ordinary_start_never_starts_the_program_that_runs_hooks() {
    use std::os::unix::fs::PermissionsExt;
    let Some(world) = World::new() else { return };
    world.staged("1.1.0", App::Starts, every_update_moment());
    world.launch();
    // From here on, any start of xpack-hook leaves a mark.
    let marker = world.dir.path().join("engine-started");
    let engine = world.paths.hook_engine_file();
    fs::write(&engine, format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display())).unwrap();
    fs::set_permissions(&engine, fs::Permissions::from_mode(0o755)).unwrap();

    world.launch();
    world.launch();
    assert!(!marker.exists(), "an ordinary start ran xpack-hook");
}

#[test]
fn a_second_start_does_not_wait_for_update_confirmed_hooks() {
    let Some(world) = World::new() else { return };
    let hooks = serde_json::json!({
        "update": { "when": "confirmed", "script": "xpack/hooks/busy.js", "timeoutSeconds": 60 }
    });
    world.staged("1.1.0", App::Starts, hooks);
    std::thread::scope(|scope| {
        let first = scope.spawn(|| world.launch());
        // Long enough for the first start to have committed the version and
        // be running its confirmed hook.
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert!(!world.paths.data_dir().join("busy-done").exists(), "the hook already ended");
        let started = std::time::Instant::now();
        let second = world.launch();
        assert_eq!(second.version, v("1.1.0"));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(4),
            "the second start waited {:?} for the hook",
            started.elapsed()
        );
        first.join().unwrap();
    });
    assert!(world.paths.data_dir().join("busy-done").exists(), "the confirmed hook did not finish");
}
