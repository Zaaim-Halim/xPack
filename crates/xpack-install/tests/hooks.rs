//! Running hooks: once, in order, only what was signed, stopped with
//! everything they started, and recorded.
//!
//! These run the real `xpack-hook`, from the workspace's build. `cargo test`
//! builds only this crate's own binaries, so they are skipped, saying so,
//! when it has not been built; CI builds the workspace first.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use xpack_core::hooks::{HookPoint, HookRecord, PointOutcome};
use xpack_core::manifest::{
    Application, FormatVersion, LaunchSpec, PayloadFile, PayloadSpec, UpdateSpec,
};
use xpack_core::{Error, InstallPaths, InstallScope, Manifest, Platform, Version};
use xpack_install::HookRun;
use xpack_platform::InstallLock;

fn engine() -> Option<PathBuf> {
    let test = std::env::current_exe().unwrap();
    let dir = test.parent()?.parent()?;
    let engine = dir.join(format!("xpack-hook{}", std::env::consts::EXE_SUFFIX));
    if engine.is_file() {
        return Some(engine);
    }
    // Where the workspace is always built first, a missing engine is a
    // broken build, never a reason to pass without testing anything.
    assert!(std::env::var_os("CI").is_none(), "{} is not built", engine.display());
    eprintln!("skipped: {} is not built; run `cargo build --workspace` first", engine.display());
    None
}

/// An installation holding version 1.0.0, whose payload is `scripts`, and a
/// manifest naming them in `hooks`.
struct Fixture {
    _dir: tempfile::TempDir,
    paths: InstallPaths,
    manifest: Manifest,
    engine: PathBuf,
    on_line: Option<xpack_install::hooks::LineSink<'static>>,
}

impl Fixture {
    fn new(hooks: serde_json::Value, scripts: &[(&str, &str)]) -> Option<Self> {
        let engine = engine()?;
        let dir = tempfile::tempdir().unwrap();
        let paths =
            InstallPaths::new(&dir.path().canonicalize().unwrap(), "com.example.app").unwrap();
        let version = Version::parse("1.0.0").unwrap();
        let version_dir = paths.version_dir(&version);
        let mut files = Vec::new();
        for (path, text) in scripts {
            let file = version_dir.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, text).unwrap();
            files.push(PayloadFile {
                path: (*path).to_string(),
                size: text.len() as u64,
                sha256: xpack_security::hash::sha256(text.as_bytes()),
                mode: None,
            });
        }
        std::fs::create_dir_all(paths.state_dir()).unwrap();
        let manifest = Manifest {
            format_version: FormatVersion(6),
            application: Application {
                id: "com.example.app".into(),
                name: "Example".into(),
                version,
                description: None,
                publisher: None,
            },
            platform: Platform::host().unwrap(),
            launch: LaunchSpec {
                executable: "bin/app".into(),
                arguments: vec![],
                working_directory: None,
                keep_working_directory: false,
                environment: BTreeMap::from([("APP_MODE".into(), "production".into())]),
            },
            update: UpdateSpec::default(),
            health: xpack_core::HealthSpec::default(),
            signing_key: None,
            desktop: xpack_core::DesktopSpec::default(),
            payload: PayloadSpec { total_size: files.iter().map(|f| f.size).sum(), files },
            created_at: None,
            command: None,
            commands: vec![],
            instance: xpack_core::InstanceSpec::default(),
            hooks: serde_json::from_value(hooks).unwrap(),
        };
        manifest.hooks.validate(&manifest.payload).unwrap();
        Some(Self { _dir: dir, paths, manifest, engine, on_line: None })
    }

    fn run_as(
        &self,
        scope: InstallScope,
        point: &str,
        cancel: Option<&AtomicBool>,
    ) -> xpack_core::Result<()> {
        let lock = InstallLock::acquire(&self.paths).unwrap();
        let run = HookRun {
            engine: &self.engine,
            scope,
            manifest: &self.manifest,
            scripts: &self.paths.version_dir(&self.manifest.application.version),
            from_version: None,
            to_version: Some(&self.manifest.application.version),
            cause: None,
            on_line: self.on_line,
            cancel,
        };
        run.run(&lock, point.parse::<HookPoint>().unwrap())
    }

    fn run(&self, point: &str) -> xpack_core::Result<()> {
        self.run_as(InstallScope::User, point, None)
    }

    fn record(&self) -> HookRecord {
        HookRecord::read(&self.paths.hook_record_file()).unwrap()
    }

    fn outcome(&self, point: &str) -> Option<PointOutcome> {
        self.record().outcome(&self.manifest.application.version, point.parse().unwrap())
    }

    fn data(&self, name: &str) -> PathBuf {
        self.paths.data_dir().join(name)
    }

    fn read_data(&self, name: &str) -> String {
        std::fs::read_to_string(self.data(name)).unwrap_or_default()
    }
}

/// A hook that appends `mark` to `dataDir/marks`.
fn marking(mark: &str) -> String {
    format!(
        "export function main(ctx) {{
            const file = ctx.path(ctx.dataDir, 'marks');
            const before = ctx.file.exists(file) ? ctx.file.read(file) : '';
            ctx.file.write(file, before + '{mark}');
        }}"
    )
}

fn failure(error: Error) -> (String, String, String, Vec<String>) {
    match error {
        Error::HookFailed { point, script, reason, output } => (point, script, reason, output),
        other => panic!("not a hook failure: {other}"),
    }
}

#[test]
fn a_hook_runs_once_and_is_recorded_as_succeeded() {
    let Some(fixture) = Fixture::new(
        serde_json::json!({ "install": "xpack/hooks/a.js" }),
        &[("xpack/hooks/a.js", &marking("a"))],
    ) else {
        return;
    };
    fixture.run("install.after").unwrap();
    assert_eq!(fixture.read_data("marks"), "a");
    assert_eq!(fixture.outcome("install.after"), Some(PointOutcome::Succeeded));

    // Again: not run, whatever asks.
    fixture.run("install.after").unwrap();
    fixture.run("install.after").unwrap();
    assert_eq!(fixture.read_data("marks"), "a", "a hook ran twice");
}

#[test]
fn the_hooks_at_a_point_run_in_the_order_declared() {
    let Some(fixture) = Fixture::new(
        serde_json::json!({ "install": ["xpack/hooks/b.js", "xpack/hooks/a.js"] }),
        &[("xpack/hooks/a.js", &marking("a")), ("xpack/hooks/b.js", &marking("b"))],
    ) else {
        return;
    };
    fixture.run("install.after").unwrap();
    assert_eq!(fixture.read_data("marks"), "ba");
}

#[test]
fn a_point_with_no_hooks_runs_nothing_and_records_nothing() {
    let Some(fixture) = Fixture::new(
        serde_json::json!({ "install": "xpack/hooks/a.js" }),
        &[("xpack/hooks/a.js", &marking("a"))],
    ) else {
        return;
    };
    fixture.run("install.before").unwrap();
    assert!(!fixture.paths.hook_record_file().exists());
}

#[test]
fn the_first_failure_stops_the_rest_for_good_and_says_why() {
    let Some(fixture) = Fixture::new(
        serde_json::json!({ "install": ["xpack/hooks/a.js", "xpack/hooks/fail.js", "xpack/hooks/b.js"] }),
        &[
            ("xpack/hooks/a.js", &marking("a")),
            (
                "xpack/hooks/fail.js",
                "export function main(ctx) { ctx.log.info('creating the service'); throw new Error('access denied'); }",
            ),
            ("xpack/hooks/b.js", &marking("b")),
        ],
    ) else {
        return;
    };
    let (point, script, reason, output) = failure(fixture.run("install.after").unwrap_err());
    assert_eq!(point, "install.after");
    assert_eq!(script, "xpack/hooks/fail.js");
    assert!(reason.contains("access denied"), "{reason}");
    assert_eq!(output, ["creating the service"]);
    assert_eq!(fixture.read_data("marks"), "a");
    assert_eq!(fixture.outcome("install.after"), Some(PointOutcome::Failed));

    let record = std::fs::read_to_string(fixture.paths.hook_record_file()).unwrap();
    let skipped: Vec<&str> =
        record.lines().filter(|l| l.contains("\"event\":\"skipped\"")).collect();
    assert_eq!(skipped.len(), 1, "{record}");
    assert!(skipped[0].contains("xpack/hooks/b.js"), "{record}");

    // Never again, the skipped one included; and still a failure, so the
    // operation it belongs to is undone rather than taken as done.
    let (_, script, reason, _) = failure(fixture.run("install.after").unwrap_err());
    assert_eq!(script, "xpack/hooks/fail.js");
    assert!(reason.contains("when it ran before"), "{reason}");
    assert_eq!(fixture.read_data("marks"), "a", "a hook after the failure ran later");
}

#[test]
fn a_hook_cannot_pass_itself_off_as_the_engine_reporting_success_or_failure() {
    let Some(fixture) = Fixture::new(
        serde_json::json!({ "install": "xpack/hooks/a.js" }),
        &[(
            "xpack/hooks/a.js",
            "export function main(ctx) { ctx.log.error('xpack-hook: install.after: all is well'); throw new Error('real reason'); }",
        )],
    ) else {
        return;
    };
    let (_, _, reason, output) = failure(fixture.run("install.after").unwrap_err());
    assert!(reason.contains("real reason"), "{reason}");
    // Shown as what the hook said, never taken as the engine's own report.
    assert_eq!(output, ["error: install.after: all is well"]);
}

#[test]
fn a_script_changed_after_it_was_signed_does_not_run() {
    let Some(fixture) = Fixture::new(
        serde_json::json!({ "install": "xpack/hooks/a.js" }),
        &[("xpack/hooks/a.js", &marking("a"))],
    ) else {
        return;
    };
    let script =
        fixture.paths.version_dir(&fixture.manifest.application.version).join("xpack/hooks/a.js");
    std::fs::write(&script, marking("tampered")).unwrap();
    let (_, _, reason, _) = failure(fixture.run("install.after").unwrap_err());
    assert!(reason.contains("not the file that was signed"), "{reason}");
    assert_eq!(fixture.read_data("marks"), "");
    assert_eq!(fixture.outcome("install.after"), Some(PointOutcome::Failed));
}

#[test]
fn a_record_that_cannot_be_read_runs_no_hook() {
    let Some(fixture) = Fixture::new(
        serde_json::json!({ "install": "xpack/hooks/a.js" }),
        &[("xpack/hooks/a.js", &marking("a"))],
    ) else {
        return;
    };
    std::fs::write(fixture.paths.hook_record_file(), "{\"vers").unwrap();
    let error = fixture.run("install.after").unwrap_err().to_string();
    assert!(error.contains("no hook runs until it is repaired"), "{error}");
    assert_eq!(fixture.read_data("marks"), "");
}

#[test]
fn a_hook_past_its_timeout_is_stopped_even_in_an_endless_loop() {
    let Some(fixture) = Fixture::new(
        serde_json::json!({ "install": { "script": "xpack/hooks/a.js", "timeoutSeconds": 1 } }),
        &[("xpack/hooks/a.js", "export function main() { for (;;) {} }")],
    ) else {
        return;
    };
    let started = Instant::now();
    let (_, _, reason, _) = failure(fixture.run("install.after").unwrap_err());
    assert!(reason.contains("still running after 1 seconds"), "{reason}");
    assert!(started.elapsed() < Duration::from_secs(4), "{:?}", started.elapsed());
    assert_eq!(fixture.outcome("install.after"), Some(PointOutcome::Failed));
}

#[test]
fn a_cancelled_hook_is_stopped_at_once_and_has_failed() {
    let Some(fixture) = Fixture::new(
        // A timeout short enough that a cancel which never arrives fails the
        // test in seconds, and long enough that a cancel is what stops it.
        serde_json::json!({ "install": { "script": "xpack/hooks/a.js", "timeoutSeconds": 10 } }),
        &[("xpack/hooks/a.js", "export function main() { for (;;) {} }")],
    ) else {
        return;
    };
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(300));
            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        fixture.run_as(InstallScope::User, "install.after", Some(&cancel))
    });
    let (_, _, reason, _) = failure(result.unwrap_err());
    assert_eq!(reason, "cancelled");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(fixture.outcome("install.after"), Some(PointOutcome::Failed));
}

#[cfg(unix)]
fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[cfg(unix)]
#[test]
fn a_hook_stopped_at_its_timeout_takes_everything_it_started_with_it() {
    let Some(fixture) = Fixture::new(
        serde_json::json!({
            "install": { "script": "xpack/hooks/a.js", "timeoutSeconds": 2 },
            "permissions": { "user": { "exec": ["/bin/sh"] } }
        }),
        &[(
            "xpack/hooks/a.js",
            "export function main(ctx) {
                const pid = ctx.path(ctx.dataDir, 'pid');
                ctx.exec('/bin/sh', ['-c', 'sleep 60 & echo $! > \"$1\"; sleep 60', 'sh', pid]);
            }",
        )],
    ) else {
        return;
    };
    let (_, _, reason, _) = failure(fixture.run("install.after").unwrap_err());
    assert!(reason.contains("still running"), "{reason}");
    let pid = fixture.read_data("pid");
    let pid = pid.trim();
    assert_ne!(pid, "");
    let started = Instant::now();
    while alive(pid) && started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!alive(pid), "what the hook started outlived it");
}

#[cfg(unix)]
#[test]
fn a_hooks_programs_get_only_the_environment_built_for_them() {
    let script = "export function main(ctx) {
        const r = ctx.exec('/usr/bin/env', [], {});
        ctx.file.write(ctx.path(ctx.dataDir, 'env'), r.stdout);
    }";
    let hooks = serde_json::json!({
        "install": "xpack/hooks/a.js",
        "permissions": { "user": { "exec": ["/usr/bin/env"] }, "machine": { "exec": ["/usr/bin/env"] } }
    });
    let Some(fixture) = Fixture::new(hooks, &[("xpack/hooks/a.js", script)]) else {
        return;
    };
    fixture.run("install.after").unwrap();
    let env = fixture.read_data("env");
    assert!(env.contains("APP_MODE=production"), "{env}");
    // The test runner's own variables are everywhere in this process's
    // environment; none may come through.
    assert!(!env.contains("CARGO"), "{env}");
    assert!(env.lines().any(|line| line.starts_with("HOME=")), "{env}");
    let temp = env.lines().find_map(|line| line.strip_prefix("TMPDIR=")).unwrap();
    assert!(
        std::path::Path::new(temp)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("xpack-hook-"),
        "{temp}"
    );
}

#[cfg(unix)]
#[test]
fn a_hook_of_an_installation_for_everyone_gets_no_users_home() {
    let script = "export function main(ctx) {
        if (ctx.home !== undefined) throw new Error('home ' + ctx.home);
        const r = ctx.exec('/usr/bin/env', [], {});
        ctx.file.write(ctx.path(ctx.dataDir, 'env'), r.stdout);
    }";
    let hooks = serde_json::json!({
        "install": "xpack/hooks/a.js",
        "permissions": { "machine": { "exec": ["/usr/bin/env"] } }
    });
    let Some(fixture) = Fixture::new(hooks, &[("xpack/hooks/a.js", script)]) else {
        return;
    };
    fixture.run_as(InstallScope::Machine, "install.after", None).unwrap();
    let env = fixture.read_data("env");
    assert!(
        !env.lines().any(|line| line.starts_with("HOME=") || line.starts_with("USERPROFILE=")),
        "{env}"
    );
}

#[test]
fn each_hook_point_of_a_version_runs_on_its_own() {
    let Some(fixture) = Fixture::new(
        serde_json::json!({ "install": [
            { "when": "before", "script": "xpack/hooks/a.js" },
            { "when": "after", "script": "xpack/hooks/b.js" }
        ] }),
        &[("xpack/hooks/a.js", &marking("a")), ("xpack/hooks/b.js", &marking("b"))],
    ) else {
        return;
    };
    fixture.run("install.before").unwrap();
    fixture.run("install.after").unwrap();
    fixture.run("install.before").unwrap();
    assert_eq!(fixture.read_data("marks"), "ab");
}

#[test]
fn a_hook_is_recorded_as_started_before_it_runs_so_a_crash_cannot_run_it_twice() {
    let Some(fixture) = Fixture::new(
        serde_json::json!({ "install": { "script": "xpack/hooks/a.js", "timeoutSeconds": 10 } }),
        &[("xpack/hooks/a.js", "export function main() { for (;;) {} }")],
    ) else {
        return;
    };
    let cancel = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let running =
            scope.spawn(|| fixture.run_as(InstallScope::User, "install.after", Some(&cancel)));
        // While the hook is still running, the record already says it began:
        // a power cut now leaves it marked, and it is never run again.
        let started = Instant::now();
        let mut seen = None;
        while started.elapsed() < Duration::from_secs(5) {
            if fixture.paths.hook_record_file().exists() {
                seen = fixture.outcome("install.after");
                if seen.is_some() {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(seen, Some(PointOutcome::Interrupted), "not recorded before it ran");
        assert!(!running.is_finished(), "the hook ended before the record was looked at");
        cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = running.join().unwrap();
    });
}

/// Appends a line to the record as a run cut short would have left it.
fn record_line(fixture: &Fixture, script: &str, event: xpack_core::hooks::HookEvent) {
    let line = xpack_core::hooks::HookRecordLine {
        version: fixture.manifest.application.version.clone(),
        point: "install.after".parse().unwrap(),
        script: script.into(),
        sha256: fixture.manifest.payload.files.iter().find(|f| f.path == script).unwrap().sha256,
        event,
        at: 1_790_972_643,
        seconds: None,
    };
    xpack_core::hooks::append(&fixture.paths.hook_record_file(), &line).unwrap();
}

#[test]
fn a_hook_interrupted_by_a_crash_is_never_run_again_and_is_a_failure() {
    let Some(fixture) = Fixture::new(
        serde_json::json!({ "install": "xpack/hooks/a.js" }),
        &[("xpack/hooks/a.js", &marking("a"))],
    ) else {
        return;
    };
    record_line(&fixture, "xpack/hooks/a.js", xpack_core::hooks::HookEvent::Started);
    let (_, _, reason, _) = failure(fixture.run("install.after").unwrap_err());
    assert!(reason.contains("when it ran before"), "{reason}");
    assert_eq!(fixture.read_data("marks"), "");
}

#[test]
fn a_crash_between_two_hooks_of_a_point_is_a_failure_and_the_second_never_runs() {
    let Some(fixture) = Fixture::new(
        serde_json::json!({ "install": ["xpack/hooks/a.js", "xpack/hooks/b.js"] }),
        &[("xpack/hooks/a.js", &marking("a")), ("xpack/hooks/b.js", &marking("b"))],
    ) else {
        return;
    };
    record_line(&fixture, "xpack/hooks/a.js", xpack_core::hooks::HookEvent::Started);
    record_line(&fixture, "xpack/hooks/a.js", xpack_core::hooks::HookEvent::Succeeded);
    let (_, script, _, _) = failure(fixture.run("install.after").unwrap_err());
    assert_eq!(script, "xpack/hooks/b.js");
    assert_eq!(fixture.read_data("marks"), "");
}

#[test]
fn each_line_a_hook_writes_is_handed_on_as_it_arrives_with_its_hook_point() {
    static LINES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    fn sink(point: HookPoint, line: &str) {
        LINES.lock().unwrap().push(format!("[{point}] {line}"));
    }
    let Some(mut fixture) = Fixture::new(
        serde_json::json!({ "install": "xpack/hooks/a.js" }),
        &[(
            "xpack/hooks/a.js",
            r"export function main(ctx) { ctx.log.info('one'); ctx.log.warn('two'); ctx.log.info('three\nfour'); }",
        )],
    ) else {
        return;
    };
    fixture.on_line = Some(&sink);
    fixture.run("install.after").unwrap();
    assert_eq!(
        *LINES.lock().unwrap(),
        [
            "[install.after] one",
            "[install.after] warning: two",
            "[install.after] three",
            "[install.after] four"
        ]
    );
}

#[test]
fn a_hook_that_floods_its_output_is_cut_short_and_still_reports_how_it_ended() {
    let Some(fixture) = Fixture::new(
        serde_json::json!({ "install": "xpack/hooks/a.js" }),
        &[(
            "xpack/hooks/a.js",
            "export function main(ctx) {
                ctx.log.info('x'.repeat(10 * 1024 * 1024));
                const line = 'y'.repeat(100);
                for (let i = 0; i < 100000; i++) ctx.log.info(line);
                throw new Error('done flooding');
            }",
        )],
    ) else {
        return;
    };
    let started = Instant::now();
    let (_, _, reason, output) = failure(fixture.run("install.after").unwrap_err());
    assert!(reason.contains("done flooding"), "{reason}");
    assert!(started.elapsed() < Duration::from_secs(60), "{:?}", started.elapsed());
    assert_eq!(output.last().map(String::as_str), Some("(further output not shown)"));
    assert!(output.iter().all(|line| line.len() <= 4 * 1024 + 4), "a line was not cut");
}

#[test]
fn anything_a_hook_logs_is_shown_and_never_fails_it() {
    let Some(fixture) = Fixture::new(
        serde_json::json!({ "install": "xpack/hooks/a.js" }),
        &[(
            "xpack/hooks/a.js",
            r"export function main(ctx) { ctx.log.info('before \uD800 after'); ctx.log.info(42); throw new Error('the end'); }",
        )],
    ) else {
        return;
    };
    let (_, _, reason, output) = failure(fixture.run("install.after").unwrap_err());
    assert!(reason.contains("the end"), "{reason}");
    assert_eq!(output, ["before \u{fffd} after", "42"]);
}

#[test]
fn each_run_gets_a_directory_inside_the_installation_never_the_inherited_temporary_one() {
    let script = "export function main(ctx) {
        ctx.file.write(ctx.path(ctx.dataDir, 'temp'), ctx.tempDir);
    }";
    for scope in [InstallScope::User, InstallScope::Machine] {
        let Some(fixture) = Fixture::new(
            serde_json::json!({ "install": "xpack/hooks/a.js" }),
            &[("xpack/hooks/a.js", script)],
        ) else {
            return;
        };
        fixture.run_as(scope, "install.after", None).unwrap();
        let temp = PathBuf::from(fixture.read_data("temp"));
        let runs = fixture.paths.hook_runs_dir().canonicalize().unwrap();
        assert!(temp.starts_with(&runs), "{scope:?}: {}", temp.display());
        assert!(!temp.exists(), "{scope:?}: the run's directory was left behind");
    }
}

#[cfg(windows)]
#[test]
fn a_hook_runs_a_system_program_with_the_environment_it_is_given() {
    let system = std::env::var_os("SystemRoot").unwrap();
    let program = std::path::Path::new(&system).join("System32").join("whoami.exe");
    let program = program.display().to_string();
    let script = format!(
        "export function main(ctx) {{
            const r = ctx.exec({}, [], {{}});
            if (r.exitCode !== 0) throw new Error('exit ' + r.exitCode + ' ' + r.stderr);
            if (!r.stdout.trim()) throw new Error('no output');
        }}",
        serde_json::to_string(&program).unwrap()
    );
    let hooks = serde_json::json!({
        "install": "xpack/hooks/a.js",
        "permissions": { "user": { "exec": [program] } }
    });
    let Some(fixture) = Fixture::new(hooks, &[("xpack/hooks/a.js", &script)]) else {
        return;
    };
    fixture.run("install.after").unwrap();
}
