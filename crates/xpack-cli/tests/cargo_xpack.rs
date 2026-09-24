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
fn the_help_describes_cargo_xpack_rather_than_xpack() {
    // Both binaries live in one crate, so a help text taken from the crate's
    // description would describe `xpack` here too.
    let out = Command::new(cargo_xpack()).args(["xpack", "--help"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let help = stdout(&out);
    assert!(help.starts_with("Package a Cargo project with xPack"), "{help}");
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
fn only_the_icon_for_the_platform_built_is_packed() {
    let dir = tempfile::tempdir().unwrap();
    let project = project(
        dir.path(),
        r#"id = "com.example.mytool"
icon = { macos = "art/mytool.icns", windows = "art/mytool.ico", linux = "art/mytool.png" }"#,
    );
    // xPack's own icons: real files in each platform's format.
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    std::fs::create_dir_all(project.join("art")).unwrap();
    for extension in ["icns", "ico", "png"] {
        std::fs::copy(
            assets.join(format!("xpack.{extension}")),
            project.join(format!("art/mytool.{extension}")),
        )
        .unwrap();
    }
    let key = keygen(dir.path());
    let out = cargo_xpack_pack(&project, &key);
    assert!(out.status.success(), "{}", stderr(&out));
    let package = PathBuf::from(stdout(&out).trim());

    let inspect = Command::new(xpack()).args(["inspect", "--json"]).arg(&package).output().unwrap();
    assert!(inspect.status.success(), "{}", stderr(&inspect));
    let text = stdout(&inspect);
    let wanted = match std::env::consts::OS {
        "macos" => "art/mytool.icns",
        "windows" => "art/mytool.ico",
        _ => "art/mytool.png",
    };
    for icon in ["art/mytool.icns", "art/mytool.ico", "art/mytool.png"] {
        assert_eq!(text.contains(icon), icon == wanted, "{icon} in {text}");
    }
}

#[test]
fn an_icon_for_a_platform_that_does_not_exist_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let project = project(
        dir.path(),
        r#"id = "com.example.mytool"
icon = { macOS = "art/mytool.icns" }"#,
    );
    let key = keygen(dir.path());
    let out = cargo_xpack_pack(&project, &key);
    assert!(!out.status.success());
    assert!(stderr(&out).contains(r#"icon: "macOS" is not a platform"#), "{}", stderr(&out));
}

/// The manifest of a packed package, as `xpack inspect --json` reports it.
fn inspect(package: &Path) -> serde_json::Value {
    let out = Command::new(xpack()).args(["inspect", "--json"]).arg(package).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn the_update_url_names_the_platform_built_and_keeps_the_channel() {
    let dir = tempfile::tempdir().unwrap();
    let project = project(
        dir.path(),
        r#"id = "com.example.mytool"
update-url = "https://updates.example.com/mytool/{platform}"
update-channel = "beta""#,
    );
    let key = keygen(dir.path());
    let out = cargo_xpack_pack(&project, &key);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!stderr(&out).contains("update-url has no"), "{}", stderr(&out));
    let report = inspect(Path::new(stdout(&out).trim()));

    let platform = xpack_core::Platform::host().unwrap();
    let update = &report["update"];
    assert_eq!(
        update["url"],
        format!("https://updates.example.com/mytool/{platform}").as_str(),
        "{report:#}"
    );
    assert_eq!(update["channel"], "beta", "{report:#}");
}

#[test]
fn without_an_update_url_the_package_checks_nowhere() {
    let dir = tempfile::tempdir().unwrap();
    let project = project(dir.path(), r#"id = "com.example.mytool""#);
    let key = keygen(dir.path());
    let out = cargo_xpack_pack(&project, &key);
    assert!(out.status.success(), "{}", stderr(&out));
    let report = inspect(Path::new(stdout(&out).trim()));
    // The report is the manifest itself; its id proves it was read.
    assert_eq!(report["application"]["id"], "com.example.mytool", "{report:#}");
    assert!(report["update"].get("url").is_none(), "{report:#}");
}

#[test]
fn an_update_channel_without_an_update_url_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let project = project(
        dir.path(),
        r#"id = "com.example.mytool"
update-channel = "beta""#,
    );
    let key = keygen(dir.path());
    let out = cargo_xpack_pack(&project, &key);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("update-url is not"), "{}", stderr(&out));
}

#[test]
fn an_update_url_that_is_not_https_is_refused() {
    // Updates are code the installation will run; a plain-HTTP index could be
    // swapped in transit to hold every installation back on an old version.
    let dir = tempfile::tempdir().unwrap();
    let project = project(
        dir.path(),
        r#"id = "com.example.mytool"
update-url = "http://updates.example.com/mytool/{platform}""#,
    );
    let key = keygen(dir.path());
    let out = cargo_xpack_pack(&project, &key);
    assert!(!out.status.success(), "an http update URL was packed");
    assert!(stderr(&out).contains("https"), "{}", stderr(&out));
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
