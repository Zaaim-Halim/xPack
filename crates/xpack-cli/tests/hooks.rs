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
        Command::new(program).current_dir(self.path()).args(args).output().unwrap()
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
    let child = Command::new(xpack())
        .current_dir(project.path())
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
        &serde_json::json!({ "install": "xpack/hooks/fail.js" }),
        &[(
            "xpack/hooks/fail.js",
            "export function main() { throw new Error('refused by the hook'); }",
        )],
    );
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
    let ran = Command::new(&installer)
        .args(["--root", &root.to_string_lossy(), "--silent"])
        .output()
        .unwrap();
    assert_eq!(ran.status.code(), Some(7), "{}", stderr(&ran));
    assert!(!root.join("com.example.demo").exists(), "the failed install left something behind");
}
