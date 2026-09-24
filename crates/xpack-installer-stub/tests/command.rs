//! The installer puts the command in place, says how to use it, and takes no
//! for an answer.
//!
//! Unix only: the command goes into `~/.local/bin` under a `HOME` pointed at
//! a temporary directory. On Windows it would edit the real user's `PATH`,
//! which a test must not do; the installation library's own tests cover that
//! against a scratch registry key.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use xpack_core::{Manifest, Platform, Version};
use xpack_installer::bundle::{self, InstallPlan, Trailer};
use xpack_package::PackageBuilder;
use xpack_security::KeyPair;

/// A real installer for a package that names the command `mytool`.
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
            id: "com.example.tool".into(),
            name: "Tool".into(),
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
        desktop: xpack_core::DesktopSpec::default(),
        payload: xpack_core::PayloadSpec::default(),
        signing_key: None,
        created_at: None,
        command: Some(xpack_core::CommandSpec { name: "mytool".into() }),
    };
    let package = dir.join("tool.xpkg");
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
        application_id: "com.example.tool".into(),
        application_name: "Tool".into(),
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

/// Runs the installer silently, with `HOME` in `dir` and `path` as the `PATH`.
fn install(dir: &Path, path: &str, extra: &[&str]) -> Output {
    let installer = build_installer(dir);
    output_of(
        Command::new(installer)
            .args(["--silent", "--root"])
            .arg(dir.join("apps"))
            .args(extra)
            .env("HOME", dir.join("home"))
            .env("PATH", path),
    )
}

/// Runs a program this test has just written, as `Command::output` does.
///
/// Linux refuses to execute a file that some process still holds open for
/// writing. The test's own handle is closed, but another test thread may have
/// forked in the moment it was open, and that child keeps a copy until it
/// execs its own program. The refusal ends within milliseconds, so it is
/// retried rather than reported.
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

#[test]
fn the_command_is_added_and_the_summary_says_how_to_use_it() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("home/.local/bin");
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let output = install(dir.path(), &path, &[]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    let script = bin.join("mytool");
    assert!(script.is_file(), "no command at {}", script.display());
    assert!(stdout(&output).contains("Or type:       mytool"), "{}", stdout(&output));
    // On the PATH already: no advice to change it.
    assert!(!stdout(&output).contains("not on your PATH"), "{}", stdout(&output));
}

#[test]
fn a_command_off_the_path_comes_with_the_line_that_puts_it_there() {
    let dir = tempfile::tempdir().unwrap();
    let output = install(dir.path(), "/usr/bin:/bin", &[]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let bin = dir.path().join("home/.local/bin");
    let text = stdout(&output);
    assert!(text.contains("is not on your PATH"), "{text}");
    assert!(text.contains(&format!("export PATH=\"{}:$PATH\"", bin.display())), "{text}");
}

#[test]
fn no_path_installs_the_application_without_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let output = install(dir.path(), "/usr/bin:/bin", &["--no-path"]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(dir.path().join("apps/com.example.tool/state/state.json").is_file());
    assert!(!dir.path().join("home/.local/bin/mytool").exists());
    assert!(!stdout(&output).contains("mytool"), "{}", stdout(&output));
}

#[test]
fn a_dry_run_says_it_would_add_the_command_and_adds_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let output = install(dir.path(), "/usr/bin:/bin", &["--dry-run"]);
    assert!(stdout(&output).contains("would add       the command mytool"), "{}", stdout(&output));
    assert!(!dir.path().join("home/.local/bin").exists());
}
