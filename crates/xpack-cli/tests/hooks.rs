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
