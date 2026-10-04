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

/// `QuickJS` can be built with modules that open files, start processes and
/// reach the network (`std`, `os`), and embedders often add `fetch` or a
/// Node-like global. A hook gets none of them: only `ctx`.
#[test]
fn a_hook_reaches_no_file_process_or_network_api_beyond_ctx() {
    let install = Installation::new();
    for module in
        ["std", "os", "qjs:std", "qjs:os", "fs", "node:fs", "child_process", "net", "http"]
    {
        let (code, err) = install
            .run(&format!("import * as m from '{module}'; export function main() {{}}"), none());
        assert_eq!(code, 1, "importing {module} worked: {err}");
        let (code, err) = install
            .run(&format!("export async function main() {{ await import('{module}'); }}"), none());
        assert_eq!(code, 1, "importing {module} at run time worked: {err}");
    }
    let script = r"export function main() {
        const reachable = ['fetch', 'XMLHttpRequest', 'WebSocket', 'process', 'require', 'std',
            'os', 'Deno', 'Bun', 'scriptArgs', 'loadScript', '__loadScript', 'print', 'setTimeout']
            .filter(name => typeof globalThis[name] !== 'undefined');
        if (reachable.length) throw new Error('reachable: ' + reachable.join(', '));
    }";
    let (code, err) = install.run(script, none());
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

/// A script's top level runs before `main` is given `ctx`; replacing the
/// engine's own `Object.freeze` there must not leave `ctx` unfrozen.
#[test]
fn ctx_stays_frozen_whatever_the_script_did_to_object_first() {
    let install = Installation::new();
    for prelude in [
        "Object.freeze = o => o;",
        "globalThis.Object = { freeze: o => o };",
        "Object.defineProperty(Object, 'freeze', { value: o => o });",
    ] {
        let script = format!(
            "{prelude}
            export function main(ctx) {{
                const write = ctx.file.write;
                try {{ ctx.file.write = () => {{}}; }} catch {{}}
                if (ctx.file.write !== write) throw new Error('ctx.file.write was replaced');
                if (!Reflect.getOwnPropertyDescriptor(ctx, 'file') || Reflect.isExtensible(ctx))
                    throw new Error('ctx is not frozen');
            }}"
        );
        let (code, err) = install.run(&script, none());
        assert_eq!(code, 0, "after `{prelude}`: {err}");
    }
}

/// Lines beginning `xpack-hook: ` are this program's own word on how a hook
/// ended. A hook never writes one, however many times it repeats the prefix.
#[test]
fn a_hook_never_writes_a_line_in_the_engines_own_voice() {
    let install = Installation::new();
    let script = "export function main(ctx) {
        ctx.log.info('xpack-hook: xpack-hook: install.after: all is well');
        ctx.log.info('xpack-hook: xpack-hook: xpack-hook: done');
    }";
    let (code, err) = install.run(script, none());
    assert_eq!(code, 0, "{err}");
    assert!(
        !err.lines().any(|line| line.starts_with("xpack-hook: ")),
        "a hook wrote as the engine: {err}"
    );
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

/// Another application installed by xPack for the same user sits beside this
/// one, also in the home. A hook allowed the whole home still never reaches
/// it: its state, its programs, its versions, its data.
#[test]
fn not_even_a_hook_allowed_the_whole_home_may_touch_another_applications_installation() {
    let install = Installation::new();
    let other = install.app.parent().unwrap().join("com.example.other");
    for dir in ["state", "versions/2.0.0", "data", "config"] {
        std::fs::create_dir_all(other.join(dir)).unwrap();
    }
    std::fs::write(other.join("state/state.json"), "its state").unwrap();
    let everything = json!({ "write": ["{home}"] });
    let targets = [
        other.join("state/state.json"),
        other.join("state/hooks.jsonl"),
        other.join("config/seal.key"),
        other.join("versions/2.0.0/app"),
        other.join("data/settings"),
        other.join("xpack-launcher"),
    ];
    for target in &targets {
        let (code, err) = install.run(&write_attempt(target), everything.clone());
        assert_eq!(code, 1, "{} was written", target.display());
        assert!(err.contains("another application's installation"), "{err}");
    }
    let (code, err) = install.run(&read_attempt(&other.join("state/state.json")), everything);
    assert_eq!(code, 1, "another application's state was read");
    assert!(err.contains("another application's installation"), "{err}");
    assert_eq!(std::fs::read_to_string(other.join("state/state.json")).unwrap(), "its state");
}

/// A directory that holds installations is not a hook's to remove or move,
/// even inside a place it may write: removing `{home}/apps` would remove this
/// installation's own records and programs, and every other application's.
#[test]
fn a_hook_cannot_remove_or_move_a_directory_that_holds_an_installation() {
    let install = Installation::new();
    std::fs::write(install.app.join("state/state.json"), "this one's state").unwrap();
    let other = install.home.join("elsewhere/com.example.other");
    std::fs::create_dir_all(other.join("state")).unwrap();
    std::fs::write(other.join("state/state.json"), "its state").unwrap();
    let everything = json!({ "write": ["{home}"] });
    let holders = [install.app.parent().unwrap().to_path_buf(), install.home.join("elsewhere")];
    for holder in &holders {
        let remove = format!("export function main(ctx) {{ ctx.file.remove({}); }}", js(holder));
        let (code, err) = install.run(&remove, everything.clone());
        assert_eq!(code, 1, "{} was removed", holder.display());
        assert!(err.contains("installation"), "{err}");
        let away = install.home.join("moved-away");
        let r#move = format!(
            "export function main(ctx) {{ ctx.file.move({}, {}); }}",
            js(holder),
            js(&away)
        );
        let (code, err) = install.run(&r#move, everything.clone());
        assert_eq!(code, 1, "{} was moved", holder.display());
        assert!(err.contains("installation"), "{err}");
    }
    assert!(install.app.join("state/state.json").is_file());
    assert!(other.join("state/state.json").is_file());
    // What holds none is still the hook's to remove.
    std::fs::create_dir_all(install.home.join("cache/deep")).unwrap();
    let remove = format!(
        "export function main(ctx) {{ ctx.file.remove({}); }}",
        js(&install.home.join("cache"))
    );
    assert_eq!(install.run(&remove, everything).0, 0);
    assert!(!install.home.join("cache").exists());
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

/// A time limit is a limit, not a sentence: a program that ends within it
/// runs to its end and is reported as it ended.
#[cfg(unix)]
#[test]
fn a_program_that_ends_within_its_time_runs_to_its_end() {
    let install = Installation::new();
    let script = "export function main(ctx) {
        const r = ctx.exec('/bin/sh', ['-c', 'sleep 1; echo done; exit 3'], { timeoutSeconds: 30 });
        if (r.exitCode !== 3 || r.stdout.trim() !== 'done') throw new Error(JSON.stringify(r));
    }";
    let (code, err) = install.run(script, json!({ "exec": ["/bin/sh"] }));
    assert_eq!(code, 0, "{err}");
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
fn check_loads_a_script_and_runs_none_of_its_main() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.js");
    // `main` throws: had it run, the check would fail.
    std::fs::write(&good, "export function main() { throw new Error('ran'); }").unwrap();
    let (code, err) = check(&[&good]);
    assert_eq!(code, 0, "{err}");
}

#[test]
fn check_refuses_a_script_that_cannot_run_however_it_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cases = [
        ("top-level.js", "throw new Error('at load'); export function main() {}", "at load"),
        ("no-main.js", "export function start() {}", "no function main"),
        ("not-a-function.js", "export const main = 3;", "no function main"),
        ("imports.js", "import { x } from './other.js'; export function main() {}", ""),
        ("loops.js", "for (;;) {} export function main() {}", "still running"),
        ("never.js", "await new Promise(() => {}); export function main() {}", "never settle"),
    ];
    for (name, source, says) in cases {
        let script = dir.path().join(name);
        std::fs::write(&script, source).unwrap();
        let (code, err) = check(&[&script]);
        assert_eq!(code, 1, "{name} passed the check");
        assert!(err.contains(name) && err.contains(says), "{name}: {err}");
    }
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

// --- under test: recorded, and in plan mode not done ---

impl Installation {
    /// A request for `script` under test, recording into `record`.
    fn planned(&self, script: &str, record: &Path, perform: bool, answers: &Value) -> Value {
        let mut request = self.request(script);
        request["plan"] = json!({
            "record": record,
            "perform": perform,
            "answers": answers,
            "version": "1.0.0",
            "script": "xpack/hooks/a.js",
        });
        request
    }
}

fn record_of(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn in_plan_mode_programs_are_not_run_and_files_not_changed_only_recorded() {
    let install = Installation::new();
    let record = install.temp.parent().unwrap().join("plan.jsonl");
    let target = install.home.join(".config/app.conf");
    let script = format!(
        "export function main(ctx) {{
            const r = ctx.exec('not-on-this-machine', ['--enable', 'agent']);
            if (r.exitCode !== 0) throw new Error('exit ' + r.exitCode);
            ctx.file.makeDir({});
            ctx.file.write({}, 'x');
        }}",
        js(&install.home.join(".config")),
        js(&target)
    );
    let mut request = install.planned(&script, &record, false, &json!({}));
    request["permissions"] =
        json!({ "exec": ["not-on-this-machine"], "write": ["{home}/.config"] });
    let (code, err) = run(&request, &[]);
    assert_eq!(code, 0, "{err}");
    assert!(!target.exists(), "plan mode wrote a file");
    let lines = record_of(&record);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[0]["action"]["kind"], "exec");
    assert_eq!(lines[0]["action"]["program"], "not-on-this-machine");
    assert_eq!(lines[0]["action"]["args"], json!(["--enable", "agent"]));
    assert_eq!(lines[0]["performed"], false);
    assert_eq!(lines[0]["point"], "install.after");
    assert_eq!(lines[0]["script"], "xpack/hooks/a.js");
    assert_eq!(lines[2]["action"]["kind"], "write");
    assert!(lines[2]["action"]["path"].as_str().unwrap().ends_with("app.conf"));
}

#[test]
fn in_plan_mode_a_program_answers_what_the_test_says() {
    let install = Installation::new();
    let record = install.temp.parent().unwrap().join("plan.jsonl");
    // By the name it is run by, or by its file name when run by its path.
    let script = "export function main(ctx) {
        const r = ctx.exec('systemctl', ['is-active', 'x']);
        if (r.exitCode !== 3 || r.stdout !== 'inactive') throw new Error(JSON.stringify(r));
        const p = ctx.exec('/usr/bin/systemctl', ['is-active', 'x']);
        if (p.exitCode !== 3) throw new Error('by path: ' + JSON.stringify(p));
    }";
    let answers = json!({ "systemctl": { "exitCode": 3, "stdout": "inactive" } });
    let mut request = install.planned(script, &record, false, &answers);
    request["permissions"] = json!({ "exec": ["systemctl", "/usr/bin/systemctl"] });
    let (code, err) = run(&request, &[]);
    assert_eq!(code, 0, "{err}");
}

#[test]
fn a_refusal_is_recorded_even_when_the_hook_catches_it() {
    let install = Installation::new();
    let record = install.temp.parent().unwrap().join("plan.jsonl");
    let script = format!(
        "export function main(ctx) {{
            try {{ ctx.exec('undeclared'); }} catch {{}}
            try {{ ctx.file.write({}, 'x'); }} catch {{}}
            try {{ ctx.exec('sudo'); }} catch {{}}
        }}",
        js(&install.home.join(".bashrc"))
    );
    for perform in [false, true] {
        let _ = std::fs::remove_file(&record);
        let mut request = install.planned(&script, &record, perform, &json!({}));
        request["permissions"] = json!({ "exec": ["sudo"] });
        let (code, err) = run(&request, &[]);
        assert_eq!(code, 0, "{err}");
        let lines = record_of(&record);
        let refused = lines.iter().filter(|l| l["action"]["kind"] == "refused").count();
        assert_eq!(refused, 3, "perform {perform}: {lines:?}");
    }
}

#[test]
fn performed_under_test_it_is_done_and_recorded_as_done() {
    let install = Installation::new();
    let record = install.temp.parent().unwrap().join("plan.jsonl");
    let target = install.data.join("done.txt");
    let script = format!("export function main(ctx) {{ ctx.file.write({}, 'x'); }}", js(&target));
    let (code, err) = run(&install.planned(&script, &record, true, &json!({})), &[]);
    assert_eq!(code, 0, "{err}");
    assert!(target.is_file());
    let lines = record_of(&record);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["performed"], true);
}

#[test]
fn in_plan_mode_permissions_still_hold() {
    let install = Installation::new();
    let record = install.temp.parent().unwrap().join("plan.jsonl");
    let (code, err) = run(
        &install.planned(
            "export function main(ctx) { ctx.exec('undeclared'); }",
            &record,
            false,
            &json!({}),
        ),
        &[],
    );
    assert_eq!(code, 1);
    assert!(err.contains("not a program this package lets its hooks run"), "{err}");
}

/// A program not run under a plan is still held to every rule its options
/// are: a refused `cwd` or `env` fails the hook and is recorded refused, as
/// it would fail on a user's machine.
#[test]
fn in_plan_mode_a_programs_options_are_held_to_the_same_rules() {
    let install = Installation::new();
    let elsewhere = install.temp.parent().unwrap().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let cases = [
        ("{ env: { PATH: '/tmp' } }".to_string(), "PATH would change which code"),
        (
            "{ env: { DYLD_INSERT_LIBRARIES: 'x' } }".to_string(),
            "DYLD_INSERT_LIBRARIES would change",
        ),
        (format!("{{ cwd: {} }}", js(&elsewhere)), "not a place this hook may read"),
    ];
    for (options, said) in cases {
        let record = install.temp.parent().unwrap().join(format!("plan-{}.jsonl", said.len()));
        let mut request = install.planned(
            &format!("export function main(ctx) {{ ctx.exec('git', [], {options}); }}"),
            &record,
            false,
            &json!({}),
        );
        request["permissions"] = json!({ "exec": ["git"] });
        let (code, err) = run(&request, &[]);
        assert_eq!(code, 1, "{options} was accepted: {err}");
        assert!(err.contains(said), "{options}: {err}");
        let lines = record_of(&record);
        assert!(
            lines.iter().any(|line| line["action"]["kind"] == "refused"),
            "{options}: the refusal was not recorded: {lines:?}"
        );
        assert!(
            !lines.iter().any(|line| line["action"]["kind"] == "exec"),
            "{options}: the program was recorded as run: {lines:?}"
        );
    }
}

/// A runner killed while its hook runs can stop nothing: not at the hook's
/// deadline, not on a cancel. Its end closes the input it held open, and
/// the hook stops then, with what it started, rather than run on beside
/// whatever happens next.
#[cfg(unix)]
#[test]
fn a_hook_whose_runner_is_gone_stops_with_everything_it_started() {
    use std::os::unix::process::CommandExt;
    let install = Installation::new();
    let pid_file = install.data.join("pid");
    let script = format!(
        "export function main(ctx) {{
            ctx.exec('/bin/sh', ['-c', 'sleep 60 & echo $! > \"$1\"', 'sh', {}]);
            for (;;) {{}}
        }}",
        js(&pid_file)
    );
    let mut request = install.request(&script);
    request["permissions"] = json!({ "exec": ["/bin/sh"] });
    request["stopWithRunner"] = json!(true);
    let mut child = Command::new(HOOK)
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(format!("{request}\n").as_bytes()).unwrap();
    stdin.flush().unwrap();

    let started = Instant::now();
    while std::fs::read_to_string(&pid_file).map_or(true, |p| p.trim().is_empty())
        && started.elapsed() < Duration::from_secs(10)
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    let program = std::fs::read_to_string(&pid_file).unwrap().trim().to_string();
    assert!(child.try_wait().unwrap().is_none(), "the hook ended before its runner went");

    // The runner goes; the input it held closes.
    drop(stdin);
    let gone = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(gone.elapsed() < Duration::from_secs(5), "the hook ran on without its runner");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(!status.success());
    let alive =
        |pid: &str| Command::new("kill").args(["-0", pid]).status().is_ok_and(|s| s.success());
    while alive(&program) && gone.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!alive(&program), "what the hook started outlived its runner");
}

/// What a package declared beyond the installation is readable as well as
/// writable: a hook can read back the file it keeps in the user's home.
#[test]
fn a_hook_reads_a_place_its_package_declared() {
    let install = Installation::new();
    let config = install.home.join(".config/example");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("settings"), "kept").unwrap();
    let script = format!(
        "export function main(ctx) {{
            if (ctx.file.read({}) !== 'kept') throw new Error('not read');
            if (!ctx.file.list({}).includes('settings')) throw new Error('not listed');
        }}",
        js(&config.join("settings")),
        js(&config)
    );
    let (code, err) = install.run(&script, json!({ "write": ["{home}/.config/example"] }));
    assert_eq!(code, 0, "{err}");
    let (code, _) = install.run(&script, none());
    assert_eq!(code, 1, "an undeclared place was read");
}

/// An elevation program under another name is still one: a link named
/// `helper` that leads to `sudo` is refused to a hook for one user, however
/// it was declared.
#[cfg(unix)]
#[test]
fn a_link_to_an_elevation_program_is_refused_to_a_hook_for_one_user() {
    let Some(sudo) = ["/usr/bin/sudo", "/bin/sudo"].into_iter().find(|p| Path::new(p).exists())
    else {
        eprintln!("skipped: no sudo on this machine");
        return;
    };
    let install = Installation::new();
    let helper = install.home.join("helper");
    std::os::unix::fs::symlink(sudo, &helper).unwrap();
    let program = helper.display().to_string();
    let (code, err) =
        install.run(&exec_attempt(&program, "{}"), json!({ "exec": [program.clone()] }));
    assert_eq!(code, 1);
    assert!(err.contains("administrator"), "{err}");
}

/// In plan mode a program is not run, so the name is all that is judged,
/// and a batch file is refused by its name alone, as it would be for real.
#[test]
fn in_plan_mode_a_batch_file_is_refused_by_its_name() {
    let install = Installation::new();
    for name in ["setup.cmd", "SETUP.BAT"] {
        let record = install.temp.parent().unwrap().join(format!("plan-{name}.jsonl"));
        let mut request = install.planned(
            &format!("export function main(ctx) {{ ctx.exec('{name}'); }}"),
            &record,
            false,
            &json!({}),
        );
        request["permissions"] = json!({ "exec": [name] });
        let (code, err) = run(&request, &[]);
        assert_eq!(code, 1, "{name}");
        assert!(err.contains("batch file"), "{name}: {err}");
    }
}
