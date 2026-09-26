//! What a shortcut opens on Windows: the windowed launcher starting a console
//! program, as it starts `java.exe`.
//!
//! The windowed launcher has no console. Windows gives a console program a new
//! one when its parent has none, which is the black window a user sees behind
//! a graphical application. These run the real windowed binary, so the parent
//! is exactly what a shortcut starts, and let the program itself say whether it
//! got a console window.

#![cfg(windows)]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use xpack_core::manifest::{
    Application, FormatVersion, HealthSpec, LaunchSpec, PayloadSpec, UpdateSpec,
};
use xpack_core::{DesktopSpec, InstallPaths, Manifest, Platform, Version};
use xpack_install::{InstallOptions, Installer, TrustDecision, open_and_verify};
use xpack_package::PackageBuilder;
use xpack_platform::InstallLock;
use xpack_security::KeyPair;

/// PowerShell, asking Windows for its console window and writing the handle
/// into the installation it was started from: `0` when it has none.
const REPORT_CONSOLE: &str = "$t = Add-Type -Name Console -Namespace XpackTest -PassThru \
     -MemberDefinition '[DllImport(\"kernel32.dll\")] public static extern System.IntPtr GetConsoleWindow();'; \
     [System.IO.File]::WriteAllText((Join-Path $env:XPACK_APPLICATION_DIR 'console.txt'), \
     $t::GetConsoleWindow().ToInt64().ToString())";

/// Installs an application whose program is PowerShell, marked as needing a
/// terminal or not, and returns its installation.
fn install(dir: &Path, terminal: bool) -> InstallPaths {
    let key = KeyPair::generate().unwrap();
    let payload = dir.join("payload");
    fs::create_dir_all(&payload).unwrap();
    fs::write(payload.join("readme.txt"), b"a payload needs a file").unwrap();

    let manifest = Manifest {
        format_version: FormatVersion::CURRENT,
        application: Application {
            id: "com.example.app".into(),
            name: "App".into(),
            version: Version::parse("1.0.0").unwrap(),
            description: None,
            publisher: None,
        },
        platform: Platform::host().unwrap(),
        launch: LaunchSpec {
            executable: "powershell".into(),
            arguments: ["-NoProfile", "-NonInteractive", "-Command", REPORT_CONSOLE]
                .map(str::to_owned)
                .to_vec(),
            working_directory: None,
            keep_working_directory: false,
            environment: BTreeMap::new(),
        },
        update: UpdateSpec::default(),
        health: HealthSpec { startup_timeout_seconds: 30, require_startup_report: false },
        signing_key: None,
        desktop: DesktopSpec { terminal, ..DesktopSpec::default() },
        payload: PayloadSpec::default(),
        created_at: None,
        command: None,
        commands: Vec::new(),
    };
    let package = dir.join("app.xpkg");
    PackageBuilder::new(&payload, manifest).build(&package, &key).unwrap();

    let paths = InstallPaths::new(&dir.join("apps"), "com.example.app").unwrap();
    let lock = InstallLock::acquire(&paths).unwrap();
    let mut verified =
        open_and_verify(&package, &lock, &TrustDecision::Explicit(key.public())).unwrap();
    Installer::new(&lock)
        .install(&mut verified, &InstallOptions { activate: true, ..Default::default() })
        .unwrap();
    paths
}

/// Runs the windowed launcher from inside the installation, as its shortcut
/// does, and returns the console window the application reported.
fn console_window_seen_by_the_application(paths: &InstallPaths) -> i64 {
    // The launcher finds its installation from where it sits.
    let launcher = paths.root().join("xpack-launcherw.exe");
    fs::copy(env!("CARGO_BIN_EXE_xpack-launcherw"), &launcher).unwrap();

    let status = std::process::Command::new(&launcher).status().unwrap();
    assert!(status.success(), "the launcher failed: {status}");
    let answer = fs::read_to_string(paths.root().join("console.txt"))
        .expect("the application never ran, or could not write its answer");
    answer.trim().parse().unwrap()
}

#[test]
fn a_graphical_application_opened_by_its_shortcut_gets_no_console_window() {
    let dir = tempfile::tempdir().unwrap();
    let paths = install(dir.path(), false);
    assert_eq!(
        console_window_seen_by_the_application(&paths),
        0,
        "a console window appeared behind a graphical application"
    );
}

#[test]
fn an_application_that_needs_a_terminal_keeps_its_console() {
    let dir = tempfile::tempdir().unwrap();
    let paths = install(dir.path(), true);
    assert_ne!(
        console_window_seen_by_the_application(&paths),
        0,
        "a terminal application was started with no console to use"
    );
}
