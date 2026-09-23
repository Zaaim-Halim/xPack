//! Where a launched application starts.
//!
//! One test in its own binary: it moves this process to another directory,
//! and the working directory is shared by every thread, so a second test
//! running beside it would resolve its relative paths against the wrong place.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use xpack_core::LaunchSpec;
use xpack_platform::{LaunchRequest, launch};

/// A payload that writes the directory it runs in to `$XPACK_TEST_OUT`.
fn install_payload(version_dir: &Path) {
    let script = version_dir.join("bin/where");
    fs::create_dir_all(script.parent().unwrap()).unwrap();
    fs::write(&script, "#!/bin/sh\npwd -P > \"$XPACK_TEST_OUT\"\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
}

fn spec(out: &Path, keep: bool, working_directory: Option<&str>) -> LaunchSpec {
    LaunchSpec {
        executable: "bin/where".into(),
        arguments: Vec::new(),
        working_directory: working_directory.map(str::to_string),
        keep_working_directory: keep,
        environment: BTreeMap::from([("XPACK_TEST_OUT".to_string(), out.display().to_string())]),
    }
}

/// Launches through the real `launch` and returns where the payload ran.
fn ran_in(version_dir: &Path, spec: LaunchSpec, out: &Path) -> PathBuf {
    let _ = fs::remove_file(out);
    let mut request = LaunchRequest::new(version_dir, spec);
    request.inherit_stdio = false;
    let status = launch(&request).unwrap().wait().unwrap();
    assert!(status.success());
    PathBuf::from(fs::read_to_string(out).unwrap().trim_end())
}

#[test]
fn the_application_starts_where_its_manifest_says() {
    let scratch = tempfile::tempdir().unwrap();
    let version_dir = fs::canonicalize(scratch.path()).unwrap().join("versions/1.0.0");
    install_payload(&version_dir);
    fs::create_dir_all(version_dir.join("data")).unwrap();
    let invoked_from = fs::canonicalize(scratch.path()).unwrap().join("where-the-user-is");
    fs::create_dir_all(&invoked_from).unwrap();
    let out = scratch.path().join("out");

    // The user is somewhere else entirely, as they are when they type a
    // command's name in a terminal.
    std::env::set_current_dir(&invoked_from).unwrap();

    // Kept: the directory the application was invoked from, so a relative
    // path the user typed means what they meant.
    assert_eq!(ran_in(&version_dir, spec(&out, true, None), &out), invoked_from);

    // The default is unchanged: the version directory.
    assert_eq!(ran_in(&version_dir, spec(&out, false, None), &out), version_dir);

    // A declared directory still wins over the default.
    assert_eq!(
        ran_in(&version_dir, spec(&out, false, Some("data")), &out),
        version_dir.join("data")
    );

    // Both at once is refused rather than resolved in someone's favour.
    let err =
        launch(&LaunchRequest::new(&version_dir, spec(&out, true, Some("data")))).unwrap_err();
    assert!(err.to_string().contains("contradict"), "got {err}");
}
