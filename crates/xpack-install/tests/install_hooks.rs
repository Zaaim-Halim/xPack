//! A package's hooks in the install flow: the moments they run at, what a
//! failure at each leaves, and the program that runs them, placed in every
//! installation.
//!
//! These run the real `xpack-hook` from the workspace's build; see the note
//! in `hooks.rs` on when they are skipped.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use xpack_core::manifest::{Application, FormatVersion, LaunchSpec, PayloadSpec, UpdateSpec};
use xpack_core::progress::{ProgressEvent, ProgressReporter};
use xpack_core::state::VersionStatus;
use xpack_core::{Error, InstallPaths, InstallScope, InstallState, Manifest, Platform, Version};
use xpack_install::integration::command::CommandRoots;
use xpack_install::{InstallOptions, Installer, TrustDecision, finish_removal, open_and_verify};
use xpack_package::PackageBuilder;
use xpack_platform::InstallLock;
use xpack_security::KeyPair;

fn engine() -> Option<PathBuf> {
    let test = std::env::current_exe().unwrap();
    let dir = test.parent()?.parent()?;
    let engine = dir.join(format!("xpack-hook{}", std::env::consts::EXE_SUFFIX));
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

/// A hook that appends `<point>;` to `dataDir/marks`, and the cause when it
/// has one.
const MARK: &str = "export function main(ctx) {
    const file = ctx.path(ctx.dataDir, 'marks');
    const before = ctx.file.exists(file) ? ctx.file.read(file) : '';
    const cause = ctx.cause ? '(' + ctx.cause + ')' : '';
    ctx.file.write(file, before + ctx.operation + '.' + ctx.when + cause + ';');
}";

const FAIL: &str = "export function main(ctx) { ctx.log.info('about to fail'); throw new Error('refused by the hook'); }";

struct Fixture {
    dir: tempfile::TempDir,
    key: KeyPair,
    engine: PathBuf,
    commands: CommandRoots,
}

impl Fixture {
    fn new() -> Option<Self> {
        let engine = engine()?;
        let dir = tempfile::tempdir().unwrap();
        // Unique per test, so tests running side by side never share one.
        let unique = dir.path().file_name().unwrap().to_string_lossy().into_owned();
        let commands = CommandRoots {
            bin: dir.path().join("home/.local/bin"),
            environment_key: format!(r"Software\xpack-tests\{unique}"),
            scope: InstallScope::User,
        };
        Some(Self { dir, key: KeyPair::generate().unwrap(), engine, commands })
    }

    fn paths(&self) -> InstallPaths {
        InstallPaths::new(&self.dir.path().join("root"), "com.example.app").unwrap()
    }

    /// A signed package of `version` whose `hooks` name scripts from
    /// `scripts`, each written into its payload.
    fn package(
        &self,
        version: &str,
        hooks: serde_json::Value,
        scripts: &[(&str, &str)],
    ) -> PathBuf {
        self.package_commanding(version, hooks, scripts, None)
    }

    /// As [`Self::package`], asking for the command `command`.
    fn package_commanding(
        &self,
        version: &str,
        hooks: serde_json::Value,
        scripts: &[(&str, &str)],
        command: Option<&str>,
    ) -> PathBuf {
        let payload = self.dir.path().join(format!("src-{version}-{}", rand_suffix()));
        fs::create_dir_all(payload.join("bin")).unwrap();
        fs::write(payload.join("bin/app"), "#!/bin/sh\n").unwrap();
        for (path, text) in scripts {
            let file = payload.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, text).unwrap();
        }
        let manifest = Manifest {
            format_version: FormatVersion::CURRENT,
            application: Application {
                id: "com.example.app".into(),
                name: "Example".into(),
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
            health: xpack_core::HealthSpec::default(),
            signing_key: None,
            desktop: xpack_core::DesktopSpec::default(),
            payload: PayloadSpec::default(),
            created_at: None,
            command: command.map(|name| xpack_core::CommandSpec { name: name.into() }),
            commands: vec![],
            instance: xpack_core::InstanceSpec::default(),
            hooks: serde_json::from_value(hooks).unwrap(),
        };
        let manifest = Manifest { format_version: manifest.required_format_version(), ..manifest };
        let out = payload.with_extension("xpkg");
        PackageBuilder::new(&payload, manifest).build(&out, &self.key).unwrap();
        out
    }

    fn options(&self, scope: InstallScope) -> InstallOptions {
        InstallOptions {
            activate: true,
            scope,
            hook_engine: Some(self.engine.clone()),
            launcher: Some(self.stand_in("launcher")),
            command_roots: Some(CommandRoots { scope, ..self.commands.clone() }),
            ..Default::default()
        }
    }

    fn stand_in(&self, name: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        path
    }

    /// Installs `package` and, after a failed first install, finishes its
    /// removal, as an installer does once it has let go of the lock.
    fn install(
        &self,
        package: &Path,
        options: &InstallOptions,
        progress: &dyn ProgressReporter,
    ) -> xpack_core::Result<xpack_install::Installed> {
        let paths = self.paths();
        let lock = InstallLock::acquire(&paths).unwrap();
        let mut verified =
            open_and_verify(package, &lock, &TrustDecision::Explicit(self.key.public()))?;
        let result = Installer::new(&lock).install_with_progress(&mut verified, options, progress);
        drop(lock);
        if result.is_err() {
            finish_removal(&paths).unwrap();
        }
        result
    }

    /// Where the command `example` is, once written.
    fn command_file(&self) -> PathBuf {
        if cfg!(windows) {
            self.paths().command_dir().join("example.exe")
        } else {
            self.commands.bin.join("example")
        }
    }

    fn marks(&self) -> String {
        fs::read_to_string(self.paths().data_dir().join("marks")).unwrap_or_default()
    }

    fn state(&self) -> InstallState {
        InstallState::load(&self.paths().state_file()).unwrap().value
    }
}

fn rand_suffix() -> String {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    NEXT.fetch_add(1, Ordering::SeqCst).to_string()
}

/// The versions on disk, the active one, whether the launcher is placed, and
/// whether the command is.
type Snapshot = (Vec<String>, Option<Version>, bool, bool);

/// What the installation looked like at the instant each point's hooks
/// started: which versions were on disk, which was active, and whether the
/// launcher had been placed.
struct Moments {
    paths: InstallPaths,
    command: Option<PathBuf>,
    seen: Mutex<Vec<(String, Snapshot)>>,
    lines: Mutex<Vec<String>>,
    /// Whether any of the package was unpacked.
    unpacked: std::sync::atomic::AtomicBool,
}

impl Moments {
    fn new(paths: InstallPaths) -> Self {
        Self {
            paths,
            command: None,
            seen: Mutex::new(Vec::new()),
            lines: Mutex::new(Vec::new()),
            unpacked: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn at(&self, point: &str) -> Snapshot {
        let seen = self.seen.lock().unwrap();
        let (_, snapshot) =
            seen.iter().find(|(p, _)| p == point).unwrap_or_else(|| panic!("{point} never ran"));
        snapshot.clone()
    }

    fn points(&self) -> Vec<String> {
        self.seen.lock().unwrap().iter().map(|(p, _)| p.clone()).collect()
    }
}

impl ProgressReporter for Moments {
    fn report(&self, event: &ProgressEvent) {
        match event {
            ProgressEvent::RunningHooks { point } => {
                let mut versions: Vec<String> = fs::read_dir(self.paths.versions_dir())
                    .map(|dir| {
                        dir.filter_map(Result::ok)
                            .map(|e| e.file_name().to_string_lossy().into_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                versions.sort();
                let active = InstallState::load(&self.paths.state_file())
                    .ok()
                    .and_then(|s| s.value.current_version);
                let state = InstallState::load(&self.paths.state_file()).ok().map(|s| s.value);
                let launcher = state.as_ref().is_some_and(|state| {
                    self.paths.launcher_file_named(&state.binary_names()).is_file()
                });
                let command = self.command.as_ref().is_some_and(|file| file.exists());
                self.seen
                    .lock()
                    .unwrap()
                    .push((point.to_string(), (versions, active, launcher, command)));
            }
            ProgressEvent::ExtractionProgress { .. } => {
                self.unpacked.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            ProgressEvent::HookOutput { point, line } => {
                self.lines.lock().unwrap().push(format!("[{point}] {line}"));
            }
            _ => {}
        }
    }
}

fn hook_failure(error: Error) -> (String, String, Vec<String>) {
    match error {
        Error::HookFailed { point, reason, output, .. } => (point, reason, output),
        other => panic!("not a hook failure: {other}"),
    }
}

const INSTALL_ALL: [(&str, &str); 1] = [("xpack/hooks/mark.js", MARK)];

fn install_hooks_everywhere() -> serde_json::Value {
    serde_json::json!({ "install": [
        { "when": "before", "script": "xpack/hooks/mark.js" },
        { "when": "afterFiles", "script": "xpack/hooks/mark.js" },
        { "when": "after", "script": "xpack/hooks/mark.js" }
    ] })
}

#[test]
fn each_install_moment_finds_the_installation_as_it_says() {
    let Some(fixture) = Fixture::new() else { return };
    let package = fixture.package_commanding(
        "1.0.0",
        install_hooks_everywhere(),
        &INSTALL_ALL,
        Some("example"),
    );
    let mut moments = Moments::new(fixture.paths());
    moments.command = Some(fixture.command_file());
    let installed =
        fixture.install(&package, &fixture.options(InstallScope::User), &moments).unwrap();

    assert_eq!(moments.points(), ["install.before", "install.afterFiles", "install.after"]);
    // Before: nothing of the version written, nothing active.
    assert_eq!(moments.at("install.before"), (vec![], None, false, false));
    // After the files: the version is there, not yet active, no programs,
    // no command.
    let (versions, active, launcher, command) = moments.at("install.afterFiles");
    assert_eq!((versions.len(), active, launcher, command), (1, None, false, false));
    // After: active, with its programs and its command.
    let (_, active, launcher, command) = moments.at("install.after");
    assert_eq!((active, launcher, command), (Some(v("1.0.0")), true, true));

    assert_eq!(fixture.marks(), "install.before;install.afterFiles;install.after;");
    assert!(installed.activated);
    assert!(moments.unpacked.load(std::sync::atomic::Ordering::SeqCst), "unpacking is not seen");
}

#[test]
fn a_failed_install_hook_at_any_moment_leaves_nothing_and_says_why() {
    for failing in ["before", "afterFiles", "after"] {
        let Some(fixture) = Fixture::new() else { return };
        let hooks = serde_json::json!({ "install": [
            { "when": "before", "script": if failing == "before" { "xpack/hooks/fail.js" } else { "xpack/hooks/mark.js" } },
            { "when": "afterFiles", "script": if failing == "afterFiles" { "xpack/hooks/fail.js" } else { "xpack/hooks/mark.js" } },
            { "when": "after", "script": if failing == "after" { "xpack/hooks/fail.js" } else { "xpack/hooks/mark.js" } }
        ] });
        let package = fixture.package(
            "1.0.0",
            hooks,
            &[("xpack/hooks/mark.js", MARK), ("xpack/hooks/fail.js", FAIL)],
        );
        let error = fixture
            .install(&package, &fixture.options(InstallScope::User), &Moments::new(fixture.paths()))
            .unwrap_err();
        let (point, reason, output) = hook_failure(error);
        assert_eq!(point, format!("install.{failing}"));
        assert!(reason.contains("refused by the hook"), "{reason}");
        assert_eq!(output, ["about to fail"]);
        assert!(!fixture.paths().root().exists(), "install.{failing}: something was left behind");
    }
}

#[test]
fn a_script_that_does_not_parse_is_refused_before_any_hook_runs() {
    let Some(fixture) = Fixture::new() else { return };
    let hooks = serde_json::json!({ "install": [
        { "when": "before", "script": "xpack/hooks/mark.js" },
        { "when": "after", "script": "xpack/hooks/broken.js" }
    ] });
    let package = fixture.package(
        "1.0.0",
        hooks,
        &[("xpack/hooks/mark.js", MARK), ("xpack/hooks/broken.js", "export function main( {")],
    );
    let moments = Moments::new(fixture.paths());
    let error =
        fixture.install(&package, &fixture.options(InstallScope::User), &moments).unwrap_err();
    assert!(error.to_string().contains("cannot run"), "{error}");
    assert!(error.to_string().contains("broken.js"), "{error}");
    assert_eq!(moments.points(), Vec::<String>::new(), "a hook ran");
    assert!(!fixture.paths().root().exists());
}

#[test]
fn every_installation_gets_the_program_that_runs_hooks_with_or_without_hooks() {
    let Some(fixture) = Fixture::new() else { return };
    let package = fixture.package("1.0.0", serde_json::json!({}), &[]);
    let installed = fixture
        .install(&package, &fixture.options(InstallScope::User), &Moments::new(fixture.paths()))
        .unwrap();
    assert_eq!(installed.hook_engine, Some(xpack_install::LauncherOutcome::Installed));
    assert!(fixture.paths().hook_engine_file().is_file());
    assert_eq!(fixture.state().hook_interface, Some(xpack_core::hooks::HOOK_INTERFACE));
}

#[cfg(unix)]
#[test]
fn a_package_without_hooks_never_starts_the_program_that_runs_them() {
    use std::os::unix::fs::PermissionsExt;
    let Some(fixture) = Fixture::new() else { return };
    let marker = fixture.dir.path().join("engine-started");
    let fake = fixture.dir.path().join("fake-xpack-hook");
    fs::write(&fake, format!("#!/bin/sh\ntouch '{}'\nexit 0\n", marker.display())).unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let package = fixture.package("1.0.0", serde_json::json!({}), &[]);
    let options = InstallOptions { hook_engine: Some(fake), ..fixture.options(InstallScope::User) };
    fixture.install(&package, &options, &Moments::new(fixture.paths())).unwrap();
    assert!(!marker.exists(), "the engine was started for a package without hooks");
}

#[test]
fn a_package_with_hooks_and_nothing_to_run_them_is_refused_before_anything_is_written() {
    let Some(fixture) = Fixture::new() else { return };
    let package = fixture.package("1.0.0", install_hooks_everywhere(), &INSTALL_ALL);
    let options = InstallOptions { hook_engine: None, ..fixture.options(InstallScope::User) };
    let moments = Moments::new(fixture.paths());
    let error = fixture.install(&package, &options, &moments).unwrap_err();
    assert!(error.to_string().contains("no program to run"), "{error}");
    assert!(!fixture.paths().versions_dir().exists());
    assert!(
        !moments.unpacked.load(std::sync::atomic::Ordering::SeqCst),
        "unpacked before refusing"
    );
}

fn install_then(fixture: &Fixture, first: &Path) {
    fixture
        .install(first, &fixture.options(InstallScope::User), &Moments::new(fixture.paths()))
        .unwrap();
}

#[test]
fn applying_a_version_over_another_runs_its_update_moments_and_never_its_install_ones() {
    let Some(fixture) = Fixture::new() else { return };
    install_then(&fixture, &fixture.package("1.0.0", serde_json::json!({}), &[]));
    let hooks = serde_json::json!({
        "install": "xpack/hooks/mark.js",
        "update": [
            { "when": "before", "script": "xpack/hooks/mark.js" },
            { "when": "after", "script": "xpack/hooks/mark.js" },
            { "when": "confirmed", "script": "xpack/hooks/mark.js" }
        ]
    });
    let package = fixture.package("1.1.0", hooks, &INSTALL_ALL);
    let moments = Moments::new(fixture.paths());
    fixture.install(&package, &fixture.options(InstallScope::User), &moments).unwrap();
    // One user's installation: confirmed waits for the launcher to see it
    // start.
    assert_eq!(moments.points(), ["update.before", "update.after"]);
    assert_eq!(moments.at("update.before").1, Some(v("1.0.0")));
    assert_eq!(moments.at("update.after").1, Some(v("1.1.0")));
    assert_eq!(fixture.marks(), "update.before;update.after;");
}

#[test]
fn a_failed_update_before_hook_leaves_the_old_version_and_runs_the_new_ones_rollback_hooks() {
    let Some(fixture) = Fixture::new() else { return };
    install_then(&fixture, &fixture.package("1.0.0", serde_json::json!({}), &[]));
    let hooks = serde_json::json!({
        "update": { "when": "before", "script": "xpack/hooks/fail.js" },
        "rollback": [
            { "when": "before", "script": "xpack/hooks/mark.js" },
            { "when": "after", "script": "xpack/hooks/mark.js" }
        ]
    });
    let package = fixture.package(
        "1.1.0",
        hooks,
        &[("xpack/hooks/mark.js", MARK), ("xpack/hooks/fail.js", FAIL)],
    );
    let error = fixture
        .install(&package, &fixture.options(InstallScope::User), &Moments::new(fixture.paths()))
        .unwrap_err();
    assert_eq!(hook_failure(error).0, "update.before");
    let state = fixture.state();
    assert_eq!(state.current_version, Some(v("1.0.0")));
    assert_eq!(state.record(&v("1.1.0")).unwrap().status, VersionStatus::Bad);
    assert_eq!(fixture.marks(), "rollback.before(hookFailed);rollback.after(hookFailed);");
}

#[test]
fn a_failed_update_after_hook_rolls_back_to_the_old_version() {
    let Some(fixture) = Fixture::new() else { return };
    install_then(&fixture, &fixture.package("1.0.0", serde_json::json!({}), &[]));
    let hooks = serde_json::json!({
        "update": "xpack/hooks/fail.js",
        "rollback": "xpack/hooks/mark.js"
    });
    let package = fixture.package(
        "1.1.0",
        hooks,
        &[("xpack/hooks/mark.js", MARK), ("xpack/hooks/fail.js", FAIL)],
    );
    let error = fixture
        .install(&package, &fixture.options(InstallScope::User), &Moments::new(fixture.paths()))
        .unwrap_err();
    assert_eq!(hook_failure(error).0, "update.after");
    let state = fixture.state();
    assert_eq!(state.current_version, Some(v("1.0.0")));
    assert!(state.is_bad(&v("1.1.0")));
    assert_eq!(fixture.marks(), "rollback.after(hookFailed);");
}

#[test]
fn for_everyone_an_applied_version_is_confirmed_at_once_and_a_failed_confirmation_keeps_it() {
    let Some(fixture) = Fixture::new() else { return };
    install_then_machine(&fixture, &fixture.package("1.0.0", serde_json::json!({}), &[]));
    let hooks = serde_json::json!({
        "update": [
            { "when": "after", "script": "xpack/hooks/mark.js" },
            { "when": "confirmed", "script": "xpack/hooks/fail.js" }
        ]
    });
    let package = fixture.package(
        "1.1.0",
        hooks,
        &[("xpack/hooks/mark.js", MARK), ("xpack/hooks/fail.js", FAIL)],
    );
    let moments = Moments::new(fixture.paths());
    fixture.install(&package, &fixture.options(InstallScope::Machine), &moments).unwrap();
    assert_eq!(moments.points(), ["update.after", "update.confirmed"]);
    let state = fixture.state();
    assert_eq!(state.current_version, Some(v("1.1.0")));
    assert!(state.is_good(&v("1.1.0")));
}

fn install_then_machine(fixture: &Fixture, first: &Path) {
    fixture
        .install(first, &fixture.options(InstallScope::Machine), &Moments::new(fixture.paths()))
        .unwrap();
}

#[test]
fn staging_a_version_runs_none_of_its_hooks() {
    let Some(fixture) = Fixture::new() else { return };
    install_then(&fixture, &fixture.package("1.0.0", serde_json::json!({}), &[]));
    let package = fixture.package(
        "1.1.0",
        serde_json::json!({ "update": "xpack/hooks/mark.js" }),
        &INSTALL_ALL,
    );
    let options = InstallOptions { activate: false, ..fixture.options(InstallScope::User) };
    let moments = Moments::new(fixture.paths());
    fixture.install(&package, &options, &moments).unwrap();
    assert_eq!(moments.points(), Vec::<String>::new());
    assert_eq!(fixture.state().current_version, Some(v("1.0.0")));
}

#[test]
fn a_version_with_hooks_is_not_staged_where_nothing_could_run_them() {
    let Some(fixture) = Fixture::new() else { return };
    let first = fixture.package("1.0.0", serde_json::json!({}), &[]);
    let options = InstallOptions { hook_engine: None, ..fixture.options(InstallScope::User) };
    fixture.install(&first, &options, &Moments::new(fixture.paths())).unwrap();
    let package = fixture.package(
        "1.1.0",
        serde_json::json!({ "update": "xpack/hooks/mark.js" }),
        &INSTALL_ALL,
    );
    let options = InstallOptions {
        activate: false,
        hook_engine: None,
        ..fixture.options(InstallScope::User)
    };
    let moments = Moments::new(fixture.paths());
    let error = fixture.install(&package, &options, &moments).unwrap_err();
    assert!(error.to_string().contains("cannot run"), "{error}");
    assert!(!fixture.paths().version_dir(&v("1.1.0")).exists());
    assert_eq!(
        fixture.state().current_version,
        Some(v("1.0.0")),
        "a refusal changed the installation"
    );
    assert!(
        !moments.unpacked.load(std::sync::atomic::Ordering::SeqCst),
        "unpacked before refusing"
    );
}

#[test]
fn a_first_installation_cut_short_during_its_hooks_is_undone_by_the_next_run() {
    let Some(fixture) = Fixture::new() else { return };
    let package = fixture.package("1.0.0", install_hooks_everywhere(), &INSTALL_ALL);
    install_then(&fixture, &package);
    // As a power cut before install.after reported back leaves it: the
    // record says it started, and nothing more.
    let record = fixture.paths().hook_record_file();
    let text = fs::read_to_string(&record).unwrap();
    let kept: Vec<&str> = text.lines().collect();
    fs::write(&record, kept[..kept.len() - 1].join("\n") + "\n").unwrap();

    let error = fixture
        .install(&package, &fixture.options(InstallScope::User), &Moments::new(fixture.paths()))
        .unwrap_err();
    assert!(error.to_string().contains("did not finish"), "{error}");
    assert!(!fixture.paths().root().exists(), "the unfinished installation is still there");

    // And the run after is a new installation, whose hooks run.
    let moments = Moments::new(fixture.paths());
    fixture.install(&package, &fixture.options(InstallScope::User), &moments).unwrap();
    assert_eq!(moments.points().len(), 3);
}

#[test]
fn rollback_hooks_run_only_for_a_version_whose_update_hooks_started() {
    let Some(fixture) = Fixture::new() else { return };
    install_then(&fixture, &fixture.package("1.0.0", serde_json::json!({}), &[]));
    let package = fixture.package(
        "1.1.0",
        serde_json::json!({ "rollback": [
            { "when": "before", "script": "xpack/hooks/mark.js" },
            { "when": "after", "script": "xpack/hooks/mark.js" }
        ] }),
        &INSTALL_ALL,
    );
    install_then(&fixture, &package);
    let paths = fixture.paths();
    let lock = InstallLock::acquire(&paths).unwrap();
    let progress = Moments::new(paths.clone());
    let hooks = xpack_install::HookContext {
        engine: Some(&fixture.engine),
        scope: InstallScope::User,
        progress: &progress,
        cancel: None,
    };
    let restored = Installer::new(&lock)
        .roll_back_with_hooks(&hooks, "failedToStart", "it did not start")
        .unwrap();
    assert_eq!(restored, Some(v("1.0.0")));
    assert_eq!(progress.points(), Vec::<String>::new(), "rollback hooks ran without update hooks");
}

#[test]
fn a_hooks_output_reaches_the_progress_reporter_line_by_line_with_its_point() {
    let Some(fixture) = Fixture::new() else { return };
    let package = fixture.package(
        "1.0.0",
        serde_json::json!({ "install": "xpack/hooks/say.js" }),
        &[("xpack/hooks/say.js", "export function main(ctx) { ctx.log.info('service created'); }")],
    );
    let moments = Moments::new(fixture.paths());
    fixture.install(&package, &fixture.options(InstallScope::User), &moments).unwrap();
    assert_eq!(*moments.lines.lock().unwrap(), ["[install.after] service created"]);
}

#[test]
fn a_cancelled_install_hook_is_a_failure_and_leaves_nothing() {
    let Some(fixture) = Fixture::new() else { return };
    let package = fixture.package(
        "1.0.0",
        serde_json::json!({ "install": { "script": "xpack/hooks/loop.js", "timeoutSeconds": 20 } }),
        &[("xpack/hooks/loop.js", "export function main() { for (;;) {} }")],
    );
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let options =
        InstallOptions { cancel: Some(cancel.clone()), ..fixture.options(InstallScope::User) };
    let error = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(500));
            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        fixture.install(&package, &options, &Moments::new(fixture.paths())).unwrap_err()
    });
    assert_eq!(hook_failure(error).1, "cancelled");
    assert!(!fixture.paths().root().exists());
}

#[test]
fn finish_removal_called_on_a_working_installation_removes_nothing() {
    let Some(fixture) = Fixture::new() else { return };
    install_then(&fixture, &fixture.package("1.0.0", serde_json::json!({}), &[]));
    let (removed, _) = finish_removal(&fixture.paths()).unwrap();
    assert!(!removed);
    assert!(fixture.paths().state_file().is_file());
}

#[test]
fn a_version_that_arrived_by_update_is_never_taken_for_an_unfinished_first_installation() {
    let Some(fixture) = Fixture::new() else { return };
    install_then(&fixture, &fixture.package("1.0.0", serde_json::json!({}), &[]));
    // The same hooks block in every release: install hooks declared, never
    // run here, because this version came as an update.
    let hooks =
        serde_json::json!({ "install": "xpack/hooks/mark.js", "update": "xpack/hooks/mark.js" });
    install_then(&fixture, &fixture.package("1.1.0", hooks, &INSTALL_ALL));
    // Pruned down to the one version, as `xpack prune` can leave it.
    let paths = fixture.paths();
    {
        let lock = InstallLock::acquire(&paths).unwrap();
        let mut state = lock.load_state().unwrap().value;
        state.versions.remove("1.0.0");
        state.previous_version = None;
        lock.save_state(&state).unwrap();
        fs::remove_dir_all(paths.version_dir(&v("1.0.0"))).unwrap();
    }
    for activate in [false, true] {
        let next = fixture.package("1.2.0", serde_json::json!({}), &[]);
        let options = InstallOptions { activate, ..fixture.options(InstallScope::User) };
        let result = fixture.install(&next, &options, &Moments::new(paths.clone()));
        if activate {
            result.unwrap();
        } else {
            result.unwrap();
            // Staged now; the next run applies the same version.
            let lock = InstallLock::acquire(&paths).unwrap();
            let mut state = lock.load_state().unwrap().value;
            state.versions.remove("1.2.0");
            lock.save_state(&state).unwrap();
            fs::remove_dir_all(paths.version_dir(&v("1.2.0"))).unwrap();
        }
        assert!(
            paths.version_dir(&v("1.1.0")).is_dir(),
            "activate {activate}: the installation was undone"
        );
    }
    assert_eq!(fixture.state().current_version, Some(v("1.2.0")));
}

#[test]
fn an_unfinished_first_installation_is_left_alone_by_a_run_that_only_stages() {
    let Some(fixture) = Fixture::new() else { return };
    let package = fixture.package("1.0.0", install_hooks_everywhere(), &INSTALL_ALL);
    install_then(&fixture, &package);
    let record = fixture.paths().hook_record_file();
    let text = fs::read_to_string(&record).unwrap();
    let kept: Vec<&str> = text.lines().collect();
    fs::write(&record, kept[..kept.len() - 1].join("\n") + "\n").unwrap();

    let next = fixture.package("1.1.0", serde_json::json!({}), &[]);
    let options = InstallOptions { activate: false, ..fixture.options(InstallScope::User) };
    let error = fixture.install(&next, &options, &Moments::new(fixture.paths())).unwrap_err();
    assert!(error.to_string().contains("did not finish"), "{error}");
    assert!(fixture.paths().version_dir(&v("1.0.0")).is_dir(), "a staging run undid it");
}

/// Applies a version whose update hook at `when` loops, cancels it, and
/// returns the marks left by its other hooks and its rollback hooks.
fn cancel_update_hook(when: &str) -> Option<String> {
    let fixture = Fixture::new()?;
    install_then(&fixture, &fixture.package("1.0.0", serde_json::json!({}), &[]));
    let script = |moment: &str| {
        if moment == when { "xpack/hooks/loop.js" } else { "xpack/hooks/mark.js" }
    };
    let hooks = serde_json::json!({
        "update": [
            { "when": "before", "script": script("before"), "timeoutSeconds": 10 },
            { "when": "after", "script": script("after"), "timeoutSeconds": 10 }
        ],
        "rollback": [
            { "when": "before", "script": "xpack/hooks/mark.js" },
            { "when": "after", "script": "xpack/hooks/mark.js" }
        ]
    });
    let package = fixture.package(
        "1.1.0",
        hooks,
        &[
            ("xpack/hooks/mark.js", MARK),
            ("xpack/hooks/loop.js", "export function main() { for (;;) {} }"),
        ],
    );
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let options =
        InstallOptions { cancel: Some(cancel.clone()), ..fixture.options(InstallScope::User) };
    let error = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(500));
            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        fixture.install(&package, &options, &Moments::new(fixture.paths())).unwrap_err()
    });
    assert_eq!(hook_failure(error).1, "cancelled");
    assert_eq!(fixture.state().current_version, Some(v("1.0.0")));
    Some(fixture.marks())
}

#[test]
fn a_cancelled_update_hook_is_undone_by_rollback_hooks_the_cancel_does_not_stop() {
    let Some(marks) = cancel_update_hook("before") else { return };
    assert_eq!(marks, "rollback.before(hookFailed);rollback.after(hookFailed);");
    let Some(marks) = cancel_update_hook("after") else { return };
    assert_eq!(marks, "update.before;rollback.before(hookFailed);rollback.after(hookFailed);");
}

#[test]
fn running_the_same_installer_twice_runs_its_hooks_once() {
    let Some(fixture) = Fixture::new() else { return };
    let package = fixture.package("1.0.0", install_hooks_everywhere(), &INSTALL_ALL);
    install_then(&fixture, &package);
    let error = fixture
        .install(&package, &fixture.options(InstallScope::User), &Moments::new(fixture.paths()))
        .unwrap_err();
    assert!(error.to_string().contains("already installed"), "{error}");
    assert_eq!(fixture.marks(), "install.before;install.afterFiles;install.after;");
}
