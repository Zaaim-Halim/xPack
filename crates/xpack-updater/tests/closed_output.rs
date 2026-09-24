//! An updater whose progress stream nobody reads still runs to the end.
//!
//! A desktop application spawns `xpack-updater --progress`, reads the stream
//! to drive its own dialog, and may stop reading at any moment: the user
//! closed the dialog, the application quit. The updater must carry on and end
//! with the exit code its work earned, not with a crash's.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use xpack_core::manifest::{Application, FormatVersion, LaunchSpec, PayloadSpec, UpdateSpec};
use xpack_core::{InstallPaths, Manifest, Platform, Version};
use xpack_install::{InstallOptions, Installer, TrustDecision, open_and_verify};
use xpack_package::PackageBuilder;
use xpack_platform::InstallLock;
use xpack_security::KeyPair;

/// An update server that is not there: a local port nothing listens on.
///
/// The check starts, and says so on the stream, before the connection fails.
fn absent_server() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("http://127.0.0.1:{port}/demo")
}

/// An installation of 1.0.0 that looks for updates at `url`.
fn install(dir: &Path, url: &str) -> InstallPaths {
    let source = dir.join("src");
    std::fs::create_dir_all(source.join("bin")).unwrap();
    std::fs::write(source.join("bin/app"), "#!/bin/sh\nexit 0\n").unwrap();

    let manifest = Manifest {
        format_version: FormatVersion::CURRENT,
        application: Application {
            id: "com.example.demo".into(),
            name: "Demo".into(),
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
        update: UpdateSpec { url: Some(url.to_string()), ..UpdateSpec::default() },
        health: xpack_core::HealthSpec::default(),
        signing_key: None,
        desktop: xpack_core::DesktopSpec::default(),
        payload: PayloadSpec::default(),
        created_at: None,
        command: None,
        commands: Vec::new(),
    };
    let key = KeyPair::generate().unwrap();
    let package = dir.join("demo.xpkg");
    PackageBuilder::new(&source, manifest).build(&package, &key).unwrap();

    let paths = InstallPaths::new(&dir.join("root"), "com.example.demo").unwrap();
    let lock = InstallLock::acquire(&paths).unwrap();
    let mut verified =
        open_and_verify(&package, &lock, &TrustDecision::Explicit(key.public())).unwrap();
    let options = InstallOptions { activate: true, ..Default::default() };
    Installer::new(&lock).install(&mut verified, &options).unwrap();
    paths
}

/// A stream whose reader has gone away: every write to it fails.
fn closed_pipe() -> Stdio {
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    writer.into()
}

fn update(paths: &InstallPaths, stdout: Stdio) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xpack-updater"))
        .args(["--force", "--progress", "--application-dir"])
        .arg(paths.root())
        .stdout(stdout)
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

#[test]
fn a_progress_stream_nobody_reads_keeps_the_updaters_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let paths = install(dir.path(), &absent_server());

    // Read, the stream says the check started: so the run below really does
    // write into the closed pipe, rather than passing because it never wrote.
    let read = update(&paths, Stdio::piped());
    let stream = String::from_utf8_lossy(&read.stdout);
    assert!(stream.contains("checking"), "nothing was reported: {stream:?}");
    assert_eq!(read.status.code(), Some(1), "an absent server is a failure: {:?}", read.status);

    // Unread, the same failure, reported by the same code.
    let unread = update(&paths, closed_pipe());
    assert_eq!(
        unread.status.code(),
        Some(1),
        "{:?}\nstderr: {}",
        unread.status,
        String::from_utf8_lossy(&unread.stderr)
    );
}
