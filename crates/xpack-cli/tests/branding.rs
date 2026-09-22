//! A Windows installer carries the application's name and icon.
//!
//! A PE cannot be linked on every machine this suite runs on — it needs
//! either the Windows SDK or a mingw cross toolchain — so the executable to
//! practise on is supplied rather than built. `XPACK_TEST_PE` names one.
//! Without it there is nothing to brand, and the test says so rather than
//! passing quietly.
//!
//! One can be produced anywhere `gcc-mingw-w64` is installed:
//!
//! ```sh
//! cargo build --target x86_64-pc-windows-gnu
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

fn windows_executable() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("XPACK_TEST_PE")?);
    path.is_file().then_some(path)
}

fn xpack() -> Command {
    Command::new(env!("CARGO_BIN_EXE_xpack"))
}

fn run(command: &mut Command) -> String {
    let out = command.output().expect("xpack should run");
    assert!(out.status.success(), "{:?} failed: {}", command, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A Windows package whose payload is a stand-in for a real application.
fn windows_package(dir: &Path, stub: &Path) -> PathBuf {
    let payload = dir.join("payload");
    std::fs::create_dir_all(&payload).unwrap();
    std::fs::copy(stub, payload.join("app.exe")).unwrap();

    let config = dir.join("xpack.json");
    std::fs::write(
        &config,
        br#"{
          "application": {
            "id": "com.example.demo",
            "name": "My App",
            "version": "1.2.3",
            "publisher": "Example Ltd"
          },
          "platform": { "os": "windows", "arch": "x64" },
          "launch": { "executable": "app.exe" }
        }"#,
    )
    .unwrap();

    run(xpack().args(["keygen", "--out"]).arg(dir.join("key.json")));
    run(xpack()
        .arg("pack")
        .arg(&payload)
        .arg("--config")
        .arg(&config)
        .arg("--key")
        .arg(dir.join("key.json"))
        .arg("--out-dir")
        .arg(dir));

    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "xpkg"))
        .expect("a package was written")
}

#[test]
fn a_windows_installer_carries_the_application_name_and_icon() {
    let Some(stub) = windows_executable() else {
        eprintln!("skipped: set XPACK_TEST_PE to a Windows executable to run this");
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let package = windows_package(dir.path(), &stub);

    let icon = dir.path().join("icon.png");
    std::fs::write(&icon, ONE_PIXEL_PNG).unwrap();

    let out = dir.path().join("out");
    run(xpack()
        .arg("installer")
        .arg(&package)
        .arg("--stub")
        .arg(&stub)
        .args(["--binary"])
        .arg(&stub)
        .arg("--icon")
        .arg(&icon)
        .arg("--out-dir")
        .arg(&out));

    let installer = std::fs::read_dir(&out)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "exe"))
        .expect("an installer was written");

    // Still a PE after being rewritten and having a payload appended. An
    // installer that no longer parses is the failure this whole path risks.
    let image = editpe::Image::parse_file(&installer).expect("still a valid PE");
    let resources = image.resource_directory().expect("a resource directory");

    let info = resources.get_version_info().unwrap().expect("version information");
    let strings = &info.strings[0].strings;

    // What Explorer's properties dialog and Task Manager show.
    assert_eq!(strings.get("FileDescription").unwrap(), "Install My App");
    assert_eq!(strings.get("ProductName").unwrap(), "My App");
    assert_eq!(strings.get("CompanyName").unwrap(), "Example Ltd");
    assert_eq!(strings.get("FileVersion").unwrap(), "1.2.3");
    assert!(
        !strings.get("ProductName").unwrap().contains("xpack"),
        "the installer still names xPack rather than the application"
    );

    assert!(resources.get_main_icon().unwrap().is_some(), "no icon was written");

    // What Windows reads before the program runs: never elevated, and drawn
    // at the display's real DPI rather than stretched.
    let manifest = resources.get_manifest().unwrap().expect("a manifest");
    assert!(manifest.contains(r#"level="asInvoker""#), "{manifest}");
    assert!(manifest.contains(">true</dpiAware>"), "{manifest}");
}

#[test]
fn branding_leaves_the_payload_readable() {
    // Rewriting a resource section moves bytes. Doing it after the payload
    // was appended would leave the trailer pointing into the wrong place, so
    // the installer has to still describe the package it carries.
    let Some(stub) = windows_executable() else {
        eprintln!("skipped: set XPACK_TEST_PE to a Windows executable to run this");
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let package = windows_package(dir.path(), &stub);
    let out = dir.path().join("out");

    let reported = run(xpack()
        .arg("installer")
        .arg(&package)
        .arg("--stub")
        .arg(&stub)
        .args(["--binary"])
        .arg(&stub)
        .arg("--out-dir")
        .arg(&out)
        .arg("--json"));

    assert!(reported.contains("\"layout\": \"executable\""), "{reported}");
    assert!(reported.contains("com.example.demo"), "{reported}");
}

/// A 1x1 transparent PNG, so no binary fixture is needed in the repository.
const ONE_PIXEL_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];
