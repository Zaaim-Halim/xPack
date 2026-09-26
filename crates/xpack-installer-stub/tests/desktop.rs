//! The installer puts a shortcut on the desktop beside the desktop entry,
//! says so, and takes no for an answer.
//!
//! Unix only, with `HOME` pointed at a temporary directory: the entry, the
//! shortcut and the desktop they go on are all inside it. On Windows they
//! would go into the real user's Start Menu and desktop, which a test must
//! not touch; the installation library's own tests cover those paths.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use xpack_core::{Manifest, Platform, Version};
use xpack_installer::bundle::{self, InstallPlan, Trailer};
use xpack_package::PackageBuilder;
use xpack_security::KeyPair;

/// A real installer for a package that asks for a desktop entry.
fn build_installer(dir: &Path) -> PathBuf {
    let key = KeyPair::generate().unwrap();
    let source = dir.join("src");
    std::fs::create_dir_all(source.join("bin")).unwrap();
    std::fs::write(source.join("bin/app"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(source.join("bin/app"), std::fs::Permissions::from_mode(0o755))
        .unwrap();

    let manifest = Manifest {
        format_version: xpack_core::FormatVersion(2),
        application: xpack_core::Application {
            id: "com.example.desk".into(),
            name: "Desk".into(),
            version: Version::parse("1.0.0").unwrap(),
            description: None,
            publisher: None,
        },
        platform: Platform::host().unwrap(),
        launch: xpack_core::LaunchSpec {
            executable: "bin/app".into(),
            arguments: Vec::new(),
            working_directory: None,
            keep_working_directory: true,
            environment: std::collections::BTreeMap::new(),
        },
        update: xpack_core::UpdateSpec::default(),
        health: xpack_core::HealthSpec::default(),
        desktop: xpack_core::DesktopSpec { shortcut: true, ..xpack_core::DesktopSpec::default() },
        payload: xpack_core::PayloadSpec::default(),
        signing_key: None,
        created_at: None,
        command: None,
        commands: Vec::new(),
    };
    let package = dir.join("desk.xpkg");
    PackageBuilder::new(&source, manifest).build(&package, &key).unwrap();

    let binaries: Vec<PathBuf> = ["xpack-launcher", "xpack-updater", "xpack-uninstaller"]
        .iter()
        .map(|name| {
            let path = dir.join(name);
            std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
            path
        })
        .collect();
    let plan = InstallPlan {
        format_version: InstallPlan::CURRENT_VERSION,
        application_id: "com.example.desk".into(),
        application_name: "Desk".into(),
        version: "1.0.0".into(),
        signing_key: key.public().to_hex(),
        activate: true,
        ui: None,
    };
    let payload = bundle::build(&plan, &package, &binaries).unwrap();

    let installer = dir.join("installer");
    let mut bytes = std::fs::read(env!("CARGO_BIN_EXE_xpack-installer")).unwrap();
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(
        &Trailer {
            payload_len: payload.len() as u64,
            payload_sha256: xpack_security::sha256(&payload),
        }
        .to_bytes(),
    );
    std::fs::write(&installer, &bytes).unwrap();
    std::fs::set_permissions(&installer, std::fs::Permissions::from_mode(0o755)).unwrap();
    installer
}

/// The desktop folder under the test's `HOME`, made beforehand as a session
/// makes it: a Linux desktop that does not exist gets no shortcut.
fn desktop(dir: &Path) -> PathBuf {
    let desktop = dir.join("home").join("Desktop");
    std::fs::create_dir_all(&desktop).unwrap();
    desktop
}

/// Runs the installer silently, with every user directory inside `dir`.
fn install(dir: &Path, extra: &[&str]) -> Output {
    let installer = build_installer(dir);
    let home = dir.join("home");
    output_of(
        Command::new(installer)
            .args(["--silent", "--root"])
            .arg(dir.join("apps"))
            .args(extra)
            .env("HOME", &home)
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_CONFIG_HOME", home.join(".config")),
    )
}

/// Runs a program this test has just written, waiting out the brief "text
/// file busy" Linux reports while another test thread's child still holds it.
fn output_of(command: &mut Command) -> Output {
    let mut attempts = 0;
    loop {
        match command.output() {
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts < 100 =>
            {
                attempts += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            result => return result.unwrap(),
        }
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn on_the_desktop(desktop: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(desktop).unwrap().map(|entry| entry.unwrap().path()).collect()
}

#[test]
fn a_first_installation_puts_a_shortcut_on_the_desktop_and_says_where() {
    let dir = tempfile::tempdir().unwrap();
    let desktop = desktop(dir.path());
    let output = install(dir.path(), &[]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    let shortcuts = on_the_desktop(&desktop);
    assert_eq!(shortcuts.len(), 1, "expected one shortcut: {shortcuts:?}");
    let said = stdout(&output);
    assert!(said.contains(&format!("Added         {}", shortcuts[0].display())), "{said}");
}

#[test]
fn no_desktop_shortcut_is_taken_for_an_answer() {
    let dir = tempfile::tempdir().unwrap();
    let desktop = desktop(dir.path());
    let output = install(dir.path(), &["--no-desktop-shortcut"]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    assert_eq!(on_the_desktop(&desktop), Vec::<PathBuf>::new());
    assert!(stdout(&output).contains("Added"), "the desktop entry itself is still added");
}

#[test]
fn no_entry_means_no_shortcut_on_the_desktop_either() {
    let dir = tempfile::tempdir().unwrap();
    let desktop = desktop(dir.path());
    let output = install(dir.path(), &["--no-shortcut"]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(on_the_desktop(&desktop), Vec::<PathBuf>::new());
}

#[test]
fn a_dry_run_says_whether_a_shortcut_would_go_on_the_desktop() {
    let dir = tempfile::tempdir().unwrap();
    let desktop = desktop(dir.path());
    let would = stdout(&install(dir.path(), &["--dry-run"]));
    assert!(would.contains("would add       a shortcut on the desktop"), "{would}");
    let would_not = stdout(&install(dir.path(), &["--dry-run", "--no-desktop-shortcut"]));
    assert!(!would_not.contains("on the desktop"), "{would_not}");
    assert_eq!(on_the_desktop(&desktop), Vec::<PathBuf>::new(), "a dry run wrote something");
}
