//! Hooks from the command line: declared in `xpack.json`, checked by
//! `xpack pack` and `xpack hooks check`, and run by `xpack install` and
//! `xpack uninstall` with the `xpack-hook` beside `xpack`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn xpack() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_xpack"))
}

/// Whether `xpack-hook` was built beside `xpack`; see the note on the same
/// check in `end_to_end.rs`. Where the workspace is always built first, a
/// missing one is a broken build.
fn engine_is_built() -> bool {
    let engine =
        xpack().parent().unwrap().join(format!("xpack-hook{}", std::env::consts::EXE_SUFFIX));
    if engine.is_file() {
        return true;
    }
    assert!(std::env::var_os("CI").is_none(), "{} is not built", engine.display());
    eprintln!("skipped: run `cargo build --workspace` first");
    false
}

const MARK: &str = "export function main(ctx) {
    ctx.file.write(ctx.path(ctx.dataDir, 'installed'), ctx.toVersion);
}";

struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    /// A project whose payload holds `scripts`, declaring `hooks`.
    fn new(hooks: &serde_json::Value, scripts: &[(&str, &[u8])]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("payload");
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        std::fs::write(payload.join("bin/app"), "app").unwrap();
        for (path, bytes) in scripts {
            let file = payload.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, bytes).unwrap();
        }
        let config = serde_json::json!({
            "application": { "id": "com.example.demo", "name": "Demo", "version": "1.0.0" },
            "launch": { "executable": "bin/app" },
            "hooks": hooks,
        });
        std::fs::write(dir.path().join("xpack.json"), config.to_string()).unwrap();
        let project = Self { dir };
        let keygen = project.run(&["keygen", "--out", "signing.json"]);
        assert!(keygen.status.success(), "{}", stderr(&keygen));
        project
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with(&xpack(), args)
    }

    fn run_with(&self, program: &Path, args: &[&str]) -> Output {
        self.command(program).args(args).output().unwrap()
    }

    /// `program`, run in the project with a home of its own on Unix: hooks
    /// under test are told where the user's home is, and a test, or a bug in
    /// what it tests, must never write into the real one. Windows takes the
    /// home from the system's known folders, not the environment, and
    /// redirecting `USERPROFILE` only breaks the folders found beside it.
    fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        command.current_dir(self.path());
        if cfg!(unix) {
            let home = self.path().join("home");
            std::fs::create_dir_all(&home).unwrap();
            command.env("HOME", &home);
        }
        command
    }

    fn pack(&self) -> Output {
        self.run(&["pack", "payload", "--key", "signing.json", "--out", "demo.xpkg"])
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_package_with_hooks_is_packed_installed_with_them_and_uninstalled() {
    if !engine_is_built() {
        return;
    }
    let hooks = serde_json::json!({
        "install": "xpack/hooks/mark.js",
        "uninstall": "xpack/hooks/mark.js",
    });
    let project = Project::new(&hooks, &[("xpack/hooks/mark.js", MARK.as_bytes())]);
    let packed = project.pack();
    assert!(packed.status.success(), "{}", stderr(&packed));

    let root = project.path().join("root");
    let root_arg = root.to_string_lossy().into_owned();
    let installed = project.run(&[
        "--root",
        &root_arg,
        "install",
        "demo.xpkg",
        "--trust",
        "signing.pub.json",
        "--no-launcher",
    ]);
    assert!(installed.status.success(), "{}", stderr(&installed));
    let installation = root.join("com.example.demo");
    assert_eq!(
        std::fs::read_to_string(installation.join("data/installed")).unwrap(),
        "1.0.0",
        "the install hook did not run"
    );

    let removed = project.run(&["--root", &root_arg, "uninstall", "com.example.demo", "--yes"]);
    assert!(removed.status.success(), "{}", stderr(&removed));
    assert!(!installation.exists(), "something was left behind");
}

#[test]
fn every_installation_made_with_xpack_install_gets_the_program_that_runs_hooks() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    assert!(project.pack().status.success());
    let root = project.path().join("root");
    let installed = project.run(&[
        "--root",
        &root.to_string_lossy(),
        "install",
        "demo.xpkg",
        "--trust",
        "signing.pub.json",
    ]);
    assert!(installed.status.success(), "{}", stderr(&installed));
    let engine =
        root.join("com.example.demo").join(format!("xpack-hook{}", std::env::consts::EXE_SUFFIX));
    assert!(engine.is_file(), "no xpack-hook in the installation");
}

/// A project's hooks, the scripts in its payload, and what refusing it says.
type Case = (serde_json::Value, Vec<(&'static str, &'static [u8])>, &'static str);

#[test]
fn pack_refuses_hooks_that_could_never_run_and_names_the_hook_and_the_rule() {
    if !engine_is_built() {
        return;
    }
    let script = |source: &'static str| -> Vec<(&'static str, &'static [u8])> {
        vec![("xpack/hooks/a.js", source.as_bytes())]
    };
    let cases: Vec<Case> = vec![
        (
            serde_json::json!({ "update": { "when": "afterFiles", "script": "xpack/hooks/a.js" } }),
            script(MARK),
            "not a moment of update",
        ),
        (
            serde_json::json!({ "install": "xpack/hooks/missing.js" }),
            script(MARK),
            "not a file of the payload",
        ),
        (
            serde_json::json!({ "install": "xpack/hooks/a.js" }),
            vec![("xpack/hooks/a.js", &[0xff, 0xfe, b'x'][..])],
            "not valid UTF-8",
        ),
        (
            serde_json::json!({ "install": "xpack/hooks/a.js" }),
            script("const fs = require('fs'); export function main() {}"),
            "uses require",
        ),
        (
            serde_json::json!({ "install": "xpack/hooks/a.js" }),
            script("export async function main() { await import('./b.js'); }"),
            "dynamic import",
        ),
        (
            serde_json::json!({ "install": "xpack/hooks/a.js" }),
            script("export function start() {}"),
            "no function main",
        ),
        (
            serde_json::json!({ "install": "xpack/hooks/a.js" }),
            script("export function main( {"),
            "cannot run",
        ),
        (
            serde_json::json!({ "install": "xpack/hooks/a.js" }),
            script("import { x } from './b.js'; export function main() {}"),
            "cannot run",
        ),
        (
            serde_json::json!({ "install": "xpack/hooks/a.js", "permissions": { "user": { "exec": ["sudo"] } } }),
            script(MARK),
            "administrator rights",
        ),
    ];
    for (hooks, scripts, says) in cases {
        let project = Project::new(&hooks, &scripts);
        let packed = project.pack();
        assert!(!packed.status.success(), "{hooks} was packed");
        assert!(stderr(&packed).contains(says), "{hooks}: {}", stderr(&packed));
        assert!(!project.path().join("demo.xpkg").exists(), "{hooks}: a package was written");
    }
}

#[cfg(unix)]
#[test]
fn pack_refuses_a_hook_script_that_is_a_link() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({ "install": "xpack/hooks/a.js" }), &[]);
    let real = project.path().join("elsewhere.js");
    std::fs::write(&real, MARK).unwrap();
    std::fs::create_dir_all(project.path().join("payload/xpack/hooks")).unwrap();
    std::os::unix::fs::symlink(&real, project.path().join("payload/xpack/hooks/a.js")).unwrap();
    let packed = project.pack();
    assert!(!packed.status.success());
    assert!(stderr(&packed).contains("not a regular file"), "{}", stderr(&packed));
}

#[test]
fn pack_warns_about_hooks_whose_work_nothing_undoes() {
    if !engine_is_built() {
        return;
    }
    let hooks = serde_json::json!({ "install": "xpack/hooks/a.js", "update": "xpack/hooks/a.js" });
    let project = Project::new(&hooks, &[("xpack/hooks/a.js", MARK.as_bytes())]);
    let packed = project.pack();
    assert!(packed.status.success(), "{}", stderr(&packed));
    let said = stderr(&packed);
    assert!(
        said.contains("warning: this package has install hooks and no uninstall hook"),
        "{said}"
    );
    assert!(said.contains("warning: this package has update hooks and no rollback hook"), "{said}");
}

/// Each warning names a hook whose work is not undone, and only that: a
/// package that undoes what it does, or does nothing to undo, is told nothing.
#[test]
fn pack_warns_only_where_a_hooks_work_is_left_undone() {
    if !engine_is_built() {
        return;
    }
    let a = "xpack/hooks/a.js";
    let uninstall = "no uninstall hook";
    let rollback = "no rollback hook";
    let mandatory = "this release is mandatory";
    let cases = [
        (serde_json::json!({ "install": a, "uninstall": a }), false, vec![]),
        (serde_json::json!({ "uninstall": a, "rollback": a }), false, vec![]),
        (serde_json::json!({ "update": a, "rollback": a }), false, vec![]),
        (serde_json::json!({ "update": a, "rollback": a }), true, vec![mandatory]),
        (
            serde_json::json!({ "update": [a, { "when": "confirmed", "script": a }], "rollback": a }),
            true,
            vec![],
        ),
        (
            serde_json::json!({ "update": { "when": "before", "script": a }, "rollback": a }),
            true,
            vec![],
        ),
        (serde_json::json!({ "install": a }), false, vec![uninstall]),
    ];
    for (hooks, is_mandatory, expected) in cases {
        let project = Project::new(&hooks, &[(a, MARK.as_bytes())]);
        let config = serde_json::json!({
            "application": { "id": "com.example.demo", "name": "Demo", "version": "1.0.0" },
            "launch": { "executable": "bin/app" },
            "update": { "mandatory": is_mandatory },
            "hooks": hooks,
        });
        std::fs::write(project.path().join("xpack.json"), config.to_string()).unwrap();
        let packed = project.pack();
        assert!(packed.status.success(), "{}", stderr(&packed));
        let said = stderr(&packed);
        for warning in [uninstall, rollback, mandatory] {
            assert_eq!(
                said.contains(warning),
                expected.contains(&warning),
                "{hooks} (mandatory: {is_mandatory}), `{warning}`: {said}"
            );
        }
    }
}

#[test]
fn a_package_without_hooks_needs_no_program_to_run_them_and_one_with_hooks_does() {
    let dir = tempfile::tempdir().unwrap();
    // `xpack` alone, with nothing beside it.
    let alone = dir.path().join(format!("xpack{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(xpack(), &alone).unwrap();

    let plain = Project::new(&serde_json::json!({}), &[]);
    let packed =
        plain.run_with(&alone, &["pack", "payload", "--key", "signing.json", "--out", "demo.xpkg"]);
    assert!(packed.status.success(), "{}", stderr(&packed));

    let hooked = Project::new(
        &serde_json::json!({ "install": "xpack/hooks/a.js" }),
        &[("xpack/hooks/a.js", MARK.as_bytes())],
    );
    let packed = hooked
        .run_with(&alone, &["pack", "payload", "--key", "signing.json", "--out", "demo.xpkg"]);
    assert!(!packed.status.success());
    assert!(stderr(&packed).contains("xpack-hook was not found"), "{}", stderr(&packed));
}

#[test]
fn hooks_check_runs_the_same_checks_without_building_anything() {
    if !engine_is_built() {
        return;
    }
    let good = Project::new(
        &serde_json::json!({ "install": "xpack/hooks/a.js", "uninstall": "xpack/hooks/a.js" }),
        &[("xpack/hooks/a.js", MARK.as_bytes())],
    );
    let checked = good.run(&["hooks", "check", "payload"]);
    assert!(checked.status.success(), "{}", stderr(&checked));
    assert!(String::from_utf8_lossy(&checked.stdout).contains("ok, 1 script"));

    let bad = Project::new(
        &serde_json::json!({ "install": "xpack/hooks/a.js" }),
        &[("xpack/hooks/a.js", b"export function start() {}")],
    );
    let checked = bad.run(&["hooks", "check", "payload"]);
    assert!(!checked.status.success());
    assert!(stderr(&checked).contains("no function main"), "{}", stderr(&checked));
    assert!(!bad.path().join("demo.xpkg").exists());
}

// --- the commands that run hooks ---

/// Appends `<operation>.<moment>(<cause>);` to `dataDir/marks`.
const MARKS: &str = "export function main(ctx) {
    const file = ctx.path(ctx.dataDir, 'marks');
    const before = ctx.file.exists(file) ? ctx.file.read(file) : '';
    const cause = ctx.cause ? '(' + ctx.cause + ')' : '';
    ctx.file.write(file, before + ctx.operation + '.' + ctx.when + cause + ';');
}";

impl Project {
    /// Packs `version` of the application, its payload holding `scripts`
    /// and its configuration declaring `hooks`, as `demo-<version>.xpkg`.
    fn pack_version(
        &self,
        version: &str,
        hooks: &serde_json::Value,
        scripts: &[(&str, &str)],
    ) -> String {
        let payload = format!("payload-{version}");
        std::fs::create_dir_all(self.path().join(&payload).join("bin")).unwrap();
        std::fs::write(self.path().join(&payload).join("bin/app"), "app").unwrap();
        for (path, text) in scripts {
            let file = self.path().join(&payload).join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        let config = serde_json::json!({
            "application": { "id": "com.example.demo", "name": "Demo", "version": version },
            "launch": { "executable": "bin/app" },
            "hooks": hooks,
        });
        let config_file = format!("xpack-{version}.json");
        std::fs::write(self.path().join(&config_file), config.to_string()).unwrap();
        let out = format!("demo-{version}.xpkg");
        let packed = self.run(&[
            "pack",
            &payload,
            "--config",
            &config_file,
            "--key",
            "signing.json",
            "--out",
            &out,
        ]);
        assert!(packed.status.success(), "{}", stderr(&packed));
        out
    }

    fn root(&self) -> String {
        self.path().join("root").to_string_lossy().into_owned()
    }

    fn in_root(&self, args: &[&str]) -> Output {
        let root = self.root();
        let mut full = vec!["--root", root.as_str()];
        full.extend_from_slice(args);
        self.run(&full)
    }

    fn install(&self, package: &str, extra: &[&str]) -> Output {
        let mut args = vec!["install", package, "--trust", "signing.pub.json", "--no-launcher"];
        args.extend_from_slice(extra);
        self.in_root(&args)
    }

    fn installation(&self) -> PathBuf {
        self.path().join("root/com.example.demo")
    }

    fn marks(&self) -> String {
        std::fs::read_to_string(self.installation().join("data/marks")).unwrap_or_default()
    }
}

fn every_update_and_rollback_moment() -> serde_json::Value {
    serde_json::json!({
        "update": [
            { "when": "before", "script": "xpack/hooks/m.js" },
            { "when": "after", "script": "xpack/hooks/m.js" }
        ],
        "rollback": [
            { "when": "before", "script": "xpack/hooks/m.js" },
            { "when": "after", "script": "xpack/hooks/m.js" }
        ]
    })
}

#[test]
fn a_rollback_on_request_runs_its_hooks_and_leaves_the_version_good_to_activate_again() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let first = project.pack_version("1.0.0", &serde_json::json!({}), &[]);
    let second = project.pack_version(
        "2.0.0",
        &every_update_and_rollback_moment(),
        &[("xpack/hooks/m.js", MARKS)],
    );
    assert!(project.install(&first, &[]).status.success());
    let applied = project.install(&second, &[]);
    assert!(applied.status.success(), "{}", stderr(&applied));
    // Proved good, as a start of it would.
    commit_as_started(&project);
    assert_eq!(project.marks(), "update.before;update.after;");

    let rolled = project.in_root(&["rollback", "com.example.demo"]);
    assert!(rolled.status.success(), "{}", stderr(&rolled));
    assert_eq!(
        project.marks(),
        "update.before;update.after;rollback.before(requested);rollback.after(requested);"
    );
    let listed = String::from_utf8_lossy(&project.in_root(&["list", "com.example.demo"]).stdout)
        .into_owned();
    assert!(listed.lines().any(|l| l.contains("2.0.0") && !l.contains("bad")), "{listed}");

    // Activated again: none of its hooks a second time.
    let again = project.in_root(&["activate", "com.example.demo", "2.0.0"]);
    assert!(again.status.success(), "{}", stderr(&again));
    assert_eq!(
        project.marks(),
        "update.before;update.after;rollback.before(requested);rollback.after(requested);"
    );
}

/// Marks the active version good, as its first good start would.
fn commit_as_started(project: &Project) {
    let state_file = project.installation().join("state/state.json");
    let mut state = xpack_core::store::load::<xpack_core::InstallState>(&state_file).unwrap().value;
    let active = state.current_version.clone().unwrap();
    state.mark_good(&active);
    state.update = xpack_core::state::UpdatePhase::Idle;
    xpack_core::store::save(&state_file, &state).unwrap();
}

#[test]
fn activating_a_staged_newer_version_applies_it_with_its_update_hooks() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let first = project.pack_version("1.0.0", &serde_json::json!({}), &[]);
    let second = project.pack_version(
        "2.0.0",
        &every_update_and_rollback_moment(),
        &[("xpack/hooks/m.js", MARKS)],
    );
    assert!(project.install(&first, &[]).status.success());
    let staged = project.install(&second, &["--no-activate"]);
    assert!(staged.status.success(), "{}", stderr(&staged));
    assert_eq!(project.marks(), "", "staging ran a hook");

    let activated = project.in_root(&["activate", "com.example.demo", "2.0.0"]);
    assert!(activated.status.success(), "{}", stderr(&activated));
    assert_eq!(project.marks(), "update.before;update.after;");
}

#[test]
fn a_hooks_output_reaches_the_terminal_named_by_its_hook_point() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let package = project.pack_version(
        "1.0.0",
        &serde_json::json!({ "install": "xpack/hooks/say.js" }),
        &[("xpack/hooks/say.js", "export function main(ctx) { ctx.log.info('service created'); }")],
    );
    let installed = project.install(&package, &[]);
    assert!(installed.status.success(), "{}", stderr(&installed));
    assert!(
        stderr(&installed).contains("[install.after] service created"),
        "{}",
        stderr(&installed)
    );
}

#[test]
fn a_failed_install_hook_exits_seven_and_leaves_nothing() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let package = project.pack_version(
        "1.0.0",
        &serde_json::json!({ "install": "xpack/hooks/fail.js" }),
        &[(
            "xpack/hooks/fail.js",
            "export function main() { throw new Error('refused by the hook'); }",
        )],
    );
    let installed = project.install(&package, &[]);
    assert_eq!(installed.status.code(), Some(7), "{}", stderr(&installed));
    assert!(stderr(&installed).contains("refused by the hook"), "{}", stderr(&installed));
    assert!(!project.installation().exists(), "the failed installation left something behind");
}

#[test]
fn list_hooks_prints_the_record() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let package = project.pack_version(
        "1.0.0",
        &serde_json::json!({ "install": "xpack/hooks/m.js" }),
        &[("xpack/hooks/m.js", MARKS)],
    );
    assert!(project.install(&package, &[]).status.success());
    let listed = project.in_root(&["list", "com.example.demo", "--hooks"]);
    let text = String::from_utf8_lossy(&listed.stdout).into_owned();
    assert!(
        text.contains("install.after") && text.contains("started") && text.contains("succeeded"),
        "{text}"
    );
    assert!(text.contains("xpack/hooks/m.js") && text.contains("UTC"), "{text}");

    let listed = project.in_root(&["list", "com.example.demo", "--hooks", "--json"]);
    let lines: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(lines.as_array().unwrap().len(), 2, "{lines}");
    assert_eq!(lines[1]["event"], "succeeded");
}

#[cfg(unix)]
#[test]
fn ctrl_c_stops_a_running_install_hook_and_the_install_is_undone() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let package = project.pack_version(
        "1.0.0",
        &serde_json::json!({ "install": { "script": "xpack/hooks/loop.js", "timeoutSeconds": 60 } }),
        &[(
            "xpack/hooks/loop.js",
            "export function main(ctx) { ctx.file.write(ctx.path(ctx.dataDir, 'looping'), 'yes'); for (;;) {} }",
        )],
    );
    let root = project.root();
    let child = project
        .command(&xpack())
        .args([
            "--root",
            &root,
            "install",
            &package,
            "--trust",
            "signing.pub.json",
            "--no-launcher",
        ])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let looping = project.installation().join("data/looping");
    let started = std::time::Instant::now();
    while !looping.exists() && started.elapsed() < std::time::Duration::from_secs(30) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(looping.exists(), "the hook never started");
    let interrupted =
        Command::new("kill").args(["-INT", &child.id().to_string()]).status().unwrap();
    assert!(interrupted.success());
    let output = child.wait_with_output().unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(30), "the hook ran on");
    assert_eq!(output.status.code(), Some(7), "{}", stderr(&output));
    assert!(stderr(&output).contains("cancelled"), "{}", stderr(&output));
    assert!(!project.installation().exists(), "the cancelled install left something behind");
}

/// A hook that passes its test and fails on the machine it is installed on:
/// what it runs answers success in plan mode, and fails for real.
#[cfg(unix)]
#[test]
fn an_installer_whose_install_hook_fails_exits_seven_and_leaves_nothing() {
    if !engine_is_built() {
        return;
    }
    let bin = xpack().parent().unwrap().to_path_buf();
    let suffix = std::env::consts::EXE_SUFFIX;
    for needed in ["xpack-installer", "xpack-launcher", "xpack-updater", "xpack-uninstaller"] {
        if !bin.join(format!("{needed}{suffix}")).is_file() {
            assert!(std::env::var_os("CI").is_none(), "{needed} is not built");
            return;
        }
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let package = project.pack_version(
        "1.0.0",
        &serde_json::json!({
            "install": "xpack/hooks/fail.js",
            "uninstall": "xpack/hooks/fail.js",
            "permissions": { "user": { "exec": ["/usr/bin/false"] } }
        }),
        &[(
            "xpack/hooks/fail.js",
            "export function main(ctx) { if (ctx.exec('/usr/bin/false').exitCode !== 0) throw new Error('the service would not start'); }",
        )],
    );
    let (tested, report) = project.hooks_test(&package, &[]);
    assert!(tested.status.success(), "{report:#}");
    let stub = bin.join(format!("xpack-installer{suffix}"));
    let built = project.run(&[
        "installer",
        &package,
        "--out",
        "Demo-installer",
        "--stub",
        &stub.to_string_lossy(),
        "--json",
    ]);
    assert!(built.status.success(), "{}", stderr(&built));
    let report: serde_json::Value = serde_json::from_slice(&built.stdout).unwrap();
    let installer = if report["layout"] == "bundle" {
        let macos = project.path().join("Demo-installer/Contents/MacOS");
        std::fs::read_dir(&macos).unwrap().next().unwrap().unwrap().path()
    } else {
        project.path().join("Demo-installer")
    };
    let root = project.path().join("installed");
    let ran = project
        .command(&installer)
        .args(["--root", &root.to_string_lossy(), "--silent"])
        .output()
        .unwrap();
    assert_eq!(ran.status.code(), Some(7), "{}", stderr(&ran));
    assert!(!root.join("com.example.demo").exists(), "the failed install left something behind");
}

// --- xpack hooks test ---

impl Project {
    fn hooks_test(&self, package: &str, extra: &[&str]) -> (Output, serde_json::Value) {
        let mut args = vec!["hooks", "test", package];
        args.extend_from_slice(extra);
        let ran = self.run(&args);
        let suffix = if extra.contains(&"--all-users") {
            ".hooks-report.all-users.json"
        } else {
            ".hooks-report.json"
        };
        let report_file = self.path().join(package.replace(".xpkg", suffix));
        let report = std::fs::read(&report_file)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(serde_json::Value::Null);
        (ran, report)
    }
}

fn point<'a>(report: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    report["points"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["point"] == name)
        .unwrap_or_else(|| panic!("{name} is not in the report: {report}"))
}

/// Every hook point, each marking the data directory and running a program
/// that this machine does not have.
fn every_point() -> serde_json::Value {
    let all = |moments: &[&str]| -> serde_json::Value {
        moments
            .iter()
            .map(|m| serde_json::json!({ "when": m, "script": "xpack/hooks/t.js" }))
            .collect::<Vec<_>>()
            .into()
    };
    serde_json::json!({
        "install": all(&["before", "afterFiles", "after"]),
        "update": all(&["before", "after", "confirmed"]),
        "rollback": all(&["before", "after"]),
        "uninstall": all(&["before", "after"]),
        "permissions": { "user": { "exec": ["agentctl"] } }
    })
}

const TOUCH_AND_RUN: &str = "export function main(ctx) {
    const r = ctx.exec('agentctl', ['--' + ctx.operation]);
    if (r.exitCode !== 0) throw new Error('exit ' + r.exitCode);
    ctx.file.write(ctx.path(ctx.dataDir, ctx.operation + '-' + ctx.when), 'x');
}";

#[test]
fn hooks_test_runs_every_hook_point_and_writes_a_report_bound_to_the_package() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let previous = project.pack_version("1.0.0", &serde_json::json!({}), &[]);
    let package =
        project.pack_version("2.0.0", &every_point(), &[("xpack/hooks/t.js", TOUCH_AND_RUN)]);
    let (ran, report) = project.hooks_test(&package, &["--previous", &previous]);
    assert!(ran.status.success(), "{}\n{report:#}", stderr(&ran));
    assert_eq!(report["passed"], true, "{report:#}");
    assert_eq!(report["mode"], "plan");
    assert_eq!(report["version"], "2.0.0");
    // What it prints says the same: every moment held, so none is said not to.
    let printed = String::from_utf8_lossy(&ran.stdout);
    assert!(printed.contains("install.after        succeeded"), "{printed}");
    assert!(!printed.contains("did not hold"), "{printed}");
    for name in [
        "install.before",
        "install.afterFiles",
        "install.after",
        "update.before",
        "update.after",
        "update.confirmed",
        "rollback.before",
        "rollback.after",
        "uninstall.before",
        "uninstall.after",
    ] {
        let p = point(&report, name);
        assert_eq!(p["result"], "succeeded", "{name}: {p}");
        assert_eq!(p["momentHeld"], true, "{name}: {p}");
        assert!(
            p["actions"].as_array().unwrap().iter().any(|a| a["kind"] == "exec"),
            "{name}: {p}"
        );
    }
    assert!(
        report["programs"].as_array().unwrap().iter().any(|p| p == "agentctl --install"),
        "{report}"
    );
    // Bound to this exact package.
    let inspected = project.run(&["inspect", &package, "--json"]);
    assert!(inspected.status.success());
    assert_eq!(report["manifestSha256"].as_str().unwrap().len(), 64);
}

#[test]
fn hooks_test_without_a_previous_release_says_the_update_scenario_did_not_run() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let package =
        project.pack_version("2.0.0", &every_point(), &[("xpack/hooks/t.js", TOUCH_AND_RUN)]);
    let (ran, report) = project.hooks_test(&package, &[]);
    assert!(ran.status.success(), "{}\n{report:#}", stderr(&ran));
    let update =
        report["scenarios"].as_array().unwrap().iter().find(|s| s["name"] == "update").unwrap();
    assert_eq!(update["ran"], false);
    assert_eq!(point(&report, "update.after")["result"], "not run");
    assert!(point(&report, "rollback.after")["reason"].as_str().unwrap().contains("--previous"));
}

#[test]
fn a_failing_hook_fails_the_report_and_says_why() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let package = project.pack_version(
        "1.0.0",
        &serde_json::json!({ "install": "xpack/hooks/f.js", "uninstall": "xpack/hooks/f.js" }),
        &[("xpack/hooks/f.js", "export function main(ctx) { if (ctx.operation === 'install') throw new Error('no such service'); }")],
    );
    let (ran, report) = project.hooks_test(&package, &[]);
    assert_eq!(ran.status.code(), Some(1), "{}", stderr(&ran));
    assert_eq!(report["passed"], false);
    let install = point(&report, "install.after");
    assert_eq!(install["result"], "failed");
    assert!(install["reason"].as_str().unwrap().contains("no such service"), "{install}");
}

#[test]
fn a_hook_that_catches_a_refusal_still_fails_the_report() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let package = project.pack_version(
        "1.0.0",
        &serde_json::json!({ "install": "xpack/hooks/r.js", "uninstall": "xpack/hooks/r.js" }),
        &[(
            "xpack/hooks/r.js",
            "export function main(ctx) { try { ctx.exec('undeclared'); } catch {} }",
        )],
    );
    let (ran, report) = project.hooks_test(&package, &[]);
    assert_eq!(ran.status.code(), Some(1));
    assert_eq!(point(&report, "install.after")["result"], "refused", "{report:#}");
}

#[test]
fn what_install_writes_outside_the_installation_must_be_gone_when_uninstall_is_done() {
    if !engine_is_built() {
        return;
    }
    let writes = "export function main(ctx) {
        ctx.file.makeDir(ctx.path(ctx.home, '.config', 'demo-test-app'));
        ctx.file.write(ctx.path(ctx.home, '.config', 'demo-test-app', 'settings'), 'x');
    }";
    let removes = "export function main(ctx) {
        ctx.file.remove(ctx.path(ctx.home, '.config', 'demo-test-app'));
    }";
    let permissions = serde_json::json!({ "user": { "write": ["{home}/.config"] } });
    let project = Project::new(&serde_json::json!({}), &[]);

    let leaves = project.pack_version(
        "1.0.0",
        &serde_json::json!({ "install": "xpack/hooks/w.js", "uninstall": "xpack/hooks/u.js", "permissions": permissions }),
        &[("xpack/hooks/w.js", writes), ("xpack/hooks/u.js", "export function main() {}")],
    );
    let (ran, report) = project.hooks_test(&leaves, &[]);
    assert_eq!(ran.status.code(), Some(1), "{report:#}");
    let left: Vec<String> = report["leftBehind"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap().to_string())
        .collect();
    assert!(left.iter().any(|p| p.ends_with("settings")), "{left:?}");

    let cleans = project.pack_version(
        "1.1.0",
        &serde_json::json!({ "install": "xpack/hooks/w.js", "uninstall": "xpack/hooks/u.js", "permissions": permissions }),
        &[("xpack/hooks/w.js", writes), ("xpack/hooks/u.js", removes)],
    );
    let (ran, report) = project.hooks_test(&cleans, &[]);
    assert!(ran.status.success(), "{report:#}");
    assert_eq!(report["leftBehind"], serde_json::json!([]));
}

/// What the report counts as left behind: what install and update hooks put
/// outside the installation, by writing, copying or moving, and nothing took
/// away. Not what they keep in the installation's data, nor what an
/// uninstall hook writes, which nothing would remove after it.
#[test]
fn left_behind_counts_copies_and_moves_and_never_the_installations_own_files() {
    if !engine_is_built() {
        return;
    }
    let install = "export function main(ctx) {
        const dir = ctx.path(ctx.home, '.config', 'demo-left');
        ctx.file.write(ctx.path(ctx.dataDir, 'inside'), 'x');
        ctx.file.copy(ctx.path(ctx.versionDir, 'xpack', 'hooks', 'i.js'), ctx.path(dir, 'copied.js'));
        ctx.file.write(ctx.path(dir, 'to-move'), 'x');
        ctx.file.move(ctx.path(dir, 'to-move'), ctx.path(dir, 'moved'));
        ctx.log.info('settings kept');
    }";
    let uninstall = "export function main(ctx) {
        ctx.file.write(ctx.path(ctx.home, '.config', 'demo-left', 'uninstall-log'), 'x');
    }";
    let project = Project::new(&serde_json::json!({}), &[]);
    let package = project.pack_version(
        "1.0.0",
        &serde_json::json!({
            "install": "xpack/hooks/i.js",
            "uninstall": "xpack/hooks/u.js",
            "permissions": { "user": { "write": ["{home}/.config"] } }
        }),
        &[("xpack/hooks/i.js", install), ("xpack/hooks/u.js", uninstall)],
    );
    let (ran, report) = project.hooks_test(&package, &[]);
    // What a hook says is shown named by its point, and the report's actions
    // are the hook's own: never how the engine said it ended.
    assert!(stderr(&ran).contains("[install.after] settings kept"), "{}", stderr(&ran));
    for point in report["points"].as_array().unwrap() {
        assert!(
            point["actions"].as_array().unwrap().iter().all(|a| a["kind"] != "ended"),
            "{point}"
        );
    }
    let left: Vec<String> = report["leftBehind"]
        .as_array()
        .unwrap_or_else(|| panic!("{report:#}"))
        .iter()
        .map(|p| p.as_str().unwrap().to_string())
        .collect();
    for expected in ["copied.js", "moved"] {
        assert!(left.iter().any(|p| p.ends_with(expected)), "{expected} not reported: {left:?}");
    }
    for unexpected in ["to-move", "uninstall-log", "inside"] {
        assert!(!left.iter().any(|p| p.ends_with(unexpected)), "{unexpected} reported: {left:?}");
    }
}

#[test]
fn hooks_test_in_real_mode_does_what_the_hooks_do() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let package = project.pack_version(
        "1.0.0",
        &serde_json::json!({ "install": "xpack/hooks/m.js", "uninstall": "xpack/hooks/m.js" }),
        &[("xpack/hooks/m.js", MARKS)],
    );
    let (ran, report) = project.hooks_test(&package, &["--real"]);
    assert!(ran.status.success(), "{}\n{report:#}", stderr(&ran));
    assert_eq!(report["mode"], "real");
    let write = &point(&report, "install.after")["actions"][0];
    assert_eq!(write["kind"], "write");
}

#[test]
fn a_package_for_another_platform_is_tested_where_it_will_run() {
    if !engine_is_built() {
        return;
    }
    // The other architecture of this system, which every host can build: a
    // Windows host cannot build for a system that needs Unix permissions.
    let host = xpack_core::Platform::host().unwrap();
    let other_arch = match host.arch {
        xpack_core::platform::Arch::X64 => xpack_core::platform::Arch::Arm64,
        xpack_core::platform::Arch::Arm64 => xpack_core::platform::Arch::X64,
    };
    let other = xpack_core::Platform::new(host.os, other_arch).to_string();
    let other = other.as_str();
    let project = Project::new(
        &serde_json::json!({ "install": "xpack/hooks/m.js", "uninstall": "xpack/hooks/m.js" }),
        &[("xpack/hooks/m.js", MARKS.as_bytes())],
    );
    let packed = project.run(&[
        "pack",
        "payload",
        "--key",
        "signing.json",
        "--out",
        "other.xpkg",
        "--platform",
        other,
    ]);
    assert!(packed.status.success(), "{}", stderr(&packed));
    let (ran, _) = project.hooks_test("other.xpkg", &[]);
    assert!(!ran.status.success());
    assert!(stderr(&ran).contains(&format!("tested on {other}")), "{}", stderr(&ran));
}

#[test]
fn hooks_test_tests_the_scope_the_permissions_are_written_for() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let package = project.pack_version(
        "1.0.0",
        &serde_json::json!({
            "install": "xpack/hooks/s.js",
            "uninstall": "xpack/hooks/s.js",
            "permissions": { "machine": { "exec": ["sc.exe"] } }
        }),
        &[("xpack/hooks/s.js", "export function main(ctx) { ctx.exec('sc.exe', ['query']); }")],
    );
    let (ran, report) = project.hooks_test(&package, &[]);
    assert_eq!(ran.status.code(), Some(1), "{report:#}");
    assert_eq!(report["scope"], "user");
    assert!(
        point(&report, "install.after")["reason"].as_str().unwrap().contains("not a program"),
        "{report:#}"
    );

    let (ran, report) = project.hooks_test(&package, &["--all-users"]);
    assert!(ran.status.success(), "{}\n{report:#}", stderr(&ran));
    assert_eq!(report["scope"], "machine");
}

#[cfg(unix)]
#[test]
fn only_real_mode_runs_the_programs_a_hook_runs() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let marker = project.path().join("touched");
    let script = format!(
        "export function main(ctx) {{ ctx.exec('/usr/bin/touch', [{}]); }}",
        serde_json::to_string(&marker.display().to_string()).unwrap()
    );
    let package = project.pack_version(
        "1.0.0",
        &serde_json::json!({
            "install": "xpack/hooks/t.js",
            "uninstall": "xpack/hooks/n.js",
            "permissions": { "user": { "exec": ["/usr/bin/touch"] } }
        }),
        &[("xpack/hooks/t.js", &script), ("xpack/hooks/n.js", "export function main() {}")],
    );
    let (ran, _) = project.hooks_test(&package, &[]);
    assert!(ran.status.success(), "{}", stderr(&ran));
    assert!(!marker.exists(), "plan mode ran a program");
    let (ran, _) = project.hooks_test(&package, &["--real"]);
    assert!(ran.status.success(), "{}", stderr(&ran));
    assert!(marker.exists(), "real mode did not run the program");
}

#[test]
fn the_previous_releases_own_hooks_are_not_counted_against_this_package() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    // The previous release's install hook is refused something, and
    // carries on: its business, not this package's.
    let previous = project.pack_version(
        "1.0.0",
        &serde_json::json!({ "install": "xpack/hooks/p.js" }),
        &[(
            "xpack/hooks/p.js",
            "export function main(ctx) { try { ctx.exec('undeclared'); } catch {} }",
        )],
    );
    let package =
        project.pack_version("2.0.0", &every_point(), &[("xpack/hooks/t.js", TOUCH_AND_RUN)]);
    let (ran, report) = project.hooks_test(&package, &["--previous", &previous]);
    assert!(ran.status.success(), "{}\n{report:#}", stderr(&ran));
    assert_eq!(point(&report, "install.after")["result"], "succeeded", "{report:#}");
}

// --- the release gate ---

const NOTHING: &str = "export function main() {}";

impl Project {
    fn installer(&self, package: &str) -> Output {
        let stub = xpack()
            .parent()
            .unwrap()
            .join(format!("xpack-installer{}", std::env::consts::EXE_SUFFIX));
        self.run(&[
            "installer",
            package,
            "--out",
            "Demo-installer",
            "--stub",
            &stub.to_string_lossy(),
        ])
    }

    fn index(&self, package: &str, extra: &[&str]) -> Output {
        let mut args = vec!["index", package, "--out-dir", "updates"];
        args.extend_from_slice(extra);
        self.run(&args)
    }
}

fn installer_parts_are_built() -> bool {
    let bin = xpack().parent().unwrap().to_path_buf();
    let suffix = std::env::consts::EXE_SUFFIX;
    let built = ["xpack-installer", "xpack-launcher", "xpack-updater", "xpack-uninstaller"]
        .iter()
        .all(|name| bin.join(format!("{name}{suffix}")).is_file());
    assert!(built || std::env::var_os("CI").is_none(), "the installer's programs are not built");
    built
}

fn simple_hooks() -> serde_json::Value {
    serde_json::json!({ "install": "xpack/hooks/n.js", "uninstall": "xpack/hooks/n.js" })
}

#[test]
fn an_installer_is_not_built_from_a_package_whose_hooks_were_never_tested() {
    if !engine_is_built() || !installer_parts_are_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let package = project.pack_version("1.0.0", &simple_hooks(), &[("xpack/hooks/n.js", NOTHING)]);
    let built = project.installer(&package);
    assert!(!built.status.success());
    let said = stderr(&built);
    assert!(said.contains("no report beside it"), "{said}");
    assert!(said.contains(&format!("xpack hooks test {package}")), "{said}");
    assert!(!project.path().join("Demo-installer").exists(), "an installer was written");

    let (tested, report) = project.hooks_test(&package, &[]);
    assert!(tested.status.success(), "{report:#}");
    let built = project.installer(&package);
    assert!(built.status.success(), "{}", stderr(&built));
}

#[test]
fn a_report_for_another_build_of_the_same_version_does_not_count() {
    if !engine_is_built() || !installer_parts_are_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let package = project.pack_version("1.0.0", &simple_hooks(), &[("xpack/hooks/n.js", NOTHING)]);
    assert!(project.hooks_test(&package, &[]).0.status.success());
    // The same version built again, with a script that is not the one tested.
    let rebuilt = project.pack_version(
        "1.0.0",
        &simple_hooks(),
        &[("xpack/hooks/n.js", "export function main() { /* changed */ }")],
    );
    assert_eq!(rebuilt, package);
    let built = project.installer(&package);
    assert!(!built.status.success());
    assert!(stderr(&built).contains("for another build"), "{}", stderr(&built));
}

#[test]
fn a_failed_report_does_not_count() {
    if !engine_is_built() || !installer_parts_are_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let package = project.pack_version(
        "1.0.0",
        &simple_hooks(),
        &[(
            "xpack/hooks/n.js",
            "export function main(ctx) { if (ctx.operation === 'install') throw new Error('no'); }",
        )],
    );
    assert!(!project.hooks_test(&package, &[]).0.status.success());
    let built = project.installer(&package);
    assert!(!built.status.success());
    assert!(stderr(&built).contains("failed: install.after"), "{}", stderr(&built));
}

#[test]
fn a_delta_ships_its_target_packages_hooks_tested_or_not_at_all() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let base = project.pack_version("1.0.0", &serde_json::json!({}), &[]);
    let target = project.pack_version("1.1.0", &simple_hooks(), &[("xpack/hooks/n.js", NOTHING)]);
    let delta = project.run(&["delta", &base, &target, "--out", "d.xpkgd"]);
    assert!(!delta.status.success());
    assert!(stderr(&delta).contains("no report beside it"), "{}", stderr(&delta));
    assert!(project.hooks_test(&target, &[]).0.status.success());
    let delta = project.run(&["delta", &base, &target, "--out", "d.xpkgd"]);
    assert!(delta.status.success(), "{}", stderr(&delta));
}

#[test]
fn an_index_needs_the_update_scenario_once_it_offers_an_earlier_release() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let first = project.pack_version("1.0.0", &serde_json::json!({}), &[]);
    let published = project.index(&first, &[]);
    assert!(published.status.success(), "{}", stderr(&published));

    let hooks = serde_json::json!({
        "update": "xpack/hooks/n.js",
        "rollback": "xpack/hooks/n.js",
    });
    let second = project.pack_version("1.1.0", &hooks, &[("xpack/hooks/n.js", NOTHING)]);
    assert!(project.hooks_test(&second, &[]).0.status.success());
    let refused = project.index(&second, &["--accept-hook-changes"]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("ran no update scenario"), "{}", stderr(&refused));

    assert!(project.hooks_test(&second, &["--previous", &first]).0.status.success());
    let published = project.index(&second, &["--accept-hook-changes"]);
    assert!(published.status.success(), "{}", stderr(&published));
}

#[test]
fn an_index_shows_and_refuses_hook_changes_unless_they_are_accepted() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let first = project.pack_version("1.0.0", &simple_hooks(), &[("xpack/hooks/n.js", NOTHING)]);
    assert!(project.hooks_test(&first, &[]).0.status.success());
    let published = project.index(&first, &[]);
    assert!(published.status.success(), "{}", stderr(&published));
    let index = std::fs::read_to_string(
        std::fs::read_dir(project.path().join("updates"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("stable.json"),
    )
    .unwrap();
    assert!(index.contains("\"hooks\""), "the index does not record the release's hooks: {index}");

    // The same hooks again: nothing to accept.
    let same = project.pack_version("1.0.1", &simple_hooks(), &[("xpack/hooks/n.js", NOTHING)]);
    assert!(project.hooks_test(&same, &[]).0.status.success());
    let published = project.index(&same, &[]);
    assert!(published.status.success(), "{}", stderr(&published));

    // A changed script and a new permission.
    let mut changed = simple_hooks();
    changed["permissions"] = serde_json::json!({ "user": { "exec": ["agentctl"] } });
    let next = project.pack_version(
        "1.0.2",
        &changed,
        &[("xpack/hooks/n.js", "export function main() { /* v2 */ }")],
    );
    assert!(project.hooks_test(&next, &[]).0.status.success());
    let refused = project.index(&next, &[]);
    assert!(!refused.status.success());
    let said = stderr(&refused);
    assert!(said.contains("xpack/hooks/n.js changed"), "{said}");
    assert!(said.contains("new permission, user: may run agentctl"), "{said}");
    assert!(said.contains("--accept-hook-changes"), "{said}");

    let accepted = project.index(&next, &["--accept-hook-changes"]);
    assert!(accepted.status.success(), "{}", stderr(&accepted));
    assert!(
        stderr(&accepted).contains("new permission, user: may run agentctl"),
        "{}",
        stderr(&accepted)
    );
}

#[test]
fn a_mandatory_release_ships_only_after_a_real_run() {
    if !engine_is_built() || !installer_parts_are_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let config = serde_json::json!({
        "application": { "id": "com.example.demo", "name": "Demo", "version": "1.0.0" },
        "launch": { "executable": "bin/app" },
        "update": { "mandatory": true },
        "hooks": simple_hooks(),
    });
    std::fs::create_dir_all(project.path().join("payload/xpack/hooks")).unwrap();
    std::fs::write(project.path().join("payload/xpack/hooks/n.js"), NOTHING).unwrap();
    std::fs::write(project.path().join("xpack.json"), config.to_string()).unwrap();
    assert!(project.pack().status.success());

    assert!(project.hooks_test("demo.xpkg", &[]).0.status.success());
    let built = project.installer("demo.xpkg");
    assert!(!built.status.success());
    assert!(stderr(&built).contains("--real"), "{}", stderr(&built));

    assert!(project.hooks_test("demo.xpkg", &["--real"]).0.status.success());
    let built = project.installer("demo.xpkg");
    assert!(built.status.success(), "{}", stderr(&built));
}

#[test]
fn a_sealed_package_is_tested_and_gated_like_any_other() {
    if !engine_is_built() || !installer_parts_are_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let config = serde_json::json!({
        "application": { "id": "com.example.demo", "name": "Demo", "version": "1.0.0" },
        "launch": { "executable": "bin/app" },
        "protection": { "installer": true, "packages": true },
        "hooks": simple_hooks(),
    });
    std::fs::create_dir_all(project.path().join("payload/xpack/hooks")).unwrap();
    std::fs::write(project.path().join("payload/xpack/hooks/n.js"), NOTHING).unwrap();
    std::fs::write(project.path().join("xpack.json"), config.to_string()).unwrap();
    let with_password = |args: &[&str]| {
        project
            .command(&xpack())
            .env("XPACK_PASSWORD", "correct horse battery staple")
            .args(args)
            .output()
            .unwrap()
    };
    let packed = with_password(&["pack", "payload", "--key", "signing.json", "--out", "demo.xpkg"]);
    assert!(packed.status.success(), "{}", stderr(&packed));
    let stub =
        xpack().parent().unwrap().join(format!("xpack-installer{}", std::env::consts::EXE_SUFFIX));
    let stub = stub.to_string_lossy().into_owned();
    let installer = ["installer", "demo.xpkg", "--out", "Demo-installer", "--stub", stub.as_str()];
    let built = with_password(&installer);
    assert!(!built.status.success());
    assert!(stderr(&built).contains("no report beside it"), "{}", stderr(&built));

    let tested = with_password(&["hooks", "test", "demo.xpkg"]);
    assert!(tested.status.success(), "{}", stderr(&tested));
    assert!(project.path().join("demo.hooks-report.json").is_file());
    let built = with_password(&installer);
    assert!(built.status.success(), "{}", stderr(&built));
}

impl Project {
    /// `xpack installer` with installer settings `ui`.
    fn installer_with(&self, package: &str, ui: &serde_json::Value) -> Output {
        std::fs::write(self.path().join("installer-ui.json"), ui.to_string()).unwrap();
        let stub = xpack()
            .parent()
            .unwrap()
            .join(format!("xpack-installer{}", std::env::consts::EXE_SUFFIX));
        self.run(&[
            "installer",
            package,
            "--out",
            "Demo-installer",
            "--stub",
            &stub.to_string_lossy(),
            "--ui",
            "installer-ui.json",
        ])
    }
}

#[test]
fn an_installer_for_everyone_as_well_needs_both_reports_and_one_only_for_everyone_needs_that_one() {
    if !engine_is_built() || !installer_parts_are_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let package = project.pack_version("1.0.0", &simple_hooks(), &[("xpack/hooks/n.js", NOTHING)]);
    assert!(project.hooks_test(&package, &[]).0.status.success());

    let offer = serde_json::json!({ "allUsers": "offer" });
    let built = project.installer_with(&package, &offer);
    assert!(!built.status.success());
    assert!(stderr(&built).contains("--all-users"), "{}", stderr(&built));
    assert!(stderr(&built).contains("for an installation for everyone"), "{}", stderr(&built));

    assert!(project.hooks_test(&package, &["--all-users"]).0.status.success());
    let built = project.installer_with(&package, &offer);
    assert!(built.status.success(), "{}", stderr(&built));

    // Only for everyone: the one user's report is not needed.
    std::fs::remove_file(project.path().join(package.replace(".xpkg", ".hooks-report.json")))
        .unwrap();
    let built = project.installer_with(&package, &serde_json::json!({ "allUsers": "always" }));
    assert!(built.status.success(), "{}", stderr(&built));
}

#[test]
fn an_index_written_afresh_compares_with_the_published_tree_named_by_current() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let first = project.pack_version("1.0.0", &simple_hooks(), &[("xpack/hooks/n.js", NOTHING)]);
    assert!(project.hooks_test(&first, &[]).0.status.success());
    let published = project.run(&["index", &first, "--out-dir", "published"]);
    assert!(published.status.success(), "{}", stderr(&published));

    let mut hooks = simple_hooks();
    hooks["update"] = serde_json::json!("xpack/hooks/n.js");
    hooks["rollback"] = serde_json::json!("xpack/hooks/n.js");
    hooks["permissions"] = serde_json::json!({ "user": { "exec": ["agentctl"] } });
    let second = project.pack_version("1.1.0", &hooks, &[("xpack/hooks/n.js", NOTHING)]);
    assert!(project.hooks_test(&second, &[]).0.status.success());

    // A fresh directory and no --current: written, with a warning that
    // nothing was compared, naming what the hooks may do.
    let blind = project.run(&["index", &second, "--out-dir", "fresh-a"]);
    assert!(blind.status.success(), "{}", stderr(&blind));
    assert!(stderr(&blind).contains("nothing was compared"), "{}", stderr(&blind));
    assert!(stderr(&blind).contains("user: run agentctl"), "{}", stderr(&blind));

    // With --current, the update scenario is required, as an earlier release
    // is published...
    let refused = project.run(&[
        "index",
        &second,
        "--out-dir",
        "fresh-b",
        "--current",
        "published",
        "--accept-hook-changes",
    ]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("ran no update scenario"), "{}", stderr(&refused));
    // ...and the changes are compared and refused unless accepted.
    assert!(project.hooks_test(&second, &["--previous", &first]).0.status.success());
    let refused =
        project.run(&["index", &second, "--out-dir", "fresh-c", "--current", "published"]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("new permission, user: may run agentctl"),
        "{}",
        stderr(&refused)
    );
    let accepted = project.run(&[
        "index",
        &second,
        "--out-dir",
        "fresh-d",
        "--current",
        "published",
        "--accept-hook-changes",
    ]);
    assert!(accepted.status.success(), "{}", stderr(&accepted));
}

#[test]
fn current_must_be_a_directory_or_an_https_url() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let package = project.pack_version("1.0.0", &simple_hooks(), &[("xpack/hooks/n.js", NOTHING)]);
    assert!(project.hooks_test(&package, &[]).0.status.success());
    let refused = project.run(&[
        "index",
        &package,
        "--out-dir",
        "o",
        "--current",
        "http://example.com/updates",
    ]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("neither a directory nor an https URL"),
        "{}",
        stderr(&refused)
    );
}

#[test]
fn a_delta_of_a_package_with_update_hooks_needs_the_update_scenario() {
    if !engine_is_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let base = project.pack_version("1.0.0", &serde_json::json!({}), &[]);
    let mut hooks = simple_hooks();
    hooks["update"] = serde_json::json!("xpack/hooks/n.js");
    hooks["rollback"] = serde_json::json!("xpack/hooks/n.js");
    let target = project.pack_version("1.1.0", &hooks, &[("xpack/hooks/n.js", NOTHING)]);
    assert!(project.hooks_test(&target, &[]).0.status.success());
    let delta = project.run(&["delta", &base, &target, "--out", "d.xpkgd"]);
    assert!(!delta.status.success());
    assert!(stderr(&delta).contains("ran no update scenario"), "{}", stderr(&delta));
    assert!(project.hooks_test(&target, &["--previous", &base]).0.status.success());
    let delta = project.run(&["delta", &base, &target, "--out", "d.xpkgd"]);
    assert!(delta.status.success(), "{}", stderr(&delta));
}

#[test]
fn a_report_of_the_other_scope_under_this_ones_name_does_not_count() {
    if !engine_is_built() || !installer_parts_are_built() {
        return;
    }
    let project = Project::new(&serde_json::json!({}), &[]);
    let package = project.pack_version("1.0.0", &simple_hooks(), &[("xpack/hooks/n.js", NOTHING)]);
    assert!(project.hooks_test(&package, &["--all-users"]).0.status.success());
    std::fs::copy(
        project.path().join(package.replace(".xpkg", ".hooks-report.all-users.json")),
        project.path().join(package.replace(".xpkg", ".hooks-report.json")),
    )
    .unwrap();
    let built = project.installer(&package);
    assert!(!built.status.success());
    assert!(stderr(&built).contains("tested the other scope"), "{}", stderr(&built));
}

/// Every command that ships a version refuses each kind of report that does
/// not count: none, one for another build of the same version, and a failed
/// one. Each command finds and checks the report itself, so each is tried.
#[test]
fn installer_index_and_delta_each_refuse_every_report_that_does_not_count() {
    if !engine_is_built() || !installer_parts_are_built() {
        return;
    }
    let failing =
        "export function main(ctx) { if (ctx.operation === 'install') throw new Error('no'); }";
    let cases: [(&str, &str, &str, bool); 3] = [
        ("no report", NOTHING, "no report beside it", false),
        ("another build", NOTHING, "for another build", true),
        // Names the point that failed, and only that one.
        ("a failed report", failing, "failed: install.after)", false),
    ];
    for (case, script, said, rebuild) in cases {
        let project = Project::new(&serde_json::json!({}), &[]);
        let base = project.pack_version("1.0.0", &serde_json::json!({}), &[]);
        let package =
            project.pack_version("1.1.0", &simple_hooks(), &[("xpack/hooks/n.js", script)]);
        if case != "no report" {
            project.hooks_test(&package, &[]);
        }
        if rebuild {
            let again = project.pack_version(
                "1.1.0",
                &simple_hooks(),
                &[("xpack/hooks/n.js", "export function main() { /* not what was tested */ }")],
            );
            assert_eq!(again, package);
        }
        let runs = [
            ("installer", project.installer(&package)),
            ("index", project.index(&package, &[])),
            ("delta", project.run(&["delta", &base, &package, "--out", "d.xpkgd"])),
        ];
        for (command, output) in runs {
            assert!(!output.status.success(), "{case}: xpack {command} shipped it");
            assert!(stderr(&output).contains(said), "{case}: xpack {command}: {}", stderr(&output));
        }
        assert!(!project.path().join("d.xpkgd").exists(), "{case}: a delta was written");
    }
}
