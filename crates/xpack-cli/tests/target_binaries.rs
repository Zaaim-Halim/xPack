//! An installer for another platform than the machine building it.
//!
//! It must carry that platform's programs, chosen by the same rules as an
//! installer for this machine. Before `--target-binaries`, a stub alone for
//! another platform made an installer of this machine's programs: a Linux
//! installer built on macOS installed macOS launchers that could not start.
//!
//! The programs are stand-ins: the first bytes of a real program of the
//! right kind, enough for every check here, since building an installer
//! never runs them.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn xpack() -> Command {
    Command::new(env!("CARGO_BIN_EXE_xpack"))
}

fn run(command: &mut Command) -> String {
    let out = command.output().expect("xpack should run");
    assert!(out.status.success(), "{:?} failed: {}", command, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The first bytes of a Linux program for `arch`.
fn elf(arch: &str) -> Vec<u8> {
    let machine: u16 = if arch == "arm64" { 0xB7 } else { 0x3E };
    let mut bytes = vec![0u8; 64];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    bytes[0x12..0x14].copy_from_slice(&machine.to_le_bytes());
    bytes
}

/// The first bytes of an Apple Silicon macOS program.
fn mach_o_arm64() -> Vec<u8> {
    let mut bytes = vec![0u8; 32];
    bytes[..4].copy_from_slice(&0xFEED_FACFu32.to_le_bytes());
    bytes[4..8].copy_from_slice(&0x0100_000Cu32.to_le_bytes());
    bytes
}

/// A Linux platform that is not this machine, so the installer is always
/// for another platform than the one building it.
fn other_linux() -> (&'static str, &'static str) {
    if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
        ("linux-arm64", "arm64")
    } else {
        ("linux-x64", "x64")
    }
}

/// A package for `os`-`arch` whose payload is a stand-in application.
fn package(dir: &Path, os: &str, arch: &str) -> PathBuf {
    let payload = dir.join("payload");
    std::fs::create_dir_all(&payload).unwrap();
    std::fs::write(payload.join("app"), b"an application").unwrap();
    let config = dir.join("xpack.json");
    std::fs::write(
        &config,
        format!(
            r#"{{
              "application": {{ "id": "com.example.cross", "name": "Cross", "version": "1.0.0" }},
              "platform": {{ "os": "{os}", "arch": "{arch}" }},
              "launch": {{ "executable": "app" }}
            }}"#
        ),
    )
    .unwrap();
    run(xpack().args(["keygen", "--out"]).arg(dir.join("key.json")));
    run(xpack()
        .arg("pack")
        .arg(&payload)
        .args(["--config"])
        .arg(&config)
        .args(["--key"])
        .arg(dir.join("key.json"))
        .args(["--out-dir"])
        .arg(dir));
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "xpkg"))
        .expect("a package was written")
}

/// A release folder holding `names`, each written with `contents`.
fn release_folder(dir: &Path, names: &[&str], contents: &[u8]) -> PathBuf {
    let folder = dir.join("release");
    std::fs::create_dir_all(&folder).unwrap();
    for name in names {
        std::fs::write(folder.join(name), contents).unwrap();
    }
    folder
}

/// The programs an installer carries, by name, with their bytes.
fn carried(installer: &Path) -> BTreeMap<String, Vec<u8>> {
    let source = xpack_installer::bundle::locate(installer).unwrap();
    let payload = xpack_installer::bundle::read(installer, &source).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(payload)).unwrap();
    let mut programs = BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        if let Some(name) = entry.name().strip_prefix("bin/").map(str::to_string) {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            programs.insert(name, bytes);
        }
    }
    programs
}

#[test]
fn a_stub_alone_for_another_platform_no_longer_ships_this_machines_programs() {
    // The bug: this succeeded and the installer carried this machine's
    // launcher, updater, uninstaller and hook runner.
    let dir = tempfile::tempdir().unwrap();
    let (platform, arch) = other_linux();
    let package = package(dir.path(), "linux", arch);
    let stub = dir.path().join("xpack-installer");
    std::fs::write(&stub, elf(arch)).unwrap();

    let output = xpack()
        .arg("installer")
        .arg(&package)
        .arg("--stub")
        .arg(&stub)
        .arg("--out")
        .arg(dir.path().join("installer"))
        .output()
        .unwrap();
    assert!(!output.status.success(), "an installer of the wrong programs was built");
    assert!(stderr(&output).contains("--target-binaries"), "{}", stderr(&output));
    assert!(stderr(&output).contains(platform), "{}", stderr(&output));
    assert!(!dir.path().join("installer").exists());
}

#[test]
fn a_release_folder_supplies_the_stub_and_the_programs_the_rules_choose() {
    let dir = tempfile::tempdir().unwrap();
    let (_, arch) = other_linux();
    let package = package(dir.path(), "linux", arch);
    let program = elf(arch);
    // More than the installer needs, as a real release holds: the rules, not
    // the folder, decide what goes in.
    let folder = release_folder(
        dir.path(),
        &[
            "xpack-installer",
            "xpack-launcher",
            "xpack-updater",
            "xpack-uninstaller",
            "xpack-hook",
            "xpack-notify",
            "xpack",
        ],
        &program,
    );

    let installer = dir.path().join("installer");
    run(xpack()
        .arg("installer")
        .arg(&package)
        .arg("--target-binaries")
        .arg(&folder)
        .arg("--out")
        .arg(&installer));

    let programs = carried(&installer);
    assert_eq!(
        programs.keys().map(String::as_str).collect::<Vec<_>>(),
        ["xpack-hook", "xpack-launcher", "xpack-uninstaller", "xpack-updater"],
        "a Linux installer carries the hook runner every time, and no notice"
    );
    assert!(programs.values().all(|bytes| *bytes == program));
    assert_eq!(std::fs::read(&installer).unwrap()[..64], program[..], "built on the folder's stub");
}

#[test]
fn a_windows_release_folder_is_read_with_windows_names() {
    // Named as Windows names its programs, whatever machine builds; the
    // windowed stub and launcher included.
    let dir = tempfile::tempdir().unwrap();
    let package = package(dir.path(), "windows", "x64");
    let pe = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../xpack-installer/tests/signed/unsigned.exe"),
    )
    .unwrap()[..1024]
        .to_vec();
    let folder = release_folder(
        dir.path(),
        &[
            "xpack-installerw.exe",
            "xpack-launcher.exe",
            "xpack-launcherw.exe",
            "xpack-updater.exe",
            "xpack-uninstaller.exe",
            "xpack-hook.exe",
        ],
        &pe,
    );

    let installer = dir.path().join("Setup.exe");
    run(xpack()
        .arg("installer")
        .arg(&package)
        .arg("--target-binaries")
        .arg(&folder)
        .arg("--out")
        .arg(&installer));

    let programs = carried(&installer);
    for name in ["xpack-launcher.exe", "xpack-launcherw.exe", "xpack-hook.exe"] {
        assert!(programs.contains_key(name), "{name} missing from {:?}", programs.keys());
    }
}

#[test]
fn a_release_folder_for_another_processor_is_refused_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let (platform, arch) = other_linux();
    let package = package(dir.path(), "linux", arch);
    let wrong = if arch == "x64" { "arm64" } else { "x64" };
    let folder = release_folder(
        dir.path(),
        &["xpack-installer", "xpack-launcher", "xpack-updater", "xpack-uninstaller", "xpack-hook"],
        &elf(wrong),
    );

    let output = xpack()
        .arg("installer")
        .arg(&package)
        .arg("--target-binaries")
        .arg(&folder)
        .arg("--out")
        .arg(dir.path().join("installer"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains(&format!("built for {wrong}")), "{}", stderr(&output));
    assert!(stderr(&output).contains(platform), "{}", stderr(&output));
}

#[test]
fn a_program_missing_from_the_release_folder_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let (_, arch) = other_linux();
    let package = package(dir.path(), "linux", arch);
    let folder = release_folder(
        dir.path(),
        &["xpack-installer", "xpack-launcher", "xpack-updater", "xpack-uninstaller"],
        &elf(arch),
    );

    let output = xpack()
        .arg("installer")
        .arg(&package)
        .arg("--target-binaries")
        .arg(&folder)
        .arg("--out")
        .arg(dir.path().join("installer"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("has no xpack-hook"), "{}", stderr(&output));
}

#[test]
fn a_program_for_another_os_given_by_hand_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (_, arch) = other_linux();
    let package = package(dir.path(), "linux", arch);
    let stub = dir.path().join("xpack-installer");
    std::fs::write(&stub, elf(arch)).unwrap();
    let launcher = dir.path().join("xpack-launcher");
    std::fs::write(&launcher, mach_o_arm64()).unwrap();

    let output = xpack()
        .arg("installer")
        .arg(&package)
        .arg("--stub")
        .arg(&stub)
        .arg("--binary")
        .arg(&launcher)
        .arg("--out")
        .arg(dir.path().join("installer"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("is a macos program"), "{}", stderr(&output));
}

#[test]
fn a_release_folder_cannot_be_combined_with_a_stub_or_binaries() {
    let output = xpack()
        .args(["installer", "x.xpkg", "--target-binaries", "dir", "--stub", "s"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("cannot be used with"), "{}", stderr(&output));
}
