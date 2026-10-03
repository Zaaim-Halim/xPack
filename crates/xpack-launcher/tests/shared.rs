//! Starting an application installed for every user, as one of those users.
//!
//! The installation belongs to an administrator, so the user starting it can
//! write nothing there. The test makes it read-only for the user running the
//! suite, which is the same thing from that user's side, and starts the real
//! launcher binary: the application must start, and everything the launcher
//! writes must land in the user's own directory.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use xpack_core::manifest::{Application, FormatVersion, LaunchSpec, PayloadSpec, UpdateSpec};
use xpack_core::{InstallPaths, InstallScope, Manifest, Platform, Version};
use xpack_install::{InstallOptions, Installer, TrustDecision, open_and_verify};
use xpack_package::PackageBuilder;
use xpack_platform::InstallLock;
use xpack_security::KeyPair;

/// Records its start, where it was told to report it started, and reports.
const APP: &str = "#!/bin/sh\n\
    printf '%s\\n' \"$XPACK_HEALTH_FILE\" > \"$XPACK_TEST_OUT\"\n\
    : > \"$XPACK_HEALTH_FILE\"\n";

/// Sets `mode` on `dir` and everything in it.
fn chmod_all(dir: &Path, file: u32, directory: u32) {
    for entry in fs::read_dir(dir).unwrap().filter_map(Result::ok) {
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            chmod_all(&path, file, directory);
            fs::set_permissions(&path, fs::Permissions::from_mode(directory)).unwrap();
        } else {
            let executable = meta.permissions().mode() & 0o111 != 0;
            let mode = if executable { file | 0o111 } else { file };
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        }
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(directory)).unwrap();
}

/// Every file under `dir` and its size, to tell whether anything changed.
fn listing(dir: &Path) -> Vec<(String, u64)> {
    let mut all = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).unwrap().filter_map(Result::ok) {
            let meta = fs::symlink_metadata(entry.path()).unwrap();
            if meta.is_dir() {
                pending.push(entry.path());
            }
            all.push((entry.path().display().to_string(), meta.len()));
        }
    }
    all.sort();
    all
}

#[test]
fn a_user_starts_an_installation_they_cannot_write_to() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyPair::generate().unwrap();
    let payload = dir.path().join("payload");
    fs::create_dir_all(payload.join("bin")).unwrap();
    fs::write(payload.join("bin/app"), APP).unwrap();
    fs::set_permissions(payload.join("bin/app"), fs::Permissions::from_mode(0o755)).unwrap();

    let manifest = Manifest {
        format_version: FormatVersion::CURRENT,
        application: Application {
            id: "com.example.app".into(),
            name: "Example".into(),
            version: Version::parse("1.0.0").unwrap(),
            description: None,
            publisher: None,
        },
        platform: Platform::host().unwrap(),
        launch: LaunchSpec {
            executable: "bin/app".into(),
            arguments: vec![],
            working_directory: None,
            keep_working_directory: false,
            environment: BTreeMap::new(),
        },
        update: UpdateSpec::default(),
        health: xpack_core::HealthSpec::default(),
        signing_key: None,
        desktop: xpack_core::DesktopSpec::default(),
        payload: PayloadSpec::default(),
        created_at: None,
        command: None,
        commands: Vec::new(),
        instance: xpack_core::InstanceSpec { single: true, alongside: Vec::new() },
        hooks: xpack_core::hooks::Hooks::default(),
    };
    let mut manifest = manifest;
    manifest.format_version = manifest.required_format_version();
    let package = dir.path().join("app.xpkg");
    PackageBuilder::new(&payload, manifest).build(&package, &key).unwrap();

    // Named after the application, as a machine-wide installation is.
    let paths = InstallPaths::named(dir.path().join("Example"), "com.example.app").unwrap();
    {
        let lock = InstallLock::acquire(&paths).unwrap();
        let mut verified =
            open_and_verify(&package, &lock, &TrustDecision::Explicit(key.public())).unwrap();
        Installer::new(&lock)
            .install(
                &mut verified,
                &InstallOptions {
                    activate: true,
                    scope: InstallScope::Machine,
                    launcher: Some(env!("CARGO_BIN_EXE_xpack-launcher").into()),
                    ..Default::default()
                },
            )
            .unwrap();
    }

    chmod_all(paths.root(), 0o444, 0o555);
    let before = listing(paths.root());

    let users = dir.path().join("users-own");
    let out = dir.path().join("out.txt");
    let status = Command::new(
        paths.launcher_file_named(&xpack_core::BinaryNames::from_display_name("Example")),
    )
    .env("XPACK_USER_DIR", &users)
    .env("XPACK_TEST_OUT", &out)
    .env_remove("XPACK_BUNDLE")
    .env_remove("XPACK_APPLICATION_DIR")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .output()
    .unwrap();

    let after = listing(paths.root());
    // Back to writable, so the temporary directory can be removed.
    chmod_all(paths.root(), 0o644, 0o755);

    assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));
    let health = fs::read_to_string(&out).expect("the application never started");
    let mine = users.join("com.example.app");
    assert!(
        Path::new(health.trim()).starts_with(&mine),
        "told to report into {health}, not the user's own directory"
    );
    assert!(mine.join("logs").is_dir(), "the launcher's log is not in the user's directory");
    assert!(mine.join("instance.lock").is_file(), "the instance lock is not either");
    assert_eq!(before, after, "the installation changed");
}
