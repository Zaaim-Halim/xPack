//! An installer whose output nobody reads still installs.
//!
//! A script that pipes the installer into `head -1`, a log collector that
//! dies, a terminal closed mid-run: each leaves the installer writing into a
//! stream with no reader. The installer must finish the installation it
//! started and report the result in its exit status, not stop between two
//! steps because a line could not be printed.
//!
//! Each test runs the real stub with a payload appended, as `xpack installer`
//! builds it, and hands it a pipe whose reading end is already closed, so
//! every write to that stream fails.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use xpack_core::{Manifest, Platform, Version};
use xpack_installer::bundle::{self, InstallPlan, Trailer};
use xpack_package::PackageBuilder;
use xpack_security::KeyPair;

/// A signed package whose application is a script.
fn build_package(dir: &Path, key: &KeyPair) -> PathBuf {
    let payload = dir.join("src");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    let app = payload.join("bin/app");
    std::fs::write(&app, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let manifest = Manifest {
        format_version: xpack_core::FormatVersion::default(),
        application: xpack_core::Application {
            id: "com.example.demo".into(),
            name: "Demo".into(),
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
    };
    let out = dir.join("app.xpkg");
    PackageBuilder::new(&payload, manifest).build(&out, key).unwrap();
    out
}

/// The stub under test with a payload appended: a complete installer.
fn build_installer(dir: &Path) -> PathBuf {
    let key = KeyPair::generate().unwrap();
    let package = build_package(dir, &key);
    let binaries: Vec<PathBuf> =
        ["xpack-launcher", "xpack-launcherw", "xpack-updater", "xpack-uninstaller"]
            .iter()
            .map(|name| {
                let path = dir.join(name);
                std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
                path
            })
            .collect();
    let plan = InstallPlan {
        format_version: InstallPlan::CURRENT_VERSION,
        application_id: "com.example.demo".into(),
        application_name: "Demo".into(),
        version: "1.0.0".into(),
        signing_key: key.public().to_hex(),
        activate: true,
        ui: None,
    };
    let payload = bundle::build(&plan, &package, &binaries).unwrap();

    let installer = dir.join(format!("installer{}", std::env::consts::EXE_SUFFIX));
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&installer, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    installer
}

/// A stream whose reader has gone away: every write to it fails.
fn closed_pipe() -> Stdio {
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    writer.into()
}

fn describe(output: &Output) -> String {
    format!(
        "status {:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_installed(root: &Path) {
    let state = root.join("com.example.demo/state/state.json");
    assert!(state.is_file(), "the installation did not finish: no {}", state.display());
}

#[test]
fn an_installer_with_nobody_reading_its_output_still_installs() {
    let dir = tempfile::tempdir().unwrap();
    let installer = build_installer(dir.path());
    let root = dir.path().join("root");

    let output = Command::new(&installer)
        .args(["--silent", "--root"])
        .arg(&root)
        .stdout(closed_pipe())
        .stderr(Stdio::piped())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert_installed(&root);
}

#[test]
fn an_installer_with_nobody_reading_its_diagnostics_still_installs() {
    // Verbose, so the log has something to say on the stream that is gone.
    let dir = tempfile::tempdir().unwrap();
    let installer = build_installer(dir.path());
    let root = dir.path().join("root");

    let output = Command::new(&installer)
        .args(["--silent", "--verbose", "--root"])
        .arg(&root)
        .stdout(Stdio::piped())
        .stderr(closed_pipe())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert_installed(&root);
}

#[test]
fn a_failure_nobody_can_read_still_exits_with_its_own_code() {
    // The stub on its own carries no payload, which is an ordinary failure
    // with an ordinary exit code. Its message has nowhere to go; the code
    // must still be that failure's, not a crash's.
    let output = Command::new(env!("CARGO_BIN_EXE_xpack-installer"))
        .arg("--silent")
        .stdout(closed_pipe())
        .stderr(closed_pipe())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "status {:?}", output.status);
}
