//! `cargo xpack`, driven the way a Rust developer would drive it: a real Cargo
//! project, packed by the real subcommand, installed and run by the real
//! `xpack`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn cargo_xpack() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cargo-xpack"))
}

fn xpack() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_xpack"))
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A tiny Cargo project whose binary prints its arguments and where it runs.
fn project(dir: &Path, metadata: &str) -> PathBuf {
    let root = dir.join("mytool");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("assets")).unwrap();
    std::fs::write(root.join("assets/greeting.txt"), "hello\n").unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        format!(
            r#"[package]
name = "mytool"
version = "1.2.0"
edition = "2021"
description = "A tool packed by cargo xpack"
authors = ["Example Ltd <dev@example.com>"]

[package.metadata.xpack]
{metadata}

[workspace]
"#
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        r#"fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    println!("mytool args={}", args.join(" "));
    println!("cwd={}", std::env::current_dir().unwrap().display());
}
"#,
    )
    .unwrap();
    root
}

fn keygen(dir: &Path) -> PathBuf {
    let key = dir.join("keys/signing.json");
    let out = Command::new(xpack()).args(["keygen", "--out"]).arg(&key).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    key
}

fn cargo_xpack_pack(project: &Path, key: &Path) -> Output {
    Command::new(cargo_xpack())
        .args(["xpack", "pack", "--manifest-path"])
        .arg(project.join("Cargo.toml"))
        .arg("--key")
        .arg(key)
        .arg("--xpack")
        .arg(xpack())
        // Every test project is named `mytool`. A target directory shared
        // through the environment would have the tests running at once
        // overwrite each other's build, so each project keeps its own.
        .env("CARGO_TARGET_DIR", project.join("target"))
        .output()
        .unwrap()
}

#[test]
fn a_cargo_project_without_an_id_is_refused_with_how_to_fix_it() {
    let dir = tempfile::tempdir().unwrap();
    let project = project(dir.path(), r#"command = "mytool""#);
    let key = keygen(dir.path());
    let out = cargo_xpack_pack(&project, &key);
    assert!(!out.status.success());
    assert!(stderr(&out).contains(r#"id = "com.example.mytool""#), "{}", stderr(&out));
}

#[test]
fn a_cargo_project_is_packed_with_what_cargo_knows() {
    let dir = tempfile::tempdir().unwrap();
    let project = project(
        dir.path(),
        r#"id = "com.example.mytool"
command = "mytool"
resources = ["assets"]"#,
    );
    let key = keygen(dir.path());
    let out = cargo_xpack_pack(&project, &key);
    assert!(out.status.success(), "{}", stderr(&out));
    let package = PathBuf::from(stdout(&out).trim());
    assert!(package.is_file(), "no package at {}", package.display());

    let inspect = Command::new(xpack()).args(["inspect", "--json"]).arg(&package).output().unwrap();
    assert!(inspect.status.success(), "{}", stderr(&inspect));
    let text = stdout(&inspect);
    for expected in ["com.example.mytool", "1.2.0", "Example Ltd", "A tool packed by cargo xpack"] {
        assert!(text.contains(expected), "{expected} missing from {text}");
    }
    assert!(text.contains("assets/greeting.txt"), "the resource was not packed: {text}");
}

#[test]
fn a_packed_rust_tool_installs_and_is_typed_by_name() {
    // Unix only: installing puts the command in a `HOME` redirected here; on
    // Windows it would edit the real user's PATH.
    if cfg!(not(unix)) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let project = project(
        dir.path(),
        r#"id = "com.example.mytool"
command = "mytool""#,
    );
    let key = keygen(dir.path());
    let out = cargo_xpack_pack(&project, &key);
    assert!(out.status.success(), "{}", stderr(&out));
    let package = PathBuf::from(stdout(&out).trim());

    let home = dir.path().join("home");
    let install = Command::new(xpack())
        .arg("--root")
        .arg(dir.path().join("apps"))
        .arg("install")
        .arg(&package)
        .arg("--trust")
        .arg(key.with_file_name("signing.pub.json"))
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(install.status.success(), "{}", stderr(&install));

    // Typed by name from somewhere else: it starts there, with its argument.
    let elsewhere = dir.path().join("work");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let run = Command::new(home.join(".local/bin/mytool"))
        .arg("build")
        .current_dir(&elsewhere)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", stderr(&run));
    let text = stdout(&run);
    assert!(text.contains("mytool args=build"), "{text}");
    let cwd = text.lines().find_map(|l| l.strip_prefix("cwd=")).unwrap();
    assert_eq!(std::fs::canonicalize(cwd).unwrap(), std::fs::canonicalize(&elsewhere).unwrap());
}
