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

/// A payload that writes to `$XPACK_TEST_OUT`, a line each: the directory it
/// runs in, its first argument, and `$XPACK_TEST_PLACE`.
fn install_payload(version_dir: &Path) {
    let script = version_dir.join("bin/where");
    fs::create_dir_all(script.parent().unwrap()).unwrap();
    fs::write(
        &script,
        "#!/bin/sh\n{ pwd -P; printf '%s\\n' \"$1\" \"$XPACK_TEST_PLACE\"; } > \"$XPACK_TEST_OUT\"\n",
    )
    .unwrap();
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

/// Launches through the real `launch` and returns what the payload wrote.
fn reported(version_dir: &Path, spec: LaunchSpec, out: &Path) -> Vec<String> {
    let _ = fs::remove_file(out);
    let mut request = LaunchRequest::new(version_dir, spec);
    request.inherit_stdio = false;
    let status = launch(&request).unwrap().wait().unwrap();
    assert!(status.success());
    fs::read_to_string(out).unwrap().lines().map(str::to_string).collect()
}

/// Where the payload ran.
fn ran_in(version_dir: &Path, spec: LaunchSpec, out: &Path) -> PathBuf {
    PathBuf::from(&reported(version_dir, spec, out)[0])
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

    // Started where the user is, the application still finds its own files:
    // the placeholder is the version directory, in the manifest's arguments
    // and environment alike, while the user's arguments are left as typed.
    let mut with_placeholder = spec(&out, true, None);
    with_placeholder.arguments = vec!["{versionDir}/data".into()];
    with_placeholder.environment.insert("XPACK_TEST_PLACE".into(), "{versionDir}".into());
    let lines = reported(&version_dir, with_placeholder.clone(), &out);
    assert_eq!(PathBuf::from(&lines[0]), invoked_from);
    assert_eq!(PathBuf::from(&lines[1]), version_dir.join("data"));
    assert_eq!(PathBuf::from(&lines[2]), version_dir);

    let mut request = LaunchRequest::new(&version_dir, spec(&out, true, None))
        .with_user_arguments(vec!["{versionDir}".into()]);
    request.inherit_stdio = false;
    let _ = fs::remove_file(&out);
    assert!(launch(&request).unwrap().wait().unwrap().success());
    let lines: Vec<String> =
        fs::read_to_string(&out).unwrap().lines().map(str::to_string).collect();
    assert_eq!(lines[1], "{versionDir}", "a user's argument was rewritten");

    // A version directory named relatively still becomes an absolute path,
    // or it would mean nothing from the directory the application starts in.
    // Absolute, not tidied: `..` is kept, as `std::path::absolute` keeps it,
    // so the check is that it leads to the right place.
    let relative =
        Path::new("..").join(version_dir.strip_prefix(invoked_from.parent().unwrap()).unwrap());
    let lines = reported(&relative, with_placeholder, &out);
    let data = Path::new(&lines[1]);
    assert!(data.is_absolute(), "{}", data.display());
    assert_eq!(fs::canonicalize(data).unwrap(), version_dir.join("data"));

    // Both at once is refused rather than resolved in someone's favour.
    let err =
        launch(&LaunchRequest::new(&version_dir, spec(&out, true, Some("data")))).unwrap_err();
    assert!(err.to_string().contains("contradict"), "got {err}");
}
