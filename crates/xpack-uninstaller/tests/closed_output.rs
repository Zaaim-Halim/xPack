//! An uninstaller whose output nobody reads still finishes.
//!
//! The uninstaller reports on stderr, and its relocated copy inherits that
//! stream from a process that has already exited, so whoever was reading may
//! well be gone by the time the copy speaks. Printing must never be what
//! stops it: not before the removal, and not before the copy deletes itself.
//!
//! Each test hands the real binary a pipe whose reading end is already closed,
//! so every write to it fails.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use xpack_core::{BinaryNames, InstallPaths, Manifest, Platform, Version};
use xpack_install::{InstallOptions, Installer, TrustDecision, open_and_verify};
use xpack_package::PackageBuilder;
use xpack_platform::InstallLock;
use xpack_security::KeyPair;

const APPLICATION_ID: &str = "com.example.closedoutput";

fn uninstaller() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_xpack-uninstaller"))
}

/// Installs a real package, with this build's uninstaller placed inside it.
fn install(root: &Path) -> InstallPaths {
    let source = root.join("src");
    std::fs::create_dir_all(source.join("bin")).unwrap();
    std::fs::write(source.join("bin/app"), "#!/bin/sh\nexit 0\n").unwrap();

    let manifest = Manifest {
        format_version: xpack_core::FormatVersion::CURRENT,
        application: xpack_core::Application {
            id: APPLICATION_ID.into(),
            name: "Example".into(),
            version: Version::parse("1.0.0").unwrap(),
            description: None,
            publisher: None,
        },
        platform: Platform::host().unwrap(),
        launch: xpack_core::LaunchSpec {
            executable: "bin/app".into(),
            arguments: Vec::new(),
            working_directory: None,
            keep_working_directory: false,
            environment: std::collections::BTreeMap::new(),
        },
        update: xpack_core::UpdateSpec::default(),
        health: xpack_core::HealthSpec::default(),
        desktop: xpack_core::DesktopSpec::default(),
        payload: xpack_core::PayloadSpec::default(),
        signing_key: None,
        created_at: None,
        command: None,
        commands: Vec::new(),
    };
    let key = KeyPair::generate().unwrap();
    let package = root.join("app.xpkg");
    PackageBuilder::new(&source, manifest).build(&package, &key).unwrap();

    let paths = InstallPaths::new(&root.join("installed"), APPLICATION_ID).unwrap();
    let lock = InstallLock::acquire(&paths).unwrap();
    let mut verified =
        open_and_verify(&package, &lock, &TrustDecision::Explicit(key.public())).unwrap();
    let options = InstallOptions {
        activate: true,
        uninstaller: Some(uninstaller()),
        ..InstallOptions::default()
    };
    Installer::new(&lock).install(&mut verified, &options).unwrap();
    paths
}

/// A stream whose reader has gone away: every write to it fails.
fn closed_pipe() -> Stdio {
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    writer.into()
}

/// Waits for something a detached process is expected to bring about.
fn eventually(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn the_installed_uninstaller_finishes_with_nobody_reading() {
    // The path a person takes: the uninstaller inside the installation,
    // handing over to a copy of itself outside it.
    let dir = tempfile::tempdir().unwrap();
    let paths = install(dir.path());
    // Named by the installation's own rule, which adds `.exe` on Windows.
    let installed = paths.uninstaller_file_named(&BinaryNames::from_display_name("Example"));
    assert!(installed.is_file(), "the uninstaller was not placed");

    let first = Command::new(&installed)
        .arg("--yes")
        .stdout(Stdio::null())
        .stderr(closed_pipe())
        .spawn()
        .unwrap();
    // The copy is named after the process that made it.
    let copy_dir =
        std::env::temp_dir().join(format!("xpack-uninstall-{APPLICATION_ID}-{}", first.id()));
    let status = first.wait_with_output().unwrap().status;
    assert_eq!(status.code(), Some(0), "the hand-over failed: {status:?}");

    eventually("the installation is removed", || !paths.root().exists());
    // The copy's last act, after every line it prints: without it, a stray
    // uninstaller is left in the temporary directory. Unix only: on Windows a
    // running program cannot delete its own file, so the copy is left there
    // for the system to clear, by design, and there is nothing to wait for.
    #[cfg(unix)]
    eventually("the relocated copy removes itself", || !copy_dir.exists());
    #[cfg(not(unix))]
    let _ = copy_dir;
}

#[test]
fn a_removal_nobody_reads_about_still_exits_successfully() {
    // Run from outside the installation, so the removal happens in this
    // process and its exit status is the removal's.
    let dir = tempfile::tempdir().unwrap();
    let paths = install(dir.path());

    let output = Command::new(uninstaller())
        .arg("--yes")
        .arg("--application-dir")
        .arg(paths.root())
        .stdout(Stdio::null())
        .stderr(closed_pipe())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0), "status {:?}", output.status);
    assert!(!paths.root().exists(), "the installation is still there");
}

#[test]
fn a_refusal_nobody_can_read_still_exits_with_its_own_code() {
    let dir = tempfile::tempdir().unwrap();
    let paths = install(dir.path());

    // No --yes: an ordinary refusal, whose explanation has nowhere to go.
    let output = Command::new(uninstaller())
        .arg("--application-dir")
        .arg(paths.root())
        .stdout(Stdio::null())
        .stderr(closed_pipe())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "status {:?}", output.status);
    assert!(paths.root().exists(), "a refusal removed the installation");
}
