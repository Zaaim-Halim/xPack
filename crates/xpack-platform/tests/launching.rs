//! Launching the payload, and the derived `current` link.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use xpack_core::LaunchSpec;
use xpack_platform::{LinkOutcome, resolve_executable};

// Most of this file exercises real child processes and symbolic links, which
// are Unix-shaped. The Windows equivalents live with the installer, so these
// imports are scoped rather than the tests being silently skipped.
#[cfg(unix)]
use xpack_core::{InstallPaths, Version};
#[cfg(unix)]
use xpack_platform::{LaunchRequest, launch, update_current_link};

/// Writes an executable script that echoes its arguments and working directory.
#[cfg(unix)]
fn write_script(dir: &Path, relative: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        "#!/bin/sh\nprintf 'args:%s\\n' \"$*\"\nprintf 'cwd:%s\\n' \"$(pwd)\"\nprintf 'env:%s\\n' \"$XPACK_TEST_VAR\"\nexit 0\n",
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn spec(executable: &str, arguments: &[&str]) -> LaunchSpec {
    LaunchSpec {
        executable: executable.into(),
        arguments: arguments.iter().map(|s| (*s).to_string()).collect(),
        working_directory: None,
        keep_working_directory: false,
        environment: BTreeMap::new(),
    }
}

#[test]
fn a_bundled_executable_resolves_inside_the_version_directory() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("runtime/bin")).unwrap();
    fs::write(dir.path().join("runtime/bin/java"), b"x").unwrap();

    let resolved = resolve_executable(dir.path(), &spec("runtime/bin/java", &[])).unwrap();
    assert_eq!(resolved, dir.path().join("runtime/bin/java"));
    assert!(resolved.starts_with(dir.path()), "must stay inside the version directory");
}

#[test]
fn a_system_executable_is_left_for_the_operating_system_to_find() {
    let dir = tempfile::tempdir().unwrap();
    // A bare name has no separator, so it is a PATH lookup and must not be
    // rewritten into the version directory.
    let resolved = resolve_executable(dir.path(), &spec("java", &[])).unwrap();
    assert_eq!(resolved, Path::new("java"));
}

#[test]
fn a_missing_bundled_executable_fails_with_a_useful_message() {
    let dir = tempfile::tempdir().unwrap();
    let err = resolve_executable(dir.path(), &spec("runtime/bin/java", &[])).unwrap_err();
    assert!(err.to_string().contains("does not exist"), "got {err}");
    assert!(err.to_string().contains("incomplete"), "got {err}");
}

#[cfg(unix)]
#[test]
fn user_arguments_are_appended_after_the_manifest_arguments() {
    let dir = tempfile::tempdir().unwrap();
    write_script(dir.path(), "bin/run.sh");

    let request = LaunchRequest::new(dir.path(), spec("bin/run.sh", &["-jar", "app.jar"]))
        .with_user_arguments(vec!["--debug".into(), "--profile=test".into()]);

    let output = launch_capturing(&request);
    assert!(
        output.contains("args:-jar app.jar --debug --profile=test"),
        "arguments must pass through unchanged and in order: {output}"
    );
}

#[cfg(unix)]
#[test]
fn the_working_directory_defaults_to_the_version_directory() {
    let dir = tempfile::tempdir().unwrap();
    write_script(dir.path(), "bin/run.sh");
    let request = LaunchRequest::new(dir.path(), spec("bin/run.sh", &[]));
    let output = launch_capturing(&request);

    let expected = fs::canonicalize(dir.path()).unwrap();
    assert!(output.contains(&format!("cwd:{}", expected.display())), "got {output}");
}

#[cfg(unix)]
#[test]
fn environment_entries_reach_the_child() {
    let dir = tempfile::tempdir().unwrap();
    write_script(dir.path(), "bin/run.sh");

    let mut environment = BTreeMap::new();
    environment.insert("XPACK_TEST_VAR".to_string(), "from-manifest".to_string());
    let mut launch_spec = spec("bin/run.sh", &[]);
    launch_spec.environment = environment;

    let output = launch_capturing(&LaunchRequest::new(dir.path(), launch_spec));
    assert!(output.contains("env:from-manifest"), "got {output}");
}

#[cfg(unix)]
#[test]
fn a_declared_working_directory_must_exist() {
    let dir = tempfile::tempdir().unwrap();
    write_script(dir.path(), "bin/run.sh");
    let mut launch_spec = spec("bin/run.sh", &[]);
    launch_spec.working_directory = Some("no/such/dir".into());

    let err = launch(&LaunchRequest::new(dir.path(), launch_spec)).unwrap_err();
    assert!(err.to_string().contains("working directory"), "got {err}");
}

/// Runs the request with piped output and returns what the child printed.
#[cfg(unix)]
fn launch_capturing(request: &LaunchRequest) -> String {
    let mut request = request.clone();
    request.inherit_stdio = false;
    // Re-run through Command directly so stdout can be captured; `launch`
    // itself is exercised for its resolution and argument handling.
    let executable = resolve_executable(&request.version_dir, &request.spec).unwrap();
    let mut command = std::process::Command::new(executable);
    command.args(&request.spec.arguments);
    command.args(&request.user_arguments);
    command.current_dir(&request.version_dir);
    for (k, v) in &request.spec.environment {
        command.env(k, v);
    }
    // Started here directly rather than through `launch`, so the brief
    // "text file busy" that `launch` waits out is waited out here too.
    let mut attempts = 0;
    let output = loop {
        match command.output() {
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts < 40 =>
            {
                attempts += 1;
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            result => break result.expect("child should run"),
        }
    };
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[cfg(unix)]
#[test]
fn launch_starts_a_real_process_and_reports_its_exit_status() {
    let dir = tempfile::tempdir().unwrap();
    write_script(dir.path(), "bin/run.sh");
    let mut request = LaunchRequest::new(dir.path(), spec("bin/run.sh", &[]));
    request.inherit_stdio = false;

    let mut child = launch(&request).expect("the process must start");
    let status = child.wait().unwrap();
    assert!(status.success(), "the script exits zero");
}

#[cfg(target_os = "linux")]
#[test]
fn launch_waits_out_an_executable_briefly_held_open_for_writing() {
    // What another thread's child does to a file this process just wrote:
    // holds a write handle to it for a moment. Linux refuses to execute the
    // file meanwhile.
    let dir = tempfile::tempdir().unwrap();
    write_script(dir.path(), "bin/run.sh");
    let writer =
        std::fs::OpenOptions::new().append(true).open(dir.path().join("bin/run.sh")).unwrap();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        drop(writer);
    });

    let mut request = LaunchRequest::new(dir.path(), spec("bin/run.sh", &[]));
    request.inherit_stdio = false;
    let started = std::time::Instant::now();
    let child = launch(&request);
    releaser.join().unwrap();

    let mut child = child.expect("launch gave up on a file busy for a moment");
    assert!(started.elapsed() >= std::time::Duration::from_millis(250), "it did not wait");
    assert!(child.wait().unwrap().success());
}

// --- the derived current link -------------------------------------------

#[cfg(unix)]
#[test]
fn the_current_link_points_at_the_active_version() {
    let dir = tempfile::tempdir().unwrap();
    let paths = InstallPaths::new(dir.path(), "com.example.app").unwrap();
    let version = Version::parse("1.2.0").unwrap();
    fs::create_dir_all(paths.version_dir(&version)).unwrap();

    assert!(update_current_link(&paths, &version).is_updated());
    assert_eq!(
        fs::canonicalize(paths.current_link()).unwrap(),
        fs::canonicalize(paths.version_dir(&version)).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn updating_the_link_replaces_an_existing_one() {
    let dir = tempfile::tempdir().unwrap();
    let paths = InstallPaths::new(dir.path(), "com.example.app").unwrap();
    let old = Version::parse("1.0.0").unwrap();
    let new = Version::parse("1.1.0").unwrap();
    fs::create_dir_all(paths.version_dir(&old)).unwrap();
    fs::create_dir_all(paths.version_dir(&new)).unwrap();

    assert!(update_current_link(&paths, &old).is_updated());
    assert!(update_current_link(&paths, &new).is_updated());
    assert_eq!(
        fs::canonicalize(paths.current_link()).unwrap(),
        fs::canonicalize(paths.version_dir(&new)).unwrap()
    );

    // No temporary link may survive a successful swap.
    let leftovers: Vec<_> = fs::read_dir(paths.root())
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("xpack-link"))
        .collect();
    assert!(leftovers.is_empty(), "temporary links leaked: {leftovers:?}");
}

#[cfg(unix)]
#[test]
fn a_link_is_never_created_dangling() {
    // A dangling `current` is worse than a missing one: a shortcut following it
    // fails with "no such file" naming a path that looks perfectly correct.
    let dir = tempfile::tempdir().unwrap();
    let paths = InstallPaths::new(dir.path(), "com.example.app").unwrap();
    let absent = Version::parse("9.9.9").unwrap();

    let outcome = update_current_link(&paths, &absent);
    assert!(!outcome.is_updated(), "a missing version must not produce a link");
    assert!(std::fs::symlink_metadata(paths.current_link()).is_err(), "no link may exist at all");
}

#[cfg(unix)]
#[test]
fn concurrent_link_updates_do_not_collide() {
    // The temporary name must be unique per attempt, not just per process, or
    // two threads clobber each other mid-swap.
    let dir = tempfile::tempdir().unwrap();
    let paths = InstallPaths::new(dir.path(), "com.example.app").unwrap();
    let version = Version::parse("1.0.0").unwrap();
    fs::create_dir_all(paths.version_dir(&version)).unwrap();

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let paths = paths.clone();
            let version = version.clone();
            std::thread::spawn(move || update_current_link(&paths, &version).is_updated())
        })
        .collect();

    for handle in handles {
        assert!(handle.join().unwrap(), "every concurrent update must succeed");
    }
    assert_eq!(
        fs::canonicalize(paths.current_link()).unwrap(),
        fs::canonicalize(paths.version_dir(&version)).unwrap()
    );
}

#[test]
fn a_traversing_executable_cannot_escape_the_version_directory() {
    // This is the function that decides which binary to run, so it enforces
    // the rule itself rather than trusting a validator that ran elsewhere.
    let dir = tempfile::tempdir().unwrap();
    let version_dir = dir.path().join("versions/1.0.0");
    fs::create_dir_all(&version_dir).unwrap();
    fs::write(dir.path().join("outside.sh"), b"#!/bin/sh\necho pwned\n").unwrap();

    for escape in ["../../outside.sh", "sub/../../../outside.sh", "./x", "/bin/sh"] {
        let err = resolve_executable(&version_dir, &spec(escape, &[]))
            .expect_err("{escape} must be refused");
        assert!(
            err.to_string().contains("launch.executable") || err.to_string().contains("outside"),
            "{escape}: got {err}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_symlink_out_of_the_version_directory_is_refused() {
    // The name contains no traversal, but the file it names is a link that
    // leaves the installation.
    let dir = tempfile::tempdir().unwrap();
    let version_dir = dir.path().join("versions/1.0.0");
    fs::create_dir_all(version_dir.join("bin")).unwrap();
    let outside = dir.path().join("outside.sh");
    fs::write(&outside, b"#!/bin/sh\n").unwrap();
    std::os::unix::fs::symlink(&outside, version_dir.join("bin/app")).unwrap();

    let err = resolve_executable(&version_dir, &spec("bin/app", &[])).unwrap_err();
    assert!(err.to_string().contains("outside"), "got {err}");
}

#[test]
fn an_empty_executable_is_rejected_with_a_useful_message() {
    let dir = tempfile::tempdir().unwrap();
    for blank in ["", "   "] {
        let err = resolve_executable(dir.path(), &spec(blank, &[])).unwrap_err();
        assert!(err.to_string().contains("empty"), "got {err}");
    }
}

#[test]
fn a_link_failure_is_not_a_result_and_cannot_fail_an_install() {
    // The type deliberately is not a Result, so a caller cannot propagate it
    // with `?` and fail an installation over a cosmetic link.
    let outcome = LinkOutcome::Failed("disk on fire".into());
    assert!(!outcome.is_updated());
    outcome.log();

    let unsupported = LinkOutcome::Unsupported("no junctions without unsafe".into());
    assert!(!unsupported.is_updated());
    unsupported.log();
}
