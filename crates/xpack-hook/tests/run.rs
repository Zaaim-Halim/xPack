//! The `xpack-hook` program, run as the installer runs it: a request on
//! standard input, an exit code back. Each attack a hook might make is made
//! here, and each must end in a refusal, never in the thing attempted.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const HOOK: &str = env!("CARGO_BIN_EXE_xpack-hook");

/// An installation for one user, laid out as xPack lays one out, with a hook
/// script in its version 1.0.0.
struct Installation {
    _dir: tempfile::TempDir,
    home: PathBuf,
    app: PathBuf,
    version: PathBuf,
    data: PathBuf,
    temp: PathBuf,
}

impl Installation {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let home = root.join("home");
        let app = home.join("apps/com.example.app");
        let version = app.join("versions/1.0.0");
        let data = app.join("data");
        let temp = root.join("tmp");
        for d in [&version, &data, &temp, &app.join("state"), &app.join("config")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(app.join("state/hooks.jsonl"), "").unwrap();
        std::fs::write(app.join("config/seal.key"), "the seal key").unwrap();
        std::fs::write(app.join("xpack-launcher"), "launcher").unwrap();
        std::fs::write(version.join("readme.txt"), "shipped").unwrap();
        Self { _dir: dir, home, app, version, data, temp }
    }

    fn request(&self, script: &str) -> Value {
        let path = self.version.join("hook.js");
        std::fs::write(&path, script).unwrap();
        json!({
            "script": path,
            "point": "install.after",
            "toVersion": "1.0.0",
            "scope": "user",
            "applicationDir": self.app,
            "versionDir": self.version,
            "dataDir": self.data,
            "logDir": self.app.join("state/logs"),
            "tempDir": self.temp,
            "home": self.home,
            "timeoutSeconds": 20,
        })
    }

    /// Runs `script` with `permissions` and returns its exit code and what
    /// it wrote to standard error.
    fn run(&self, script: &str, permissions: Value) -> (i32, String) {
        let mut request = self.request(script);
        request["permissions"] = permissions;
        run(&request, &[])
    }
}

fn run(request: &Value, environment: &[(&str, &str)]) -> (i32, String) {
    run_raw(&request.to_string(), environment)
}

fn run_raw(input: &str, environment: &[(&str, &str)]) -> (i32, String) {
    let mut child = Command::new(HOOK)
        .envs(environment.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    (output.status.code().unwrap_or(-1), String::from_utf8_lossy(&output.stderr).into_owned())
}

fn none() -> Value {
    json!({})
}

fn js(path: &Path) -> String {
    serde_json::to_string(&path.display().to_string()).unwrap()
}

// --- how a hook ends ---

#[test]
fn a_hook_that_returns_succeeds_and_one_that_resolves_does_too() {
    let install = Installation::new();
    assert_eq!(install.run("export function main(ctx) {}", none()).0, 0);
    assert_eq!(install.run("export async function main(ctx) { await null; }", none()).0, 0);
}

#[test]
fn a_hook_that_throws_or_rejects_fails_with_its_reason() {
    let install = Installation::new();
    let (code, err) = install.run("export function main() { throw new Error('no disk'); }", none());
    assert_eq!(code, 1);
    assert!(err.contains("no disk"), "{err}");
    let (code, err) =
        install.run("export async function main() { throw new Error('later'); }", none());
    assert_eq!(code, 1);
    assert!(err.contains("later"), "{err}");
    let (code, err) = install.run("export function main() { throw 'a string'; }", none());
    assert_eq!(code, 1);
    assert!(err.contains("a string"), "{err}");
}

#[test]
fn a_script_with_no_main_or_that_does_not_parse_fails() {
    let install = Installation::new();
    let (code, err) = install.run("export function start() {}", none());
    assert_eq!(code, 1);
    assert!(err.contains("no function main"), "{err}");
    assert_eq!(install.run("export function main( {", none()).0, 1);
}

#[test]
fn a_hook_is_told_where_and_why_it_runs() {
    let install = Installation::new();
    let script = r#"export function main(ctx) {
        const facts = [ctx.operation, ctx.when, ctx.toVersion, ctx.scope, typeof ctx.fromVersion];
        if (facts.join() !== "install,after,1.0.0,user,undefined") throw new Error(facts.join());
        if (!ctx.platform.os || !ctx.platform.arch) throw new Error("no platform");
        if (ctx.programData !== undefined) throw new Error("programData for one user");
    }"#;
    let (code, err) = install.run(script, none());
    assert_eq!(code, 0, "{err}");
}

#[test]
fn a_request_that_cannot_be_acted_on_is_refused_before_anything_runs() {
    let install = Installation::new();
    assert_eq!(run_raw("not json", &[]).0, 3);
    let mut unknown = install.request("export function main() {}");
    unknown["extra"] = json!(true);
    assert_eq!(run(&unknown, &[]).0, 3);
    let mut relative = install.request("export function main() {}");
    relative["dataDir"] = json!("data");
    assert_eq!(run(&relative, &[]).0, 3);
    let mut elsewhere = install.request("export function main() {}");
    elsewhere["script"] = json!(install.data.join("hook.js"));
    assert_eq!(run(&elsewhere, &[]).0, 3);
}

// --- what cannot run away ---

#[test]
fn an_endless_loop_is_stopped_at_its_deadline() {
    let install = Installation::new();
    let mut request = install.request("export function main() { for (;;) {} }");
    request["timeoutSeconds"] = json!(1);
    let started = Instant::now();
    assert_eq!(run(&request, &[]).0, 2);
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
}

#[test]
fn a_hook_that_eats_memory_fails_rather_than_taking_the_machine() {
    let install = Installation::new();
    let script =
        "export function main() { const a = []; for (;;) a.push(new Array(1e6).fill(1)); }";
    let (code, err) = install.run(script, none());
    assert_eq!(code, 1, "{err}");
}

#[test]
fn runaway_recursion_fails_rather_than_crashing() {
    let install = Installation::new();
    let (code, err) =
        install.run("function f() { return f() + 1; } export function main() { f(); }", none());
    assert_eq!(code, 1, "{err}");
}

#[test]
fn a_promise_that_can_never_settle_fails_at_once_rather_than_hanging() {
    let install = Installation::new();
    let started = Instant::now();
    let (code, err) =
        install.run("export async function main() { await new Promise(() => {}); }", none());
    assert_eq!(code, 1);
    assert!(err.contains("can never settle"), "{err}");
    let (code, _) = install.run("await new Promise(() => {}); export function main() {}", none());
    assert_eq!(code, 1);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn a_hook_is_one_file_and_cannot_import_another() {
    let install = Installation::new();
    std::fs::write(install.version.join("other.js"), "export const x = 1;").unwrap();
    assert_eq!(
        install.run("import { x } from './other.js'; export function main() {}", none()).0,
        1
    );
    let (code, _) =
        install.run("export async function main() { await import('./other.js'); }", none());
    assert_eq!(code, 1);
    let (code, err) =
        install.run("export function main() { if (typeof require !== 'undefined') throw new Error('require'); }", none());
    assert_eq!(code, 0, "{err}");
}

#[test]
fn a_hook_cannot_replace_what_ctx_checks_with_its_own() {
    let install = Installation::new();
    let script = r"export function main(ctx) {
        const write = ctx.file.write, file = ctx.file, exec = ctx.exec;
        try { ctx.file.write = () => {}; } catch {}
        try { ctx.file = {}; } catch {}
        try { ctx.exec = () => ({ exitCode: 0 }); } catch {}
        try { delete ctx.file.read; } catch {}
        if (ctx.file.write !== write) throw new Error('ctx.file.write was replaced');
        if (ctx.file !== file) throw new Error('ctx.file was replaced');
        if (ctx.exec !== exec) throw new Error('ctx.exec was replaced');
        if (typeof ctx.file.read !== 'function') throw new Error('ctx.file.read was removed');
    }";
    let (code, err) = install.run(script, none());
    assert_eq!(code, 0, "{err}");
}

// --- what a hook may touch ---

fn write_attempt(path: &Path) -> String {
    format!("export function main(ctx) {{ ctx.file.write({}, 'x'); }}", js(path))
}

fn read_attempt(path: &Path) -> String {
    format!("export function main(ctx) {{ ctx.file.read({}); }}", js(path))
}

#[test]
fn a_hook_reads_its_version_and_writes_its_data() {
    let install = Installation::new();
    let script = format!(
        "export function main(ctx) {{
            const text = ctx.file.read(ctx.path(ctx.versionDir, 'readme.txt'));
            ctx.file.makeDir(ctx.path(ctx.dataDir, 'cache'));
            ctx.file.write(ctx.path(ctx.dataDir, 'cache', 'copy.txt'), text);
            ctx.file.write(ctx.path(ctx.tempDir, 'scratch'), 'x');
            ctx.file.makeDir({});
            ctx.file.write({}, 'mine');
        }}",
        js(&install.home.join(".config")),
        js(&install.home.join(".config/app.conf"))
    );
    let (code, err) = install.run(&script, json!({ "write": ["{home}/.config"] }));
    assert_eq!(code, 0, "{err}");
    assert_eq!(std::fs::read_to_string(install.data.join("cache/copy.txt")).unwrap(), "shipped");
    assert_eq!(std::fs::read_to_string(install.home.join(".config/app.conf")).unwrap(), "mine");
}

#[test]
fn not_even_a_hook_allowed_the_whole_home_may_touch_xpacks_own_files() {
    // A per-user installation sits in the user's home, so `{home}` contains
    // it. That must not reach its versions, its records or its programs.
    let install = Installation::new();
    let everything = json!({ "write": ["{home}"] });
    let targets = [
        install.version.join("readme.txt"),
        install.version.join("new.js"),
        install.app.join("state/hooks.jsonl"),
        install.app.join("config/seal.key"),
        install.app.join("xpack-launcher"),
        install.app.join("xpack-hook"),
    ];
    for target in &targets {
        let (code, err) = install.run(&write_attempt(target), everything.clone());
        assert_eq!(code, 1, "{} was written", target.display());
        assert!(err.contains("belongs to xPack"), "{err}");
    }
    for target in [install.app.join("config/seal.key"), install.app.join("state/hooks.jsonl")] {
        let (code, err) = install.run(&read_attempt(&target), everything.clone());
        assert_eq!(code, 1, "{} was read", target.display());
        assert!(err.contains("belongs to xPack"), "{err}");
    }
    assert_eq!(
        std::fs::read_to_string(install.app.join("config/seal.key")).unwrap(),
        "the seal key"
    );
    assert_eq!(std::fs::read_to_string(install.version.join("readme.txt")).unwrap(), "shipped");
}

#[test]
fn dot_dot_does_not_step_out_of_an_allowed_place() {
    let install = Installation::new();
    let sneaky = install.data.join("../state/hooks.jsonl");
    let (code, err) = install.run(&write_attempt(&sneaky), none());
    assert_eq!(code, 1);
    assert!(err.contains("belongs to xPack"), "{err}");
    let outside = install.data.join("../../../../elsewhere.txt");
    assert_eq!(install.run(&write_attempt(&outside), none()).0, 1);
}

#[test]
fn a_hook_writes_nowhere_its_package_did_not_declare() {
    let install = Installation::new();
    let (code, err) = install.run(&write_attempt(&install.home.join(".bashrc")), none());
    assert_eq!(code, 1);
    assert!(err.contains("not a place this hook may write"), "{err}");
    assert!(!install.home.join(".bashrc").exists());
}

#[test]
fn an_allowed_place_may_be_written_in_but_never_removed_whole() {
    let install = Installation::new();
    std::fs::write(install.home.join("precious"), "x").unwrap();
    for place in [&install.home, &install.data] {
        let script = format!("export function main(ctx) {{ ctx.file.remove({}); }}", js(place));
        let (code, err) = install.run(&script, json!({ "write": ["{home}"] }));
        assert_eq!(code, 1, "{} was removed", place.display());
        assert!(err.contains("not replace"), "{err}");
    }
    assert!(install.home.join("precious").exists());
}

#[test]
fn a_declaration_outside_the_users_home_is_refused_before_the_hook_runs() {
    let install = Installation::new();
    let mut request = install.request("export function main() {}");
    request["permissions"] = json!({ "write": ["/etc/systemd/system"] });
    let (code, err) = run(&request, &[]);
    assert_eq!(code, 1);
    assert!(err.contains("not under {home}"), "{err}");
}

#[cfg(unix)]
#[test]
fn a_link_does_not_lead_a_hook_out_of_an_allowed_place() {
    let install = Installation::new();
    std::os::unix::fs::symlink(install.app.join("state"), install.data.join("records")).unwrap();
    let (code, err) =
        install.run(&write_attempt(&install.data.join("records/hooks.jsonl")), none());
    assert_eq!(code, 1);
    assert!(err.contains("belongs to xPack"), "{err}");

    // A link to nothing, partway along: what it would create is somewhere
    // the check never saw.
    std::os::unix::fs::symlink(install.home.join("not-yet"), install.data.join("dangling"))
        .unwrap();
    let (code, err) = install.run(&write_attempt(&install.data.join("dangling/x/y")), none());
    assert_eq!(code, 1);
    assert!(err.contains("link to nothing"), "{err}");
    assert!(!install.home.join("not-yet").exists());

    // A declared place that is a link out of the home is refused too.
    std::os::unix::fs::symlink(install.app.join("state"), install.home.join(".config")).unwrap();
    let mut request = install.request("export function main() {}");
    request["permissions"] = json!({ "write": ["{home}/.config"] });
    let (code, err) = run(&request, &[]);
    assert_eq!(code, 0, "a link inside the home to the installation is still the home: {err}");

    let outside = install.temp.parent().unwrap().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, install.home.join(".local")).unwrap();
    let mut request = install.request("export function main() {}");
    request["permissions"] = json!({ "write": ["{home}/.local"] });
    let (code, err) = run(&request, &[]);
    assert_eq!(code, 1, "a declared place that leads out of the home was allowed");
    assert!(err.contains("outside this user's home"), "{err}");
}

#[test]
fn an_installation_for_everyone_keeps_xpacks_files_from_a_place_that_contains_them() {
    let install = Installation::new();
    let mut request = install.request(&write_attempt(&install.app.join("state/hooks.jsonl")));
    request["scope"] = json!("machine");
    request["home"] = Value::Null;
    request["permissions"] = json!({ "write": [install.home] });
    let (code, err) = run(&request, &[]);
    assert_eq!(code, 1);
    assert!(err.contains("belongs to xPack"), "{err}");
}

// --- what a hook may run ---

fn exec_attempt(program: &str, options: &str) -> String {
    format!(
        "export function main(ctx) {{ const r = ctx.exec({}, [], {options}); ctx.log.info('exit ' + r.exitCode); }}",
        serde_json::to_string(program).unwrap()
    )
}

#[test]
fn a_hook_runs_a_declared_program_and_reads_how_it_ended() {
    let install = Installation::new();
    // `xpack-hook` itself, given nothing on its input, refuses with 3.
    let (code, err) = install.run(&exec_attempt(HOOK, "{}"), json!({ "exec": [HOOK] }));
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("exit 3"), "{err}");
}

#[test]
fn a_program_the_package_did_not_declare_is_refused() {
    let install = Installation::new();
    let (code, err) = install.run(&exec_attempt(HOOK, "{}"), none());
    assert_eq!(code, 1);
    assert!(err.contains("not a program this package lets its hooks run"), "{err}");
}

#[test]
fn a_hook_for_one_user_never_runs_an_elevation_program_whatever_it_declares() {
    let install = Installation::new();
    for program in ["sudo", "RUNAS.EXE", "pkexec", "/usr/bin/sudo"] {
        let (code, err) = install.run(&exec_attempt(program, "{}"), json!({ "exec": [program] }));
        assert_eq!(code, 1, "{program}");
        assert!(err.contains("administrator rights"), "{err}");
    }
}

#[test]
fn a_bare_name_is_found_where_the_installer_looks_not_where_the_hook_says() {
    let install = Installation::new();
    let bin = Path::new(HOOK).parent().unwrap();
    let name = Path::new(HOOK).file_name().unwrap().to_str().unwrap();
    let mut request = install.request(&exec_attempt(name, "{}"));
    request["permissions"] = json!({ "exec": [name] });
    let (code, err) = run(&request, &[("PATH", bin.to_str().unwrap())]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("exit 3"), "{err}");

    for variable in ["PATH", "Path", "LD_PRELOAD", "DYLD_INSERT_LIBRARIES", "XPACK_PASSWORD"] {
        let options = format!("{{ env: {{ {variable}: {} }} }}", js(&install.data));
        let mut request = install.request(&exec_attempt(name, &options));
        request["permissions"] = json!({ "exec": [name] });
        let (code, err) = run(&request, &[("PATH", bin.to_str().unwrap())]);
        assert_eq!(code, 1, "{variable} was accepted");
        assert!(err.contains("may not set it"), "{err}");
    }
}

#[cfg(unix)]
#[test]
fn a_program_that_outlives_its_time_is_stopped() {
    let install = Installation::new();
    let script =
        "export function main(ctx) { ctx.exec('/bin/sleep', ['30'], { timeoutSeconds: 1 }); }";
    let started = Instant::now();
    let (code, err) = install.run(script, json!({ "exec": ["/bin/sleep"] }));
    assert_eq!(code, 1);
    assert!(err.contains("ran out of time"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[cfg(unix)]
#[test]
fn a_program_that_leaves_something_holding_its_output_does_not_hold_the_hook() {
    let install = Installation::new();
    let script = "export function main(ctx) { ctx.exec('/bin/sh', ['-c', 'sleep 8 &'], { timeoutSeconds: 1 }); }";
    let started = Instant::now();
    let (code, err) = install.run(script, json!({ "exec": ["/bin/sh"] }));
    assert_eq!(code, 1);
    assert!(err.contains("ran out of time"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
}

#[cfg(unix)]
#[test]
fn no_secret_of_the_installers_reaches_a_program_a_hook_runs() {
    let install = Installation::new();
    let script = "export function main(ctx) {
        const r = ctx.exec('/usr/bin/env', [], {});
        ctx.log.info('ENV<' + r.stdout + '>');
    }";
    let mut request = install.request(script);
    request["permissions"] = json!({ "exec": ["/usr/bin/env"] });
    request["environment"] = json!({ "APP_MODE": "production", "XPACK_CHANNEL": "hidden" });
    let (code, err) = run(
        &request,
        &[
            ("XPACK_PASSWORD", "hunter2"),
            ("DEPLOY_TOKEN", "hunter2"),
            ("AWS_SECRET_ACCESS_KEY", "hunter2"),
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert!(!err.contains("hunter2"), "{err}");
    assert!(!err.contains("hidden"), "{err}");
    assert!(err.contains("APP_MODE=production"), "{err}");
}

#[cfg(unix)]
#[test]
fn removing_or_moving_a_link_acts_on_the_link_never_on_what_it_points_to() {
    let install = Installation::new();
    let documents = install.home.join("Documents");
    std::fs::create_dir_all(&documents).unwrap();
    std::fs::write(documents.join("thesis.txt"), "years of work").unwrap();
    std::os::unix::fs::symlink(&documents, install.data.join("cache")).unwrap();
    std::os::unix::fs::symlink(&documents, install.data.join("old")).unwrap();
    let script = format!(
        "export function main(ctx) {{ ctx.file.remove({}); ctx.file.move({}, {}); }}",
        js(&install.data.join("cache")),
        js(&install.data.join("old")),
        js(&install.data.join("renamed")),
    );
    let (code, err) = install.run(&script, json!({ "write": ["{home}"] }));
    assert_eq!(code, 0, "{err}");
    assert!(
        std::fs::symlink_metadata(install.data.join("cache")).is_err(),
        "the link is still there"
    );
    assert!(
        std::fs::symlink_metadata(install.data.join("renamed")).unwrap().file_type().is_symlink()
    );
    assert_eq!(std::fs::read_to_string(documents.join("thesis.txt")).unwrap(), "years of work");
}

#[test]
fn an_endless_loop_anywhere_in_a_script_is_stopped_at_its_deadline() {
    let install = Installation::new();
    for script in [
        "export async function main() { for (;;) await null; }",
        "for (;;) {} export function main() {}",
    ] {
        let mut request = install.request(script);
        request["timeoutSeconds"] = json!(1);
        let started = Instant::now();
        assert_eq!(run(&request, &[]).0, 2, "{script}");
        assert!(started.elapsed() < Duration::from_secs(10), "{script}: {:?}", started.elapsed());
    }
}

#[test]
fn a_batch_file_is_never_run_because_windows_runs_it_through_a_shell() {
    let install = Installation::new();
    for name in ["run.cmd", "RUN.BAT"] {
        let batch = install.data.join(name);
        std::fs::write(&batch, "@echo off").unwrap();
        let program = batch.display().to_string();
        let (code, err) = install.run(&exec_attempt(&program, "{}"), json!({ "exec": [program] }));
        assert_eq!(code, 1, "{name}");
        assert!(err.contains("batch file"), "{err}");
    }
}

#[cfg(unix)]
#[test]
fn a_program_that_writes_without_end_hands_the_hook_only_so_much() {
    let install = Installation::new();
    let script = "export function main(ctx) {
        const r = ctx.exec('/bin/sh', ['-c', 'head -c 20000000 /dev/zero'], {});
        if (r.exitCode !== 0) throw new Error('exit ' + r.exitCode);
        if (r.stdout.length !== 8 * 1024 * 1024) throw new Error('length ' + r.stdout.length);
    }";
    let (code, err) = install.run(script, json!({ "exec": ["/bin/sh"] }));
    assert_eq!(code, 0, "{err}");
}

// --- checking a script before it is accepted ---

fn check(scripts: &[&Path]) -> (i32, String) {
    let output = Command::new(HOOK).arg("--check").args(scripts).output().unwrap();
    (output.status.code().unwrap_or(-1), String::from_utf8_lossy(&output.stderr).into_owned())
}

#[test]
fn check_parses_a_script_and_runs_none_of_it() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.js");
    // Its top level throws: if any of it ran, the check would fail.
    std::fs::write(&good, "throw new Error('ran'); export function main() {}").unwrap();
    let (code, err) = check(&[&good]);
    assert_eq!(code, 0, "{err}");
}

#[test]
fn check_names_every_script_that_does_not_parse() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.js");
    let broken = dir.path().join("broken.js");
    let missing = dir.path().join("missing.js");
    std::fs::write(&good, "export function main() {}").unwrap();
    std::fs::write(&broken, "export function main( {").unwrap();
    let (code, err) = check(&[&good, &broken, &missing]);
    assert_eq!(code, 1);
    assert!(err.contains("broken.js"), "{err}");
    assert!(err.contains("missing.js"), "{err}");
    assert!(!err.contains("good.js"), "{err}");
    assert_eq!(check(&[]).0, 3);
}

#[test]
fn a_script_copied_into_the_runs_own_directory_may_run_and_one_elsewhere_may_not() {
    let install = Installation::new();
    let mut request = install.request("export function main() {}");
    let copy = install.temp.join("hook.js");
    std::fs::write(&copy, "export function main() {}").unwrap();
    request["script"] = json!(copy);
    assert_eq!(run(&request, &[]).0, 0);
    let elsewhere = install.home.join("hook.js");
    std::fs::write(&elsewhere, "export function main() {}").unwrap();
    request["script"] = json!(elsewhere);
    assert_eq!(run(&request, &[]).0, 3);
}
